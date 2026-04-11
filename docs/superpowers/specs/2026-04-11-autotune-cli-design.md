# Autotune CLI Design Spec

## Overview

A Rust CLI that orchestrates autonomous, benchmark-driven performance tuning of codebases using LLM agents. The CLI owns the tune loop as an explicit state machine, spawning agents for research and implementation while maintaining deterministic control over testing, benchmarking, metric extraction, scoring, and git integration.

The CLI solves two key problems observed in agent-driven tuning:

1. Agents skip running tests after performance changes, introducing correctness bugs.
2. Agents use fragile `grep` to parse benchmark output instead of structured extraction.

By owning the loop, the CLI ensures tests always run, benchmarks are always parsed by deterministic adaptors, and experiment state survives crashes.

## Architecture

### Workspace Structure

```
autotune2/
├── Cargo.toml                      # workspace root
├── crates/
│   ├── autotune/                   # CLI binary + state machine orchestrator
│   │   └── src/
│   │       ├── main.rs             # clap CLI entry point
│   │       ├── cli.rs              # command definitions
│   │       ├── machine.rs          # state machine driver + transitions
│   │       └── resume.rs           # crash recovery logic
│   ├── autotune-config/            # config loading + validation
│   │   └── src/lib.rs
│   ├── autotune-state/             # experiment state persistence
│   │   └── src/lib.rs
│   ├── autotune-agent/             # agent trait + backend implementations
│   │   └── src/
│   │       ├── lib.rs              # Agent trait, AgentConfig, AgentSession
│   │       └── claude.rs           # ClaudeAgent backend (shells out to `claude` CLI)
│   ├── autotune-plan/              # research agent interaction (Planning phase)
│   │   └── src/lib.rs
│   ├── autotune-implement/         # implementation agent spawning (Implementing phase)
│   │   └── src/lib.rs
│   ├── autotune-test/              # test runner (Testing phase)
│   │   └── src/lib.rs
│   ├── autotune-benchmark/         # benchmark runner + metric extraction (Benchmarking phase)
│   │   └── src/lib.rs
│   ├── autotune-adaptor/           # metric adaptor framework
│   │   └── src/
│   │       ├── lib.rs              # Adaptor trait, AdaptorOutput type
│   │       ├── regex.rs            # built-in regex adaptor
│   │       ├── criterion.rs        # built-in criterion JSON reader
│   │       └── script.rs           # custom script adaptor
│   ├── autotune-score/             # score calculator adaptor framework
│   │   └── src/
│   │       ├── lib.rs              # ScoreCalculator trait, ScoreOutput type
│   │       ├── weighted_sum.rs     # built-in weighted sum scorer
│   │       ├── threshold.rs        # built-in threshold scorer
│   │       └── script.rs           # custom script/command scorer
│   └── autotune-git/               # git operations
│       └── src/lib.rs
```

Each state in the state machine maps to its own crate. Crates communicate through well-defined types (defined in `autotune-state` and `autotune-config`). The CLI binary in `autotune/` composes them.

### Crate Responsibilities

| Crate | Responsibility |
|---|---|
| `autotune` | CLI binary, state machine driver, signal handling, terminal output |
| `autotune-config` | Parse and validate `.autotune.toml`, `ConfigError` types |
| `autotune-state` | Read/write `state.json`, `ledger.json`, iteration records, phase transitions |
| `autotune-agent` | `Agent` trait, `AgentConfig`, `AgentSession`, backend implementations |
| `autotune-plan` | Interact with research agent to produce next hypothesis |
| `autotune-implement` | Spawn sandboxed implementation agent in worktree |
| `autotune-test` | Run configured test commands, report pass/fail |
| `autotune-benchmark` | Run benchmark commands, invoke adaptors, return raw metrics |
| `autotune-adaptor` | `Adaptor` trait, built-in adaptors (regex, criterion), custom script adaptor |
| `autotune-score` | Score calculator adaptor framework: built-in scorers (weighted_sum, threshold) + custom script/command |
| `autotune-git` | Worktree create/cleanup, branch, merge, cherry-pick, revert |

### Dependency Flow

