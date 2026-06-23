use crate::{
    Metrics, ScoreCalculator, ScoreError, ScoreInput, ScoreMetricContribution, ScoreOutput,
    within_noise,
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

            // A guardrail "regression" within the noise envelope — or on a
            // metric the candidate's diff can't causally affect — is not a real
            // regression; don't trip the guardrail on it.
            if input.excluded_metrics.contains(&guardrail.name)
                || within_noise(
                    cand_val - best_val,
                    best_val,
                    input.candidate_variances.get(&guardrail.name),
                    input.best_variances.get(&guardrail.name),
                    &input.noise,
                )
            {
                continue;
            }

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

            // Noise gate: a delta whose RAW magnitude doesn't exceed the noise
            // envelope is indistinguishable from re-run jitter. It must count as
            // neither improvement nor regression — zero its contribution and
            // drop it from the reason string.
            let raw_delta = cand_val - best_val;
            let noisy = input.excluded_metrics.contains(&primary.name)
                || within_noise(
                    raw_delta,
                    best_val,
                    input.candidate_variances.get(&primary.name),
                    input.best_variances.get(&primary.name),
                    &input.noise,
                );

            let contribution = if noisy { 0.0 } else { primary.weight * delta };
            rank += contribution;
            if noisy {
                reasons.push(format!(
                    "{}: {:.2}% (within noise)",
                    primary.name,
                    delta * 100.0
                ));
            } else {
                reasons.push(format!("{}: {:.2}%", primary.name, delta * 100.0));
            }
            details.push(ScoreMetricContribution {
                name: primary.name.clone(),
                delta,
                weight: primary.weight,
                contribution,
                within_noise: noisy,
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
        ScoreInput::new(to_map(best), to_map(candidate), to_map(best))
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

    use crate::{DEFAULT_STDDEV_K, MetricVariance, NoiseConfig, Variances};

    fn variances(pairs: &[(&str, MetricVariance)]) -> Variances {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// THE backward-compatibility pin: with no variances and the default
    /// (zero-threshold) noise config, the rank, decision, and contributions are
    /// bit-for-bit identical to the pre-noise scorer. This is what guarantees
    /// existing scorer behavior never silently changes.
    #[test]
    fn no_variance_zero_threshold_reproduces_legacy_rank_exactly() {
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
        let out = scorer
            .calculate(&input(
                &[("throughput", 100.0), ("latency", 50.0)],
                &[("throughput", 110.0), ("latency", 40.0)],
            ))
            .unwrap();

        // Identical to `details_carry_weight_and_contribution_summing_to_rank`.
        assert!((out.rank - 0.125).abs() < 1e-12, "rank {}", out.rank);
        assert_eq!(out.decision, "keep");
        let details = out.details.unwrap();
        for d in &details {
            assert!(!d.within_noise, "{} flagged noisy under identity", d.name);
        }
        let tp = details.iter().find(|d| d.name == "throughput").unwrap();
        assert!((tp.contribution - 0.075).abs() < 1e-12);
    }

    /// A regression whose magnitude falls inside the CI noise envelope is
    /// excluded: it contributes 0 to the rank and is flagged `within_noise`.
    /// This is the Clifford episode in miniature — a "+31% regression" that is
    /// pure re-run jitter must not drag the rank negative.
    #[test]
    fn within_noise_regression_is_excluded_from_rank() {
        let scorer = WeightedSumScorer::new(
            vec![PrimaryMetricDef {
                name: "sparse_vec_ns".to_string(),
                direction: Direction::Minimize,
                weight: 1.0,
            }],
            vec![],
        );
        // best 100, candidate 131 → naive Minimize delta = -0.31 (a 31%
        // "regression"). But both measurements have CI half-width 35, so the
        // envelope is 70 and |131-100|=31 <= 70 → within noise.
        let mut score_input = input(&[("sparse_vec_ns", 100.0)], &[("sparse_vec_ns", 131.0)]);
        let var = MetricVariance {
            stddev: Some(20.0),
            ci_lower: Some(65.0),
            ci_upper: Some(135.0), // half-width 35
        };
        score_input.candidate_variances = variances(&[("sparse_vec_ns", var)]);
        score_input.best_variances = variances(&[("sparse_vec_ns", var)]);

        let out = scorer.calculate(&score_input).unwrap();
        assert_eq!(out.rank, 0.0, "within-noise delta must not move the rank");
        let d = &out.details.unwrap()[0];
        assert!(d.within_noise);
        assert_eq!(d.contribution, 0.0);
        assert!(
            out.reason.contains("within noise"),
            "reason: {}",
            out.reason
        );
    }

    /// A delta that EXCEEDS the envelope is still scored normally even when
    /// variance is present — the gate only discounts sub-noise deltas.
    #[test]
    fn significant_delta_with_variance_still_scores() {
        let scorer = WeightedSumScorer::new(
            vec![PrimaryMetricDef {
                name: "lat".to_string(),
                direction: Direction::Minimize,
                weight: 1.0,
            }],
            vec![],
        );
        // best 100, candidate 50 → |delta|=50 > envelope (2*5+2*5 via... here
        // CI half-width 5 each → envelope 10). 50 > 10, so it scores.
        let mut score_input = input(&[("lat", 100.0)], &[("lat", 50.0)]);
        let var = MetricVariance {
            stddev: Some(2.0),
            ci_lower: Some(95.0),
            ci_upper: Some(105.0), // half-width 5
        };
        score_input.candidate_variances = variances(&[("lat", var)]);
        score_input.best_variances = variances(&[("lat", var)]);

        let out = scorer.calculate(&score_input).unwrap();
        assert!((out.rank - 0.5).abs() < 1e-12, "rank {}", out.rank);
        assert!(!out.details.unwrap()[0].within_noise);
    }

    /// A within-noise guardrail "regression" does not trip the guardrail.
    #[test]
    fn within_noise_guardrail_regression_does_not_discard() {
        let scorer = WeightedSumScorer::new(
            vec![PrimaryMetricDef {
                name: "tp".to_string(),
                direction: Direction::Maximize,
                weight: 1.0,
            }],
            vec![GuardrailMetricDef {
                name: "errors".to_string(),
                direction: Direction::Minimize,
                max_regression: 0.05,
            }],
        );
        // errors best 100 → 130: naive 30% regression > 5% max. But CI
        // half-width 35 each → envelope 70, |30| <= 70 → noise, skip guardrail.
        let mut score_input = input(
            &[("tp", 100.0), ("errors", 100.0)],
            &[("tp", 110.0), ("errors", 130.0)],
        );
        let var = MetricVariance {
            stddev: Some(20.0),
            ci_lower: Some(65.0),
            ci_upper: Some(135.0),
        };
        score_input.candidate_variances = variances(&[("errors", var)]);
        score_input.best_variances = variances(&[("errors", var)]);

        let out = scorer.calculate(&score_input).unwrap();
        assert_eq!(out.decision, "keep", "noise guardrail should not discard");
    }

    /// Relative `noise_threshold` (no variance present) discounts a small delta.
    #[test]
    fn relative_threshold_discounts_small_delta() {
        let scorer = WeightedSumScorer::new(
            vec![PrimaryMetricDef {
                name: "m".to_string(),
                direction: Direction::Maximize,
                weight: 1.0,
            }],
            vec![],
        );
        // best 100 → 102: +2% gain. With a 5% relative noise floor, envelope is
        // 5.0 and |2| <= 5 → within noise, no rank.
        let mut score_input = input(&[("m", 100.0)], &[("m", 102.0)]);
        score_input.noise = NoiseConfig {
            relative_threshold: 0.05,
            stddev_k: DEFAULT_STDDEV_K,
        };
        let out = scorer.calculate(&score_input).unwrap();
        assert_eq!(out.rank, 0.0);
        assert!(out.details.unwrap()[0].within_noise);
    }
}
