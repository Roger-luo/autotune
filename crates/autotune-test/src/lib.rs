use autotune_config::TestConfig;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TestError {
    #[error("test '{name}' failed (exit code {code})")]
    Failed {
        name: String,
        code: i32,
        stdout: String,
        stderr: String,
    },

    #[error("test '{name}' timed out after {timeout}s")]
    Timeout { name: String, timeout: u64 },

    #[error("IO error running test '{name}': {source}")]
    Io {
        name: String,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub duration_secs: f64,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_test(config: &TestConfig, working_dir: &Path) -> Result<TestResult, TestError> {
    let start = Instant::now();
    let timeout = Duration::from_secs(config.timeout);

    let program = &config.command[0];
    let args = &config.command[1..];

    let mut child = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| TestError::Io {
            name: config.name.clone(),
            source,
        })?;

    let stdout_reader = spawn_output_reader(child.stdout.take(), &config.name)?;
    let stderr_reader = spawn_output_reader(child.stderr.take(), &config.name)?;

    let status = loop {
        if let Some(status) = child.try_wait().map_err(|source| TestError::Io {
            name: config.name.clone(),
            source,
        })? {
            break status;
        }

        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            drop(stdout_reader);
            drop(stderr_reader);
            return Err(TestError::Timeout {
                name: config.name.clone(),
                timeout: config.timeout,
            });
        }

        thread::sleep(Duration::from_millis(10));
    };

    let duration = start.elapsed().as_secs_f64();
    let stdout = collect_output(stdout_reader, &config.name)?;
    let stderr = collect_output(stderr_reader, &config.name)?;

    if status.success() {
        Ok(TestResult {
            name: config.name.clone(),
            passed: true,
            duration_secs: duration,
            stdout,
            stderr,
        })
    } else {
        Ok(TestResult {
            name: config.name.clone(),
            passed: false,
            duration_secs: duration,
            stdout,
            stderr,
        })
    }
}

fn spawn_output_reader(
    stream: Option<impl Read + Send + 'static>,
    test_name: &str,
) -> Result<JoinHandle<Result<Vec<u8>, TestError>>, TestError> {
    let Some(mut stream) = stream else {
        return Err(TestError::Io {
            name: test_name.to_string(),
            source: std::io::Error::other("child output pipe was not captured"),
        });
    };
    let test_name = test_name.to_string();

    Ok(thread::spawn(move || {
        let mut bytes = Vec::new();
        stream
            .read_to_end(&mut bytes)
            .map_err(|source| TestError::Io {
                name: test_name,
                source,
            })?;
        Ok(bytes)
    }))
}

fn collect_output(
    reader: JoinHandle<Result<Vec<u8>, TestError>>,
    test_name: &str,
) -> Result<String, TestError> {
    let bytes = reader.join().map_err(|_| TestError::Io {
        name: test_name.to_string(),
        source: std::io::Error::other("child output reader thread panicked"),
    })??;

    Ok(String::from_utf8_lossy(&bytes).to_string())
}

pub fn run_all_tests(
    configs: &[TestConfig],
    working_dir: &Path,
) -> Result<Vec<TestResult>, TestError> {
    let mut results = Vec::new();

    for config in configs {
        let result = run_test(config, working_dir)?;
        let passed = result.passed;
        results.push(result);
        if !passed {
            break;
        }
    }

    Ok(results)
}

pub fn all_passed(results: &[TestResult]) -> bool {
    results.iter().all(|r| r.passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(passed: bool) -> TestResult {
        TestResult {
            name: "test".to_string(),
            passed,
            duration_secs: 0.0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn make_config(name: &str, cmd: &[&str]) -> TestConfig {
        TestConfig {
            name: name.to_string(),
            command: cmd.iter().map(|s| s.to_string()).collect(),
            timeout: 30,
        }
    }

    #[test]
    fn all_passed_empty_slice_returns_true() {
        assert!(all_passed(&[]));
    }

    #[test]
    fn all_passed_all_passing() {
        let results = vec![make_result(true), make_result(true)];
        assert!(all_passed(&results));
    }

    #[test]
    fn all_passed_mixed_returns_false() {
        let results = vec![make_result(true), make_result(false)];
        assert!(!all_passed(&results));
    }

    #[test]
    fn all_passed_single_failure() {
        let results = vec![make_result(false)];
        assert!(!all_passed(&results));
    }

    #[test]
    fn run_test_passing_command() {
        let tmp = std::env::temp_dir();
        let config = make_config("pass", &["sh", "-c", "exit 0"]);
        let result = run_test(&config, &tmp).unwrap();
        assert!(result.passed);
        assert_eq!(result.name, "pass");
    }

    #[test]
    fn run_test_failing_command_returns_not_passed() {
        let tmp = std::env::temp_dir();
        let config = make_config("fail", &["sh", "-c", "exit 1"]);
        let result = run_test(&config, &tmp).unwrap();
        assert!(!result.passed);
        assert_eq!(result.name, "fail");
    }

    #[test]
    fn run_all_tests_short_circuits_after_first_failure() {
        let tmp = std::env::temp_dir();
        // second test succeeds but should never run
        let configs = vec![
            make_config("fail", &["sh", "-c", "exit 1"]),
            make_config("pass", &["sh", "-c", "exit 0"]),
        ];
        let results = run_all_tests(&configs, &tmp).unwrap();
        // Only one result — stopped after the failure
        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
    }

    #[test]
    fn run_all_tests_all_pass_returns_all_results() {
        let tmp = std::env::temp_dir();
        let configs = vec![
            make_config("a", &["sh", "-c", "exit 0"]),
            make_config("b", &["sh", "-c", "exit 0"]),
        ];
        let results = run_all_tests(&configs, &tmp).unwrap();
        assert_eq!(results.len(), 2);
        assert!(all_passed(&results));
    }
}
