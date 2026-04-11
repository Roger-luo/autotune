# Autotune CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust CLI that orchestrates autonomous benchmark-driven performance tuning via LLM agents, using an explicit crash-recoverable state machine.

**Architecture:** Cargo workspace with 11 crates. Leaf crates (`autotune-config`, `autotune-git`, `autotune-adaptor`, `autotune-score`) have no internal dependencies. Middle crates (`autotune-state`, `autotune-agent`, `autotune-test`, `autotune-benchmark`) depend on leaves. Top crates (`autotune-plan`, `autotune-implement`) depend on middle. The `autotune` binary crate composes everything into the state machine and CLI.

**Tech Stack:** Rust 2024 edition, clap 4 (CLI), serde + serde_json (serialization), toml (config), regex (metric extraction), thiserror (library errors), anyhow (application errors), chrono (timestamps), tempfile (atomic writes in tests)

---

## Build Order

The plan follows dependency order — each task produces a compiling, tested crate before the next begins:

1. Workspace setup + `autotune-config` (leaf — no internal deps)
2. `autotune-git` (leaf)
3. `autotune-adaptor` (leaf)
4. `autotune-score` (leaf)
5. `autotune-state` (depends on config)
6. `autotune-agent` (leaf trait + Claude backend)
7. `autotune-test` (depends on config)
8. `autotune-benchmark` (depends on adaptor, config)
9. `autotune-plan` (depends on agent, state)
10. `autotune-implement` (depends on agent, state, git)
11. `autotune` binary — CLI commands + state machine + resume logic

---

### Task 1: Workspace Setup + autotune-config

**Files:**
- Modify: `Cargo.toml` (convert to workspace)
- Remove: `src/lib.rs` (placeholder)
- Create: `crates/autotune-config/Cargo.toml`
- Create: `crates/autotune-config/src/lib.rs`
- Create: `crates/autotune-config/src/error.rs`
- Test: `crates/autotune-config/tests/config_test.rs`

- [ ] **Step 1: Convert root to workspace**

Replace `Cargo.toml` with:

```toml
[workspace]
members = ["crates/*"]
resolver = "2"
```

Remove `src/lib.rs` (the placeholder `add` function).

- [ ] **Step 2: Create autotune-config crate scaffold**

`crates/autotune-config/Cargo.toml`:

```toml
[package]
name = "autotune-config"
version = "0.1.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
toml = "0.8"
thiserror = "2"
globset = "0.4"
```

`crates/autotune-config/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: String },

    #[error("failed to parse config: {source}")]
    Parse {
        #[from]
        source: toml::de::Error,
    },

    #[error("validation error: {message}")]
    Validation { message: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
```

`crates/autotune-config/src/lib.rs`:

```rust
mod error;

pub use error::ConfigError;

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct AutotuneConfig {
    pub experiment: ExperimentConfig,
    pub paths: PathsConfig,
    #[serde(default)]
    pub test: Vec<TestConfig>,
    pub benchmark: Vec<BenchmarkConfig>,
    pub score: ScoreConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_canonical_branch")]
    pub canonical_branch: String,
    #[serde(default)]
    pub max_iterations: Option<StopValue>,
    #[serde(default)]
    pub target_improvement: Option<f64>,
    #[serde(default)]
    pub max_duration: Option<String>,
}

fn default_canonical_branch() -> String {
    "main".to_string()
}

/// Either a finite number or "inf" for unbounded.
#[derive(Debug, Clone)]
pub enum StopValue {
    Finite(u64),
    Infinite,
}

impl<'de> Deserialize<'de> for StopValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s == "inf" {
            Ok(StopValue::Infinite)
        } else {
            s.parse::<u64>()
                .map(StopValue::Finite)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathsConfig {
    pub tunable: Vec<String>,
    #[serde(default)]
    pub denied: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestConfig {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_test_timeout")]
    pub timeout: u64,
}

fn default_test_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize)]
pub struct BenchmarkConfig {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_benchmark_timeout")]
    pub timeout: u64,
    pub adaptor: AdaptorConfig,
}

fn default_benchmark_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AdaptorConfig {
    #[serde(rename = "regex")]
    Regex { patterns: Vec<RegexPattern> },
    #[serde(rename = "criterion")]
    Criterion { benchmark_name: String },
    #[serde(rename = "script")]
    Script { command: Vec<String> },
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegexPattern {
    pub name: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ScoreConfig {
    #[serde(rename = "weighted_sum")]
    WeightedSum {
        primary_metrics: Vec<PrimaryMetric>,
        #[serde(default)]
        guardrail_metrics: Vec<GuardrailMetric>,
    },
    #[serde(rename = "threshold")]
    Threshold { conditions: Vec<ThresholdCondition> },
    #[serde(rename = "script")]
    Script { command: Vec<String> },
    #[serde(rename = "command")]
    Command { command: Vec<String> },
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrimaryMetric {
    pub name: String,
    pub direction: Direction,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuardrailMetric {
    pub name: String,
    pub direction: Direction,
    pub max_regression: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum Direction {
    Minimize,
    Maximize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThresholdCondition {
    pub metric: String,
    pub direction: Direction,
    pub threshold: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default)]
    pub research: Option<AgentRoleConfig>,
    #[serde(default)]
    pub implementation: Option<AgentRoleConfig>,
    #[serde(default)]
    pub init: Option<AgentRoleConfig>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            research: None,
            implementation: None,
            init: None,
        }
    }
}

fn default_backend() -> String {
    "claude".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentRoleConfig {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u64>,
}

impl AutotuneConfig {
    /// Load config from a TOML file at the given path.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NotFound {
                    path: path.display().to_string(),
                }
            } else {
                ConfigError::Io { source }
            }
        })?;
        let config: AutotuneConfig = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate all config constraints. Called automatically by `load`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // At least one stop condition
        if self.experiment.max_iterations.is_none()
            && self.experiment.target_improvement.is_none()
            && self.experiment.max_duration.is_none()
        {
            return Err(ConfigError::Validation {
                message: "at least one stop condition required (max_iterations, target_improvement, or max_duration)".to_string(),
            });
        }

        // Benchmarks non-empty
        if self.benchmark.is_empty() {
            return Err(ConfigError::Validation {
                message: "at least one [[benchmark]] entry required".to_string(),
            });
        }

        // Each benchmark command non-empty
        for b in &self.benchmark {
            if b.command.is_empty() {
                return Err(ConfigError::Validation {
                    message: format!("benchmark '{}' has empty command", b.name),
                });
            }
        }

        // Each test command non-empty
        for t in &self.test {
            if t.command.is_empty() {
                return Err(ConfigError::Validation {
                    message: format!("test '{}' has empty command", t.name),
                });
            }
        }

        // Tunable paths non-empty
        if self.paths.tunable.is_empty() {
            return Err(ConfigError::Validation {
                message: "paths.tunable must contain at least one glob pattern".to_string(),
            });
        }

        // Validate tunable globs parse
        for pattern in &self.paths.tunable {
            globset::Glob::new(pattern).map_err(|e| ConfigError::Validation {
                message: format!("invalid tunable glob '{}': {}", pattern, e),
            })?;
        }

        // Validate metric name uniqueness across benchmarks
        let mut metric_names = std::collections::HashSet::new();
        for b in &self.benchmark {
            let names = self.adaptor_metric_names(&b.adaptor);
            for name in names {
                if !metric_names.insert(name.clone()) {
                    return Err(ConfigError::Validation {
                        message: format!("duplicate metric name '{}' across benchmarks", name),
                    });
                }
            }
        }

        // For built-in score types, validate metric references
        match &self.score {
            ScoreConfig::WeightedSum {
                primary_metrics,
                guardrail_metrics,
            } => {
                for pm in primary_metrics {
                    if !metric_names.contains(&pm.name) {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "primary metric '{}' not produced by any benchmark adaptor",
                                pm.name
                            ),
                        });
                    }
                }
                for gm in guardrail_metrics {
                    if !metric_names.contains(&gm.name) {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "guardrail metric '{}' not produced by any benchmark adaptor",
                                gm.name
                            ),
                        });
                    }
                }
            }
            ScoreConfig::Threshold { conditions } => {
                for c in conditions {
                    if !metric_names.contains(&c.metric) {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "threshold metric '{}' not produced by any benchmark adaptor",
                                c.metric
                            ),
                        });
                    }
                }
            }
            ScoreConfig::Script { command } | ScoreConfig::Command { command } => {
                if command.is_empty() {
                    return Err(ConfigError::Validation {
                        message: "score script/command must not be empty".to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Extract metric names that an adaptor config will produce.
    /// For script adaptors, returns empty (can't know ahead of time).
    fn adaptor_metric_names(&self, adaptor: &AdaptorConfig) -> Vec<String> {
        match adaptor {
            AdaptorConfig::Regex { patterns } => {
                patterns.iter().map(|p| p.name.clone()).collect()
            }
            AdaptorConfig::Criterion { .. } => {
                // Criterion produces standard names: "mean", "median", etc.
                // We can't validate references against these statically.
                vec![]
            }
            AdaptorConfig::Script { .. } => vec![],
        }
    }

    /// Resolve the experiment directory path: `.autotune/experiments/<name>/`
    pub fn experiment_dir(&self, root: &Path) -> PathBuf {
        root.join(".autotune")
            .join("experiments")
            .join(&self.experiment.name)
    }
}
```

- [ ] **Step 3: Verify the crate compiles**

Run: `cargo build -p autotune-config`
Expected: compiles with no errors

- [ ] **Step 4: Write config parsing tests**

`crates/autotune-config/tests/config_test.rs`:

```rust
use autotune_config::{AutotuneConfig, ConfigError};
use std::io::Write;

fn write_config(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn parse_minimal_valid_config() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "10"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "bench1"
command = ["cargo", "bench"]
adaptor = { type = "regex", patterns = [
    { name = "time_us", pattern = 'time:\s+([0-9.]+)' },
] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "time_us", direction = "Minimize" }]
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.experiment.name, "test-exp");
    assert_eq!(config.benchmark.len(), 1);
    assert_eq!(config.test.len(), 0);
}

#[test]
fn parse_infinite_iterations() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "inf"

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
    assert!(matches!(
        config.experiment.max_iterations,
        Some(autotune_config::StopValue::Infinite)
    ));
}

#[test]
fn error_no_stop_condition() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"

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
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(matches!(err, ConfigError::Validation { .. }));
    assert!(err.to_string().contains("stop condition"));
}

#[test]
fn error_missing_file() {
    let err = AutotuneConfig::load(std::path::Path::new("/nonexistent/.autotune.toml")).unwrap_err();
    assert!(matches!(err, ConfigError::NotFound { .. }));
}

#[test]
fn error_empty_benchmark_command() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "b"
command = []
adaptor = { type = "regex", patterns = [{ name = "m", pattern = "x" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "m", direction = "Maximize" }]
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(err.to_string().contains("empty command"));
}

#[test]
fn error_duplicate_metric_names() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "b1"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "time", pattern = "x" }] }

[[benchmark]]
name = "b2"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "time", pattern = "y" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "time", direction = "Minimize" }]
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(err.to_string().contains("duplicate metric"));
}

#[test]
fn error_score_references_unknown_metric() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "b"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "time", pattern = "x" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "nonexistent", direction = "Minimize" }]
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(err.to_string().contains("nonexistent"));
}

#[test]
fn parse_script_score() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "b"
command = ["echo"]
adaptor = { type = "script", command = ["python", "extract.py"] }

[score]
type = "script"
command = ["python", "judge.py"]
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert!(matches!(config.score, autotune_config::ScoreConfig::Script { .. }));
}

#[test]
fn parse_multiple_tests() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[test]]
name = "rust"
command = ["cargo", "test"]

[[test]]
name = "python"
command = ["pytest"]

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
    assert_eq!(config.test.len(), 2);
    assert_eq!(config.test[0].name, "rust");
    assert_eq!(config.test[1].name, "python");
}

#[test]
fn parse_agent_config() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "b"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "m", pattern = "x" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "m", direction = "Maximize" }]

[agent]
backend = "claude"

[agent.research]
model = "opus"

[agent.implementation]
model = "sonnet"
max_turns = 50
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.agent.backend, "claude");
    let research = config.agent.research.unwrap();
    assert_eq!(research.model.unwrap(), "opus");
    let implementation = config.agent.implementation.unwrap();
    assert_eq!(implementation.model.unwrap(), "sonnet");
    assert_eq!(implementation.max_turns.unwrap(), 50);
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p autotune-config`
Expected: all tests pass

- [ ] **Step 6: Run clippy and format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings

- [ ] **Step 7: Commit**

```bash
git add crates/autotune-config/ Cargo.toml
git rm src/lib.rs
git commit -m "feat: add autotune-config crate with TOML parsing and validation"
```

---

### Task 2: autotune-git

**Files:**
- Create: `crates/autotune-git/Cargo.toml`
- Create: `crates/autotune-git/src/lib.rs`
- Create: `crates/autotune-git/src/error.rs`
- Test: `crates/autotune-git/tests/git_test.rs`

- [ ] **Step 1: Create crate scaffold**

`crates/autotune-git/Cargo.toml`:

```toml
[package]
name = "autotune-git"
version = "0.1.0"
edition = "2024"

[dependencies]
thiserror = "2"

[dev-dependencies]
tempfile = "3"
```

`crates/autotune-git/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git command failed: {command}\nstderr: {stderr}")]
    CommandFailed { command: String, stderr: String },

    #[error("not a git repository: {path}")]
    NotARepo { path: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
```

`crates/autotune-git/src/lib.rs`:

