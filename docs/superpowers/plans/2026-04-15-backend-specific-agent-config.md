# Backend-Specific Agent Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add strict, backend-aware agent config support so flat `[agent]` and `[agent.<role>]` tables can express backend-specific keys without overloading fields across Claude and Codex.

**Architecture:** Extend `autotune-config` to model and validate backend-specific role fields, then thread the new effective role config through runtime agent construction in `autotune` and `autotune-agent`. Keep inheritance flat: top-level `[agent]` provides defaults, role tables override, and invalid backend/key combinations fail during config load.

**Tech Stack:** Rust, Serde/TOML, cargo test, cargo nextest

---

### Task 1: Add Config Types For Backend-Specific Role Settings

**Files:**
- Modify: `crates/autotune-config/src/lib.rs`
- Test: `crates/autotune-config/tests/config_test.rs`

- [ ] **Step 1: Write failing config parsing tests for Codex reasoning effort**

Add tests in `crates/autotune-config/tests/config_test.rs` that parse:

```toml
[task]
name = "agent-config"
max_iterations = "5"

[paths]
tunable = ["crates/**"]

[[measure]]
name = "m"
command = ["echo", "line=1"]
adaptor = { type = "regex", patterns = [{ name = "line", pattern = 'line=([0-9.]+)' }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "line", direction = "Maximize" }]

[agent]
backend = "codex"
model = "gpt-5.4"
reasoning_effort = "medium"

[agent.research]
reasoning_effort = "high"
```

Assert that:
- `config.agent.backend == "codex"`
- top-level `reasoning_effort` parses
- role-level `reasoning_effort` parses

- [ ] **Step 2: Run the new targeted test and confirm it fails**

Run: `cargo test -p autotune-config parse_codex_reasoning_effort_config -- --exact`

Expected: FAIL because `reasoning_effort` is not yet part of the config schema.

- [ ] **Step 3: Add typed reasoning-effort support to the config model**

In `crates/autotune-config/src/lib.rs`, add:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}
```

and extend `AgentRoleConfig` with:

```rust
#[serde(default)]
pub reasoning_effort: Option<ReasoningEffort>,
```

Also add helper methods to overlay defaults cleanly:

```rust
impl AgentRoleConfig {
    pub fn overlay(&self, defaults: &AgentRoleConfig) -> AgentRoleConfig {
        AgentRoleConfig {
            backend: self.backend.clone().or_else(|| defaults.backend.clone()),
            model: self.model.clone().or_else(|| defaults.model.clone()),
            max_turns: self.max_turns.or(defaults.max_turns),
            reasoning_effort: self.reasoning_effort.or(defaults.reasoning_effort),
            max_fix_attempts: self.max_fix_attempts.or(defaults.max_fix_attempts),
            max_fresh_spawns: self.max_fresh_spawns.or(defaults.max_fresh_spawns),
        }
    }
}
```

- [ ] **Step 4: Re-run the parsing test and confirm it passes**

Run: `cargo test -p autotune-config parse_codex_reasoning_effort_config -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit the config-model change**

```bash
git add crates/autotune-config/src/lib.rs crates/autotune-config/tests/config_test.rs
git commit -m "feat: add backend-specific agent config fields"
```

### Task 2: Enforce Strict Backend-Aware Validation At Config Load

**Files:**
- Modify: `crates/autotune-config/src/lib.rs`
- Test: `crates/autotune-config/tests/config_test.rs`

- [ ] **Step 1: Write failing validation tests for invalid backend/key combinations**

Add tests that attempt to load configs containing:

```toml
[agent]
backend = "codex"
max_turns = 10
```

and:

```toml
[agent]
backend = "claude"
reasoning_effort = "medium"
```

Use the same minimal task/paths/measure/score scaffolding as Task 1. Assert that load fails and the error mentions both the field and backend.

- [ ] **Step 2: Run the targeted validation tests and confirm they fail**

Run: `cargo test -p autotune-config codex_rejects_max_turns -- --exact`

Run: `cargo test -p autotune-config claude_rejects_reasoning_effort -- --exact`

Expected: FAIL because validation is not yet backend-aware.

- [ ] **Step 3: Add effective-role validation after inheritance**

In `crates/autotune-config/src/lib.rs`, add validation helpers that compute:
- effective defaults from `[agent]`
- effective role configs for `research`, `implementation`, and `init`

Then validate with logic shaped like:

