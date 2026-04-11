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
