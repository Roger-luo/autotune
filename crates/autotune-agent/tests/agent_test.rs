use autotune_agent::claude::ClaudeAgent;
use autotune_agent::codex::CodexAgent;
use autotune_agent::{
    Agent, AgentConfig, AgentConfigWithEvents, AgentError, AgentEvent, AgentSession, ToolPermission,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn claude_backend_name() {
    let agent = ClaudeAgent::new();
    assert_eq!(agent.backend_name(), "claude");
}

#[test]
fn claude_handover_command() {
    let agent = ClaudeAgent::new();
    let session = AgentSession {
        session_id: "abc-123".to_string(),
        backend: "claude".to_string(),
    };
    assert_eq!(agent.handover_command(&session), "claude -r abc-123");
}

#[test]
fn codex_backend_name() {
    let agent = CodexAgent::new();
    assert_eq!(agent.backend_name(), "codex");
}

#[test]
fn codex_handover_command() {
    let agent = CodexAgent::new();
    let session = AgentSession {
        session_id: "thread-123".to_string(),
        backend: "codex".to_string(),
    };
    assert_eq!(agent.handover_command(&session), "codex resume thread-123");
}

#[test]
fn agent_config_builds() {
    let config = AgentConfig {
        prompt: "test prompt".to_string(),
        allowed_tools: vec![
            ToolPermission::Allow("Read".to_string()),
            ToolPermission::Allow("Write".to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: PathBuf::from("/tmp"),
        model: Some("opus".to_string()),
        max_turns: None,
        reasoning_effort: None,
    };

    assert_eq!(config.prompt, "test prompt");
    assert_eq!(config.allowed_tools.len(), 3);
    assert_eq!(config.model.unwrap(), "opus");
}

#[test]
fn claude_send_preserves_spawn_context() {
    let harness = FakeClaudeHarness::new(r#"{"session_id":"sess-123","result":"spawned"}"#);
    let agent = ClaudeAgent::with_command(harness.claude_path());
    let working_directory = harness.root.join("workspace");
    fs::create_dir_all(&working_directory).unwrap();

    let config = AgentConfig {
        prompt: "initial prompt".to_string(),
        allowed_tools: vec![
            ToolPermission::Allow("Read".to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: working_directory.clone(),
        model: Some("opus".to_string()),
        max_turns: Some(7),
        reasoning_effort: None,
    };

    let response = agent.spawn(&config).unwrap();

    harness.write_response(r#"{"session_id":"sess-123","result":"follow-up"}"#);

    let session = AgentSession {
        session_id: response.session_id,
        backend: "claude".to_string(),
    };
    let send_response = agent.send(&session, "second prompt").unwrap();

    assert_eq!(send_response.text, "follow-up");

    let invocations = harness.read_invocations();
    assert_eq!(invocations.len(), 2);
    assert_eq!(
        fs::canonicalize(&invocations[1].pwd).unwrap(),
        fs::canonicalize(&working_directory).unwrap()
    );
    assert!(invocations[1].args.contains(&"-r".to_string()));
    assert!(invocations[1].args.contains(&"sess-123".to_string()));
    assert!(invocations[1].args.contains(&"--model".to_string()));
    assert!(invocations[1].args.contains(&"opus".to_string()));
    assert!(invocations[1].args.contains(&"--max-turns".to_string()));
    assert!(invocations[1].args.contains(&"7".to_string()));
    assert!(invocations[1].args.contains(&"--allowedTools".to_string()));
    assert!(invocations[1].args.contains(&"Read".to_string()));
    assert!(
        invocations[1]
            .args
            .contains(&"--disallowedTools".to_string())
    );
    assert!(invocations[1].args.contains(&"Bash".to_string()));
}

#[test]
fn claude_spawn_rejects_malformed_json() {
    let harness = FakeClaudeHarness::new("not json");
    let agent = ClaudeAgent::with_command(harness.claude_path());

    let error = agent.spawn(&basic_config(&harness.root)).unwrap_err();
    assert!(matches!(error, AgentError::ParseFailed { .. }));
}

#[test]
fn claude_spawn_rejects_missing_session_id() {
    let harness = FakeClaudeHarness::new(r#"{"result":"ok"}"#);
    let agent = ClaudeAgent::with_command(harness.claude_path());

    let error = agent.spawn(&basic_config(&harness.root)).unwrap_err();
    assert!(matches!(error, AgentError::ParseFailed { .. }));
}

#[test]
fn claude_spawn_rejects_missing_result() {
    let harness = FakeClaudeHarness::new(r#"{"session_id":"sess-123"}"#);
    let agent = ClaudeAgent::with_command(harness.claude_path());

    let error = agent.spawn(&basic_config(&harness.root)).unwrap_err();
    assert!(matches!(error, AgentError::ParseFailed { .. }));
}

#[test]
fn codex_send_preserves_spawn_context() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"agent_message_delta","delta":"spawned"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let codex_home = harness.root.join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let agent =
        CodexAgent::with_command_and_codex_home(harness.codex_path(), Some(codex_home.clone()));
    let working_directory = harness.root.join("workspace");
    fs::create_dir_all(&working_directory).unwrap();

    let config = AgentConfig {
        prompt: "initial prompt".to_string(),
        allowed_tools: vec![
            ToolPermission::Allow("Read".to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: working_directory.clone(),
        model: Some("gpt-5.4".to_string()),
        max_turns: Some(12),
        reasoning_effort: None,
    };

    let response = agent.spawn(&config).unwrap();
    harness.write_response(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"agent_message_delta","delta":"follow-up"}
{"event":"turn_complete","last_agent_message":"follow-up"}"#,
    );

    let session = AgentSession {
        session_id: response.session_id,
        backend: "codex".to_string(),
    };
    let send_response = agent.send(&session, "second prompt").unwrap();

    assert_eq!(send_response.text, "follow-up");

    let invocations = harness.read_invocations();
    assert_eq!(invocations.len(), 2);
    assert!(
        invocations[1].args.starts_with(&[
            "-a".to_string(),
            "untrusted".to_string(),
            "--sandbox".to_string(),
            "read-only".to_string(),
            "--add-dir".to_string(),
            codex_home.display().to_string(),
            "-C".to_string(),
            working_directory.display().to_string(),
            "exec".to_string(),
            "resume".to_string(),
            "--json".to_string(),
            "--skip-git-repo-check".to_string(),
        ]),
        "unexpected args: {:?}",
        invocations[1].args
    );
    assert!(invocations[1].args.contains(&"--model".to_string()));
    assert!(invocations[1].args.contains(&"gpt-5.4".to_string()));
    assert!(!invocations[1].args.contains(&"--color".to_string()));
    assert_eq!(
        invocations[1]
            .args
            .iter()
            .position(|arg| arg == "thread-123"),
        Some(invocations[1].args.len() - 2),
        "session id should be the final positional argument before the prompt: {:?}",
        invocations[1].args
    );
}

#[test]
fn codex_spawn_uses_reasoning_effort_flag() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let agent = CodexAgent::with_command(harness.codex_path());
    let working_directory = harness.root.join("workspace");
    fs::create_dir_all(&working_directory).unwrap();

    let config = AgentConfig {
        prompt: "initial prompt".to_string(),
        allowed_tools: vec![ToolPermission::Allow("Read".to_string())],
        working_directory,
        model: Some("gpt-5.4".to_string()),
        max_turns: None,
        reasoning_effort: Some("high".to_string()),
    };

    agent.spawn(&config).unwrap();

    let invocations = harness.read_invocations();
    assert!(invocations[0].args.contains(&"-c".to_string()));
    assert!(
        invocations[0]
            .args
            .contains(&"model_reasoning_effort=high".to_string()),
        "unexpected args: {:?}",
        invocations[0].args
    );
}

#[test]
fn codex_spawn_uses_top_level_approval_flags() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let codex_home = harness.root.join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let agent =
        CodexAgent::with_command_and_codex_home(harness.codex_path(), Some(codex_home.clone()));
    let working_directory = harness.root.join("workspace");
    let writable_dir = harness.root.join("scratch");
    fs::create_dir_all(&working_directory).unwrap();
    fs::create_dir_all(&writable_dir).unwrap();

    let config = AgentConfig {
        prompt: "initial prompt".to_string(),
        allowed_tools: vec![
            ToolPermission::Allow("Read".to_string()),
            ToolPermission::AllowScoped("Write".to_string(), writable_dir.display().to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: working_directory.clone(),
        model: None,
        max_turns: None,
        reasoning_effort: None,
    };

    agent.spawn(&config).unwrap();

    let invocations = harness.read_invocations();
    assert_eq!(invocations.len(), 1);
    assert!(
        invocations[0].args.starts_with(&[
            "-a".to_string(),
            "untrusted".to_string(),
            "--sandbox".to_string(),
            "workspace-write".to_string(),
            "--add-dir".to_string(),
            writable_dir.display().to_string(),
            "--add-dir".to_string(),
            codex_home.display().to_string(),
            "-C".to_string(),
            working_directory.display().to_string(),
            "exec".to_string(),
            "--json".to_string(),
            "--skip-git-repo-check".to_string(),
        ]),
        "unexpected args: {:?}",
        invocations[0].args
    );
    assert!(!invocations[0].args.contains(&"--color".to_string()));
    assert_eq!(
        invocations[0]
            .args
            .iter()
            .filter(|arg| arg.as_str() == "-a")
            .count(),
        1,
        "approval flag should only be passed at the top level: {:?}",
        invocations[0].args
    );
}

#[test]
fn codex_spawn_mounts_codex_home_for_read_only_runs() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let codex_home = harness.root.join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let agent =
        CodexAgent::with_command_and_codex_home(harness.codex_path(), Some(codex_home.clone()));

    agent.spawn(&basic_config(&harness.root)).unwrap();

    let invocations = harness.read_invocations();
    assert!(
        invocations[0].args.starts_with(&[
            "-a".to_string(),
            "never".to_string(),
            "--sandbox".to_string(),
            "read-only".to_string(),
            "--add-dir".to_string(),
            codex_home.display().to_string(),
            "-C".to_string(),
            harness.root.display().to_string(),
            "exec".to_string(),
            "--json".to_string(),
            "--skip-git-repo-check".to_string(),
        ]),
        "unexpected args: {:?}",
        invocations[0].args
    );
}

#[test]
fn codex_streaming_parses_jsonl_events() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"agent_message_delta","delta":"thinking"}
{"event":"exec_command_begin","call_id":"cmd-1","command":["rg","foo"]}
{"event":"turn_complete","last_agent_message":"done"}"#,
    );
    let agent = CodexAgent::with_command(harness.codex_path());
    let seen = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let seen_for_handler = Arc::clone(&seen);

    let response = agent
        .spawn_streaming(
            AgentConfigWithEvents::new(basic_config(&harness.root)).with_event_handler(Box::new(
                move |event| seen_for_handler.lock().unwrap().push(event),
            )),
        )
        .unwrap();

    assert_eq!(response.session_id, "thread-123");
    assert_eq!(response.text, "done");

    let events = seen.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Text(text) if text == "thinking")),
        "missing text event in {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolUse {
                tool,
                input_summary,
            } if tool == "exec_command" && input_summary.contains("rg")
        )),
        "missing tool-use event in {events:?}"
    );
}

