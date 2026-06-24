---
name: dogfooding-autotune
description: >-
  Use when hardening the autotune CLI by running it end-to-end against a real
  target repo — i.e. "try autotune on <project> and fix what breaks", "run
  autotune against X and watch for malfunctions", "can autotune actually improve
  X", or any dogfooding/shakedown of the tune loop. Dogfooding has two goals:
  (1) robustness — the loop doesn't crash, hang, orphan processes, fill the disk,
  or record the wrong outcome; and (2) efficacy — autotune produces meaningful,
  correct improvements (good hypotheses, real wins that clear the noise envelope,
  not gamed/overfit). Covers launching a traced unattended run, watching the
  trace/logs/ledger, diagnosing via trace `phase.decision`, fixing bugs OR
  improving autotune itself (research prompts, scoring, specialization) with a
  failing-test-first discipline, and rerunning until N iterations complete
  cleanly and the wins are judged real. Not for plain unit changes — just run
  nextest.
allowed-tools: Bash, Read, Write, Edit
---

# Dogfooding autotune on a real project

## Overview

Dogfooding hardens autotune two ways at once, and **both matter**:

1. **Robustness** — the loop doesn't malfunction (crash, hang, orphan
   processes, blow up the disk, record the wrong outcome) against a real
   project's tooling (git hooks, `mise`, slow benches, snapshot tests, real LLM
   agents). Mock/unit tests can't surface these.
2. **Efficacy** — autotune actually produces *meaningful, correct* improvements:
   the research agent forms high-value hypotheses, kept iterations clear the
   noise envelope, the cumulative gain is real (not noise, overfit, or a gamed
   metric), and correctness holds. A loop that runs forever without crashing but
   never finds a real win is **also** a failure — just a different one.

The loop is:

```
install binary under test → launch traced run → watch trace/logs/ledger
   → spot a malfunction OR a weak/false improvement → diagnose
   → FIX a bug, or IMPROVE autotune itself (research prompts, scoring,
     specialization) so hypotheses are more likely to pay off → reinstall
   → rerun (resume for speed) → repeat until ≥N iterations complete cleanly
     AND you've judged whether the wins are real
```

A **malfunction** is the loop crashing (non-zero exit), hanging, orphaning
processes, running the disk out of space, or recording the wrong outcome. A
clean `discard` is a *valid* iteration outcome, not a malfunction. But also watch
for **efficacy gaps** (below): low-value hypotheses, noise scored as signal, or
"wins" that don't reproduce — those call for *improving autotune*, not just
fixing a crash.

## When to use

- The user says "try autotune on <project> and fix the problems", "run autotune
  in X until it can iterate N times", or asks to dogfood / shake out autotune.
- After changing tune-loop behavior, to confirm it still drives a real project.
- To evaluate (and improve) whether autotune produces *meaningful* improvements
  on a real workload — "can autotune actually speed up X", "does the research
  agent form good hypotheses", "are the kept wins real".

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
4. **Check disk headroom first** — `df -h`. Rust `target/` dirs are large and
   accumulate; a run on a near-full disk dies mid-build with `No space left on
   device`. Autotune now shares **one** `CARGO_TARGET_DIR` per task (bounded to
   ~one project build) and aborts cleanly on `ENOSPC`, but the target repo's own
   `target/` + sibling projects can still leave you short — `cargo clean` inactive
   projects if tight. (Per-iteration target blowup was a real malfunction; see the
   signatures table.)
5. **If the user is actively working in the target repo**, run in a **dedicated
   git worktree** off a **dedicated base branch pinned at `origin/main`** (e.g.
   `git branch autotune-X-base origin/main`; set `canonical_branch` to it). Never
   operate on the user's primary checkout or branch (preserve their WIP).
   `canonical_branch = "main"` couples to the *local* `main` ref — which may be
   stale (missing a just-merged fixture) or checked out by the user; pin a base
   branch to decouple. Do all target-repo git ops in worktrees.

