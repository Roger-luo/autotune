pub mod claude;
pub mod protocol;
pub mod terminal;
pub mod trace;

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent command failed: {message}")]
    CommandFailed { message: String },

    #[error("failed to parse agent response: {message}")]
    ParseFailed { message: String },

    #[error("agent timed out after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("agent interrupted by signal")]
    Interrupted,

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub enum ToolPermission {
    Allow(String),
    AllowScoped(String, String),
    Deny(String),
}

/// Events emitted by the agent during execution.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Agent is using a tool (e.g., Read, Glob, Grep).
    ToolUse { tool: String, input_summary: String },
    /// Agent produced intermediate text output.
    Text(String),
}

/// Callback for receiving agent events during execution.
/// The agent calls this for each streaming event. Implementations
/// should be cheap (e.g., print to stderr) since they block the read loop.
pub type EventHandler = Box<dyn Fn(AgentEvent) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub prompt: String,
    pub allowed_tools: Vec<ToolPermission>,
    pub working_directory: PathBuf,
    pub model: Option<String>,
    pub max_turns: Option<u64>,
}

/// Extended config with optional event streaming. Used internally
/// by agent implementations — callers set the handler via `with_event_handler`.
pub struct AgentConfigWithEvents {
    pub config: AgentConfig,
    pub event_handler: Option<EventHandler>,
}

impl AgentConfigWithEvents {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            event_handler: None,
        }
    }

    pub fn with_event_handler(mut self, handler: EventHandler) -> Self {
        self.event_handler = Some(handler);
        self
    }
}

#[derive(Debug, Clone)]
pub struct AgentSession {
    pub session_id: String,
    pub backend: String,
}

#[derive(Debug, Clone)]
pub struct AgentResponse {
    pub text: String,
    pub session_id: String,
}

pub trait Agent {
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError>;

    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError>;

    /// Spawn with event streaming. Default falls back to non-streaming spawn.
    fn spawn_streaming(
        &self,
        config_with_events: AgentConfigWithEvents,
    ) -> Result<AgentResponse, AgentError> {
        self.spawn(&config_with_events.config)
    }

    /// Send with event streaming. Default falls back to non-streaming send.
    fn send_streaming(
        &self,
        session: &AgentSession,
        message: &str,
        event_handler: Option<&EventHandler>,
    ) -> Result<AgentResponse, AgentError> {
        let _ = event_handler;
        self.send(session, message)
    }

    fn backend_name(&self) -> &str;

    fn handover_command(&self, session: &AgentSession) -> String;

    /// Add a tool permission to an existing session so subsequent `send_streaming`
    /// calls will include it in the allowed tools list. Used to approve runtime
    /// tool requests from an agent. Default impl errors — backends that keep
    /// per-session context should override.
    fn grant_session_permission(
        &self,
        session: &AgentSession,
        permission: ToolPermission,
    ) -> Result<(), AgentError> {
        let _ = (session, permission);
        Err(AgentError::CommandFailed {
            message: "this agent backend does not support runtime permission grants".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dummy_config() -> AgentConfig {
        AgentConfig {
            prompt: "test".to_string(),
            allowed_tools: vec![],
            working_directory: PathBuf::from("."),
            model: None,
            max_turns: None,
        }
    }

    #[test]
    fn agent_config_with_events_new_has_no_handler() {
        let config = AgentConfigWithEvents::new(dummy_config());
        assert!(config.event_handler.is_none());
    }

    #[test]
    fn agent_config_with_events_with_handler_sets_handler() {
        let config =
            AgentConfigWithEvents::new(dummy_config()).with_event_handler(Box::new(|_event| {}));
        assert!(config.event_handler.is_some());
    }

    #[test]
    fn default_grant_session_permission_returns_error() {
        struct MinimalAgent;

        impl Agent for MinimalAgent {
            fn spawn(&self, _config: &AgentConfig) -> Result<AgentResponse, AgentError> {
                unimplemented!()
            }

            fn send(
                &self,
                _session: &AgentSession,
                _message: &str,
            ) -> Result<AgentResponse, AgentError> {
                unimplemented!()
            }

            fn backend_name(&self) -> &str {
                "minimal"
            }

            fn handover_command(&self, _session: &AgentSession) -> String {
                String::new()
            }
        }

        let agent = MinimalAgent;
        let session = AgentSession {
            session_id: "s1".to_string(),
            backend: "minimal".to_string(),
        };
        let err = agent
            .grant_session_permission(&session, ToolPermission::Allow("Read".to_string()))
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does not support runtime permission grants"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn default_spawn_streaming_delegates_to_spawn() {
        struct ReturnAgent;

        impl Agent for ReturnAgent {
            fn spawn(&self, _config: &AgentConfig) -> Result<AgentResponse, AgentError> {
                Ok(AgentResponse {
                    text: "spawned".to_string(),
                    session_id: "sess-1".to_string(),
                })
            }

            fn send(
                &self,
                _session: &AgentSession,
                _message: &str,
            ) -> Result<AgentResponse, AgentError> {
                unimplemented!()
            }

            fn backend_name(&self) -> &str {
                "return"
            }

            fn handover_command(&self, _session: &AgentSession) -> String {
                String::new()
            }
        }

        let agent = ReturnAgent;
        let config = AgentConfigWithEvents::new(dummy_config());
        let response = agent.spawn_streaming(config).unwrap();
        assert_eq!(response.text, "spawned");
        assert_eq!(response.session_id, "sess-1");
    }

    #[test]
    fn default_send_streaming_delegates_to_send() {
        struct ReturnAgent;

        impl Agent for ReturnAgent {
            fn spawn(&self, _config: &AgentConfig) -> Result<AgentResponse, AgentError> {
                unimplemented!()
            }

            fn send(
                &self,
                _session: &AgentSession,
                _message: &str,
            ) -> Result<AgentResponse, AgentError> {
                Ok(AgentResponse {
                    text: "sent".to_string(),
                    session_id: "sess-2".to_string(),
                })
            }

            fn backend_name(&self) -> &str {
                "return"
            }

            fn handover_command(&self, _session: &AgentSession) -> String {
                String::new()
            }
        }

        let agent = ReturnAgent;
        let session = AgentSession {
            session_id: "sess-2".to_string(),
            backend: "return".to_string(),
        };
        let response = agent.send_streaming(&session, "hello", None).unwrap();
        assert_eq!(response.text, "sent");
        assert_eq!(response.session_id, "sess-2");
    }
}