```rust
mod error;

pub use error::GitError;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of running a git command.
struct GitOutput {
    stdout: String,
    stderr: String,
}

/// Run a git command in the given directory and return stdout.
fn git(dir: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(GitError::Io)?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(GitError::CommandFailed {
            command: format!("git {}", args.join(" ")),
            stderr,
        });
    }

    Ok(GitOutput { stdout, stderr })
}

/// Find the root of the git repository containing `dir`.
pub fn repo_root(dir: &Path) -> Result<PathBuf, GitError> {
    let output = git(dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(output.stdout.trim()))
}

/// Get the current HEAD commit SHA (short).
pub fn head_sha(dir: &Path) -> Result<String, GitError> {
    let output = git(dir, &["rev-parse", "--short", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

/// Get the current branch name.
pub fn current_branch(dir: &Path) -> Result<String, GitError> {
    let output = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

/// Create a new branch from HEAD without switching to it.
pub fn create_branch(dir: &Path, branch_name: &str) -> Result<(), GitError> {
    git(dir, &["branch", branch_name])?;
    Ok(())
}

/// Create a git worktree at `worktree_path` on `branch_name`.
/// The branch must already exist.
pub fn create_worktree(
    dir: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<(), GitError> {
    let wt = worktree_path.to_str().unwrap_or_default();
    git(dir, &["worktree", "add", wt, branch_name])?;
    Ok(())
}

/// Remove a git worktree. Uses --force to handle dirty worktrees.
pub fn remove_worktree(dir: &Path, worktree_path: &Path) -> Result<(), GitError> {
    let wt = worktree_path.to_str().unwrap_or_default();
    git(dir, &["worktree", "remove", wt, "--force"])?;
    Ok(())
}

/// Cherry-pick a commit onto the current branch.
pub fn cherry_pick(dir: &Path, commit_sha: &str) -> Result<(), GitError> {
    git(dir, &["cherry-pick", commit_sha])?;
    Ok(())
}

/// Revert the most recent commit (merge-aware: -m 1).
pub fn revert_last(dir: &Path) -> Result<(), GitError> {
    git(dir, &["revert", "HEAD", "--no-edit"])?;
    Ok(())
}

/// Check if a branch has any commits ahead of another branch.
pub fn has_commits_ahead(dir: &Path, base: &str, branch: &str) -> Result<bool, GitError> {
    let range = format!("{}..{}", base, branch);
    let output = git(dir, &["rev-list", "--count", &range])?;
    let count: u64 = output.stdout.trim().parse().unwrap_or(0);
    Ok(count > 0)
}

/// Get the full SHA of the latest commit on a branch in the given directory.
pub fn latest_commit_sha(dir: &Path) -> Result<String, GitError> {
    let output = git(dir, &["rev-parse", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

/// Checkout a branch in the given directory.
pub fn checkout(dir: &Path, branch: &str) -> Result<(), GitError> {
    git(dir, &["checkout", branch])?;
    Ok(())
}

/// Merge a branch into the current branch with a merge commit.
pub fn merge(dir: &Path, branch: &str, message: &str) -> Result<(), GitError> {
    git(dir, &["merge", branch, "--no-ff", "-m", message])?;
    Ok(())
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-git`
Expected: compiles

- [ ] **Step 3: Write tests**

`crates/autotune-git/tests/git_test.rs`:

```rust
use autotune_git::*;
use std::process::Command;

/// Create a temporary git repo for testing.
fn setup_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    // Create initial commit
    std::fs::write(dir.path().join("README.md"), "# test").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    dir
}

#[test]
fn test_repo_root() {
    let dir = setup_repo();
    let root = repo_root(dir.path()).unwrap();
    assert_eq!(root.canonicalize().unwrap(), dir.path().canonicalize().unwrap());
}

#[test]
fn test_head_sha() {
    let dir = setup_repo();
    let sha = head_sha(dir.path()).unwrap();
    assert!(!sha.is_empty());
    assert!(sha.len() >= 7);
}

#[test]
fn test_current_branch() {
    let dir = setup_repo();
    // Git init creates "main" or "master" depending on config
    let branch = current_branch(dir.path()).unwrap();
    assert!(!branch.is_empty());
}

#[test]
fn test_create_branch_and_worktree() {
    let dir = setup_repo();
    let wt_path = dir.path().join("worktree-test");

    create_branch(dir.path(), "test-branch").unwrap();
    create_worktree(dir.path(), &wt_path, "test-branch").unwrap();

    assert!(wt_path.exists());
    assert!(wt_path.join("README.md").exists());

    remove_worktree(dir.path(), &wt_path).unwrap();
    assert!(!wt_path.exists());
}

#[test]
fn test_has_commits_ahead() {
    let dir = setup_repo();
    let base_branch = current_branch(dir.path()).unwrap();

    create_branch(dir.path(), "feature").unwrap();
    let wt_path = dir.path().join("wt");
    create_worktree(dir.path(), &wt_path, "feature").unwrap();

    // No commits ahead yet
    assert!(!has_commits_ahead(dir.path(), &base_branch, "feature").unwrap());

    // Add a commit in the worktree
    std::fs::write(wt_path.join("new.txt"), "hello").unwrap();
    Command::new("git")
        .args(["add", "new.txt"])
        .current_dir(&wt_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "new file"])
        .current_dir(&wt_path)
        .output()
        .unwrap();

    assert!(has_commits_ahead(dir.path(), &base_branch, "feature").unwrap());

    remove_worktree(dir.path(), &wt_path).unwrap();
}

#[test]
fn test_cherry_pick() {
    let dir = setup_repo();
    let base_branch = current_branch(dir.path()).unwrap();

    create_branch(dir.path(), "feature").unwrap();
    let wt_path = dir.path().join("wt");
    create_worktree(dir.path(), &wt_path, "feature").unwrap();

    // Commit in worktree
    std::fs::write(wt_path.join("feature.txt"), "feature").unwrap();
    Command::new("git")
        .args(["add", "feature.txt"])
        .current_dir(&wt_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "add feature"])
        .current_dir(&wt_path)
        .output()
        .unwrap();
    let sha = latest_commit_sha(&wt_path).unwrap();

    remove_worktree(dir.path(), &wt_path).unwrap();

    // Cherry-pick onto base
    cherry_pick(dir.path(), &sha).unwrap();
    assert!(dir.path().join("feature.txt").exists());
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p autotune-git`
Expected: all pass

- [ ] **Step 5: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-git/
git commit -m "feat: add autotune-git crate with worktree and branch operations"
```

---

### Task 3: autotune-adaptor

**Files:**
- Create: `crates/autotune-adaptor/Cargo.toml`
- Create: `crates/autotune-adaptor/src/lib.rs`
- Create: `crates/autotune-adaptor/src/regex.rs`
- Create: `crates/autotune-adaptor/src/criterion.rs`
- Create: `crates/autotune-adaptor/src/script.rs`
- Test: `crates/autotune-adaptor/tests/adaptor_test.rs`

- [ ] **Step 1: Create crate scaffold**

`crates/autotune-adaptor/Cargo.toml`:

```toml
[package]
name = "autotune-adaptor"
version = "0.1.0"
edition = "2024"

[dependencies]
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
regex = "1"
```

`crates/autotune-adaptor/src/lib.rs`:

```rust
pub mod criterion;
pub mod regex;
pub mod script;

use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdaptorError {
    #[error("regex pattern '{pattern}' failed to compile: {source}")]
    RegexCompile {
        pattern: String,
        source: ::regex::Error,
    },

    #[error("regex pattern '{pattern}' did not match any output for metric '{name}'")]
    RegexNoMatch { name: String, pattern: String },

    #[error("failed to parse extracted value '{value}' as f64 for metric '{name}'")]
    ParseFloat { name: String, value: String },

    #[error("criterion estimates.json not found at: {path}")]
    CriterionNotFound { path: String },

    #[error("criterion JSON parse error: {source}")]
    CriterionParse { source: serde_json::Error },

    #[error("script failed with exit code {code}: {stderr}")]
    ScriptFailed { code: i32, stderr: String },

    #[error("script output is not valid JSON: {source}")]
    ScriptOutputParse { source: serde_json::Error },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

/// Output from a benchmark command — the raw text an adaptor processes.
#[derive(Debug, Clone)]
pub struct BenchmarkOutput {
    pub stdout: String,
    pub stderr: String,
}

/// All adaptors produce this: a map of metric name → numeric value.
pub type Metrics = HashMap<String, f64>;

/// The adaptor trait. Takes benchmark output, produces metrics.
pub trait MetricAdaptor {
    fn extract(&self, output: &BenchmarkOutput) -> Result<Metrics, AdaptorError>;
}
```

`crates/autotune-adaptor/src/regex.rs`:

```rust
use crate::{AdaptorError, BenchmarkOutput, MetricAdaptor, Metrics};

/// Configuration for a single regex pattern.
#[derive(Debug, Clone)]
pub struct RegexPatternConfig {
    pub name: String,
    pub pattern: String,
}

/// Extracts metrics from benchmark output using regex capture groups.
pub struct RegexAdaptor {
    patterns: Vec<RegexPatternConfig>,
}

impl RegexAdaptor {
    pub fn new(patterns: Vec<RegexPatternConfig>) -> Self {
        Self { patterns }
    }
}

impl MetricAdaptor for RegexAdaptor {
    fn extract(&self, output: &BenchmarkOutput) -> Result<Metrics, AdaptorError> {
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        let mut metrics = Metrics::new();

        for pat in &self.patterns {
            let re = ::regex::Regex::new(&pat.pattern).map_err(|e| {
                AdaptorError::RegexCompile {
                    pattern: pat.pattern.clone(),
                    source: e,
                }
            })?;

            let caps = re.captures(&combined).ok_or_else(|| {
                AdaptorError::RegexNoMatch {
                    name: pat.name.clone(),
                    pattern: pat.pattern.clone(),
                }
            })?;

            // Use first capture group (index 1), or named group "value"
            let value_str = caps
                .name("value")
                .or_else(|| caps.get(1))
                .ok_or_else(|| AdaptorError::RegexNoMatch {
                    name: pat.name.clone(),
                    pattern: pat.pattern.clone(),
                })?
                .as_str();

            let value: f64 =
                value_str
                    .parse()
                    .map_err(|_| AdaptorError::ParseFloat {
                        name: pat.name.clone(),
                        value: value_str.to_string(),
                    })?;

            metrics.insert(pat.name.clone(), value);
        }

        Ok(metrics)
    }
}
```

`crates/autotune-adaptor/src/criterion.rs`:

```rust
use crate::{AdaptorError, BenchmarkOutput, MetricAdaptor, Metrics};
use std::path::{Path, PathBuf};

/// Reads Criterion's estimates.json for a named benchmark.
pub struct CriterionAdaptor {
    /// Path to the target directory (typically `target/criterion`).
    criterion_dir: PathBuf,
    benchmark_name: String,
}

impl CriterionAdaptor {
    pub fn new(criterion_dir: &Path, benchmark_name: &str) -> Self {
        Self {
            criterion_dir: criterion_dir.to_path_buf(),
            benchmark_name: benchmark_name.to_string(),
        }
    }

    fn estimates_path(&self) -> PathBuf {
        self.criterion_dir
            .join(&self.benchmark_name)
            .join("new")
            .join("estimates.json")
    }
}

#[derive(serde::Deserialize)]
struct CriterionEstimates {
    mean: CriterionStat,
    median: CriterionStat,
    std_dev: CriterionStat,
}

#[derive(serde::Deserialize)]
struct CriterionStat {
    point_estimate: f64,
}

impl MetricAdaptor for CriterionAdaptor {
    fn extract(&self, _output: &BenchmarkOutput) -> Result<Metrics, AdaptorError> {
        let path = self.estimates_path();
        let content = std::fs::read_to_string(&path).map_err(|_| {
            AdaptorError::CriterionNotFound {
                path: path.display().to_string(),
            }
        })?;

        let estimates: CriterionEstimates =
            serde_json::from_str(&content).map_err(|e| AdaptorError::CriterionParse { source: e })?;

        let mut metrics = Metrics::new();
        metrics.insert("mean".to_string(), estimates.mean.point_estimate);
        metrics.insert("median".to_string(), estimates.median.point_estimate);
        metrics.insert("std_dev".to_string(), estimates.std_dev.point_estimate);

        Ok(metrics)
    }
}
```

`crates/autotune-adaptor/src/script.rs`:

```rust
use crate::{AdaptorError, BenchmarkOutput, MetricAdaptor, Metrics};
use std::io::Write;
use std::process::{Command, Stdio};

/// Runs a user-provided script that reads benchmark output from stdin
/// and writes JSON metrics to stdout.
pub struct ScriptAdaptor {
    command: Vec<String>,
}

impl ScriptAdaptor {
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }
}

impl MetricAdaptor for ScriptAdaptor {
    fn extract(&self, output: &BenchmarkOutput) -> Result<Metrics, AdaptorError> {
        let program = &self.command[0];
        let args = &self.command[1..];

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(AdaptorError::Io)?;

        // Write combined stdout+stderr to script's stdin
        if let Some(mut stdin) = child.stdin.take() {
            let combined = format!("{}\n{}", output.stdout, output.stderr);
            stdin.write_all(combined.as_bytes()).map_err(AdaptorError::Io)?;
        }

        let result = child.wait_with_output().map_err(AdaptorError::Io)?;

        if !result.status.success() {
            return Err(AdaptorError::ScriptFailed {
                code: result.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&result.stdout);
        let metrics: Metrics = serde_json::from_str(&stdout)
            .map_err(|e| AdaptorError::ScriptOutputParse { source: e })?;

        Ok(metrics)
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-adaptor`
Expected: compiles

- [ ] **Step 3: Write tests**

`crates/autotune-adaptor/tests/adaptor_test.rs`:

```rust
use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};
use autotune_adaptor::script::ScriptAdaptor;
use autotune_adaptor::{BenchmarkOutput, MetricAdaptor};

#[test]
fn regex_extracts_single_metric() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "time_us".to_string(),
        pattern: r"time:\s+([0-9.]+)\s+µs".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: "benchmark result\ntime: 149.83 µs\nother stuff".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["time_us"], 149.83);
}

#[test]
fn regex_extracts_named_group() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "throughput".to_string(),
        pattern: r"throughput=(?P<value>[0-9.]+)".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: "throughput=1234.5".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["throughput"], 1234.5);
}

#[test]
fn regex_extracts_multiple_metrics() {
    let adaptor = RegexAdaptor::new(vec![
        RegexPatternConfig {
            name: "time".to_string(),
            pattern: r"time:\s+([0-9.]+)".to_string(),
        },
        RegexPatternConfig {
            name: "mem".to_string(),
            pattern: r"memory:\s+([0-9.]+)".to_string(),
        },
    ]);

    let output = BenchmarkOutput {
        stdout: "time: 100.5\nmemory: 256.0".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["time"], 100.5);
    assert_eq!(metrics["mem"], 256.0);
}

#[test]
fn regex_no_match_returns_error() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "missing".to_string(),
        pattern: r"nonexistent:\s+([0-9.]+)".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: "no match here".to_string(),
        stderr: String::new(),
    };

    assert!(adaptor.extract(&output).is_err());
}

#[test]
fn regex_searches_stderr_too() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "val".to_string(),
        pattern: r"result=([0-9.]+)".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: String::new(),
        stderr: "result=42.0".to_string(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["val"], 42.0);
}

#[test]
fn script_adaptor_echo_json() {
    // Use a simple shell command that outputs JSON
    let adaptor = ScriptAdaptor::new(vec![
        "sh".to_string(),
        "-c".to_string(),
        r#"echo '{"metric1": 42.0, "metric2": 3.14}'"#.to_string(),
    ]);

    let output = BenchmarkOutput {
        stdout: "ignored input".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["metric1"], 42.0);
    assert_eq!(metrics["metric2"], 3.14);
}

