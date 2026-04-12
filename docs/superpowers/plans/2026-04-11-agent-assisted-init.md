# Agent-Assisted Init Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When `autotune init` runs without `.autotune.toml`, spawn an init agent that explores the codebase, converses with the user, and incrementally builds validated config sections — then seamlessly continue into baseline benchmarking.

**Architecture:** New `autotune-init` crate owns the conversation loop. Protocol types (`AgentRequest`, `ConfigSection`) live in `autotune-agent` for reuse by the research agent later. Global config (`GlobalConfig`) in `autotune-config` resolves agent settings from system/user defaults. The binary crate wires it together — calling `run_init()` when no `.autotune.toml` exists, then continuing into the existing init flow.

**Tech Stack:** Rust 2024 edition, serde (JSON for protocol, TOML for config), thiserror, anyhow, dirs (for XDG paths)

---

## File Map

| Action | File | Responsibility |
|--------|------|---------------|
| Modify | `crates/autotune-config/Cargo.toml` | Add `dirs` dependency |
| Modify | `crates/autotune-config/src/lib.rs` | Add `Serialize` derives, add `Serialize` impl for `StopValue`, add `GlobalConfig` |
| Create | `crates/autotune-config/src/global.rs` | `GlobalConfig` struct + `load()` with system/user resolution |
| Create | `crates/autotune-config/tests/global_config_test.rs` | Tests for global config loading |
| Modify | `crates/autotune-agent/Cargo.toml` | Add `autotune-config` dependency |
| Modify | `crates/autotune-agent/src/lib.rs` | Add protocol module re-export |
| Create | `crates/autotune-agent/src/protocol.rs` | `AgentRequest`, `QuestionOption`, `ConfigSection`, `parse_agent_request()` |
| Create | `crates/autotune-agent/tests/protocol_test.rs` | Tests for JSON parsing of `AgentRequest` |
| Create | `crates/autotune-init/Cargo.toml` | New crate manifest |
| Create | `crates/autotune-init/src/lib.rs` | `run_init()`, conversation loop, prompt builder, section accumulator |
| Create | `crates/autotune-init/src/error.rs` | `InitError` type |
| Create | `crates/autotune-init/src/prompt.rs` | System prompt construction |
| Create | `crates/autotune-init/tests/init_test.rs` | Integration tests with MockAgent |
| Modify | `crates/autotune-mock/src/lib.rs` | Add init-mode support to MockAgent |
| Modify | `crates/autotune/Cargo.toml` | Add `autotune-init` dependency |
| Modify | `crates/autotune/src/main.rs` | Update `cmd_init` to call `run_init()` when config missing |

---

### Task 1: Add `Serialize` to config types

The protocol sends `ConfigSection` variants containing config types. The agent also produces JSON. Config types currently only derive `Deserialize` — we need `Serialize` too for TOML serialization of the final config.

**Files:**
- Modify: `crates/autotune-config/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/autotune-config/tests/config_test.rs`:

```rust
#[test]
fn roundtrip_serialize_deserialize() {
    let f = write_config(
        r#"
[experiment]
name = "roundtrip"
max_iterations = "10"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "b"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "m", pattern = "x" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "m", direction = "Maximize" }]
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    let serialized = toml::to_string_pretty(&config).unwrap();
    let reparsed: AutotuneConfig = toml::from_str(&serialized).unwrap();
    assert_eq!(reparsed.experiment.name, "roundtrip");
    assert_eq!(reparsed.benchmark.len(), 1);
}
```

Also add `use toml;` if not already imported. Note: this requires `toml` in `[dev-dependencies]` of `autotune-config/Cargo.toml` — it's already in `[dependencies]`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p autotune-config -E 'test(roundtrip_serialize)'`
Expected: FAIL — `Serialize` is not implemented for `AutotuneConfig`.

- [ ] **Step 3: Add `Serialize` derive to all config types**

In `crates/autotune-config/src/lib.rs`, change every `#[derive(Debug, Clone, Deserialize)]` to `#[derive(Debug, Clone, Serialize, Deserialize)]`. Also add `Serialize` to the import: change `use serde::Deserialize;` to `use serde::{Deserialize, Serialize};`.

Types to update (all of them):
- `AutotuneConfig` (line 8)
- `ExperimentConfig` (line 20)
- `PathsConfig` (line 62)
- `TestConfig` (line 69)
- `BenchmarkConfig` (line 81)
- `AdaptorConfig` (line 94)
- `RegexPattern` (line 105)
- `ScoreConfig` (line 111)
- `PrimaryMetric` (line 128)
- `GuardrailMetric` (line 140)
- `Direction` (line 147)
- `ThresholdCondition` (line 153)
- `AgentConfig` (line 160)
- `AgentRoleConfig` (line 187)

For `StopValue` (line 40), change `#[derive(Debug, Clone)]` to `#[derive(Debug, Clone, Serialize)]` and add a custom `Serialize` impl:

```rust
impl Serialize for StopValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            StopValue::Finite(n) => serializer.serialize_str(&n.to_string()),
            StopValue::Infinite => serializer.serialize_str("inf"),
        }
    }
}
```

Remove the `Serialize` from the derive since we're implementing it manually:

```rust
#[derive(Debug, Clone)]
pub enum StopValue {
    Finite(u64),
    Infinite,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p autotune-config -E 'test(roundtrip_serialize)'`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo nextest run -p autotune-config`
Expected: All tests pass (existing + new).

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-config/src/lib.rs crates/autotune-config/tests/config_test.rs
git commit -m "feat(config): add Serialize derives to all config types"
```

---

### Task 2: Add `GlobalConfig` to `autotune-config`

**Files:**
- Create: `crates/autotune-config/src/global.rs`
- Modify: `crates/autotune-config/src/lib.rs` (add `pub mod global;` and re-export)
- Modify: `crates/autotune-config/Cargo.toml` (add `dirs` dependency)
- Create: `crates/autotune-config/tests/global_config_test.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/autotune-config/tests/global_config_test.rs`:

```rust
use autotune_config::global::GlobalConfig;
use std::io::Write;

#[test]
fn load_from_explicit_path() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
[agent]
backend = "claude"

[agent.init]
model = "opus"
"#,
    )
    .unwrap();

    let config = GlobalConfig::load_from(f.path()).unwrap();
    let agent = config.agent.unwrap();
    assert_eq!(agent.backend, "claude");
    let init = agent.init.unwrap();
    assert_eq!(init.model.as_deref(), Some("opus"));
}

#[test]
fn load_from_missing_file_returns_empty() {
    let config = GlobalConfig::load_from(std::path::Path::new("/nonexistent/config.toml")).unwrap();
    assert!(config.agent.is_none());
}

#[test]
fn merge_user_overrides_system() {
    let mut sys = tempfile::NamedTempFile::new().unwrap();
    sys.write_all(
        br#"
[agent]
backend = "claude"

[agent.init]
model = "sonnet"
"#,
    )
    .unwrap();

    let mut user = tempfile::NamedTempFile::new().unwrap();
    user.write_all(
        br#"
[agent]
backend = "claude"

[agent.init]
model = "opus"
"#,
    )
    .unwrap();

    let config = GlobalConfig::load_layered(&[sys.path(), user.path()]).unwrap();
    let agent = config.agent.unwrap();
    let init = agent.init.unwrap();
    assert_eq!(init.model.as_deref(), Some("opus"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p autotune-config -E 'test(global)'`
