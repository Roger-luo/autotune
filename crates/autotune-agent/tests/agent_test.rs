use autotune_agent::claude::ClaudeAgent;
use autotune_agent::{Agent, AgentConfig, AgentSession, ToolPermission};
use std::path::PathBuf;

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
            ToolPermission::AllowScoped("Edit".to_string(), "src/**".to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: PathBuf::from("/tmp"),
        model: Some("opus".to_string()),
        max_turns: Some(50),
    };

    assert_eq!(config.prompt, "test prompt");
    assert_eq!(config.allowed_tools.len(), 3);
    assert_eq!(config.model.unwrap(), "opus");
}
