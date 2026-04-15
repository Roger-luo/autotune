use crate::{ScoreCalculator, ScoreError, ScoreInput, ScoreOutput};
use std::io::Write;
use std::process::{Command, Stdio};

pub struct ScriptScorer {
    command: Vec<String>,
}

impl ScriptScorer {
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }
}

impl ScoreCalculator for ScriptScorer {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError> {
        let program = self.command.first().ok_or_else(|| ScoreError::Io {
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"),
        })?;

        let mut child = Command::new(program)
            .args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            let json = serde_json::to_vec(input)
                .map_err(|source| ScoreError::ScriptOutputParse { source })?;
            stdin.write_all(&json)?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(ScoreError::ScriptFailed {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        serde_json::from_slice(&output.stdout)
            .map_err(|source| ScoreError::ScriptOutputParse { source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScoreCalculator, ScoreError, ScoreInput};

    fn empty_input() -> ScoreInput {
        ScoreInput {
            baseline: std::collections::HashMap::new(),
            candidate: std::collections::HashMap::new(),
            best: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn empty_command_errors() {
        let scorer = ScriptScorer::new(vec![]);
        let err = scorer.calculate(&empty_input()).unwrap_err();
        assert!(matches!(err, ScoreError::Io { .. }));
    }

    #[test]
    fn nonzero_exit_errors() {
        let scorer = ScriptScorer::new(vec![
            "sh".to_string(), "-c".to_string(), "exit 42".to_string(),
        ]);
        let err = scorer.calculate(&empty_input()).unwrap_err();
        assert!(matches!(err, ScoreError::ScriptFailed { code: 42, .. }));
    }

    #[test]
    fn bad_json_output_errors() {
        let scorer = ScriptScorer::new(vec![
            "sh".to_string(), "-c".to_string(), "echo 'not json at all'".to_string(),
        ]);
        let err = scorer.calculate(&empty_input()).unwrap_err();
        assert!(matches!(err, ScoreError::ScriptOutputParse { .. }));
    }
}