Expected: FAIL — module `global` doesn't exist.

- [ ] **Step 3: Add `dirs` dependency**

In `crates/autotune-config/Cargo.toml`, add to `[dependencies]`:

```toml
dirs = "6"
```

- [ ] **Step 4: Create `global.rs`**

Create `crates/autotune-config/src/global.rs`:

```rust
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{AgentConfig, ConfigError};

/// Global (user/system) config. Only agent defaults — project-specific
/// settings live in `.autotune.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub agent: Option<AgentConfig>,
}

impl GlobalConfig {
    /// Load from the standard system → user config paths.
    /// Missing files are silently skipped.
    pub fn load() -> Result<Self, ConfigError> {
        let paths = Self::config_paths();
        let path_refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        Self::load_layered(&path_refs)
    }

    /// Load from a single explicit path. Returns empty config if file is missing.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        Self::load_layered(&[path])
    }

    /// Load from an ordered list of paths (earlier = lower priority).
    /// Missing files are silently skipped.
    pub fn load_layered(paths: &[&Path]) -> Result<Self, ConfigError> {
        let mut result = GlobalConfig::default();
        for path in paths {
            if path.exists() {
                let content = std::fs::read_to_string(path).map_err(|source| ConfigError::Io { source })?;
                let layer: GlobalConfig = toml::from_str(&content)?;
                result = result.merge(layer);
            }
        }
        Ok(result)
    }

    /// Standard config file paths: system then user.
    fn config_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // System config
        paths.push(PathBuf::from("/etc/autotune/config.toml"));

        // User config (XDG)
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("autotune").join("config.toml"));
        }

        paths
    }

    /// Merge another GlobalConfig on top of self (other wins on conflicts).
    fn merge(self, other: GlobalConfig) -> GlobalConfig {
        GlobalConfig {
            agent: match (self.agent, other.agent) {
                (_, Some(other_agent)) => Some(other_agent),
                (some, None) => some,
            },
        }
    }
}
```

- [ ] **Step 5: Add module declaration and re-export**

In `crates/autotune-config/src/lib.rs`, add after line 1 (`pub use error::ConfigError;`):

```rust
pub mod global;
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo nextest run -p autotune-config -E 'test(global)'`
Expected: All 3 tests pass.

- [ ] **Step 7: Run full config test suite**

Run: `cargo nextest run -p autotune-config`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/autotune-config/Cargo.toml crates/autotune-config/src/global.rs crates/autotune-config/src/lib.rs crates/autotune-config/tests/global_config_test.rs
git commit -m "feat(config): add GlobalConfig for system/user agent defaults"
```

---

### Task 3: Add protocol types to `autotune-agent`

The agent request/response protocol that the init agent (and later the research agent) uses to communicate structured requests to the CLI.

**Files:**
- Create: `crates/autotune-agent/src/protocol.rs`
- Modify: `crates/autotune-agent/src/lib.rs` (add `pub mod protocol;`)
- Modify: `crates/autotune-agent/Cargo.toml` (add `autotune-config` dependency)
- Create: `crates/autotune-agent/tests/protocol_test.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/autotune-agent/tests/protocol_test.rs`:

```rust
use autotune_agent::protocol::{parse_agent_request, AgentRequest, ConfigSection};

#[test]
fn parse_message_request() {
    let json = r#"{"type":"message","text":"Hello, I found some benchmarks."}"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Message { text } => {
            assert_eq!(text, "Hello, I found some benchmarks.");
        }
        _ => panic!("expected Message"),
    }
}

#[test]
fn parse_question_request() {
    let json = r#"{
        "type": "question",
        "text": "What type of project is this?",
        "options": [
            {"key": "a", "description": "Rust library"},
            {"key": "b", "description": "Python package"}
        ],
        "allow_free_response": true
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Question { text, options, allow_free_response } => {
            assert_eq!(text, "What type of project is this?");
            assert_eq!(options.len(), 2);
            assert_eq!(options[0].key, "a");
            assert!(allow_free_response);
        }
        _ => panic!("expected Question"),
    }
}

#[test]
fn parse_config_experiment_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "experiment",
            "name": "my-experiment",
            "max_iterations": "10",
            "canonical_branch": "main"
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            match section {
                ConfigSection::Experiment(exp) => {
                    assert_eq!(exp.name, "my-experiment");
                }
                _ => panic!("expected Experiment section"),
            }
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_paths_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "paths",
            "tunable": ["src/**/*.rs"],
            "denied": []
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            match section {
                ConfigSection::Paths(paths) => {
                    assert_eq!(paths.tunable, vec!["src/**/*.rs"]);
                }
                _ => panic!("expected Paths section"),
            }
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_test_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "test",
            "name": "rust",
            "command": ["cargo", "test"]
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            assert!(matches!(section, ConfigSection::Test(_)));
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_benchmark_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "benchmark",
            "name": "perf",
            "command": ["cargo", "bench"],
            "adaptor": {
                "type": "regex",
                "patterns": [{"name": "time_us", "pattern": "time:\\s+([0-9.]+)"}]
            }
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            assert!(matches!(section, ConfigSection::Benchmark(_)));
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_score_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "score",
            "value": {
                "type": "weighted_sum",
                "primary_metrics": [{"name": "time_us", "direction": "Minimize"}]
            }
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            assert!(matches!(section, ConfigSection::Score(_)));
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_request_with_surrounding_prose() {
    let response = r#"
I've analyzed your project. Here's my suggestion:

{"type":"message","text":"This looks like a Rust project with Criterion benchmarks."}

Let me know if you'd like to proceed.
"#;
    let req = parse_agent_request(response).unwrap();
    assert!(matches!(req, AgentRequest::Message { .. }));
}

#[test]
fn parse_request_no_json_errors() {
    let response = "I couldn't figure out what to do.";
    let err = parse_agent_request(response).unwrap_err();
    assert!(err.to_string().contains("no valid JSON"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p autotune-agent -E 'test(protocol)'`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Add `autotune-config` dependency to `autotune-agent`**

In `crates/autotune-agent/Cargo.toml`, add to `[dependencies]`:

```toml
autotune-config = { path = "../autotune-config" }
```

- [ ] **Step 4: Create `protocol.rs`**

