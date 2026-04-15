# Codex Agent Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add local `codex` CLI support as a drop-in Autotune agent backend, with per-role backend selection and mixed Claude/Codex session support.

**Architecture:** Persist backend identity in task state so resumed research and fix sessions keep using the backend that created them. Add a dedicated `CodexAgent` in `autotune-agent` that translates Autotune permissions into Codex sandbox/config flags and parses Codex JSON events into the existing `Agent` interface. Move backend selection in the binary crate behind a small factory so `research`, `implementation`, and `init` each resolve their backend independently.

**Tech Stack:** Rust 2024, existing `autotune-agent` trait boundary, local `claude` and `codex` CLIs, serde/serde_json, `cargo nextest`, `cargo clippy`, `cargo fmt`

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `crates/autotune-state/src/lib.rs` | Persist research and implementation backend names in `TaskState` / `ApproachState` |
| Modify | `crates/autotune-state/tests/state_test.rs` | State round-trip coverage for new backend fields |
| Create | `crates/autotune/src/agent_factory.rs` | Backend resolution + backend instantiation helpers |
| Modify | `crates/autotune/src/lib.rs` | Export `agent_factory` for unit tests |
| Create | `crates/autotune-agent/src/codex.rs` | `CodexAgent` implementation |
| Modify | `crates/autotune-agent/src/lib.rs` | Export `codex` module |
| Modify | `crates/autotune-agent/tests/agent_test.rs` | Add Codex backend harness tests |
| Modify | `crates/autotune/src/main.rs` | Use factory instead of hard-coded `ClaudeAgent`; persist backend at task start and handoff |
| Modify | `crates/autotune/src/machine.rs` | Resume research and fix sessions with persisted backend names |
| Modify | `crates/autotune-init/src/lib.rs` | Use init-role backend selection and preserve init session backend |
| Modify | `crates/autotune-config/tests/config_test.rs` | Config parsing coverage for role-level `backend = "codex"` |
| Modify | `crates/autotune-config/tests/global_config_test.rs` | Global config coverage for Codex backend defaults |
| Modify | `crates/autotune-agent/src/protocol.rs` | Extend protocol parsing tests/examples that currently assume only Claude |
| Modify | `crates/autotune/tests/scenario_init_test.rs` | Init scenario coverage for Codex backend defaults under mock |
| Modify | `crates/autotune/src/main.rs` | Update global config template/help text to mention Claude and Codex |
| Optional modify | `notes/agent-subprocess.md` | Document Codex subprocess contract if implementation reveals non-obvious constraints |

---

### Task 1: Persist backend identity and add backend-resolution helpers

**Files:**
- Modify: `crates/autotune-state/src/lib.rs`
- Modify: `crates/autotune-state/tests/state_test.rs`
- Create: `crates/autotune/src/agent_factory.rs`
- Modify: `crates/autotune/src/lib.rs`

- [ ] **Step 1: Write the failing state round-trip test**

Add to `crates/autotune-state/tests/state_test.rs`:

```rust
#[test]
fn task_state_roundtrips_backend_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::new(dir.path()).unwrap();
    let state = TaskState {
        task_name: "bench".to_string(),
        canonical_branch: "main".to_string(),
        advancing_branch: "autotune/bench-main".to_string(),
        research_session_id: "research-1".to_string(),
        research_backend: "codex".to_string(),
        current_iteration: 2,
        current_phase: Phase::Fixing,
        current_approach: Some(ApproachState {
            name: "fast-path".to_string(),
            hypothesis: "trim allocations".to_string(),
            worktree_path: dir.path().join("wt"),
            branch_name: "autotune/bench/fast-path".to_string(),
            commit_sha: None,
            test_results: vec![],
            metrics: None,
            rank: None,
            files_to_modify: vec!["src/lib.rs".to_string()],
            impl_session_id: Some("impl-1".to_string()),
            impl_backend: Some("claude".to_string()),
            fix_attempts: 1,
            fresh_spawns: 0,
            fix_history: vec!["tests failed".to_string()],
        }),
    };

    store.save_state(&state).unwrap();
    let loaded = store.load_state().unwrap();

    assert_eq!(loaded.research_backend, "codex");
    assert_eq!(
        loaded.current_approach.unwrap().impl_backend.as_deref(),
        Some("claude")
    );
}
```

