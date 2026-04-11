pub mod script;
pub mod threshold;
pub mod weighted_sum;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScoreError {
    #[error("missing metric '{name}' in candidate")]
    MissingMetric { name: String },

    #[error(
        "guardrail failed for '{name}': regression {regression:.4} exceeds max {max_regression:.4}"
    )]
    GuardrailFailed {
        name: String,
        regression: f64,
        max_regression: f64,
    },

    #[error("script failed with exit code {code}: {stderr}")]
    ScriptFailed { code: i32, stderr: String },

    #[error("script output parse error: {source}")]
    ScriptOutputParse { source: serde_json::Error },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

pub type Metrics = HashMap<String, f64>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreInput {
    pub baseline: Metrics,
    pub candidate: Metrics,
    pub best: Metrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreOutput {
    pub rank: f64,
    pub decision: String,
    pub reason: String,
}

pub trait ScoreCalculator {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError>;
}
