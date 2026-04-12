use autotune_agent::AgentError;
use autotune_config::ConfigError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InitError {
    #[error("agent error: {source}")]
    Agent {
        #[from]
        source: AgentError,
    },

    #[error("config validation error: {source}")]
    Config {
        #[from]
        source: ConfigError,
    },

    #[error("user aborted init")]
    UserAborted,

    #[error("agent failed to produce valid request after retry: {message}")]
    ProtocolFailure { message: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
