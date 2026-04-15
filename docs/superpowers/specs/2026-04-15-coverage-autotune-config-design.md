## Goal

Add a project `.autotune.toml` that tunes for higher full-workspace line coverage in this repository.

## Constraints

- Use the full workspace test suite for each iteration.
- Measure coverage with `llvm-cov`.
- Optimize total line coverage, not branch coverage or per-crate coverage.

## Configuration Design

The config will define a single coverage-improvement task on `main`.

`[paths]` will allow edits under `crates/` so the implementation agent can add or adjust Rust tests and related source changes that improve coverage without touching unrelated repository areas.

`[[test]]` will run `cargo nextest run` as the gating test phase. This keeps Autotune's explicit Testing phase aligned with the repository's standard full-workspace validation path.

`[[measure]]` will run `cargo llvm-cov nextest --workspace --summary-only` to collect instrumented coverage data for the full workspace. The measure step will use the built-in regex adaptor to extract the aggregate line coverage percentage from the `TOTAL` summary row, avoiding any helper script.

`[score]` will maximize a single metric named `line_coverage` with a weighted-sum scorer. Higher values are better, and no extra guardrails are needed for the first version because the explicit test phase already prevents obvious regressions.

## Tradeoffs

This setup is slower than crate-scoped tuning because both testing and coverage measurement operate on the full workspace. The benefit is that the optimization target matches the stated goal and avoids local improvements that do not move the repository-wide coverage number.

Separating `test` from `measure` is slightly redundant because `llvm-cov` runs instrumented tests as part of measurement, but keeping both phases preserves the state machine's clearer failure boundaries and makes failed test iterations easier to diagnose.

## Verification

The resulting config should support:

- `autotune run` creating a coverage-focused task.
- failed test iterations being discarded during the Testing phase.
- successful iterations producing a `line_coverage` metric during Measuring.
- Scoring treating larger `line_coverage` values as improvements.
