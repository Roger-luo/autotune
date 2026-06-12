---
name: dogfooding-autotune
description: >-
  Use when hardening the autotune CLI by running it end-to-end against a real
  target repo and fixing whatever malfunctions — i.e. "try autotune on <project>
  and fix what breaks", "run autotune against X and watch for malfunctions", or
  any dogfooding/shakedown of the tune loop. Covers launching a traced
  unattended run, watching the trace/logs/ledger for malfunctions, diagnosing
  via trace `phase.decision` reasons, fixing in the autotune crates with a
  failing-test-first discipline, and rerunning (resume for speed) until N
  iterations complete cleanly. Not for plain unit changes — just run nextest.
allowed-tools: Bash, Read, Write, Edit
---

# Dogfooding autotune on a real project

## Overview

Autotune is best hardened by running it end-to-end against a real target
codebase and fixing whatever breaks. Mock/unit tests can't surface how the loop
behaves against a real project's tooling (git hooks, `mise`, slow benchmarks,
snapshot tests) or real LLM agents.

The loop is:

```
install binary under test → launch traced run → watch trace/logs/ledger
   → spot a malfunction → diagnose (trace phase.decision + worktree state)
   → fix in the autotune crates (failing test first) → reinstall
   → rerun (resume for speed) → repeat until ≥N iterations complete cleanly
```

A **malfunction** is the loop crashing (non-zero exit), hanging, orphaning
processes, or recording the wrong outcome. A clean `discard` is a *valid*
iteration outcome, not a malfunction.

## When to use

- The user says "try autotune on <project> and fix the problems", "run autotune
  in X until it can iterate N times", or asks to dogfood / shake out autotune.
- After changing tune-loop behavior, to confirm it still drives a real project.

Do **not** use for changes verifiable by `cargo nextest run` alone.

## Setup

1. **Pick/confirm a target repo** with a valid `.autotune.toml` (task, paths,
   `[[test]]`, `[[measure]]` + adaptor, `[score]`). Note its `max_iterations`.
2. **Build & install the binary under test** so `autotune` on PATH is *your*
   code — `~/.cargo/bin/autotune` is what the user runs:
   ```bash
   cargo install --path crates/autotune --force
   ```
   **Re-run this after every fix.** A stale binary is the #1 way to chase a bug
   you already fixed.
3. **Clean stale trace/log files** — `AUTOTUNE_TRACE_FILE` errors if the file
   already exists: `rm -f /tmp/at-trace.jsonl /tmp/at-run.log`.

## Launch a traced, unattended run

```bash
cd path/to/target-repo
AUTOTUNE_TRACE_FILE=/tmp/at-trace.jsonl AUTOTUNE_AUTO_APPROVE=1 \
  autotune run > /tmp/at-run.log 2>&1   # run in the background
```

- `AUTOTUNE_TRACE_FILE` → JSONL replay log. Categories to grep: `agent.spawn`,
  `agent.send`, `phase.enter`, **`phase.decision`** (branch + reason — the
  fastest "why"), `approval.prompt`/`approval.answer`, `implement.prompt`/
  `implement.result`, `worktree.setup`, `plan.attempt`/`plan.retry`. It only
  populates once an agent call *returns* (the research spawn explores for
  minutes before its first record — an empty trace early on is normal).
- `AUTOTUNE_AUTO_APPROVE=1` → grants the research agent's runtime tool requests
  without a prompt. Required unattended: with non-interactive stdin and no
  override, autotune auto-denies tool requests.
- Run it **in the background** and stop it yourself: the loop runs to
  `max_iterations` (often 30), far past the ≥N you need to validate.

## Watch for malfunctions