```rust
fn validate_role_backend_fields(
    path: &str,
    backend: &str,
    role: &AgentRoleConfig,
) -> Result<(), ConfigError> {
    match backend {
        "claude" => {
            if role.reasoning_effort.is_some() {
                return Err(ConfigError::Validation {
                    message: format!("{path}.reasoning_effort is not valid for backend 'claude'"),
                });
            }
        }
        "codex" => {
            if role.max_turns.is_some() {
                return Err(ConfigError::Validation {
                    message: format!("{path}.max_turns is not valid for backend 'codex'"),
                });
            }
        }
        _ => {}
    }
    Ok(())
}
```

Call this for top-level `[agent]` and each effective role config during `AutotuneConfig::load`.

- [ ] **Step 4: Re-run the validation tests and confirm they pass**

Run:
- `cargo test -p autotune-config codex_rejects_max_turns -- --exact`
- `cargo test -p autotune-config claude_rejects_reasoning_effort -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit the validation change**

```bash
git add crates/autotune-config/src/lib.rs crates/autotune-config/tests/config_test.rs
git commit -m "feat: validate backend-specific agent config keys"
```

### Task 3: Implement Global And Role Default Inheritance

**Files:**
- Modify: `crates/autotune/src/main.rs`
- Test: `crates/autotune-config/tests/global_config_test.rs`
- Test: `crates/autotune/tests/scenario_init_test.rs`

- [ ] **Step 1: Write failing tests for inheritance precedence**

Add tests covering:
- global `[agent]` default `reasoning_effort = "medium"`
- project `[agent.research]` override `reasoning_effort = "high"`
- project role values winning over global defaults

For init-path coverage, extend an existing scenario or add a focused one that sets a global config with:

```toml
[agent]
backend = "codex"
model = "gpt-5.4"
reasoning_effort = "medium"

