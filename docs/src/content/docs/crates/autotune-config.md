---
title: autotune-config
description: Parses .autotune.toml into a validated AutotuneConfig and merges global agent defaults under it.
section: Crates
order: 5
---

`autotune-config` owns the configuration layer for Autotune. It deserializes a project's `.autotune.toml` into a strongly-typed `AutotuneConfig` (task, paths, tests, measures, score, agent) and validates it on load. A separate `GlobalConfig` reads agent defaults from `~/.config/autotune/config.toml`, which the binary merges underneath project settings (project wins, global fills gaps).

## When to use it

- Reach for this crate whenever you need to read, validate, or reason about a project's Autotune configuration.
- It is the single source of truth for the shape of `.autotune.toml` — every field a user can set lives here as a serde struct.
- It is a leaf crate, so you can depend on it (or test it) without pulling in agents, git, or state.
- Loading via `AutotuneConfig::load` runs full validation, so callers get a guaranteed-consistent config or a descriptive `ConfigError`.

## Public API

- `AutotuneConfig` — top-level config: `task`, `paths`, `test`, `measure`, `score`, `agent`.
- `AutotuneConfig::load(&Path)` — read a TOML file, deserialize, and validate; returns `ConfigError::NotFound` for a missing file.
- `AutotuneConfig::validate(&self)` — enforces all constraints (stop conditions, non-empty measures/commands, glob validity, metric-name uniqueness, score metric references); called automatically by `load`.
- `AutotuneConfig::task_dir(&self, root)` — resolves `<root>/.autotune/tasks/<name>/`.
- `TaskConfig` — `name`, `description`, `canonical_branch` (default `"main"`), and stop conditions: `max_iterations`, `target_improvement`, `max_duration`, `target_metric`.
- `StopValue` — `Finite(u64)` or `Infinite`, parsed from a string (`"inf"` for unbounded).
- `TargetMetric` — a `name`/`value`/`direction` metric threshold acting as a stop condition.
- `PathsConfig` — `tunable` and `denied` glob lists.
- `TestConfig` — `name`, `command`, `timeout` (default 300), `allow_test_edits`.
- `MeasureConfig` — `name`, optional `command`, `timeout` (default 600), `adaptor`.
- `AdaptorConfig` — tagged enum: `Regex { patterns }`, `Criterion { benchmarks }`, `Script { command }`, `Judge { persona, rubrics }`.
- `RegexPattern`, `CriterionBenchmark`, `CriterionStat` (`Mean`/`Median`/`StdDev`), `RubricConfig`, `ScoreRangeConfig` — adaptor sub-types.
- `ScoreConfig` — tagged enum: `WeightedSum { primary_metrics, guardrail_metrics }`, `Threshold { conditions }`, `Script { command }`, `Command { command }`.
- `PrimaryMetric`, `GuardrailMetric`, `ThresholdCondition` — scoring sub-types.
- `Direction` — `Minimize` / `Maximize`.
- `AgentConfig` — top-level agent defaults plus per-role overrides (`research`, `implementation`, `init`, `judge`); also `backend`, `model`, `max_turns`, `reasoning_effort`, `max_fix_attempts`, `max_fresh_spawns`.
- `AgentRoleConfig` — per-role settings with `overlay(&defaults)`, `effective_max_fix_attempts()` (default 10), and `effective_max_fresh_spawns()` (default 1).
- `ReasoningEffort` — `Low` / `Medium` / `High`.
- `global::GlobalConfig` — user-level agent defaults; `load()`, `load_from(&Path)`, `load_layered(&[&Path])`, `user_config_path()`.
- `ConfigError` — `NotFound`, `Parse`, `Validation`, `Io`.

## Usage

```rust
use std::path::Path;
use autotune_config::{AutotuneConfig, ScoreConfig, global::GlobalConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Loads and fully validates .autotune.toml.
    let config = AutotuneConfig::load(Path::new(".autotune.toml"))?;

    println!("tuning task: {}", config.task.name);
    println!("canonical branch: {}", config.task.canonical_branch);

    // Inspect the score strategy.
    if let ScoreConfig::WeightedSum { primary_metrics, .. } = &config.score {
        for pm in primary_metrics {
            println!("  metric {} ({:?}) weight {}", pm.name, pm.direction, pm.weight);
        }
    }

    // Global agent defaults fill gaps left by the project config.
    let global = GlobalConfig::load()?;
    if let Some(agent) = global.agent {
        if let Some(model) = agent.model {
            println!("global default model: {model}");
        }
    }

    // Resolve where this task's state lives.
    let task_dir = config.task_dir(Path::new("."));
    println!("task dir: {}", task_dir.display());
    Ok(())
}
```

## Internal dependencies

None — this is a leaf crate. It depends only on external crates (`serde`, `toml`, `thiserror`, `globset`, `dirs`).

## Notes

- `Direction` here is its own enum. The scoring crate defines separate `Direction` types (for weighted-sum and threshold scorers), so values must be mapped between them in the binary's `main.rs` — they are not interchangeable.
- `validate()` does more than check presence: it parses every tunable/denied glob, enforces metric-name uniqueness across measures, and verifies that every `weighted_sum`/`threshold` metric reference is actually produced by some adaptor. Script adaptors contribute no known metric names, so references to script-produced metrics can't be validated ahead of time.
- A backend mismatch is rejected: `max_turns` is invalid for the `codex` backend and `reasoning_effort` is invalid for the `claude` backend.
- A config with no stop condition fails validation — at least one of `max_iterations`, `target_improvement`, `max_duration`, or `target_metric` is required.