```
autotune (binary)
├── autotune-config
├── autotune-state
├── autotune-agent
├── autotune-plan       → autotune-agent, autotune-state
├── autotune-implement  → autotune-agent, autotune-state, autotune-git
├── autotune-test       → autotune-config
├── autotune-benchmark  → autotune-adaptor
├── autotune-adaptor
├── autotune-score      → autotune-state
└── autotune-git
```

## State Machine

The experiment lifecycle is a linear state machine. Each state persists to `.autotune/experiments/<name>/state.json` before transitioning. Crash at any point and `autotune resume` re-enters the current state.

```
                    ┌─────────────────────────────────┐
                    │                                  │
                    ▼                                  │
              ┌──────────┐                             │
              │ Planning │ ── research agent picks     │
              └────┬─────┘    next hypothesis          │
                   │                                   │
                   ▼                                   │
           ┌──────────────┐                            │
           │ Implementing │ ── ephemeral agent in      │
           └──────┬───────┘    worktree                │
                  │                                    │
                  ▼                                    │
             ┌─────────┐                               │
             │ Testing  │ ── CLI runs test commands    │
             └────┬─────┘                              │
                  │                                    │
            ┌─────┴──────┐                             │
            │ pass  fail │                             │
            ▼            ▼                             │
    ┌──────────────┐  ┌───────────┐                    │
    │ Benchmarking │  │ Discarded │────────────────────┤
    └──────┬───────┘  └───────────┘                    │
           │                                           │
           ▼                                           │
       ┌─────────┐                                     │
       │ Scoring │ ── extract metrics, check           │
       └────┬────┘    guardrails, compute rank         │
            │                                          │
      ┌─────┴──────────┐                               │
      │ improved  regressed/guardrail_failed           │
      ▼                ▼                               │
  ┌────────┐     ┌───────────┐                         │
  │  Kept  │     │ Discarded │─────────────────────────┤
  └───┬────┘     └───────────┘                         │
      │                                                │
      ├── cherry-pick to canonical branch              │
      │                                                │
      ▼                                                │
  ┌──────────┐                                         │
  │ Recorded │ ── write ledger, check stop conditions  │
  └────┬─────┘                                         │
       │                                               │
       ├── stop condition met ──▶ Done                 │
       │                                               │
       └───────────────────────────────────────────────┘
```

Discarded iterations (from test failure or scoring regression) still go through `Recorded` — the attempt is written to the ledger with status `discarded` and the reason, then the loop continues to `Planning`.

### State Persistence

```rust
struct ExperimentState {
    experiment_name: String,
    canonical_branch: String,
    research_session_id: String,
    current_iteration: usize,
    current_phase: Phase,
    current_approach: Option<ApproachState>,
}

enum Phase {
    Planning,
    Implementing,
    Testing,
    Benchmarking,
    Scoring,
    Integrating,
    Recorded,
    Done,
}

struct ApproachState {
    name: String,
    hypothesis: String,
    worktree_path: PathBuf,
    branch_name: String,
    commit_sha: Option<String>,
    test_results: Vec<TestResult>,
    metrics: Option<HashMap<String, f64>>,
    rank: Option<f64>,
}
```

State writes use write-to-temp-then-rename for atomicity.

### Crash Recovery

| Crashed during | Resume behavior |
|---|---|
| Planning | Re-ask the research agent (idempotent — same session, re-send prompt) |
| Implementing | Check worktree for commits. If commit exists, proceed to Testing. If not, discard worktree, re-enter Planning. |
| Testing | Re-run tests in existing worktree. Deterministic, safe to retry. |
| Benchmarking | Re-run benchmarks. Deterministic, safe to retry. |
| Scoring | Re-compute from metrics already saved in iteration directory. Pure computation. |
| Integrating | Check git state: cherry-pick/revert already applied? If yes, skip to Recorded. If not, re-apply. |
| Recorded | Ledger already written. Check stop conditions and proceed. |

Each phase is idempotent or can detect "already done" from on-disk artifacts. Worst case is re-running a benchmark, never losing data.

## Agent Orchestration

### Agent Trait

The CLI does not hardcode any specific LLM provider. Agent interaction is behind a trait:

