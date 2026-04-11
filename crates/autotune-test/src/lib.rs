use autotune_config::TestConfig;
use std::path::Path;
use std::process::Command;
use std::time::Instant;
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

    let program = &config.command[0];
    let args = &config.command[1..];

    let output = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .output()
        .map_err(|source| TestError::Io {
            name: config.name.clone(),
            source,
        })?;

    let duration = start.elapsed().as_secs_f64();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
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
