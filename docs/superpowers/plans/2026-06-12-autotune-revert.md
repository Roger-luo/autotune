# `autotune revert` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `autotune revert <iteration>` so a kept iteration can be undone *through* autotune (non-destructive `git revert` + re-measure), keeping the ledger, scoring, and planning records in sync.

**Architecture:** Per-iteration commit SHAs are recorded in the ledger. `revert` appends an inverse commit on the advancing branch, re-measures the branch, and appends a `Reverted` checkpoint row. Scoring's "best" and the planning prompt both read the ledger, so widening them to recognize `Reverted` rows is all that keeps them honest.

**Tech Stack:** Rust 2024 workspace; `clap` CLI; `anyhow`/`thiserror`; `cargo nextest`; `git` via `autotune-git`.

**Spec:** `docs/superpowers/specs/2026-06-12-autotune-revert-design.md`

**Conventions (from AGENTS.md):**
- Failing-test-first. Pre-commit checklist before every commit:
  `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run`
  (add `--features mock` when scenario tests are touched).
- Commit messages: conventional commits; end with
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Work on the current branch `feat/autotune-revert`.

---

## File Structure

**Modify:**
- `crates/autotune-state/src/lib.rs` — `IterationRecord` fields, `IterationStatus::Reverted`, tests.
- `crates/autotune-git/src/lib.rs` — `revert`, `revert_continue`, `revert_abort`, tests.
- `crates/autotune/src/machine.rs` — `build_kept_record` SHA arg + `run_integrating` capture; `run_scoring` best-selection; extract `resolve_conflicts` and reuse for rebase.
- `crates/autotune-plan/src/lib.rs` — render `Reverted` rows in the planning prompt.
- `crates/autotune/src/cli.rs` — `Revert` subcommand.
- `crates/autotune/src/main.rs` — `cmd_revert`, dispatch, tests.

**Create:**
- `crates/autotune/tests/scenario_revert_test.rs` — end-to-end revert scenario.

---

## Task 1: Ledger data model (`autotune-state`)

**Files:**
- Modify: `crates/autotune-state/src/lib.rs` (`IterationRecord`, `IterationStatus`)
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/autotune-state/src/lib.rs`:

```rust
/// New revert fields default to None so ledgers written before this feature
/// still deserialize. Pins backward compatibility.
#[test]
fn legacy_iteration_record_without_revert_fields_deserializes() {
    let legacy_json = r#"{
        "iteration": 1,
        "approach": "a",
        "status": "kept",
        "metrics": {},
        "rank": 0.0,
        "timestamp": "2026-04-15T00:00:00Z"
    }"#;
    let record: IterationRecord = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(record.commit_sha, None);
    assert_eq!(record.reverted_iteration, None);
}

/// The new `Reverted` status round-trips through serde in snake_case.
#[test]
fn reverted_status_roundtrips_snake_case() {
    let json = serde_json::to_string(&IterationStatus::Reverted).unwrap();
    assert_eq!(json, "\"reverted\"");
    let back: IterationStatus = serde_json::from_str("\"reverted\"").unwrap();
    assert_eq!(back, IterationStatus::Reverted);
}

/// A fully-populated reverted row round-trips, carrying the inverse-commit SHA
/// and the iteration it undid.
#[test]
fn reverted_record_roundtrips_with_fields() {
    let json = r#"{
        "iteration": 6,
        "approach": "revert of iteration 5",
        "status": "reverted",
        "metrics": {"x": 1.0},
        "rank": 0.0,
        "commit_sha": "deadbeef",
        "reverted_iteration": 5,
        "timestamp": "2026-04-15T00:00:00Z"
    }"#;
    let record: IterationRecord = serde_json::from_str(json).unwrap();
    assert_eq!(record.status, IterationStatus::Reverted);
    assert_eq!(record.commit_sha.as_deref(), Some("deadbeef"));
    assert_eq!(record.reverted_iteration, Some(5));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p autotune-state -E 'test(reverted) + test(legacy_iteration_record_without_revert)'`
Expected: FAIL to compile — `IterationStatus::Reverted` and the fields don't exist yet.

- [ ] **Step 3: Add the enum variant and fields**

In `crates/autotune-state/src/lib.rs`, add `Reverted` to `IterationStatus`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IterationStatus {
    Baseline,
    Kept,
    Discarded,
    Crash,
    Reverted,
}
```

In `IterationRecord`, add two fields just before `timestamp`:

```rust
    /// The commit this row corresponds to on the advancing branch:
    /// - `Kept` rows: the post-integration advancing-branch HEAD.
    /// - `Reverted` rows: the `git revert` (inverse) commit.
    ///
    /// `None` on rows written before SHA tracking existed (and on
    /// discarded/crash rows, which were never integrated).
    #[serde(default)]
    pub commit_sha: Option<String>,
    /// Set only on `Reverted` rows: the iteration number this revert undid.
    #[serde(default)]
    pub reverted_iteration: Option<usize>,
```

