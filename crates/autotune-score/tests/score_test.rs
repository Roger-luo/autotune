use autotune_score::script::ScriptScorer;
use autotune_score::threshold::{Direction as TDirection, ThresholdConditionDef, ThresholdScorer};
use autotune_score::weighted_sum::{
    Direction, GuardrailMetricDef, PrimaryMetricDef, WeightedSumScorer,
};
use autotune_score::{Metrics, ScoreCalculator, ScoreInput};

fn make_input(best: &[(&str, f64)], candidate: &[(&str, f64)]) -> ScoreInput {
    let to_map = |pairs: &[(&str, f64)]| -> Metrics {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    };

    ScoreInput {
        baseline: to_map(best),
        candidate: to_map(candidate),
        best: to_map(best),
    }
}

#[test]
fn weighted_sum_minimize_improvement() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("time_us", 200.0)], &[("time_us", 170.0)]);
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "keep");
    assert!((result.rank - 0.15).abs() < 0.001);
}

#[test]
fn weighted_sum_minimize_regression() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("time_us", 200.0)], &[("time_us", 220.0)]);
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "discard");
    assert!(result.rank < 0.0);
}

#[test]
fn weighted_sum_maximize_improvement() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "throughput".to_string(),
            direction: Direction::Maximize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("throughput", 100.0)], &[("throughput", 120.0)]);
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "keep");
    assert!((result.rank - 0.2).abs() < 0.001);
}

#[test]
fn weighted_sum_guardrail_blocks() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![GuardrailMetricDef {
            name: "accuracy".to_string(),
            direction: Direction::Maximize,
            max_regression: 0.01,
        }],
    );

    let input = make_input(
        &[("time_us", 200.0), ("accuracy", 0.99)],
        &[("time_us", 150.0), ("accuracy", 0.95)],
    );
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "discard");
    assert!(result.reason.contains("guardrail"));
}

#[test]
fn weighted_sum_guardrail_passes() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![GuardrailMetricDef {
            name: "accuracy".to_string(),
            direction: Direction::Maximize,
            max_regression: 0.05,
        }],
    );

    let input = make_input(
        &[("time_us", 200.0), ("accuracy", 1.0)],
        &[("time_us", 150.0), ("accuracy", 0.97)],
    );
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "keep");
}

#[test]
fn weighted_sum_zero_best_improvement_is_not_neutral() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "m".to_string(),
            direction: Direction::Maximize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("m", 100.0)], &[("m", 1.0)]);
    let input = ScoreInput {
        baseline: input.baseline,
        candidate: input.candidate,
        best: [("m", 0.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    };
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "keep");
    assert!(result.rank > 0.0);
}

#[test]
fn weighted_sum_zero_best_guardrail_blocks() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "m".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![GuardrailMetricDef {
            name: "m".to_string(),
            direction: Direction::Minimize,
            max_regression: 0.01,
        }],
    );

    let input = make_input(&[("m", 10.0)], &[("m", 1.0)]);
    let input = ScoreInput {
        baseline: input.baseline,
        candidate: input.candidate,
        best: [("m", 0.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    };
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "discard");
    assert!(result.reason.contains("guardrail"));
}

#[test]
fn weighted_sum_uses_best_not_baseline() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = ScoreInput {
        baseline: [("time", 100.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        candidate: [("time", 60.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        best: [("time", 50.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    };
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "discard");
    assert!(result.rank < 0.0);
}

#[test]
fn weighted_sum_multiple_metrics_weighting() {
    let scorer = WeightedSumScorer::new(
        vec![
            PrimaryMetricDef {
                name: "time".to_string(),
                direction: Direction::Minimize,
                weight: 2.0,
            },
            PrimaryMetricDef {
                name: "mem".to_string(),
                direction: Direction::Minimize,
                weight: 1.0,
            },
        ],
        vec![],
    );

    let input = make_input(
        &[("time", 100.0), ("mem", 200.0)],
        &[("time", 90.0), ("mem", 190.0)],
    );
    let result = scorer.calculate(&input).unwrap();

    assert!((result.rank - 0.25).abs() < 0.001);
    assert_eq!(result.decision, "keep");
}

#[test]
fn threshold_all_pass() {
    let scorer = ThresholdScorer::new(vec![ThresholdConditionDef {
        metric: "size_kb".to_string(),
        direction: TDirection::Minimize,
        threshold: 0.0,
    }]);

    let input = make_input(&[("size_kb", 100.0)], &[("size_kb", 95.0)]);
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "keep");
}

#[test]
fn threshold_fails() {
    let scorer = ThresholdScorer::new(vec![ThresholdConditionDef {
        metric: "size_kb".to_string(),
        direction: TDirection::Minimize,
        threshold: 0.0,
    }]);

    let input = make_input(&[("size_kb", 100.0)], &[("size_kb", 105.0)]);
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.decision, "discard");
}

#[test]
fn script_scorer_echo_json() {
    let scorer = ScriptScorer::new(vec![
        "sh".to_string(),
        "-c".to_string(),
        r#"printf '%s' '{"rank":0.5,"decision":"keep","reason":"looks good"}'"#.to_string(),
    ]);

    let input = make_input(&[("m", 1.0)], &[("m", 2.0)]);
    let result = scorer.calculate(&input).unwrap();

    assert_eq!(result.rank, 0.5);
    assert_eq!(result.decision, "keep");
}