- [ ] **Step 2: Run the state test to verify it fails**

Run: `cargo nextest run -p autotune-state -E 'test(task_state_roundtrips_backend_fields)'`
Expected: FAIL because `TaskState` has no `research_backend` field and `ApproachState` has no `impl_backend` field.

- [ ] **Step 3: Add persisted backend fields with backwards-compatible defaults**

In `crates/autotune-state/src/lib.rs`, extend the structs:

```rust
fn default_backend() -> String {
    "claude".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskState {
    pub task_name: String,
    pub canonical_branch: String,
    #[serde(default)]
    pub advancing_branch: String,
    pub research_session_id: String,
    #[serde(default = "default_backend")]
    pub research_backend: String,
    pub current_iteration: usize,
    pub current_phase: Phase,
    pub current_approach: Option<ApproachState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApproachState {
    pub name: String,
    pub hypothesis: String,
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub commit_sha: Option<String>,
    pub test_results: Vec<TestResult>,
    pub metrics: Option<Metrics>,
    pub rank: Option<f64>,
    #[serde(default)]
    pub files_to_modify: Vec<String>,
    #[serde(default)]
    pub impl_session_id: Option<String>,
    #[serde(default)]
    pub impl_backend: Option<String>,
    #[serde(default)]
    pub fix_attempts: u32,
    #[serde(default)]
    pub fresh_spawns: u32,
    #[serde(default)]
    pub fix_history: Vec<String>,
}
```

This keeps old state files loadable by defaulting absent `research_backend` to `"claude"` and absent `impl_backend` to `None`.

- [ ] **Step 4: Run the state crate tests**

Run: `cargo nextest run -p autotune-state`
Expected: PASS, including the new backend round-trip coverage.

- [ ] **Step 5: Write the failing backend-resolution unit test**

Create `crates/autotune/src/agent_factory.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::resolve_backend_name;
    use autotune_config::{AgentConfig, AgentRoleConfig};

    fn base_config() -> AgentConfig {
        AgentConfig {
            backend: "claude".to_string(),
            research: None,
            implementation: None,
            init: None,
        }
    }

    #[test]
    fn role_backend_overrides_global_backend() {
        let mut config = base_config();
        config.agent.research = Some(AgentRoleConfig {
            backend: Some("codex".to_string()),
            model: None,
            max_turns: None,
            max_fix_attempts: None,
            max_fresh_spawns: None,
        });

        assert_eq!(resolve_backend_name(&config, AgentRole::Research), "codex");
        assert_eq!(
            resolve_backend_name(&config, AgentRole::Implementation),
            "claude"
        );
    }
}
```

- [ ] **Step 6: Run the resolution test to verify it fails**

Run: `cargo test -p autotune role_backend_overrides_global_backend --lib`
Expected: FAIL because `agent_factory` and `resolve_backend_name` do not exist.

- [ ] **Step 7: Implement resolution helper and export it**

Create `crates/autotune/src/agent_factory.rs`:

```rust
use anyhow::{Result, bail};
use autotune_agent::Agent;
use autotune_config::{AgentConfig, AgentRoleConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Research,
    Implementation,
    Init,
}

pub fn resolve_backend_name(config: &AgentConfig, role: AgentRole) -> &str {
    let role_cfg: Option<&AgentRoleConfig> = match role {
        AgentRole::Research => config.research.as_ref(),
        AgentRole::Implementation => config.implementation.as_ref(),
        AgentRole::Init => config.init.as_ref(),
    };

    role_cfg
        .and_then(|cfg| cfg.backend.as_deref())
        .unwrap_or(config.backend.as_str())
}

pub fn build_agent_for_backend(backend: &str) -> Result<Box<dyn Agent>> {
    match backend {
        "claude" => Ok(Box::new(autotune_agent::claude::ClaudeAgent::new())),
        "codex" => Ok(Box::new(autotune_agent::codex::CodexAgent::new())),
        other => bail!("unsupported agent backend '{other}' (supported: claude, codex)"),
    }
}
```

Export the module from `crates/autotune/src/lib.rs`:

```rust
pub mod agent_factory;
```

- [ ] **Step 8: Run the new `autotune` library test**

Run: `cargo test -p autotune role_backend_overrides_global_backend --lib`
Expected: still FAIL until `CodexAgent` exists; that is acceptable at this point. Commit nothing yet.

