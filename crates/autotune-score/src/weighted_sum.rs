use crate::{Metrics, ScoreCalculator, ScoreError, ScoreInput, ScoreOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Minimize,
    Maximize,
}

#[derive(Debug, Clone)]
pub struct PrimaryMetricDef {
    pub name: String,
    pub direction: Direction,
    pub weight: f64,
}

#[derive(Debug, Clone)]
pub struct GuardrailMetricDef {
    pub name: String,
    pub direction: Direction,
    pub max_regression: f64,
}

pub struct WeightedSumScorer {
    primary: Vec<PrimaryMetricDef>,
    guardrails: Vec<GuardrailMetricDef>,
}

impl WeightedSumScorer {
    pub fn new(primary: Vec<PrimaryMetricDef>, guardrails: Vec<GuardrailMetricDef>) -> Self {
        Self {
            primary,
            guardrails,
        }
    }
}

pub fn improvement(best: f64, candidate: f64, direction: Direction) -> f64 {
    if best == 0.0 {
        return 0.0;
    }

    match direction {
        Direction::Maximize => (candidate - best) / best.abs(),
        Direction::Minimize => (best - candidate) / best.abs(),
    }
}

pub fn check_guardrail(
    best: f64,
    candidate: f64,
    direction: Direction,
    max_regression: f64,
) -> Option<f64> {
    if best == 0.0 {
        return None;
    }

    let regression = match direction {
        Direction::Maximize => (best - candidate) / best.abs(),
        Direction::Minimize => (candidate - best) / best.abs(),
    };

    if regression > max_regression {
        Some(regression)
    } else {
        None
    }
}

pub fn get_metric(metrics: &Metrics, name: &str) -> Result<f64, ScoreError> {
    metrics
        .get(name)
        .copied()
        .ok_or_else(|| ScoreError::MissingMetric {
            name: name.to_string(),
        })
}

impl ScoreCalculator for WeightedSumScorer {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError> {
        for guardrail in &self.guardrails {
            let best_val = get_metric(&input.best, &guardrail.name)?;
            let cand_val = get_metric(&input.candidate, &guardrail.name)?;

            if let Some(regression) = check_guardrail(
                best_val,
                cand_val,
                guardrail.direction,
                guardrail.max_regression,
            ) {
                return Ok(ScoreOutput {
                    rank: -regression,
                    decision: "discard".to_string(),
                    reason: format!(
                        "guardrail '{}' failed: regression {:.2}% exceeds max {:.2}%",
                        guardrail.name,
                        regression * 100.0,
                        guardrail.max_regression * 100.0
                    ),
                });
            }
        }

        let mut rank = 0.0;
        let mut reasons = Vec::new();

        for primary in &self.primary {
            let best_val = get_metric(&input.best, &primary.name)?;
            let cand_val = get_metric(&input.candidate, &primary.name)?;
            let delta = improvement(best_val, cand_val, primary.direction);
            rank += primary.weight * delta;
            reasons.push(format!("{}: {:.2}%", primary.name, delta * 100.0));
        }

        Ok(ScoreOutput {
            rank,
            decision: if rank > 0.0 {
                "keep".to_string()
            } else {
                "discard".to_string()
            },
            reason: reasons.join(", "),
        })
    }
}