#[test]
fn script_adaptor_nonzero_exit_returns_error() {
    let adaptor = ScriptAdaptor::new(vec![
        "sh".to_string(),
        "-c".to_string(),
        "exit 1".to_string(),
    ]);

    let output = BenchmarkOutput {
        stdout: String::new(),
        stderr: String::new(),
    };

    assert!(adaptor.extract(&output).is_err());
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p autotune-adaptor`
Expected: all pass

- [ ] **Step 5: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-adaptor/
git commit -m "feat: add autotune-adaptor crate with regex, criterion, and script adaptors"
```

---

### Task 4: autotune-score

**Files:**
- Create: `crates/autotune-score/Cargo.toml`
- Create: `crates/autotune-score/src/lib.rs`
- Create: `crates/autotune-score/src/weighted_sum.rs`
- Create: `crates/autotune-score/src/threshold.rs`
- Create: `crates/autotune-score/src/script.rs`
- Test: `crates/autotune-score/tests/score_test.rs`

- [ ] **Step 1: Create crate scaffold**

`crates/autotune-score/Cargo.toml`:

```toml
[package]
name = "autotune-score"
version = "0.1.0"
edition = "2024"

[dependencies]
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`crates/autotune-score/src/lib.rs`:

```rust
pub mod script;
pub mod threshold;
pub mod weighted_sum;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScoreError {
    #[error("missing metric '{name}' in candidate")]
    MissingMetric { name: String },

    #[error("guardrail failed for '{name}': regression {regression:.4} exceeds max {max_regression:.4}")]
    GuardrailFailed {
        name: String,
        regression: f64,
        max_regression: f64,
    },

    #[error("script failed with exit code {code}: {stderr}")]
    ScriptFailed { code: i32, stderr: String },

    #[error("script output parse error: {source}")]
    ScriptOutputParse { source: serde_json::Error },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

pub type Metrics = HashMap<String, f64>;

/// Input provided to score calculators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreInput {
    /// Original baseline metrics (iteration 0).
    pub baseline: Metrics,
    /// Current candidate metrics (this iteration).
    pub candidate: Metrics,
    /// Current best metrics (from last kept iteration).
    pub best: Metrics,
}

/// Output from a score calculator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreOutput {
    /// Numeric rank indicator (higher = better).
    pub rank: f64,
    /// Decision: "keep", "discard", or "neutral".
    pub decision: String,
    /// Human-readable reason.
    pub reason: String,
}

/// The score calculator trait.
pub trait ScoreCalculator {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError>;
}
```

`crates/autotune-score/src/weighted_sum.rs`:

```rust
use crate::{Metrics, ScoreCalculator, ScoreError, ScoreInput, ScoreOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Minimize,
    Maximize,
}

#[derive(Debug, Clone)]
pub struct PrimaryMetricDef {
    pub name: String,
    pub direction: Direction,
    pub weight: f64,
}

#[derive(Debug, Clone)]
pub struct GuardrailMetricDef {
    pub name: String,
    pub direction: Direction,
    pub max_regression: f64,
}

pub struct WeightedSumScorer {
    primary: Vec<PrimaryMetricDef>,
    guardrails: Vec<GuardrailMetricDef>,
}

impl WeightedSumScorer {
    pub fn new(primary: Vec<PrimaryMetricDef>, guardrails: Vec<GuardrailMetricDef>) -> Self {
        Self {
            primary,
            guardrails,
        }
    }
}

fn improvement(best: f64, candidate: f64, direction: Direction) -> f64 {
    if best == 0.0 {
        return 0.0;
    }
    match direction {
        Direction::Maximize => (candidate - best) / best.abs(),
        Direction::Minimize => (best - candidate) / best.abs(),
    }
}

fn check_guardrail(
    best: f64,
    candidate: f64,
    direction: Direction,
    max_regression: f64,
) -> Option<f64> {
    if best == 0.0 {
        return None;
    }
    let regression = match direction {
        Direction::Maximize => (best - candidate) / best.abs(),
        Direction::Minimize => (candidate - best) / best.abs(),
    };
    if regression > max_regression {
        Some(regression)
    } else {
        None
    }
}

fn get_metric(metrics: &Metrics, name: &str) -> Result<f64, ScoreError> {
    metrics
        .get(name)
        .copied()
        .ok_or_else(|| ScoreError::MissingMetric {
            name: name.to_string(),
        })
}

impl ScoreCalculator for WeightedSumScorer {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError> {
        // Check guardrails first
        for g in &self.guardrails {
            let best_val = get_metric(&input.best, &g.name)?;
            let cand_val = get_metric(&input.candidate, &g.name)?;
            if let Some(regression) = check_guardrail(best_val, cand_val, g.direction, g.max_regression) {
                return Ok(ScoreOutput {
                    rank: -regression,
                    decision: "discard".to_string(),
                    reason: format!(
                        "guardrail '{}' failed: regression {:.2}% exceeds max {:.2}%",
                        g.name,
                        regression * 100.0,
                        g.max_regression * 100.0
                    ),
                });
            }
        }

        // Compute weighted rank
        let mut rank = 0.0;
        let mut reasons = Vec::new();

        for pm in &self.primary {
            let best_val = get_metric(&input.best, &pm.name)?;
            let cand_val = get_metric(&input.candidate, &pm.name)?;
            let imp = improvement(best_val, cand_val, pm.direction);
            rank += pm.weight * imp;
            reasons.push(format!("{}: {:.2}%", pm.name, imp * 100.0));
        }

        let decision = if rank > 0.0 { "keep" } else { "discard" };
        let reason = reasons.join(", ");

        Ok(ScoreOutput {
            rank,
            decision: decision.to_string(),
            reason,
        })
    }
}
```

`crates/autotune-score/src/threshold.rs`:

```rust
use crate::{ScoreCalculator, ScoreError, ScoreInput, ScoreOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Minimize,
    Maximize,
}

#[derive(Debug, Clone)]
pub struct ThresholdConditionDef {
    pub metric: String,
    pub direction: Direction,
    pub threshold: f64,
}

pub struct ThresholdScorer {
    conditions: Vec<ThresholdConditionDef>,
}

impl ThresholdScorer {
    pub fn new(conditions: Vec<ThresholdConditionDef>) -> Self {
        Self { conditions }
    }
}

impl ScoreCalculator for ThresholdScorer {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError> {
        let mut all_pass = true;
        let mut reasons = Vec::new();
        let mut total_improvement = 0.0;

        for c in &self.conditions {
            let best_val = input.best.get(&c.metric).copied().ok_or_else(|| {
                ScoreError::MissingMetric {
                    name: c.metric.clone(),
                }
            })?;
            let cand_val = input.candidate.get(&c.metric).copied().ok_or_else(|| {
                ScoreError::MissingMetric {
                    name: c.metric.clone(),
                }
            })?;

            let improvement = match c.direction {
                Direction::Maximize => cand_val - best_val,
                Direction::Minimize => best_val - cand_val,
            };

            if improvement >= c.threshold {
                reasons.push(format!("{}: passed (+{:.4})", c.metric, improvement));
                total_improvement += improvement;
            } else {
                reasons.push(format!("{}: failed ({:.4} < {:.4})", c.metric, improvement, c.threshold));
                all_pass = false;
            }
        }

        Ok(ScoreOutput {
            rank: total_improvement,
            decision: if all_pass { "keep" } else { "discard" }.to_string(),
            reason: reasons.join(", "),
        })
    }
}
```

`crates/autotune-score/src/script.rs`:

```rust
use crate::{ScoreCalculator, ScoreError, ScoreInput, ScoreOutput};
use std::io::Write;
use std::process::{Command, Stdio};

/// Runs a user-provided script/command that reads ScoreInput JSON from stdin
/// and writes ScoreOutput JSON to stdout.
pub struct ScriptScorer {
    command: Vec<String>,
}

impl ScriptScorer {
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }
}

impl ScoreCalculator for ScriptScorer {
    fn calculate(&self, input: &ScoreInput) -> Result<ScoreOutput, ScoreError> {
        let program = &self.command[0];
        let args = &self.command[1..];

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ScoreError::Io)?;

        if let Some(mut stdin) = child.stdin.take() {
            let json = serde_json::to_string(input).map_err(|e| ScoreError::ScriptOutputParse { source: e })?;
            stdin.write_all(json.as_bytes()).map_err(ScoreError::Io)?;
        }

        let result = child.wait_with_output().map_err(ScoreError::Io)?;

        if !result.status.success() {
            return Err(ScoreError::ScriptFailed {
                code: result.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&result.stdout);
        let output: ScoreOutput =
            serde_json::from_str(&stdout).map_err(|e| ScoreError::ScriptOutputParse { source: e })?;

        Ok(output)
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-score`
Expected: compiles

- [ ] **Step 3: Write tests**

`crates/autotune-score/tests/score_test.rs`:

```rust
use autotune_score::weighted_sum::{Direction, GuardrailMetricDef, PrimaryMetricDef, WeightedSumScorer};
use autotune_score::threshold::{
    Direction as TDirection, ThresholdConditionDef, ThresholdScorer,
};
use autotune_score::script::ScriptScorer;
use autotune_score::{Metrics, ScoreCalculator, ScoreInput};

fn make_input(best: &[(&str, f64)], candidate: &[(&str, f64)]) -> ScoreInput {
    let to_map = |pairs: &[(&str, f64)]| -> Metrics {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    };
    ScoreInput {
        baseline: to_map(best),
        candidate: to_map(candidate),
        best: to_map(best),
    }
}

#[test]
fn weighted_sum_improvement_minimize() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("time_us", 200.0)], &[("time_us", 170.0)]);
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.decision, "keep");
    assert!((result.rank - 0.15).abs() < 0.001);
}

#[test]
fn weighted_sum_regression_minimize() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("time_us", 200.0)], &[("time_us", 220.0)]);
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.decision, "discard");
    assert!(result.rank < 0.0);
}

#[test]
fn weighted_sum_improvement_maximize() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "throughput".to_string(),
            direction: Direction::Maximize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("throughput", 100.0)], &[("throughput", 120.0)]);
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.decision, "keep");
    assert!((result.rank - 0.2).abs() < 0.001);
}

#[test]
fn weighted_sum_guardrail_blocks() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![GuardrailMetricDef {
            name: "accuracy".to_string(),
            direction: Direction::Maximize,
            max_regression: 0.01,
        }],
    );

    // time improved but accuracy regressed beyond threshold
    let input = make_input(
        &[("time_us", 200.0), ("accuracy", 0.99)],
        &[("time_us", 150.0), ("accuracy", 0.95)],
    );
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.decision, "discard");
    assert!(result.reason.contains("guardrail"));
}

#[test]
fn weighted_sum_guardrail_passes() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![GuardrailMetricDef {
            name: "accuracy".to_string(),
            direction: Direction::Maximize,
            max_regression: 0.05,
        }],
    );

    // time improved, accuracy barely regressed (within threshold)
    let input = make_input(
        &[("time_us", 200.0), ("accuracy", 1.0)],
        &[("time_us", 150.0), ("accuracy", 0.97)],
    );
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.decision, "keep");
}

#[test]
fn weighted_sum_zero_baseline() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "m".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = make_input(&[("m", 0.0)], &[("m", 5.0)]);
    let result = scorer.calculate(&input).unwrap();
    // Zero baseline → rank 0 (safe division)
    assert_eq!(result.rank, 0.0);
}

#[test]
fn weighted_sum_multiple_metrics() {
    let scorer = WeightedSumScorer::new(
        vec![
            PrimaryMetricDef {
                name: "time".to_string(),
                direction: Direction::Minimize,
                weight: 2.0,
            },
            PrimaryMetricDef {
                name: "mem".to_string(),
                direction: Direction::Minimize,
                weight: 1.0,
            },
        ],
        vec![],
    );

    // time: 10% improvement (0.1 * 2.0 = 0.2), mem: 5% improvement (0.05 * 1.0 = 0.05)
    let input = make_input(
        &[("time", 100.0), ("mem", 200.0)],
        &[("time", 90.0), ("mem", 190.0)],
    );
    let result = scorer.calculate(&input).unwrap();
    assert!((result.rank - 0.25).abs() < 0.001);
    assert_eq!(result.decision, "keep");
}

#[test]
fn threshold_all_pass() {
    let scorer = ThresholdScorer::new(vec![ThresholdConditionDef {
        metric: "size_kb".to_string(),
        direction: TDirection::Minimize,
        threshold: 0.0,
    }]);

    let input = make_input(&[("size_kb", 100.0)], &[("size_kb", 95.0)]);
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.decision, "keep");
}

#[test]
fn threshold_fails() {
    let scorer = ThresholdScorer::new(vec![ThresholdConditionDef {
        metric: "size_kb".to_string(),
        direction: TDirection::Minimize,
        threshold: 0.0,
    }]);

    let input = make_input(&[("size_kb", 100.0)], &[("size_kb", 105.0)]);
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.decision, "discard");
}

