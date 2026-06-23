pub mod criterion;
pub mod regex;
pub mod script;

use serde::{Deserialize, Serialize};
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

/// A per-metric noise estimate, populated by adaptors that can measure
/// dispersion (e.g. `CriterionAdaptor` reading criterion's `estimates.json`).
/// Regex/script adaptors leave this empty.
///
/// All fields are `Option` and `#[serde(default)]` so a partially-populated
/// estimate (e.g. stddev but no CI, or vice versa) round-trips, and so older
/// ledgers/state files that predate this feature still deserialize.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricVariance {
    /// Standard deviation of the metric's sample (the dispersion of repeated
    /// measurements), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev: Option<f64>,
    /// Lower bound of the metric's confidence interval, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_lower: Option<f64>,
    /// Upper bound of the metric's confidence interval, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_upper: Option<f64>,
}

impl MetricVariance {
    /// Half-width of the confidence interval, `(upper - lower) / 2`, if both
    /// bounds are present. This is the symmetric noise radius around the point
    /// estimate used to build the noise envelope.
    pub fn ci_half_width(&self) -> Option<f64> {
        match (self.ci_lower, self.ci_upper) {
            (Some(lower), Some(upper)) => Some((upper - lower).abs() / 2.0),
            _ => None,
        }
    }

    /// True when no dispersion information is carried at all.
    pub fn is_empty(&self) -> bool {
        self.stddev.is_none() && self.ci_lower.is_none() && self.ci_upper.is_none()
    }
}

/// Per-metric noise estimates keyed by metric name. Mirrors [`Metrics`].
pub type Variances = HashMap<String, MetricVariance>;

/// The adaptor trait. Takes measure output, produces metrics.
pub trait MetricAdaptor {
    fn extract(&self, output: &MeasureOutput) -> Result<Metrics, AdaptorError>;

    /// Optionally extract per-metric noise estimates (stddev / confidence
    /// interval) alongside the point values. Adaptors that have no dispersion
    /// information (regex, script) keep the default empty map; the criterion
    /// adaptor overrides this to read the CI/stddev criterion records.
    fn extract_variances(&self, _output: &MeasureOutput) -> Result<Variances, AdaptorError> {
        Ok(Variances::new())
    }
}
