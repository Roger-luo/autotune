use crate::{AdaptorError, BenchmarkOutput, MetricAdaptor, Metrics};
use std::io::Write;
use std::process::{Command, Stdio};

/// Runs a user-provided script that reads benchmark output from stdin
/// and writes JSON metrics to stdout.
pub struct ScriptAdaptor {
    command: Vec<String>,
}

impl ScriptAdaptor {
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }
}

impl MetricAdaptor for ScriptAdaptor {
    fn extract(&self, output: &BenchmarkOutput) -> Result<Metrics, AdaptorError> {
        let program = &self.command[0];
        let args = &self.command[1..];

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| AdaptorError::Io { source })?;

        if let Some(mut stdin) = child.stdin.take() {
            let combined = format!("{}\n{}", output.stdout, output.stderr);
            stdin
                .write_all(combined.as_bytes())
                .map_err(|source| AdaptorError::Io { source })?;
        }

        let result = child
            .wait_with_output()
            .map_err(|source| AdaptorError::Io { source })?;

        if !result.status.success() {
            return Err(AdaptorError::ScriptFailed {
                code: result.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&result.stdout);
        let metrics: Metrics = serde_json::from_str(&stdout)
            .map_err(|source| AdaptorError::ScriptOutputParse { source })?;

        Ok(metrics)
    }
}