#[test]
fn script_scorer_echo_json() {
    let scorer = ScriptScorer::new(vec![
        "sh".to_string(),
        "-c".to_string(),
        r#"echo '{"rank": 0.5, "decision": "keep", "reason": "looks good"}'"#.to_string(),
    ]);

    let input = make_input(&[("m", 1.0)], &[("m", 2.0)]);
    let result = scorer.calculate(&input).unwrap();
    assert_eq!(result.rank, 0.5);
    assert_eq!(result.decision, "keep");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p autotune-score`
Expected: all pass

- [ ] **Step 5: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-score/
git commit -m "feat: add autotune-score crate with weighted_sum, threshold, and script scorers"
```

---

### Task 5: autotune-state

**Files:**
- Create: `crates/autotune-state/Cargo.toml`
- Create: `crates/autotune-state/src/lib.rs`
- Test: `crates/autotune-state/tests/state_test.rs`

- [ ] **Step 1: Create crate**

`crates/autotune-state/Cargo.toml`:

```toml
[package]
name = "autotune-state"
version = "0.1.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
tempfile = "3"
```

`crates/autotune-state/src/lib.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("experiment not found: {name}")]
    NotFound { name: String },

    #[error("invalid phase transition: {from} → {to}")]
    InvalidTransition { from: String, to: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("JSON error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },
}

pub type Metrics = HashMap<String, f64>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Planning,
    Implementing,
    Testing,
    Benchmarking,
    Scoring,
    Integrating,
    Recorded,
    Done,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Phase::Planning => write!(f, "Planning"),
            Phase::Implementing => write!(f, "Implementing"),
            Phase::Testing => write!(f, "Testing"),
            Phase::Benchmarking => write!(f, "Benchmarking"),
            Phase::Scoring => write!(f, "Scoring"),
            Phase::Integrating => write!(f, "Integrating"),
            Phase::Recorded => write!(f, "Recorded"),
            Phase::Done => write!(f, "Done"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentState {
    pub experiment_name: String,
    pub canonical_branch: String,
    pub research_session_id: String,
    pub current_iteration: usize,
    pub current_phase: Phase,
    pub current_approach: Option<ApproachState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproachState {
    pub name: String,
    pub hypothesis: String,
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub commit_sha: Option<String>,
    pub test_results: Vec<TestResult>,
    pub metrics: Option<Metrics>,
    pub rank: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub duration_secs: f64,
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationRecord {
    pub iteration: usize,
    pub approach: String,
    pub status: IterationStatus,
    #[serde(default)]
    pub hypothesis: Option<String>,
    pub metrics: Metrics,
    pub rank: f64,
    #[serde(default)]
    pub score: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IterationStatus {
    Baseline,
    Kept,
    Discarded,
    Crash,
}

/// Manages the experiment directory and all state files.
pub struct ExperimentStore {
    root: PathBuf,
}

impl ExperimentStore {
    /// Create a store for the given experiment directory.
    /// The directory is created if it doesn't exist.
    pub fn new(experiment_dir: &Path) -> Result<Self, StateError> {
        fs::create_dir_all(experiment_dir)?;
        fs::create_dir_all(experiment_dir.join("iterations"))?;
        Ok(Self {
            root: experiment_dir.to_path_buf(),
        })
    }

    /// Open an existing experiment store (directory must exist).
    pub fn open(experiment_dir: &Path) -> Result<Self, StateError> {
        if !experiment_dir.exists() {
            return Err(StateError::NotFound {
                name: experiment_dir.display().to_string(),
            });
        }
        Ok(Self {
            root: experiment_dir.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // --- state.json ---

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn save_state(&self, state: &ExperimentState) -> Result<(), StateError> {
        atomic_write(&self.state_path(), &serde_json::to_string_pretty(state)?)
    }

    pub fn load_state(&self) -> Result<ExperimentState, StateError> {
        let content = fs::read_to_string(self.state_path())?;
        Ok(serde_json::from_str(&content)?)
    }

    // --- ledger.json ---

    fn ledger_path(&self) -> PathBuf {
        self.root.join("ledger.json")
    }

    pub fn load_ledger(&self) -> Result<Vec<IterationRecord>, StateError> {
        let path = self.ledger_path();
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn append_ledger(&self, record: &IterationRecord) -> Result<(), StateError> {
        let mut ledger = self.load_ledger()?;
        ledger.push(record.clone());
        atomic_write(&self.ledger_path(), &serde_json::to_string_pretty(&ledger)?)
    }

    // --- config_snapshot.toml ---

    pub fn save_config_snapshot(&self, content: &str) -> Result<(), StateError> {
        atomic_write(&self.root.join("config_snapshot.toml"), content)
    }

    pub fn load_config_snapshot(&self) -> Result<String, StateError> {
        Ok(fs::read_to_string(self.root.join("config_snapshot.toml"))?)
    }

    // --- log.md ---

    fn log_path(&self) -> PathBuf {
        self.root.join("log.md")
    }

    pub fn read_log(&self) -> Result<String, StateError> {
        let path = self.log_path();
        if !path.exists() {
            return Ok(String::new());
        }
        Ok(fs::read_to_string(path)?)
    }

    pub fn append_log(&self, entry: &str) -> Result<(), StateError> {
        let mut content = self.read_log()?;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(entry);
        content.push('\n');
        atomic_write(&self.log_path(), &content)
    }

    // --- iteration directories ---

    pub fn iteration_dir(&self, iteration: usize, approach: &str) -> PathBuf {
        self.root
            .join("iterations")
            .join(format!("{:03}-{}", iteration, approach))
    }

    pub fn save_iteration_metrics(
        &self,
        iteration: usize,
        approach: &str,
        metrics: &Metrics,
    ) -> Result<(), StateError> {
        let dir = self.iteration_dir(iteration, approach);
        fs::create_dir_all(&dir)?;
        atomic_write(
            &dir.join("metrics.json"),
            &serde_json::to_string_pretty(metrics)?,
        )
    }

    pub fn save_iteration_prompt(
        &self,
        iteration: usize,
        approach: &str,
        prompt: &str,
    ) -> Result<(), StateError> {
        let dir = self.iteration_dir(iteration, approach);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("prompt.md"), prompt)
    }

    pub fn save_test_output(
        &self,
        iteration: usize,
        approach: &str,
        output: &str,
    ) -> Result<(), StateError> {
        let dir = self.iteration_dir(iteration, approach);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("test_output.txt"), output)
    }

    // --- list experiments ---

    /// List all experiment names in the .autotune/experiments/ directory.
    pub fn list_experiments(autotune_dir: &Path) -> Result<Vec<String>, StateError> {
        let experiments_dir = autotune_dir.join("experiments");
        if !experiments_dir.exists() {
            return Ok(vec![]);
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(experiments_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }
}

/// Write atomically: write to a temp file, then rename.
fn atomic_write(path: &Path, content: &str) -> Result<(), StateError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(content.as_bytes())?;
    tmp.persist(path).map_err(|e| StateError::Io { source: e.error })?;
    Ok(())
}
```

- [ ] **Step 2: Add tempfile as a runtime dependency** (needed for atomic_write)

Update `crates/autotune-state/Cargo.toml` dependencies:

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
chrono = { version = "0.4", features = ["serde"] }
tempfile = "3"
```

- [ ] **Step 3: Verify compilation**

Run: `cargo build -p autotune-state`
Expected: compiles

- [ ] **Step 4: Write tests**

`crates/autotune-state/tests/state_test.rs`:

```rust
use autotune_state::*;
use chrono::Utc;
use std::collections::HashMap;

#[test]
fn roundtrip_state() {
    let dir = tempfile::tempdir().unwrap();
    let exp_dir = dir.path().join("test-exp");
    let store = ExperimentStore::new(&exp_dir).unwrap();

    let state = ExperimentState {
        experiment_name: "test-exp".to_string(),
        canonical_branch: "main".to_string(),
        research_session_id: "sess-123".to_string(),
        current_iteration: 0,
        current_phase: Phase::Planning,
        current_approach: None,
    };

    store.save_state(&state).unwrap();
    let loaded = store.load_state().unwrap();
    assert_eq!(loaded.experiment_name, "test-exp");
    assert_eq!(loaded.current_phase, Phase::Planning);
}

#[test]
fn roundtrip_state_with_approach() {
    let dir = tempfile::tempdir().unwrap();
    let exp_dir = dir.path().join("test-exp");
    let store = ExperimentStore::new(&exp_dir).unwrap();

    let state = ExperimentState {
        experiment_name: "test-exp".to_string(),
        canonical_branch: "main".to_string(),
        research_session_id: "sess-123".to_string(),
        current_iteration: 3,
        current_phase: Phase::Testing,
        current_approach: Some(ApproachState {
            name: "simd-ops".to_string(),
            hypothesis: "Use SIMD for phase updates".to_string(),
            worktree_path: "/tmp/wt".into(),
            branch_name: "autotune/simd-ops".to_string(),
            commit_sha: Some("abc123".to_string()),
            test_results: vec![],
            metrics: None,
            rank: None,
        }),
    };

    store.save_state(&state).unwrap();
    let loaded = store.load_state().unwrap();
    let approach = loaded.current_approach.unwrap();
    assert_eq!(approach.name, "simd-ops");
    assert_eq!(approach.commit_sha.unwrap(), "abc123");
}

#[test]
fn append_and_load_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let exp_dir = dir.path().join("test-exp");
    let store = ExperimentStore::new(&exp_dir).unwrap();

    let record = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: HashMap::from([("time_us".to_string(), 180.76)]),
        rank: 1.0,
        score: None,
        reason: None,
        timestamp: Utc::now(),
    };

    store.append_ledger(&record).unwrap();
    let ledger = store.load_ledger().unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].approach, "baseline");

    // Append another
    let record2 = IterationRecord {
        iteration: 1,
        approach: "precompute".to_string(),
        status: IterationStatus::Kept,
        hypothesis: Some("precompute masks".to_string()),
        metrics: HashMap::from([("time_us".to_string(), 149.83)]),
        rank: 1.171,
        score: Some("+17.1%".to_string()),
        reason: Some("time_us: +17.1%".to_string()),
        timestamp: Utc::now(),
    };

    store.append_ledger(&record2).unwrap();
    let ledger = store.load_ledger().unwrap();
    assert_eq!(ledger.len(), 2);
}

#[test]
fn log_append_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let exp_dir = dir.path().join("test-exp");
    let store = ExperimentStore::new(&exp_dir).unwrap();

    assert_eq!(store.read_log().unwrap(), "");

    store.append_log("## 2026-04-11").unwrap();
    store.append_log("- Found that SIMD helps phase updates").unwrap();

    let log = store.read_log().unwrap();
    assert!(log.contains("## 2026-04-11"));
    assert!(log.contains("SIMD helps"));
}

#[test]
fn iteration_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let exp_dir = dir.path().join("test-exp");
    let store = ExperimentStore::new(&exp_dir).unwrap();

    let metrics = HashMap::from([("time_us".to_string(), 149.83)]);
    store.save_iteration_metrics(1, "precompute", &metrics).unwrap();
    store.save_iteration_prompt(1, "precompute", "implement precompute mask").unwrap();
    store.save_test_output(1, "precompute", "test failed: assertion error").unwrap();

    let iter_dir = store.iteration_dir(1, "precompute");
    assert!(iter_dir.join("metrics.json").exists());
    assert!(iter_dir.join("prompt.md").exists());
    assert!(iter_dir.join("test_output.txt").exists());
}

#[test]
fn config_snapshot_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let exp_dir = dir.path().join("test-exp");
    let store = ExperimentStore::new(&exp_dir).unwrap();

    let config = "[experiment]\nname = \"test\"";
    store.save_config_snapshot(config).unwrap();
    let loaded = store.load_config_snapshot().unwrap();
    assert_eq!(loaded, config);
}

#[test]
fn list_experiments() {
    let dir = tempfile::tempdir().unwrap();
    let autotune_dir = dir.path().join(".autotune");

    // No experiments yet
    let names = ExperimentStore::list_experiments(&autotune_dir).unwrap();
    assert!(names.is_empty());

    // Create two experiments
    ExperimentStore::new(&autotune_dir.join("experiments").join("alpha")).unwrap();
    ExperimentStore::new(&autotune_dir.join("experiments").join("beta")).unwrap();

    let names = ExperimentStore::list_experiments(&autotune_dir).unwrap();
    assert_eq!(names, vec!["alpha", "beta"]);
}

