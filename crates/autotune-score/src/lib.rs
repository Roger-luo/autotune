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
    /// Optional structured per-metric breakdown of how the rank was produced.
    /// `WeightedSumScorer` populates this (one entry per primary metric, with
    /// the weight and weighted contribution to the rank); other scorers leave
    /// it `None`. `#[serde(default)]` keeps script-scorer JSON output that
    /// omits the field deserializable, so the `ScoreCalculator` trait stays
    /// backward compatible across all scorers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<ScoreMetricContribution>>,
}

/// One scorer-provided primary-metric contribution to a weighted-sum rank.
///
/// This is the *scorer's* view: it knows the weight it applied and the
/// resulting weighted contribution. The richer per-metric breakdown that also
/// records baseline/candidate/best values and deltas is assembled by the CLI
/// (which holds those values) — see `autotune_state::MetricBreakdown`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreMetricContribution {
    /// Metric name.
    pub name: String,
    /// Per-metric improvement vs `best`, as a fraction (e.g. `0.09` = 9%).
    pub delta: f64,
    /// The weight this metric carried in the weighted sum.
    pub weight: f64,
    /// `weight * delta` — this metric's contribution to the overall rank.
    pub contribution: f64,
}

pub trait ScoreCalculator {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError>;
}