```rust
trait Agent {
    fn spawn(&self, config: AgentConfig) -> Result<AgentSession>;
    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse>;
}

struct AgentConfig {
    prompt: String,
    allowed_tools: Vec<ToolPermission>,
    tunable_paths: Vec<String>,
    output_format: OutputFormat,
    working_directory: PathBuf,
}

struct AgentSession {
    session_id: String,
    backend: String,
}
```

`ClaudeAgent` is the initial implementation (shells out to `claude` CLI). Other backends (Codex, etc.) can be added by implementing the trait.

### Three Agent Roles

| Agent | Lifecycle | Purpose | Backend Config |
|---|---|---|---|
| Init agent | One-shot, discarded after config is written | Help user write `.autotune.toml` via REPL | `[agent.init]` |
| Research agent | Per-experiment, persistent across iterations | Accumulate context, propose hypotheses | `[agent.research]` |
| Implementation agent | Per-iteration, ephemeral | Write code in sandboxed worktree | `[agent.implementation]` |

### Research Agent (persistent)

Spawned once at experiment start. Resumed each iteration via `agent.send()` with the previous iteration's results. Accumulates context across the experiment.

Each planning turn provides:
- Results from last iteration (approach, status, metrics, reason)
- Current experiment state (iterations completed, best rank, cumulative improvement)
- Contents of `log.md` (durable findings)

The research agent outputs structured JSON:
```json
{
  "approach": "short-kebab-name",
  "hypothesis": "what and why",
  "files_to_modify": ["src/foo.rs", "src/bar.rs"]
}
```

Permissions: full read access to the repo, no write access.

### Implementation Agent (ephemeral)

Fresh spawn per iteration, runs in a CLI-created worktree.

Sandboxed permissions:
- `Read`, `Glob`, `Grep` — unrestricted (needs to understand the codebase)
- `Edit`, `Write` — scoped to `tunable_paths` only
- No `Bash` — cannot run tests, benchmarks, or grep output
- No `Agent` — cannot spawn sub-agents
- No `WebFetch` — no external access

Must commit all changes before exiting. The CLI validates a commit exists in the worktree branch.

### Init Agent (one-shot)

Spawned by `autotune init` when no `.autotune.toml` exists. Provides a REPL-style interactive session where the user and agent collaboratively write the config. May involve profiling, codebase exploration, and discussion of which metrics to optimize.

The init agent session is discarded after the config is written — its context does not bleed into the research agent.

### Handover at Experiment End

When the loop stops (stop condition or Ctrl+C):
1. CLI prints experiment summary via `autotune report`
2. CLI prints the research agent session ID
3. CLI offers to open an interactive session (e.g., `claude -r <session-id>` for the Claude backend)
4. User drops into the research agent's full context and can continue manually

## Configuration

### `.autotune.toml`

```toml
[experiment]
name = "msd-sampling"
description = "Improve MSD sampling performance for 85-qubit circuits"
canonical_branch = "main"

# Stop conditions — at least one required. Use "inf" for explicit unbounded.
max_iterations = "inf"
# target_improvement = 0.5           # stop at 50% cumulative improvement
# max_duration = "4h"                # wall-clock limit

[paths]
tunable = [
    "crates/runtime/src/**",
    "crates/tableau/src/**",
]
# denied = ["secrets/**"]            # agent can't even read these

[[test]]
name = "rust"
command = ["cargo", "test"]
# timeout = 300                      # seconds

[[test]]
name = "python"
command = ["pytest", "tests/"]
# timeout = 120

[[benchmark]]
name = "msd_sampling"
command = ["cargo", "bench", "--bench", "msd_sampling"]
# timeout = 600
adaptor = { type = "regex", patterns = [
    { name = "time_us", pattern = 'time:\s+([0-9.]+)\s+µs' },
] }

[[benchmark]]
name = "fidelity"
command = ["python", "scripts/simulate.py"]
adaptor = { type = "script", command = ["python", "scripts/extract_fidelity.py"] }

[score]
type = "weighted_sum"

[[score.primary_metrics]]
name = "time_us"
direction = "Minimize"

[[score.guardrail_metrics]]
name = "correctness_rate"
direction = "Maximize"
max_regression = 0.01

# Or use a custom script:
# [score]
# type = "script"
# command = ["python", "scripts/judge_quality.py"]

[agent]
backend = "claude"

[agent.research]
backend = "claude"
# model = "opus"

[agent.implementation]
backend = "claude"
# model = "sonnet"
# max_turns = 50
```