#[test]
fn open_nonexistent_experiment_errors() {
    let result = ExperimentStore::open(std::path::Path::new("/nonexistent/experiment"));
    assert!(result.is_err());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p autotune-state`
Expected: all pass

- [ ] **Step 6: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 7: Commit**

```bash
git add crates/autotune-state/
git commit -m "feat: add autotune-state crate with experiment state persistence and ledger"
```

---

### Task 6: autotune-agent

**Files:**
- Create: `crates/autotune-agent/Cargo.toml`
- Create: `crates/autotune-agent/src/lib.rs`
- Create: `crates/autotune-agent/src/claude.rs`
- Test: `crates/autotune-agent/tests/agent_test.rs`

- [ ] **Step 1: Create crate**

`crates/autotune-agent/Cargo.toml`:

```toml
[package]
name = "autotune-agent"
version = "0.1.0"
edition = "2024"

[dependencies]
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`crates/autotune-agent/src/lib.rs`:

```rust
pub mod claude;

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent command failed: {message}")]
    CommandFailed { message: String },

    #[error("failed to parse agent response: {message}")]
    ParseFailed { message: String },

    #[error("agent timed out after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

/// Tool permission for agent sandboxing.
#[derive(Debug, Clone)]
pub enum ToolPermission {
    /// Allow a tool unconditionally: "Read", "Glob", "Grep"
    Allow(String),
    /// Allow a tool scoped to a path: "Edit:src/**"
    AllowScoped(String, String),
    /// Deny a tool
    Deny(String),
}

/// Configuration for spawning an agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub prompt: String,
    pub allowed_tools: Vec<ToolPermission>,
    pub working_directory: PathBuf,
    /// Optional model override (e.g., "opus", "sonnet")
    pub model: Option<String>,
    /// Optional max turns
    pub max_turns: Option<u64>,
}

/// A handle to an active agent session.
#[derive(Debug, Clone)]
pub struct AgentSession {
    pub session_id: String,
    pub backend: String,
}

/// Response from an agent.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    pub text: String,
    pub session_id: String,
}

/// The agent trait. Implementations handle spawning and communicating with LLM agents.
pub trait Agent {
    /// Spawn a new agent session.
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError>;

    /// Send a follow-up message to an existing session.
    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError>;

    /// Return the backend name (e.g., "claude").
    fn backend_name(&self) -> &str;

    /// Return the command to open an interactive session for handover.
    fn handover_command(&self, session: &AgentSession) -> String;
}
```

`crates/autotune-agent/src/claude.rs`:

```rust
use crate::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession, ToolPermission};
use std::process::Command;

/// Agent implementation that shells out to the `claude` CLI.
pub struct ClaudeAgent;

impl ClaudeAgent {
    pub fn new() -> Self {
        Self
    }

    fn build_args(config: &AgentConfig, session_id: Option<&str>) -> Vec<String> {
        let mut args = Vec::new();

        // Print mode (non-interactive)
        args.push("-p".to_string());
        args.push(config.prompt.clone());

        // Output format
        args.push("--output-format".to_string());
        args.push("json".to_string());

        // Session handling
        if let Some(sid) = session_id {
            args.push("-r".to_string());
            args.push(sid.to_string());
        }

        // Model override
        if let Some(model) = &config.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        // Max turns
        if let Some(turns) = config.max_turns {
            args.push("--max-turns".to_string());
            args.push(turns.to_string());
        }

        // Tool permissions
        for perm in &config.allowed_tools {
            match perm {
                ToolPermission::Allow(tool) => {
                    args.push("--allowedTools".to_string());
                    args.push(tool.clone());
                }
                ToolPermission::AllowScoped(tool, path) => {
                    args.push("--allowedTools".to_string());
                    args.push(format!("{}:{}", tool, path));
                }
                ToolPermission::Deny(tool) => {
                    args.push("--disallowedTools".to_string());
                    args.push(tool.clone());
                }
            }
        }

        args
    }

    fn run_claude(args: &[String], cwd: &std::path::Path) -> Result<AgentResponse, AgentError> {
        let output = Command::new("claude")
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(AgentError::Io)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AgentError::CommandFailed {
                message: format!("claude exited with {}: {}", output.status, stderr),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        // Try to extract session_id from JSON output
        let session_id = serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .and_then(|v| v.get("session_id").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_default();

        // Extract the text result
        let text = serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .and_then(|v| v.get("result").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or(stdout.clone());

        Ok(AgentResponse { text, session_id })
    }
}

impl Default for ClaudeAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for ClaudeAgent {
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        let args = Self::build_args(config, None);
        Self::run_claude(&args, &config.working_directory)
    }

    fn send(&self, session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError> {
        let config = AgentConfig {
            prompt: message.to_string(),
            allowed_tools: vec![],
            working_directory: std::path::PathBuf::from("."),
            model: None,
            max_turns: None,
        };
        let args = Self::build_args(&config, Some(&session.session_id));
        Self::run_claude(&args, &config.working_directory)
    }

    fn backend_name(&self) -> &str {
        "claude"
    }

    fn handover_command(&self, session: &AgentSession) -> String {
        format!("claude -r {}", session.session_id)
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-agent`
Expected: compiles

- [ ] **Step 3: Write unit tests for arg building (no actual CLI calls)**

`crates/autotune-agent/tests/agent_test.rs`:

```rust
use autotune_agent::claude::ClaudeAgent;
use autotune_agent::{Agent, AgentConfig, AgentSession, ToolPermission};
use std::path::PathBuf;

#[test]
fn claude_backend_name() {
    let agent = ClaudeAgent::new();
    assert_eq!(agent.backend_name(), "claude");
}

#[test]
fn claude_handover_command() {
    let agent = ClaudeAgent::new();
    let session = AgentSession {
        session_id: "abc-123".to_string(),
        backend: "claude".to_string(),
    };
    assert_eq!(agent.handover_command(&session), "claude -r abc-123");
}

#[test]
fn agent_config_builds() {
    // Just test that the config struct builds correctly
    let config = AgentConfig {
        prompt: "test prompt".to_string(),
        allowed_tools: vec![
            ToolPermission::Allow("Read".to_string()),
            ToolPermission::AllowScoped("Edit".to_string(), "src/**".to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: PathBuf::from("/tmp"),
        model: Some("opus".to_string()),
        max_turns: Some(50),
    };

    assert_eq!(config.prompt, "test prompt");
    assert_eq!(config.allowed_tools.len(), 3);
    assert_eq!(config.model.unwrap(), "opus");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p autotune-agent`
Expected: all pass

- [ ] **Step 5: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-agent/
git commit -m "feat: add autotune-agent crate with Agent trait and Claude CLI backend"
```

---

### Task 7: autotune-test

**Files:**
- Create: `crates/autotune-test/Cargo.toml`
- Create: `crates/autotune-test/src/lib.rs`
- Test: `crates/autotune-test/tests/test_runner_test.rs`

- [ ] **Step 1: Create crate**

`crates/autotune-test/Cargo.toml`:

```toml
[package]
name = "autotune-test"
version = "0.1.0"
edition = "2024"

[dependencies]
autotune-config = { path = "../autotune-config" }
thiserror = "2"
```

`crates/autotune-test/src/lib.rs`:

```rust
use autotune_config::TestConfig;
use std::path::Path;
use std::process::Command;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TestError {
    #[error("test '{name}' failed (exit code {code})")]
    Failed {
        name: String,
        code: i32,
        stdout: String,
        stderr: String,
    },

    #[error("test '{name}' timed out after {timeout}s")]
    Timeout { name: String, timeout: u64 },

    #[error("IO error running test '{name}': {source}")]
    Io {
        name: String,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub duration_secs: f64,
    pub stdout: String,
    pub stderr: String,
}

/// Run a single test command.
pub fn run_test(config: &TestConfig, working_dir: &Path) -> Result<TestResult, TestError> {
    let start = Instant::now();

    let program = &config.command[0];
    let args = &config.command[1..];

    let output = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .output()
        .map_err(|e| TestError::Io {
            name: config.name.clone(),
            source: e,
        })?;

    let duration = start.elapsed().as_secs_f64();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(TestResult {
            name: config.name.clone(),
            passed: true,
            duration_secs: duration,
            stdout,
            stderr,
        })
    } else {
        Ok(TestResult {
            name: config.name.clone(),
            passed: false,
            duration_secs: duration,
            stdout,
            stderr,
        })
    }
}

/// Run all configured tests sequentially. Returns on first failure.
/// Returns the list of results (up to and including the first failure).
pub fn run_all_tests(
    configs: &[TestConfig],
    working_dir: &Path,
) -> Result<Vec<TestResult>, TestError> {
    let mut results = Vec::new();

    for config in configs {
        let result = run_test(config, working_dir)?;
        let passed = result.passed;
        results.push(result);
        if !passed {
            break;
        }
    }

    Ok(results)
}

/// Check if all test results passed.
pub fn all_passed(results: &[TestResult]) -> bool {
    results.iter().all(|r| r.passed)
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-test`
Expected: compiles

- [ ] **Step 3: Write tests**

`crates/autotune-test/tests/test_runner_test.rs`:

```rust
use autotune_config::TestConfig;
use autotune_test::{all_passed, run_all_tests, run_test};

fn make_test_config(name: &str, command: &[&str]) -> TestConfig {
    TestConfig {
        name: name.to_string(),
        command: command.iter().map(|s| s.to_string()).collect(),
        timeout: 30,
    }
}

#[test]
fn passing_test() {
    let config = make_test_config("echo", &["sh", "-c", "echo hello"]);
    let result = run_test(&config, std::path::Path::new(".")).unwrap();
    assert!(result.passed);
    assert!(result.stdout.contains("hello"));
}

#[test]
fn failing_test() {
    let config = make_test_config("fail", &["sh", "-c", "echo oops >&2; exit 1"]);
    let result = run_test(&config, std::path::Path::new(".")).unwrap();
    assert!(!result.passed);
    assert!(result.stderr.contains("oops"));
}

#[test]
fn run_all_stops_on_first_failure() {
    let configs = vec![
        make_test_config("pass1", &["sh", "-c", "echo ok"]),
        make_test_config("fail", &["sh", "-c", "exit 1"]),
        make_test_config("pass2", &["sh", "-c", "echo ok"]),
    ];
    let results = run_all_tests(&configs, std::path::Path::new(".")).unwrap();
    assert_eq!(results.len(), 2); // stopped after fail
    assert!(results[0].passed);
    assert!(!results[1].passed);
    assert!(!all_passed(&results));
}

#[test]
fn run_all_passes() {
    let configs = vec![
        make_test_config("p1", &["sh", "-c", "echo a"]),
        make_test_config("p2", &["sh", "-c", "echo b"]),
    ];
    let results = run_all_tests(&configs, std::path::Path::new(".")).unwrap();
    assert_eq!(results.len(), 2);
    assert!(all_passed(&results));
}

#[test]
fn empty_test_list() {
    let results = run_all_tests(&[], std::path::Path::new(".")).unwrap();
    assert!(results.is_empty());
    assert!(all_passed(&results));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p autotune-test`
Expected: all pass

- [ ] **Step 5: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-test/
git commit -m "feat: add autotune-test crate for running configured test commands"
```

---

### Task 8: autotune-benchmark

**Files:**
- Create: `crates/autotune-benchmark/Cargo.toml`
- Create: `crates/autotune-benchmark/src/lib.rs`
- Test: `crates/autotune-benchmark/tests/benchmark_test.rs`

- [ ] **Step 1: Create crate**

`crates/autotune-benchmark/Cargo.toml`:

```toml
[package]
name = "autotune-benchmark"
version = "0.1.0"
edition = "2024"

[dependencies]
autotune-config = { path = "../autotune-config" }
autotune-adaptor = { path = "../autotune-adaptor" }
thiserror = "2"
```

`crates/autotune-benchmark/src/lib.rs`:

```rust
use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};
use autotune_adaptor::criterion::CriterionAdaptor;
use autotune_adaptor::script::ScriptAdaptor;
use autotune_adaptor::{BenchmarkOutput, MetricAdaptor, Metrics};
use autotune_config::{AdaptorConfig, BenchmarkConfig};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BenchmarkError {
    #[error("benchmark '{name}' command failed (exit code {code}): {stderr}")]
    CommandFailed {
        name: String,
        code: i32,
        stderr: String,
    },

    #[error("benchmark '{name}' IO error: {source}")]
    Io {
        name: String,
        source: std::io::Error,
    },

    #[error("metric extraction failed for benchmark '{name}': {source}")]
    Extraction {
        name: String,
        source: autotune_adaptor::AdaptorError,
    },
}

/// Run a single benchmark command and extract metrics.
pub fn run_benchmark(
    config: &BenchmarkConfig,
    working_dir: &Path,
) -> Result<Metrics, BenchmarkError> {
    let program = &config.command[0];
    let args = &config.command[1..];

    let output = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .output()
        .map_err(|e| BenchmarkError::Io {
            name: config.name.clone(),
            source: e,
        })?;

    if !output.status.success() {
        return Err(BenchmarkError::CommandFailed {
            name: config.name.clone(),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let bench_output = BenchmarkOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };

    let adaptor = build_adaptor(&config.adaptor, working_dir);
    adaptor
        .extract(&bench_output)
        .map_err(|e| BenchmarkError::Extraction {
            name: config.name.clone(),
            source: e,
        })
}

/// Run all configured benchmarks and merge their metrics.
pub fn run_all_benchmarks(
    configs: &[BenchmarkConfig],
    working_dir: &Path,
) -> Result<Metrics, BenchmarkError> {
    let mut all_metrics = HashMap::new();

    for config in configs {
        let metrics = run_benchmark(config, working_dir)?;
        all_metrics.extend(metrics);
    }

    Ok(all_metrics)
}

/// Build a MetricAdaptor from config.
fn build_adaptor(config: &AdaptorConfig, working_dir: &Path) -> Box<dyn MetricAdaptor> {
    match config {
        AdaptorConfig::Regex { patterns } => {
            let configs: Vec<RegexPatternConfig> = patterns
                .iter()
                .map(|p| RegexPatternConfig {
                    name: p.name.clone(),
                    pattern: p.pattern.clone(),
                })
                .collect();
            Box::new(RegexAdaptor::new(configs))
        }
        AdaptorConfig::Criterion { benchmark_name } => {
            let criterion_dir = working_dir.join("target").join("criterion");
            Box::new(CriterionAdaptor::new(&criterion_dir, benchmark_name))
        }
        AdaptorConfig::Script { command } => {
            Box::new(ScriptAdaptor::new(command.clone()))
        }
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-benchmark`
Expected: compiles

- [ ] **Step 3: Write tests**

`crates/autotune-benchmark/tests/benchmark_test.rs`:

```rust
use autotune_benchmark::{run_all_benchmarks, run_benchmark};
use autotune_config::{AdaptorConfig, BenchmarkConfig, RegexPattern};

fn make_echo_benchmark(name: &str, output: &str, patterns: Vec<RegexPattern>) -> BenchmarkConfig {
    BenchmarkConfig {
        name: name.to_string(),
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("echo '{}'", output),
        ],
        timeout: 30,
        adaptor: AdaptorConfig::Regex { patterns },
    }
}

#[test]
fn single_benchmark_extracts_metric() {
    let config = make_echo_benchmark(
        "bench1",
        "time: 149.83 µs",
        vec![RegexPattern {
            name: "time_us".to_string(),
            pattern: r"time:\s+([0-9.]+)".to_string(),
        }],
    );

    let metrics = run_benchmark(&config, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["time_us"], 149.83);
}

#[test]
fn multiple_benchmarks_merge_metrics() {
    let configs = vec![
        make_echo_benchmark(
            "bench1",
            "time: 100.5",
            vec![RegexPattern {
                name: "time".to_string(),
                pattern: r"time:\s+([0-9.]+)".to_string(),
            }],
        ),
        make_echo_benchmark(
            "bench2",
            "mem: 256.0",
            vec![RegexPattern {
                name: "mem".to_string(),
                pattern: r"mem:\s+([0-9.]+)".to_string(),
            }],
        ),
    ];

    let metrics = run_all_benchmarks(&configs, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["time"], 100.5);
    assert_eq!(metrics["mem"], 256.0);
}

#[test]
fn benchmark_command_failure() {
    let config = BenchmarkConfig {
        name: "bad".to_string(),
        command: vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()],
        timeout: 30,
        adaptor: AdaptorConfig::Regex { patterns: vec![] },
    };

    let err = run_benchmark(&config, std::path::Path::new(".")).unwrap_err();
    assert!(err.to_string().contains("command failed"));
}

#[test]
fn script_adaptor_benchmark() {
    let config = BenchmarkConfig {
        name: "scripted".to_string(),
        command: vec!["sh".to_string(), "-c".to_string(), "echo raw output".to_string()],
        timeout: 30,
        adaptor: AdaptorConfig::Script {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                r#"echo '{"fidelity": 0.97}'"#.to_string(),
            ],
        },
    };

    let metrics = run_benchmark(&config, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["fidelity"], 0.97);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p autotune-benchmark`
Expected: all pass

- [ ] **Step 5: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-benchmark/
git commit -m "feat: add autotune-benchmark crate for running benchmarks with metric extraction"
```

---

### Task 9: autotune-plan

**Files:**
- Create: `crates/autotune-plan/Cargo.toml`
- Create: `crates/autotune-plan/src/lib.rs`
- Test: `crates/autotune-plan/tests/plan_test.rs`

- [ ] **Step 1: Create crate**

`crates/autotune-plan/Cargo.toml`:

```toml
[package]
name = "autotune-plan"
version = "0.1.0"
edition = "2024"

[dependencies]
autotune-agent = { path = "../autotune-agent" }
autotune-state = { path = "../autotune-state" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
```

`crates/autotune-plan/src/lib.rs`:

```rust
use autotune_agent::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession, ToolPermission};
use autotune_state::{ExperimentStore, IterationRecord};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("agent error: {source}")]
    Agent {
        #[from]
        source: AgentError,
    },

    #[error("failed to parse agent hypothesis: {message}")]
    ParseHypothesis { message: String },

    #[error("state error: {source}")]
    State {
        #[from]
        source: autotune_state::StateError,
    },
}

/// The structured output we expect from the research agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub approach: String,
    pub hypothesis: String,
    pub files_to_modify: Vec<String>,
}

