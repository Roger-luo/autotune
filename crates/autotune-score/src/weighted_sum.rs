use crate::{
    Metrics, ScoreCalculator, ScoreError, ScoreInput, ScoreMetricContribution, ScoreOutput,
};

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
        match direction {
            Direction::Maximize => candidate - best,
            Direction::Minimize => best - candidate,
        }
    } else {
        match direction {
            Direction::Maximize => (candidate - best) / best.abs(),
            Direction::Minimize => (best - candidate) / best.abs(),
        }
    }
}

pub fn check_guardrail(
    best: f64,
    candidate: f64,
    direction: Direction,
    max_regression: f64,
) -> Option<f64> {
    let regression = if best == 0.0 {
        match direction {
            Direction::Maximize => best - candidate,
            Direction::Minimize => candidate - best,
        }
    } else {
        match direction {
            Direction::Maximize => (best - candidate) / best.abs(),
            Direction::Minimize => (candidate - best) / best.abs(),
        }
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
                    // A guardrail failure short-circuits before any primary
                    // metric contributes, so there is no weighted breakdown.
                    details: Some(Vec::new()),
                });
            }
        }

        let mut rank = 0.0;
        let mut reasons = Vec::new();
        let mut details = Vec::with_capacity(self.primary.len());

        for primary in &self.primary {
            let best_val = get_metric(&input.best, &primary.name)?;
            let cand_val = get_metric(&input.candidate, &primary.name)?;
            let delta = improvement(best_val, cand_val, primary.direction);
            let contribution = primary.weight * delta;
            rank += contribution;
            reasons.push(format!("{}: {:.2}%", primary.name, delta * 100.0));
            details.push(ScoreMetricContribution {
                name: primary.name.clone(),
                delta,
                weight: primary.weight,
                contribution,
            });
        }

        Ok(ScoreOutput {
            rank,
            decision: if rank > 0.0 {
                "keep".to_string()
            } else {
                "discard".to_string()
            },
            reason: reasons.join(", "),
            details: Some(details),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScoreCalculator;

    fn input(best: &[(&str, f64)], candidate: &[(&str, f64)]) -> ScoreInput {
        let to_map = |pairs: &[(&str, f64)]| -> Metrics {
            pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
        };
        ScoreInput {
            baseline: to_map(best),
            candidate: to_map(candidate),
            best: to_map(best),
        }
    }

    /// The structured breakdown must record each primary metric's delta,
    /// weight, and weighted contribution — and the contributions must sum to
    /// the overall rank. This is the data the analysis artifact exposes.
    #[test]
    fn details_carry_weight_and_contribution_summing_to_rank() {
        let scorer = WeightedSumScorer::new(
            vec![
                PrimaryMetricDef {
                    name: "throughput".to_string(),
                    direction: Direction::Maximize,
                    weight: 0.75,
                },
                PrimaryMetricDef {
                    name: "latency".to_string(),
                    direction: Direction::Minimize,
                    weight: 0.25,
                },
            ],
            vec![],
        );
        // throughput: best 100 → 110 (Maximize) = +0.10 delta.
        // latency: best 50 → 40 (Minimize) = +0.20 delta.
        let out = scorer
            .calculate(&input(
                &[("throughput", 100.0), ("latency", 50.0)],
                &[("throughput", 110.0), ("latency", 40.0)],
            ))
            .unwrap();

        let details = out.details.expect("weighted-sum must populate details");
        assert_eq!(details.len(), 2);

        let tp = details.iter().find(|d| d.name == "throughput").unwrap();
        assert!((tp.delta - 0.10).abs() < 1e-9, "delta {}", tp.delta);
        assert!((tp.weight - 0.75).abs() < 1e-9);
        assert!((tp.contribution - 0.075).abs() < 1e-9);

        let lat = details.iter().find(|d| d.name == "latency").unwrap();
        assert!((lat.delta - 0.20).abs() < 1e-9, "delta {}", lat.delta);
        assert!((lat.contribution - 0.05).abs() < 1e-9);

        let sum: f64 = details.iter().map(|d| d.contribution).sum();
        assert!(
            (sum - out.rank).abs() < 1e-9,
            "contributions {sum} must sum to rank {}",
            out.rank
        );
    }

    /// A guardrail breach short-circuits before any primary metric is scored,
    /// so the breakdown is present but empty (distinguishing "no contributions"
    /// from "scorer doesn't support breakdowns" = `None`).
    #[test]
    fn guardrail_breach_yields_empty_details() {
        let scorer = WeightedSumScorer::new(
            vec![PrimaryMetricDef {
                name: "throughput".to_string(),
                direction: Direction::Maximize,
                weight: 1.0,
            }],
            vec![GuardrailMetricDef {
                name: "errors".to_string(),
                direction: Direction::Minimize,
                max_regression: 0.05,
            }],
        );
        // errors best 1.0 → 2.0 (Minimize): regression 100% > 5% max.
        let out = scorer
            .calculate(&input(
                &[("throughput", 100.0), ("errors", 1.0)],
                &[("throughput", 110.0), ("errors", 2.0)],
            ))
            .unwrap();
        assert_eq!(out.decision, "discard");
        assert_eq!(out.details.unwrap(), Vec::new());
    }

    /// `ScoreOutput` without `details` (e.g. a script scorer's JSON) must still
    /// deserialize — the field is `#[serde(default)]`.
    #[test]
    fn score_output_without_details_deserializes() {
        let json = r#"{"rank": 0.5, "decision": "keep", "reason": "ok"}"#;
        let out: ScoreOutput = serde_json::from_str(json).unwrap();
        assert!(out.details.is_none());
    }
}
