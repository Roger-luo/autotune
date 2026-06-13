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

## Footgun: manual edits to the advancing branch diverge from the ledger

The ledger is autotune's source of truth in two places that both assume the
advancing branch contains *exactly* the kept commits the ledger records:

- **Scoring** (`run_scoring`) takes `best` = the most recent `Kept` ledger row's
  metrics.
- **Planning** (`build_planning_prompt`) echoes each iteration's approach +
  `status` + metrics, so the research agent's mental model of "what is already
  applied" comes from the ledger, not the working tree.

If you rewrite the advancing branch *outside* autotune — e.g. `git revert` a
commit a kept iteration produced, or hand-add commits — that assumption breaks
and nothing reconciles it:

- Scoring still treats the reverted iteration's metrics as `best`, even though
  the branch no longer performs that way. The next candidate is built on the
  (now slower) post-revert branch but judged against the unreachable old `best`,
  so it is likely discarded as "no improvement" — a **false discard**.
- Planning tells the agent the reverted iteration was `Kept` with its old
  metrics, so the agent plans on top of an optimization that is no longer in the
  code — it may re-propose the reverted change or build on a false premise.

`resume` does **not** re-measure the advancing-branch HEAD or diff it against the
recorded commits; the ledger doesn't even store per-iteration commit SHAs. This
was observed first-hand dogfooding ppvm's `trotter-perf-3`: iteration 5 ("drop
the `contains_with` probe") was recorded `Kept`, then reverted by hand on the
branch, and the resumed iteration 6 was still told iter 5 was applied and was
still scored against iter 5's metrics.

Recommendations until this is reconciled in code:

- Prefer letting autotune discard an iteration over hand-reverting a kept
  commit. If you must revert, expect the next iteration to be scored against a
  stale `best`.
- A proper fix would record each kept iteration's commit SHA in the ledger,
  detect on `resume` when `HEAD` != the last kept SHA, and re-measure to refresh
  `best` (and mark reverted rows so the planning prompt stops advertising them
  as applied).

See:

- `crates/autotune/src/machine.rs` — `run_scoring()` (`best` selection)
- `crates/autotune-plan/src/lib.rs` — `build_planning_prompt()` (ledger echo)
