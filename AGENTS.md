# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Autotune is a Rust CLI that orchestrates autonomous, benchmark-driven performance tuning of codebases using LLM agents. The CLI owns the tune loop as an explicit crash-recoverable state machine — it spawns agents for research (persistent session) and implementation (ephemeral, sandboxed in worktrees) while maintaining deterministic control over testing, benchmarking, metric extraction, scoring, and git integration.

## Build & Test Commands

```bash
cargo build                              # Dev build
cargo test                               # All tests (107 across 12 crates)
cargo test -p autotune                   # Tests for binary crate only
cargo test -p autotune-config            # Tests for a specific crate
cargo test test_full_pipeline            # Single test by name substring
cargo clippy                             # Lint
cargo fmt                                # Format
```

## Pre-commit Checklist

**Run all three before committing** — CI checks these exact commands:

```bash
cargo fmt --all                                          # 1. Format
cargo clippy --all-targets --all-features -- -D warnings # 2. Lint (warnings are errors)
cargo test                                               # 3. Test
```

## Architecture

Cargo workspace with 12 crates under `crates/`. The binary crate (`autotune`) composes all library crates via a state machine.

### State Machine (crates/autotune/src/machine.rs)

The experiment lifecycle is a linear state machine with 8 phases. State is persisted to disk before every transition, enabling crash recovery via `autotune resume`.

```
Planning → Implementing → Testing → Benchmarking → Scoring → Integrating → Recorded → Planning (loop)
                                 ↘ Discarded ─────────────────────────────→ Recorded
                                                            ↗ Discarded ──→ Recorded
```

- **Planning**: Research agent (persistent session) proposes a hypothesis via `plan_next()`
- **Implementing**: Ephemeral agent writes code in a sandboxed worktree (no Bash, scoped Edit/Write)
- **Testing**: CLI runs configured test commands; failure → discard
- **Benchmarking**: CLI runs benchmarks, extracts metrics via adaptors
- **Scoring**: Score calculator produces rank + keep/discard decision
- **Integrating**: Cherry-pick kept commits onto canonical branch
- **Recorded**: Check stop conditions; loop or finish

`run_single_phase()` executes one transition (used by step commands). `run_experiment()` loops until Done or shutdown.

### Three Pluggable Trait Systems

**1. Agent trait** (`autotune-agent`): `spawn()` + `send()` for LLM interaction. `ClaudeAgent` shells out to `claude` CLI with session persistence. The trait is backend-agnostic — new backends implement `Agent`.

**2. MetricAdaptor trait** (`autotune-adaptor`): Extracts `HashMap<String, f64>` from benchmark output. Built-in: `RegexAdaptor`, `CriterionAdaptor`, `ScriptAdaptor`.

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
2. Config → `Agent`, `ScoreCalculator`, benchmark/test commands (wired in `main.rs`)
3. State machine drives the loop, persisting `ExperimentState` to `.autotune/experiments/<name>/state.json`
4. Results accumulate in `ledger.json` (append-only)
5. On exit, research agent session ID is printed for handover

### Experiment Storage (gitignored)

```
.autotune/experiments/<name>/
├── state.json              # current phase + approach state
├── config_snapshot.toml    # frozen config at experiment start
├── ledger.json             # append-only iteration records
├── log.md                  # research agent durable findings
└── iterations/
    └── 001-approach-name/
        ├── metrics.json    # benchmark results
        ├── prompt.md       # implementation agent prompt
        └── test_output.txt # saved on test failure
```

### ClaudeAgent Session Model

`ClaudeAgent` stores session contexts in a `Mutex<HashMap>`. When `send()` is called, it looks up the original `AgentConfig` (tools, working dir, model) from the session created by `spawn()`. Do not reconstruct a new `ClaudeAgent` between `spawn` and `send` — the session context would be lost.

## Key Conventions

- **Error handling:** `anyhow::Result` for application code, `thiserror` for library errors
- **Rust edition:** 2024
- **Atomic state writes:** All state persistence uses write-to-temp-then-rename (via `tempfile::NamedTempFile`)
- **Direction types:** `autotune_config::Direction`, `autotune_score::weighted_sum::Direction`, and `autotune_score::threshold::Direction` are separate enums that need mapping in `main.rs`

## Git Conventions

- **Conventional commits:** `feat:`, `fix:`, `docs:`, `test:`, `ci:`, `refactor:`, `perf:`, `build:`, `chore:`
- **Breaking changes:** Use `feat!:` or `fix!:` (note the `!`) or add a `BREAKING CHANGE:` footer
