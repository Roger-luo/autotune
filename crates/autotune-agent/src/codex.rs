use crate::{
    Agent, AgentConfig, AgentConfigWithEvents, AgentError, AgentEvent, AgentResponse, AgentSession,
    EventHandler, ToolPermission,
};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub struct CodexAgent {
    command: PathBuf,
    codex_home: Option<PathBuf>,
    sessions: Mutex<HashMap<String, SessionContext>>,
}

#[derive(Debug, Clone)]
struct SessionContext {
    working_directory: PathBuf,
    model: Option<String>,
    max_turns: Option<u64>,
    reasoning_effort: Option<String>,
    allowed_tools: Vec<ToolPermission>,
}

impl CodexAgent {
    pub fn new() -> Self {
        Self::with_command(PathBuf::from("codex"))
    }

    pub fn with_command(command: PathBuf) -> Self {
        Self::with_command_and_codex_home(command, Self::default_codex_home())
    }

    pub fn with_command_and_codex_home(command: PathBuf, codex_home: Option<PathBuf>) -> Self {
        Self {
            command,
            codex_home,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn build_args(&self, config: &AgentConfig, session_id: Option<&str>) -> Vec<String> {
        let mut args = Self::permission_args(&config.allowed_tools, self.codex_home.as_deref());
        args.extend([
            "-C".to_string(),
            config.working_directory.display().to_string(),
            "exec".to_string(),
        ]);
        if session_id.is_some() {
            args.push("resume".to_string());
        }
        args.extend(["--json".to_string(), "--skip-git-repo-check".to_string()]);
        if let Some(model) = &config.model {
            args.extend(["--model".to_string(), model.clone()]);
        }
        if let Some(reasoning_effort) = &config.reasoning_effort {
            args.extend([
                "-c".to_string(),
                format!("model_reasoning_effort={reasoning_effort}"),
            ]);
        }
        if let Some(session_id) = session_id {
            args.push(session_id.to_string());
        }
        args.push(Self::normalize_prompt(&config.prompt).to_string());
        args
    }

    fn default_codex_home() -> Option<PathBuf> {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
    }

    fn normalize_prompt(prompt: &str) -> &str {
        if prompt.trim().is_empty() {
            "Continue."
        } else {
            prompt
        }
    }

    fn permission_args(perms: &[ToolPermission], codex_home: Option<&Path>) -> Vec<String> {
        let mut writable_dirs = BTreeSet::new();
        let mut has_write = false;
        let mut deny_bash = false;
        let mut allow_search = false;

        for perm in perms {
            match perm {
                ToolPermission::Allow(tool) if tool == "Write" || tool == "Edit" => {
                    has_write = true;
                }
                ToolPermission::Allow(tool) if tool == "WebFetch" || tool == "WebSearch" => {
                    allow_search = true;
                }
                ToolPermission::AllowScoped(tool, path) if tool == "Write" || tool == "Edit" => {
                    has_write = true;
                    if !Self::looks_like_glob(path) {
                        writable_dirs.insert(path.clone());
                    }
                }
                ToolPermission::AllowScoped(tool, _)
                    if tool == "WebFetch" || tool == "WebSearch" =>
                {
                    allow_search = true;
                }
                ToolPermission::Deny(tool) if tool == "Bash" => {
                    deny_bash = true;
                }
                _ => {}
            }
        }

        // Codex does not expose a shell-disable flag for `exec`. The closest
        // available behavior when Bash is denied is `-a untrusted`, which
        // blocks most command execution behind approval instead of silently
        // allowing it. Trusted read-only commands may still run.
        let mut args = vec![
            "-a".to_string(),
            if deny_bash {
                "untrusted".to_string()
            } else {
                "never".to_string()
            },
            "--sandbox".to_string(),
            if has_write {
                "workspace-write".to_string()
            } else {
                "read-only".to_string()
            },
        ];

        for dir in writable_dirs {
            args.extend(["--add-dir".to_string(), dir]);
        }
        if let Some(dir) = codex_home {
            args.extend(["--add-dir".to_string(), dir.display().to_string()]);
        }
        if allow_search {
            args.push("--search".to_string());
        }

        args
    }

    fn looks_like_glob(path: &str) -> bool {
        path.contains('*') || path.contains('?') || path.contains('[') || path.contains('{')
    }

    fn remember_session(&self, session_id: &str, config: &AgentConfig) -> Result<(), AgentError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AgentError::CommandFailed {
                message: "codex session state unavailable".to_string(),
            })?;
        sessions.insert(
            session_id.to_string(),
            SessionContext {
                working_directory: config.working_directory.clone(),
                model: config.model.clone(),
                max_turns: config.max_turns,
                reasoning_effort: config.reasoning_effort.clone(),
                allowed_tools: config.allowed_tools.clone(),
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
                message: "codex session state unavailable".to_string(),
            })?;
        let context = sessions
            .get(session_id)
            .ok_or_else(|| AgentError::CommandFailed {
                message: format!("missing codex session context for '{session_id}'"),
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

    fn parse_jsonl<R: BufRead>(
        reader: R,
        handler: Option<&EventHandler>,
    ) -> Result<AgentResponse, AgentError> {
        let mut thread_id: Option<String> = None;
        let mut last_message = String::new();

        for line in reader.lines() {
            let line = line.map_err(|source| AgentError::Io { source })?;
            if line.trim().is_empty() {
                continue;
            }

            let value: Value =
                serde_json::from_str(&line).map_err(|source| AgentError::ParseFailed {
                    message: format!("invalid codex JSON output: {source}"),
                })?;

            let event = value
                .get("event")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");

            if thread_id.is_none() {
                thread_id = Self::extract_thread_id(&value);
            }

            match event {
                "thread.started" | "thread/started" | "thread_started" => {}
                "agent_message_delta" => {
                    if let Some(text) = Self::delta_text(&value)
                        && !text.is_empty()
                    {
                        if let Some(handler) = handler {
                            handler(AgentEvent::Text(text.clone()));
                        }
                        last_message.push_str(&text);
                    }
                }
                "exec_command_begin" => {
                    if let Some(handler) = handler {
                        handler(AgentEvent::ToolUse {
                            tool: "exec_command".to_string(),
                            input_summary: Self::command_summary(&value),
                        });
                    }
                }
                "item.completed" | "item_completed" | "item/completed" => {
                    if let Some(text) = value.get("item").and_then(|item| {
                        match item.get("type").and_then(Value::as_str) {
                            Some("agent_message") => item.get("text").and_then(Value::as_str),
                            _ => None,
                        }
                    }) {
                        last_message = text.to_string();
                    }
                }
                "turn_complete" | "task_complete" | "turn.completed" | "task.completed"
                | "turn/completed" | "task/completed" => {
                    if let Some(text) = value
                        .get("last_agent_message")
                        .or_else(|| value.get("lastAgentMessage"))
                        .and_then(Value::as_str)
                    {
                        last_message = text.to_string();
                    }
                }
                _ => {}
            }
        }

        let session_id = thread_id.ok_or_else(|| AgentError::ParseFailed {
            message: "codex JSON missing thread/session id".to_string(),
        })?;

        Ok(AgentResponse {
            text: last_message,
            session_id,
        })
    }

    fn extract_thread_id(value: &Value) -> Option<String> {
        value
            .get("thread_id")
            .or_else(|| value.get("threadId"))
            .or_else(|| value.get("thread").and_then(|thread| thread.get("id")))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    fn delta_text(value: &Value) -> Option<String> {
        if let Some(delta) = value.get("delta") {
            if let Some(text) = delta.as_str() {
                return Some(text.to_string());
            }
            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }
        }
        value
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    fn command_summary(value: &Value) -> String {
        match value.get("command") {
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            Some(Value::String(command)) => command.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    }

    fn run_codex(&self, args: &[String], cwd: &Path) -> Result<AgentResponse, AgentError> {
        let _guard = crate::terminal::Guard::new();
        let output = Command::new(&self.command)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .map_err(|source| AgentError::Io { source })?;

        if !output.status.success() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if output.status.signal() == Some(2) {
                    return Err(AgentError::Interrupted);
                }
            }
            return Err(AgentError::CommandFailed {
                message: format!(
                    "codex exited with {}\nargs: {:?}{}",
                    output.status,
                    args,
                    Self::output_details(&output.stdout, &output.stderr)
                ),
            });
        }

        Self::parse_jsonl(BufReader::new(Cursor::new(output.stdout)), None)
    }

    fn run_codex_streaming(
        &self,
        args: &[String],
        cwd: &Path,
        event_handler: &EventHandler,
    ) -> Result<AgentResponse, AgentError> {
        let _guard = crate::terminal::Guard::new();
        let mut child = Command::new(&self.command)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| AgentError::Io { source })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::CommandFailed {
                message: "failed to capture codex stdout".to_string(),
            })?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| AgentError::CommandFailed {
                message: "failed to capture codex stderr".to_string(),
            })?;

        let response = Self::parse_jsonl(BufReader::new(stdout), Some(event_handler));
        let status = child.wait().map_err(|source| AgentError::Io { source })?;

        if !status.success() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if status.signal() == Some(2) {
                    return Err(AgentError::Interrupted);
                }
            }
            let mut stderr_text = String::new();
            let _ = stderr.read_to_string(&mut stderr_text);
            let details = if stderr_text.trim().is_empty() {
                String::new()
            } else {
                format!("\nstderr: {}", stderr_text.trim())
            };
            return Err(AgentError::CommandFailed {
                message: format!("codex exited with {}\nargs: {:?}{}", status, args, details),
            });
        }

        response
    }

    fn output_details(stdout: &[u8], stderr: &[u8]) -> String {
        let stdout = String::from_utf8_lossy(stdout);
        let stderr = String::from_utf8_lossy(stderr);
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

        details
    }
}

