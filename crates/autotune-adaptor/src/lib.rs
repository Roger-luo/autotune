pub mod criterion;
pub mod regex;
pub mod script;

use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdaptorError {
    #[error("regex pattern '{pattern}' failed to compile: {source}")]
    RegexCompile {
        pattern: String,
        source: ::regex::Error,
    },

    #[error("regex pattern '{pattern}' did not match any output for metric '{name}'")]
    RegexNoMatch { name: String, pattern: String },

    #[error("failed to parse extracted value '{value}' as f64 for metric '{name}'")]
    ParseFloat { name: String, value: String },

    #[error("criterion estimates.json not found at: {path}")]
    CriterionNotFound { path: String },

    #[error("criterion JSON parse error: {source}")]
    CriterionParse { source: serde_json::Error },

    #[error("script failed with exit code {code}: {stderr}")]
    ScriptFailed { code: i32, stderr: String },

    #[error("script command is empty")]
    ScriptEmptyCommand,

    #[error("script output is not valid JSON: {source}")]
    ScriptOutputParse { source: serde_json::Error },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

/// Output from a measure command - the raw text an adaptor processes.
#[derive(Debug, Clone)]
pub struct MeasureOutput {
    pub stdout: String,
    pub stderr: String,
}

/// All adaptors produce this: a map of metric name -> numeric value.
pub type Metrics = HashMap<String, f64>;

/// The adaptor trait. Takes measure output, produces metrics.
pub trait MetricAdaptor {
    fn extract(&self, output: &MeasureOutput) -> Result<Metrics, AdaptorError>;
}