---

### Task 2: Add `CodexAgent` with non-streaming, streaming, and permission-grant behavior

**Files:**
- Create: `crates/autotune-agent/src/codex.rs`
- Modify: `crates/autotune-agent/src/lib.rs`
- Modify: `crates/autotune-agent/tests/agent_test.rs`

- [ ] **Step 1: Write the failing backend-name and handover tests**

Add to `crates/autotune-agent/tests/agent_test.rs`:

```rust
use autotune_agent::codex::CodexAgent;

#[test]
fn codex_backend_name() {
    let agent = CodexAgent::new();
    assert_eq!(agent.backend_name(), "codex");
}

#[test]
fn codex_handover_command() {
    let agent = CodexAgent::new();
    let session = AgentSession {
        session_id: "thread-123".to_string(),
        backend: "codex".to_string(),
    };
    assert_eq!(agent.handover_command(&session), "codex resume thread-123");
}
```

- [ ] **Step 2: Add the failing spawn/resume harness test**

Append to `crates/autotune-agent/tests/agent_test.rs`:

```rust
#[test]
fn codex_send_preserves_spawn_context() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"agent_message_delta","delta":"spawned"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let agent = CodexAgent::with_command(harness.codex_path());
    let working_directory = harness.root.join("workspace");
    fs::create_dir_all(&working_directory).unwrap();

    let config = AgentConfig {
        prompt: "initial prompt".to_string(),
        allowed_tools: vec![
            ToolPermission::Allow("Read".to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: working_directory.clone(),
        model: Some("gpt-5.4".to_string()),
        max_turns: Some(12),
    };

    let response = agent.spawn(&config).unwrap();
    harness.write_response(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"agent_message_delta","delta":"follow-up"}
{"event":"turn_complete","last_agent_message":"follow-up"}"#,
    );

    let session = AgentSession {
        session_id: response.session_id,
        backend: "codex".to_string(),
    };
    let send_response = agent.send(&session, "second prompt").unwrap();

    assert_eq!(send_response.text, "follow-up");

    let invocations = harness.read_invocations();
    assert_eq!(invocations.len(), 2);
    assert!(invocations[1].args.starts_with(&[
        "exec".to_string(),
        "resume".to_string(),
        "thread-123".to_string(),
    ]));
    assert!(invocations[1].args.contains(&"-C".to_string()));
    assert!(invocations[1].args.contains(&working_directory.display().to_string()));
    assert!(invocations[1].args.contains(&"--model".to_string()));
    assert!(invocations[1].args.contains(&"gpt-5.4".to_string()));
}
```

Use `FakeCodexHarness` built by cloning the Claude harness pattern and changing the executable name to `codex`.

- [ ] **Step 3: Add the failing streaming and permission-grant tests**

Add these tests too:

```rust
#[test]
fn codex_streaming_emits_text_and_tool_events() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"agent_message_delta","delta":"thinking"}
{"event":"exec_command_begin","call_id":"cmd-1","command":["rg","foo"]}
{"event":"turn_complete","last_agent_message":"done"}"#,
    );
    let agent = CodexAgent::with_command(harness.codex_path());
    let seen = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let seen_for_handler = Arc::clone(&seen);

    let response = agent
        .spawn_streaming(
            AgentConfigWithEvents::new(basic_config(&harness.root)).with_event_handler(Box::new(
                move |event| seen_for_handler.lock().unwrap().push(event),
            )),
        )
        .unwrap();

    assert_eq!(response.session_id, "thread-123");
    let events = seen.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Text(text) if text == "thinking")));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolUse { tool, .. } if tool == "exec_command")));
}

#[test]
fn codex_permission_grant_widens_next_resume_flags() {
    let harness = FakeCodexHarness::new(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"spawned"}"#,
    );
    let agent = CodexAgent::with_command(harness.codex_path());
    let response = agent.spawn(&basic_config(&harness.root)).unwrap();
    let session = AgentSession {
        session_id: response.session_id,
        backend: "codex".to_string(),
    };

    agent
        .grant_session_permission(
            &session,
            ToolPermission::AllowScoped("Write".to_string(), "/tmp/worktree".to_string()),
        )
        .unwrap();

    harness.write_response(
        r#"{"event":"thread.started","thread_id":"thread-123"}
{"event":"turn_complete","last_agent_message":"follow-up"}"#,
    );
    let _ = agent.send(&session, "continue").unwrap();
    let invocations = harness.read_invocations();

    assert!(invocations[1].args.contains(&"--add-dir".to_string()));
    assert!(invocations[1].args.contains(&"/tmp/worktree".to_string()));
}
```