### Validation Rules

- At least one stop condition must be set (`max_iterations`, `target_improvement`, or `max_duration`). Error if none are set.
- `[score]` section is required. `score.type` must be one of `"weighted_sum"`, `"threshold"`, `"script"`, or `"command"`.
- For built-in score types (`weighted_sum`, `threshold`): every referenced metric name must be producible by at least one benchmark's adaptor.
- `tunable` paths must be valid globs.
- Each `test` entry must have a non-empty `command`.
- Each `benchmark` entry must have a non-empty `command` and a valid `adaptor` section.
- `adaptor.type` must be `"regex"`, `"criterion"`, or `"script"`. Script adaptors must have a `command` field.
- Metric names must be unique across all benchmarks (no collisions).
- Baseline run validates the full pipeline (metric extraction → score calculation) before the tune loop begins.

## Metric Adaptors

All adaptors implement the same contract: take benchmark output, produce `HashMap<String, f64>`.

### Built-in: Regex

```toml
[[benchmark]]
name = "msd_sampling"
command = ["cargo", "bench", "--bench", "msd_sampling"]
adaptor = { type = "regex", patterns = [
    { name = "time_us", pattern = 'time:\s+([0-9.]+)\s+µs' },
] }
```

Extracts named metrics from benchmark stdout/stderr using regex capture groups.

### Built-in: Criterion

```toml
[[benchmark]]
name = "msd_sampling"
command = ["cargo", "bench", "--bench", "msd_sampling"]
adaptor = { type = "criterion", benchmark_name = "msd_sampling" }
```

Reads Criterion's `estimates.json` from `target/criterion/<benchmark_name>/new/estimates.json`.

### Custom Script

```toml
[[benchmark]]
name = "fidelity"
command = ["python", "scripts/simulate.py"]
adaptor = { type = "script", command = ["python", "scripts/extract_fidelity.py"] }
```

Contract:
- **stdin:** benchmark command's stdout and stderr
- **stdout:** JSON object of metric name to numeric value, e.g., `{"fidelity": 0.97}`
- **exit 0:** extraction succeeded
- **exit non-zero:** extraction failed, iteration discarded

## Scoring

Scoring follows the same adaptor pattern as metric extraction: built-in implementations for common cases, custom scripts or commands for anything exotic. The score calculator takes the full set of extracted metrics (candidate + baseline) and produces a **judgment** — the single indicator used to decide keep/discard.

### Score Calculator Adaptor

All score calculators implement the same contract:

- **Input:** JSON on stdin:
  ```json
  {
    "baseline": {"time_us": 180.76, "fidelity": 0.95},
    "candidate": {"time_us": 149.83, "fidelity": 0.96},
    "best": {"time_us": 155.0, "fidelity": 0.96}
  }
  ```
- **Output:** JSON on stdout:
  ```json
  {
    "rank": 0.171,
    "decision": "keep",
    "reason": "+17.1% improvement on time_us, fidelity stable"
  }
  ```
  - `rank`: numeric indicator (higher = better), stored in the ledger
  - `decision`: `"keep"` | `"discard"` | `"neutral"` (neutral defers to a threshold in config)
  - `reason`: human-readable explanation for the report

### Built-in: Weighted Sum

The default scorer. Config-driven, no script needed:

```toml
[score]
type = "weighted_sum"

[[score.primary_metrics]]
name = "time_us"
direction = "Minimize"
# weight = 1.0                         # default

[[score.guardrail_metrics]]
name = "correctness_rate"
direction = "Maximize"
max_regression = 0.01
```

Computes rank as weighted sum of per-metric improvements:
- **Maximize:** `improvement_i = (candidate_i - best_i) / best_i`
- **Minimize:** `improvement_i = (best_i - candidate_i) / best_i`
- `rank = sum(weight_i * improvement_i)`

