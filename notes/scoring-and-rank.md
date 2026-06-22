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
recorded commits to detect manual surgery (kept rows now carry a `commit_sha`,
but nothing reconciles a hand-edit against it). This was observed first-hand
dogfooding ppvm's `trotter-perf-3` (before per-iteration SHAs were recorded):
iteration 5 ("drop the `contains_with` probe") was recorded `Kept`, then
reverted by hand on the branch, and the resumed iteration 6 was still told iter
5 was applied and was still scored against iter 5's metrics.

The supported way to undo a kept iteration is **`autotune revert <iteration>`**.
It records the inverse commit's SHA, re-measures the advancing branch, and
appends a `Reverted` checkpoint row. `best_metrics_from_ledger` counts
`Kept | Baseline | Reverted` rows (skipping empty-metrics rows), so after a
revert `best` is refreshed to the last non-reverted kept result — no false
discards, no stale planning context.

A *manual* `git revert` outside autotune is still unreconciled: the ledger
won't know the branch changed and the issues described above still apply. Use
the command instead.

See:

- `crates/autotune/src/machine.rs` — `run_scoring()` (`best` selection)
- `crates/autotune-plan/src/lib.rs` — `build_planning_prompt()` (ledger echo)

## Criterion adaptor: config `group` is the benchmark id, not the on-disk path

`[measure.adaptor] type = "criterion"` takes a `group` per metric. The natural
thing for a user to write there is the benchmark id exactly as `cargo bench`
prints it — e.g. `gates/two-qubit/cnot` or `sparse-vec/u128/add_or_insert/existing`.
That is **not** the on-disk directory.

Criterion sanitizes the path-separator `/` *within* each id component (the group
name and the function id) into `_` when it builds the report directory, keeping
only the single `/` between group and function. So:

| benchmark id (`cargo bench` / config `group`) | on-disk `target/criterion/<dir>/new/estimates.json` |
|---|---|
| `gates/two-qubit/cnot` | `gates_two-qubit/cnot` |
| `sparse-vec/u128/add_or_insert/existing` | `sparse-vec/u128_add_or_insert_existing` |
| `msd/msd-0` | `msd/msd-0` (no inner `/`, unchanged) |

You cannot reconstruct the directory from the `group` string alone (you'd have to
know where the group ends and the function begins). The adaptor therefore:

1. tries the literal path `criterion_dir/<group>/new/estimates.json` first
   (fast path, and what already-working configs with slash-free groups rely on);
2. on a miss, walks `target/criterion/**/new/benchmark.json` once, reads each
   file's logical `full_id`, and matches `group` against it — then reads the
   `estimates.json` next to the matching `benchmark.json`.

This was a real dogfooding malfunction: a perfectly reasonable ppvm config using
`gates/two-qubit/cnot` failed baseline with `CriterionNotFound` even though the
bench had just run, because the adaptor only tried the literal path. Note that
Criterion does **not** sanitize spaces/commas (only path separators), which is
why the older `trotter-scaling` config — group `"ByteF64FxIndexMap_8, …"`, no
inner `/` — resolved fine as a literal path and never hit this.

See:

- `crates/autotune-adaptor/src/criterion.rs` — `estimates_path()` (literal) and
  `build_full_id_index()` (the `benchmark.json` `full_id` fallback)
