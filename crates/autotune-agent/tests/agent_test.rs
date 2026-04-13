use autotune_agent::claude::ClaudeAgent;
use autotune_agent::{Agent, AgentConfig, AgentError, AgentSession, ToolPermission};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

fn basic_config(root: &Path) -> AgentConfig {
    AgentConfig {
        prompt: "prompt".to_string(),
        allowed_tools: vec![],
        working_directory: root.to_path_buf(),
        model: None,
        max_turns: None,
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
