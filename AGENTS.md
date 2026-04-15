# AGENTS.md

This file provides guidance when working with code in this repository.

## Project

Autotune is a Rust CLI that orchestrates autonomous, metric-driven tuning of codebases using LLM agents. The CLI owns the tune loop as an explicit crash-recoverable state machine — it spawns agents for research (persistent session) and implementation (ephemeral, sandboxed in worktrees) while maintaining deterministic control over testing, measurement, metric extraction, scoring, and git integration.

## Build & Test Commands

```bash
cargo build                              # Dev build
cargo nextest run                        # All tests (166 across 13 crates)
cargo nextest run --features mock        # Adds scenario tests (~184 total)
cargo nextest run -p autotune            # Tests for binary crate only
cargo nextest run -p autotune-config     # Tests for a specific crate
cargo nextest run -E 'test(full_pipeline)' # Single test by name substring
cargo clippy                             # Lint
cargo fmt                                # Format
```

## Pre-commit Checklist

**Run all three before committing** — CI checks these exact commands:

```bash
cargo fmt --all                                          # 1. Format
cargo clippy --all-targets --all-features -- -D warnings # 2. Lint (warnings are errors)
cargo nextest run                                        # 3. Test
```

## Architecture

Cargo workspace with 13 crates under `crates/`. The binary crate (`autotune`) composes all library crates via a state machine.

### State Machine (crates/autotune/src/machine.rs)

The task lifecycle is a linear state machine with 8 phases. State is persisted to disk before every transition, enabling crash recovery via `autotune resume`.

```
Planning → Implementing → Testing → Measuring → Scoring → Integrating → Recorded → Planning (loop)
                                 ↘ Discarded ─────────────────────────────→ Recorded
                                                            ↗ Discarded ──→ Recorded
```

- **Planning**: Research agent (persistent session) proposes a hypothesis via `plan_next()`
- **Implementing**: Ephemeral agent writes code in a sandboxed worktree (no Bash, scoped Edit/Write)
- **Testing**: CLI runs configured test commands; failure → discard
- **Measuring**: CLI runs task commands, extracts metrics via adaptors
- **Scoring**: Score calculator produces rank + keep/discard decision
- **Integrating**: Rebase kept commits onto the task's advancing branch (linear history, canonical untouched — see [notes/git-integration.md](notes/git-integration.md))
- **Recorded**: Check stop conditions; loop or finish

`run_single_phase()` executes one transition (used by step commands). `run_task()` loops until Done or shutdown.

### Three Pluggable Trait Systems

**1. Agent trait** (`autotune-agent`): `spawn()` + `send()` for LLM interaction. `ClaudeAgent` shells out to `claude` CLI with session persistence. The trait is backend-agnostic — new backends implement `Agent`.

**2. MetricAdaptor trait** (`autotune-adaptor`): Extracts `HashMap<String, f64>` from task output. Built-in: `RegexAdaptor`, `CriterionAdaptor`, `ScriptAdaptor`.

**3. ScoreCalculator trait** (`autotune-score`): Takes baseline/candidate/best metrics, returns `ScoreOutput { rank, decision, reason }`. Built-in: `WeightedSumScorer`, `ThresholdScorer`, `ScriptScorer`.

### Crate Dependency Graph

```
autotune (binary+lib)
├── autotune-plan       → autotune-agent, autotune-state
├── autotune-implement  → autotune-agent, autotune-git
├── autotune-init       → autotune-agent, autotune-config
├── autotune-test       → autotune-config
├── autotune-benchmark  → autotune-config, autotune-adaptor
├── autotune-config     (leaf)
├── autotune-state      (leaf)
├── autotune-agent      (leaf)
├── autotune-adaptor    (leaf)
├── autotune-score      (leaf)
├── autotune-git        (leaf)
└── autotune-mock       (dev-only, for testing)
```

Leaf crates have no internal workspace dependencies. This means you can work on `autotune-score` without touching git, agents, or config.

### Key Data Flow

1. `.autotune.toml` → `AutotuneConfig` (parsed by `autotune-config`)
2. Config → `Agent`, `ScoreCalculator`, task/test commands (wired in `main.rs`)
3. State machine drives the loop, persisting `TaskState` to `.autotune/tasks/<name>/state.json`
4. Results accumulate in `ledger.json` (append-only)
5. On exit, research agent session ID is printed for handover

### Task Storage (gitignored)

```
.autotune/tasks/<name>/
├── state.json              # current phase + approach state
├── config_snapshot.toml    # frozen config at task start
├── ledger.json             # append-only iteration records
├── log.md                  # research agent durable findings
└── iterations/
    └── 001-approach-name/
        ├── metrics.json    # task measurement results
        ├── prompt.md       # implementation agent prompt
        └── test_output.txt # saved on test failure
```

### ClaudeAgent Session Model

`ClaudeAgent` stores session contexts in a `Mutex<HashMap>`. When `send()` is called, it looks up the original `AgentConfig` (tools, working dir, model) from the session created by `spawn()`. Do not reconstruct a new `ClaudeAgent` between `spawn` and `send` — the session context would be lost.

`grant_session_permission(session, permission)` mutates the stored context to add a tool for subsequent `send` calls. Used when integration needs to temporarily grant the research agent `Edit` for conflict resolution.

Agent-to-CLI communication uses an XML fragment protocol (`<plan>`, `<task>`, `<measure>`, etc.), not JSON — see [notes/agent-protocol.md](notes/agent-protocol.md). MockAgent responses must be XML.

### Further reading

