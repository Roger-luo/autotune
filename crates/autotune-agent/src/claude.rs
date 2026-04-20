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
    reasoning_effort: Option<String>,
}

impl ClaudeAgent {
    pub fn new() -> Self {
        Self {
            command: PathBuf::from("claude"),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Create with a custom binary path (useful for testing with mock binaries).
    pub fn with_command(command: PathBuf) -> Self {
        Self {
            command,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn build_args(config: &AgentConfig, session_id: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            config.prompt.clone(),
            "--output-format".to_string(),
            "json".to_string(),
            // Why --dangerously-skip-permissions instead of --permission-mode dontAsk:
            //
            // We need scoped tool permissions like `Edit:/worktree/crates/**/*.rs`
            // to restrict the implementation agent to its worktree. The scoped
            // syntax `Tool:path` is NOT supported by dontAsk mode — it silently
            // rejects the tool even when listed in --allowedTools.
            //
            // --dangerously-skip-permissions bypasses the interactive permission
            // *prompt* but does NOT override tool-level restrictions:
            //   - --disallowedTools still blocks denied tools entirely
            //   - --allowedTools scoped paths (Edit:/path) still restrict edits
            // Tested: `--dangerously-skip-permissions --disallowedTools Bash`
            // correctly prevents the agent from using Bash.
            "--dangerously-skip-permissions".to_string(),
            // Prevent agents from invoking skills/slash-commands which could
            // cause unexpected side effects.
            // NOTE: --bare is NOT used because it disables OAuth/keychain auth,
            // requiring ANTHROPIC_API_KEY to be set. Most users authenticate
            // via OAuth or the system keychain.
            "--disable-slash-commands".to_string(),
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
                reasoning_effort: config.reasoning_effort.clone(),
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
            reasoning_effort: context.reasoning_effort.clone(),
        })
    }

    fn run_claude(&self, args: &[String], cwd: &Path) -> Result<AgentResponse, AgentError> {
        let _guard = crate::terminal::Guard::new();
        let output = Command::new(&self.command)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|source| AgentError::Io { source })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut details = String::new();
            if !stderr.trim().is_empty() {
                details.push_str(&format!("\nstderr: {}", stderr.trim()));
            }
            if !stdout.trim().is_empty() {
                details.push_str(&format!("\nstdout: {}", stdout.trim()));
            }
            if details.is_empty() {
                details.push_str(" (no output)");
            }
            return Err(AgentError::CommandFailed {
                message: format!(
                    "claude exited with {}\nargs: {:?}{}",
                    output.status, args, details
                ),
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
        let _guard = crate::terminal::Guard::new();
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
                    // Content blocks in assistant messages
                    if let Some(arr) = event
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(Value::as_array)
                    {
                        for block in arr {
                            match block.get("type").and_then(Value::as_str) {
                                Some("tool_use") => {
                                    let tool = block
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown")
                                        .to_string();
                                    let input_summary =
                                        Self::summarize_tool_input(block.get("input"));
                                    event_handler(AgentEvent::ToolUse {
                                        tool,
                                        input_summary,
                                    });
                                }
                                Some("text") => {
                                    if let Some(text) = block.get("text").and_then(Value::as_str)
                                        && !text.is_empty()
                                    {
                                        event_handler(AgentEvent::Text(text.to_string()));
                                    }
                                }
                                _ => {}
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
                "content_block_delta" => {
                    // Streaming text deltas
                    if let Some(text) = event
                        .get("delta")
                        .filter(|d| d.get("type").and_then(Value::as_str) == Some("text_delta"))
                        .and_then(|d| d.get("text"))
                        .and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        event_handler(AgentEvent::Text(text.to_string()));
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
            // Distinguish "killed by SIGINT" (user pressed Ctrl+C) from a real
            // command failure. On Unix, signal exits have no exit code and
            // their signal() is Some(2) for SIGINT.
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                // SIGINT is signal 2 on every POSIX platform.
                if status.signal() == Some(2) {
                    return Err(AgentError::Interrupted);
                }
            }
            let stderr = stderr_handle
                .map(|mut h| {
                    let mut buf = String::new();
                    let _ = std::io::Read::read_to_string(&mut h, &mut buf);
                    buf
                })
                .unwrap_or_default();
            let details = if stderr.trim().is_empty() {
                " (no stderr output)".to_string()
            } else {
                format!("\nstderr: {}", stderr.trim())
            };
            return Err(AgentError::CommandFailed {
                message: format!("claude exited with {}\nargs: {:?}{}", status, args, details),
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
        trace_spawn(config, &response);
        Ok(response)
    }

    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError> {
        // Claude CLI treats an empty prompt with -r as "resume deferred tool",
        // which fails if the session completed normally. Use a fallback prompt.
        let message = if message.trim().is_empty() {
            "Continue."
        } else {
            message
        };
        let config = self.config_for_session(&session.session_id, message)?;
        let args = Self::build_args(&config, Some(&session.session_id));
        let response = self.run_claude(&args, &config.working_directory)?;
        self.remember_session(&response.session_id, &config)?;
        trace_send(session, message, &response);
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
        trace_spawn(config, &response);
        Ok(response)
    }

    fn send_streaming(
        &self,
        session: &AgentSession,
        message: &str,
        event_handler: Option<&EventHandler>,
    ) -> Result<AgentResponse, AgentError> {
        // Claude CLI treats an empty prompt with -r as "resume deferred tool",
        // which fails if the session completed normally. Use a fallback prompt.
        let message = if message.trim().is_empty() {
            "Continue."
        } else {
            message
        };
        let config = self.config_for_session(&session.session_id, message)?;
        let args = Self::build_args(&config, Some(&session.session_id));
        let response = if let Some(handler) = event_handler {
            self.run_claude_streaming(&args, &config.working_directory, handler)?
        } else {
            self.run_claude(&args, &config.working_directory)?
        };
        self.remember_session(&response.session_id, &config)?;
        trace_send(session, message, &response);
        Ok(response)
    }

    fn backend_name(&self) -> &str {
        "claude"
    }

    fn handover_command(&self, session: &AgentSession) -> String {
        format!("claude -r {}", session.session_id)
    }

    fn hydrate_session(
        &self,
        session: &AgentSession,
        config: &AgentConfig,
    ) -> Result<(), AgentError> {
        self.remember_session(&session.session_id, config)
    }

    fn grant_session_permission(
        &self,
        session: &AgentSession,
        permission: ToolPermission,
    ) -> Result<(), AgentError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AgentError::CommandFailed {
                message: "claude session state unavailable".to_string(),
            })?;
        let ctx =
            sessions
                .get_mut(&session.session_id)
                .ok_or_else(|| AgentError::CommandFailed {
                    message: format!(
                        "cannot grant permission — no session context for '{}'",
                        session.session_id
                    ),
                })?;
        ctx.allowed_tools.push(permission);
        Ok(())
    }
}

/// Emit one `agent.spawn` trace record. Captures prompt + response verbatim
/// so a trace file is enough to replay a run end-to-end.
fn trace_spawn(config: &AgentConfig, response: &AgentResponse) {
    if !crate::trace::is_enabled() {
        return;
    }
    crate::trace::record(
        "agent.spawn",
        serde_json::json!({
            "backend": "claude",
            "working_dir": config.working_directory.display().to_string(),
            "model": config.model,
            "max_turns": config.max_turns,
            "prompt": config.prompt,
            "response_session_id": response.session_id,
            "response_text": response.text,
        }),
    );
}

/// Emit one `agent.send` trace record.
fn trace_send(session: &AgentSession, message: &str, response: &AgentResponse) {
    if !crate::trace::is_enabled() {
        return;
    }
    crate::trace::record(
        "agent.send",
        serde_json::json!({
            "backend": session.backend,
            "session_id": session.session_id,
            "message": message,
            "response_session_id": response.session_id,
            "response_text": response.text,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn config_with_tools(allowed_tools: Vec<ToolPermission>) -> AgentConfig {
        AgentConfig {
            prompt: "prompt".to_string(),
            allowed_tools,
            working_directory: PathBuf::from("/tmp/worktree"),
            model: Some("sonnet".to_string()),
            max_turns: Some(7),
            reasoning_effort: Some("high".to_string()),
        }
    }

    fn session(id: &str) -> AgentSession {
        AgentSession {
            session_id: id.to_string(),
            backend: "claude".to_string(),
        }
    }

    struct ClaudeHarness {
        _tmp: TempDir,
        root: PathBuf,
        script_path: PathBuf,
        stdout_path: PathBuf,
        stderr_path: PathBuf,
        status_path: PathBuf,
    }

    impl ClaudeHarness {
        fn new(stdout: &str, stderr: &str, status: i32) -> Self {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().to_path_buf();
            let script_path = root.join("claude");
            let stdout_path = root.join("stdout.jsonl");
            let stderr_path = root.join("stderr.txt");
            let status_path = root.join("status.txt");

            fs::write(&stdout_path, stdout).unwrap();
            fs::write(&stderr_path, stderr).unwrap();
            fs::write(&status_path, status.to_string()).unwrap();
            fs::create_dir_all(root.join("workspace")).unwrap();

            let script = format!(
                "#!/bin/sh\ncat '{stdout}'\ncat '{stderr}' >&2\nexit \"$(cat '{status}')\"\n",
                stdout = stdout_path.display(),
                stderr = stderr_path.display(),
                status = status_path.display(),
            );
            fs::write(&script_path, script).unwrap();
            let mut perms = fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).unwrap();

            Self {
                _tmp: tmp,
                root,
                script_path,
                stdout_path,
                stderr_path,
                status_path,
            }
        }

        fn agent(&self) -> ClaudeAgent {
            ClaudeAgent::with_command(self.script_path.clone())
        }

        fn workspace(&self) -> PathBuf {
            self.root.join("workspace")
        }

        fn write_stdout(&self, stdout: &str) {
            fs::write(&self.stdout_path, stdout).unwrap();
        }

        fn write_stderr_and_status(&self, stderr: &str, status: i32) {
            fs::write(&self.stderr_path, stderr).unwrap();
            fs::write(&self.status_path, status.to_string()).unwrap();
        }
    }

    #[test]
    fn build_args_maps_config_and_session_into_claude_flags() {
        let args = ClaudeAgent::build_args(
            &config_with_tools(vec![
                ToolPermission::Allow("Read".to_string()),
                ToolPermission::AllowScoped(
                    "Edit".to_string(),
                    "/tmp/worktree/**/*.rs".to_string(),
                ),
                ToolPermission::Deny("Bash".to_string()),
            ]),
            Some("sess-123"),
        );

        assert_eq!(
            args,
            vec![
                "-p".to_string(),
                "prompt".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--disable-slash-commands".to_string(),
                "-r".to_string(),
                "sess-123".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
                "--max-turns".to_string(),
                "7".to_string(),
                "--allowedTools".to_string(),
                "Read".to_string(),
                "--allowedTools".to_string(),
                "Edit:/tmp/worktree/**/*.rs".to_string(),
                "--disallowedTools".to_string(),
                "Bash".to_string(),
            ]
        );
    }

    #[test]
    fn parse_response_requires_session_id_and_result() {
        let ok = ClaudeAgent::parse_response(r#"{"session_id":"sess-1","result":"done"}"#).unwrap();
        assert_eq!(ok.session_id, "sess-1");
        assert_eq!(ok.text, "done");

        assert!(matches!(
            ClaudeAgent::parse_response("not json"),
            Err(AgentError::ParseFailed { .. })
        ));
        assert!(matches!(
            ClaudeAgent::parse_response(r#"{"result":"done"}"#),
            Err(AgentError::ParseFailed { .. })
        ));
        assert!(matches!(
            ClaudeAgent::parse_response(r#"{"session_id":"sess-1"}"#),
            Err(AgentError::ParseFailed { .. })
        ));
    }

    #[test]
    fn summarize_tool_input_prefers_paths_then_pattern_then_command() {
        assert_eq!(
            ClaudeAgent::summarize_tool_input(Some(
                &serde_json::json!({ "file_path": "/tmp/a.rs" })
            )),
            "/tmp/a.rs"
        );
        assert_eq!(
            ClaudeAgent::summarize_tool_input(Some(&serde_json::json!({ "pattern": "needle" }))),
            "pattern: needle"
        );
        assert_eq!(
            ClaudeAgent::summarize_tool_input(Some(
                &serde_json::json!({ "command": "cargo test" })
            )),
            "cargo test"
        );
        assert_eq!(ClaudeAgent::summarize_tool_input(None), "");
    }

    #[test]
    fn remember_hydrate_and_grant_permission_update_session_context() {
        let original = config_with_tools(vec![ToolPermission::Allow("Read".to_string())]);
        let agent = ClaudeAgent::with_command(PathBuf::from("claude"));

        agent.remember_session("sess-1", &original).unwrap();
        agent
            .grant_session_permission(
                &session("sess-1"),
                ToolPermission::AllowScoped("Edit".to_string(), "/tmp/worktree".to_string()),
            )
            .unwrap();

        let resumed = agent.config_for_session("sess-1", "continue").unwrap();
        assert_eq!(resumed.prompt, "continue");
        assert_eq!(resumed.working_directory, original.working_directory);
        assert_eq!(resumed.model, original.model);
        assert_eq!(resumed.max_turns, original.max_turns);
        assert_eq!(resumed.reasoning_effort, original.reasoning_effort);
        assert_eq!(resumed.allowed_tools.len(), 2);

        let fresh = ClaudeAgent::with_command(PathBuf::from("claude"));
        fresh
            .hydrate_session(&session("sess-2"), &original)
            .unwrap();
        let hydrated = fresh.config_for_session("sess-2", "resumed").unwrap();
        assert_eq!(hydrated.prompt, "resumed");
        assert_eq!(hydrated.allowed_tools.len(), 1);
    }

    #[test]
    fn grant_session_permission_requires_known_session() {
        let agent = ClaudeAgent::with_command(PathBuf::from("claude"));
        let err = agent
            .grant_session_permission(
                &session("missing"),
                ToolPermission::Allow("Read".to_string()),
            )
            .unwrap_err();

        match err {
            AgentError::CommandFailed { message } => {
                assert!(message.contains("no session context for 'missing'"));
            }
            other => panic!("expected command failure, got {other:?}"),
        }
    }

    #[test]
    fn run_claude_streaming_emits_tool_use_and_text_events() {
        let harness = ClaudeHarness::new(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/input.rs"}},{"type":"text","text":"assistant text"}]}}
{"type":"content_block_start","content_block":{"type":"tool_use","name":"Grep","input":{"pattern":"needle"}}}
{"type":"content_block_delta","delta":{"type":"text_delta","text":"delta text"}}
{"type":"result","session_id":"sess-123","result":"final"}"#,
            "",
            0,
        );
        let agent = harness.agent();
        let seen = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
        let seen_for_handler = Arc::clone(&seen);
        let handler: EventHandler = Box::new(move |event| {
            seen_for_handler.lock().unwrap().push(event);
        });
        let mut config = config_with_tools(vec![]);
        config.working_directory = harness.workspace();
        let args = ClaudeAgent::build_args(&config, None);

        let response = agent
            .run_claude_streaming(&args, &config.working_directory, &handler)
            .unwrap();

        assert_eq!(response.session_id, "sess-123");
        assert_eq!(response.text, "final");

        let events = seen.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolUse { tool, input_summary }
            if tool == "Read" && input_summary == "/tmp/input.rs"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Text(text) if text == "assistant text"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolUse { tool, input_summary }
            if tool == "Grep" && input_summary.is_empty()
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Text(text) if text == "delta text"
        )));
    }

    #[test]
    fn run_claude_streaming_rejects_missing_result_event() {
        let harness = ClaudeHarness::new(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}"#,
            "",
            0,
        );
        let agent = harness.agent();
        let mut config = config_with_tools(vec![]);
        config.working_directory = harness.workspace();
        let args = ClaudeAgent::build_args(&config, None);
        let handler: EventHandler = Box::new(|_event| {});

        let err = agent
            .run_claude_streaming(&args, &config.working_directory, &handler)
            .unwrap_err();

        match err {
            AgentError::ParseFailed { message } => {
                assert!(message.contains("ended without a result event"));
            }
            other => panic!("expected parse failure, got {other:?}"),
        }
    }

    #[test]
    fn run_claude_streaming_reports_stderr_on_command_failure() {
        let harness = ClaudeHarness::new("", "permission denied", 1);
        let agent = harness.agent();
        let mut config = config_with_tools(vec![]);
        config.working_directory = harness.workspace();
        let args = ClaudeAgent::build_args(&config, None);
        let handler: EventHandler = Box::new(|_event| {});

        let err = agent
            .run_claude_streaming(&args, &config.working_directory, &handler)
            .unwrap_err();

        match err {
            AgentError::CommandFailed { message } => {
                assert!(message.contains("permission denied"));
                assert!(message.contains("--verbose"));
                assert!(message.contains("stream-json"));
            }
            other => panic!("expected command failure, got {other:?}"),
        }

        harness.write_stdout("");
        harness.write_stderr_and_status("", 1);
        let err = agent
            .run_claude_streaming(&args, &config.working_directory, &handler)
            .unwrap_err();
        match err {
            AgentError::CommandFailed { message } => {
                assert!(message.contains("(no stderr output)"));
            }
            other => panic!("expected command failure, got {other:?}"),
        }
    }
}
