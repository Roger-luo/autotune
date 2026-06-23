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

## Measurement robustness: cross-build envelope + two-phase confirmation (opt-in)

Within-run criterion CIs only see **within-build** sampling jitter. They do NOT
capture **cross-build** codegen/layout noise — the thing that swung unrelated
ppvm #149 benches ±35% between rebuilds. Two opt-in `[score]` knobs harden the
loop against this, both gated on `AutotuneConfig::optimizes_runtime_perf()` (any
criterion measure declared) so deterministic tasks (coverage/size) pay ZERO
extra cost, and both OFF by default (identical to the prior behavior).

### Within-run vs cross-build noise (the model)

- **Within-run**: re-sampling the *same compiled binary* (criterion's CI /
  stddev). Sizes the existing envelope.
- **Cross-build**: re-*compiling* and re-running. Codegen and binary layout
  change, so an unrelated bench can shift well beyond its within-run CI. This is
  the honest, usually larger envelope — and the real ppvm #149 failure mode.

`noise_envelope(best, cand_var, best_var, empirical, &NoiseConfig)` now takes the
**MAX** of the within-run component and a per-metric `empirical` cross-build
floor. The MAX (never a sum, never a min) means the cross-build floor can only
*widen* the envelope, and a genuinely wide within-run CI is never shrunk.

### Option 1: `[score] baseline_replicates = N` (default `0` = off)

When a perf task sets `N > 0`, baseline measurement is repeated `N` extra times
at task start. The original baseline value plus the `N` replicates form a series;
`empirical_cross_build_envelope(&[f64])` takes the **half-range** `(max-min)/2`
(a radius comparable to a CI half-width). The per-metric map is persisted to
`.autotune/tasks/<name>/noise_envelope.json` (`TaskStore::save/load_empirical_envelope`)
and folded into EVERY iteration's scoring via `ScoreInput.empirical_envelope`.

`[score] replicate_rebuild` (default `true`): the rebuild between replicates is
the POINT — it re-rolls codegen/layout. AutoTune forces it build-system-agnostically
by setting a distinct, inert `RUSTFLAGS="--cfg autotune_baseline_replicate=\"K\""`
per replicate, which changes Cargo's build fingerprint so the project recompiles.
Set `false` for a cheaper no-rebuild spread (captures only re-run jitter — much
weaker, but free of recompiles). Deterministic tasks ignore `N` entirely.

### Option 5: `[score] confirm_significant = true` (default `false`)

When a perf task enables it, after the first scoring pass any metric that DROVE
the keep/discard decision **and** was significant (delta beyond the envelope) is
re-confirmed: the candidate is re-measured ONCE (rebuild + re-run via the same
`RUSTFLAGS` cfg trick), and each driving metric's significance is re-checked
against the freshly-measured value. A driving metric that no longer reproduces as
significant is added to `excluded_metrics` (treated as noise) and the candidate
is re-scored, so a **one-off swing never flips a decision**. A metric that
reproduces stands. Cost is bounded to one extra full measurement pass — the
measure command emits all metrics together, so the targeting is in *which
metrics' significance we re-check* (only the drivers via `significant_driving_metrics`),
not in isolating a single bench. The re-measured candidate values + variances
replace the one-off measurement on the recorded row; the ledger `reason` and the
`phase.decision` trace record that a confirmation pass ran and its outcome.

### Backward-compat pins

- `empty_empirical_envelope_reproduces_legacy_rank_exactly` (`weighted_sum.rs`):
  an empty empirical map + no variances ⇒ the documented #18 rank/decision,
  bit-for-bit.
- `noise_envelope_zero_empirical_is_identity` (`lib.rs`): `empirical == 0.0`
  leaves the within-run envelope untouched.
- Scenario `scenario_within_noise_regression_is_not_counted` runs with the knobs
  at their defaults and still asserts the #18 outcome.

### Cost implications

- `baseline_replicates = N` ⇒ `N` extra full baseline measurements ONCE at task
  start (each a rebuild + bench run). A one-time cost amortized over the run.
- `confirm_significant` ⇒ at most one extra measurement pass per iteration, and
  only when a significant metric drove the decision (the common no-significant
  iteration pays nothing).
- Both are no-ops for deterministic (non-criterion) tasks.

### Determinism / isolation recommendations (option 3)

Replication (option 1) is the real mitigation for cross-build noise, but reduce
the noise at the source too:

- **Isolate bench binaries** so unrelated metrics don't share a process — a
  layout change in one shouldn't perturb another's measurement.
- **`codegen-units = 1`** (and a fixed `opt-level`) in the bench profile so
  codegen is more reproducible build-to-build.
- **Pin CPU frequency** (disable turbo / set a fixed governor) and quiesce the
  machine; thermal/frequency drift is a large noise source.
- Remember the **within-run-vs-cross-build distinction**: criterion's CI will
  look tight even while cross-build swings are huge. Don't trust a tight CI as
  proof a delta is real on a perf task — that's exactly what option 1 measures.

