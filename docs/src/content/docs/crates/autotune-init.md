---
title: autotune-init
description: Agent-assisted initialization that explores a repo and writes a starter .autotune.toml via an XML-fragment conversation.
section: Crates
order: 8
---

`autotune-init` drives the agent-assisted `autotune init` flow: it spawns a read-only LLM agent that explores the repository, then collects and validates `[task]`, `[paths]`, `[measure]`, `[score]` (and optional `[test]`/`[agent]`) config fragments through an XML-fragment conversation, assembling a complete `AutotuneConfig`. It also centralizes terminal-state restoration so an interrupted init never leaves the user's shell in raw mode.

## When to use it

- Use it to implement the `init` subcommand: turn a user's plain-language goal plus a repository into a validated starter `.autotune.toml`.
- The init agent gets read-only tools only (`Read`, `Glob`, `Grep`) — it inspects code but never edits.
- Supply an optional `ConfigValidator` to run a trial measurement after the user approves the config, capturing baseline metrics (or surfacing extraction errors back to the agent for revision).
- For testing the flow, drive it with `MockInput` instead of `TerminalInput`.

## Public API

- `run_init(agent, global_config, repo_root, user_input, config_validator) -> Result<InitResult, InitError>` — runs the full init conversation, installing a Ctrl+C handler and restoring terminal state on every exit path.
- `InitResult` — `{ config: AutotuneConfig, baseline_metrics: Option<HashMap<String, f64>> }`; the assembled config plus baseline metrics when a validator ran successfully.
- `ConfigValidator<'a>` — type alias for `dyn Fn(&AutotuneConfig) -> Result<HashMap<String, f64>, String>`; validates an assembled config (typically by running its measure commands).
- `InitError` — error enum: `Agent`, `Config`, `UserAborted`, `ProtocolFailure { message }`, `Io`.
- `UserInput` — trait abstracting user interaction: `prompt_text`, `prompt_select`, `prompt_approve`.
- `TerminalInput` — interactive implementation; `new()`, `with_history(PathBuf)` (rustyline history + reverse search), uses dialoguer arrow-key menus on a TTY and falls back to line input when piped.
- `MockInput` — test implementation; `new(response)` returns a fixed response (and the first option key for selects).
- `build_init_prompt(repo_root) -> String` — builds the init agent's system prompt embedding the XML wire-protocol schema.

## Usage

```rust
use std::collections::HashMap;
use std::path::Path;

use autotune_init::{run_init, InitError, InitResult, TerminalInput};
use autotune_config::AutotuneConfig;
use autotune_config::global::GlobalConfig;
use autotune_agent::Agent;

fn init_project(
    agent: &dyn Agent,
    global_config: &GlobalConfig,
    repo_root: &Path,
) -> Result<(), InitError> {
    let user_input = TerminalInput::new();

    // Optional: validate the proposed config by running a trial measurement.
    let validator = |config: &AutotuneConfig| -> Result<HashMap<String, f64>, String> {
        // run config.measure commands, extract metrics, return them or an error string
        Ok(HashMap::new())
    };

    let InitResult { config, baseline_metrics } = run_init(
        agent,
        global_config,
        repo_root,
        &user_input,
        Some(&validator),
    )?;

    let toml = toml::to_string_pretty(&config).expect("serialize config");
    std::fs::write(repo_root.join(".autotune.toml"), toml)
        .map_err(|source| InitError::Io { source })?;

    println!("baseline: {baseline_metrics:?}");
    Ok(())
}
```

## Internal dependencies

- `autotune-agent` — `Agent` trait, streaming spawn/send, the XML fragment protocol parser, and `terminal` restoration helpers.
- `autotune-config` — `AutotuneConfig` and its section/adaptor/score types, plus `GlobalConfig`.

## Notes

- The agent communicates only via XML fragments (`<task>`, `<paths>`, `<measure>`, `<score>`, etc.), not JSON; `build_init_prompt` embeds the full schema and `run_init` enforces it, retrying once on malformed XML and capping the conversation at 50 turns before returning `ProtocolFailure`.
- A `judge` measure is staged as a pending measure: its rubrics are collected one at a time via `<rubric>` fragments (user Accept/Reject/Modify) and only finalized on `<rubrics-done>`, so the config stays incomplete until then.
- Validation is incremental — duplicate metric names across measures are rejected, score metrics must be produced by some measure adaptor (skipped when a `script` adaptor is present, since its metric names are only known at runtime), and a final `config.validate()` runs before returning.