**For a performance target**, write the `.autotune.toml` to use the **criterion
adaptor** — that flips on autotune's perf specialization (profiler-first research
guidance + variance / cross-build-noise handling). Make off-target or co-located
metrics **guardrails** (not weighted objectives) so layout/measurement noise
can't distort the score, and set a *small* `baseline_replicates` for the
cross-build envelope (it's rebuild-expensive; the envelope is real but modest for
ms-scale workloads). If a bench needs a parser/higher-layer crate, it must live
where that crate is reachable (e.g. a STIM-driven bench goes in `ppvm-stim`, not
`ppvm-tableau`, because of the dependency direction) — not necessarily in the
crate being optimized.

## Launch a traced, unattended run

```bash
cd path/to/target-repo   # the dedicated worktree, if dogfooding a live repo
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
  `panicked`, `No space left`.
- **Liveness** — `ps -eo etime,command | grep -E "trotter_scaling|cargo bench|claude -p"`
  to tell "slow but progressing" from "stalled". A research-agent `claude` at 0%
  CPU is normal (waiting on the API).
- **Disk** — `df -h` periodically on a big project; bounded now, but confirm it.
- **Trace `phase.decision`** — shows the branch taken and the reason string;
  read this first when a phase crashes/discards.

## Watch for efficacy (not just malfunctions)

A clean-running loop isn't the goal — *meaningful, correct improvement* is. While
polling, also judge:

- **Hypothesis quality** (trace `plan.attempt` / `implement.prompt`). Is the
  research agent profiling and targeting the real hot path, or proposing
  low-value / scattershot changes? Weak hypotheses → improve the research/plan
  prompts (this session added profiler-first guidance), not a bug fix.
- **Are kept wins real?** A `kept` row should clear the **noise envelope** and
  reproduce — not be measurement noise scored as signal. Check the ledger's
  `score_breakdown` / `within_noise` flags and the cross-build envelope. Noise
  scored as signal → improve scoring (variance capture, guardrails, per-metric
  causal attribution all came from this).
- **Correctness, not just speed.** A "win" that changes results, weakens a test,
  or trips a guardrail isn't a win. Confirm tests + snapshots still pass and
  guardrail metrics didn't regress beyond their envelope.
- **Cumulative trajectory.** Is the advancing branch actually getting better
  iteration over iteration (e.g. `5.17 ms → 3.77 ms → 1.20 ms` on cultivation),
  or churning without net gain? `autotune analyze` emits the metric×iteration
  matrix + per-metric breakdowns for exactly this.
- **No bias toward perf.** Autotune is a *general* metric loop; perf-specific
  behavior (profiling guidance, replication) should activate only for benchmark
  metrics (the criterion adaptor), never for coverage/size/accuracy tasks.

When the loop runs fine but the *results* are weak or false, that's an efficacy
gap → **improve autotune** (next section), and capture it like any finding.

## Diagnose

- Pull the failing `phase.decision` reason from the trace.
- Inspect the candidate worktree's git state:
  `.autotune/tasks/<task>/worktrees/<approach>/` (e.g. `git status` to spot a
  dirty tree).
- Decide which crate owns the logic (config / git / agent / implement / machine
  / score / adaptor).

## Fix bugs — and improve efficacy (discipline — see AGENTS.md "Bug-fix & test-gap workflow")

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

**Improving efficacy** (when the loop runs but the wins are weak/false) follows
the same test-first discipline, but the target is autotune's *judgment*, not a
crash: reshape the research/plan prompts so hypotheses target the profiled hot
path; make scoring noise-aware (variance, cross-build envelope) so it can't bank
noise; add guardrails / per-metric causal attribution so off-target moves don't
distort the score; keep perf behavior gated to benchmark metrics. Pin each with a
scenario test (a MockAgent run asserting the new behavior) so it can't regress.

## Rerun fast

- **Resume** a task that crashed/was-stopped mid-phase to skip baseline + the
  iterations already done — a big time saver since benches dominate:
  ```bash
  autotune resume --task TASK
  ```
- A fresh `autotune run` **auto-forks** the task name when state already exists
  (`trotter-perf` → `trotter-perf-2`); an incomplete dir with no `state.json` is
  cleaned and reused. For a guaranteed-fresh run on the same name, clear the
  task's leftover worktrees/branches + `.autotune/tasks/<task>/` first.
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
| iteration dirs nested/garbled (`NNN-...rx/rzz` with `metrics.json` under a child dir) | `iteration_dir` used the raw approach name (often multi-line prose with `/`) instead of a slug | `autotune-state` `slug_component` sanitizes the approach into one path component |
| `iterations/<n>/prompt.md` missing though AGENTS.md documents it | `save_iteration_prompt` was never called | `machine.rs` `run_implementing` persists `autotune_implement::full_implementation_prompt` before the spawn |
| resumed iteration scored against / planned on top of a kept commit you reverted by hand | ledger `best`/planning echo diverge from the advancing branch after manual git ops | see `notes/scoring-and-rank.md` "manual edits diverge from the ledger" (not yet reconciled in code) |
| build dies `No space left on device`; exit 101; fix agent loops on the infra error | per-iteration worktrees each created a full `target/` → disk fills | **fixed**: shared `CARGO_TARGET_DIR` per task + clean `ENOSPC` abort — `machine.rs` `shared_target_dir` / `PhaseFailure::DiskFull`; `df` before runs |
| baseline/measure fails: `criterion estimates.json not found at <wt>/target/criterion/…` | adaptor read `<wt>/target` while a redirected `CARGO_TARGET_DIR` sent results elsewhere | **fixed**: adaptor resolves under the effective target dir — `autotune-benchmark` `effective_target_dir` |

See `notes/` (git-integration, agent-subprocess, config-and-tasks,
live-tail-rendering, scoring-and-rank) for the detailed rationale behind each.

## Guidelines

- **Don't bypass the target's harness.** The project's git hooks define "valid
  code"; a rejected commit is fed back to the implementer (fix-retry → discard),
  never `--no-verify`'d away. (Watch, too, for the implementer taking the easy
  out — e.g. adding `#[allow(...)]` to dodge a clippy hook instead of refactoring.)