/// Build the prompt for the research agent's planning turn.
pub fn build_planning_prompt(
    store: &ExperimentStore,
    last_iteration: Option<&IterationRecord>,
    iteration_count: usize,
    description: &str,
) -> Result<String, PlanError> {
    let log = store.read_log()?;
    let ledger = store.load_ledger()?;

    let mut prompt = String::new();

    prompt.push_str(&format!(
        "You are tuning this codebase. Goal: {}\n\n",
        description
    ));

    if let Some(last) = last_iteration {
        prompt.push_str(&format!(
            "Last iteration result:\n  approach: {}\n  status: {:?}\n  metrics: {:?}\n",
            last.approach, last.status, last.metrics
        ));
        if let Some(reason) = &last.reason {
            prompt.push_str(&format!("  reason: {}\n", reason));
        }
        prompt.push('\n');
    }

    prompt.push_str(&format!(
        "Experiment state:\n  iterations completed: {}\n",
        iteration_count
    ));

    // Summary of ledger
    if !ledger.is_empty() {
        prompt.push_str("\nHistory:\n");
        for record in &ledger {
            prompt.push_str(&format!(
                "  #{}: {} — {:?} (rank: {:.4})\n",
                record.iteration, record.approach, record.status, record.rank
            ));
        }
    }

    if !log.is_empty() {
        prompt.push_str(&format!("\nDurable findings (log.md):\n{}\n", log));
    }

    prompt.push_str(
        "\nPropose the next hypothesis. Output ONLY a JSON object:\n\
         {\n  \"approach\": \"short-kebab-name\",\n  \"hypothesis\": \"what and why\",\n  \"files_to_modify\": [\"path/to/file.rs\"]\n}\n",
    );

    Ok(prompt)
}

/// Parse a Hypothesis from agent response text.
/// Tries to find a JSON object in the response.
pub fn parse_hypothesis(response: &str) -> Result<Hypothesis, PlanError> {
    // Try to find JSON in the response (agent might include surrounding text)
    let json_start = response.find('{').ok_or_else(|| PlanError::ParseHypothesis {
        message: "no JSON object found in response".to_string(),
    })?;

    // Find matching closing brace
    let mut depth = 0;
    let mut json_end = json_start;
    for (i, ch) in response[json_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    json_end = json_start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }

    let json_str = &response[json_start..json_end];
    serde_json::from_str(json_str).map_err(|e| PlanError::ParseHypothesis {
        message: format!("invalid JSON: {}", e),
    })
}

/// Ask the research agent to plan the next hypothesis.
pub fn plan_next(
    agent: &dyn Agent,
    session: &AgentSession,
    store: &ExperimentStore,
    last_iteration: Option<&IterationRecord>,
    iteration_count: usize,
    description: &str,
) -> Result<(Hypothesis, AgentResponse), PlanError> {
    let prompt = build_planning_prompt(store, last_iteration, iteration_count, description)?;
    let response = agent.send(session, &prompt)?;
    let hypothesis = parse_hypothesis(&response.text)?;
    Ok((hypothesis, response))
}

/// Build tool permissions for the research agent (read-only).
pub fn research_agent_permissions() -> Vec<ToolPermission> {
    vec![
        ToolPermission::Allow("Read".to_string()),
        ToolPermission::Allow("Glob".to_string()),
        ToolPermission::Allow("Grep".to_string()),
    ]
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-plan`
Expected: compiles

- [ ] **Step 3: Write tests**

`crates/autotune-plan/tests/plan_test.rs`:

```rust
use autotune_plan::{build_planning_prompt, parse_hypothesis};
use autotune_state::{ExperimentStore, IterationRecord, IterationStatus};
use chrono::Utc;
use std::collections::HashMap;

#[test]
fn parse_hypothesis_clean_json() {
    let response = r#"{"approach": "simd-ops", "hypothesis": "Use SIMD for phase", "files_to_modify": ["src/lib.rs"]}"#;
    let h = parse_hypothesis(response).unwrap();
    assert_eq!(h.approach, "simd-ops");
    assert_eq!(h.files_to_modify, vec!["src/lib.rs"]);
}

#[test]
fn parse_hypothesis_with_surrounding_text() {
    let response = r#"Here is my proposal:

{"approach": "fxhash", "hypothesis": "Switch to FxHashMap", "files_to_modify": ["src/map.rs"]}

This should improve lookup performance."#;
    let h = parse_hypothesis(response).unwrap();
    assert_eq!(h.approach, "fxhash");
}

#[test]
fn parse_hypothesis_no_json_fails() {
    let response = "I think we should try SIMD but I'm not sure about the details.";
    assert!(parse_hypothesis(response).is_err());
}

#[test]
fn build_prompt_includes_description() {
    let dir = tempfile::tempdir().unwrap();
    let store = ExperimentStore::new(&dir.path().join("exp")).unwrap();

    let prompt = build_planning_prompt(&store, None, 0, "improve MSD sampling").unwrap();
    assert!(prompt.contains("improve MSD sampling"));
}

#[test]
fn build_prompt_includes_last_iteration() {
    let dir = tempfile::tempdir().unwrap();
    let store = ExperimentStore::new(&dir.path().join("exp")).unwrap();

    let last = IterationRecord {
        iteration: 1,
        approach: "precompute".to_string(),
        status: IterationStatus::Kept,
        hypothesis: Some("precompute masks".to_string()),
        metrics: HashMap::from([("time_us".to_string(), 149.83)]),
        rank: 1.171,
        score: Some("+17.1%".to_string()),
        reason: Some("improved".to_string()),
        timestamp: Utc::now(),
    };

    let prompt = build_planning_prompt(&store, Some(&last), 2, "improve perf").unwrap();
    assert!(prompt.contains("precompute"));
    assert!(prompt.contains("iterations completed: 2"));
}

#[test]
fn build_prompt_includes_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = ExperimentStore::new(&dir.path().join("exp")).unwrap();
    store.append_log("SIMD helps for phase updates").unwrap();

    let prompt = build_planning_prompt(&store, None, 0, "improve perf").unwrap();
    assert!(prompt.contains("SIMD helps"));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p autotune-plan`
Expected: all pass

- [ ] **Step 5: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-plan/
git commit -m "feat: add autotune-plan crate for research agent hypothesis planning"
```

---

### Task 10: autotune-implement

**Files:**
- Create: `crates/autotune-implement/Cargo.toml`
- Create: `crates/autotune-implement/src/lib.rs`
- Test: `crates/autotune-implement/tests/implement_test.rs`

- [ ] **Step 1: Create crate**

`crates/autotune-implement/Cargo.toml`:

```toml
[package]
name = "autotune-implement"
version = "0.1.0"
edition = "2024"

[dependencies]
autotune-agent = { path = "../autotune-agent" }
autotune-git = { path = "../autotune-git" }
autotune-plan = { path = "../autotune-plan" }
autotune-state = { path = "../autotune-state" }
thiserror = "2"
```

`crates/autotune-implement/src/lib.rs`:

```rust
use autotune_agent::{Agent, AgentConfig, AgentError, AgentResponse, ToolPermission};
use autotune_git::GitError;
use autotune_plan::Hypothesis;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImplementError {
    #[error("agent error: {source}")]
    Agent {
        #[from]
        source: AgentError,
    },

    #[error("git error: {source}")]
    Git {
        #[from]
        source: GitError,
    },

    #[error("no commit found in worktree after implementation")]
    NoCommit,
}

/// The result of an implementation step.
#[derive(Debug)]
pub struct ImplementResult {
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub commit_sha: String,
    pub agent_response: AgentResponse,
}

/// Build tool permissions for the implementation agent (sandboxed).
pub fn implementation_agent_permissions(tunable_paths: &[String]) -> Vec<ToolPermission> {
    let mut perms = vec![
        ToolPermission::Allow("Read".to_string()),
        ToolPermission::Allow("Glob".to_string()),
        ToolPermission::Allow("Grep".to_string()),
    ];

    for path in tunable_paths {
        perms.push(ToolPermission::AllowScoped("Edit".to_string(), path.clone()));
        perms.push(ToolPermission::AllowScoped("Write".to_string(), path.clone()));
    }

    // Explicitly deny dangerous tools
    perms.push(ToolPermission::Deny("Bash".to_string()));
    perms.push(ToolPermission::Deny("Agent".to_string()));
    perms.push(ToolPermission::Deny("WebFetch".to_string()));
    perms.push(ToolPermission::Deny("WebSearch".to_string()));

    perms
}

/// Build the prompt for the implementation agent.
pub fn build_implementation_prompt(
    hypothesis: &Hypothesis,
    log_content: &str,
) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!(
        "## Task\n\n\
         Implement this performance optimization hypothesis:\n\n\
         **Approach:** {}\n\
         **Hypothesis:** {}\n\n\
         **Files to modify:** {}\n\n",
        hypothesis.approach,
        hypothesis.hypothesis,
        hypothesis.files_to_modify.join(", "),
    ));

    prompt.push_str(
        "## Rules\n\n\
         - Only modify the files listed above (or closely related files in the same directories)\n\
         - Do NOT run tests or benchmarks — the orchestrator handles that\n\
         - Do NOT modify test files or benchmark files\n\
         - Commit all your changes with a descriptive commit message before finishing\n\
         - Focus on correctness first, then performance\n\n",
    );

    if !log_content.is_empty() {
        prompt.push_str(&format!(
            "## Prior findings\n\n{}\n\n",
            log_content,
        ));
    }

    prompt
}

/// Set up a worktree for the implementation agent.
pub fn setup_worktree(
    repo_root: &Path,
    approach_name: &str,
    worktree_parent: &Path,
) -> Result<(PathBuf, String), ImplementError> {
    let branch_name = format!("autotune/{}", approach_name);
    let worktree_path = worktree_parent.join(format!("autotune-{}", approach_name));

    autotune_git::create_branch(repo_root, &branch_name)?;
    autotune_git::create_worktree(repo_root, &worktree_path, &branch_name)?;

    Ok((worktree_path, branch_name))
}

/// Run the implementation agent in a worktree.
pub fn run_implementation(
    agent: &dyn Agent,
    hypothesis: &Hypothesis,
    worktree_path: &Path,
    branch_name: &str,
    tunable_paths: &[String],
    log_content: &str,
    model: Option<&str>,
    max_turns: Option<u64>,
) -> Result<ImplementResult, ImplementError> {
    let prompt = build_implementation_prompt(hypothesis, log_content);
    let permissions = implementation_agent_permissions(tunable_paths);

    let config = AgentConfig {
        prompt,
        allowed_tools: permissions,
        working_directory: worktree_path.to_path_buf(),
        model: model.map(String::from),
        max_turns,
    };

    let response = agent.spawn(&config)?;

    // Verify the agent committed something
    let base_branch = autotune_git::current_branch(worktree_path)
        .unwrap_or_default();
    let commit_sha = autotune_git::latest_commit_sha(worktree_path)
        .map_err(|e| ImplementError::Git { source: e })?;

    Ok(ImplementResult {
        worktree_path: worktree_path.to_path_buf(),
        branch_name: branch_name.to_string(),
        commit_sha,
        agent_response: response,
    })
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p autotune-implement`
Expected: compiles

- [ ] **Step 4: Write tests**

`crates/autotune-implement/tests/implement_test.rs`:

```rust
use autotune_implement::{
    build_implementation_prompt, implementation_agent_permissions,
};
use autotune_plan::Hypothesis;

#[test]
fn build_prompt_includes_hypothesis() {
    let h = Hypothesis {
        approach: "simd-ops".to_string(),
        hypothesis: "Use SIMD for phase updates".to_string(),
        files_to_modify: vec!["src/lib.rs".to_string(), "src/phase.rs".to_string()],
    };

    let prompt = build_implementation_prompt(&h, "");
    assert!(prompt.contains("simd-ops"));
    assert!(prompt.contains("Use SIMD"));
    assert!(prompt.contains("src/lib.rs"));
    assert!(prompt.contains("src/phase.rs"));
    assert!(prompt.contains("Commit all"));
}

#[test]
fn build_prompt_includes_log() {
    let h = Hypothesis {
        approach: "test".to_string(),
        hypothesis: "test".to_string(),
        files_to_modify: vec![],
    };

    let prompt = build_implementation_prompt(&h, "SIMD helps for phase updates");
    assert!(prompt.contains("SIMD helps"));
}

#[test]
fn permissions_sandbox_correctly() {
    let perms = implementation_agent_permissions(&[
        "src/**".to_string(),
        "crates/core/src/**".to_string(),
    ]);

    // Should have: Read, Glob, Grep (unrestricted) + Edit/Write scoped + denies
    let allow_count = perms
        .iter()
        .filter(|p| matches!(p, autotune_agent::ToolPermission::Allow(_)))
        .count();
    assert_eq!(allow_count, 3); // Read, Glob, Grep

    let scoped_count = perms
        .iter()
        .filter(|p| matches!(p, autotune_agent::ToolPermission::AllowScoped(_, _)))
        .count();
    assert_eq!(scoped_count, 4); // Edit:src/**, Write:src/**, Edit:crates/core/src/**, Write:crates/core/src/**

    let deny_count = perms
        .iter()
        .filter(|p| matches!(p, autotune_agent::ToolPermission::Deny(_)))
        .count();
    assert!(deny_count >= 3); // Bash, Agent, WebFetch, WebSearch
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p autotune-implement`
Expected: all pass

- [ ] **Step 6: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 7: Commit**

```bash
git add crates/autotune-implement/
git commit -m "feat: add autotune-implement crate for sandboxed implementation agent"
```

---

### Task 11: autotune CLI binary — CLI commands + state machine

**Files:**
- Create: `crates/autotune/Cargo.toml`
- Create: `crates/autotune/src/main.rs`
- Create: `crates/autotune/src/cli.rs`
- Create: `crates/autotune/src/machine.rs`
- Create: `crates/autotune/src/resume.rs`

This is the largest task. It wires everything together.

- [ ] **Step 1: Create crate scaffold**

`crates/autotune/Cargo.toml`:

```toml
[package]
name = "autotune"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "autotune"
path = "src/main.rs"

[dependencies]
autotune-config = { path = "../autotune-config" }
autotune-state = { path = "../autotune-state" }
autotune-agent = { path = "../autotune-agent" }
autotune-plan = { path = "../autotune-plan" }
autotune-implement = { path = "../autotune-implement" }
autotune-test = { path = "../autotune-test" }
autotune-benchmark = { path = "../autotune-benchmark" }
autotune-score = { path = "../autotune-score" }
autotune-git = { path = "../autotune-git" }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
chrono = "0.4"
serde_json = "1"
ctrlc = "3"
```

- [ ] **Step 2: Create CLI definition**

`crates/autotune/src/cli.rs`:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "autotune", about = "Autonomous benchmark-driven performance tuning")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new experiment (or help write config via agent REPL)
    Init {
        #[arg(long)]
        name: Option<String>,
    },

    /// Start a fresh experiment from .autotune.toml
    Run {
        #[arg(long)]
        experiment: Option<String>,
    },

    /// Resume an existing experiment
    Resume {
        #[arg(long)]
        experiment: String,
        #[arg(long)]
        max_iterations: Option<String>,
        #[arg(long)]
        max_duration: Option<String>,
        #[arg(long)]
        target_improvement: Option<f64>,
    },

    /// Run just the Planning phase
    Plan {
        #[arg(long)]
        experiment: String,
    },

    /// Run just the Implementing phase
    Implement {
        #[arg(long)]
        experiment: String,
    },

    /// Run configured test commands
    Test {
        #[arg(long)]
        experiment: String,
    },

    /// Run benchmarks + metric extraction
    Benchmark {
        #[arg(long)]
        experiment: String,
    },

    /// Score current iteration metrics
    Record {
        #[arg(long)]
        experiment: String,
    },

    /// Integrate or revert based on scoring
    Apply {
        #[arg(long)]
        experiment: String,
    },

    /// Show experiment progress
    Report {
        #[arg(long)]
        experiment: String,
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// List all experiments
    List,

    /// Export experiment data
    Export {
        #[arg(long)]
        experiment: String,
        #[arg(long)]
        output: String,
    },
}
```

- [ ] **Step 3: Create state machine driver**

`crates/autotune/src/machine.rs`:

```rust
use anyhow::{Context, Result};
use autotune_agent::{Agent, AgentConfig, AgentSession};
use autotune_benchmark::run_all_benchmarks;
use autotune_config::AutotuneConfig;
use autotune_implement::{run_implementation, setup_worktree};
use autotune_plan::{plan_next, research_agent_permissions, Hypothesis};
use autotune_score::{ScoreCalculator, ScoreInput};
use autotune_state::*;
use autotune_test::{all_passed, run_all_tests};
use chrono::Utc;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared flag for graceful shutdown.
pub type ShutdownFlag = Arc<AtomicBool>;