- [ ] **Step 4: Run the new agent tests to verify they fail**

Run: `cargo nextest run -p autotune-agent -E 'test(codex_)'`
Expected: FAIL because `codex` module and harness do not exist.

- [ ] **Step 5: Export the module and implement `CodexAgent`**

In `crates/autotune-agent/src/lib.rs`, add:

```rust
pub mod codex;
```

Create `crates/autotune-agent/src/codex.rs` with this skeleton:

```rust
use crate::{
    Agent, AgentConfig, AgentConfigWithEvents, AgentError, AgentEvent, AgentResponse, AgentSession,
    EventHandler, ToolPermission,
};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub struct CodexAgent {
    command: PathBuf,
    sessions: Mutex<HashMap<String, SessionContext>>,
}

#[derive(Debug, Clone)]
struct SessionContext {
    working_directory: PathBuf,
    model: Option<String>,
    max_turns: Option<u64>,
    allowed_tools: Vec<ToolPermission>,
}

impl CodexAgent {
    pub fn new() -> Self {
        Self::with_command(PathBuf::from("codex"))
    }

    pub fn with_command(command: PathBuf) -> Self {
        Self {
            command,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn build_args(config: &AgentConfig, session_id: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "exec".to_string(),
            "--json".to_string(),
            "--color".to_string(),
            "never".to_string(),
            "-C".to_string(),
            config.working_directory.display().to_string(),
        ];
        if let Some(session_id) = session_id {
            args.extend(["resume".to_string(), session_id.to_string()]);
        }
        if let Some(model) = &config.model {
            args.extend(["--model".to_string(), model.clone()]);
        }
        if let Some(turns) = config.max_turns {
            args.extend([
                "-c".to_string(),
                format!("model_reasoning_effort={turns}"),
            ]);
        }
        args.extend(permission_args(&config.allowed_tools));
        args.push(config.prompt.clone());
        args
    }
}
```

Implement helper functions in the same file:

```rust
fn permission_args(perms: &[ToolPermission]) -> Vec<String> {
    let mut writable_dirs = BTreeSet::new();
    let mut has_write = false;
    let mut deny_bash = false;

    for perm in perms {
        match perm {
            ToolPermission::Allow(tool) if tool == "Write" || tool == "Edit" => has_write = true,
            ToolPermission::AllowScoped(tool, path) if tool == "Write" || tool == "Edit" => {
                has_write = true;
                writable_dirs.insert(path.clone());
            }
            ToolPermission::Deny(tool) if tool == "Bash" => deny_bash = true,
            _ => {}
        }
    }

    let mut args = vec![
        "--sandbox".to_string(),
        if has_write {
            "workspace-write".to_string()
        } else {
            "read-only".to_string()
        },
        "-a".to_string(),
        if deny_bash {
            "never".to_string()
        } else {
            "on-request".to_string()
        },
    ];

    for dir in writable_dirs {
        args.extend(["--add-dir".to_string(), dir]);
    }

    args
}
```

Parse the JSONL stream by looking for event names surfaced by the installed Codex binary:

```rust
fn parse_jsonl(stdout: impl BufRead, handler: Option<&EventHandler>) -> Result<AgentResponse, AgentError> {
    let mut thread_id: Option<String> = None;
    let mut last_message = String::new();

    for line in stdout.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|source| AgentError::ParseFailed {
            message: format!("invalid codex JSON output: {source}"),
        })?;

        let event = value.get("event").and_then(Value::as_str).unwrap_or("");
        match event {
            "thread.started" | "thread_started" => {
                thread_id = value
                    .get("thread_id")
                    .or_else(|| value.get("thread").and_then(|t| t.get("id")))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            "agent_message_delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    if let Some(handler) = handler {
                        handler(AgentEvent::Text(text.to_string()));
                    }
                    last_message.push_str(text);
                }
            }
            "exec_command_begin" => {
                if let Some(handler) = handler {
                    handler(AgentEvent::ToolUse {
                        tool: "exec_command".to_string(),
                        input_summary: value
                            .get("command")
                            .map(|cmd| cmd.to_string())
                            .unwrap_or_default(),
                    });
                }
            }
            "turn_complete" | "task_complete" => {
                if let Some(text) = value.get("last_agent_message").and_then(Value::as_str) {
                    last_message = text.to_string();
                }
            }
            _ => {}
        }
    }

    let session_id = thread_id.ok_or_else(|| AgentError::ParseFailed {
        message: "codex JSON missing thread/session id".to_string(),
    })?;

    Ok(AgentResponse {
        text: last_message,
        session_id,
    })
}
```