Create `crates/autotune-agent/src/protocol.rs`:

```rust
use serde::{Deserialize, Serialize};

use autotune_config::{
    AgentConfig as AgentSectionConfig, BenchmarkConfig, ExperimentConfig, PathsConfig, ScoreConfig,
    TestConfig,
};

use crate::AgentError;

/// A structured request from the agent to the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    /// Free-form text to the user. User responds naturally.
    Message { text: String },

    /// Structured question with specific options.
    Question {
        text: String,
        options: Vec<QuestionOption>,
        #[serde(default)]
        allow_free_response: bool,
    },

    /// Propose a config section for validation.
    Config { section: ConfigSection },
}

/// An option in a structured question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub key: String,
    pub description: String,
}

/// A section of the autotune config, proposed incrementally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigSection {
    Experiment(ExperimentConfig),
    Paths(PathsConfig),
    Test(TestConfig),
    Benchmark(BenchmarkConfig),
    Score { value: ScoreConfig },
    Agent(AgentSectionConfig),
}

/// Parse an `AgentRequest` from an agent response that may contain surrounding prose.
/// Uses the same brace-depth scanning pattern as `parse_hypothesis` in `autotune-plan`.
pub fn parse_agent_request(response: &str) -> Result<AgentRequest, AgentError> {
    let mut depth = 0i32;
    let mut start = None;

    for (i, ch) in response.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        let candidate = &response[s..=i];
                        if let Ok(request) = serde_json::from_str::<AgentRequest>(candidate) {
                            return Ok(request);
                        }
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }

    Err(AgentError::ParseFailed {
        message: "no valid JSON agent request found in response".to_string(),
    })
}
```

- [ ] **Step 5: Add module declaration**

In `crates/autotune-agent/src/lib.rs`, add after line 1 (`pub mod claude;`):

```rust
pub mod protocol;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p autotune-agent -E 'test(protocol)'`
Expected: All 9 tests pass.

- [ ] **Step 7: Run full agent test suite**

Run: `cargo nextest run -p autotune-agent`
Expected: All tests pass.

- [ ] **Step 8: Lint check**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings. Note: the circular dependency concern — `autotune-agent` now depends on `autotune-config` — is fine because `autotune-config` is a leaf crate with no workspace dependencies.

- [ ] **Step 9: Commit**

```bash
git add crates/autotune-agent/Cargo.toml crates/autotune-agent/src/protocol.rs crates/autotune-agent/src/lib.rs crates/autotune-agent/tests/protocol_test.rs
git commit -m "feat(agent): add agent request/response protocol types"
```

---

### Task 4: Add init-mode support to MockAgent

Extend `MockAgent` so it can simulate the init conversation: cycling through a sequence of `AgentRequest` JSON responses.

**Files:**
- Modify: `crates/autotune-mock/Cargo.toml` (add `autotune-config` dependency)
- Modify: `crates/autotune-mock/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to a new file `crates/autotune-mock/tests/mock_init_test.rs`:

```rust
use autotune_agent::protocol::{parse_agent_request, AgentRequest};
use autotune_agent::{Agent, AgentConfig, ToolPermission};
use autotune_mock::MockAgent;
use std::path::PathBuf;

