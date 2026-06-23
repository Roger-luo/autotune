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

/// A per-metric noise estimate the scorer uses to size a significance
/// envelope. Mirrors `autotune_adaptor::MetricVariance` (mapped at the
/// `main.rs` boundary, like the `Direction` enums) so this crate stays a leaf.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricVariance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_lower: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_upper: Option<f64>,
}

impl MetricVariance {
    /// Half-width of the confidence interval, `(upper - lower) / 2`, if known.
    pub fn ci_half_width(&self) -> Option<f64> {
        match (self.ci_lower, self.ci_upper) {
            (Some(lower), Some(upper)) => Some((upper - lower).abs() / 2.0),
            _ => None,
        }
    }
}

/// Per-metric noise estimates keyed by metric name.
pub type Variances = HashMap<String, MetricVariance>;

/// Per-metric empirical cross-build noise floor (absolute units), keyed by
/// metric name. Computed once at baseline time by re-measuring the baseline
/// `N` times with a rebuild between each replicate (option 1: cross-build
/// codegen/layout noise), then folded into the noise envelope as a `MAX` floor.
/// Empty by default → no cross-build floor, behavior unchanged.
pub type EmpiricalEnvelopes = HashMap<String, f64>;

/// Compute the empirical cross-build noise floor for one metric from a set of
/// replicate measurements (the baseline measured `1 + N` times, rebuilding
/// between each so the spread captures build-to-build codegen/layout noise).
///
/// Returns the **half-range** `(max - min) / 2` across the replicates. Half the
/// peak-to-peak swing is the honest radius a single new measurement could land
/// inside purely from rebuild jitter, and it composes with the existing
/// CI-half-width model (which also returns a radius), so the two are directly
/// comparable when we take their `MAX`.
///
/// Fewer than two replicates ⇒ `0.0` (no spread observed, no floor). This is
/// the backward-compatible identity when `baseline_replicates == 0` (only the
/// single baseline measurement exists).
pub fn empirical_cross_build_envelope(replicate_values: &[f64]) -> f64 {
    if replicate_values.len() < 2 {
        return 0.0;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in replicate_values {
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    (max - min) / 2.0
}

/// Tunable parameters for the noise-aware significance gate. The default is the
/// no-op identity: with no variances and `relative_threshold == 0.0` the
/// envelope is `0.0`, so only an exactly-zero delta is "within noise" — and a
/// zero delta already contributes nothing to the rank. This preserves the
/// pre-noise scorer behavior bit-for-bit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NoiseConfig {
    /// Relative noise floor as a fraction of `|best|` (e.g. `0.05` = 5%). Used
    /// when no per-metric variance is available. `0.0` disables it.
    #[serde(default)]
    pub relative_threshold: f64,
    /// Multiplier `k` applied to a metric's stddev when no confidence interval
    /// is available (envelope = `k * stddev`). Criterion supplies stddev even
    /// when a CI is missing.
    #[serde(default = "default_stddev_k")]
    pub stddev_k: f64,
}

fn default_stddev_k() -> f64 {
    DEFAULT_STDDEV_K
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            relative_threshold: 0.0,
            stddev_k: DEFAULT_STDDEV_K,
        }
    }
}

/// Default stddev multiplier when sizing an envelope from stddev alone. Two
/// standard deviations is a conventional ~95% band, matching criterion's CI
/// confidence level, so CI-based and stddev-based envelopes are comparable.
pub const DEFAULT_STDDEV_K: f64 = 2.0;

/// Compute the noise envelope (the absolute magnitude a delta must EXCEED to
/// count as a real change) for one metric, from the candidate and best
/// variances plus the noise config. The WITHIN-RUN component is picked in
/// preference order:
///
/// 1. Confidence intervals: `candidate_half_width + best_half_width` (the task's
///    primary model — two independent measurements, errors add).
/// 2. Stddev: `stddev_k * max(candidate_stddev, best_stddev)`.
/// 3. Relative floor: `relative_threshold * |best_value|`.
///
/// The final envelope is `MAX(within_run, empirical_cross_build)`. Criterion's
/// per-run CI captures only *within-build* sampling jitter; it cannot see the
/// codegen/layout noise that swings a bench ±35% between rebuilds (option 1).
/// The empirical cross-build floor — measured by replicating the baseline with
/// a rebuild between each — is the more honest, usually larger envelope, so we
/// never let the (tighter) within-run CI shrink it. `empirical == 0.0` (the
/// default when `baseline_replicates == 0`) leaves the within-run value
/// untouched, the backward-compatible identity.
///
/// Returns `0.0` when nothing applies (the backward-compatible identity).
pub fn noise_envelope(
    best_value: f64,
    candidate_variance: Option<&MetricVariance>,
    best_variance: Option<&MetricVariance>,
    empirical: f64,
    config: &NoiseConfig,
) -> f64 {
    let within_run = within_run_envelope(best_value, candidate_variance, best_variance, config);
    within_run.max(empirical)
}

