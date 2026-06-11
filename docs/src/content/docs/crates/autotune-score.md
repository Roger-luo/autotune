---
title: autotune-score
description: Pluggable scoring for tuning candidates — the ScoreCalculator trait and weighted-sum, threshold, and script scorers.
section: Crates
order: 12
---

`autotune-score` defines the `ScoreCalculator` trait and three built-in implementations — `WeightedSumScorer`, `ThresholdScorer`, and `ScriptScorer`. Each takes a `ScoreInput { baseline, candidate, best }` of per-metric `HashMap<String, f64>` values and returns a `ScoreOutput { rank, decision, reason }` that tells the state machine whether to keep or discard an iteration and how it ranks.

## When to use it

- You're wiring up how Autotune decides whether a candidate is an improvement worth keeping.
- You want weighted improvement scoring with optional regression guardrails (`WeightedSumScorer`), simple pass/fail thresholds (`ThresholdScorer`), or a custom external scoring program (`ScriptScorer`).
- You're implementing a new scoring policy: implement `ScoreCalculator` and it slots into the same pipeline as the built-ins.

## Public API

- `ScoreCalculator` — trait with one method, `calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError>`.
- `ScoreInput` — `{ baseline, candidate, best }`, each a `Metrics` map; `Serialize`/`Deserialize`.
- `ScoreOutput` — `{ rank: f64, decision: String, reason: String }`; `Serialize`/`Deserialize`.
- `Metrics` — type alias for `HashMap<String, f64>`.
- `ScoreError` — error enum: `MissingMetric`, `GuardrailFailed`, `ScriptFailed`, `ScriptOutputParse`, `Io`.
- `weighted_sum::WeightedSumScorer` — `new(primary, guardrails)`; sums weighted per-metric improvements over `best`, rejecting candidates that trip a guardrail.
- `weighted_sum::PrimaryMetricDef` — `{ name, direction, weight }` for a scored metric.
- `weighted_sum::GuardrailMetricDef` — `{ name, direction, max_regression }` for a regression guard.
- `weighted_sum::Direction` — `Minimize` / `Maximize`.
- `weighted_sum::{improvement, check_guardrail, get_metric}` — free functions used by the scorer and reusable directly.
- `threshold::ThresholdScorer` — `new(conditions)`; keeps only when every condition's delta meets its threshold.
- `threshold::ThresholdConditionDef` — `{ metric, direction, threshold }`.
- `threshold::Direction` — `Minimize` / `Maximize` (a distinct enum from `weighted_sum::Direction`).
- `script::ScriptScorer` — `new(command)`; pipes `ScoreInput` as JSON to an external program's stdin and parses a `ScoreOutput` from its stdout.

## Usage

```rust
use std::collections::HashMap;
use autotune_score::{ScoreCalculator, ScoreInput};
use autotune_score::weighted_sum::{
    Direction, GuardrailMetricDef, PrimaryMetricDef, WeightedSumScorer,
};

let scorer = WeightedSumScorer::new(
    vec![PrimaryMetricDef {
        name: "throughput".to_string(),
        direction: Direction::Maximize,
        weight: 1.0,
    }],
    vec![GuardrailMetricDef {
        name: "latency_p99".to_string(),
        direction: Direction::Minimize,
        max_regression: 0.05, // tolerate at most 5% regression
    }],
);

let metric = |pairs: &[(&str, f64)]| -> HashMap<String, f64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
};

let input = ScoreInput {
    baseline: metric(&[("throughput", 100.0), ("latency_p99", 20.0)]),
    best: metric(&[("throughput", 100.0), ("latency_p99", 20.0)]),
    candidate: metric(&[("throughput", 110.0), ("latency_p99", 20.5)]),
};

let output = scorer.calculate(&input).expect("scoring failed");
assert_eq!(output.decision, "keep");
println!("rank={} reason={}", output.rank, output.reason);
```

## Internal dependencies

None — this is a leaf crate. It depends only on `serde`, `serde_json`, and `thiserror`.

## Notes

- `rank` is an improvement score, not a raw metric. For `WeightedSumScorer` it is the weighted sum of per-metric deltas measured *relative to `best`* (not `baseline`): `(candidate - best) / |best|` for `Maximize`, the negation for `Minimize`. With a single weighted metric, a candidate of `0.872` over a best of `0.80` yields rank `0.09` (a 9% relative gain), not `0.872`.
- `decision` and `reason` are plain `String`s, not enums. The built-in scorers emit `"keep"` or `"discard"`; a `ScriptScorer` can return any strings its program prints.
- `weighted_sum::Direction` and `threshold::Direction` are separate enums (and distinct from `autotune_config::Direction`), so callers must map between them.
- When `best` is `0.0`, `improvement` and `check_guardrail` fall back to absolute differences to avoid dividing by zero.