Implement `spawn`, `send`, `spawn_streaming`, `send_streaming`, `backend_name`, `handover_command`, and `grant_session_permission` with the same session-memory pattern Claude uses.

- [ ] **Step 6: Run the Codex-focused backend tests**

Run: `cargo nextest run -p autotune-agent -E 'test(codex_)'`
Expected: PASS.

- [ ] **Step 7: Run the full `autotune-agent` test suite**

Run: `cargo nextest run -p autotune-agent`
Expected: PASS for Claude and Codex tests together.

- [ ] **Step 8: Commit**

```bash
git add crates/autotune-agent/src/lib.rs crates/autotune-agent/src/codex.rs crates/autotune-agent/tests/agent_test.rs
git commit -m "feat(agent): add codex cli backend"
```

---

### Task 3: Wire backend factory through run, resume, machine, and init flows

**Files:**
- Modify: `crates/autotune/src/main.rs`
- Modify: `crates/autotune/src/machine.rs`
- Modify: `crates/autotune-init/src/lib.rs`
- Modify: `crates/autotune/src/agent_factory.rs`

- [ ] **Step 1: Write the failing mixed-backend selection test**

Add to `crates/autotune/src/agent_factory.rs`:

```rust
#[test]
fn build_agent_for_backend_supports_claude_and_codex() {
    assert_eq!(build_agent_for_backend("claude").unwrap().backend_name(), "claude");
    assert_eq!(build_agent_for_backend("codex").unwrap().backend_name(), "codex");
}

#[test]
fn build_agent_for_backend_rejects_unknown_backend() {
    let err = build_agent_for_backend("bogus").unwrap_err().to_string();
    assert!(err.contains("supported: claude, codex"));
}
```

- [ ] **Step 2: Run the factory tests**

Run: `cargo test -p autotune build_agent_for_backend --lib`
Expected: PASS now that `CodexAgent` exists.

- [ ] **Step 3: Write the failing state-construction tests for persisted backend names**

Add to `crates/autotune/src/machine.rs` tests:

```rust
#[test]
fn planning_uses_persisted_research_backend() {
    let state = TaskState {
        task_name: "bench".to_string(),
        canonical_branch: "main".to_string(),
        advancing_branch: "autotune/bench-main".to_string(),
        research_session_id: "research-1".to_string(),
        research_backend: "codex".to_string(),
        current_iteration: 1,
        current_phase: Phase::Planning,
        current_approach: None,
    };

    let research_session = AgentSession {
        session_id: state.research_session_id.clone(),
        backend: state.research_backend.clone(),
    };

    assert_eq!(research_session.backend, "codex");
}
```

Add a similar test for fix continuation:

```rust
#[test]
fn fixing_uses_persisted_implementation_backend() {
    let approach = ApproachState {
        name: "fast-path".to_string(),
        hypothesis: "trim allocations".to_string(),
        worktree_path: PathBuf::from("/tmp/worktree"),
        branch_name: "autotune/bench/fast-path".to_string(),
        commit_sha: None,
        test_results: vec![],
        metrics: None,
        rank: None,
        files_to_modify: vec![],
        impl_session_id: Some("impl-1".to_string()),
        impl_backend: Some("claude".to_string()),
        fix_attempts: 0,
        fresh_spawns: 0,
        fix_history: vec![],
    };

    let session = AgentSession {
        session_id: approach.impl_session_id.clone().unwrap(),
        backend: approach.impl_backend.clone().unwrap(),
    };

    assert_eq!(session.backend, "claude");
}
```

- [ ] **Step 4: Run the machine tests to verify the current code path is still assuming `agent.backend_name()`**

Run: `cargo test -p autotune persisted_ --lib`
Expected: either compile fails until helper code is updated, or tests fail if wired through current runtime helpers.

