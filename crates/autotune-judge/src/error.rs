use thiserror::Error;

#[derive(Debug, Error)]
pub enum JudgeError {
    #[error("invalid rubric: {message}")]
    InvalidRubric { message: String },

    #[error("invalid assessment: {message}")]
    InvalidAssessment { message: String },

    #[error("prompt render failed: {message}")]
    PromptRender { message: String },

    #[error("backend call failed: {message}")]
    BackendCall { message: String },

    #[error("backend response parse failed: {message}")]
    BackendParse { message: String },

    // One Io variant: thiserror cannot generate two From<std::io::Error> impls.
    #[error("io error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("json error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },
}
