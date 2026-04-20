use anyhow::{Result, bail};
use autotune_agent::{Agent, claude::ClaudeAgent, codex::CodexAgent};
use autotune_config::{AgentConfig, AgentRoleConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Research,
    Implementation,
    Init,
}

pub fn resolve_backend_name(config: &AgentConfig, role: AgentRole) -> Option<&str> {
    let role_config: Option<&AgentRoleConfig> = match role {
        AgentRole::Research => config.research.as_ref(),
        AgentRole::Implementation => config.implementation.as_ref(),
        AgentRole::Init => config.init.as_ref(),
    };

    role_config
        .and_then(|role_config| role_config.backend.as_deref())
        .or(config.backend.as_deref())
}

pub fn build_agent_for_backend(backend: &str) -> Result<Box<dyn Agent>> {
    match backend {
        "claude" => Ok(Box::new(ClaudeAgent::new())),
        "codex" => Ok(Box::new(CodexAgent::new())),
        other => bail!("unsupported agent backend '{other}' (supported: claude, codex)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> AgentConfig {
        AgentConfig {
            backend: Some("claude".to_string()),
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: None,
            max_fresh_spawns: None,
            research: None,
            implementation: None,
            init: None,
            judge: None,
        }
    }

    #[test]
    fn role_backend_overrides_global_backend() {
        let mut config = base_config();
        config.research = Some(AgentRoleConfig {
            backend: Some("codex".to_string()),
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: None,
            max_fresh_spawns: None,
        });

        assert_eq!(
            resolve_backend_name(&config, AgentRole::Research),
            Some("codex")
        );
        assert_eq!(
            resolve_backend_name(&config, AgentRole::Implementation),
            Some("claude")
        );
        assert_eq!(
            resolve_backend_name(&config, AgentRole::Init),
            Some("claude")
        );
    }

    #[test]
    fn resolve_backend_name_returns_none_when_backend_omitted() {
        let config = AgentConfig {
            backend: None,
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: None,
            max_fresh_spawns: None,
            research: None,
            implementation: None,
            init: None,
            judge: None,
        };

        assert_eq!(resolve_backend_name(&config, AgentRole::Research), None);
    }

    #[test]
    fn build_agent_for_claude_backend() {
        let agent = build_agent_for_backend("claude").unwrap();
        assert_eq!(agent.backend_name(), "claude");
    }

    #[test]
    fn build_agent_for_backend_supports_claude_and_codex() {
        assert_eq!(
            build_agent_for_backend("claude").unwrap().backend_name(),
            "claude"
        );
        assert_eq!(
            build_agent_for_backend("codex").unwrap().backend_name(),
            "codex"
        );
    }

    #[test]
    fn build_agent_for_backend_rejects_unknown_backend() {
        let err = match build_agent_for_backend("unknown") {
            Ok(agent) => panic!("unexpected backend: {}", agent.backend_name()),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("supported: claude, codex"),
            "unexpected error: {err}"
        );
    }
}
