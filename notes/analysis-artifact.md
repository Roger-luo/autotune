# Analysis Artifact

`autotune analyze` (and the `analysis` key embedded in `autotune export`) emit a
single, self-contained, machine-readable JSON artifact so an external tool or
agent can reason about every improvement/regression in a run **without
re-joining the scattered per-iteration files** (`ledger.json`, per-iteration
`metrics.json`, raw measure stdout/stderr, etc.).

Built by `build_analysis_json` in `crates/autotune/src/main.rs`. The shape is
versioned by `ANALYSIS_SCHEMA_VERSION` (currently `1`) — bump it on any breaking
change.

## Two commands, one artifact

- **`autotune analyze [--task <name>] [--output <path>]`** writes ONLY the
  artifact (to stdout by default). This is the focused, documented format a
  downstream consumer should target.
- **`autotune export --task <name> --output <path>`** stays backward compatible:
  it preserves its original top-level keys (`task_name`, `config`, `ledger`,
  `log`, `state`) and *additionally* embeds the same artifact under a new
  `analysis` key. Old consumers keep working; new ones can read `.analysis`.

## Schema (version 1)

Top-level object:

- `schema_version` (int) — `ANALYSIS_SCHEMA_VERSION`.
- `task` — `{ name, description?, phase, iteration }`. `description` is parsed
  best-effort from the config snapshot's `[task].description`.
- `config` (string) — the raw `.autotune.toml` snapshot frozen at task start
  (full reproducibility; the artifact never paraphrases config).
- `log` (string) — the research agent's durable findings (`log.md`).
- `metric_names` (string[]) — sorted union of every metric name seen anywhere in
  the ledger.
- `baseline` — `{ iteration, metrics }` for the baseline row, or `null`.
- `metric_matrix` — object keyed by metric name. Each value is an array of one
  entry per ledger row that measured that metric:
  - `iteration`, `status`,
  - `value`,
  - `delta_vs_baseline` — `value - baseline_value` (raw; `null` if no baseline),
  - `delta_vs_best` — `value - running_best`, where the running best is the most
    recent prior `Kept`/`Baseline`/`Reverted` value (mirrors
    `best_metrics_from_ledger` in `machine.rs`). `null` for the first row.
- `iterations` — one object per ledger row, carrying everything an analyzer
  needs joined in place:
  - `iteration`, `approach`, `status`, `hypothesis`, `rank`,
  - `decision` (the ledger's free-text `score`: `"keep"`/`"discard"`),
  - `reason`, `commit_sha`, `reverted_iteration`, `fix_attempts`,
    `fresh_spawns`, `timestamp`,
  - `metrics` — this iteration's measured values,
  - `changed_files` — the files this iteration's commit touched vs its parent on
    the advancing branch (see below); `null` if not captured,
  - `score_breakdown` — the structured per-metric breakdown (see below),
  - `measure_output_dir` / `measure_output_files` — repo-relative references to
    the raw measure stdout/stderr saved under
    `iterations/<NNN>-<slug>/measure_output/` (not inlined — referenced so the
    artifact stays small; read on demand).

### `score_breakdown` (per iteration)

`autotune_state::ScoreBreakdown { decision, metrics: [MetricBreakdown] }`.

Each `MetricBreakdown` records, for one scored metric:

- `name`,
- `baseline` / `candidate` / `best` — the three values scoring compared,
- `delta_vs_baseline` / `delta_vs_best` — raw differences (NOT
  direction-normalized),
- `direction` — `"higher"` or `"lower"` (from config), or `null` for scorers
  that declare none (script/command),
- `weight` — weight in the weighted sum, or `null`,
- `improvement_vs_best` — the **direction-normalized** improvement fraction that
  actually feeds the rank (weighted-sum only), or `null`,
- `contribution` — `weight * improvement_vs_best`, this metric's share of the
  rank (weighted-sum only), or `null`.

The breakdown is populated at the Scoring phase (`run_scoring` →
`build_score_breakdown`) and carried on `ApproachState.score_breakdown` to the
recording path, so BOTH kept rows (integration) and scorer-discarded rows
persist it. Pre-scoring discards (test/hook failures) and crash rows have no
breakdown.

## Where the data comes from

- **Structured score detail.** `autotune_score::ScoreOutput` grew an optional
  `details: Option<Vec<ScoreMetricContribution>>`. `WeightedSumScorer` populates
  it (`name`, `delta`, `weight`, `contribution`); `ThresholdScorer` and
  `ScriptScorer` leave it `None`. The field is `#[serde(default)]` so script
  scorers whose JSON omits it still deserialize, and the `ScoreCalculator` trait
  is unchanged. The CLI merges the scorer's `details` with the baseline/best
  values it holds to build the richer `MetricBreakdown`.
- **Changed files.** `autotune_git::diff_name_only(dir, sha)` runs
  `git diff --name-only <sha>^..<sha>` (with an empty-tree fallback for a root
  commit). Captured in `run_integrating` from the post-rebase advancing-branch
  SHA, in the advancing worktree. Kept clean and reusable — a follow-up will use
  it for noise-aware scoring (flagging deltas attributed to a diff that never
  touched the relevant code).

## Backward compatibility

All new fields on `IterationRecord` (`score_breakdown`, `changed_files`) and on
`ApproachState` (`score_breakdown`) are `#[serde(default)]`, so ledgers and
state files written before this feature still deserialize (their new fields read
as `None`). Pinned by `legacy_iteration_record_without_analysis_fields_*`
tests in `crates/autotune-state/src/lib.rs`.

## Deferred follow-up: variance / confidence intervals

The artifact does NOT yet capture per-metric stddev/CIs (e.g. copying criterion
`estimates.json`). That's valuable for noise-aware scoring but was deferred to
keep this change focused. When added, it should hang off each `MetricBreakdown`
(or the matrix entries) so the schema bump is localized.

See:

- `crates/autotune/src/main.rs` — `build_analysis_json`, `cmd_analyze`,
  `cmd_export`, `ANALYSIS_SCHEMA_VERSION`.
- `crates/autotune/src/machine.rs` — `run_scoring`, `build_score_breakdown`,
  `run_integrating` (changed-files capture).
- `crates/autotune-state/src/lib.rs` — `ScoreBreakdown`, `MetricBreakdown`, the
  new `IterationRecord` fields.
- `crates/autotune-score/src/lib.rs` — `ScoreOutput.details`,
  `ScoreMetricContribution`.
- `crates/autotune-git/src/lib.rs` — `diff_name_only`.
