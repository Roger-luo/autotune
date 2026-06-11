---
title: autotune
description: The binary and library crate that owns the crash-recoverable tune-loop state machine and composes every other crate.
section: Crates
order: 1
---

`autotune` is the workspace's top crate: both the `autotune` binary and a library that wires configuration into agents, scorers, test/measure commands, and git integration. It owns the tune loop as an explicit, crash-recoverable state machine, persisting `TaskState` to disk before every phase transition so a run can be resumed after a crash or `Ctrl+C`.

## When to use it

This is the entry point and orchestrator — touch it when you change CLI surface, the loop's control flow, or how the pieces fit together. `src/main.rs` parses commands and builds the agent/scorer/measure/test wiring from `AutotuneConfig`; `src/machine.rs` defines the phase transitions and the keep/discard logic. Leaf behavior (scoring math, metric extraction, git plumbing) lives in the sibling crates and is usually edited there. Reach for this crate when a phase needs to be added or reordered, or when a new subcommand or global-config-driven wiring decision is required.

## Command-line usage

```bash
# Full loop
autotune init [--name <name>]        # agent-assisted config, or normalize existing .autotune.toml
autotune run [--task <name>]         # start a fresh task (auto-forks if the name exists)
autotune resume --task <name> \      # continue a persisted task; flags override stop conditions
  [--max-iterations <n>] [--max-duration <dur>] [--target-improvement <f>]

# Inspecting tasks
autotune list                        # list all tasks with their phase
autotune report [--task <name>] [--format table|json]
autotune export --task <name> --output <file.json>

# Single-phase stepping (each requires the task be in the matching phase)
autotune plan      --task <name>     # Planning
autotune implement --task <name>     # Implementing
autotune test      --task <name>     # Testing
autotune measure   --task <name>     # Measuring
autotune record    --task <name>     # Scoring
autotune apply     --task <name>     # Integrating

# Finishing
autotune ff [--task <name>]          # fast-forward canonical to advancing branch, then clean up

# Global user config
autotune config get <key>
autotune config set <key> <value>
autotune config unset <key>
autotune config list
autotune config edit
```

## Key internals

- **`machine::run_task`** — Loops `run_single_phase` until `Done` or shutdown, persisting state on each iteration. Classifies phase errors into clean exit (interrupt/Ctrl+C), wait-and-retry (agent rate limit), or fatal.
- **`machine::run_single_phase`** — Executes exactly one phase transition; the building block behind every `step` subcommand. Returns `true` once the task reaches `Done`.
- **State machine phases** (`autotune_state::Phase`) — `Planning → Implementing → Testing → Measuring → Scoring → Integrating → Recorded`, looping back to `Planning`. A failed test routes through `Fixing` (bounded fix/respawn budget); exhausted budgets and discard decisions skip ahead. `Recorded` checks stop conditions and either loops or finishes at `Done`.
- **`cmd_run` / `cmd_resume`** — Set up a task: load and merge config, snapshot it, run sanity tests, collect baseline metrics, spawn (or hydrate) the persistent research-agent session, create the advancing branch (`autotune/<task>-main`), then drive `run_task`. `resume` re-hydrates the saved session and can override stop conditions.
- **`build_scorer`** — Maps `ScoreConfig` to a `ScoreCalculator` (`WeightedSumScorer`, `ThresholdScorer`, or `ScriptScorer`), translating the config-level `Direction` enum into the scorer-level ones.
- **`agent_factory`** — `resolve_backend_name` picks a backend per `AgentRole` (Research, Implementation, Init, Judge) honoring role overrides; `build_agent_for_backend` constructs the `claude` or `codex` agent.
- **`build_research_agent_prompt`** — Front-loads the research agent with the task goal, tunable/denied paths, the exact test/measure commands the CLI (not the agent) will run, the scoring rule, and the already-collected baseline.
- **`cmd_ff`** — Removes task worktrees, fast-forwards the canonical branch onto the advancing branch, and deletes the per-task and advancing branches.
- **`RunContext`** — Bundles the human-in-the-loop tool approver and the optional judge context (for LLM-based measure adaptors) passed through the loop.

## Internal dependencies

- **autotune-config** — Parses `.autotune.toml` and global user config into `AutotuneConfig`; source of `Direction`, `ScoreConfig`, and agent role settings.
- **autotune-state** — `TaskState`, `Phase`, `IterationRecord`, and `TaskStore` (the on-disk, crash-recoverable task store and ledger).
- **autotune-agent** — The `Agent` trait, session/handover model, tagged-output helpers (`aprintln!`/`aeprintln!`), terminal restoration, and tracing.
- **autotune-plan** — Research-agent permissions, tool-request handling, and the `ToolApprover` trait used during planning.
- **autotune-implement** — Ephemeral, sandboxed implementation agent plus the fix/respawn outcome types.
- **autotune-test** — Runs configured sanity/test suites and reports pass/fail.
- **autotune-benchmark** — Runs measure commands, builds metric adaptors, and supplies the `JudgeContext` for LLM-judged measures.
- **autotune-score** — `ScoreCalculator` implementations (weighted-sum, threshold, script) that produce the keep/discard decision.
- **autotune-git** — Repo-root discovery, branch/worktree management, and fast-forward merges for integration.
- **autotune-init** — Agent-assisted `init` flow and terminal input handling for generating a config.
- **autotune-mock** *(optional, `mock` feature / dev)* — Scriptable mock agent for scenario and unit tests.
