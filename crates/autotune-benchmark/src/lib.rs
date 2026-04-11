use autotune_adaptor::criterion::CriterionAdaptor;
use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};
use autotune_adaptor::{BenchmarkOutput, MetricAdaptor, Metrics};
use autotune_config::{AdaptorConfig, BenchmarkConfig};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Errors returned by benchmark execution and metric extraction.
#[derive(Debug, Error)]
pub enum BenchmarkError {
    #[error("benchmark '{name}' command failed (exit code {code}): {stderr}")]
    CommandFailed {
        name: String,
        code: i32,
        stderr: String,
    },

    #[error("benchmark '{name}' IO error: {source}")]
    Io {
        name: String,
        source: std::io::Error,
    },

    #[error("benchmark '{name}' timed out after {timeout} seconds")]
    TimedOut { name: String, timeout: u64 },

    #[error("metric extraction failed for benchmark '{name}': {source}")]
    Extraction {
        name: String,
        source: autotune_adaptor::AdaptorError,
    },
}

/// Run a single benchmark command and extract metrics.
pub fn run_benchmark(
    config: &BenchmarkConfig,
    working_dir: &Path,
) -> Result<Metrics, BenchmarkError> {
    let output = run_command_with_timeout(config, working_dir)?;

    if !output.status.success() {
        return Err(BenchmarkError::CommandFailed {
            name: config.name.clone(),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let bench_output = BenchmarkOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };

    let adaptor = build_adaptor(&config.adaptor, working_dir);
    adaptor
        .extract(&bench_output)
        .map_err(|source| BenchmarkError::Extraction {
            name: config.name.clone(),
            source,
        })
}

/// Run all configured benchmarks and merge their metrics.
pub fn run_all_benchmarks(
    configs: &[BenchmarkConfig],
    working_dir: &Path,
) -> Result<Metrics, BenchmarkError> {
    let mut all_metrics = HashMap::new();

    for config in configs {
        let metrics = run_benchmark(config, working_dir)?;
        all_metrics.extend(metrics);
    }

    Ok(all_metrics)
}

/// Build a MetricAdaptor from config.
fn build_adaptor(config: &AdaptorConfig, working_dir: &Path) -> Box<dyn MetricAdaptor> {
    match config {
        AdaptorConfig::Regex { patterns } => {
            let configs: Vec<RegexPatternConfig> = patterns
                .iter()
                .map(|pattern| RegexPatternConfig {
                    name: pattern.name.clone(),
                    pattern: pattern.pattern.clone(),
                })
                .collect();
            Box::new(RegexAdaptor::new(configs))
        }
        AdaptorConfig::Criterion { benchmark_name } => {
            let criterion_dir = working_dir.join("target").join("criterion");
            Box::new(CriterionAdaptor::new(&criterion_dir, benchmark_name))
        }
        AdaptorConfig::Script { command } => Box::new(ScriptAdaptorWithWorkingDir::new(
            command.clone(),
            working_dir.to_path_buf(),
        )),
    }
}

fn run_command_with_timeout(
    config: &BenchmarkConfig,
    working_dir: &Path,
) -> Result<Output, BenchmarkError> {
    let program = &config.command[0];
    let args = &config.command[1..];

    let mut child = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| BenchmarkError::Io {
            name: config.name.clone(),
            source,
        })?;

    wait_for_child(config, &mut child)
}

fn wait_for_child(config: &BenchmarkConfig, child: &mut Child) -> Result<Output, BenchmarkError> {
    let deadline = Duration::from_secs(config.timeout);
    let started_at = Instant::now();

    loop {
        if child
            .try_wait()
            .map_err(|source| BenchmarkError::Io {
                name: config.name.clone(),
                source,
            })?
            .is_some()
        {
            let status = child.wait().map_err(|source| BenchmarkError::Io {
                name: config.name.clone(),
                source,
            })?;
            return collect_output(config, child, status);
        }

        if started_at.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(BenchmarkError::TimedOut {
                name: config.name.clone(),
                timeout: config.timeout,
            });
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn collect_output(
    config: &BenchmarkConfig,
    child: &mut Child,
    status: ExitStatus,
) -> Result<Output, BenchmarkError> {
    let mut stdout = Vec::new();
    if let Some(mut stdout_pipe) = child.stdout.take() {
        stdout_pipe
            .read_to_end(&mut stdout)
            .map_err(|source| BenchmarkError::Io {
                name: config.name.clone(),
                source,
            })?;
    }

    let mut stderr = Vec::new();
    if let Some(mut stderr_pipe) = child.stderr.take() {
        stderr_pipe
            .read_to_end(&mut stderr)
            .map_err(|source| BenchmarkError::Io {
                name: config.name.clone(),
                source,
            })?;
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

struct ScriptAdaptorWithWorkingDir {
    command: Vec<String>,
    working_dir: PathBuf,
}

impl ScriptAdaptorWithWorkingDir {
    fn new(command: Vec<String>, working_dir: PathBuf) -> Self {
        Self {
            command,
            working_dir,
        }
    }
}

impl MetricAdaptor for ScriptAdaptorWithWorkingDir {
    fn extract(&self, output: &BenchmarkOutput) -> Result<Metrics, autotune_adaptor::AdaptorError> {
        let Some((program, args)) = self.command.split_first() else {
            return Err(autotune_adaptor::AdaptorError::ScriptEmptyCommand);
        };

        let mut child = Command::new(program)
            .args(args)
            .current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| autotune_adaptor::AdaptorError::Io { source })?;

        if let Some(mut stdin) = child.stdin.take() {
            let combined = format!("{}\n{}", output.stdout, output.stderr);
            stdin
                .write_all(combined.as_bytes())
                .map_err(|source| autotune_adaptor::AdaptorError::Io { source })?;
        }

        let result = child
            .wait_with_output()
            .map_err(|source| autotune_adaptor::AdaptorError::Io { source })?;

        if !result.status.success() {
            return Err(autotune_adaptor::AdaptorError::ScriptFailed {
                code: result.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&result.stdout);
        let metrics: Metrics = serde_json::from_str(&stdout)
            .map_err(|source| autotune_adaptor::AdaptorError::ScriptOutputParse { source })?;

        Ok(metrics)
    }
}
