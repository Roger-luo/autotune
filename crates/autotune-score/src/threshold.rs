use crate::{ScoreCalculator, ScoreError, ScoreInput, ScoreOutput};

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
        ScoreInput {
            baseline: to_map(best),
            candidate: to_map(candidate),
            best: to_map(best),
        }
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
}