/// The within-run (single-build) component of the noise envelope — CI, then
/// stddev, then relative floor. Split out so the cross-build empirical floor can
/// be `MAX`ed in by [`noise_envelope`].
fn within_run_envelope(
    best_value: f64,
    candidate_variance: Option<&MetricVariance>,
    best_variance: Option<&MetricVariance>,
    config: &NoiseConfig,
) -> f64 {
    let cand_ci = candidate_variance.and_then(|v| v.ci_half_width());
    let best_ci = best_variance.and_then(|v| v.ci_half_width());
    if cand_ci.is_some() || best_ci.is_some() {
        return cand_ci.unwrap_or(0.0) + best_ci.unwrap_or(0.0);
    }

    let cand_sd = candidate_variance.and_then(|v| v.stddev);
    let best_sd = best_variance.and_then(|v| v.stddev);
    if cand_sd.is_some() || best_sd.is_some() {
        let sd = cand_sd.unwrap_or(0.0).max(best_sd.unwrap_or(0.0));
        return config.stddev_k * sd;
    }

    config.relative_threshold * best_value.abs()
}

/// True when a raw delta's magnitude does NOT exceed the noise envelope — i.e.
/// the change is statistically indistinguishable from measurement noise and
/// must not count as an improvement or a regression.
pub fn within_noise(
    delta: f64,
    best_value: f64,
    candidate_variance: Option<&MetricVariance>,
    best_variance: Option<&MetricVariance>,
    empirical: f64,
    config: &NoiseConfig,
) -> bool {
    let envelope = noise_envelope(
        best_value,
        candidate_variance,
        best_variance,
        empirical,
        config,
    );
    delta.abs() <= envelope
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreInput {
    pub baseline: Metrics,
    pub candidate: Metrics,
    pub best: Metrics,
    /// Per-metric noise estimates for the candidate measurement. Empty when no
    /// adaptor supplied variance (regex/script) — then scoring is unchanged.
    #[serde(default)]
    pub candidate_variances: Variances,
    /// Per-metric noise estimates for the `best` row (recovered from the
    /// ledger). Empty when the best row predates variance capture.
    #[serde(default)]
    pub best_variances: Variances,
    /// Noise gate tuning. Defaults to the identity (no discounting).
    #[serde(default)]
    pub noise: NoiseConfig,
    /// Metric names to exclude from the keep/discard accounting entirely,
    /// treated exactly like a within-noise delta (zero contribution, no
    /// guardrail trip). Populated by the CLI for causally-unrelated metrics
    /// (Part C: the candidate's diff never touched the code the metric
    /// exercises). Empty by default → no causal filtering, behavior unchanged.
    #[serde(default)]
    pub excluded_metrics: std::collections::HashSet<String>,
    /// Per-metric empirical CROSS-BUILD noise floor (absolute units), computed
    /// once at baseline time by replicating the baseline with a rebuild between
    /// each measurement (option 1). Folded into the per-metric noise envelope as
    /// a `MAX` floor (see [`noise_envelope`]). Empty by default
    /// (`baseline_replicates == 0`) → no cross-build floor, behavior unchanged.
    #[serde(default)]
    pub empirical_envelope: EmpiricalEnvelopes,
}

impl ScoreInput {
    /// Convenience constructor for the common variance-free case (used widely
    /// in tests): no candidate/best variances, default noise config.
    pub fn new(baseline: Metrics, candidate: Metrics, best: Metrics) -> Self {
        Self {
            baseline,
            candidate,
            best,
            candidate_variances: Variances::new(),
            best_variances: Variances::new(),
            noise: NoiseConfig::default(),
            excluded_metrics: std::collections::HashSet::new(),
            empirical_envelope: EmpiricalEnvelopes::new(),
        }
    }
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
    /// Forced to `0.0` when the metric's raw delta was within the noise
    /// envelope (`within_noise == true`), so noise neither helps nor hurts.
    pub contribution: f64,
    /// True when this metric's raw delta did NOT exceed the noise envelope and
    /// was therefore excluded from the rank. `#[serde(default)]` keeps script
    /// scorer JSON that omits it deserializable.
    #[serde(default)]
    pub within_noise: bool,
}

pub trait ScoreCalculator {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── empirical_cross_build_envelope ─────────────────────────────────────

    /// Fewer than two replicates means no observed spread, so the empirical
    /// floor is `0.0` — the backward-compatible identity when
    /// `baseline_replicates == 0` (only the single baseline measurement exists).
    #[test]
    fn empirical_envelope_zero_for_under_two_replicates() {
        assert_eq!(empirical_cross_build_envelope(&[]), 0.0);
        assert_eq!(empirical_cross_build_envelope(&[100.0]), 0.0);
    }

    /// The empirical floor is the half-range `(max - min) / 2` across replicates
    /// — half the peak-to-peak swing, a radius directly comparable to a CI
    /// half-width.
    #[test]
    fn empirical_envelope_is_half_range_across_replicates() {
        // baseline rebuilt three extra times: 100, 135, 80, 120.
        // max 135, min 80 → range 55 → half-range 27.5.
        let env = empirical_cross_build_envelope(&[100.0, 135.0, 80.0, 120.0]);
        assert!((env - 27.5).abs() < 1e-12, "env {env}");
    }

    // ── noise_envelope: MAX(within-run CI, empirical cross-build) ──────────

    fn ci(lower: f64, upper: f64) -> MetricVariance {
        MetricVariance {
            stddev: None,
            ci_lower: Some(lower),
            ci_upper: Some(upper),
        }
    }

    /// The cross-build empirical floor is the more honest, larger envelope when
    /// it exceeds the tight within-run CI — `noise_envelope` returns the MAX so
    /// the within-run CI can never shrink the cross-build floor.
    #[test]
    fn noise_envelope_takes_max_of_ci_and_empirical() {
        // within-run CI half-width 5 each → 10. Empirical cross-build floor 35.
        let var = ci(95.0, 105.0);
        let env = noise_envelope(100.0, Some(&var), Some(&var), 35.0, &NoiseConfig::default());
        assert!(
            (env - 35.0).abs() < 1e-12,
            "expected empirical 35, got {env}"
        );
    }

    /// When the within-run CI is the larger of the two, it wins the MAX — the
    /// empirical floor never *shrinks* a genuinely wide within-run envelope.
    #[test]
    fn noise_envelope_keeps_wider_within_run_ci() {
        // within-run CI half-width 40 each → 80. Empirical floor only 10.
        let var = ci(60.0, 140.0);
        let env = noise_envelope(100.0, Some(&var), Some(&var), 10.0, &NoiseConfig::default());
        assert!(
            (env - 80.0).abs() < 1e-12,
            "expected within-run 80, got {env}"
        );
    }

    /// `empirical == 0.0` (the default, `baseline_replicates == 0`) leaves the
    /// within-run envelope exactly as it was — the backward-compatible identity.
    #[test]
    fn noise_envelope_zero_empirical_is_identity() {
        let var = ci(95.0, 105.0);
        let with = noise_envelope(100.0, Some(&var), Some(&var), 0.0, &NoiseConfig::default());
        let within_run =
            within_run_envelope(100.0, Some(&var), Some(&var), &NoiseConfig::default());
        assert_eq!(with, within_run);
        assert!((with - 10.0).abs() < 1e-12, "env {with}");
    }

    /// A delta within the empirical cross-build envelope is discounted even
    /// though it exceeds the within-run CI — the core option-1 behavior.
    #[test]
    fn within_noise_discounts_delta_inside_empirical_but_outside_ci() {
        // Tight within-run CI (half-width 5 each → 10). A +25 delta exceeds the
        // CI, but the empirical cross-build floor of 35 swallows it.
        let var = ci(95.0, 105.0);
        let cfg = NoiseConfig::default();
        // Without the empirical floor: 25 > 10 → NOT within noise (significant).
        assert!(!within_noise(
            25.0,
            100.0,
            Some(&var),
            Some(&var),
            0.0,
            &cfg
        ));
        // With the empirical floor 35: 25 <= 35 → within noise (discounted).
        assert!(within_noise(
            25.0,
            100.0,
            Some(&var),
            Some(&var),
            35.0,
            &cfg
        ));
    }
}