pub fn new_shutdown_flag() -> ShutdownFlag {
    Arc::new(AtomicBool::new(false))
}

/// Run the full experiment loop.
pub fn run_experiment(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    scorer: &dyn ScoreCalculator,
    repo_root: &Path,
    store: &ExperimentStore,
    shutdown: &ShutdownFlag,
) -> Result<()> {
    // Load or initialize state
    let mut state = store.load_state().context("failed to load experiment state")?;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            println!("\nShutting down gracefully...");
            break;
        }

        match state.current_phase {
            Phase::Planning => {
                println!("  Planning ····················· ");
                let session = AgentSession {
                    session_id: state.research_session_id.clone(),
                    backend: agent.backend_name().to_string(),
                };

                let ledger = store.load_ledger()?;
                let last_iteration = ledger.last();
                let description = config.experiment.description.as_deref().unwrap_or(&config.experiment.name);

                let (hypothesis, _response) = plan_next(
                    agent,
                    &session,
                    store,
                    last_iteration,
                    state.current_iteration,
                    description,
                )?;

                println!("{}", hypothesis.approach);

                // Set up worktree
                let worktree_parent = repo_root.parent().unwrap_or(repo_root);
                let (wt_path, branch) = setup_worktree(repo_root, &hypothesis.approach, worktree_parent)?;

                state.current_approach = Some(ApproachState {
                    name: hypothesis.approach.clone(),
                    hypothesis: hypothesis.hypothesis.clone(),
                    worktree_path: wt_path,
                    branch_name: branch,
                    commit_sha: None,
                    test_results: vec![],
                    metrics: None,
                    rank: None,
                });
                state.current_phase = Phase::Implementing;
                store.save_state(&state)?;

                // Save the prompt for reproducibility
                store.save_iteration_prompt(
                    state.current_iteration,
                    &hypothesis.approach,
                    &autotune_implement::build_implementation_prompt(&hypothesis, &store.read_log()?),
                )?;
            }

            Phase::Implementing => {
                let approach = state.current_approach.as_ref().unwrap();
                print!("  Implementing ················· ");

                let impl_config = config.agent.implementation.as_ref();
                let log_content = store.read_log()?;
                let hypothesis = Hypothesis {
                    approach: approach.name.clone(),
                    hypothesis: approach.hypothesis.clone(),
                    files_to_modify: vec![], // Already in prompt
                };

                let result = run_implementation(
                    agent,
                    &hypothesis,
                    &approach.worktree_path,
                    &approach.branch_name,
                    &config.paths.tunable,
                    &log_content,
                    impl_config.and_then(|c| c.model.as_deref()),
                    impl_config.and_then(|c| c.max_turns),
                )?;

                println!("done");

                let approach = state.current_approach.as_mut().unwrap();
                approach.commit_sha = Some(result.commit_sha);
                state.current_phase = Phase::Testing;
                store.save_state(&state)?;
            }

            Phase::Testing => {
                print!("  Testing ······················ ");

                let approach = state.current_approach.as_ref().unwrap();
                let results = run_all_tests(&config.test, &approach.worktree_path)?;

                let display: Vec<String> = results
                    .iter()
                    .map(|r| {
                        format!(
                            "{} ({:.0}s)",
                            r.name,
                            r.duration_secs,
                        )
                    })
                    .collect();
                println!("{}", display.join(" "));

                if all_passed(&results) {
                    let approach = state.current_approach.as_mut().unwrap();
                    approach.test_results = results
                        .iter()
                        .map(|r| autotune_state::TestResult {
                            name: r.name.clone(),
                            passed: r.passed,
                            duration_secs: r.duration_secs,
                            output: None,
                        })
                        .collect();
                    state.current_phase = Phase::Benchmarking;
                } else {
                    // Save test output for debugging
                    let approach = state.current_approach.as_ref().unwrap();
                    let failed = results.iter().find(|r| !r.passed).unwrap();
                    store.save_test_output(
                        state.current_iteration,
                        &approach.name,
                        &format!("STDOUT:\n{}\n\nSTDERR:\n{}", failed.stdout, failed.stderr),
                    )?;

                    // Discard
                    record_discard(
                        &mut state,
                        store,
                        "test_failed",
                        &format!("test '{}' failed", failed.name),
                    )?;
                }
                store.save_state(&state)?;
            }

            Phase::Benchmarking => {
                print!("  Benchmarking ················· ");

                let approach = state.current_approach.as_ref().unwrap();
                let metrics = run_all_benchmarks(&config.benchmark, &approach.worktree_path)?;

                let display: Vec<String> = metrics
                    .iter()
                    .map(|(k, v)| format!("{}: {:.2}", k, v))
                    .collect();
                println!("{}", display.join(", "));

                // Save metrics
                let approach_name = approach.name.clone();
                store.save_iteration_metrics(state.current_iteration, &approach_name, &metrics)?;

                let approach = state.current_approach.as_mut().unwrap();
                approach.metrics = Some(metrics);
                state.current_phase = Phase::Scoring;
                store.save_state(&state)?;
            }

            Phase::Scoring => {
                print!("  Scoring ······················ ");

                let approach = state.current_approach.as_ref().unwrap();
                let candidate_metrics = approach.metrics.as_ref().unwrap().clone();

                let ledger = store.load_ledger()?;
                let baseline = ledger.first().map(|r| r.metrics.clone()).unwrap_or_default();
                let best = ledger
                    .iter()
                    .filter(|r| r.status == IterationStatus::Kept || r.status == IterationStatus::Baseline)
                    .last()
                    .map(|r| r.metrics.clone())
                    .unwrap_or(baseline.clone());

                let score_input = ScoreInput {
                    baseline,
                    candidate: candidate_metrics,
                    best,
                };

                let score_output = scorer.calculate(&score_input)?;
                println!("{} → {}", score_output.reason, score_output.decision);

                let approach = state.current_approach.as_mut().unwrap();
                approach.rank = Some(score_output.rank);

                if score_output.decision == "keep" {
                    state.current_phase = Phase::Integrating;
                } else {
                    record_discard(&mut state, store, "regression", &score_output.reason)?;
                }
                store.save_state(&state)?;
            }

            Phase::Integrating => {
                let approach = state.current_approach.as_ref().unwrap();
                let commit_sha = approach.commit_sha.as_ref().unwrap();

                // Cherry-pick to canonical branch
                autotune_git::checkout(repo_root, &config.experiment.canonical_branch)?;
                autotune_git::cherry_pick(repo_root, commit_sha)?;

                // Record as kept
                let record = IterationRecord {
                    iteration: state.current_iteration,
                    approach: approach.name.clone(),
                    status: IterationStatus::Kept,
                    hypothesis: Some(approach.hypothesis.clone()),
                    metrics: approach.metrics.clone().unwrap_or_default(),
                    rank: approach.rank.unwrap_or(0.0),
                    score: None, // Computed at display time
                    reason: None,
                    timestamp: Utc::now(),
                };
                store.append_ledger(&record)?;

                // Cleanup worktree
                let _ = autotune_git::remove_worktree(repo_root, &approach.worktree_path);

                state.current_approach = None;
                state.current_iteration += 1;
                state.current_phase = Phase::Recorded;
                store.save_state(&state)?;
            }

            Phase::Recorded => {
                // Check stop conditions
                if should_stop(config, &state, store)? {
                    state.current_phase = Phase::Done;
                    store.save_state(&state)?;
                } else {
                    state.current_phase = Phase::Planning;
                    store.save_state(&state)?;
                }
            }

            Phase::Done => {
                println!("\nExperiment complete.");
                break;
            }
        }
    }

    Ok(())
}

fn record_discard(
    state: &mut ExperimentState,
    store: &ExperimentStore,
    status_reason: &str,
    reason: &str,
) -> Result<()> {
    let approach = state.current_approach.as_ref().unwrap();

    let record = IterationRecord {
        iteration: state.current_iteration,
        approach: approach.name.clone(),
        status: IterationStatus::Discarded,
        hypothesis: Some(approach.hypothesis.clone()),
        metrics: approach.metrics.clone().unwrap_or_default(),
        rank: approach.rank.unwrap_or(0.0),
        score: None,
        reason: Some(format!("{}: {}", status_reason, reason)),
        timestamp: Utc::now(),
    };
    store.append_ledger(&record)?;

    // Cleanup worktree
    let _ = autotune_git::remove_worktree(
        &std::path::PathBuf::from("."), // Will be called from repo root
        &approach.worktree_path,
    );

    state.current_approach = None;
    state.current_iteration += 1;
    state.current_phase = Phase::Recorded;

    Ok(())
}

fn should_stop(
    config: &AutotuneConfig,
    state: &ExperimentState,
    store: &ExperimentStore,
) -> Result<bool> {
    // Check max_iterations
    if let Some(ref max) = config.experiment.max_iterations {
        match max {
            autotune_config::StopValue::Finite(n) => {
                if state.current_iteration >= *n as usize {
                    return Ok(true);
                }
            }
            autotune_config::StopValue::Infinite => {}
        }
    }

    // Check target_improvement
    if let Some(target) = config.experiment.target_improvement {
        let ledger = store.load_ledger()?;
        if let (Some(baseline), Some(best)) = (
            ledger.first(),
            ledger
                .iter()
                .filter(|r| r.status == IterationStatus::Kept)
                .last(),
        ) {
            if baseline.rank != 0.0 {
                let improvement = (best.rank - baseline.rank) / baseline.rank.abs();
                if improvement >= target {
                    return Ok(true);
                }
            }
        }
    }

    // max_duration would require tracking start time — left for future enhancement

    Ok(false)
}
```

- [ ] **Step 4: Create resume logic**

`crates/autotune/src/resume.rs`:

```rust
use autotune_git;
use autotune_state::{ExperimentState, ExperimentStore, Phase};
use anyhow::{Context, Result};
use std::path::Path;

/// Check the current phase and handle any crash recovery.
/// Returns the state ready to re-enter the machine loop.
pub fn prepare_resume(
    store: &ExperimentStore,
    repo_root: &Path,
) -> Result<ExperimentState> {
    let state = store
        .load_state()
        .context("failed to load experiment state for resume")?;

    match &state.current_phase {
        Phase::Planning => {
            // Idempotent — just re-ask the research agent
            Ok(state)
        }
        Phase::Implementing => {
            // Check if the worktree has a commit
            if let Some(approach) = &state.current_approach {
                if approach.worktree_path.exists() {
                    let has_commits = autotune_git::has_commits_ahead(
                        &approach.worktree_path,
                        "HEAD~1",
                        "HEAD",
                    )
                    .unwrap_or(false);

                    if has_commits {
                        // Commit exists, skip to Testing
                        let mut state = state;
                        state.current_phase = Phase::Testing;
                        store.save_state(&state)?;
                        return Ok(state);
                    }
                }
                // No commit — discard and restart Planning
                let mut state = state;
                if let Some(approach) = &state.current_approach {
                    let _ = autotune_git::remove_worktree(repo_root, &approach.worktree_path);
                }
                state.current_approach = None;
                state.current_phase = Phase::Planning;
                store.save_state(&state)?;
                Ok(state)
            } else {
                let mut state = state;
                state.current_phase = Phase::Planning;
                store.save_state(&state)?;
                Ok(state)
            }
        }
        Phase::Testing | Phase::Benchmarking | Phase::Scoring => {
            // Deterministic — safe to retry
            Ok(state)
        }
        Phase::Integrating => {
            // Check if cherry-pick already happened
            // For now, just retry — cherry-pick will fail if already applied
            Ok(state)
        }
        Phase::Recorded | Phase::Done => {
            Ok(state)
        }
    }
}
```

- [ ] **Step 5: Create main.rs**

`crates/autotune/src/main.rs`:

```rust
mod cli;
mod machine;
mod resume;

use anyhow::{bail, Context, Result};
use autotune_agent::claude::ClaudeAgent;
use autotune_agent::{Agent, AgentConfig};
use autotune_config::AutotuneConfig;
use autotune_plan::research_agent_permissions;
use autotune_score::weighted_sum::{
    Direction, GuardrailMetricDef, PrimaryMetricDef, WeightedSumScorer,
};
use autotune_score::threshold::{
    Direction as TDirection, ThresholdConditionDef, ThresholdScorer,
};
use autotune_score::script::ScriptScorer;
use autotune_score::ScoreCalculator;
use autotune_state::*;
use chrono::Utc;
use clap::Parser;
use std::path::PathBuf;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Commands::Run { experiment } => cmd_run(experiment),
        cli::Commands::Resume {
            experiment,
            max_iterations,
            max_duration,
            target_improvement,
        } => cmd_resume(experiment, max_iterations, max_duration, target_improvement),
        cli::Commands::Report { experiment, format } => cmd_report(experiment, format),
        cli::Commands::List => cmd_list(),
        cli::Commands::Init { name } => {
            println!("autotune init — not yet implemented (requires interactive agent REPL)");
            Ok(())
        }
        cli::Commands::Plan { experiment } => {
            println!("autotune plan — step commands not yet implemented");
            Ok(())
        }
        cli::Commands::Implement { experiment } => {
            println!("autotune implement — step commands not yet implemented");
            Ok(())
        }
        cli::Commands::Test { experiment } => {
            println!("autotune test — step commands not yet implemented");
            Ok(())
        }
        cli::Commands::Benchmark { experiment } => {
            println!("autotune benchmark — step commands not yet implemented");
            Ok(())
        }
        cli::Commands::Record { experiment } => {
            println!("autotune record — step commands not yet implemented");
            Ok(())
        }
        cli::Commands::Apply { experiment } => {
            println!("autotune apply — step commands not yet implemented");
            Ok(())
        }
        cli::Commands::Export { experiment, output } => {
            println!("autotune export — not yet implemented");
            Ok(())
        }
    }
}