impl Default for CodexAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for CodexAgent {
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        let args = self.build_args(config, None);
        let response = self.run_codex(&args, &config.working_directory)?;
        self.remember_session(&response.session_id, config)?;
        Ok(response)
    }

    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError> {
        let config = self.config_for_session(&session.session_id, message)?;
        let args = self.build_args(&config, Some(&session.session_id));
        let response = self.run_codex(&args, &config.working_directory)?;
        self.remember_session(&response.session_id, &config)?;
        Ok(response)
    }

    fn spawn_streaming(
        &self,
        config_with_events: AgentConfigWithEvents,
    ) -> Result<AgentResponse, AgentError> {
        let config = &config_with_events.config;
        let args = self.build_args(config, None);
        let response = if let Some(ref handler) = config_with_events.event_handler {
            self.run_codex_streaming(&args, &config.working_directory, handler)?
        } else {
            self.run_codex(&args, &config.working_directory)?
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
        let args = self.build_args(&config, Some(&session.session_id));
        let response = if let Some(handler) = event_handler {
            self.run_codex_streaming(&args, &config.working_directory, handler)?
        } else {
            self.run_codex(&args, &config.working_directory)?
        };
        self.remember_session(&response.session_id, &config)?;
        Ok(response)
    }

    fn backend_name(&self) -> &str {
        "codex"
    }

    fn handover_command(&self, session: &AgentSession) -> String {
        format!("codex resume {}", session.session_id)
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
                message: "codex session state unavailable".to_string(),
            })?;
        let context =
            sessions
                .get_mut(&session.session_id)
                .ok_or_else(|| AgentError::CommandFailed {
                    message: format!(
                        "cannot grant permission — no session context for '{}'",
                        session.session_id
                    ),
                })?;
        context.allowed_tools.push(permission);
        Ok(())
    }
}
