# `autotune revert` — design

**Date:** 2026-06-12
**Status:** Approved (design); implementation pending

## Motivation

Autotune owns a per-task **advancing branch** and a **ledger** that records each
iteration. The ledger is the source of truth for two things:

- **Scoring** picks `best` = the most recent kept iteration's metrics.
- **Planning** echoes each iteration's status/metrics, so the research agent's
  model of "what is already applied" comes from the ledger, not the working tree.

If a user undoes a kept iteration **outside** autotune (a manual `git revert` on
the advancing branch), nothing reconciles the ledger. Observed dogfooding ppvm's
`trotter-perf-3`: iteration 5 was reverted by hand, but the ledger still treated
it as the kept `best`. On resume the agent was told iter 5 was applied, planned
iteration 6 as *"behavior-identical to the kept iteration 5"*, reintroduced the
exact behavior the user had reverted, and the project's guard test caught it —
the agent churned on a false premise the stale ledger fed it.

The fix is to make reverting a **first-class autotune operation** so the records
stay in sync, with per-iteration commit SHAs as the enabling foundation.

## Goals

- A user can undo a specific kept iteration through autotune, not raw git.
- Scoring and planning immediately reflect the post-revert branch state.
- The ledger remains an accurate, append-only audit trail.

## Non-goals

- Retroactively reconciling tasks whose ledger predates per-iteration SHAs
  (e.g. the current `trotter-perf-3`). Those rows have no SHA; `revert` errors
  with guidance. `trotter-perf-3` itself will be handled by PRing its curated
  branch — out of scope here.
- Reverting uncommitted/discarded/crashed iterations (they were never
  integrated, so there is nothing on the branch to undo).
- Rewriting history to *remove* an iteration's commit (rejected in favor of a
  non-destructive inverse commit — see Decision 1).

## Decisions (resolved during brainstorming)

1. **Mechanism: `git revert` (inverse commit).** Append a new commit that undoes
   iteration N's commit. Non-destructive, auditable, leaves every other
   iteration's SHA valid, and matches the manual pattern users already reach for.
   Rejected: history rewrite (drops the commit but rewrites every later commit's
   SHA, invalidates ledger SHAs, and breaks any external ref/PR to the branch).
2. **Scope: any kept iteration**, not just the tip. `git revert` works on any
   commit; reverting a middle commit that conflicts with a later iteration reuses
   the existing research-agent conflict-resolution path, or aborts cleanly.
3. **Post-revert `best`: re-measure the branch.** After the revert commit lands,
   run the configured measures on the advancing branch and append a checkpoint
   row carrying the fresh metrics, which becomes the new `best`. This is correct
   even for a middle revert, where no previously recorded iteration matches the
   new branch state. Rejected: "mark row, fall back to prior kept metrics" —
   cheap but stale for middle reverts.
4. **Command surface: positional iteration, `--task` override.** `autotune
   revert <iteration>` infers the task from config like `report` does, with
   `--task <T>` as the escape hatch when inference is wrong.
5. **Legacy SHA-less rows: error with guidance.** No `--sha` override, no
   migration path (see Non-goals).

## Data model (`autotune-state`)

`IterationRecord` gains two fields, both `#[serde(default)]` so existing
`ledger.json` files still deserialize:

```rust
/// The commit this row corresponds to on the advancing branch:
/// - Kept rows: the post-integration advancing-branch HEAD.
/// - Reverted rows: the `git revert` (inverse) commit.
#[serde(default)]
pub commit_sha: Option<String>,

/// Set only on `Reverted` rows: the iteration number this revert undid.
#[serde(default)]
pub reverted_iteration: Option<usize>,
```

`IterationStatus` gains a variant (snake_case serde → `"reverted"`):

```rust
pub enum IterationStatus { Baseline, Kept, Discarded, Crash, Reverted }
```