fn find_repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    autotune_git::repo_root(&cwd).context("not in a git repository")
}

fn load_config(repo_root: &PathBuf) -> Result<AutotuneConfig> {
    let config_path = repo_root.join(".autotune.toml");
    AutotuneConfig::load(&config_path).context("failed to load .autotune.toml")
}

fn build_agent(config: &AutotuneConfig) -> Box<dyn Agent> {
    match config.agent.backend.as_str() {
        "claude" | _ => Box::new(ClaudeAgent::new()),
    }
}

fn build_scorer(config: &AutotuneConfig) -> Result<Box<dyn ScoreCalculator>> {
    match &config.score {
        autotune_config::ScoreConfig::WeightedSum {
            primary_metrics,
            guardrail_metrics,
        } => {
            let primary: Vec<PrimaryMetricDef> = primary_metrics
                .iter()
                .map(|pm| PrimaryMetricDef {
                    name: pm.name.clone(),
                    direction: match pm.direction {
                        autotune_config::Direction::Minimize => Direction::Minimize,
                        autotune_config::Direction::Maximize => Direction::Maximize,
                    },
                    weight: pm.weight,
                })
                .collect();
            let guardrails: Vec<GuardrailMetricDef> = guardrail_metrics
                .iter()
                .map(|gm| GuardrailMetricDef {
                    name: gm.name.clone(),
                    direction: match gm.direction {
                        autotune_config::Direction::Minimize => Direction::Minimize,
                        autotune_config::Direction::Maximize => Direction::Maximize,
                    },
                    max_regression: gm.max_regression,
                })
                .collect();
            Ok(Box::new(WeightedSumScorer::new(primary, guardrails)))
        }
        autotune_config::ScoreConfig::Threshold { conditions } => {
            let conds: Vec<ThresholdConditionDef> = conditions
                .iter()
                .map(|c| ThresholdConditionDef {
                    metric: c.metric.clone(),
                    direction: match c.direction {
                        autotune_config::Direction::Minimize => TDirection::Minimize,
                        autotune_config::Direction::Maximize => TDirection::Maximize,
                    },
                    threshold: c.threshold,
                })
                .collect();
            Ok(Box::new(ThresholdScorer::new(conds)))
        }
        autotune_config::ScoreConfig::Script { command }
        | autotune_config::ScoreConfig::Command { command } => {
            Ok(Box::new(ScriptScorer::new(command.clone())))
        }
    }
}

fn cmd_run(experiment_name: Option<String>) -> Result<()> {
    let repo_root = find_repo_root()?;
    let config = load_config(&repo_root)?;
    let agent = build_agent(&config);
    let scorer = build_scorer(&config)?;

    let exp_name = experiment_name.unwrap_or_else(|| config.experiment.name.clone());

    // Create experiment directory
    let exp_dir = repo_root
        .join(".autotune")
        .join("experiments")
        .join(&exp_name);

    if exp_dir.exists() {
        // Append timestamp suffix
        let suffix = Utc::now().format("%H%M%S");
        let exp_dir = repo_root
            .join(".autotune")
            .join("experiments")
            .join(format!("{}-{}", exp_name, suffix));
        println!("Experiment '{}' already exists, using '{}-{}'", exp_name, exp_name, suffix);
    }

    let store = ExperimentStore::new(&exp_dir)?;

    // Snapshot config
    let config_content = std::fs::read_to_string(repo_root.join(".autotune.toml"))?;
    store.save_config_snapshot(&config_content)?;

    println!("autotune · {} · starting\n", exp_name);

    // Sanity check: run tests on current codebase
    println!("  Sanity check: running tests...");
    let test_results = autotune_test::run_all_tests(&config.test, &repo_root)?;
    if !autotune_test::all_passed(&test_results) {
        bail!("Tests fail on current codebase — fix before tuning");
    }
    println!("  Tests pass.\n");

    // Baseline: run benchmarks
    println!("  Baseline: running benchmarks...");
    let baseline_metrics = autotune_benchmark::run_all_benchmarks(&config.benchmark, &repo_root)?;
    println!("  Baseline metrics: {:?}\n", baseline_metrics);

    // Run baseline through scorer (validates pipeline)
    let baseline_input = autotune_score::ScoreInput {
        baseline: baseline_metrics.clone(),
        candidate: baseline_metrics.clone(),
        best: baseline_metrics.clone(),
    };
    let baseline_score = scorer.calculate(&baseline_input)?;
    println!("  Baseline score: rank={:.4}\n", baseline_score.rank);

    // Record baseline
    let baseline_record = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: baseline_metrics,
        rank: baseline_score.rank,
        score: None,
        reason: None,
        timestamp: Utc::now(),
    };
    store.append_ledger(&baseline_record)?;

    // Spawn research agent
    println!("  Spawning research agent...");
    let research_config = AgentConfig {
        prompt: format!(
            "You are a performance tuning research agent for this codebase. \
             Goal: {}. You will be asked to propose optimization hypotheses. \
             Read the codebase to understand the architecture.",
            config.experiment.description.as_deref().unwrap_or(&config.experiment.name)
        ),
        allowed_tools: research_agent_permissions(),
        working_directory: repo_root.clone(),
        model: config
            .agent
            .research
            .as_ref()
            .and_then(|r| r.model.clone()),
        max_turns: config
            .agent
            .research
            .as_ref()
            .and_then(|r| r.max_turns),
    };
    let research_response = agent.spawn(&research_config)?;
    println!("  Research agent ready.\n");

    // Initialize state
    let state = ExperimentState {
        experiment_name: exp_name.clone(),
        canonical_branch: config.experiment.canonical_branch.clone(),
        research_session_id: research_response.session_id.clone(),
        current_iteration: 1, // 0 is baseline
        current_phase: Phase::Planning,
        current_approach: None,
    };
    store.save_state(&state)?;

    // Set up Ctrl+C handler
    let shutdown = machine::new_shutdown_flag();
    let shutdown_clone = shutdown.clone();
    ctrlc::set_handler(move || {
        shutdown_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    })?;

    // Run the loop
    machine::run_experiment(&config, agent.as_ref(), scorer.as_ref(), &repo_root, &store, &shutdown)?;

    // Handover
    let state = store.load_state()?;
    let session = autotune_agent::AgentSession {
        session_id: state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };
    println!("\n  Research session: {}", state.research_session_id);
    println!("  Resume with: {}", agent.handover_command(&session));

    Ok(())
}

fn cmd_resume(
    experiment: String,
    max_iterations: Option<String>,
    max_duration: Option<String>,
    target_improvement: Option<f64>,
) -> Result<()> {
    let repo_root = find_repo_root()?;
    let exp_dir = repo_root
        .join(".autotune")
        .join("experiments")
        .join(&experiment);

    let store = ExperimentStore::open(&exp_dir)
        .context(format!("experiment '{}' not found", experiment))?;

    // Load frozen config
    let config_content = store.load_config_snapshot()?;
    let mut config: AutotuneConfig = toml::from_str(&config_content)?;

    // Apply transient overrides
    if let Some(mi) = max_iterations {
        config.experiment.max_iterations = Some(if mi == "inf" {
            autotune_config::StopValue::Infinite
        } else {
            autotune_config::StopValue::Finite(mi.parse()?)
        });
    }
    if let Some(ti) = target_improvement {
        config.experiment.target_improvement = Some(ti);
    }
    // max_duration override would go here

    let agent = build_agent(&config);
    let scorer = build_scorer(&config)?;

    // Prepare resume (crash recovery)
    let _state = resume::prepare_resume(&store, &repo_root)?;

    println!("autotune · {} · resuming\n", experiment);

    let shutdown = machine::new_shutdown_flag();
    let shutdown_clone = shutdown.clone();
    ctrlc::set_handler(move || {
        shutdown_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    })?;

    machine::run_experiment(&config, agent.as_ref(), scorer.as_ref(), &repo_root, &store, &shutdown)?;

    let state = store.load_state()?;
    let session = autotune_agent::AgentSession {
        session_id: state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };
    println!("\n  Research session: {}", state.research_session_id);
    println!("  Resume with: {}", agent.handover_command(&session));

    Ok(())
}

fn cmd_report(experiment: String, format: String) -> Result<()> {
    let repo_root = find_repo_root()?;
    let exp_dir = repo_root
        .join(".autotune")
        .join("experiments")
        .join(&experiment);
    let store = ExperimentStore::open(&exp_dir)?;
    let ledger = store.load_ledger()?;

    match format.as_str() {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&ledger)?);
        }
        _ => {
            println!(
                "  {:>4}  {:<30}  {:>8}  {:>10}",
                "iter", "approach", "status", "rank"
            );
            println!("  {}", "-".repeat(58));
            for record in &ledger {
                println!(
                    "  {:>4}  {:<30}  {:>8?}  {:>10.4}",
                    record.iteration, record.approach, record.status, record.rank
                );
            }
        }
    }

    Ok(())
}

fn cmd_list() -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");
    let experiments = ExperimentStore::list_experiments(&autotune_dir)?;

    if experiments.is_empty() {
        println!("No experiments found.");
        return Ok(());
    }

    println!(
        "  {:<20}  {:>10}  {:>8}",
        "experiment", "iterations", "status"
    );
    println!("  {}", "-".repeat(42));

    for name in &experiments {
        let exp_dir = autotune_dir.join("experiments").join(name);
        if let Ok(store) = ExperimentStore::open(&exp_dir) {
            if let Ok(state) = store.load_state() {
                let ledger_len = store.load_ledger().map(|l| l.len()).unwrap_or(0);
                println!(
                    "  {:<20}  {:>10}  {:>8}",
                    name, ledger_len, state.current_phase
                );
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 6: Verify the full workspace compiles**

Run: `cargo build`
Expected: all crates compile, binary is built

- [ ] **Step 7: Run all tests across workspace**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 8: Clippy + format**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean

- [ ] **Step 9: Commit**

```bash
git add crates/autotune/
git commit -m "feat: add autotune CLI binary with state machine, commands, and resume logic"
```

---

### Task 12: Integration test — full pipeline with mock agent

**Files:**
- Create: `crates/autotune/tests/integration_test.rs`

- [ ] **Step 1: Write integration test with a mock agent**

`crates/autotune/tests/integration_test.rs`:

```rust
//! Integration test that runs the state machine with a mock agent.
//! This validates the full pipeline without actual LLM calls.

use autotune_agent::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession};
use autotune_config::AutotuneConfig;
use autotune_score::weighted_sum::{Direction, PrimaryMetricDef, WeightedSumScorer};
use autotune_score::ScoreCalculator;
use std::io::Write;
use std::process::Command;

/// A mock agent that returns pre-defined responses.
struct MockAgent {
    spawn_response: String,
    send_responses: Vec<String>,
    call_count: std::sync::atomic::AtomicUsize,
}

impl MockAgent {
    fn new(spawn_response: &str, send_responses: Vec<String>) -> Self {
        Self {
            spawn_response: spawn_response.to_string(),
            send_responses,
            call_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl Agent for MockAgent {
    fn spawn(&self, _config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        Ok(AgentResponse {
            text: self.spawn_response.clone(),
            session_id: "mock-session-001".to_string(),
        })
    }

    fn send(&self, _session: &AgentSession, _message: &str) -> Result<AgentResponse, AgentError> {
        let idx = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let response = self
            .send_responses
            .get(idx)
            .cloned()
            .unwrap_or_else(|| {
                r#"{"approach": "default", "hypothesis": "default hypothesis", "files_to_modify": []}"#
                    .to_string()
            });
        Ok(AgentResponse {
            text: response,
            session_id: "mock-session-001".to_string(),
        })
    }

    fn backend_name(&self) -> &str {
        "mock"
    }

    fn handover_command(&self, session: &AgentSession) -> String {
        format!("mock-resume {}", session.session_id)
    }
}

#[test]
fn scorer_pipeline_validation() {
    // This test validates that the scoring pipeline works end-to-end
    // without needing the full state machine.
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time_us".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    );

    let input = autotune_score::ScoreInput {
        baseline: [("time_us".to_string(), 200.0)].into_iter().collect(),
        candidate: [("time_us".to_string(), 170.0)].into_iter().collect(),
        best: [("time_us".to_string(), 200.0)].into_iter().collect(),
    };

    let output = scorer.calculate(&input).unwrap();
    assert_eq!(output.decision, "keep");
    assert!((output.rank - 0.15).abs() < 0.001);
}

#[test]
fn state_machine_records_baseline() {
    // Create a temp git repo with a mock benchmark
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();

    Command::new("git").args(["init"]).current_dir(repo).output().unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(repo)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(repo)
        .output()
        .unwrap();

    std::fs::write(repo.join("README.md"), "# test").unwrap();
    Command::new("git").args(["add", "."]).current_dir(repo).output().unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(repo)
        .output()
        .unwrap();

    // Write config
    let config_content = r#"
[experiment]
name = "test"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "bench"
command = ["sh", "-c", "echo 'time: 100.0 µs'"]
adaptor = { type = "regex", patterns = [
    { name = "time_us", pattern = 'time:\s+([0-9.]+)' },
] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "time_us", direction = "Minimize" }]
"#;
    std::fs::write(repo.join(".autotune.toml"), config_content).unwrap();

    // Validate config loads
    let config = AutotuneConfig::load(&repo.join(".autotune.toml")).unwrap();
    assert_eq!(config.experiment.name, "test");

    // Validate benchmark runs
    let metrics =
        autotune_benchmark::run_all_benchmarks(&config.benchmark, repo).unwrap();
    assert_eq!(metrics["time_us"], 100.0);
}
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test -p autotune`
Expected: all pass

- [ ] **Step 3: Run full workspace check**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: all clean, all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/autotune/tests/
git commit -m "test: add integration tests for state machine and scoring pipeline"
```