#[test]
fn codex_parses_current_cli_event_shape() {
    let harness = FakeCodexHarness::new(
        r#"{"type":"thread.started","thread_id":"thread-123"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"OK"}}
{"type":"turn.completed"}"#,
    );
    let agent = CodexAgent::with_command(harness.codex_path());

    let response = agent.spawn(&basic_config(&harness.root)).unwrap();

    assert_eq!(response.session_id, "thread-123");
    assert_eq!(response.text, "OK");
}

#[test]
fn codex_streaming_reports_stderr_when_command_fails_before_json() {
    let harness = FakeCodexHarness::new_with_stderr_and_status(
        "",
        "Error loading config.toml: invalid type: unit variant, expected string only",
        1,
    );
    let agent = CodexAgent::with_command(harness.codex_path());

    let error = agent
        .spawn_streaming(
            AgentConfigWithEvents::new(basic_config(&harness.root))
                .with_event_handler(Box::new(|_event| {})),
        )
        .unwrap_err();

    match error {
        AgentError::CommandFailed { message } => {
            assert!(
                message.contains("Error loading config.toml"),
                "unexpected error: {message}"
            );
        }
        other => panic!("expected command failure, got {other:?}"),
    }
}

#[test]
fn codex_grant_session_permission_persists_to_send() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let agent = CodexAgent::with_command(harness.codex_path());
    let response = agent.spawn(&basic_config(&harness.root)).unwrap();
    let session = AgentSession {
        session_id: response.session_id,
        backend: "codex".to_string(),
    };

    agent
        .grant_session_permission(
            &session,
            ToolPermission::AllowScoped("Write".to_string(), "/tmp/worktree".to_string()),
        )
        .unwrap();
    agent
        .grant_session_permission(&session, ToolPermission::Allow("Read".to_string()))
        .unwrap();

    harness.write_response(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"follow-up"}"#,
    );
    let send_response = agent.send(&session, "continue").unwrap();
    assert_eq!(send_response.text, "follow-up");

    let invocations = harness.read_invocations();
    assert!(invocations[1].args.contains(&"--add-dir".to_string()));
    assert!(invocations[1].args.contains(&"/tmp/worktree".to_string()));
    assert!(invocations[1].args.contains(&"--sandbox".to_string()));
    assert!(invocations[1].args.contains(&"workspace-write".to_string()));
    assert!(
        invocations[1]
            .args
            .starts_with(&["-a".to_string(), "never".to_string()]),
        "unexpected args: {:?}",
        invocations[1].args
    );
}