A revert **appends** a `Reverted` row — it does **not** mutate the original kept
row. The ledger stays append-only. The `Reverted` row carries: fresh
re-measured metrics, the revert commit's SHA (`commit_sha`), `reverted_iteration:
Some(N)`, and the optional reason.

### SHA capture for kept rows

The stored SHA for a kept row is the **post-integration advancing-branch HEAD**
(`autotune_git::latest_commit_sha(repo_root)` after the rebase + fast-forward),
**not** the raw worktree `ApproachState.commit_sha`. They are identical in the
common no-conflict case (the worktree branch is one commit ahead of the
advancing HEAD, so the rebase is a no-op ff), but diverge when a conflict-rebase
replays commits into new SHAs. `run_integrating` captures the advancing HEAD and
passes it into `build_kept_record`.

## Git helper (`autotune-git`)

```rust
/// Append an inverse commit that undoes `sha`. Conflicts surface as a distinct
/// error so the caller can route to conflict resolution or abort.
pub fn revert(dir: &Path, sha: &str) -> Result<(), GitError>;
```

Runs `git revert --no-edit <sha>`. A conflict (non-zero exit with conflict
markers) maps to a distinct `GitError` variant (mirroring how rebase conflicts
are surfaced today). Also add `revert_abort(dir)` → `git revert --abort` for the
give-up path.

## Command & flow (`autotune`)

```
autotune revert <iteration> [--task <T>] [--reason "..."] [--no-measure]
```

`cmd_revert(iteration, task, reason, no_measure)`:

1. **Resolve task** — `--task` if given, else `config.task.name` (mirrors
   `report`). Open the `TaskStore`; error clearly if missing.
2. **Load config snapshot** — read `config_snapshot.toml` (same as `resume`) for
   the measure commands + adaptors and the advancing-branch name.
3. **Validate** — two checks:
   - **Clean task state:** no in-progress approach (`state.current_approach`
     is `None`; phase is `Planning`/`Recorded`). Reverting mid-iteration would
     tangle with a half-integrated candidate → error "finish or discard the
     current iteration before reverting".
   - **Target row** for `<iteration>`: must exist, be `Kept`, carry a
     `commit_sha`, and not already be reverted (no later `Reverted` row with
     `reverted_iteration == iteration`). Otherwise a specific error:
     - unknown iteration → "no iteration N in ledger"
     - not kept → "iteration N is <status>, only kept iterations can be reverted"
     - no SHA → "iteration N predates SHA tracking; revert it with git and PR
       the branch" (the legacy case)
     - already reverted → "iteration N was already reverted (see iteration M)"
4. **Revert on the advancing branch** (checked out at repo root):
   `autotune_git::revert(repo_root, sha_N)`.
   - On conflict → reuse `resolve_rebase_conflicts` (grants the research agent
     `Edit`, loops `revert --continue`). If unresolved within budget →
     `revert_abort`, clean error, **ledger untouched**.
5. **Re-measure** (unless `--no-measure`) — run the configured measures at the
   repo root (where the advancing branch is checked out) with a live tail (same
   machinery as `run`), extract metrics via the configured adaptors.
6. **Record** — append a `Reverted` row whose `iteration` is the next monotonic
   ledger index (e.g. reverting iter 5 when the last row is iter 5 appends row
   6): `status: Reverted`, fresh `metrics`, `commit_sha` = the revert commit,
   `reverted_iteration: Some(iteration)`, `reason`. Set
   `state.current_iteration` to that index + 1 and save state so the next
   `resume`/`run` plans cleanly on top.

   **Budget vs. best (two orthogonal aspects):**
   - *Iteration budget:* a `Reverted` row counts toward `max_iterations`
     exactly like a `Discarded` row — it consumes a ledger index and advances
     `current_iteration`, but it is not a "win". (Reverts are rare/manual, so
     nudging the budget by one is acceptable.)
   - *Best-selection:* unlike a discard, the `Reverted` row's fresh re-measured
     metrics **are** included in best-selection (next section), so scoring
     compares future candidates against the true post-revert branch state.
   - With `--no-measure`: the `Reverted` row is appended with empty metrics; a
     warning notes that `best` falls back to the prior measured row and the next
     real iteration will re-establish metrics.

## Scoring & planning stay in sync

This is the payoff — both consumers already read the ledger; we widen what they
count as "the current branch state":

- **`run_scoring` best selection:** the "rows that reflect the real branch" set
  becomes `{Baseline, Kept, Reverted}` (currently `{Baseline, Kept}`). `best` =
  the most recent such row's metrics. A `Reverted` checkpoint therefore becomes
  `best` immediately, and a reverted-then-superseded iteration is never chosen.
  Correct for tip and middle reverts alike.
- **`build_planning_prompt` ledger history:** a `Reverted` row renders as
  e.g. `Iteration 6: reverted iteration 5 (<reason>), status=Reverted` so the
  research agent sees the change was undone instead of building on it. (The
  persistent research session may still remember the original "kept" framing
  from a prior run; the explicit `Reverted` line in the prompt is the corrective
  signal. A `--reason` is encouraged for exactly this.)

## Edge cases & failure handling

- **Conflict during revert** → resolve via research agent, else `revert --abort`
  + clean error; no ledger/state change.
- **Re-measure fails** → the revert commit is already on the branch; record the
  `Reverted` row with empty metrics and warn (don't leave the branch reverted
  but the ledger silent). The next real iteration re-establishes metrics.
- **Legacy SHA-less row** → error with guidance (Non-goal to fix automatically).
- **No advancing branch / dirty repo** → error before touching git.

## Testing

- **`autotune-state` (unit):** new fields round-trip; a legacy `ledger.json`
  without `commit_sha`/`reverted_iteration` still deserializes (extend the
  existing legacy-deserialize test).
- **`autotune-git` (unit):** `revert` produces an inverse commit (HEAD changes,
  file content restored); a conflicting revert surfaces the distinct error;
  `revert_abort` restores HEAD.
- **`autotune-score` / `machine` (unit):** best-selection includes a `Reverted`
  row and skips the undone iteration, including a middle-revert fixture where
  the naive "latest kept" would pick stale metrics.
- **`autotune` (scenario, `scenario_revert_test.rs`):** seed a task with kept
  iterations carrying SHAs on a real advancing branch → `autotune revert
  <N>` → assert (a) a revert commit is on the branch, (b) a `Reverted` row is
  appended with `reverted_iteration: N`, (c) the next `build_planning_prompt`
  shows the revert. Drive measures with the existing echo-based fixture so the
  re-measure step runs without slow benches.

## Future (out of scope for v1)

- Auto-pick the latest fork for cwd inference (today: `config.task.name`, like
  `report`, with `--task` override).
- A general "reconcile" command that diffs the advancing-branch HEAD against the
  last recorded SHA on `resume` and warns / re-measures when they diverge —
  catches manual git surgery this command doesn't cover.
- `--sha` override / migration for legacy SHA-less tasks.
