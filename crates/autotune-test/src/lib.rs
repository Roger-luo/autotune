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
