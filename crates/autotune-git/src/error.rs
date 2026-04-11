use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git command failed: {command}\nstderr: {stderr}")]
    CommandFailed { command: String, stderr: String },

    #[error("not a git repository: {path}")]
    NotARepo { path: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