- [ ] **Step 4: Fix existing constructors of `IterationRecord`**

`IterationRecord` is constructed in a few places (`crates/autotune/src/machine.rs` `build_kept_record`, `build_discard`/`record_crash` helpers, the baseline append in `crates/autotune/src/main.rs`, and test fixtures in `scenario_run_test.rs`). Compiler errors will list each. Add `commit_sha: None, reverted_iteration: None` to every struct literal for now (Task 3 sets `commit_sha` for kept rows).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p autotune-state`
Expected: PASS (all, including the 3 new tests).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings
git add crates/autotune-state/src/lib.rs crates/autotune/src/machine.rs crates/autotune/src/main.rs crates/autotune/tests/scenario_run_test.rs
git commit -m "feat(state): add commit_sha + reverted_iteration ledger fields and Reverted status" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `git revert` helpers (`autotune-git`)

**Files:**
- Modify: `crates/autotune-git/src/lib.rs` (add `revert`, `revert_continue`, `revert_abort`)
- Test: same file's test module

Mirror the existing `rebase`/`rebase_continue`/`rebase_abort` (a conflict is detected via `has_merge_conflicts` and reported as `Ok(false)`; success is `Ok(true)`).

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/autotune-git/src/lib.rs` (mirror neighbours that build a temp repo — reuse the existing test helper that inits a repo with commits; if helpers differ, follow the pattern used by `latest_commit_sha_returns_nonempty`):

```rust
#[test]
fn revert_creates_inverse_commit() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path(), "file.txt", "original\n"); // existing helper
    // Commit a change we will revert.
    std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
    git(dir.path(), &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("change")]).unwrap();
    let target = latest_commit_sha(dir.path()).unwrap();

    let clean = revert(dir.path(), &target).unwrap();
    assert!(clean, "no-conflict revert should report clean");
    // File restored to original content; a new commit exists on top.
    assert_eq!(std::fs::read_to_string(dir.path().join("file.txt")).unwrap(), "original\n");
    assert_ne!(latest_commit_sha(dir.path()).unwrap(), target);
}

#[test]
fn revert_reports_conflict_without_erroring() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path(), "file.txt", "v1\n");
    std::fs::write(dir.path().join("file.txt"), "v2\n").unwrap();
    git(dir.path(), &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("v2")]).unwrap();
    let target = latest_commit_sha(dir.path()).unwrap();
    // A later commit touches the same line so reverting `target` conflicts.
    std::fs::write(dir.path().join("file.txt"), "v3\n").unwrap();
    git(dir.path(), &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("v3")]).unwrap();

    let clean = revert(dir.path(), &target).unwrap();
    assert!(!clean, "conflicting revert should report Ok(false), not Err");
    assert!(has_merge_conflicts(dir.path()).unwrap());

    revert_abort(dir.path()).unwrap();
    assert!(!has_merge_conflicts(dir.path()).unwrap());
}
```