[agent.init]
reasoning_effort = "low"
```

Assert that init still succeeds under mock and does not fall back to Claude.

- [ ] **Step 2: Run the targeted inheritance tests and confirm they fail**

Run:
- `cargo test -p autotune-config load_codex_backend_defaults -- --exact`
- `cargo test -p autotune scenario_init_uses_global_codex_backend_default_under_mock -- --exact`

Expected: at least one failure or missing assertion coverage for `reasoning_effort`.

- [ ] **Step 3: Replace the ad hoc merge logic with uniform overlay**

In `crates/autotune/src/main.rs`, rework `apply_global_agent_defaults` so it conceptually applies:

```rust
effective_top = project_top.overlay(&global_top)
effective_role = project_role.overlay(&effective_top).overlay(&global_role_if_needed)
```

Implement the repository’s precedence explicitly:
1. start with global `[agent]`
2. overlay global `[agent.<role>]`
3. overlay project `[agent]`
4. overlay project `[agent.<role>]`

Make sure `init` is included; the current merge helper skips it.

- [ ] **Step 4: Re-run the inheritance tests and confirm they pass**

Run:
- `cargo test -p autotune-config load_codex_backend_defaults -- --exact`
- `cargo test -p autotune scenario_init_uses_global_codex_backend_default_under_mock -- --exact`

Expected: PASS with assertions updated to cover `reasoning_effort`.

- [ ] **Step 5: Commit the inheritance change**

```bash
git add crates/autotune/src/main.rs crates/autotune-config/tests/global_config_test.rs crates/autotune/tests/scenario_init_test.rs
git commit -m "feat: inherit backend-specific agent defaults"
```

### Task 4: Split Runtime Agent Settings Between Claude And Codex

**Files:**
- Modify: `crates/autotune-agent/src/lib.rs`
- Modify: `crates/autotune-agent/src/claude.rs`
- Modify: `crates/autotune-agent/src/codex.rs`
- Test: `crates/autotune-agent/tests/agent_test.rs`

- [ ] **Step 1: Write failing runtime tests for Codex reasoning effort**

Add a Codex agent test that builds an `autotune_agent::AgentConfig` with:

```rust
AgentConfig {
    prompt: "test".to_string(),
    allowed_tools: vec![],
    working_directory: harness.root.clone(),
    model: Some("gpt-5.4".to_string()),
    max_turns: None,
    reasoning_effort: Some(ReasoningEffort::High),
}
```

Assert that the captured Codex invocation contains:

```text
-c model_reasoning_effort=high
```

and does not require `max_turns`.

- [ ] **Step 2: Run the targeted Codex agent test and confirm it fails**

Run: `cargo test -p autotune-agent codex_uses_reasoning_effort -- --exact`

Expected: FAIL because runtime `AgentConfig` does not yet expose `reasoning_effort`.

- [ ] **Step 3: Extend runtime agent config and backend mapping**

In `crates/autotune-agent/src/lib.rs`, add:

```rust
pub reasoning_effort: Option<autotune_config::ReasoningEffort>,
```

to runtime `AgentConfig`.

Update:
- `crates/autotune-agent/src/claude.rs` to ignore `reasoning_effort`
- `crates/autotune-agent/src/codex.rs` to emit `model_reasoning_effort=<level>` from `reasoning_effort`
- session context structs in both backends so resume preserves the new field where relevant

- [ ] **Step 4: Re-run the targeted agent tests and confirm they pass**

Run:
- `cargo test -p autotune-agent codex_uses_reasoning_effort -- --exact`
- `cargo test -p autotune-agent codex_send_preserves_spawn_context -- --exact`
- `cargo test -p autotune-agent claude_spawn_uses_max_turns -- --exact`

Expected: PASS. Claude behavior remains unchanged.

- [ ] **Step 5: Commit the runtime-agent change**

```bash
git add crates/autotune-agent/src/lib.rs crates/autotune-agent/src/claude.rs crates/autotune-agent/src/codex.rs crates/autotune-agent/tests/agent_test.rs
git commit -m "feat: separate codex reasoning effort from claude max turns"
```

### Task 5: Thread Effective Role Config Into Agent Construction And Config CLI

**Files:**
- Modify: `crates/autotune/src/main.rs`
- Modify: `crates/autotune/src/machine.rs`
- Test: `crates/autotune/tests/integration_test.rs`

- [ ] **Step 1: Write failing tests for config key inspection**

Add tests for config helper surfaces that expect support for:
- `agent.reasoning_effort`
- `agent.research.reasoning_effort`
- `agent.implementation.reasoning_effort`
- `agent.init.reasoning_effort`

Also add an integration assertion that a Codex-configured research or implementation agent gets `gpt-5.4` plus the expected reasoning-effort level in the runtime config captured by mocks or harnesses.

- [ ] **Step 2: Run the targeted CLI/integration tests and confirm they fail**

Run:
- `cargo test -p autotune config_get_agent_reasoning_effort -- --exact`
- `cargo test -p autotune codex_role_config_flows_to_runtime_agent -- --exact`

Expected: FAIL because the key list and runtime construction do not yet support the new field.

- [ ] **Step 3: Update role-to-runtime conversion and CLI key handling**

In `crates/autotune/src/main.rs` and `crates/autotune/src/machine.rs`:
- build runtime `autotune_agent::AgentConfig` from effective role config, not ad hoc direct field reads
- populate `reasoning_effort` for Codex roles
- keep `max_turns` for Claude roles

Update config CLI support in `crates/autotune/src/main.rs`:
- sample config output
- allowed dotted-key list
- getter logic
- setter parsing for `reasoning_effort`

- [ ] **Step 4: Re-run the targeted CLI/integration tests and confirm they pass**

Run:
- `cargo test -p autotune config_get_agent_reasoning_effort -- --exact`
- `cargo test -p autotune codex_role_config_flows_to_runtime_agent -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit the CLI/runtime wiring change**

```bash
git add crates/autotune/src/main.rs crates/autotune/src/machine.rs crates/autotune/tests/integration_test.rs
git commit -m "feat: wire backend-specific agent config through runtime"
```

### Task 6: Run Verification And Refresh Project Config

**Files:**
- Modify: `.autotune.toml`

- [ ] **Step 1: Update the local project config to the new Codex schema**

Change `.autotune.toml` from `max_turns`-style Codex tuning to explicit reasoning-effort fields, for example:

```toml
[agent]
backend = "codex"
model = "gpt-5.4"
reasoning_effort = "medium"

[agent.research]
reasoning_effort = "high"

[agent.implementation]
reasoning_effort = "low"

[agent.init]
reasoning_effort = "medium"
```

- [ ] **Step 2: Run focused verification while iterating**

Run:
- `cargo test -p autotune-config`
- `cargo test -p autotune-agent`
- `cargo test -p autotune scenario_init_uses_global_codex_backend_default_under_mock -- --exact`

Expected: PASS.

- [ ] **Step 3: Run repository pre-commit verification**

Run:
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo nextest run`

Expected: all PASS.

- [ ] **Step 4: Commit the final verified state**

```bash
git add .autotune.toml
git commit -m "feat: support backend-specific agent config"
```
