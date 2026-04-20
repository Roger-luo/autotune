# Judge Adaptor Design

**Date:** 2026-04-20
**Status:** Approved

## Overview

Integrate `autotune-judge` into the autotune CLI as a built-in adaptor type `judge`. A judge measure spawns a single LLM agent session, sends all rubrics in one batched prompt under a shared persona, and emits one numeric metric per rubric ID. These metrics feed the existing scorer unchanged.

The primary use case: rubric-driven critique of code (or any artifact a command can capture) as a first-class autotune measurement, replacing ad-hoc LLM scoring scripts.

## Config Shape

```toml
[[measure]]
name = "critique"
command = ["sh", "-c", "find crates/kirin-interpreter-* -name '*.rs' | sort | xargs cat"]
# command is optional — if absent, subject context comes from iteration metadata only

[measure.adaptor]
type = "judge"
persona = "A strict Rust expert reviewing an interpreter framework design."

[[measure.adaptor.rubrics]]
id = "r1_completeness"
title = "Requirement completeness"
instruction = "Score whether all features are present. 5 = all present, 1 = core feature missing."
score_range = { min = 1, max = 5 }
guidance = "Check for concrete/abstract, SCF, cross-stage, forward/backward AI, sparse AI."  # optional

[[measure.adaptor.rubrics]]
id = "r2_api_symmetry"
title = "API symmetry"
instruction = "Score Lift/Project uniformity across cursors, values, effects, and environments. 5 = fully uniform, 1 = absent."
score_range = { min = 1, max = 5 }

# ... additional rubrics ...

[agent.judge]          # optional role; falls back to agent.model/backend if absent
model = "claude-sonnet-4-6"
backend = "claude"
```

**Scoring config** references rubric IDs as metric names:

```toml
[score]
type = "weighted_sum"
primary_metrics = [
  { name = "r1_completeness",     direction = "Maximize", weight = 5.0 },
  { name = "r2_api_symmetry",     direction = "Maximize", weight = 3.0 },
]
```

**Stop conditions** express convergence floors:

```toml
[[task.target_metric]]
name = "r1_completeness"
value = 4.0
direction = "Maximize"
```

## Data Flow

```
Measuring phase (machine.rs)
  │
  ├─ AdaptorConfig::Regex/Criterion/Script  →  run_measure_with_output() [unchanged]
  │
  └─ AdaptorConfig::Judge { persona, rubrics }
       ├─ [optional] run command → stdout/stderr as SubjectContext("command_output")
       ├─ inject iteration metadata as SubjectContext
       │    (approach name, iteration number, worktree path)
       ├─ Subject { title: measure.name, summary: approach.name, context }
       ├─ render_batch_prompt(persona, subject, rubrics)
       ├─ agent.spawn(prompt) → raw text
       ├─ parse_batch_response(rubrics, text) → Vec<(rubric_id, score)>
       └─ Metrics { "r1_completeness": 4.0, "r2_api_symmetry": 3.0, ... }

  All Metrics merged → Scoring phase [unchanged]
```

## Batch Protocol

Single agent call per judge measure. The prompt bundles all rubrics under the shared persona. The required response format — one blank-line-separated block per rubric, in declaration order:

```
r1_completeness
score: 4
reason: All concrete modes present but sparse AI untested.

r2_api_symmetry
score: 3
reason: Lift/Project applied to cursors only, not values or effects.
```

Parser rules:
- Split response on blank lines → one block per rubric
- Block line 1: rubric ID (must match a declared rubric exactly)
- Block line 2: `score: <int>` within the rubric's `score_range`
- Block line 3: `reason: <non-empty single line>`
- Unknown IDs, missing blocks, or out-of-range scores → parse error → `MeasureError::Extraction`

## Changes by Crate

### `autotune-config`

- `MeasureConfig.command: Option<Vec<String>>` — absent means no subprocess
- New `RubricConfig { id, title, persona_is_shared, instruction, score_range: ScoreRangeConfig, guidance: Option<String> }`
- New `ScoreRangeConfig { min: i32, max: i32 }` (mirrors `autotune_judge::ScoreRange`)
- `AdaptorConfig::Judge { persona: String, rubrics: Vec<RubricConfig> }` variant
- `AgentConfig.judge: Option<AgentRoleConfig>` — optional judge role
- `adaptor_metric_names` returns rubric IDs for judge adaptors
- Validation:
  - Judge adaptor must have ≥ 1 rubric
  - Rubric IDs must be unique within a measure
  - If command is `Some`, it must be non-empty
  - If any measure has a judge adaptor, `agent.judge` (or `agent.*` fallback) must resolve a backend

### `autotune-judge`

- `render_batch_prompt(persona: &str, subject: &Subject, rubrics: &[Rubric]) -> String`
  alongside the existing `render_assessment_prompt`
- `parse_batch_response(rubrics: &[Rubric], text: &str) -> Result<Vec<Assessment>, JudgeError>`
  alongside `parse_backend_text`
- No changes to existing `Judge` trait, `AgentJudge`, or single-rubric path

### `autotune-benchmark`

- `JudgeContext<'a> { agent: &'a dyn Agent, agent_config: AgentConfig }`
- `run_judge_measure(config: &MeasureConfig, working_dir: &Path, approach_name: &str, iteration: u32, ctx: &JudgeContext) -> Result<MeasureReport, MeasureError>`
  — runs optional command, builds subject, calls batch judge, returns metrics
- `run_all_measures_with_output(configs, working_dir, judge_ctx: Option<&JudgeContext>) -> Result<(Metrics, Vec<MeasureReport>), MeasureError>`
  — dispatches to `run_judge_measure` for judge adaptors, existing path for others

### `autotune` (binary)

- `agent_factory.rs`: `build_judge_agent_config(config, global) -> Option<(Box<dyn Agent>, AgentConfig)>`
  — returns `None` if no judge measures present (no agent built)
- `machine.rs` Measuring phase: builds `JudgeContext` once if needed, passes into `run_all_measures_with_output`

## Error Handling

| Case | Behavior |
|---|---|
| Batch response missing a rubric block | `MeasureError::Extraction` → iteration discarded |
| Score outside `score_range` | `MeasureError::Extraction` → iteration discarded |
| Judge agent call fails | `MeasureError::Extraction` wrapping `JudgeError::BackendCall` |
| `agent.judge` absent with judge adaptor | Config validation error at startup |
| Command `Some([])` with judge adaptor | Config validation error at startup |
| Command exits non-zero | `MeasureError::CommandFailed` (same as existing) |

## What Is Not Included (v1)

- **Review step** (`TerminalReviewPrompter`): skipped in automated loop; raw agent score used directly
- **Example store** (`JsonlExampleStore`): not wired; judge runs stateless each iteration
- **Parallel rubric calls**: all rubrics are batched in one agent call per measure
- **Backward compatibility**: `command` is `Option`; no empty-vec fallback shims

## Testing Plan

- **`autotune-judge`**: unit tests for `render_batch_prompt` snapshot; `parse_batch_response` happy path, missing rubric block, out-of-range score, unknown rubric ID
- **`autotune-benchmark`**: `run_judge_measure` with a `MockJudgeBackend`-backed mock agent; verify metric map keys and values
- **`autotune-config`**: validation tests for no rubrics, rubric ID collision, absent command with judge, absent agent backend with judge adaptor present
- **`autotune` scenario**: full iteration with a judge adaptor measure via `AUTOTUNE_MOCK_RESEARCH_SCRIPT`; verify metric keys appear in saved state