Decision: `keep` if rank > 0 and all guardrails pass; `discard` otherwise.

### Built-in: Threshold

Simple pass/fail on individual metrics:

```toml
[score]
type = "threshold"

[[score.conditions]]
metric = "binary_size_kb"
direction = "Minimize"
threshold = 0.0                        # any reduction counts
```

### Custom Script

```toml
[score]
type = "script"
command = ["python", "scripts/judge_quality.py"]
```

Same stdin/stdout contract as above. The script can implement arbitrary logic — multi-criteria optimization, Pareto ranking, statistical significance tests, etc.

### Custom Command (shell one-liner)

```toml
[score]
type = "command"
command = ["sh", "-c", "jq '{rank: (.candidate.fidelity - .baseline.fidelity), decision: (if .candidate.fidelity > .baseline.fidelity then \"keep\" else \"discard\" end), reason: \"fidelity check\"}' < /dev/stdin"]
```

### LLM-as-Judge

```toml
[score]
type = "script"
command = ["python", "scripts/llm_judge.py"]
```

The script calls an LLM API with the metrics, gets back a judgment. Same contract — the CLI doesn't care that an LLM is involved. The script is responsible for prompt construction and response parsing.

### Score in Reports

The human-readable percentage displayed in reports (`score` column) is derived from the rank values in the ledger: `score = (rank - baseline_rank) / |baseline_rank|`. This is computed at display time, not by the score calculator.

### Baseline Validation

The first iteration (iteration 0) runs the **full pipeline end-to-end** — metrics extraction through score calculation — with no special-casing. This validates that:

1. Benchmark commands produce output the metric adaptors can parse.
2. Metric adaptor output matches the shape the score calculator expects.
3. The score calculator produces a valid judgment.

If any step fails at baseline, the experiment aborts with a clear error before any agent work begins. This is the pipeline integration test.

## Experiment Storage

```
.autotune/                                  # gitignored
├── experiments/
│   ├── msd-sampling/
│   │   ├── state.json                      # live state machine state
│   │   ├── config_snapshot.toml            # frozen config at experiment start
│   │   ├── ledger.json                     # append-only iteration records
│   │   ├── log.md                          # research agent's durable findings
│   │   ├── cli.log                         # timestamped CLI action log
│   │   └── iterations/
│   │       ├── 000-baseline/
│   │       │   └── metrics.json
│   │       ├── 001-precompute-phase/
│   │       │   ├── metrics.json
│   │       │   ├── prompt.md               # implementation agent prompt
│   │       │   └── test_output.txt         # only saved on failure
│   │       └── 002-simd-ops/
│   │           ├── metrics.json
│   │           └── prompt.md
│   └── gate-fidelity/
│       ├── state.json
│       ├── config_snapshot.toml
│       ├── ledger.json
│       ├── log.md
│       ├── cli.log
│       └── iterations/
```

**`state.json`** — Written atomically before every state transition. What `autotune resume` reads.

**`config_snapshot.toml`** — Frozen at experiment start. The CLI uses this (not the live `.autotune.toml`) during the run.

**`ledger.json`** — Append-only iteration records:
```json
[
  {
    "iteration": 0,
    "approach": "baseline",
    "status": "baseline",
    "metrics": {"time_us": 180.76},
    "rank": 1.0,
    "timestamp": "2026-04-11T14:30:00Z"
  },
  {
    "iteration": 1,
    "approach": "precompute-phase",
    "status": "kept",
    "hypothesis": "precompute bitmask for odd-phase destabilizers",
    "metrics": {"time_us": 149.83},
    "rank": 1.171,
    "score": "+17.1%",
    "timestamp": "2026-04-11T14:42:00Z"
  }
]
```

**`log.md`** — Research agent appends durable findings. Read at the start of each planning turn.

**Per-iteration directories** — Raw artifacts for debugging. `prompt.md` records the exact implementation agent prompt (reproducibility). `test_output.txt` saved on failure only.

## CLI Commands