Detailed notes on non-obvious mechanics live in [notes/](notes/):

- [agent-subprocess.md](notes/agent-subprocess.md) — Claude CLI flags, why `--permission-mode dontAsk` and `--bare` don't work, how tool scoping is enforced.
- [agent-protocol.md](notes/agent-protocol.md) — XML fragment protocol, MockAgent format requirements.
- [git-integration.md](notes/git-integration.md) — Advancing branch, rebase integration, worktree branch namespacing.
- [config-and-tasks.md](notes/config-and-tasks.md) — Global vs project config merge, task auto-forking, how the implementation agent receives AGENTS.md/CLAUDE.md.

## Key Conventions

- **Error handling:** `anyhow::Result` for application code, `thiserror` for library errors
- **Rust edition:** 2024
- **No unsafe code:** Do not use `unsafe` blocks anywhere in the codebase. Use safe abstractions from crates like `nix` for Unix APIs, and `CommandExt::process_group()` instead of raw `libc` calls. For tests, prefer `ClaudeAgent::with_command()` over modifying environment variables (which requires `unsafe` in edition 2024).
- **Atomic state writes:** All state persistence uses write-to-temp-then-rename (via `tempfile::NamedTempFile`)
- **Direction types:** `autotune_config::Direction`, `autotune_score::weighted_sum::Direction`, and `autotune_score::threshold::Direction` are separate enums that need mapping in `main.rs`

### Terminal state restoration

Subprocesses (the Claude CLI) and interactive prompt libraries (`dialoguer`, `crossterm` raw mode) leave the terminal in non-default modes — Kitty keyboard protocol, bracketed paste, hidden cursor, mouse reporting. If not restored on exit, the user's shell is left typing garbage like `^[[99;5u` until they run `reset`.

All restoration logic is centralized in **`autotune_agent::terminal`**. Three overlapping layers guarantee cleanup on every exit path:

| Layer | Mechanism | Catches |
|---|---|---|
| 1. `terminal::Guard` | RAII; `Drop` calls `restore()` | Normal return, `?` error propagation, unwinding panics |
| 2. `terminal::install_panic_hook()` | Global panic hook runs `restore()` before the prior hook | Uncaught panics that escape all Guards |
| 3. Explicit `terminal::restore()` in signal handlers | Manual call before `std::process::exit()` | Direct exit paths (no Drop, no panic) |

**Rules for contributors:**

- **Call `install_panic_hook()` once early in `main()`.** Already wired in the binary crate — don't remove it.
- **Hold a `Guard` for any scope that may mutate terminal state.** That means: spawning the Claude CLI, calling `dialoguer::*::interact()`, enabling crossterm raw mode, or wrapping any third-party code that does the above.
  ```rust
  let _guard = autotune_agent::terminal::Guard::new();
  // ... interactive call ...
  // Guard's Drop restores on any scope exit
  ```
- **In signal handlers that `exit()`**, call `autotune_agent::terminal::restore()` explicitly — Drop won't run. (The init crate's `restore_terminal()` already wraps this plus crossterm-specific cleanup.)
- **Don't sprinkle terminal CSI sequences elsewhere.** If a new mode needs restoring, extend `terminal::restore()` so every site benefits.

Rust can't enforce "all terminal-touching code wears a Guard" at the type level (third-party APIs don't take our witness). The discipline is: the number of call sites is small, they're listed in `autotune_agent::terminal` module docs, and every new one should hold a Guard.

## Bug-fix & test-gap workflow

When you spot a bug or a behavior that isn't covered by tests, follow this order — **do not** skip to the fix. The discipline keeps regressions from reappearing and surfaces missing test infrastructure instead of working around it.

1. **Write a failing scenario test first.** Reproduce the bug end-to-end in `crates/autotune/tests/scenario_run_test.rs` (or the closest scenario file). Scenario tests exercise the full CLI via `assert_cmd` or the `scenario` crate's PTY harness against a `MockAgent`, driven by `AUTOTUNE_MOCK_RESEARCH_SCRIPT`. Running the test should fail for the exact reason the user's bug did.
2. **Fix the bug, then add unit tests alongside.** The scenario test pins the externally-visible behavior; unit tests in the affected crate pin the internal invariant so future refactors can't silently regress the same surface. Most fixes should grow at least one unit test in the crate that owns the logic.
3. **If scenario coverage is missing the knobs you need** — e.g. the mock can't emit the response shape, the harness can't simulate an input, the config doesn't support what you're testing — **implement a small ad-hoc version first** so the current fix can land with an end-to-end test. Keep the ad-hoc addition scoped (one new builder method, one env var, one fixture helper).
4. **Open an issue on [Roger-luo/Ion](https://github.com/Roger-luo/Ion/issues)** for the proper generalized version of any ad-hoc infrastructure you added, and for any follow-up scenario coverage you identified but didn't implement. Use `enhancement` for missing features, `bug` for broken behavior. Link the issue from a code comment if the ad-hoc path is load-bearing for the test.

**Why:** every real bug is a test gap. Fixing without a test lets the same bug recur; fixing with only a unit test misses integration-level regressions. And when scenario infrastructure is the bottleneck, we'd rather grow it deliberately than let tests rot because "the harness can't do that yet."

## Git Conventions

- **Conventional commits:** `feat:`, `fix:`, `docs:`, `test:`, `ci:`, `refactor:`, `perf:`, `build:`, `chore:`
- **Breaking changes:** Use `feat!:` or `fix!:` (note the `!`) or add a `BREAKING CHANGE:` footer