#[test]
fn codex_hydrate_session_allows_resume_on_fresh_agent_instance() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let spawn_agent = CodexAgent::with_command(harness.codex_path());
    let working_directory = harness.root.join("workspace");
    fs::create_dir_all(&working_directory).unwrap();
    let config = AgentConfig {
        prompt: "initial prompt".to_string(),
        allowed_tools: vec![ToolPermission::Allow("Read".to_string())],
        working_directory: working_directory.clone(),
        model: Some("gpt-5.4".to_string()),
        max_turns: Some(12),
        reasoning_effort: None,
    };

    let response = spawn_agent.spawn(&config).unwrap();

    let fresh_agent = CodexAgent::with_command(harness.codex_path());
    let session = AgentSession {
        session_id: response.session_id,
        backend: "codex".to_string(),
    };
    fresh_agent.hydrate_session(&session, &config).unwrap();

    harness.write_response(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"follow-up"}"#,
    );
    let send_response = fresh_agent.send(&session, "continue").unwrap();

    assert_eq!(send_response.text, "follow-up");
}

#[test]
fn claude_streaming_preserves_text_delta_newlines() {
    let harness = FakeClaudeHarness::new(
        r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"\n<plan>\n"}}
{"type":"result","session_id":"sess-123","result":"<plan></plan>"}"#,
    );
    let agent = ClaudeAgent::with_command(harness.claude_path());
    let seen = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let seen_for_handler = Arc::clone(&seen);

    let response = agent
        .spawn_streaming(
            AgentConfigWithEvents::new(basic_config(&harness.root)).with_event_handler(Box::new(
                move |event| {
                    seen_for_handler.lock().unwrap().push(event);
                },
            )),
        )
        .unwrap();

    assert_eq!(response.session_id, "sess-123");
    let events = seen.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Text(text) if text == "\n<plan>\n")),
        "expected raw text delta with surrounding newlines, got {events:?}"
    );
}