```
autotune init [--name <name>]
    If .autotune.toml exists: validate config, create experiment, take baseline.
    If not: spawn init agent REPL to help write the config.

autotune run [--experiment <name>]
    Start a fresh experiment from .autotune.toml. Always creates a new experiment
    (appends suffix if name already exists). Fresh research agent session.
    --experiment overrides the name from config.

autotune resume --experiment <name> [--max-iterations N] [--max-duration T] [--target-improvement F]
    Resume an existing experiment from persisted state. Stop condition overrides
    are transient (not written to frozen config), apply to this session only.

autotune plan --experiment <name>
    Run just the Planning phase. Persists approach to state.json.

autotune implement --experiment <name>
    Run just the Implementing phase for the current approach.

autotune test --experiment <name>
    Run configured test commands in the current worktree.

autotune benchmark --experiment <name>
    Run benchmarks + metric extraction in the current worktree.

autotune record --experiment <name>
    Score current iteration, check guardrails, decide keep/discard.

autotune apply --experiment <name>
    Integrate (cherry-pick) or revert based on scoring decision.

autotune report --experiment <name> [--format json|table|chart]
    Show experiment progress. Default: terminal table + ASCII chart.

autotune list
    Show all experiments: name, status, iteration count, best score.

autotune export --experiment <name> --output <path>
    Dump experiment data for sharing.
```

**Command behavior:**
- `run` always starts fresh. `resume` always continues existing.
- Step commands (`plan`, `implement`, `test`, `benchmark`, `record`, `apply`) validate the experiment is in the correct phase before executing. Wrong phase is an error.
- `--experiment` is optional when exactly one experiment exists.
- Ctrl+C: first signal finishes persisting current state and exits gracefully. Second signal exits immediately (state was persisted at last transition).

## Startup Sequence (`autotune run`)

1. Load `.autotune.toml`, validate config.
2. Create `.autotune/experiments/<name>/`, snapshot config.
3. Spawn fresh research agent session, persist session ID to state.
4. Run all test commands on current codebase (sanity check — abort if tests fail before tuning).
5. Run all benchmark commands, extract metrics via adaptors.
6. Run metrics through scoring pipeline (no special-casing for baseline).
7. Record iteration 0 as `baseline` in ledger.
8. Feed baseline metrics to research agent as initial context.
9. Enter state machine at `Planning`.

## Error Handling

**Agent failures:**
- Research agent returns unparseable output: retry once with a corrective prompt, then abort the experiment.
- Implementation agent fails to commit: discard iteration, record as `crash` in ledger, continue to next Planning.
- Implementation agent times out: same as crash.

**Ctrl+C:**
- First Ctrl+C: finish persisting current state, clean up gracefully, print summary + research session ID.
- Second Ctrl+C: immediate exit.

## Terminal Output

During `autotune run`:
```
autotune · msd-sampling · iteration 3/inf

  Planning ····················· precompute-phase-mask
  Implementing ················· done (12 files changed)
  Testing ······················ rust (14s) python (8s)
  Benchmarking ················· time_us: 149.83
  Scoring ······················ +17.1% → kept

  ┌─────────────────────────────────────────────┐
  │ iteration  approach              score      │
  │ 0          baseline              ——         │
  │ 1          direct-bit-ops        -3.0%      │
  │ 2          precompute-phase      +17.1%     │
  │ 3          bulk-tableau-ops      +4.0%      │
  └─────────────────────────────────────────────┘

  cumulative: +21.1% · kept: 2/3 · elapsed: 38m
```

`autotune report`: terminal table + ASCII bar chart (default), JSON (`--format json`), or table only (`--format table`).

`autotune list`:
```
  experiment       status      iterations  best     cumulative
  msd-sampling     running     8/inf       +17.1%   +34.2%
  gate-fidelity    stopped     5/10        +8.3%    +12.1%
```

## Key Conventions

- **Error handling:** `anyhow::Result` for application code, `thiserror` for library error types.
- **Rust edition:** 2024.
- **Commits:** Conventional commits (`feat:`, `fix:`, `refactor:`, etc.).
- **State atomicity:** All state writes use write-to-temp-then-rename.
- **Logging:** All CLI actions logged to `cli.log`. Agent prompts and raw responses saved per-iteration.
