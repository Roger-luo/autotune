use crate::{ScoreCalculator, ScoreError, ScoreInput, ScoreOutput, within_noise};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Minimize,
    Maximize,
}

#[derive(Debug, Clone)]
pub struct ThresholdConditionDef {
    pub metric: String,
    pub direction: Direction,
    pub threshold: f64,
}

pub struct ThresholdScorer {
    conditions: Vec<ThresholdConditionDef>,
}

impl ThresholdScorer {
    pub fn new(conditions: Vec<ThresholdConditionDef>) -> Self {
        Self { conditions }
    }
}

impl ScoreCalculator for ThresholdScorer {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError> {
        let mut all_pass = true;
        let mut total_improvement = 0.0;
        let mut reasons = Vec::new();

        for condition in &self.conditions {
            let best = input.best.get(&condition.metric).copied().ok_or_else(|| {
                ScoreError::MissingMetric {
                    name: condition.metric.clone(),
                }
            })?;
            let candidate = input
                .candidate
                .get(&condition.metric)
                .copied()
                .ok_or_else(|| ScoreError::MissingMetric {
                    name: condition.metric.clone(),
                })?;

            // A metric the candidate's diff can't causally affect, or whose
            // raw delta is within the noise envelope, is neither a pass nor a
            // failure — it doesn't count toward the all-pass decision. (The
            // envelope is symmetric, so direction doesn't matter here.)
            if input.excluded_metrics.contains(&condition.metric)
                || within_noise(
                    candidate - best,
                    best,
                    input.candidate_variances.get(&condition.metric),
                    input.best_variances.get(&condition.metric),
                    input
                        .empirical_envelope
                        .get(&condition.metric)
                        .copied()
                        .unwrap_or(0.0),
                    &input.noise,
                )
            {
                reasons.push(format!("{}: within noise (skipped)", condition.metric));
                continue;
            }

            let delta = match condition.direction {
                Direction::Maximize => candidate - best,
                Direction::Minimize => best - candidate,
            };

            if delta >= condition.threshold {
                total_improvement += delta;
                reasons.push(format!("{}: passed (+{:.4})", condition.metric, delta));
            } else {
                all_pass = false;
                reasons.push(format!(
                    "{}: failed ({:.4} < {:.4})",
                    condition.metric, delta, condition.threshold
                ));
            }
        }

        Ok(ScoreOutput {
            rank: total_improvement,
            decision: if all_pass {
                "keep".to_string()
            } else {
                "discard".to_string()
            },
            reason: reasons.join(", "),
            // The threshold scorer has no per-metric weight model; the CLI
            // still records baseline/candidate/best deltas without it.
            details: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScoreCalculator, ScoreError, ScoreInput};
    use std::collections::HashMap;

    fn make_input(best: &[(&str, f64)], candidate: &[(&str, f64)]) -> ScoreInput {
        let to_map = |pairs: &[(&str, f64)]| -> HashMap<String, f64> {
            pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
        };
        ScoreInput::new(to_map(best), to_map(candidate), to_map(best))
    }

    #[test]
    fn threshold_missing_metric_errors() {
        let scorer = ThresholdScorer::new(vec![ThresholdConditionDef {
            metric: "missing_metric".to_string(),
            direction: Direction::Minimize,
            threshold: 0.0,
        }]);
        let input = make_input(&[("other_metric", 1.0)], &[("other_metric", 0.9)]);
        let err = scorer.calculate(&input).unwrap_err();
        assert!(matches!(err, ScoreError::MissingMetric { ref name } if name == "missing_metric"));
    }

    #[test]
    fn threshold_maximize_direction() {
        let scorer = ThresholdScorer::new(vec![ThresholdConditionDef {
            metric: "throughput".to_string(),
            direction: Direction::Maximize,
            threshold: 0.0,
        }]);
        // candidate > best → delta = candidate - best > 0 >= threshold
        let input = make_input(&[("throughput", 100.0)], &[("throughput", 110.0)]);
        let result = scorer.calculate(&input).unwrap();
        assert_eq!(result.decision, "keep");
    }

    /// A within-noise condition is skipped (neither pass nor fail), so a sole
    /// noisy "failure" doesn't discard. Pins the threshold noise gate.
    #[test]
    fn threshold_within_noise_condition_is_skipped() {
        use crate::MetricVariance;
        let scorer = ThresholdScorer::new(vec![ThresholdConditionDef {
            metric: "lat".to_string(),
            direction: Direction::Minimize,
            threshold: 1.0,
        }]);
        // best 100 → 105 would naively fail (delta -5 < 1.0). But CI half-width
        // 40 each → envelope 80; |5| <= 80 → skipped, so all_pass stays true.
        let mut input = make_input(&[("lat", 100.0)], &[("lat", 105.0)]);
        let var = MetricVariance {
            stddev: Some(20.0),
            ci_lower: Some(60.0),
            ci_upper: Some(140.0),
        };
        input.candidate_variances = [("lat".to_string(), var)].into_iter().collect();
        input.best_variances = [("lat".to_string(), var)].into_iter().collect();
        let result = scorer.calculate(&input).unwrap();
        assert_eq!(result.decision, "keep");
        assert!(result.reason.contains("within noise"));
    }
}