fn basic_config(root: &Path) -> AgentConfig {
    AgentConfig {
        prompt: "prompt".to_string(),
        allowed_tools: vec![],
        working_directory: root.to_path_buf(),
        model: None,
        max_turns: None,
        reasoning_effort: None,
    }
}

#[derive(Debug)]
struct Invocation {
    pwd: PathBuf,
    args: Vec<String>,
}

struct FakeClaudeHarness {
    root: PathBuf,
    bin_dir: PathBuf,
    response_file: PathBuf,
    invocations_file: PathBuf,
}

struct FakeCodexHarness {
    root: PathBuf,
    bin_dir: PathBuf,
    response_file: PathBuf,
    invocations_file: PathBuf,
}

impl FakeCodexHarness {
    fn new(initial_response: &str) -> Self {
        Self::new_with_stderr_and_status(initial_response, "", 0)
    }

    fn new_with_stderr_and_status(initial_response: &str, stderr: &str, status: i32) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "autotune-codex-agent-test-{}-{}",
            std::process::id(),
            unique
        ));
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let response_file = root.join("response.jsonl");
        let stderr_file = root.join("stderr.txt");
        let status_file = root.join("status.txt");
        let invocations_file = root.join("invocations.log");
        fs::write(&response_file, initial_response).unwrap();
        fs::write(&stderr_file, stderr).unwrap();
        fs::write(&status_file, status.to_string()).unwrap();

        let script_path = bin_dir.join("codex");
        let script = format!(
            "#!/bin/sh\nprintf 'PWD=%s\\n' \"$PWD\" >> '{log}'\nfor arg in \"$@\"; do\n  printf 'ARG=%s\\n' \"$arg\" >> '{log}'\ndone\nprintf 'END\\n' >> '{log}'\ncat '{response}'\ncat '{stderr}' >&2\nexit \"$(cat '{status}')\"\n",
            log = invocations_file.display(),
            response = response_file.display(),
            stderr = stderr_file.display(),
            status = status_file.display(),
        );
        fs::write(&script_path, script).unwrap();

        let mut permissions = fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).unwrap();

        Self {
            root,
            bin_dir,
            response_file,
            invocations_file,
        }
    }

    fn codex_path(&self) -> PathBuf {
        self.bin_dir.join("codex")
    }

    fn write_response(&self, response: &str) {
        fs::write(&self.response_file, response).unwrap();
    }

    fn read_invocations(&self) -> Vec<Invocation> {
        let log = fs::read_to_string(&self.invocations_file).unwrap();
        let mut invocations = Vec::new();
        let mut pwd = None;
        let mut args = Vec::new();

        for line in log.lines() {
            if let Some(value) = line.strip_prefix("PWD=") {
                pwd = Some(PathBuf::from(value));
            } else if let Some(value) = line.strip_prefix("ARG=") {
                args.push(value.to_string());
            } else if line == "END" {
                invocations.push(Invocation {
                    pwd: pwd.take().unwrap(),
                    args: std::mem::take(&mut args),
                });
            }
        }

        invocations
    }
}