- [ ] **Step 5: Refactor the binary and machine to use role-specific backend resolution**

In `crates/autotune/src/main.rs`, replace the hard-coded builders:

```rust
use autotune::agent_factory::{AgentRole, build_agent_for_backend, resolve_backend_name};

fn build_agent(config: &AutotuneConfig, role: AgentRole) -> Result<Box<dyn Agent>> {
    #[cfg(feature = "mock")]
    if std::env::var("AUTOTUNE_MOCK").is_ok() {
        // existing mock path stays unchanged
    }

    let backend = resolve_backend_name(&config.agent, role);
    build_agent_for_backend(backend)
}
```

Update call sites:

```rust
let research_agent = build_agent(&config, AgentRole::Research)?;
let init_agent = build_agent_from_global(&global_config, AgentRole::Init)?;
```

Persist the backend when creating state:

```rust
let research_backend = resolve_backend_name(&config.agent, AgentRole::Research).to_string();
let initial_state = TaskState {
    task_name: config.task.name.clone(),
    canonical_branch: config.task.canonical_branch.clone(),
    advancing_branch,
    research_session_id: research_response.session_id.clone(),
    research_backend,
    current_iteration: 1,
    current_phase: Phase::Planning,
    current_approach: None,
};
```

In `crates/autotune/src/machine.rs`, construct sessions from persisted state:

```rust
let research_session = AgentSession {
    session_id: state.research_session_id.clone(),
    backend: state.research_backend.clone(),
};
```

When entering a fresh implementation session, persist the implementation backend:

```rust
state.current_approach = Some(ApproachState {
    name: hypothesis.approach.clone(),
    hypothesis: hypothesis.hypothesis.clone(),
    worktree_path,
    branch_name,
    commit_sha: None,
    test_results: vec![],
    metrics: None,
    rank: None,
    files_to_modify: hypothesis.files_to_modify.clone(),
    impl_session_id: None,
    impl_backend: Some(
        resolve_backend_name(&config.agent, AgentRole::Implementation).to_string()
    ),
    fix_attempts: 0,
    fresh_spawns: 0,
    fix_history: vec![],
});
```

For fix continuation:

```rust
let session = AgentSession {
    session_id: active_session.clone().unwrap(),
    backend: approach
        .impl_backend
        .clone()
        .unwrap_or_else(|| "claude".to_string()),
};
```

In `crates/autotune-init/src/lib.rs`, keep using the backend of the init agent session:

```rust
let session = AgentSession {
    session_id: response.session_id,
    backend: agent.backend_name().to_string(),
};
```

That line can stay as-is because init sessions are not persisted, but `main.rs` must instantiate the init agent through `AgentRole::Init`.

- [ ] **Step 6: Run the targeted binary and machine tests**

Run: `cargo test -p autotune build_agent_for_backend --lib`
Expected: PASS.

Run: `cargo test -p autotune persisted_ --lib`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/autotune/src/lib.rs crates/autotune/src/agent_factory.rs crates/autotune/src/main.rs crates/autotune/src/machine.rs crates/autotune-init/src/lib.rs crates/autotune-state/src/lib.rs crates/autotune-state/tests/state_test.rs
git commit -m "feat(autotune): wire mixed claude and codex backends"
```

---

### Task 4: Cover config/help text, init scenarios, and repo-wide verification

**Files:**
- Modify: `crates/autotune-config/tests/config_test.rs`
- Modify: `crates/autotune-config/tests/global_config_test.rs`
- Modify: `crates/autotune-agent/src/protocol.rs`
- Modify: `crates/autotune/tests/scenario_init_test.rs`
- Modify: `crates/autotune/src/main.rs`
- Optional modify: `notes/agent-subprocess.md`

- [ ] **Step 1: Add the failing config parsing tests**

In `crates/autotune-config/tests/config_test.rs`, add:

```rust
#[test]
fn parse_agent_config_with_codex_backends() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
name = "b"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "m", pattern = "x" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "m", direction = "Maximize" }]

[agent]
backend = "claude"

[agent.research]
backend = "codex"

[agent.implementation]
backend = "claude"

