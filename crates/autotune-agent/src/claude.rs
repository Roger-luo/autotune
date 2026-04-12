use crate::{
    Agent, AgentConfig, AgentConfigWithEvents, AgentError, AgentEvent, AgentResponse, AgentSession,
    EventHandler, ToolPermission,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

    /// Run claude with `--output-format stream-json`, forwarding events to the handler.
    fn run_claude_streaming(
        &self,
        args: &[String],
        cwd: &Path,
        event_handler: &EventHandler,
    ) -> Result<AgentResponse, AgentError> {
        // Replace --output-format json with stream-json and add --verbose
        let mut args: Vec<String> = args
            .iter()
            .map(|a| {
                if a == "json" {
                    "stream-json".to_string()
                } else {
                    a.clone()
                }
            })
            .collect();
        args.push("--verbose".to_string());

        let mut child = Command::new(&self.command)
            .args(&args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| AgentError::Io { source })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::CommandFailed {
                message: "failed to capture claude stdout".to_string(),
            })?;

        let stderr_handle = child.stderr.take();

        let mut final_result: Option<AgentResponse> = None;
        let reader = std::io::BufReader::new(stdout);

        for line in reader.lines() {
            let line = line.map_err(|source| AgentError::Io { source })?;
            if line.is_empty() {
                continue;
            }

            let event: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");

            match event_type {
                "assistant" => {
                    // Tool use events in assistant messages
                    if let Some(arr) = event
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(Value::as_array)
                    {
                        for block in arr {
                            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                                let tool = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown")
                                    .to_string();
                                let input_summary = Self::summarize_tool_input(block.get("input"));
                                event_handler(AgentEvent::ToolUse {
                                    tool,
                                    input_summary,
                                });
                            }
                        }
                    }
                }
                "content_block_start" => {
                    if let Some(block) = event.get("content_block")
                        && block.get("type").and_then(Value::as_str) == Some("tool_use")
                    {
                        let tool = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        event_handler(AgentEvent::ToolUse {
                            tool,
                            input_summary: String::new(),
                        });
                    }
                }
                "result" => {
                    // Final result — same shape as non-streaming JSON output
                    let session_id = event
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let text = event
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    final_result = Some(AgentResponse { text, session_id });
                }
                _ => {}
            }
        }

        let status = child.wait().map_err(|source| AgentError::Io { source })?;

        if !status.success() {
            let stderr = stderr_handle
                .map(|mut h| {
                    let mut buf = String::new();
                    let _ = std::io::Read::read_to_string(&mut h, &mut buf);
                    buf
                })
                .unwrap_or_default();
            return Err(AgentError::CommandFailed {
                message: format!("claude exited with {}: {}", status, stderr.trim()),
            });
        }

        final_result.ok_or_else(|| AgentError::ParseFailed {
            message: "claude stream ended without a result event".to_string(),
        })
    }

    /// Summarize tool input for display (e.g., file path for Read, pattern for Grep).
    fn summarize_tool_input(input: Option<&Value>) -> String {
        let Some(input) = input else {
            return String::new();
        };
        // Common tool input fields
        if let Some(path) = input.get("file_path").and_then(Value::as_str) {
            return path.to_string();
        }
        if let Some(pattern) = input.get("pattern").and_then(Value::as_str) {
            return format!("pattern: {}", pattern);
        }
        if let Some(command) = input.get("command").and_then(Value::as_str) {
            return command.to_string();
        }
        String::new()
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

    fn spawn_streaming(
        &self,
        config_with_events: AgentConfigWithEvents,
    ) -> Result<AgentResponse, AgentError> {
        let config = &config_with_events.config;
        let args = Self::build_args(config, None);
        let response = if let Some(ref handler) = config_with_events.event_handler {
            self.run_claude_streaming(&args, &config.working_directory, handler)?
        } else {
            self.run_claude(&args, &config.working_directory)?
        };
        self.remember_session(&response.session_id, config)?;
        Ok(response)
    }

    fn send_streaming(
        &self,
        session: &AgentSession,
        message: &str,
        event_handler: Option<&EventHandler>,
    ) -> Result<AgentResponse, AgentError> {
        let config = self.config_for_session(&session.session_id, message)?;
        let args = Self::build_args(&config, Some(&session.session_id));
        let response = if let Some(handler) = event_handler {
            self.run_claude_streaming(&args, &config.working_directory, handler)?
        } else {
            self.run_claude(&args, &config.working_directory)?
        };
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
