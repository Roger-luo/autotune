---
title: autotune-state
description: Persistent crash-recoverable task state — phases, approach state, and an append-only iteration ledger on disk.
section: Crates
order: 13
---

`autotune-state` owns the on-disk representation of a tuning task: the current `Phase`, the in-flight `ApproachState`, and an append-only ledger of `IterationRecord`s. All persistence goes through `TaskStore`, which writes every file atomically (write-to-temp, fsync, rename, fsync the directory) so an interrupted run can be resumed without corrupting state.

## When to use it

- You need to read or write a task's durable state (`state.json`, `ledger.json`, config snapshot, research log, per-iteration artifacts).
- You're implementing crash recovery and need state that survives a kill at any point between phase transitions.
- It's a leaf crate with no internal dependencies, so you can model or test state handling in isolation from agents, git, or config.

## Public API

- `Phase` — the task lifecycle enum: `Planning`, `Implementing`, `Testing`, `Fixing`, `Measuring`, `Scoring`, `Integrating`, `Recorded`, `Done` (with a `Display` impl).
- `TaskState` — the current task snapshot: name, canonical/advancing branches, research session id and backend, iteration counter, current phase, and optional `current_approach`.
- `ApproachState` — the in-flight approach: hypothesis, worktree path, branch, commit sha, test results, metrics, rank, files to modify, and fix-retry bookkeeping (`fix_attempts`, `fresh_spawns`, `fix_history`, `impl_session_id`, `impl_backend`, `score_reason`).
- `TestResult` — one test's name, pass/fail, duration, and optional output.
- `IterationRecord` — a ledger row: iteration number, approach, `IterationStatus`, hypothesis, metrics, rank, score/reason, fix bookkeeping, and timestamp.
- `IterationStatus` — `Baseline`, `Kept`, `Discarded`, `Crash`.
- `Metrics` — alias for `HashMap<String, f64>`.
- `StateError` — error type covering `NotFound`, `InvalidTransition`, IO, and JSON failures.
- `TaskStore` — the persistence handle. Key methods: `new`/`open`/`root`; `save_state`/`load_state`; `load_ledger`/`append_ledger`; `save_config_snapshot`/`load_config_snapshot`; `read_log`/`append_log`; `iteration_dir`; `save_iteration_metrics`/`save_iteration_prompt`/`save_test_output`; `measure_output_dir`/`save_measure_output`; and the static `list_tasks`.

## Usage

```rust
use autotune_state::{IterationRecord, IterationStatus, Phase, TaskState, TaskStore};
use chrono::Utc;
use std::collections::HashMap;
use std::path::Path;

fn main() -> Result<(), autotune_state::StateError> {
    // Create (or open) the task's storage directory.
    let store = TaskStore::new(Path::new(".autotune/tasks/speedup"))?;

    // Persist the initial state.
    let state = TaskState {
        task_name: "speedup".to_string(),
        canonical_branch: "main".to_string(),
        advancing_branch: "autotune/speedup-main".to_string(),
        research_session_id: "sess-123".to_string(),
        research_backend: "claude".to_string(),
        current_iteration: 1,
        current_phase: Phase::Planning,
        current_approach: None,
    };
    store.save_state(&state)?;

    // Append a baseline measurement to the ledger.
    store.append_ledger(&IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: HashMap::from([("runtime_ms".to_string(), 120.0)]),
        rank: 0.0,
        score: None,
        reason: None,
        fix_attempts: 0,
        fresh_spawns: 0,
        timestamp: Utc::now(),
    })?;

    // Later (e.g. on `resume`): reload and inspect.
    let restored = store.load_state()?;
    println!("resuming in phase {}", restored.current_phase);
    Ok(())
}
```

## Internal dependencies

None — this is a leaf crate.

## Notes

Every write goes through an internal atomic helper: content is written to a `tempfile::NamedTempFile` in the destination directory, `sync_all`'d, the directory is fsync'd, then `persist`d (rename) and the directory fsync'd again. New fields on `ApproachState` and `IterationRecord` carry `#[serde(default)]`, so `state.json`/`ledger.json` files written by older versions (e.g. before the `Fixing` phase) still load.
