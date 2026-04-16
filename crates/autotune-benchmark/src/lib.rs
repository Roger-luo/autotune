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

/// Result of running a single measure: the raw stdout/stderr plus the
/// extracted metrics. Raw output is retained so callers can persist it for
/// later inspection (e.g. by a research agent looking for context beyond the
/// summary metrics).
#[derive(Debug, Clone)]
pub struct MeasureReport {
    pub name: String,
    pub stdout: String,
    pub stderr: String,
    pub metrics: Metrics,
}

/// Run a single measure command and extract metrics.
pub fn run_measure(config: &MeasureConfig, working_dir: &Path) -> Result<Metrics, MeasureError> {
    run_measure_with_output(config, working_dir).map(|r| r.metrics)
}

/// Run a single measure command, returning the extracted metrics along with
/// the raw stdout/stderr captured during the run.
pub fn run_measure_with_output(
    config: &MeasureConfig,
    working_dir: &Path,
) -> Result<MeasureReport, MeasureError> {
    let output = run_command_with_timeout(config, working_dir)?;

    if !output.status.success() {
        return Err(MeasureError::CommandFailed {
            name: config.name.clone(),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let bench_output = MeasureOutput {
        stdout: stdout.clone(),
        stderr: stderr.clone(),
    };

    let adaptor = build_adaptor(&config.adaptor, working_dir);
    let metrics = adaptor
        .extract(&bench_output)
        .map_err(|source| MeasureError::Extraction {
            name: config.name.clone(),
            source,
        })?;

    Ok(MeasureReport {
        name: config.name.clone(),
        stdout,
        stderr,
        metrics,
    })
}

/// Run all configured measures and merge their metrics.
pub fn run_all_measures(
    configs: &[MeasureConfig],
    working_dir: &Path,
) -> Result<Metrics, MeasureError> {
    run_all_measures_with_output(configs, working_dir).map(|(metrics, _)| metrics)
}

/// Run all configured measures, returning the merged metrics and the per-measure
/// raw output reports (in the order the measures were configured).
pub fn run_all_measures_with_output(
    configs: &[MeasureConfig],
    working_dir: &Path,
) -> Result<(Metrics, Vec<MeasureReport>), MeasureError> {
    let mut all_metrics = HashMap::new();
    let mut reports = Vec::with_capacity(configs.len());

    for config in configs {
        let report = run_measure_with_output(config, working_dir)?;
        all_metrics.extend(report.metrics.clone());
        reports.push(report);
    }

    Ok((all_metrics, reports))
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

#[cfg(test)]
mod tests {
    use super::*;
    use autotune_adaptor::AdaptorError;
    use autotune_config::{AdaptorConfig, MeasureConfig, RegexPattern};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn script_adaptor(command: Vec<String>, working_dir: &Path) -> ScriptAdaptorWithWorkingDir {
        ScriptAdaptorWithWorkingDir::new(command, working_dir.to_path_buf())
    }

    #[test]
    fn build_adaptor_regex_produces_regex_adaptor() {
        let config = AdaptorConfig::Regex {
            patterns: vec![RegexPattern {
                name: "m".to_string(),
                pattern: "([0-9]+)".to_string(),
            }],
        };
        let adaptor = build_adaptor(&config, Path::new("."));
        let output = MeasureOutput {
            stdout: "value: 42\n".to_string(),
            stderr: String::new(),
        };
        let metrics = adaptor.extract(&output).unwrap();
        assert_eq!(*metrics.get("m").unwrap(), 42.0);
    }

    #[test]
    fn build_adaptor_script_returns_adaptor() {
        let config = AdaptorConfig::Script {
            command: vec!["echo".to_string()],
        };
        let _adaptor = build_adaptor(&config, Path::new("."));
        // Just verify no panic on construction.
    }

    #[test]
    fn script_adaptor_extract_rejects_empty_command() {
        let tmp = tempfile::tempdir().unwrap();
        let adaptor = script_adaptor(Vec::new(), tmp.path());
        let output = MeasureOutput {
            stdout: "ignored".to_string(),
            stderr: "ignored".to_string(),
        };

        let err = adaptor.extract(&output).unwrap_err();

        assert!(matches!(err, AdaptorError::ScriptEmptyCommand));
    }

    #[test]
    fn script_adaptor_extract_passes_combined_output_and_working_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("marker.txt"), "present").unwrap();
        let script = tmp.path().join("extract.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
test -f marker.txt || exit 1
bytes=$(cat | wc -c | tr -d ' ')
echo "{\"stdin_bytes\": $bytes, \"pwd_ok\": 1}"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
        let adaptor = script_adaptor(vec![script.display().to_string()], tmp.path());
        let output = MeasureOutput {
            stdout: "alpha".to_string(),
            stderr: "beta".to_string(),
        };

        let metrics = adaptor.extract(&output).unwrap();

        assert_eq!(metrics.get("stdin_bytes"), Some(&10.0));
        assert_eq!(metrics.get("pwd_ok"), Some(&1.0));
    }

    #[test]
    fn script_adaptor_extract_surfaces_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let adaptor = script_adaptor(
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'script blew up' >&2\nexit 7".to_string(),
            ],
            tmp.path(),
        );
        let output = MeasureOutput {
            stdout: String::new(),
            stderr: String::new(),
        };

        let err = adaptor.extract(&output).unwrap_err();

        assert!(
            matches!(err, AdaptorError::ScriptFailed { code, ref stderr } if code == 7 && stderr.contains("script blew up"))
        );
    }

    #[test]
    fn run_measure_returns_error_on_command_failure() {
        let config = MeasureConfig {
            name: "fail-test".to_string(),
            command: vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()],
            timeout: 30,
            adaptor: AdaptorConfig::Regex { patterns: vec![] },
        };
        let tmp = tempfile::tempdir().unwrap();
        let result = run_measure(&config, tmp.path());
        assert!(
            matches!(result, Err(MeasureError::CommandFailed { ref name, .. }) if name == "fail-test"),
            "expected CommandFailed, got: {result:?}"
        );
    }

    #[test]
    fn run_measure_extracts_metrics_on_success() {
        let config = MeasureConfig {
            name: "score-test".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'score: 99.5'".to_string(),
            ],
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "metric-name".to_string(),
                    pattern: r"score: ([0-9.]+)".to_string(),
                }],
            },
        };
        let tmp = tempfile::tempdir().unwrap();
        let metrics = run_measure(&config, tmp.path()).unwrap();
        assert_eq!(*metrics.get("metric-name").unwrap(), 99.5);
    }

    #[test]
    fn run_measure_with_output_returns_timeout_error() {
        let config = MeasureConfig {
            name: "timeout-test".to_string(),
            command: vec!["sh".to_string(), "-c".to_string(), "sleep 1".to_string()],
            timeout: 0,
            adaptor: AdaptorConfig::Regex { patterns: vec![] },
        };
        let tmp = tempfile::tempdir().unwrap();

        let result = run_measure_with_output(&config, tmp.path());

        assert!(
            matches!(result, Err(MeasureError::TimedOut { ref name, timeout }) if name == "timeout-test" && timeout == 0),
            "expected TimedOut, got: {result:?}"
        );
    }

    #[test]
    fn run_measure_with_output_maps_extraction_failures() {
        let config = MeasureConfig {
            name: "extract-fail-test".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'not a matching metric'".to_string(),
            ],
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "score".to_string(),
                    pattern: r"score: ([0-9.]+)".to_string(),
                }],
            },
        };
        let tmp = tempfile::tempdir().unwrap();

        let result = run_measure_with_output(&config, tmp.path());

        assert!(
            matches!(result, Err(MeasureError::Extraction { ref name, .. }) if name == "extract-fail-test"),
            "expected Extraction, got: {result:?}"
        );
    }

    #[test]
    fn run_measure_with_output_returns_report_with_stdout() {
        let config = MeasureConfig {
            name: "output-test".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'val: 7'".to_string(),
            ],
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "val".to_string(),
                    pattern: r"val: ([0-9]+)".to_string(),
                }],
            },
        };
        let tmp = tempfile::tempdir().unwrap();
        let report = run_measure_with_output(&config, tmp.path()).unwrap();
        assert_eq!(report.name, "output-test");
        assert!(
            report.stdout.contains("val: 7"),
            "stdout: {:?}",
            report.stdout
        );
        assert_eq!(*report.metrics.get("val").unwrap(), 7.0);
    }

    #[test]
    fn run_all_measures_with_output_returns_per_measure_reports() {
        let m1 = MeasureConfig {
            name: "alpha".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'x: 10'".to_string(),
            ],
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "x".to_string(),
                    pattern: r"x: ([0-9]+)".to_string(),
                }],
            },
        };
        let m2 = MeasureConfig {
            name: "beta".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'y: 20'".to_string(),
            ],
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "y".to_string(),
                    pattern: r"y: ([0-9]+)".to_string(),
                }],
            },
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_metrics, reports) = run_all_measures_with_output(&[m1, m2], tmp.path()).unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].name, "alpha");
        assert!(reports[0].stdout.contains("x: 10"));
        assert_eq!(reports[1].name, "beta");
        assert!(reports[1].stdout.contains("y: 20"));
    }

    #[test]
    fn run_all_measures_merges_metrics() {
        let m1 = MeasureConfig {
            name: "first".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'a: 1'".to_string(),
            ],
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "metric-a".to_string(),
                    pattern: r"a: ([0-9]+)".to_string(),
                }],
            },
        };
        let m2 = MeasureConfig {
            name: "second".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'b: 2'".to_string(),
            ],
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "metric-b".to_string(),
                    pattern: r"b: ([0-9]+)".to_string(),
                }],
            },
        };
        let tmp = tempfile::tempdir().unwrap();
        let metrics = run_all_measures(&[m1, m2], tmp.path()).unwrap();
        assert_eq!(*metrics.get("metric-a").unwrap(), 1.0);
        assert_eq!(*metrics.get("metric-b").unwrap(), 2.0);
    }
}
