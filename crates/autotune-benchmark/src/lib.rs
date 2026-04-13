use autotune_adaptor::criterion::CriterionAdaptor;
use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};
use autotune_adaptor::{MetricAdaptor, Metrics};

// Re-export for consumers that need to work with build_adaptor
pub use autotune_adaptor::MeasureOutput;
use autotune_config::{AdaptorConfig, MeasureConfig};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Errors returned by measure execution and metric extraction.
#[derive(Debug, Error)]
pub enum MeasureError {
    #[error("measure '{name}' command failed (exit code {code}): {stderr}")]
    CommandFailed {
        name: String,
        code: i32,
        stderr: String,
    },

    #[error("measure '{name}' IO error: {source}")]
    Io {
        name: String,
        source: std::io::Error,
    },

    #[error("measure '{name}' timed out after {timeout} seconds")]
    TimedOut { name: String, timeout: u64 },

    #[error("metric extraction failed for measure '{name}': {source}")]
    Extraction {
        name: String,
        source: autotune_adaptor::AdaptorError,
    },
}

/// Run a single measure command and extract metrics.
pub fn run_measure(config: &MeasureConfig, working_dir: &Path) -> Result<Metrics, MeasureError> {
    let output = run_command_with_timeout(config, working_dir)?;

    if !output.status.success() {
        return Err(MeasureError::CommandFailed {
            name: config.name.clone(),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let bench_output = MeasureOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };

    let adaptor = build_adaptor(&config.adaptor, working_dir);
    adaptor
        .extract(&bench_output)
        .map_err(|source| MeasureError::Extraction {
            name: config.name.clone(),
            source,
        })
}

/// Run all configured measures and merge their metrics.
pub fn run_all_measures(
    configs: &[MeasureConfig],
    working_dir: &Path,
) -> Result<Metrics, MeasureError> {
    let mut all_metrics = HashMap::new();

    for config in configs {
        let metrics = run_measure(config, working_dir)?;
        all_metrics.extend(metrics);
    }

    Ok(all_metrics)
}

/// Build a MetricAdaptor from config.
pub fn build_adaptor(config: &AdaptorConfig, working_dir: &Path) -> Box<dyn MetricAdaptor> {
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
        AdaptorConfig::Criterion { measure_name } => {
            let criterion_dir = working_dir.join("target").join("criterion");
            Box::new(CriterionAdaptor::new(&criterion_dir, measure_name))
        }
        AdaptorConfig::Script { command } => Box::new(ScriptAdaptorWithWorkingDir::new(
            command.clone(),
            working_dir.to_path_buf(),
        )),
    }
}

fn run_command_with_timeout(
    config: &MeasureConfig,
    working_dir: &Path,
) -> Result<Output, MeasureError> {
    let program = &config.command[0];
    let args = &config.command[1..];

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|source| MeasureError::Io {
        name: config.name.clone(),
        source,
    })?;
    let stdout_handle = spawn_stdout_reader(child.stdout.take());
    let stderr_handle = spawn_stderr_reader(child.stderr.take());

    match wait_for_child(config, &mut child) {
        Ok(status) => collect_output(config, status, stdout_handle, stderr_handle),
        Err(err) => {
            let _ = join_reader(config, stdout_handle);
            let _ = join_reader(config, stderr_handle);
            Err(err)
        }
    }
}

fn wait_for_child(config: &MeasureConfig, child: &mut Child) -> Result<ExitStatus, MeasureError> {
    let deadline = Duration::from_secs(config.timeout);
    let started_at = Instant::now();

    loop {
        if let Some(status) = child.try_wait().map_err(|source| MeasureError::Io {
            name: config.name.clone(),
            source,
        })? {
            return Ok(status);
        }

        if started_at.elapsed() >= deadline {
            terminate_child(child);
            let _ = child.wait();
            return Err(MeasureError::TimedOut {
                name: config.name.clone(),
                timeout: config.timeout,
            });
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn collect_output(
    config: &MeasureConfig,
    status: ExitStatus,
    stdout_handle: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr_handle: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<Output, MeasureError> {
    let stdout = join_reader(config, stdout_handle)?;
    let stderr = join_reader(config, stderr_handle)?;

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_stdout_reader(
    stdout: Option<ChildStdout>,
) -> Option<JoinHandle<std::io::Result<Vec<u8>>>> {
    stdout.map(spawn_pipe_reader)
}

fn spawn_stderr_reader(
    stderr: Option<ChildStderr>,
) -> Option<JoinHandle<std::io::Result<Vec<u8>>>> {
    stderr.map(spawn_pipe_reader)
}

fn spawn_pipe_reader<R>(mut reader: R) -> JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer)?;
        Ok(buffer)
    })
}

fn join_reader(
    config: &MeasureConfig,
    handle: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<Vec<u8>, MeasureError> {
    let Some(handle) = handle else {
        return Ok(Vec::new());
    };

    handle
        .join()
        .map_err(|_| MeasureError::Io {
            name: config.name.clone(),
            source: std::io::Error::other("measure output reader thread panicked"),
        })?
        .map_err(|source| MeasureError::Io {
            name: config.name.clone(),
            source,
        })
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    let pgid = Pid::from_raw(child.id() as i32);
    // Measures run in their own process group so timeout cleanup can reach descendants.
    if signal::killpg(pgid, Signal::SIGKILL).is_err() {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) {
    let _ = child.kill();
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
    fn extract(&self, output: &MeasureOutput) -> Result<Metrics, autotune_adaptor::AdaptorError> {
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