A measure can already set its own build environment by putting it in the measure
`command` (e.g. `["sh","-c","RUSTFLAGS=… cargo bench …"]`), and AutoTune passes
per-replicate/confirmation env through `run_all_measures_with_output_env`. No
dedicated per-measure `env`/`RUSTFLAGS` config key was added — the command-level
escape hatch covers it.

## Causal attribution (opt-in)

`[[measure]] sources = ["glob/**", …]` declares which source paths a measure
exercises. When set, a metric whose iteration `changed_files` do **not** intersect
its `sources` globs is flagged `MetricBreakdown.causally_unrelated =
true` and added to `ScoreInput.excluded_metrics`, which the built-in scorers
treat exactly like a within-noise delta (zero contribution, no guardrail trip).
A change can't be blamed for moving code it never touched. When `sources` is
absent the set is empty and behavior is unchanged. Candidate `changed_files` are
computed at scoring time from the worktree commit (`diff_name_only`).

### Per-metric `sources` (finer than per-measure)

One measure can emit many metrics — a single criterion bench binary produces
`micro_cnot`, `sparse-vec/trim`, … — and a measure-level glob can't separate a
touched metric from an untouched co-located one. So `sources` may also be
declared at **metric granularity** on a regex pattern, a criterion benchmark, or
a judge rubric:

```toml
[[measure]]
name = "bench"
sources = ["src/**"]                       # measure-level fallback
adaptor = { type = "criterion", benchmarks = [
  { name = "micro_cnot",  group = "gates/cnot",      sources = ["src/gates/cnot.rs"] },
  { name = "sparse_trim", group = "sparse-vec/trim" },   # inherits "src/**"
] }
```

Precedence per metric: the metric's own `sources` (if non-empty) else the
measure's `sources` else none. `AutotuneConfig::metric_sources()` resolves it;
`machine::metric_sources` delegates. So `sparse_trim` is `causally_unrelated`
when a diff touches only `src/gates/cnot.rs`, even though `micro_cnot` (its
co-located metric) is not. Absent everywhere ⇒ today's behavior.

## Guardrail metrics: noise-tolerant constraints

A weighted-sum primary can be declared a **guardrail/constraint** (not an
optimization target) with `guardrail = true`:

```toml
[score]
type = "weighted_sum"
primary_metrics = [
  { name = "throughput", direction = "Maximize", weight = 1.0 },
  { name = "peak_mem",   direction = "Minimize", guardrail = true },
]
```

A guardrail metric: (i) **never contributes to the rank** (its `weight` is
ignored — it's split into `WeightedSumScorer`'s `noise_guardrails`); (ii) can
only **VETO** (force discard) when it regresses by MORE than its noise envelope;
(iii) ignores within-noise moves, and a significant *improvement* is harmless.
The veto threshold IS the noise envelope (reuses `noise_envelope`) — unlike
`[[score.guardrail_metrics]]`, which needs an explicit `max_regression`, so a
guardrail never trips on jitter. This is "declaring how a metric participates"
(specialize by declaration, not a mode flag). Default (none declared) ⇒
identical rank/decision (pinned by
`absent_noise_guardrails_reproduces_legacy_rank_exactly`); a weighted-sum config
must keep at least one non-guardrail primary (validated).

See:

- `crates/autotune-config/src/lib.rs` — `RegexPattern/CriterionBenchmark/
  RubricConfig::sources`, `metric_sources()`, `PrimaryMetric::guardrail`.
- `crates/autotune-score/src/weighted_sum.rs` — `NoiseGuardrailDef`,
  `with_noise_guardrails`, the veto loop in `calculate`.

- `crates/autotune-score/src/lib.rs` — `MetricVariance`, `NoiseConfig`,
  `noise_envelope` (now MAX of within-run and `empirical`), `within_noise`,
  `empirical_cross_build_envelope`, `ScoreInput.{candidate_variances,
  best_variances, noise, excluded_metrics, empirical_envelope}`.
- `crates/autotune-adaptor/src/criterion.rs` — `extract_variances`,
  `estimates_files`.
- `crates/autotune-config/src/lib.rs` — `ScoreConfig::{baseline_replicates,
  replicate_rebuild, confirm_significant}`.
- `crates/autotune-benchmark/src/lib.rs` — `run_all_measures_with_output_env`
  (per-replicate/confirmation `RUSTFLAGS` passthrough).
- `crates/autotune-state/src/lib.rs` — `TaskStore::{save,load}_empirical_envelope`
  (the persisted `noise_envelope.json`).
- `crates/autotune/src/main.rs` — `collect_baseline_replicate_envelope` (option 1).
- `crates/autotune/src/machine.rs` — `run_measuring` (variance capture + copy),
  `best_variances_from_ledger`, `run_scoring` (loads the empirical envelope,
  runs confirmation), `run_confirmation_pass` + `significant_driving_metrics`
  (option 5), `build_score_breakdown`, `metric_sources`,
  `causally_unrelated_metrics`.

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