#[test]
fn mock_agent_init_conversation() {
    let agent = MockAgent::builder()
        .init_response(r#"{"type":"message","text":"I see a Rust project."}"#)
        .init_response(r#"{"type":"question","text":"Pick a name","options":[{"key":"a","description":"my-exp"}],"allow_free_response":false}"#)
        .build();

    let config = AgentConfig {
        prompt: "init prompt".to_string(),
        allowed_tools: vec![ToolPermission::Allow("Read".to_string())],
        working_directory: PathBuf::from("/tmp"),
        model: None,
        max_turns: None,
    };

    // spawn returns first init_response
    let resp = agent.spawn(&config).unwrap();
    let req = parse_agent_request(&resp.text).unwrap();
    assert!(matches!(req, AgentRequest::Message { .. }));

    // send returns second init_response
    let session = autotune_agent::AgentSession {
        session_id: resp.session_id.clone(),
        backend: "mock".to_string(),
    };
    let resp2 = agent.send(&session, "sounds good").unwrap();
    let req2 = parse_agent_request(&resp2.text).unwrap();
    assert!(matches!(req2, AgentRequest::Question { .. }));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p autotune-mock -E 'test(mock_agent_init)'`
Expected: FAIL — `init_response` method doesn't exist on `MockAgentBuilder`.

- [ ] **Step 3: Add init mode to MockAgent**

In `crates/autotune-mock/src/lib.rs`, add an `init_responses` field to `MockAgent` and `MockAgentBuilder`:

Add to `MockAgent` struct (after `impl_behavior` field):

```rust
    init_responses: Vec<String>,
```

Add to `MockAgentBuilder` struct:

```rust
    init_responses: Vec<String>,
```

Add builder method to `MockAgentBuilder` impl:

```rust
    /// Queue a raw JSON response for the init conversation.
    /// Responses are returned in order: first from `spawn()`, then from `send()` calls.
    pub fn init_response(mut self, json: &str) -> Self {
        self.init_responses.push(json.to_string());
        self
    }
```

Update `MockAgentBuilder::build()` to pass `init_responses`:

```rust
    pub fn build(self) -> MockAgent {
        MockAgent {
            hypotheses: self.hypotheses,
            impl_behavior: self.impl_behavior,
            init_responses: self.init_responses,
            spawn_count: Mutex::new(0),
            send_count: Mutex::new(0),
            last_spawn_config: Mutex::new(None),
            last_send_message: Mutex::new(None),
        }
    }
```

Update `MockAgent::builder()` to initialize `init_responses`:

```rust
    pub fn builder() -> MockAgentBuilder {
        MockAgentBuilder {
            hypotheses: Vec::new(),
            impl_behavior: ImplBehavior::CommitDummy,
            init_responses: Vec::new(),
        }
    }
```

Update `Agent::spawn()` impl — when `init_responses` is non-empty and this is a non-worktree spawn, return the first init response instead of "ready":

```rust
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        let mut count = self.spawn_count.lock().unwrap();
        let idx = *count;
        *count += 1;
        drop(count);

        *self.last_spawn_config.lock().unwrap() = Some(config.clone());

        let wd = &config.working_directory;
        let is_worktree = wd.join(".git").is_file();

        if idx == 0 && !is_worktree {
            // Init mode: return first init_response if available
            let text = if !self.init_responses.is_empty() {
                self.init_responses[0].clone()
            } else {
                "ready".to_string()
            };
            return Ok(AgentResponse {
                text,
                session_id: "mock-session-001".to_string(),
            });
        }

        // Implementation spawn (unchanged)
        match &self.impl_behavior {
            ImplBehavior::CommitDummy => {
                create_dummy_commit(wd, idx);
            }
            ImplBehavior::NoCommit => {}
            ImplBehavior::Custom(f) => {
                f(wd);
            }
        }

        Ok(AgentResponse {
            text: "implementation done".to_string(),
            session_id: "mock-session-001".to_string(),
        })
    }
```

Update `Agent::send()` impl — when `init_responses` is non-empty, cycle through init_responses (offset by 1 since spawn consumed index 0):

```rust
    fn send(&self, _session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError> {
        *self.last_send_message.lock().unwrap() = Some(message.to_string());

        let mut count = self.send_count.lock().unwrap();
        let idx = *count;
        *count += 1;
        drop(count);

        // Init mode: cycle through init_responses (offset by 1 for spawn)
        if !self.init_responses.is_empty() {
            let response_idx = (idx + 1) % self.init_responses.len();
            return Ok(AgentResponse {
                text: self.init_responses[response_idx].clone(),
                session_id: "mock-session-001".to_string(),
            });
        }

        // Research mode: cycle through hypotheses
        let hyp_idx = idx % self.hypotheses.len().max(1);

        if self.hypotheses.is_empty() {
            return Ok(AgentResponse {
                text: r#"{"approach":"default","hypothesis":"no hypothesis configured","files_to_modify":[]}"#.to_string(),
                session_id: "mock-session-001".to_string(),
            });
        }

        let entry = &self.hypotheses[hyp_idx];
        let json = serde_json::json!({
            "approach": entry.approach,
            "hypothesis": entry.hypothesis,
            "files_to_modify": entry.files_to_modify,
        });

        Ok(AgentResponse {
            text: json.to_string(),
            session_id: "mock-session-001".to_string(),
        })
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p autotune-mock -E 'test(mock_agent_init)'`
Expected: PASS

- [ ] **Step 5: Run full test suite to check nothing broke**

Run: `cargo nextest run`
Expected: All tests pass — existing MockAgent usage (research/impl mode) still works because `init_responses` defaults to empty.

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-mock/src/lib.rs crates/autotune-mock/tests/mock_init_test.rs
git commit -m "feat(mock): add init conversation mode to MockAgent"
```

---

### Task 5: Create `autotune-init` crate — error types and prompt builder

**Files:**
- Create: `crates/autotune-init/Cargo.toml`
- Create: `crates/autotune-init/src/lib.rs`
- Create: `crates/autotune-init/src/error.rs`
- Create: `crates/autotune-init/src/prompt.rs`
- Create: `crates/autotune-init/tests/prompt_test.rs`

- [ ] **Step 1: Create `Cargo.toml`**

Create `crates/autotune-init/Cargo.toml`:

```toml
[package]
name = "autotune-init"
version = "0.1.0"
edition = "2024"

[dependencies]
autotune-agent = { path = "../autotune-agent" }
autotune-config = { path = "../autotune-config" }
thiserror = "2"
serde_json = "1"
toml = "0.8"

[dev-dependencies]
autotune-mock = { path = "../autotune-mock" }
tempfile = "3"
```

- [ ] **Step 2: Create `error.rs`**

Create `crates/autotune-init/src/error.rs`:

```rust
use autotune_agent::AgentError;
use autotune_config::ConfigError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InitError {
    #[error("agent error: {source}")]
    Agent {
        #[from]
        source: AgentError,
    },

    #[error("config validation error: {source}")]
    Config {
        #[from]
        source: ConfigError,
    },

    #[error("user aborted init")]
    UserAborted,

    #[error("agent failed to produce valid request after retry: {message}")]
    ProtocolFailure { message: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
```

- [ ] **Step 3: Create `prompt.rs`**

Create `crates/autotune-init/src/prompt.rs`:

```rust
use std::path::Path;

/// Build the system prompt for the init agent.
///
/// Includes the protocol schema, config section descriptions, and
/// instructions for exploring the codebase before proposing config.
pub fn build_init_prompt(repo_root: &Path) -> String {
    format!(
        r#"You are an autotune init agent. Your job is to help the user configure autotune for their project by exploring the codebase and asking questions.

## Repo Root
{repo_root}

## Protocol
You MUST respond with exactly one JSON object per message. The JSON must match one of these schemas:

### Message — free-form text to the user
```json
{{"type": "message", "text": "your message here"}}
```

### Question — structured question with options
```json
{{
  "type": "question",
  "text": "your question",
  "options": [{{"key": "a", "description": "option A"}}, {{"key": "b", "description": "option B"}}],
  "allow_free_response": true
}}
```

### Config — propose a config section for validation
```json
{{"type": "config", "section": {{...}}}}
```

## Config Sections
Propose sections one at a time. The CLI validates each immediately.

### experiment (required)
```json
{{"type": "config", "section": {{"type": "experiment", "name": "experiment-name", "description": "what to optimize", "canonical_branch": "main", "max_iterations": "10"}}}}
```
- `name`: short kebab-case name (required)
- `description`: what the experiment optimizes (optional)
- `canonical_branch`: branch to cherry-pick improvements onto (default "main")
- Stop conditions (at least one required): `max_iterations` ("10" or "inf"), `target_improvement` (float), `max_duration` ("4h")

### paths (required)
```json
{{"type": "config", "section": {{"type": "paths", "tunable": ["src/**/*.rs"], "denied": []}}}}
```
- `tunable`: glob patterns for files the implementation agent can modify (required, non-empty)
- `denied`: glob patterns the agent cannot read (optional)

### test (optional, one per test suite)
```json
{{"type": "config", "section": {{"type": "test", "name": "rust", "command": ["cargo", "test"]}}}}
```
- `name`: identifier for this test suite
- `command`: shell command as array of strings
- `timeout`: seconds (default 300)

### benchmark (required, at least one)
```json
{{"type": "config", "section": {{"type": "benchmark", "name": "perf", "command": ["cargo", "bench"], "adaptor": {{"type": "regex", "patterns": [{{"name": "time_us", "pattern": "time:\\s+([0-9.]+)\\s+µs"}}]}}}}}}
```
- `name`: identifier for this benchmark
- `command`: shell command as array of strings
- `timeout`: seconds (default 600)
- `adaptor`: how to extract metrics. Types:
  - `regex`: `{{"type": "regex", "patterns": [{{"name": "metric_name", "pattern": "regex_with_capture_group"}}]}}`
  - `criterion`: `{{"type": "criterion", "benchmark_name": "bench_name"}}`
  - `script`: `{{"type": "script", "command": ["python", "extract.py"]}}`

### score (required)
```json
{{"type": "config", "section": {{"type": "score", "value": {{"type": "weighted_sum", "primary_metrics": [{{"name": "time_us", "direction": "Minimize"}}]}}}}}}
```
- `value.type`: "weighted_sum", "threshold", "script", or "command"
- For weighted_sum: `primary_metrics` (name, direction, optional weight) and optional `guardrail_metrics` (name, direction, max_regression)
- For threshold: `conditions` (metric, direction, threshold)
- For script/command: `command` array
- Direction values: "Minimize" or "Maximize"
- Metric names must match names produced by benchmark adaptors

### agent (optional)
```json
{{"type": "config", "section": {{"type": "agent", "backend": "claude", "research": {{"model": "opus"}}, "implementation": {{"model": "sonnet"}}}}}}
```

## Instructions
1. First, use your read tools (Read, Glob, Grep) to explore the project structure — look for existing benchmarks, test commands, Cargo.toml/package.json, CI config, build systems.
2. Start the conversation with a Message summarizing what you found.
3. Ask Questions to understand what the user wants to optimize.
4. Propose config sections in this order: experiment → paths → tests → benchmarks → score.
5. If the CLI reports a validation error, correct the section and re-propose it.
6. Keep the conversation focused and efficient."#,
        repo_root = repo_root.display()
    )
}
```

- [ ] **Step 4: Create `lib.rs` stub**

Create `crates/autotune-init/src/lib.rs`:

```rust
mod error;
mod prompt;

pub use error::InitError;
pub use prompt::build_init_prompt;
```

- [ ] **Step 5: Write prompt test**

Create `crates/autotune-init/tests/prompt_test.rs`:

```rust
use autotune_init::build_init_prompt;
use std::path::Path;

#[test]
fn prompt_contains_repo_root() {
    let prompt = build_init_prompt(Path::new("/home/user/myproject"));
    assert!(prompt.contains("/home/user/myproject"));
}

#[test]
fn prompt_contains_protocol_schema() {
    let prompt = build_init_prompt(Path::new("/tmp"));
    assert!(prompt.contains(r#""type": "message""#));
    assert!(prompt.contains(r#""type": "question""#));
    assert!(prompt.contains(r#""type": "config""#));
}

#[test]
fn prompt_contains_section_descriptions() {
    let prompt = build_init_prompt(Path::new("/tmp"));
    assert!(prompt.contains("experiment (required)"));
    assert!(prompt.contains("paths (required)"));
    assert!(prompt.contains("benchmark (required"));
    assert!(prompt.contains("score (required)"));
    assert!(prompt.contains("test (optional"));
    assert!(prompt.contains("agent (optional)"));
}
```

- [ ] **Step 6: Run tests**

Run: `cargo nextest run -p autotune-init`
Expected: All 3 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/autotune-init/
git commit -m "feat(init): create autotune-init crate with error types and prompt builder"
```

---

### Task 6: Implement the conversation loop in `autotune-init`

The core `run_init()` function that drives the agent conversation, accumulates validated config sections, and returns a complete `AutotuneConfig`.

**Files:**
- Modify: `crates/autotune-init/src/lib.rs`
- Create: `crates/autotune-init/tests/init_test.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/autotune-init/tests/init_test.rs`:

```rust
use autotune_config::global::GlobalConfig;
use autotune_init::{run_init, InitError};
use autotune_mock::MockAgent;
use std::path::PathBuf;

/// Helper: build a MockAgent that walks through a complete init conversation.
fn complete_init_agent() -> MockAgent {
    MockAgent::builder()
        // spawn: agent greets user
        .init_response(r#"{"type":"message","text":"I found a Rust project with Cargo.toml."}"#)
        // send 1: agent proposes experiment section
        .init_response(r#"{"type":"config","section":{"type":"experiment","name":"test-exp","max_iterations":"10","canonical_branch":"main"}}"#)
        // send 2: agent proposes paths section
        .init_response(r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#)
        // send 3: agent proposes benchmark section
        .init_response(r#"{"type":"config","section":{"type":"benchmark","name":"bench1","command":["cargo","bench"],"adaptor":{"type":"regex","patterns":[{"name":"time_us","pattern":"time:\\s+([0-9.]+)"}]}}}"#)
        // send 4: agent proposes score section
        .init_response(r#"{"type":"config","section":{"type":"score","value":{"type":"weighted_sum","primary_metrics":[{"name":"time_us","direction":"Minimize"}]}}}"#)
        .build()
}

#[test]
fn run_init_complete_conversation() {
    let agent = complete_init_agent();
    let global = GlobalConfig::default();

    // Use a fake input provider that always says "yes"
    // (handles both conversation replies and final approval)
    let config = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        || Ok("yes".to_string()),
    )
    .unwrap();

    assert_eq!(config.experiment.name, "test-exp");
    assert_eq!(config.paths.tunable, vec!["src/**"]);
    assert_eq!(config.benchmark.len(), 1);
    assert_eq!(config.benchmark[0].name, "bench1");
}

#[test]
fn run_init_missing_required_sections_keeps_going() {
    // Agent only proposes experiment and paths — no benchmark or score.
    // The loop should keep sending messages to the agent asking for more.
    // With MockAgent cycling, it will eventually re-send earlier responses.
    // This test verifies the loop doesn't hang — it terminates when it detects
    // a cycle (max turns exceeded).
    let agent = MockAgent::builder()
        .init_response(r#"{"type":"config","section":{"type":"experiment","name":"test-exp","max_iterations":"10"}}"#)
        .init_response(r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#)
        .build();

    let global = GlobalConfig::default();
    let result = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        || Ok("yes".to_string()),
    );

    // Should error because we never get benchmark + score sections
    assert!(result.is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p autotune-init -E 'test(run_init)'`
Expected: FAIL — `run_init` function doesn't exist.

- [ ] **Step 3: Implement `run_init` and section accumulator**

Update `crates/autotune-init/src/lib.rs`:

```rust
mod error;
mod prompt;

pub use error::InitError;
pub use prompt::build_init_prompt;

use autotune_agent::protocol::{parse_agent_request, AgentRequest, ConfigSection};
use autotune_agent::{Agent, AgentConfig, AgentSession, ToolPermission};
use autotune_config::global::GlobalConfig;
use autotune_config::{
    AutotuneConfig, BenchmarkConfig, ExperimentConfig, PathsConfig, ScoreConfig, TestConfig,
};

use std::path::Path;

/// Maximum conversation turns before giving up.
const MAX_TURNS: usize = 50;

/// Accumulated config sections during the init conversation.
#[derive(Clone, Default)]
struct ConfigAccumulator {
    experiment: Option<ExperimentConfig>,
    paths: Option<PathsConfig>,
    tests: Vec<TestConfig>,
    benchmarks: Vec<BenchmarkConfig>,
    score: Option<ScoreConfig>,
    agent: Option<autotune_config::AgentConfig>,
}

impl ConfigAccumulator {
    fn is_complete(&self) -> bool {
        self.experiment.is_some()
            && self.paths.is_some()
            && !self.benchmarks.is_empty()
            && self.score.is_some()
    }

    /// Render a TOML preview of the current accumulated config for user approval.
    fn assemble_preview(&self) -> String {
        // Build a partial config for display
        if let Some(config) = self.clone_assemble() {
            toml::to_string_pretty(&config).unwrap_or_else(|_| "failed to render preview".to_string())
        } else {
            "incomplete config".to_string()
        }
    }

    fn clone_assemble(&self) -> Option<AutotuneConfig> {
        Some(AutotuneConfig {
            experiment: self.experiment.clone()?,
            paths: self.paths.clone()?,
            test: self.tests.clone(),
            benchmark: if self.benchmarks.is_empty() { return None } else { self.benchmarks.clone() },
            score: self.score.clone()?,
            agent: self.agent.clone().unwrap_or_default(),
        })
    }

    fn missing_sections(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.experiment.is_none() {
            missing.push("experiment");
        }
        if self.paths.is_none() {
            missing.push("paths");
        }
        if self.benchmarks.is_empty() {
            missing.push("benchmark (at least one)");
        }
        if self.score.is_none() {
            missing.push("score");
        }
        missing
    }

    /// Try to assemble a complete AutotuneConfig. Returns None if required sections are missing.
    fn assemble(self) -> Option<AutotuneConfig> {
        let experiment = self.experiment?;
        let paths = self.paths?;
        if self.benchmarks.is_empty() {
            return None;
        }
        let score = self.score?;
        let agent = self.agent.unwrap_or_default();

        Some(AutotuneConfig {
            experiment,
            paths,
            test: self.tests,
            benchmark: self.benchmarks,
            score,
            agent,
        })
    }
}

/// Validate a single config section against the accumulator's current state.
/// Returns Ok(description) on success or Err(message) on validation failure.
fn validate_section(
    section: &ConfigSection,
    acc: &ConfigAccumulator,
) -> Result<String, String> {
    match section {
        ConfigSection::Experiment(exp) => {
            if exp.name.is_empty() {
                return Err("experiment name must not be empty".to_string());
            }
            if exp.max_iterations.is_none()
                && exp.target_improvement.is_none()
                && exp.max_duration.is_none()
            {
                return Err(
                    "at least one stop condition required (max_iterations, target_improvement, or max_duration)".to_string(),
                );
            }
            Ok(format!("experiment '{}' accepted", exp.name))
        }
        ConfigSection::Paths(paths) => {
            if paths.tunable.is_empty() {
                return Err("paths.tunable must contain at least one glob pattern".to_string());
            }
            for pattern in &paths.tunable {
                globset::Glob::new(pattern).map_err(|e| {
                    format!("invalid tunable glob '{}': {}", pattern, e)
                })?;
            }
            for pattern in &paths.denied {
                globset::Glob::new(pattern).map_err(|e| {
                    format!("invalid denied glob '{}': {}", pattern, e)
                })?;
            }
            Ok("paths accepted".to_string())
        }
        ConfigSection::Test(test) => {
            if test.command.is_empty() {
                return Err(format!("test '{}' has empty command", test.name));
            }
            Ok(format!("test '{}' accepted", test.name))
        }
        ConfigSection::Benchmark(bench) => {
            if bench.command.is_empty() {
                return Err(format!("benchmark '{}' has empty command", bench.name));
            }
            // Check metric name uniqueness against accumulated benchmarks
            let new_names = adaptor_metric_names(&bench.adaptor);
            let existing_names: std::collections::HashSet<String> = acc
                .benchmarks
                .iter()
                .flat_map(|b| adaptor_metric_names(&b.adaptor))
                .collect();
            for name in &new_names {
                if existing_names.contains(name) {
                    return Err(format!("duplicate metric name '{}' across benchmarks", name));
                }
            }
            Ok(format!("benchmark '{}' accepted", bench.name))
        }
        ConfigSection::Score { value } => {
            // Validate that referenced metrics exist in accumulated benchmarks
            let metric_names: std::collections::HashSet<String> = acc
                .benchmarks
                .iter()
                .flat_map(|b| adaptor_metric_names(&b.adaptor))
                .collect();

            match value {
                ScoreConfig::WeightedSum {
                    primary_metrics,
                    guardrail_metrics,
                } => {
                    for pm in primary_metrics {
                        if !metric_names.contains(&pm.name) {
                            return Err(format!(
                                "primary metric '{}' not produced by any benchmark adaptor",
                                pm.name
                            ));
                        }
                    }
                    for gm in guardrail_metrics {
                        if !metric_names.contains(&gm.name) {
                            return Err(format!(
                                "guardrail metric '{}' not produced by any benchmark adaptor",
                                gm.name
                            ));
                        }
                    }
                }
                ScoreConfig::Threshold { conditions } => {
                    for c in conditions {
                        if !metric_names.contains(&c.metric) {
                            return Err(format!(
                                "threshold metric '{}' not produced by any benchmark adaptor",
                                c.metric
                            ));
                        }
                    }
                }
                ScoreConfig::Script { command } | ScoreConfig::Command { command } => {
                    if command.is_empty() {
                        return Err("score script/command must not be empty".to_string());
                    }
                }
            }
            Ok("score accepted".to_string())
        }
        ConfigSection::Agent(_) => Ok("agent config accepted".to_string()),
    }
}

/// Extract metric names that an adaptor config will produce.
fn adaptor_metric_names(adaptor: &autotune_config::AdaptorConfig) -> Vec<String> {
    match adaptor {
        autotune_config::AdaptorConfig::Regex { patterns } => {
            patterns.iter().map(|p| p.name.clone()).collect()
        }
        autotune_config::AdaptorConfig::Criterion { .. } => {
            vec![
                "mean".to_string(),
                "median".to_string(),
                "std_dev".to_string(),
            ]
        }
        autotune_config::AdaptorConfig::Script { .. } => vec![],
    }
}

/// Permissions for the init agent: read-only access.
fn init_agent_permissions() -> Vec<ToolPermission> {
    vec![
        ToolPermission::Allow("Read".to_string()),
        ToolPermission::Allow("Glob".to_string()),
        ToolPermission::Allow("Grep".to_string()),
    ]
}

/// Run the agent-assisted init conversation.
///
/// `read_user_input` is a closure that reads a line from the user.
/// This is injectable for testing (mock agents don't need real stdin).
pub fn run_init<F>(
    agent: &dyn Agent,
    global_config: &GlobalConfig,
    repo_root: &Path,
    read_user_input: F,
) -> Result<AutotuneConfig, InitError>
where
    F: Fn() -> Result<String, std::io::Error>,
{
    let prompt = build_init_prompt(repo_root);

    let model = global_config
        .agent
        .as_ref()
        .and_then(|a| a.init.as_ref())
        .and_then(|i| i.model.clone());

    let max_turns = global_config
        .agent
        .as_ref()
        .and_then(|a| a.init.as_ref())
        .and_then(|i| i.max_turns);

    let agent_config = AgentConfig {
        prompt,
        allowed_tools: init_agent_permissions(),
        working_directory: repo_root.to_path_buf(),
        model,
        max_turns,
    };

    // Spawn the init agent
    let response = agent.spawn(&agent_config)?;
    let session = AgentSession {
        session_id: response.session_id,
        backend: agent.backend_name().to_string(),
    };

    let mut acc = ConfigAccumulator::default();
    let mut last_response_text = response.text;
    let mut turns = 0;

    loop {
        if turns >= MAX_TURNS {
            return Err(InitError::ProtocolFailure {
                message: format!(
                    "exceeded {} conversation turns. Still missing: {}",
                    MAX_TURNS,
                    acc.missing_sections().join(", ")
                ),
            });
        }
        turns += 1;

        let request = match parse_agent_request(&last_response_text) {
            Ok(req) => req,
            Err(_) => {
                // Retry once with corrective prompt
                let retry = agent.send(
                    &session,
                    "Your previous response was not valid JSON. Please respond with exactly one JSON object matching the protocol schema.",
                )?;
                match parse_agent_request(&retry.text) {
                    Ok(req) => req,
                    Err(e) => {
                        return Err(InitError::ProtocolFailure {
                            message: format!("agent failed to produce valid JSON after retry: {}", e),
                        });
                    }
                }
            }
        };

        let reply = match request {
            AgentRequest::Message { text } => {
                println!("\n{}", text);
                print!("> ");
                let input = read_user_input().map_err(InitError::Io)?;
                input
            }
            AgentRequest::Question {
                text,
                options,
                allow_free_response,
            } => {
                println!("\n{}", text);
                for opt in &options {
                    println!("  {}) {}", opt.key, opt.description);
                }
                if allow_free_response {
                    println!("  (or type a custom response)");
                }
                print!("> ");
                let input = read_user_input().map_err(InitError::Io)?;
                input
            }
            AgentRequest::Config { section } => {
                match validate_section(&section, &acc) {
                    Ok(msg) => {
                        // Accumulate the valid section
                        match section {
                            ConfigSection::Experiment(exp) => {
                                println!("[autotune] {}", msg);
                                acc.experiment = Some(exp);
                            }
                            ConfigSection::Paths(paths) => {
                                println!("[autotune] {}", msg);
                                acc.paths = Some(paths);
                            }
                            ConfigSection::Test(test) => {
                                println!("[autotune] {}", msg);
                                acc.tests.push(test);
                            }
                            ConfigSection::Benchmark(bench) => {
                                println!("[autotune] {}", msg);
                                acc.benchmarks.push(bench);
                            }
                            ConfigSection::Score { value } => {
                                println!("[autotune] {}", msg);
                                acc.score = Some(value);
                            }
                            ConfigSection::Agent(agent_cfg) => {
                                println!("[autotune] {}", msg);
                                acc.agent = Some(agent_cfg);
                            }
                        }

                        // Check if we have everything
                        if acc.is_complete() {
                            // Show assembled config for final approval
                            let preview = acc.assemble_preview();
                            println!("\n[autotune] All required sections collected. Proposed config:\n");
                            println!("{}", preview);
                            println!("\nApprove this config? (yes to write, or provide feedback)");
                            print!("> ");
                            let approval = read_user_input().map_err(InitError::Io)?;
                            let trimmed = approval.trim().to_lowercase();
                            if trimmed == "yes" || trimmed == "y" {
                                break;
                            }
                            // User rejected — send feedback to agent to revise
                            let response = agent.send(&session, &format!(
                                "User rejected the config with feedback: {}. Please revise the relevant sections.",
                                approval
                            ))?;
                            last_response_text = response.text;
                            continue;
                        }

                        let missing = acc.missing_sections();
                        format!(
                            "Section accepted. Still needed: {}. Please propose the next section.",
                            missing.join(", ")
                        )
                    }
                    Err(err) => {
                        println!("[autotune] validation error: {}", err);
                        format!("Validation error: {}. Please correct and re-propose.", err)
                    }
                }
            }
        };

        let response = agent.send(&session, &reply)?;
        last_response_text = response.text;
    }

    // Assemble and validate the full config
    let config = acc
        .assemble()
        .expect("is_complete() was true but assemble() returned None");

    // Run full validation as a final check
    config.validate().map_err(InitError::Config)?;

    Ok(config)
}
```

- [ ] **Step 4: Add globset dependency**

In `crates/autotune-init/Cargo.toml`, add to `[dependencies]`:

```toml
globset = "0.4"
```

- [ ] **Step 5: Update lib.rs exports**

The lib.rs from step 3 already has the full implementation. Make sure the `use` statements include everything needed.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p autotune-init`
Expected: All tests pass — `run_init_complete_conversation` succeeds, `run_init_missing_required_sections_keeps_going` errors as expected.

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/autotune-init/
git commit -m "feat(init): implement conversation loop and section accumulator"
```

---

### Task 7: Wire `autotune-init` into the binary crate

**Files:**
- Modify: `crates/autotune/Cargo.toml`
- Modify: `crates/autotune/src/main.rs`

- [ ] **Step 1: Add dependency**

In `crates/autotune/Cargo.toml`, add to `[dependencies]`:

```toml
autotune-init = { path = "../autotune-init" }
```

- [ ] **Step 2: Update `cmd_init` in `main.rs`**

In `crates/autotune/src/main.rs`, add to the imports at the top:

```rust
use autotune_config::global::GlobalConfig;
```

Replace the body of `cmd_init` (lines 441-522) with:

```rust
fn cmd_init(name_override: Option<String>) -> Result<()> {
    let repo_root = find_repo_root()?;
    let config_path = repo_root.join(".autotune.toml");

    let mut config = if config_path.exists() {
        load_config(&repo_root)?
    } else {
        // Agent-assisted init
        println!("[autotune] no .autotune.toml found — starting agent-assisted init");

        let global_config =
            GlobalConfig::load().context("failed to load global config")?;

        let agent = build_agent_from_global(&global_config);

        let config = autotune_init::run_init(&agent, &global_config, &repo_root, || {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            Ok(input.trim().to_string())
        })
        .context("agent-assisted init failed")?;

        // Write .autotune.toml
        let toml_content =
            toml::to_string_pretty(&config).context("failed to serialize config")?;
        std::fs::write(&config_path, &toml_content)
            .context("failed to write .autotune.toml")?;
        println!("[autotune] wrote .autotune.toml");

        config
    };

    if let Some(name) = name_override {
        config.experiment.name = name;
    }

    let experiment_dir = config.experiment_dir(&repo_root);
    if experiment_dir.exists() {
        bail!(
            "experiment '{}' already exists at {}. Use 'resume' to continue it.",
            config.experiment.name,
            experiment_dir.display()
        );
    }

    let store =
        ExperimentStore::new(&experiment_dir).context("failed to create experiment store")?;

    // Snapshot config
    let config_content = std::fs::read_to_string(&config_path).context("failed to read config")?;
    store
        .save_config_snapshot(&config_content)
        .context("failed to save config snapshot")?;

    // Run sanity tests
    if !config.test.is_empty() {
        println!("[autotune] running sanity tests...");
        let test_results = autotune_test::run_all_tests(&config.test, &repo_root)
            .context("sanity tests failed to execute")?;
        if !autotune_test::all_passed(&test_results) {
            let failed: Vec<_> = test_results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| r.name.as_str())
                .collect();
            bail!("sanity tests failed: {}", failed.join(", "));
        }
        println!("[autotune] sanity tests passed");
    }

    // Take baseline benchmarks
    println!("[autotune] running baseline benchmarks...");
    let baseline_metrics = autotune_benchmark::run_all_benchmarks(&config.benchmark, &repo_root)
        .context("baseline benchmarks failed")?;
    println!("[autotune] baseline metrics: {:?}", baseline_metrics);

    // Record baseline in ledger
    let baseline_record = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: baseline_metrics,
        rank: 0.0,
        score: None,
        reason: None,
        timestamp: Utc::now(),
    };
    store
        .append_ledger(&baseline_record)
        .context("failed to record baseline")?;

    println!();
    println!(
        "[autotune] experiment '{}' initialized",
        config.experiment.name
    );
    println!("[autotune] results at: {}", experiment_dir.display());
    println!("[autotune] run `autotune run` to start the tune loop or use step commands");

    Ok(())
}
```

- [ ] **Step 3: Add `build_agent_from_global` helper**

Add this function near the existing `build_agent` function in `main.rs`:

```rust
fn build_agent_from_global(_global_config: &GlobalConfig) -> Box<dyn Agent> {
    // Currently only the Claude backend is supported.
    // In the future, read global_config.agent.backend to select backend.
    Box::new(ClaudeAgent::new())
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`
Expected: Compiles without errors.

- [ ] **Step 5: Run full test suite**

Run: `cargo nextest run`
Expected: All tests pass.

- [ ] **Step 6: Run clippy and fmt**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: Clean.

- [ ] **Step 7: Commit**

```bash
git add crates/autotune/Cargo.toml crates/autotune/src/main.rs
git commit -m "feat: wire agent-assisted init into autotune CLI"
```

---

### Task 8: Final validation — end-to-end test

Write an integration test in the binary crate that exercises the full init flow with a MockAgent.

**Files:**
- Modify: `crates/autotune/tests/integration_test.rs` (or create new test file)

- [ ] **Step 1: Check existing integration test structure**

Read `crates/autotune/tests/integration_test.rs` to understand the test helpers (`init_temp_repo`, `write_config`, etc.).

- [ ] **Step 2: Write the integration test**

Add to `crates/autotune/tests/integration_test.rs` (or a new file `crates/autotune/tests/init_test.rs`):

```rust
use autotune_config::global::GlobalConfig;
use autotune_init::run_init;
use autotune_mock::MockAgent;
use std::path::PathBuf;

#[test]
fn agent_assisted_init_produces_valid_config() {
    let agent = MockAgent::builder()
        .init_response(r#"{"type":"message","text":"I see a Rust project."}"#)
        .init_response(
            r#"{"type":"config","section":{"type":"experiment","name":"perf-opt","description":"Optimize performance","max_iterations":"20","canonical_branch":"main"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**/*.rs"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"test","name":"rust","command":["cargo","test"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"benchmark","name":"bench1","command":["cargo","bench"],"adaptor":{"type":"regex","patterns":[{"name":"time_us","pattern":"time:\\s+([0-9.]+)"}]}}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"score","value":{"type":"weighted_sum","primary_metrics":[{"name":"time_us","direction":"Minimize"}]}}}"#,
        )
        .build();

    let global = GlobalConfig::default();
    let config = run_init(&agent, &global, &PathBuf::from("/tmp/fake"), || {
        Ok("ok".to_string())
    })
    .unwrap();

    // Verify all sections are present and correct
    assert_eq!(config.experiment.name, "perf-opt");
    assert_eq!(config.experiment.description.as_deref(), Some("Optimize performance"));
    assert_eq!(config.paths.tunable, vec!["src/**/*.rs"]);
    assert_eq!(config.test.len(), 1);
    assert_eq!(config.test[0].name, "rust");
    assert_eq!(config.benchmark.len(), 1);
    assert_eq!(config.benchmark[0].name, "bench1");

    // Verify the config serializes to valid TOML that roundtrips
    let toml_str = toml::to_string_pretty(&config).unwrap();
    let reparsed: autotune_config::AutotuneConfig = toml::from_str(&toml_str).unwrap();
    reparsed.validate().unwrap();
    assert_eq!(reparsed.experiment.name, "perf-opt");
}

#[test]
fn agent_assisted_init_validates_sections_incrementally() {
    // Agent proposes an invalid experiment (no stop condition), then a valid one
    let agent = MockAgent::builder()
        .init_response(
            r#"{"type":"config","section":{"type":"experiment","name":"test-exp"}}"#,
        )
        // After validation error, agent retries with stop condition
        .init_response(
            r#"{"type":"config","section":{"type":"experiment","name":"test-exp","max_iterations":"5"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"benchmark","name":"b","command":["echo"],"adaptor":{"type":"regex","patterns":[{"name":"m","pattern":"x"}]}}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"score","value":{"type":"weighted_sum","primary_metrics":[{"name":"m","direction":"Maximize"}]}}}"#,
        )
        .build();

    let global = GlobalConfig::default();
    let config = run_init(&agent, &global, &PathBuf::from("/tmp/fake"), || {
        Ok("ok".to_string())
    })
    .unwrap();

    assert_eq!(config.experiment.name, "test-exp");
}
```

- [ ] **Step 3: Add dev-dependencies if needed**

If testing in a new file under `crates/autotune/tests/`, ensure `Cargo.toml` has:

```toml
[dev-dependencies]
autotune-mock = { path = "../autotune-mock" }
autotune-init = { path = "../autotune-init" }
autotune-config = { path = "../autotune-config" }
tempfile = "3"
toml = "0.8"
```

(Some of these may already be present.)

- [ ] **Step 4: Run the integration tests**

Run: `cargo nextest run -p autotune -E 'test(agent_assisted_init)'`
Expected: Both tests pass.

- [ ] **Step 5: Run full test suite**

Run: `cargo nextest run`
Expected: All tests pass across all crates.

- [ ] **Step 6: Run full pre-commit checks**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run`
Expected: All clean.

- [ ] **Step 7: Commit**

```bash
git add crates/autotune/tests/ crates/autotune/Cargo.toml
git commit -m "test: add end-to-end integration tests for agent-assisted init"
```
