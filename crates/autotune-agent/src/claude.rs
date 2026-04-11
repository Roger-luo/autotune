use crate::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession, ToolPermission};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

pub struct ClaudeAgent {
    command: PathBuf,
    sessions: Mutex<HashMap<String, SessionContext>>,
}

#[derive(Debug, Clone)]
struct SessionContext {
    allowed_tools: Vec<ToolPermission>,
    working_directory: PathBuf,
    model: Option<String>,
    max_turns: Option<u64>,
}

impl ClaudeAgent {
    pub fn new() -> Self {
        Self {
            command: PathBuf::from("claude"),
            sessions: Mutex::new(HashMap::new()),
        }
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

    fn parse_response(stdout: &str) -> Result<AgentResponse, AgentError> {
        let parsed =
            serde_json::from_str::<Value>(stdout).map_err(|source| AgentError::ParseFailed {
                message: format!("invalid claude JSON output: {source}"),
            })?;
        let session_id = parsed
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::ParseFailed {
                message: "claude JSON missing string field 'session_id'".to_string(),
            })?;
        let text = parsed
            .get("result")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::ParseFailed {
                message: "claude JSON missing string field 'result'".to_string(),
            })?;

        Ok(AgentResponse {
            text: text.to_string(),
            session_id: session_id.to_string(),
        })
    }

    fn remember_session(&self, session_id: &str, config: &AgentConfig) -> Result<(), AgentError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AgentError::CommandFailed {
                message: "claude session state unavailable".to_string(),
            })?;
        sessions.insert(
            session_id.to_string(),
            SessionContext {
                allowed_tools: config.allowed_tools.clone(),
                working_directory: config.working_directory.clone(),
                model: config.model.clone(),
                max_turns: config.max_turns,
            },
        );
        Ok(())
    }

    fn config_for_session(
        &self,
        session_id: &str,
        message: &str,
    ) -> Result<AgentConfig, AgentError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| AgentError::CommandFailed {
                message: "claude session state unavailable".to_string(),
            })?;
        let context = sessions
            .get(session_id)
            .ok_or_else(|| AgentError::CommandFailed {
                message: format!("missing claude session context for '{session_id}'"),
            })?;

        Ok(AgentConfig {
            prompt: message.to_string(),
            allowed_tools: context.allowed_tools.clone(),
            working_directory: context.working_directory.clone(),
            model: context.model.clone(),
            max_turns: context.max_turns,
        })
    }

    fn run_claude(&self, args: &[String], cwd: &Path) -> Result<AgentResponse, AgentError> {
        let output = Command::new(&self.command)
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
        Self::parse_response(&stdout)
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
        let response = self.run_claude(&args, &config.working_directory)?;
        self.remember_session(&response.session_id, config)?;
        Ok(response)
    }

    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError> {
        let config = self.config_for_session(&session.session_id, message)?;
        let args = Self::build_args(&config, Some(&session.session_id));
        let response = self.run_claude(&args, &config.working_directory)?;
        self.remember_session(&response.session_id, &config)?;
        Ok(response)
    }

    fn backend_name(&self) -> &str {
        "claude"
    }

    fn handover_command(&self, session: &AgentSession) -> String {
        format!("claude -r {}", session.session_id)
    }
}