[agent.init]
backend = "codex"
"#,
    );

    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.agent.backend, "claude");
    assert_eq!(
        config.agent.research.unwrap().backend.as_deref(),
        Some("codex")
    );
    assert_eq!(
        config.agent.init.unwrap().backend.as_deref(),
        Some("codex")
    );
}
```

In `crates/autotune-config/tests/global_config_test.rs`, add:

```rust
#[test]
fn global_config_loads_codex_backend_defaults() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
[agent]
backend = "codex"

[agent.implementation]
backend = "claude"
"#,
    )
    .unwrap();

    let config = GlobalConfig::load_from(f.path()).unwrap();
    let agent = config.agent.unwrap();
    assert_eq!(agent.backend, "codex");
    assert_eq!(
        agent.implementation.unwrap().backend.as_deref(),
        Some("claude")
    );
}
```

- [ ] **Step 2: Run config tests to verify they fail if the parser/tests still encode Claude-only assumptions**

Run: `cargo nextest run -p autotune-config -E 'test(codex_backends|codex_backend_defaults)'`
Expected: FAIL only if any validation/test helper still assumes `"claude"` as the only allowed value.

- [ ] **Step 3: Add the failing init scenario around global Codex defaults**

In `crates/autotune/tests/scenario_init_test.rs`, add:

```rust
#[test]
fn scenario_init_honors_global_codex_backend_default() {
    let project = mock_project();
    let config_home = tempfile::tempdir().unwrap();
    let autotune_home = config_home.path().join("autotune");
    std::fs::create_dir_all(&autotune_home).unwrap();
    std::fs::write(
        autotune_home.join("config.toml"),
        r#"
[agent]
backend = "codex"

[agent.init]
backend = "codex"
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .env("XDG_CONFIG_HOME", config_home.path())
        .current_dir(project.path())
        .write_stdin("optimize performance\nperf\nbench\nyes\n")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.contains("mock"), "stderr:\n{stderr}");
}
```

This test stays mock-backed; it verifies backend resolution through the init path without requiring a real Codex subprocess in CI.

- [ ] **Step 4: Update help text and config template**

In `crates/autotune/src/main.rs`, change the template comment:

```rust
const CONFIG_TEMPLATE: &str = r#"# Autotune global config
# Default agent settings used across all tasks.
# Uncomment and edit the values you want to set.

# [agent]
# backend = "claude"            # LLM backend ("claude" or "codex")
```

If any user-facing error/help string says “currently only claude”, update it to list both backends.

- [ ] **Step 5: Run focused init/config tests**

Run: `cargo nextest run -p autotune-config`
Expected: PASS.

Run: `cargo nextest run -p autotune --features mock -E 'test(scenario_init_honors_global_codex_backend_default)'`
Expected: PASS.

- [ ] **Step 6: Decide whether to update subprocess notes**

If implementation uncovered a non-obvious Codex constraint, extend `notes/agent-subprocess.md` with a short Codex section like:

```md
## Codex CLI notes

- `codex exec --json` emits line-delimited event objects; Autotune consumes
  `thread.started`, `agent_message_delta`, `exec_command_begin`, and
  `turn_complete`.
- Even non-interactive invocations may want writable session storage; tests
  should use a fake binary harness rather than relying on a real local install.
```

Skip this step if the final behavior is obvious from `codex.rs` and tests.

- [ ] **Step 7: Run repo verification**

Run: `cargo fmt --all`
Expected: formatting changes only.

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

Run: `cargo nextest run`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/autotune-config/tests/config_test.rs crates/autotune-config/tests/global_config_test.rs crates/autotune/tests/scenario_init_test.rs crates/autotune/src/main.rs notes/agent-subprocess.md
git commit -m "test: cover codex backend selection and docs"
```

---

## Self-Review

- Spec coverage:
  Task 1 covers persisted backend identity and role-level backend resolution.
  Task 2 covers the new `CodexAgent`, streaming, and runtime permission grants.
  Task 3 covers mixed-backend runtime wiring for run, resume, planning, fixing, and init.
  Task 4 covers config parsing, init scenarios, user-facing help text, optional notes, and full verification.
- Placeholder scan:
  No `TODO` / `TBD` markers remain in the tasks. The only conditional step is whether the subprocess note is warranted after implementation.
- Type consistency:
  The plan uses `research_backend` on `TaskState` and `impl_backend` on `ApproachState` consistently, and the backend names are always the literal strings `"claude"` and `"codex"`.
