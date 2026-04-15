## Goal

Generalize agent configuration so backend-specific settings can be expressed directly in `[agent]` and `[agent.<role>]` tables, with top-level agent settings acting as defaults for each role when the role-specific table omits a field.

## Problem

The current config abstraction is Claude-shaped. It exposes `model` and `max_turns` as generic role settings, then reuses `max_turns` as a Codex-specific proxy for reasoning effort. That creates a semantic mismatch in both the config file and runtime agent abstraction:

- users cannot write Codex-native settings like `reasoning_effort`
- top-level `[agent]` only provides a backend default, not full per-field defaults
- backend-specific validation is weak because the schema does not distinguish common keys from backend-specific keys

## Requirements

- Allow backend-specific keys directly inside `[agent]` and `[agent.<role>]`.
- Treat `[agent]` as the default source for agent settings when a role table does not specify a field.
- Preserve existing common keys such as `backend` and `model`.
- Support Codex-native `reasoning_effort` without overloading `max_turns`.
- Reject invalid backend/key combinations at config load with clear error messages.
- Update config inspection and mutation surfaces to understand the new keys.

## Proposed Config Shape

Example:

```toml
[agent]
backend = "codex"
model = "gpt-5.4"
reasoning_effort = "medium"

[agent.research]
reasoning_effort = "high"

[agent.implementation]
reasoning_effort = "low"
max_fix_attempts = 10

[agent.init]
reasoning_effort = "medium"
```

The schema remains flat. There is no backend-specific nested subtable. A role's effective configuration is computed by taking `[agent]` as defaults and then overlaying `[agent.<role>]`.

## Config Model

`autotune-config` should evolve from a minimal common-role struct into a role struct with:

- common fields used by multiple backends:
  - `backend`
  - `model`
  - `max_fix_attempts`
  - `max_fresh_spawns`
- Claude-specific fields:
  - `max_turns`
- Codex-specific fields:
  - `reasoning_effort`

`reasoning_effort` should be a typed enum rather than a raw string so invalid values fail early. The allowed values should match the Codex CLI values the repository intends to support, such as `low`, `medium`, and `high` if those are the chosen supported levels.

The top-level `[agent]` table should be treated as an `AgentRoleConfig` default template plus role override slots, instead of only carrying a separate backend string. This keeps inheritance rules uniform.

## Validation Rules

Validation should become backend-aware after inheritance is applied:

- if the effective backend is `claude`, allow Claude-compatible keys and reject Codex-only keys such as `reasoning_effort`
- if the effective backend is `codex`, allow Codex-compatible keys and reject Claude-only keys such as `max_turns`
- if the backend is unknown, return the existing unsupported-backend error

Errors should name the specific field and the effective backend, for example: `agent.research.reasoning_effort is not valid for backend 'claude'`.

This validation should run during config load so invalid configs fail before any agent process is launched.

## Runtime Abstraction

The runtime agent config in `autotune-agent` should stop pretending one field fits all backends. It should gain separate runtime fields for backend-specific execution options, at minimum:

- `model`
- `max_turns` for Claude
- `reasoning_effort` for Codex

Claude should continue mapping `max_turns` to `--max-turns`.
Codex should map `reasoning_effort` to its native CLI setting instead of reading `max_turns`.

Session hydration and remembered session context should persist whichever backend-specific runtime values are needed for resumed sends.

## Global Defaults And Role Inheritance

The merge logic in `main.rs` currently only fills missing role fields from the user global config and only treats top-level `agent.backend` specially. That should be replaced with uniform field inheritance:

1. start with global `[agent]`
2. overlay global `[agent.<role>]`
3. overlay project `[agent]`
4. overlay project `[agent.<role>]`

This preserves the existing precedence rule that project config wins over global config while making defaults work for all agent fields, not only `backend`, `model`, and `max_turns`.

## CLI Surface

The config helper commands in `crates/autotune/src/main.rs` should be updated to include new dotted keys such as:

- `agent.model`
- `agent.reasoning_effort`
- `agent.research.reasoning_effort`
- `agent.implementation.reasoning_effort`
- `agent.init.reasoning_effort`

If `agent.max_turns` is retained as a top-level default for Claude, it should also be surfaced consistently.

The sample config output should stop implying that `max_turns` is the universal tuning knob.

## Testing

Tests should cover:

- parsing top-level and role-level `reasoning_effort`
- inheritance from `[agent]` into role configs
- project-over-global precedence for backend-specific keys
- rejection of `reasoning_effort` under Claude
- rejection of `max_turns` under Codex
- Codex runtime arg construction using `reasoning_effort`
- Claude runtime behavior remaining unchanged
- config CLI getter/setter coverage for new dotted keys

Scenario coverage should confirm that a project configured for Codex with role-level reasoning-effort overrides still runs through init or run paths without falling back to Claude defaults.

## Migration

Existing Claude configs should continue to work unchanged.

Existing Codex configs that relied on `max_turns` as reasoning effort should fail fast. There is no compatibility bridge.

Backend-specific validation should be strict:

- `backend = "codex"` rejects Claude-only keys such as `max_turns`
- `backend = "claude"` rejects Codex-only keys such as `reasoning_effort`

This keeps the abstraction honest and avoids silently preserving the current field overloading.
