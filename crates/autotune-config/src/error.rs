use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: String },

    #[error("failed to parse config: {source}")]
    Parse {
        #[from]
        source: toml::de::Error,
    },

    #[error("validation error: {message}")]
    Validation { message: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