> If no `init_repo_with_commit` helper exists, add a small one in the test module that runs `git init`, configures `user.email`/`user.name`, writes the file, and commits — mirror the setup in `crates/autotune/tests/scenario_run_test.rs::git_init`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p autotune-git -E 'test(revert)'`
Expected: FAIL to compile — `revert`/`revert_abort` don't exist.

- [ ] **Step 3: Implement the helpers**

Add near `rebase`/`rebase_continue`/`rebase_abort` in `crates/autotune-git/src/lib.rs`:

```rust
/// Append a commit that undoes `sha` (`git revert --no-edit <sha>`).
/// Returns `Ok(true)` if it applied cleanly, `Ok(false)` if it stopped on a
/// conflict (the caller resolves or calls [`revert_abort`]). A non-conflict
/// failure is returned as `Err` so the real git message is preserved.
pub fn revert(dir: &Path, sha: &str) -> Result<bool, GitError> {
    let result = git(
        dir,
        &[OsStr::new("revert"), OsStr::new("--no-edit"), OsStr::new(sha)],
    );
    match result {
        Ok(_) => Ok(true),
        Err(e) => {
            if has_merge_conflicts(dir)? {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Stage resolved files and continue an in-progress revert.
/// `Ok(true)` if it completed, `Ok(false)` if another conflict was hit.
pub fn revert_continue(dir: &Path) -> Result<bool, GitError> {
    // `git revert --continue` opens an editor by default; `GIT_EDITOR=true`
    // accepts the existing message non-interactively.
    let result = git_with_env(
        dir,
        &[OsStr::new("revert"), OsStr::new("--continue")],
        &[("GIT_EDITOR", "true")],
    );
    match result {
        Ok(_) => Ok(true),
        Err(e) => {
            if has_merge_conflicts(dir)? {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Abort an in-progress revert, restoring the pre-revert HEAD.
pub fn revert_abort(dir: &Path) -> Result<(), GitError> {
    git(dir, &[OsStr::new("revert"), OsStr::new("--abort")])?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p autotune-git -E 'test(revert)'`
Expected: PASS (2 new tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings
git add crates/autotune-git/src/lib.rs
git commit -m "feat(git): add revert/revert_continue/revert_abort helpers" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Capture the advancing-branch SHA into kept rows (`machine.rs`)

**Files:**
- Modify: `crates/autotune/src/machine.rs` (`build_kept_record`, `run_integrating`)
- Test: same file's test module

- [ ] **Step 1: Update the failing unit test for `build_kept_record`**

In `crates/autotune/src/machine.rs` tests, find `build_kept_record_preserves_score_reason`. Change its call to pass a SHA and assert it lands on the record. Add a focused assertion:

```rust
// build_kept_record now takes the post-integration advancing SHA.
let record = build_kept_record(2, &approach, Some("abc123advancing".to_string()));
assert_eq!(record.commit_sha.as_deref(), Some("abc123advancing"));
assert_eq!(record.reverted_iteration, None);
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p autotune -E 'test(build_kept_record)'`
Expected: FAIL to compile — `build_kept_record` takes 2 args.

- [ ] **Step 3: Add the SHA parameter**

Change `build_kept_record` in `crates/autotune/src/machine.rs`:

```rust
fn build_kept_record(
    iteration: usize,
    approach: &ApproachState,
    commit_sha: Option<String>,
) -> IterationRecord {
    IterationRecord {
        iteration,
        approach: approach.name.clone(),
        status: IterationStatus::Kept,
        hypothesis: Some(approach.hypothesis.clone()),
        metrics: approach.metrics.clone().unwrap_or_default(),
        rank: approach.rank.unwrap_or(0.0),
        score: Some("keep".to_string()),
        reason: approach.score_reason.clone(),
        fix_attempts: approach.fix_attempts,
        fresh_spawns: approach.fresh_spawns,
        commit_sha,
        reverted_iteration: None,
        timestamp: Utc::now(),
    }
}
```

- [ ] **Step 4: Capture the SHA in `run_integrating`**

In `run_integrating`, just after the fast-forward succeeds, capture the advancing-branch HEAD and pass it in:

```rust
    autotune_git::merge_ff_only(repo_root, &approach.branch_name)
        .context("fast-forward advancing branch failed")?;

    // The SHA now on the advancing branch — what `autotune revert` targets.
    // Equals approach.commit_sha in the no-conflict case; differs when a
    // conflict-rebase replayed the commit into a new SHA.
    let advancing_sha = autotune_git::latest_commit_sha(repo_root).ok();

    let metrics = approach.metrics.clone().unwrap_or_default();
    let _ = store.save_iteration_metrics(state.current_iteration, &approach.name, &metrics);

    let record = build_kept_record(state.current_iteration, approach, advancing_sha);
    store.append_ledger(&record)?;
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo nextest run -p autotune -E 'test(build_kept_record)'`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run
git add crates/autotune/src/machine.rs
git commit -m "feat(integrate): record post-integration advancing SHA on kept ledger rows" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Scoring best-selection recognizes `Reverted` (`machine.rs`)

**Files:**
- Modify: `crates/autotune/src/machine.rs` (`run_scoring` best-selection)
- Test: same file's test module (add a pure helper to make this unit-testable)

The current best-selection is inline in `run_scoring`. Extract it into a pure function so it can be tested directly with a fabricated ledger (including a middle-revert case).

- [ ] **Step 1: Write the failing test**

Add to the `machine.rs` tests module:

```rust
fn rec(iteration: usize, status: IterationStatus, metric: f64) -> IterationRecord {
    IterationRecord {
        iteration,
        approach: format!("a{iteration}"),
        status,
        hypothesis: None,
        metrics: std::collections::HashMap::from([("m".to_string(), metric)]),
        rank: 0.0,
        score: None,
        reason: None,
        fix_attempts: 0,
        fresh_spawns: 0,
        commit_sha: None,
        reverted_iteration: None,
        timestamp: Utc::now(),
    }
}

#[test]
fn best_metrics_uses_latest_kept_or_baseline() {
    let ledger = vec![
        rec(0, IterationStatus::Baseline, 100.0),
        rec(1, IterationStatus::Kept, 90.0),
        rec(2, IterationStatus::Discarded, 95.0),
    ];
    // Discarded is ignored; latest Kept (iter1) wins.
    assert_eq!(best_metrics_from_ledger(&ledger)["m"], 90.0);
}

#[test]
fn best_metrics_uses_revert_checkpoint_after_middle_revert() {
    // kept 1,2,3 then a Reverted checkpoint (re-measured) at row 4.
    let ledger = vec![
        rec(0, IterationStatus::Baseline, 100.0),
        rec(1, IterationStatus::Kept, 90.0),
        rec(2, IterationStatus::Kept, 80.0),
        rec(3, IterationStatus::Kept, 70.0),
        rec(4, IterationStatus::Reverted, 85.0), // post-revert re-measure
    ];
    // The Reverted checkpoint's fresh metrics are the new best — NOT iter3's
    // stale 70.0 which still included the reverted change.
    assert_eq!(best_metrics_from_ledger(&ledger)["m"], 85.0);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p autotune -E 'test(best_metrics)'`
Expected: FAIL to compile — `best_metrics_from_ledger` doesn't exist.

- [ ] **Step 3: Extract and widen the helper**

Add a pure function in `crates/autotune/src/machine.rs`:

```rust
/// The metrics that represent the advancing branch's current state, used as
/// scoring's `best`. Rows that reflect an integrated branch state count:
/// `Kept`, `Baseline`, and `Reverted` (post-revert re-measure). `Discarded`
/// and `Crash` rows never reached the branch and are ignored.
fn best_metrics_from_ledger(ledger: &[IterationRecord]) -> Metrics {
    ledger
        .iter()
        .rev()
        .find(|r| {
            matches!(
                r.status,
                IterationStatus::Kept | IterationStatus::Baseline | IterationStatus::Reverted
            )
        })
        .map(|r| r.metrics.clone())
        .unwrap_or_default()
}
```

Replace the inline `best_metrics` block in `run_scoring` with a call:

```rust
    let best_metrics = best_metrics_from_ledger(&ledger);
```

(Keep the existing `baseline_metrics` lookup as-is; `best_metrics_from_ledger` returns empty only when the ledger has no baseline/kept/reverted row, which matches the prior `unwrap_or_else(baseline)` fallback because baseline is always present.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p autotune -E 'test(best_metrics)'`
Expected: PASS (2 new tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run
git add crates/autotune/src/machine.rs
git commit -m "feat(score): include Reverted checkpoint rows in best-selection" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Planning prompt renders `Reverted` rows (`autotune-plan`)

**Files:**
- Modify: `crates/autotune-plan/src/lib.rs` (`build_planning_prompt` ledger-history loop)
- Test: same file's test module

- [ ] **Step 1: Write the failing test**

Add to the `autotune-plan` tests module (mirror `ledger_history_summarizes_multiline_approach`):

```rust
/// A Reverted row must tell the research agent the change was undone (which
/// iteration, and why) so it doesn't plan on top of a reverted optimization.
#[test]
fn ledger_history_marks_reverted_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();
    let rec: IterationRecord = serde_json::from_str(
        r#"{
            "iteration": 6,
            "approach": "revert of iteration 5",
            "status": "reverted",
            "metrics": {},
            "rank": 0.0,
            "reason": "broke truncation fidelity test",
            "reverted_iteration": 5,
            "timestamp": "2026-04-15T00:00:00Z"
        }"#,
    )
    .unwrap();
    store.append_ledger(&rec).unwrap();

    let prompt = build_planning_prompt(&store, None, 7, "task desc").unwrap();
    assert!(prompt.contains("reverted iteration 5"), "prompt:\n{prompt}");
    assert!(prompt.contains("broke truncation fidelity test"), "prompt:\n{prompt}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p autotune-plan -E 'test(ledger_history_marks_reverted)'`
Expected: FAIL — the generic line shows `approach=revert of iteration 5, status=Reverted` but not the `reverted iteration 5 (... reason)` phrasing.

- [ ] **Step 3: Special-case Reverted in the ledger-history loop**

In `build_planning_prompt`, replace the single `prompt.push_str(format!("- Iteration {}: approach=..."))` in the ledger loop with a branch on status:

```rust
        for record in &ledger {
            if record.status == IterationStatus::Reverted {
                let undid = record
                    .reverted_iteration
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let reason = record.reason.as_deref().unwrap_or("no reason given");
                prompt.push_str(&format!(
                    "- Iteration {}: reverted iteration {} ({}), status=Reverted\n",
                    record.iteration, undid, reason
                ));
            } else {
                prompt.push_str(&format!(
                    "- Iteration {}: approach={}, status={:?}, rank={}\n",
                    record.iteration,
                    summarize_approach(&record.approach),
                    record.status,
                    record.rank
                ));
            }
        }
```

Add `IterationStatus` to the `use autotune_state::{...}` import at the top of the file if not already present.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p autotune-plan -E 'test(ledger_history)'`
Expected: PASS (both ledger-history tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings
git add crates/autotune-plan/src/lib.rs
git commit -m "feat(plan): render Reverted rows so the agent sees undone iterations" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Reusable conflict-resolution loop (`machine.rs`)

`resolve_rebase_conflicts` hardcodes `rebase_continue`. `revert` needs the same agent-driven loop but with `revert_continue`. Extract the shared loop, parameterized by the "continue" operation, and reuse it for rebase.

**Files:**
- Modify: `crates/autotune/src/machine.rs` (`resolve_rebase_conflicts` → thin wrapper over new `resolve_conflicts`)

- [ ] **Step 1: Read the current `resolve_rebase_conflicts`**

Run: `sed -n '/fn resolve_rebase_conflicts/,/^}/p' crates/autotune/src/machine.rs`
Note: it loops up to `MAX_CONFLICT_ROUNDS`, sending the agent a conflict prompt, verifying via `has_merge_conflicts`, then calling `rebase_continue`.

- [ ] **Step 2: Extract `resolve_conflicts`**

Introduce a generic loop that takes the continue operation as a closure. Keep the exact prompt/round logic; only the continue call is parameterized:

```rust
/// Drive the research agent to resolve in-progress merge conflicts (from a
/// rebase or revert), calling `continue_op` after each agent turn. Returns
/// `Ok(())` when conflicts clear, `Err` if unresolved within MAX_CONFLICT_ROUNDS.
fn resolve_conflicts(
    agent: &dyn Agent,
    dir: &Path,
    research_session: &AgentSession,
    op_label: &str, // "rebase" or "revert", used only in the prompt/messages
    mut continue_op: impl FnMut(&Path) -> Result<bool, autotune_git::GitError>,
) -> Result<()> {
    // ... move the existing body of resolve_rebase_conflicts here, replacing
    // the `autotune_git::rebase_continue(dir)?` call with `continue_op(dir)?`,
    // and using `op_label` where the prompt/log mentions "rebase". ...
}

fn resolve_rebase_conflicts(
    agent: &dyn Agent,
    repo_root: &Path,
    research_session: &AgentSession,
) -> Result<()> {
    resolve_conflicts(agent, repo_root, research_session, "rebase", autotune_git::rebase_continue)
}
```

- [ ] **Step 3: Run the full suite to verify no regression**

Run: `cargo nextest run -p autotune`
Expected: PASS — existing integration/rebase-conflict tests still pass (behavior unchanged for rebase).

- [ ] **Step 4: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings
git add crates/autotune/src/machine.rs
git commit -m "refactor(machine): extract resolve_conflicts for rebase + revert reuse" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: `revert` command + CLI wiring (`cli.rs`, `main.rs`)

**Files:**
- Modify: `crates/autotune/src/cli.rs` (`Revert` subcommand)
- Modify: `crates/autotune/src/main.rs` (`cmd_revert`, dispatch arm)
- Test: `main.rs` tests (validation paths via fixtures)

`cmd_revert` mirrors `cmd_resume`'s setup (open store, load snapshot config, build agent + session, build scorer, ctrlc handler), then runs the revert flow. Make `build_kept_record`'s sibling `build_reverted_record` and the validation a pure helper so they're unit-testable without git.

- [ ] **Step 1: Add the CLI subcommand**

In `crates/autotune/src/cli.rs`, add to `enum Commands` (after `Resume`):

```rust
    /// Revert a kept iteration's change on the advancing branch
    Revert {
        /// Iteration number to revert
        iteration: usize,
        /// Task name (defaults to the task in .autotune.toml)
        #[arg(long)]
        task: Option<String>,
        /// Why it's being reverted (recorded + shown to the research agent)
        #[arg(long)]
        reason: Option<String>,
        /// Skip re-measuring the branch after the revert
        #[arg(long)]
        no_measure: bool,
    },
```

- [ ] **Step 2: Add the dispatch arm**

In `crates/autotune/src/main.rs`, in the `match cli.command` block (near the other `Commands::` arms):

```rust
        Commands::Revert {
            iteration,
            task,
            reason,
            no_measure,
        } => cmd_revert(iteration, task, reason, no_measure),
```

- [ ] **Step 3: Write the failing validation unit test**

Add a pure validation function and test it. In `main.rs` tests:

```rust
#[test]
fn validate_revert_target_rejects_non_kept_and_missing_sha() {
    use autotune_state::{IterationRecord, IterationStatus};
    let mk = |it, status, sha: Option<&str>, reverted: Option<usize>| IterationRecord {
        iteration: it, approach: format!("a{it}"), status, hypothesis: None,
        metrics: Default::default(), rank: 0.0, score: None, reason: None,
        fix_attempts: 0, fresh_spawns: 0, commit_sha: sha.map(String::from),
        reverted_iteration: reverted, timestamp: chrono::Utc::now(),
    };
    let ledger = vec![
        mk(0, IterationStatus::Baseline, None, None),
        mk(1, IterationStatus::Kept, Some("sha1"), None),
        mk(2, IterationStatus::Discarded, None, None),
        mk(3, IterationStatus::Kept, None, None),        // legacy, no SHA
        mk(4, IterationStatus::Reverted, Some("revsha"), Some(1)), // reverts iter1
    ];
    // Happy path: kept with SHA, not yet reverted.
    assert!(validate_revert_target(&ledger, 1).is_err()); // already reverted by row 4
    assert!(validate_revert_target(&ledger, 2).is_err()); // discarded
    assert!(validate_revert_target(&ledger, 3).is_err()); // no SHA (legacy)
    assert!(validate_revert_target(&ledger, 9).is_err()); // unknown
    // A kept, un-reverted, SHA-bearing row validates and returns its SHA.
    let ok = validate_revert_target(&[mk(1, IterationStatus::Kept, Some("sha1"), None)], 1);
    assert_eq!(ok.unwrap(), "sha1");
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo nextest run -p autotune -E 'test(validate_revert_target)'`
Expected: FAIL to compile — `validate_revert_target` doesn't exist.

- [ ] **Step 5: Implement `validate_revert_target`**

In `crates/autotune/src/main.rs`:

```rust
/// Validate that iteration `n` can be reverted; return its commit SHA.
fn validate_revert_target(
    ledger: &[IterationRecord],
    n: usize,
) -> Result<String> {
    let row = ledger
        .iter()
        .find(|r| r.iteration == n)
        .with_context(|| format!("no iteration {n} in ledger"))?;
    if row.status != IterationStatus::Kept {
        anyhow::bail!(
            "iteration {n} is {:?}, only kept iterations can be reverted",
            row.status
        );
    }
    if ledger
        .iter()
        .any(|r| r.status == IterationStatus::Reverted && r.reverted_iteration == Some(n))
    {
        let by = ledger
            .iter()
            .find(|r| r.status == IterationStatus::Reverted && r.reverted_iteration == Some(n))
            .map(|r| r.iteration)
            .unwrap();
        anyhow::bail!("iteration {n} was already reverted (see iteration {by})");
    }
    row.commit_sha.clone().with_context(|| {
        format!("iteration {n} predates SHA tracking; revert it with git and PR the branch")
    })
}
```

- [ ] **Step 6: Run to verify it passes**

Run: `cargo nextest run -p autotune -E 'test(validate_revert_target)'`
Expected: PASS.

- [ ] **Step 7: Implement `cmd_revert`**

In `crates/autotune/src/main.rs`. This mirrors `cmd_resume`'s store/config/agent/scorer setup; the novel part is the revert flow.

```rust
fn cmd_revert(
    iteration: usize,
    task: Option<String>,
    reason: Option<String>,
    no_measure: bool,
) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");

    // Resolve task: --task, else the config's task name (mirrors cmd_report).
    let task_name = match task {
        Some(t) => t,
        None => load_config(&repo_root)?.task.name,
    };
    let task_dir = autotune_dir.join("tasks").join(&task_name);
    let store = TaskStore::open(&task_dir)
        .with_context(|| format!("task '{task_name}' not found at {}", rel(&task_dir, &repo_root)))?;

    // Frozen config (measures, adaptors, advancing branch) — same as resume.
    let config_snapshot = store.load_config_snapshot().context("failed to load config snapshot")?;
    let config: AutotuneConfig =
        toml::from_str(&config_snapshot).context("failed to parse frozen config")?;

    let mut state = store.load_state().context("failed to load task state")?;

    // Clean-state guard: no in-progress approach.
    if state.current_approach.is_some() {
        anyhow::bail!(
            "iteration {} is in progress (phase {:?}); finish or discard it before reverting",
            state.current_iteration,
            state.current_phase
        );
    }

    // Validate target and get the SHA on the advancing branch.
    let ledger = store.load_ledger()?;
    let target_sha = validate_revert_target(&ledger, iteration)?;

    aprintln!("[autotune] reverting iteration {iteration} ({target_sha}) on '{}'", state.advancing_branch);

    // Ensure the advancing branch is checked out at the repo root.
    autotune_git::checkout(&repo_root, &state.advancing_branch)
        .context("failed to checkout advancing branch")?;

    // git revert; resolve conflicts via the research agent, else abort cleanly.
    let clean = autotune_git::revert(&repo_root, &target_sha)
        .context("git revert failed")?;
    if !clean {
        let agent = build_agent_for_backend(&state.research_backend)?;
        let research_session = autotune_agent::AgentSession {
            session_id: state.research_session_id.clone(),
            backend: state.research_backend.clone(),
        };
        agent.hydrate_session(&research_session, &research_agent_session_config(&config, &repo_root))?;
        if let Err(e) = machine::resolve_conflicts(
            agent.as_ref(),
            &repo_root,
            &research_session,
            "revert",
            autotune_git::revert_continue,
        ) {
            let _ = autotune_git::revert_abort(&repo_root);
            anyhow::bail!("revert conflict resolution failed: {e}; aborted, ledger unchanged");
        }
    }

    let revert_sha = autotune_git::latest_commit_sha(&repo_root)?;

    // Re-measure the branch (unless skipped) so scoring's best reflects reality.
    let new_index = ledger.iter().map(|r| r.iteration).max().unwrap_or(0) + 1;
    // The revert commit is already on the branch, so a re-measure failure must
    // NOT abort silently — record the row with empty metrics and warn (best
    // then falls back to the prior measured row; the next real iteration
    // re-establishes metrics). See spec "Edge cases & failure handling".
    let metrics = if no_measure {
        aprintln!("[autotune] --no-measure: recording revert without fresh metrics");
        Default::default()
    } else {
        match autotune_benchmark::run_all_measures_with_output(
            &config.measure,
            &repo_root,
            &format!("revert-{iteration}"),
            new_index as u32,
            None, // judge adaptors during revert re-measure are future work
        ) {
            Ok((metrics, _reports)) => metrics,
            Err(e) => {
                aeprintln!(
                    "[autotune] re-measure failed ({e}); recording the revert with empty \
                     metrics — scoring 'best' falls back to the prior measured row"
                );
                Default::default()
            }
        }
    };

    // Append the Reverted checkpoint row (counts toward budget like a discard;
    // its metrics feed best-selection).
    let record = build_reverted_record(new_index, iteration, revert_sha, metrics, reason);
    store.append_ledger(&record)?;

    state.current_iteration = new_index + 1;
    state.current_phase = autotune_state::Phase::Planning;
    store.save_state(&state)?;

    aprintln!("[autotune] reverted iteration {iteration}; recorded checkpoint iteration {new_index}");
    Ok(())
}

/// Build the `Reverted` checkpoint ledger row.
fn build_reverted_record(
    index: usize,
    reverted_iteration: usize,
    revert_sha: String,
    metrics: autotune_state::Metrics,
    reason: Option<String>,
) -> IterationRecord {
    IterationRecord {
        iteration: index,
        approach: format!("revert of iteration {reverted_iteration}"),
        status: IterationStatus::Reverted,
        hypothesis: None,
        metrics,
        rank: 0.0,
        score: None,
        reason,
        fix_attempts: 0,
        fresh_spawns: 0,
        commit_sha: Some(revert_sha),
        reverted_iteration: Some(reverted_iteration),
        timestamp: Utc::now(),
    }
}
```

> `resolve_conflicts` must be `pub(crate)` (or re-exported via `machine`) so `main.rs` can call it. Update its visibility in Task 6's function if needed. `autotune_git::checkout` already exists (used by `run_integrating`).

- [ ] **Step 8: Run the suite**

Run: `cargo nextest run -p autotune`
Expected: PASS (validation test + no regressions).

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run
git add crates/autotune/src/cli.rs crates/autotune/src/main.rs crates/autotune/src/machine.rs
git commit -m "feat(cli): add 'autotune revert <iteration>' command" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: End-to-end scenario test

**Files:**
- Create: `crates/autotune/tests/scenario_revert_test.rs`

Seed a task with a baseline + one kept iteration carrying a real commit SHA on a real advancing branch, then run `autotune revert 1` and assert the branch + ledger + planning prompt reflect the revert. Drive measures with an echo-based config (no slow benches).

- [ ] **Step 1: Write the scenario test**

Create `crates/autotune/tests/scenario_revert_test.rs`:

```rust
//! Scenario test for `autotune revert`. Requires `--features mock` to compile
//! the shared mock plumbing, though this test drives the compiled binary
//! directly and does not need a mock agent (no conflict path exercised here).
#![cfg(feature = "mock")]

use assert_cmd::Command;
use std::path::Path;
use std::process::Command as StdCommand;

const CONFIG_TOML: &str = r#"
[task]
name = "revert-task"
description = "revert scenario"
canonical_branch = "main"
max_iterations = "10"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "echo-bench"
command = ["sh", "-c", "echo 'metric_value: 42.0'"]
timeout = 10
adaptor = { type = "regex", patterns = [{ name = "metric_value", pattern = "metric_value: ([0-9.]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric_value", direction = "Minimize", weight = 1.0 }]
guardrail_metrics = []
"#;

fn git(dir: &Path, args: &[&str]) {
    StdCommand::new("git").args(args).current_dir(dir).output().unwrap();
}

#[test]
fn scenario_revert_undoes_iteration_and_records_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Project with a committed src file + config.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn v() -> i32 { 1 }\n").unwrap();
    std::fs::write(root.join(".autotune.toml"), CONFIG_TOML).unwrap();
    git(root, &["init"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial"]);
    git(root, &["branch", "-M", "main"]);

    // Advancing branch with one kept-iteration commit.
    git(root, &["checkout", "-b", "autotune/revert-task-main"]);
    std::fs::write(root.join("src/lib.rs"), "pub fn v() -> i32 { 2 }\n").unwrap();
    git(root, &["commit", "-am", "autotune: iteration 1 change"]);
    let kept_sha = String::from_utf8(
        StdCommand::new("git").args(["rev-parse", "HEAD"]).current_dir(root).output().unwrap().stdout,
    ).unwrap().trim().to_string();

    // Seed task state + ledger (baseline + kept iter1 carrying the SHA).
    let task_dir = root.join(".autotune/tasks/revert-task");
    std::fs::create_dir_all(task_dir.join("iterations")).unwrap();
    std::fs::write(task_dir.join("config_snapshot.toml"), CONFIG_TOML).unwrap();
    std::fs::write(
        task_dir.join("state.json"),
        r#"{"task_name":"revert-task","canonical_branch":"main","advancing_branch":"autotune/revert-task-main","research_session_id":"sid","research_backend":"claude","current_iteration":2,"current_phase":"planning","current_approach":null}"#,
    ).unwrap();
    let ledger = format!(
        r#"[{{"iteration":0,"approach":"baseline","status":"baseline","metrics":{{"metric_value":50.0}},"rank":0.0,"timestamp":"2026-04-15T00:00:00Z"}},
{{"iteration":1,"approach":"bump v","status":"kept","metrics":{{"metric_value":42.0}},"rank":0.1,"commit_sha":"{kept_sha}","timestamp":"2026-04-15T00:00:01Z"}}]"#
    );
    std::fs::write(task_dir.join("ledger.json"), ledger).unwrap();

    // Run the revert.
    let output = Command::cargo_bin("autotune").unwrap()
        .args(["revert", "1", "--task", "revert-task", "--reason", "test revert"])
        .current_dir(root)
        .output().unwrap();
    assert!(output.status.success(),
        "revert failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    // (a) An inverse commit is on the advancing branch (file restored to 1).
    assert_eq!(std::fs::read_to_string(root.join("src/lib.rs")).unwrap(), "pub fn v() -> i32 { 1 }\n");

    // (b) A Reverted checkpoint row was appended pointing at iteration 1.
    let ledger_after = std::fs::read_to_string(task_dir.join("ledger.json")).unwrap();
    assert!(ledger_after.contains("\"status\":\"reverted\""), "ledger:\n{ledger_after}");
    assert!(ledger_after.contains("\"reverted_iteration\":1"), "ledger:\n{ledger_after}");

    // (c) The recorded checkpoint carries fresh re-measured metrics (42.0 from echo).
    assert!(ledger_after.contains("\"metric_value\":42.0"));
}
```

- [ ] **Step 2: Run to verify it passes**

Run: `cargo nextest run --features mock -E 'test(scenario_revert)'`
Expected: PASS. (If it fails on JSON field ordering in assertions, relax to `serde_json`-parsed checks rather than substring matches.)

- [ ] **Step 3: Commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --features mock
git add crates/autotune/tests/scenario_revert_test.rs
git commit -m "test: end-to-end scenario for 'autotune revert'" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Docs + final verification

**Files:**
- Modify: `AGENTS.md` (state machine note: `Reverted` is an out-of-band ledger row), `notes/git-integration.md` (revert flow), `notes/scoring-and-rank.md` (resolve the "Recommendations" now that `revert` exists).

- [ ] **Step 1: Update notes**

In `notes/git-integration.md`, add a short "Reverting an iteration" subsection: `autotune revert <N>` appends a non-destructive inverse commit on the advancing branch, re-measures, and records a `Reverted` checkpoint row; conflicts reuse `resolve_conflicts`.

In `notes/scoring-and-rank.md`, update the "Footgun: manual edits diverge from the ledger" section: the supported path is now `autotune revert` (records the SHA, re-measures, keeps `best` honest); manual `git revert` is still unreconciled.

- [ ] **Step 2: Update `AGENTS.md` task-storage / state notes**

Note that the ledger can contain a `Reverted` row (status), that kept rows carry `commit_sha`, and that `autotune revert <iteration>` is the command that produces them.

- [ ] **Step 3: Full pre-commit checklist**

Run:
```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run --features mock
```
Expected: all green.

- [ ] **Step 4: Manual smoke test (optional, recommended)**

Build + install, then dry-run against a throwaway task:
```bash
cargo install --path crates/autotune --force
autotune revert --help   # confirm the subcommand + flags render
```

- [ ] **Step 5: Commit**

```bash
git add AGENTS.md notes/git-integration.md notes/scoring-and-rank.md
git commit -m "docs: document autotune revert (notes + AGENTS.md)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-review notes (for the implementer)

- **SHA accuracy:** kept rows store the *post-integration* advancing HEAD (Task 3), so `revert` targets a SHA guaranteed reachable from the advancing branch — robust even after a conflict-rebase renamed the worktree commit.
- **Budget vs best:** the `Reverted` row both counts toward `max_iterations` (it consumes a ledger index and bumps `current_iteration`, Task 7) *and* feeds best-selection (Task 4) — the two orthogonal behaviors the spec calls out.
- **Legacy rows:** `validate_revert_target` errors on a missing SHA, so SHA-less pre-feature tasks (e.g. `trotter-perf-3`) are handled by a clear message, not a crash.
- **Conflict reuse:** Task 6 generalizes the agent-driven loop so `revert` gets the same conflict resolution as rebase without duplicating it.