Poll periodically. Keep each `sleep` **under 120s** (the Bash tool's timeout);
benches are slow, so expect ~10 min/iteration (each worktree recompiles).

- **Ledger** — `.autotune/tasks/<task>/ledger.json`, one record per iteration
  (`baseline`/`kept`/`discarded`/`crash`). The ground truth for progress:
  ```bash
  python3 -c "import json;[print(r['iteration'],r['status']) for r in json.load(open('.autotune/tasks/TASK/ledger.json'))]"
  ```
- **Log milestones** (strip ANSI first: `sed 's/\x1b\[[0-9;]*[a-zA-Z]//g'`) —
  grep for `iteration N — (planning|implementing|testing|measuring|score:|recorded)`,
  `entering Fixing`, `commit rejected`, `rebase failed`, `discarding`, `stop:`,
  `panicked`.
- **Liveness** — `ps -eo etime,command | grep -E "trotter_scaling|cargo bench|claude -p"`
  to tell "slow but progressing" from "stalled". A research-agent `claude` at 0%
  CPU is normal (waiting on the API).
- **Trace `phase.decision`** — shows the branch taken and the reason string;
  read this first when a phase crashes/discards.

## Diagnose

- Pull the failing `phase.decision` reason from the trace.
- Inspect the candidate worktree's git state:
  `.autotune/tasks/<task>/worktrees/<approach>/` (e.g. `git status` to spot a
  dirty tree).
- Decide which crate owns the logic (config / git / agent / implement / machine
  / score).

## Fix (discipline — see AGENTS.md "Bug-fix & test-gap workflow")

1. **Write a failing test first** that reproduces the malfunction in the owning
   crate (unit test for internal logic; scenario test in
   `crates/autotune/tests/scenario_run_test.rs` for end-to-end behavior, driven
   by `MockAgent` + `AUTOTUNE_MOCK*`). Confirm it fails for the right reason —
   temporarily reverting the fix to watch it fail is worth the few seconds.
2. **Fix it**, then keep both unit + scenario coverage.
3. **Green the checklist**:
   `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run`
   (add `--features mock` to include scenario tests).
4. **Reinstall** and rerun.

## Rerun fast

- **Resume** a task that crashed/was-stopped mid-phase to skip baseline + the
  iterations already done — a big time saver since benches dominate:
  ```bash
  autotune resume --task TASK
  ```
- A fresh `autotune run` **auto-forks** the task name when state already exists
  (`trotter-perf` → `trotter-perf-2`); an incomplete dir with no `state.json` is
  cleaned and reused.
- **Stop cleanly** when done — orphan teardown is automatic on SIGINT/SIGTERM,
  but verify no `claude -p`/bench survives:
  ```bash
  pkill -xf "autotune run"; pkill -f "claude -p"; pkill -f "trotter_scaling --bench"
  ```

## Known malfunction signatures

Recognize recurrences fast — these have each been fixed once already:

| Symptom in log/trace | Root cause | Lives in |
|---|---|---|
| research agent spawns with the wrong model | global per-role vs general precedence | `main.rs` `apply_global_agent_defaults` |
| crash the instant the agent requests a tool (non-TTY) | `dialoguer` errors without a terminal | `stream_ui.rs` (use `AUTOTUNE_AUTO_APPROVE=1`) |
| every iteration `crash` at the implement commit; hook/license/`mise` errors | project pre-commit hooks reject the commit | hooks are the harness — fed back to the implementer; `[worktree] setup` (e.g. `["mise","trust"]`) preps the env |
| `rebase failed for an unexpected reason`; worktree shows a stray `.snap.new`/dirty file | a test run left the worktree dirty | `machine.rs` integration resets to HEAD before rebase |
| `.snap.new`/`.orig` committed into candidate commits | `git add -A` staged transient artifacts | `autotune-git` `stage_all_and_commit` excludes them |
| orphaned `claude` after killing autotune | child not in its own process group | `autotune-agent::child` (process groups + SIGTERM teardown) |
| `resume` behaves differently from `run` (e.g. fix budget) | snapshot stored the raw, not merged, config | `main.rs` snapshots the merged config |

See `notes/` (git-integration, agent-subprocess, config-and-tasks,
live-tail-rendering) for the detailed rationale behind each.

## Guidelines

- **Don't bypass the target's harness.** The project's git hooks define "valid
  code"; a rejected commit is fed back to the implementer (fix-retry → discard),
  never `--no-verify`'d away.
- **Make environment fixes configurable, not tool-specific** (e.g. `[worktree]
  setup` rather than hardcoding `mise`).
- **Budget for slowness.** Each iteration ≈ research + implement + tests + 2
  benches (each worktree recompiles). Poll; don't block on a single long sleep.
- **Real agents cost tokens.** Stop as soon as you've validated ≥N clean
  iterations — don't run to `max_iterations`.
- **Capture new findings.** If you fix a non-obvious malfunction, add a row to
  the signatures table above and a note in `notes/`.
