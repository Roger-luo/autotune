# AGENTS.md

This file provides guidance when working with code in this repository.

## Project

Autotune is a Rust CLI that orchestrates autonomous, metric-driven tuning of codebases using LLM agents. The CLI owns the tune loop as an explicit crash-recoverable state machine — it spawns agents for research (persistent session) and implementation (ephemeral, sandboxed in worktrees) while maintaining deterministic control over testing, measurement, metric extraction, scoring, and git integration.

## Build & Test Commands

```bash
cargo build                              # Dev build
cargo nextest run                        # All tests (109 across 12 crates)
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

Cargo workspace with 12 crates under `crates/`. The binary crate (`autotune`) composes all library crates via a state machine.

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
- **Integrating**: Cherry-pick kept commits onto canonical branch
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

## Git Conventions

- **Conventional commits:** `feat:`, `fix:`, `docs:`, `test:`, `ci:`, `refactor:`, `perf:`, `build:`, `chore:`
- **Breaking changes:** Use `feat!:` or `fix!:` (note the `!`) or add a `BREAKING CHANGE:` footer
