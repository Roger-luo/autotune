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

## Noise-aware scoring

A metric delta only counts — as an improvement **or** a regression — when its
magnitude exceeds the **noise envelope**. Within-noise deltas are excluded from
the rank/weighted-sum contribution, from guardrail tripping, and from
regression reporting. This stops AutoTune treating same-binary codegen/layout
jitter as causal signal (the Clifford episode: criterion reported a `sparse-vec`
"+31% regression" on a diff that only touched `clifford.rs`, and re-runs swung
±35%).

### The envelope (per metric)

`autotune_score::noise_envelope(best_value, candidate_var, best_var, &NoiseConfig)`
picks, in order:

1. **Confidence intervals** (criterion's primary model): `candidate_ci_half_width
   + best_ci_half_width`. Two independent measurements, so the error radii add.
   CI half-width is `(upper - lower) / 2`.
2. **Stddev**: `noise_k * max(candidate_stddev, best_stddev)`. `noise_k`
   defaults to `2.0` (~95% band, matching criterion's CI confidence level).
   Used when criterion supplied stddev but no CI.
3. **Relative floor**: `noise_threshold * |best_value|`. Used when no per-metric
   variance is available at all.

`within_noise(delta, …)` returns `|delta| <= envelope`. A metric flagged noisy
gets `contribution = 0` and `MetricBreakdown.within_noise = true`.

### Where variance comes from

`CriterionAdaptor::extract_variances` reads the selected stat's
`confidence_interval {lower_bound, upper_bound}` and the `std_dev` point estimate
from the same `estimates.json` it reads the mean from. Other adaptors
(regex/script) return no variance. The per-iteration `estimates.json` is **copied
into the iteration dir** (`iterations/<NNN>-<slug>/measures/<metric>.estimates.json`)
at measure time so the CI data survives after the worktree is removed.

Variances flow: adaptor → `MeasureReport.variances` →
`ApproachState.variances` → persisted on the ledger row's `variances` field. At
scoring, the candidate's variance comes from `ApproachState`; the `best` row's
variance is recovered from the ledger via `best_variances_from_ledger` (mirrors
`best_metrics_from_ledger`). The three crates each define their own
`MetricVariance` (adaptor/state/score), mapped at the `main.rs`/`benchmark`
boundaries — same pattern as the parallel `Direction` enums (keeps each a leaf).

### Backward compatibility (critical)

The default `NoiseConfig` is the **identity**: `relative_threshold = 0.0`,
`stddev_k = 2.0`. With no variance present and a zero threshold, the envelope is
`0.0`, so only an exactly-zero delta is "within noise" — and a zero delta already
contributes nothing. So with no variance AND no `[score] noise_threshold`,
scoring is **bit-for-bit identical to before**. Pinned by
`no_variance_zero_threshold_reproduces_legacy_rank_exactly` in `weighted_sum.rs`.

### Config

`[score] noise_threshold` (relative, default `0.0`) and `noise_k` (default `2.0`)
on the `weighted_sum` and `threshold` score types. `ScoreConfig::noise_params()`
returns `(threshold, k)`; script/command scorers own their full decision and get
the identity.

## Causal attribution (Part C, opt-in)

`[[measure]] sources = ["glob/**", …]` declares which source paths a measure
exercises. When set, a metric whose iteration `changed_files` do **not** intersect
its measure's `sources` globs is flagged `MetricBreakdown.causally_unrelated =
true` and added to `ScoreInput.excluded_metrics`, which the built-in scorers
treat exactly like a within-noise delta (zero contribution, no guardrail trip).
A change can't be blamed for moving code it never touched. When `sources` is
absent the set is empty and behavior is unchanged. Candidate `changed_files` are
computed at scoring time from the worktree commit (`diff_name_only`).

See:

- `crates/autotune-score/src/lib.rs` — `MetricVariance`, `NoiseConfig`,
  `noise_envelope`, `within_noise`, `ScoreInput.{candidate_variances,
  best_variances, noise, excluded_metrics}`.
- `crates/autotune-adaptor/src/criterion.rs` — `extract_variances`,
  `estimates_files`.
- `crates/autotune/src/machine.rs` — `run_measuring` (variance capture + copy),
  `best_variances_from_ledger`, `run_scoring`, `build_score_breakdown`,
  `metric_sources`, `causally_unrelated_metrics`.

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