impl FakeClaudeHarness {
    fn new(initial_response: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "autotune-agent-test-{}-{}",
            std::process::id(),
            unique
        ));
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let response_file = root.join("response.json");
        let invocations_file = root.join("invocations.log");
        fs::write(&response_file, initial_response).unwrap();

        let script_path = bin_dir.join("claude");
        let script = format!(
            "#!/bin/sh\nprintf 'PWD=%s\\n' \"$PWD\" >> '{log}'\nfor arg in \"$@\"; do\n  printf 'ARG=%s\\n' \"$arg\" >> '{log}'\ndone\nprintf 'END\\n' >> '{log}'\ncat '{response}'\n",
            log = invocations_file.display(),
            response = response_file.display()
        );
        fs::write(&script_path, script).unwrap();

        let mut permissions = fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).unwrap();

        Self {
            root,
            bin_dir,
            response_file,
            invocations_file,
        }
    }

    fn claude_path(&self) -> PathBuf {
        self.bin_dir.join("claude")
    }

    fn write_response(&self, response: &str) {
        fs::write(&self.response_file, response).unwrap();
    }

    fn read_invocations(&self) -> Vec<Invocation> {
        let log = fs::read_to_string(&self.invocations_file).unwrap();
        let mut invocations = Vec::new();
        let mut pwd = None;
        let mut args = Vec::new();

        for line in log.lines() {
            if let Some(value) = line.strip_prefix("PWD=") {
                pwd = Some(PathBuf::from(value));
            } else if let Some(value) = line.strip_prefix("ARG=") {
                args.push(value.to_string());
            } else if line == "END" {
                invocations.push(Invocation {
                    pwd: pwd.take().unwrap(),
                    args: std::mem::take(&mut args),
                });
            }
        }

        invocations
    }
}
