use crate::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession, ToolPermission};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

pub struct ClaudeAgent;

impl ClaudeAgent {
    pub fn new() -> Self {
        Self
    }

    fn build_args(config: &AgentConfig, session_id: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            config.prompt.clone(),
            "--output-format".to_string(),
            "json".to_string(),
        ];

        if let Some(sid) = session_id {
            args.push("-r".to_string());
            args.push(sid.to_string());
        }

        if let Some(model) = &config.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        if let Some(turns) = config.max_turns {
            args.push("--max-turns".to_string());
            args.push(turns.to_string());
        }

        for perm in &config.allowed_tools {
            match perm {
                ToolPermission::Allow(tool) => {
                    args.push("--allowedTools".to_string());
                    args.push(tool.clone());
                }
                ToolPermission::AllowScoped(tool, path) => {
                    args.push("--allowedTools".to_string());
                    args.push(format!("{tool}:{path}"));
                }
                ToolPermission::Deny(tool) => {
                    args.push("--disallowedTools".to_string());
                    args.push(tool.clone());
                }
            }
        }

        args
    }

    fn extract_response(stdout: &str) -> AgentResponse {
        let parsed = serde_json::from_str::<Value>(stdout).ok();
        let session_id = parsed
            .as_ref()
            .and_then(|value| value.get("session_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let text = parsed
            .as_ref()
            .and_then(|value| value.get("result"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| stdout.to_string());

        AgentResponse { text, session_id }
    }

    fn run_claude(args: &[String], cwd: &Path) -> Result<AgentResponse, AgentError> {
        let output = Command::new("claude")
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|source| AgentError::Io { source })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AgentError::CommandFailed {
                message: format!("claude exited with {}: {}", output.status, stderr),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(Self::extract_response(&stdout))
    }
}

impl Default for ClaudeAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for ClaudeAgent {
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        let args = Self::build_args(config, None);
        Self::run_claude(&args, &config.working_directory)
    }

    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError> {
        let config = AgentConfig {
            prompt: message.to_string(),
            allowed_tools: vec![],
            working_directory: std::path::PathBuf::from("."),
            model: None,
            max_turns: None,
        };
        let args = Self::build_args(&config, Some(&session.session_id));
        Self::run_claude(&args, &config.working_directory)
    }

    fn backend_name(&self) -> &str {
        "claude"
    }

    fn handover_command(&self, session: &AgentSession) -> String {
        format!("claude -r {}", session.session_id)
    }
}