- **Make environment fixes configurable, not tool-specific** (e.g. `[worktree]
  setup` rather than hardcoding `mise`).
- **Budget for slowness.** Each iteration ≈ research + implement + tests + 2
  benches (each worktree recompiles). Poll; don't block on a single long sleep.
- **Real agents cost tokens.** Stop as soon as you've validated ≥N clean
  iterations and judged the wins — don't run to `max_iterations`.
- **Check the disk first, and keep the target shared.** `df -h` before a run;
  autotune bounds itself to one shared `target/` per task, but a near-full disk
  still kills builds. `cargo clean` inactive projects if tight.
- **Dogfooding a live repo: stay in worktrees.** Use a dedicated worktree + a
  base branch pinned at `origin/main`; never touch the user's primary checkout
  or assume `canonical_branch = "main"` matches their local ref.
- **Landing a fixture in an active upstream races branch protection.** Rebase +
  merge fast, `--admin` an API-neutral change (if authorized), or run on the
  fixture branch directly. Re-verify the bench compiles after each rebase — an
  upstream parser/AST refactor can break it.
- **Expect to find bugs in whatever autotune change you just landed.** Dogfooding
  right after a change tends to surface bugs *in that change* (this session: the
  shared-target feature both fixed the disk blowup and introduced a criterion-path
  regression). That's the loop working.
- **Judge efficacy, not just survival.** "Ran N iterations without crashing" is
  necessary, not sufficient — confirm the wins are real, correct, and not noise,
  and improve autotune when they aren't.
- **Capture new findings.** If you fix a non-obvious malfunction, add a row to
  the signatures table above and a note in `notes/`. If you improve autotune's
  efficacy (prompts/scoring/specialization), capture the rationale in `notes/`.
