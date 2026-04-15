# Scoring And Rank

`IterationRecord.rank` is an improvement score, not the raw metric value.

## Baseline row

At task start, `cmd_run` records the baseline as a synthetic ledger row with
`rank = 0.0`. This is not computed by a scorer; it is hardcoded so the ledger
has an anchor row before iteration 1.

See:

- `crates/autotune/src/main.rs` — baseline ledger append

## What weighted-sum rank means

For `WeightedSumScorer`, rank is the weighted sum of per-metric deltas against
`input.best`, not `input.baseline`.

For each primary metric:

- `Maximize`: `(candidate - best) / abs(best)`
- `Minimize`: `(best - candidate) / abs(best)`

Then:

- `rank += weight * delta`

With a single weighted metric, rank is therefore just the relative improvement
over the current best kept result.

Examples:

- Baseline coverage `0.80`, candidate `0.872` → rank `0.09` (9% relative gain)
- Baseline coverage `0.80`, candidate `0.872` does **not** mean rank `87.2%`

See:

- `crates/autotune-score/src/weighted_sum.rs` — `improvement()` and
  `WeightedSumScorer::calculate()`

## What "best" means today

`run_scoring` constructs `ScoreInput` like this:

- `baseline`: the metrics from the ledger's baseline row
- `best`: the most recent kept iteration, or baseline if nothing has been kept
- `candidate`: the current iteration's measured metrics

That means weighted-sum rank is:

- baseline-relative for iteration 1
- incremental relative to the latest kept result for later iterations

So rank is not a stable absolute score over the whole task; it is a local
improvement signal for the current candidate.

See:

- `crates/autotune/src/machine.rs` — `run_scoring()`

## Consequences

- `target_improvement` currently compares against the latest kept iteration's
  rank, not a recomputed "improvement over baseline" value.
- The report column labeled `Rank` is showing this improvement score, not the
  underlying metric.
- The research-agent prompt currently says weighted-sum scoring is "relative to
  baseline", which is only strictly true before the first kept iteration.

If future work wants a baseline-relative score throughout the run, either the
scorer must use `input.baseline` for rank, or the system must record both
"delta vs baseline" and "delta vs best" separately.
