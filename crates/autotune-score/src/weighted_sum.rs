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

/// A NOISE-TOLERANT guardrail (Part C / option 4): a metric declared purely as
/// a constraint, NOT an optimization target. Unlike [`GuardrailMetricDef`] it
/// has no explicit `max_regression` — its veto threshold IS the metric's noise
/// envelope. It (i) never contributes to the weighted objective/rank, (ii)
/// vetoes (forces discard) only when it regresses by MORE than its noise
/// envelope, (iii) ignores within-noise moves. With none declared, behavior is
/// identical to today.
#[derive(Debug, Clone)]
pub struct NoiseGuardrailDef {
    pub name: String,
    pub direction: Direction,
}

pub struct WeightedSumScorer {
    primary: Vec<PrimaryMetricDef>,
    guardrails: Vec<GuardrailMetricDef>,
    /// Noise-tolerant constraint metrics (Part C). Veto-only; threshold = the
    /// noise envelope. Empty ⇒ behavior identical to the legacy scorer.
    noise_guardrails: Vec<NoiseGuardrailDef>,
}

impl WeightedSumScorer {
    /// Construct with explicit-threshold guardrails only (no noise-tolerant
    /// constraints). Equivalent to `with_noise_guardrails(primary, guardrails,
    /// vec![])`; kept for call sites and tests that predate Part C.
    pub fn new(primary: Vec<PrimaryMetricDef>, guardrails: Vec<GuardrailMetricDef>) -> Self {
        Self::with_noise_guardrails(primary, guardrails, Vec::new())
    }

