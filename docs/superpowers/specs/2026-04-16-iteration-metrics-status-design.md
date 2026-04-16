# Iteration Metrics Status Design

## Goal

After each iteration is scored, the CLI should print the current measured metrics for that iteration in addition to the delta-based `rank`.

## Current Behavior

`run_scoring()` in `crates/autotune/src/machine.rs` prints a single line with:

- `rank`
- `decision`
- `reason`

The measured metrics already exist in memory as `candidate_metrics`, but they are only traced and persisted, not shown in the CLI status output.

## Proposed Change

Keep the existing score line and add a second line immediately after it:

- First line remains the score summary with `rank`, `decision`, and `reason`
- Second line prints every measured metric from `candidate_metrics`

Example shape:

```text
[autotune] iteration 3 — score: rank=0.1234, decision=keep, reason=improved latency
[autotune] iteration 3 — metrics: latency_ms=12.3000, throughput=88.0000
```

## Output Rules

- Metrics are printed in stable key order so output is deterministic.
- Every measured metric for the iteration is included.
- This is a presentation-only change; no state schema changes are required.
- The existing score line format should remain intact to avoid breaking current expectations.

## Testing

- Add a unit test around the scoring-phase output in `crates/autotune/src/machine.rs`.
- The test should verify that:
  - the score line still appears
  - the metrics line appears
  - the metrics are rendered in stable sorted order

## Risks

- Long metric lists will add output noise, but a separate line keeps the output readable.
- Formatting should stay compact and deterministic to avoid flaky tests.
