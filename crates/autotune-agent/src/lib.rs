pub mod claude;
pub mod protocol;

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

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub prompt: String,
    pub allowed_tools: Vec<ToolPermission>,
    pub working_directory: PathBuf,
    pub model: Option<String>,
    pub max_turns: Option<u64>,
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

    fn backend_name(&self) -> &str;

    fn handover_command(&self, session: &AgentSession) -> String;
}