    /// Construct with explicit-threshold guardrails AND noise-tolerant
    /// constraint metrics (Part C / option 4).
    pub fn with_noise_guardrails(
        primary: Vec<PrimaryMetricDef>,
        guardrails: Vec<GuardrailMetricDef>,
        noise_guardrails: Vec<NoiseGuardrailDef>,
    ) -> Self {
        Self {
            primary,
            guardrails,
            noise_guardrails,
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

        // Noise-tolerant constraint metrics (Part C / option 4): veto-only,
        // threshold = the metric's noise envelope. They never contribute to the
        // rank. A within-noise (or causally-unrelated) move is ignored; any
        // regression whose magnitude EXCEEDS the envelope forces discard.
        for guardrail in &self.noise_guardrails {
            let best_val = get_metric(&input.best, &guardrail.name)?;
            let cand_val = get_metric(&input.candidate, &guardrail.name)?;
            let raw_delta = cand_val - best_val;

            if input.excluded_metrics.contains(&guardrail.name)
                || within_noise(
                    raw_delta,
                    best_val,
                    input.candidate_variances.get(&guardrail.name),
                    input.best_variances.get(&guardrail.name),
                    &input.noise,
                )
            {
                continue;
            }

            // The delta exceeds the envelope: it's a real, significant move.
            // Veto only if it's a regression in the constraint's direction; a
            // significant *improvement* on a guardrail is harmless.
            let regression = match guardrail.direction {
                Direction::Maximize => best_val - cand_val,
                Direction::Minimize => cand_val - best_val,
            };
            if regression > 0.0 {
                return Ok(ScoreOutput {
                    rank: 0.0,
                    decision: "discard".to_string(),
                    reason: format!(
                        "guardrail '{}' failed: significant regression beyond the noise envelope",
                        guardrail.name
                    ),
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

    // ---- Part C: noise-tolerant constraint guardrails (option 4) ----

    use crate::weighted_sum::NoiseGuardrailDef;

    fn objective(name: &str, dir: Direction) -> Vec<PrimaryMetricDef> {
        vec![PrimaryMetricDef {
            name: name.to_string(),
            direction: dir,
            weight: 1.0,
        }]
    }

    /// A noise-tolerant guardrail VETOes (forces discard) when its regression
    /// exceeds the noise envelope — even though the weighted objective improved.
    #[test]
    fn noise_guardrail_vetoes_significant_regression() {
        let scorer = WeightedSumScorer::with_noise_guardrails(
            objective("tp", Direction::Maximize),
            vec![],
            vec![NoiseGuardrailDef {
                name: "mem".to_string(),
                direction: Direction::Minimize,
            }],
        );
        // tp improves 100→110 (real gain). mem 100→200 (Minimize regression of
        // 100). CI half-width 5 each → envelope 10, |100| > 10 → significant
        // regression → veto.
        let mut score_input = input(
            &[("tp", 100.0), ("mem", 100.0)],
            &[("tp", 110.0), ("mem", 200.0)],
        );
        let tight = MetricVariance {
            stddev: Some(2.0),
            ci_lower: Some(95.0),
            ci_upper: Some(105.0),
        };
        score_input.candidate_variances = variances(&[("mem", tight)]);
        score_input.best_variances = variances(&[("mem", tight)]);

        let out = scorer.calculate(&score_input).unwrap();
        assert_eq!(out.decision, "discard", "out: {out:?}");
        assert!(out.reason.contains("mem"), "reason: {}", out.reason);
        assert!(
            out.reason.contains("noise envelope"),
            "reason: {}",
            out.reason
        );
    }

    /// A within-noise guardrail move does NOT veto: the candidate is kept on the
    /// strength of the (real) objective improvement.
    #[test]
    fn noise_guardrail_within_noise_move_does_not_veto() {
        let scorer = WeightedSumScorer::with_noise_guardrails(
            objective("tp", Direction::Maximize),
            vec![],
            vec![NoiseGuardrailDef {
                name: "mem".to_string(),
                direction: Direction::Minimize,
            }],
        );
        // mem 100→130 looks like a 30% regression, but CI half-width 35 each →
        // envelope 70, |30| <= 70 → within noise → ignored.
        let mut score_input = input(
            &[("tp", 100.0), ("mem", 100.0)],
            &[("tp", 110.0), ("mem", 130.0)],
        );
        let wide = MetricVariance {
            stddev: Some(20.0),
            ci_lower: Some(65.0),
            ci_upper: Some(135.0),
        };
        score_input.candidate_variances = variances(&[("mem", wide)]);
        score_input.best_variances = variances(&[("mem", wide)]);

        let out = scorer.calculate(&score_input).unwrap();
        assert_eq!(out.decision, "keep", "within-noise guardrail must not veto");
        assert!((out.rank - 0.10).abs() < 1e-9, "rank {}", out.rank);
    }

    /// A guardrail never contributes to the rank, and a significant guardrail
    /// IMPROVEMENT is harmless (no veto). The rank reflects ONLY the objective.
    #[test]
    fn noise_guardrail_does_not_contribute_to_rank() {
        let scorer = WeightedSumScorer::with_noise_guardrails(
            objective("tp", Direction::Maximize),
            vec![],
            vec![NoiseGuardrailDef {
                name: "mem".to_string(),
                direction: Direction::Minimize,
            }],
        );
        // mem improves a lot (100→10, well beyond noise) — but guardrails don't
        // earn rank. tp 100→110 is the only contributor → rank 0.10.
        let mut score_input = input(
            &[("tp", 100.0), ("mem", 100.0)],
            &[("tp", 110.0), ("mem", 10.0)],
        );
        let tight = MetricVariance {
            stddev: Some(2.0),
            ci_lower: Some(95.0),
            ci_upper: Some(105.0),
        };
        score_input.candidate_variances = variances(&[("mem", tight)]);
        score_input.best_variances = variances(&[("mem", tight)]);

        let out = scorer.calculate(&score_input).unwrap();
        assert_eq!(out.decision, "keep");
        assert!((out.rank - 0.10).abs() < 1e-9, "rank {}", out.rank);
        let details = out.details.unwrap();
        assert_eq!(
            details.len(),
            1,
            "guardrail must not appear as a contributor"
        );
        assert_eq!(details[0].name, "tp");
    }

    /// Backward-compat pin: with NO guardrails declared, `with_noise_guardrails`
    /// (empty) produces a rank and decision bit-for-bit identical to the legacy
    /// scorer. This guarantees Part C is a pure opt-in.
    #[test]
    fn absent_noise_guardrails_reproduces_legacy_rank_exactly() {
        let primary = vec![
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
        ];
        let legacy = WeightedSumScorer::new(primary.clone(), vec![]);
        let with_empty = WeightedSumScorer::with_noise_guardrails(primary, vec![], vec![]);
        let inp = input(
            &[("throughput", 100.0), ("latency", 50.0)],
            &[("throughput", 110.0), ("latency", 40.0)],
        );
        let a = legacy.calculate(&inp).unwrap();
        let b = with_empty.calculate(&inp).unwrap();
        assert_eq!(a.rank, b.rank);
        assert_eq!(a.decision, b.decision);
        assert_eq!(a.reason, b.reason);
        assert_eq!(a.details, b.details);
        // And it matches the documented legacy value.
        assert!((b.rank - 0.125).abs() < 1e-12, "rank {}", b.rank);
    }
}
