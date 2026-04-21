mod error;

pub use error::ConfigError;
pub mod global;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutotuneConfig {
    pub task: TaskConfig,
    pub paths: PathsConfig,
    #[serde(default)]
    pub test: Vec<TestConfig>,
    pub measure: Vec<MeasureConfig>,
    pub score: ScoreConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConfig {
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
    /// Stop when specific metrics reach absolute thresholds.
    /// All listed metrics must meet their threshold (AND semantics).
    #[serde(default)]
    pub target_metric: Vec<TargetMetric>,
}

/// A metric threshold that acts as a stop condition.
///
/// For `direction = Maximize`, stops when the metric value is `>= value`.
/// For `direction = Minimize`, stops when the metric value is `<= value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetMetric {
    pub name: String,
    pub value: f64,
    pub direction: Direction,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    pub tunable: Vec<String>,
    #[serde(default)]
    pub denied: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_test_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub allow_test_edits: bool,
}

fn default_test_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasureConfig {
    pub name: String,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default = "default_measure_timeout")]
    pub timeout: u64,
    pub adaptor: AdaptorConfig,
}

fn default_measure_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRangeConfig {
    pub min: i32,
    pub max: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricConfig {
    pub id: String,
    pub title: String,
    pub instruction: String,
    pub score_range: ScoreRangeConfig,
    #[serde(default)]
    pub guidance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AdaptorConfig {
    #[serde(rename = "regex")]
    Regex { patterns: Vec<RegexPattern> },
    #[serde(rename = "criterion")]
    Criterion { measure_name: String },
    #[serde(rename = "script")]
    Script { command: Vec<String> },
    #[serde(rename = "judge")]
    Judge {
        persona: String,
        #[serde(default)]
        rubrics: Vec<RubricConfig>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegexPattern {
    pub name: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimaryMetric {
    pub name: String,
    pub direction: Direction,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailMetric {
    pub name: String,
    pub direction: Direction,
    pub max_regression: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Direction {
    Minimize,
    Maximize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdCondition {
    pub metric: String,
    pub direction: Direction,
    pub threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u64>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub max_fix_attempts: Option<u32>,
    #[serde(default)]
    pub max_fresh_spawns: Option<u32>,
    #[serde(default)]
    pub research: Option<AgentRoleConfig>,
    #[serde(default)]
    pub implementation: Option<AgentRoleConfig>,
    #[serde(default)]
    pub init: Option<AgentRoleConfig>,
    #[serde(default)]
    pub judge: Option<AgentRoleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRoleConfig {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u64>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Total implementer fix-retry turns allowed per iteration when tests
    /// fail. Counts both session-continuation turns and fresh respawns.
    /// `0` disables retry (tests fail → discard immediately, legacy
    /// behavior). `None` (field absent) is treated as the default by
    /// [`AgentRoleConfig::effective_max_fix_attempts`].
    ///
    /// Only read off the `implementation` role; ignored elsewhere.
    #[serde(default)]
    pub max_fix_attempts: Option<u32>,
    /// Of the `max_fix_attempts` budget, how many may be fresh respawns
    /// (context reset, new CLI invocation on the same worktree). `0`
    /// disables fresh spawns (session-continuation only). `None` is
    /// treated as the default by
    /// [`AgentRoleConfig::effective_max_fresh_spawns`].
    #[serde(default)]
    pub max_fresh_spawns: Option<u32>,
}

impl AgentRoleConfig {
    /// Default fix-attempt budget when the config omits the field.
    pub const DEFAULT_MAX_FIX_ATTEMPTS: u32 = 10;

    /// Default fresh-respawn budget when the config omits the field.
    pub const DEFAULT_MAX_FRESH_SPAWNS: u32 = 1;

    pub fn effective_max_fix_attempts(&self) -> u32 {
        self.max_fix_attempts
            .unwrap_or(Self::DEFAULT_MAX_FIX_ATTEMPTS)
    }

    pub fn effective_max_fresh_spawns(&self) -> u32 {
        self.max_fresh_spawns
            .unwrap_or(Self::DEFAULT_MAX_FRESH_SPAWNS)
    }

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
        let agent_defaults = self.effective_agent_defaults();
        self.validate_agent_backend_fields("agent", &agent_defaults)?;
        for (role_name, role) in [
            ("research", &self.agent.research),
            ("implementation", &self.agent.implementation),
            ("init", &self.agent.init),
            ("judge", &self.agent.judge),
        ] {
            if let Some(role) = role {
                let effective = role.overlay(&agent_defaults);
                self.validate_agent_backend_fields(&format!("agent.{role_name}"), &effective)?;
            }
        }

        // At least one stop condition
        if self.task.max_iterations.is_none()
            && self.task.target_improvement.is_none()
            && self.task.max_duration.is_none()
            && self.task.target_metric.is_empty()
        {
            return Err(ConfigError::Validation {
                message: "at least one stop condition required (max_iterations, target_improvement, max_duration, or target_metric)".to_string(),
            });
        }

        // Measures non-empty
        if self.measure.is_empty() {
            return Err(ConfigError::Validation {
                message: "at least one [[measure]] entry required".to_string(),
            });
        }

        // Each measure command non-empty (with adaptor-type-specific rules)
        for b in &self.measure {
            match &b.adaptor {
                AdaptorConfig::Judge { rubrics, .. } => {
                    if rubrics.is_empty() {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "measure '{}' judge adaptor must have at least one rubric",
                                b.name
                            ),
                        });
                    }
                    let mut seen_ids = std::collections::HashSet::new();
                    for r in rubrics {
                        if !seen_ids.insert(&r.id) {
                            return Err(ConfigError::Validation {
                                message: format!(
                                    "measure '{}' has duplicate rubric id '{}'",
                                    b.name, r.id
                                ),
                            });
                        }
                        if r.score_range.min > r.score_range.max {
                            return Err(ConfigError::Validation {
                                message: format!(
                                    "measure '{}' rubric '{}' score_range min ({}) > max ({})",
                                    b.name, r.id, r.score_range.min, r.score_range.max
                                ),
                            });
                        }
                    }
                    if let Some(cmd) = &b.command
                        && cmd.is_empty()
                    {
                        return Err(ConfigError::Validation {
                            message: format!("measure '{}' has empty command", b.name),
                        });
                    }
                }
                _ => {
                    match &b.command {
                        None => {
                            return Err(ConfigError::Validation {
                                message: format!("measure '{}' requires a command", b.name),
                            });
                        }
                        Some(cmd) if cmd.is_empty() => {
                            return Err(ConfigError::Validation {
                                message: format!("measure '{}' has empty command", b.name),
                            });
                        }
                        _ => {}
                    }
                    if let AdaptorConfig::Script { command } = &b.adaptor
                        && command.is_empty()
                    {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "measure '{}' has empty script adaptor command",
                                b.name
                            ),
                        });
                    }
                }
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
        for pattern in &self.paths.denied {
            globset::Glob::new(pattern).map_err(|e| ConfigError::Validation {
                message: format!("invalid denied glob '{}': {}", pattern, e),
            })?;
        }

        // Validate metric name uniqueness across measures
        let mut metric_names = std::collections::HashSet::new();
        for b in &self.measure {
            let names = self.adaptor_metric_names(&b.adaptor);
            for name in names {
                if !metric_names.insert(name.clone()) {
                    return Err(ConfigError::Validation {
                        message: format!("duplicate metric name '{}' across measures", name),
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
                if primary_metrics.is_empty() {
                    return Err(ConfigError::Validation {
                        message:
                            "weighted_sum score must contain at least one primary metric"
                                .to_string(),
                    });
                }
                for pm in primary_metrics {
                    if !metric_names.contains(&pm.name) {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "primary metric '{}' not produced by any measure adaptor",
                                pm.name
                            ),
                        });
                    }
                }
                for gm in guardrail_metrics {
                    if !metric_names.contains(&gm.name) {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "guardrail metric '{}' not produced by any measure adaptor",
                                gm.name
                            ),
                        });
                    }
                }
            }
            ScoreConfig::Threshold { conditions } => {
                if conditions.is_empty() {
                    return Err(ConfigError::Validation {
                        message: "threshold score must contain at least one condition"
                            .to_string(),
                    });
                }
                for c in conditions {
                    if !metric_names.contains(&c.metric) {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "threshold metric '{}' not produced by any measure adaptor",
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

    fn effective_agent_defaults(&self) -> AgentRoleConfig {
        AgentRoleConfig {
            backend: self.agent.backend.clone(),
            model: self.agent.model.clone(),
            max_turns: self.agent.max_turns,
            reasoning_effort: self.agent.reasoning_effort,
            max_fix_attempts: self.agent.max_fix_attempts,
            max_fresh_spawns: self.agent.max_fresh_spawns,
        }
    }

    fn validate_agent_backend_fields(
        &self,
        path: &str,
        role: &AgentRoleConfig,
    ) -> Result<(), ConfigError> {
        let Some(backend) = role.backend.as_deref() else {
            return Ok(());
        };

        match backend {
            "codex" if role.max_turns.is_some() => Err(ConfigError::Validation {
                message: format!("{path}.max_turns is not valid for backend 'codex'"),
            }),
            "claude" if role.reasoning_effort.is_some() => Err(ConfigError::Validation {
                message: format!("{path}.reasoning_effort is not valid for backend 'claude'"),
            }),
            _ => Ok(()),
        }
    }

    /// Extract metric names that an adaptor config will produce.
    /// For script adaptors, returns empty (can't know ahead of time).
    fn adaptor_metric_names(&self, adaptor: &AdaptorConfig) -> Vec<String> {
        match adaptor {
            AdaptorConfig::Regex { patterns } => patterns.iter().map(|p| p.name.clone()).collect(),
            AdaptorConfig::Criterion { .. } => {
                vec![
                    "mean".to_string(),
                    "median".to_string(),
                    "std_dev".to_string(),
                ]
            }
            AdaptorConfig::Script { .. } => vec![],
            AdaptorConfig::Judge { rubrics, .. } => rubrics.iter().map(|r| r.id.clone()).collect(),
        }
    }

    /// Resolve the task directory path: `.autotune/tasks/<name>/`
    pub fn task_dir(&self, root: &Path) -> PathBuf {
        root.join(".autotune").join("tasks").join(&self.task.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config_with_score(score_toml: &str) -> String {
        format!(
            r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "m"
command = ["echo"]
adaptor = {{ type = "regex", patterns = [{{ name = "val", pattern = "x([0-9]+)" }}] }}
{score_toml}
"#
        )
    }

    #[test]
    fn validate_threshold_unknown_metric_errors() {
        let toml = minimal_config_with_score(
            r#"
[score]
type = "threshold"
conditions = [{ metric = "nonexistent", direction = "Minimize", threshold = 0.0 }]
"#,
        );
        let config: AutotuneConfig = toml::from_str(&toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("nonexistent"), "error: {err}");
    }

    #[test]
    fn validate_script_score_empty_command_errors() {
        let toml = minimal_config_with_score(
            r#"
[score]
type = "script"
command = []
"#,
        );
        let config: AutotuneConfig = toml::from_str(&toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty"), "error: {err}");
    }

    #[test]
    fn validate_command_score_empty_command_errors() {
        let toml = minimal_config_with_score(
            r#"
[score]
type = "command"
command = []
"#,
        );
        let config: AutotuneConfig = toml::from_str(&toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty"), "error: {err}");
    }

    #[test]
    fn adaptor_metric_names_criterion_returns_three() {
        let toml = minimal_config_with_score(
            r#"
[score]
type = "weighted_sum"
primary_metrics = [{ name = "mean", direction = "Minimize" }]
"#,
        );
        let config: AutotuneConfig = toml::from_str(&toml).unwrap();
        let adaptor = AdaptorConfig::Criterion {
            measure_name: "bench".to_string(),
        };
        let names = config.adaptor_metric_names(&adaptor);
        assert_eq!(names, vec!["mean", "median", "std_dev"]);
    }

    #[test]
    fn adaptor_metric_names_script_returns_empty() {
        let toml = minimal_config_with_score(
            r#"
[score]
type = "weighted_sum"
primary_metrics = [{ name = "val", direction = "Maximize" }]
"#,
        );
        let config: AutotuneConfig = toml::from_str(&toml).unwrap();
        let adaptor = AdaptorConfig::Script {
            command: vec!["sh".to_string()],
        };
        let names = config.adaptor_metric_names(&adaptor);
        assert!(names.is_empty());
    }

    #[test]
    fn task_dir_returns_expected_path() {
        let toml = minimal_config_with_score(
            r#"
[score]
type = "weighted_sum"
primary_metrics = [{ name = "val", direction = "Maximize" }]
"#,
        );
        let config: AutotuneConfig = toml::from_str(&toml).unwrap();
        let root = std::path::Path::new("/tmp/myproject");
        let dir = config.task_dir(root);
        assert_eq!(
            dir,
            std::path::Path::new("/tmp/myproject/.autotune/tasks/t")
        );
    }

    #[test]
    fn effective_fix_budget_uses_defaults_when_absent() {
        // Absent fields → `Option::None` resolves to the DEFAULT_ constants.
        let role = AgentRoleConfig {
            backend: None,
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: None,
            max_fresh_spawns: None,
        };
        assert_eq!(role.effective_max_fix_attempts(), 10);
        assert_eq!(role.effective_max_fresh_spawns(), 1);
    }

    #[test]
    fn effective_fix_budget_honors_zero_as_disabled() {
        // `Some(0)` means "disabled" in the machine's budget logic; the
        // effective_* helpers must surface the user's `0`, not the default.
        let role = AgentRoleConfig {
            backend: None,
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: Some(0),
            max_fresh_spawns: Some(0),
        };
        assert_eq!(role.effective_max_fix_attempts(), 0);
        assert_eq!(role.effective_max_fresh_spawns(), 0);
    }

    #[test]
    fn fix_budget_deserializes_from_toml() {
        let toml = minimal_config_with_score(
            r#"
[score]
type = "weighted_sum"
primary_metrics = [{ name = "val", direction = "Maximize" }]

[agent.implementation]
max_fix_attempts = 5
max_fresh_spawns = 2
"#,
        );
        let config: AutotuneConfig = toml::from_str(&toml).unwrap();
        let role = config.agent.implementation.expect("implementation role");
        assert_eq!(role.max_fix_attempts, Some(5));
        assert_eq!(role.max_fresh_spawns, Some(2));
        assert_eq!(role.effective_max_fix_attempts(), 5);
        assert_eq!(role.effective_max_fresh_spawns(), 2);
    }

    fn make_config_direct(
        task: TaskConfig,
        paths: PathsConfig,
        test: Vec<TestConfig>,
        measure: Vec<MeasureConfig>,
        score: ScoreConfig,
    ) -> AutotuneConfig {
        AutotuneConfig {
            task,
            paths,
            test,
            measure,
            score,
            agent: AgentConfig::default(),
        }
    }

    fn default_task_with_stop() -> TaskConfig {
        TaskConfig {
            name: "t".to_string(),
            description: None,
            canonical_branch: "main".to_string(),
            max_iterations: Some(StopValue::Finite(5)),
            target_improvement: None,
            max_duration: None,
            target_metric: vec![],
        }
    }

    fn default_paths() -> PathsConfig {
        PathsConfig {
            tunable: vec!["src/**".to_string()],
            denied: vec![],
        }
    }

    fn regex_measure(name: &str, metric_name: &str) -> MeasureConfig {
        MeasureConfig {
            name: name.to_string(),
            command: Some(vec!["echo".to_string()]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: metric_name.to_string(),
                    pattern: "([0-9]+)".to_string(),
                }],
            },
        }
    }

    fn weighted_sum_score(metric_name: &str) -> ScoreConfig {
        ScoreConfig::WeightedSum {
            primary_metrics: vec![PrimaryMetric {
                name: metric_name.to_string(),
                direction: Direction::Maximize,
                weight: 1.0,
            }],
            guardrail_metrics: vec![],
        }
    }

    #[test]
    fn validate_rejects_no_stop_conditions() {
        let toml = r#"
[task]
name = "t"
canonical_branch = "main"

[[measure]]
name = "m"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "val", pattern = "x([0-9]+)" }] }

[paths]
tunable = ["src/**"]

[score]
type = "weighted_sum"
primary_metrics = [{ name = "val", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("stop condition"), "error: {err}");
    }

    #[test]
    fn validate_rejects_empty_measures() {
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![],
            ScoreConfig::Script {
                command: vec!["sh".to_string()],
            },
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("measure"), "error: {err}");
    }

    #[test]
    fn validate_rejects_empty_measure_command() {
        let measure = MeasureConfig {
            name: "m".to_string(),
            command: Some(vec![]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex { patterns: vec![] },
        };
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![measure],
            ScoreConfig::Script {
                command: vec!["sh".to_string()],
            },
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty"), "error: {err}");
    }

    #[test]
    fn validate_rejects_empty_script_adaptor_command() {
        let measure = MeasureConfig {
            name: "m".to_string(),
            command: Some(vec!["echo".to_string()]),
            timeout: 30,
            adaptor: AdaptorConfig::Script { command: vec![] },
        };
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![measure],
            ScoreConfig::Script {
                command: vec!["sh".to_string()],
            },
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty"), "error: {err}");
    }

    #[test]
    fn validate_rejects_empty_test_command() {
        let test = TestConfig {
            name: "t".to_string(),
            command: vec![],
            timeout: 30,
            allow_test_edits: false,
        };
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![test],
            vec![regex_measure("m", "val")],
            weighted_sum_score("val"),
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty"), "error: {err}");
    }

    #[test]
    fn validate_rejects_empty_tunable_paths() {
        let config = make_config_direct(
            default_task_with_stop(),
            PathsConfig {
                tunable: vec![],
                denied: vec![],
            },
            vec![],
            vec![regex_measure("m", "val")],
            weighted_sum_score("val"),
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("tunable"), "error: {err}");
    }

    #[test]
    fn validate_rejects_invalid_tunable_glob() {
        let config = make_config_direct(
            default_task_with_stop(),
            PathsConfig {
                tunable: vec!["[invalid".to_string()],
                denied: vec![],
            },
            vec![],
            vec![regex_measure("m", "val")],
            weighted_sum_score("val"),
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("invalid"), "error: {err}");
    }

    #[test]
    fn validate_rejects_invalid_denied_glob() {
        let config = make_config_direct(
            default_task_with_stop(),
            PathsConfig {
                tunable: vec!["src/**".to_string()],
                denied: vec!["[bad".to_string()],
            },
            vec![],
            vec![regex_measure("m", "val")],
            weighted_sum_score("val"),
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("invalid"), "error: {err}");
    }

    #[test]
    fn validate_rejects_duplicate_metric_names() {
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![regex_measure("m1", "val"), regex_measure("m2", "val")],
            weighted_sum_score("val"),
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate"), "error: {err}");
    }

    #[test]
    fn validate_rejects_primary_metric_not_in_adaptor() {
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![regex_measure("m", "val")],
            ScoreConfig::WeightedSum {
                primary_metrics: vec![PrimaryMetric {
                    name: "nonexistent".to_string(),
                    direction: Direction::Maximize,
                    weight: 1.0,
                }],
                guardrail_metrics: vec![],
            },
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("nonexistent"), "error: {err}");
    }

    #[test]
    fn validate_rejects_empty_weighted_sum_primary_metrics() {
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![regex_measure("m", "val")],
            ScoreConfig::WeightedSum {
                primary_metrics: vec![],
                guardrail_metrics: vec![],
            },
        );
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("at least one primary metric"),
            "error: {err}"
        );
    }

    #[test]
    fn validate_rejects_guardrail_metric_not_in_adaptor() {
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![regex_measure("m", "val")],
            ScoreConfig::WeightedSum {
                primary_metrics: vec![PrimaryMetric {
                    name: "val".to_string(),
                    direction: Direction::Maximize,
                    weight: 1.0,
                }],
                guardrail_metrics: vec![GuardrailMetric {
                    name: "missing-guard".to_string(),
                    direction: Direction::Minimize,
                    max_regression: 0.1,
                }],
            },
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("missing-guard"), "error: {err}");
    }

    #[test]
    fn validate_rejects_empty_threshold_conditions() {
        let config = make_config_direct(
            default_task_with_stop(),
            default_paths(),
            vec![],
            vec![regex_measure("m", "val")],
            ScoreConfig::Threshold { conditions: vec![] },
        );
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("at least one condition"),
            "error: {err}"
        );
    }

    #[test]
    fn effective_max_fresh_spawns_with_explicit_value() {
        let role = AgentRoleConfig {
            backend: None,
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: None,
            max_fresh_spawns: Some(5),
        };
        assert_eq!(role.effective_max_fresh_spawns(), 5);
    }

    #[test]
    fn judge_adaptor_parses_from_toml() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A strict reviewer"
[[measure.adaptor.rubrics]]
id = "correctness"
title = "Correctness"
instruction = "Score correctness 1-5."
score_range = { min = 1, max = 5 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "correctness", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        config.validate().unwrap();
        let AdaptorConfig::Judge { persona, rubrics } = &config.measure[0].adaptor else {
            panic!("expected Judge adaptor");
        };
        assert_eq!(persona, "A strict reviewer");
        assert_eq!(rubrics.len(), 1);
        assert_eq!(rubrics[0].id, "correctness");
        assert_eq!(rubrics[0].score_range.min, 1);
        assert_eq!(rubrics[0].score_range.max, 5);
        assert!(config.measure[0].command.is_none());
    }

    #[test]
    fn judge_adaptor_with_command_parses() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
command = ["sh", "-c", "cat src/lib.rs"]
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "quality"
title = "Quality"
instruction = "Score 1-3."
score_range = { min = 1, max = 3 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "quality", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        config.validate().unwrap();
        let expected: &[String] = &[
            "sh".to_string(),
            "-c".to_string(),
            "cat src/lib.rs".to_string(),
        ];
        assert_eq!(config.measure[0].command.as_deref(), Some(expected));
    }

    #[test]
    fn judge_adaptor_with_no_rubrics_fails_validation() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[score]
type = "weighted_sum"
primary_metrics = [{ name = "anything", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("rubric"), "error: {err}");
    }

    #[test]
    fn judge_adaptor_empty_command_fails_validation() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
command = []
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "q"
title = "Q"
instruction = "Score 1-5."
score_range = { min = 1, max = 5 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "q", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty"), "error: {err}");
    }

    #[test]
    fn non_judge_measure_without_command_fails_validation() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "m"
adaptor = { type = "regex", patterns = [{ name = "val", pattern = "([0-9]+)" }] }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "val", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("command"), "error: {err}");
    }

    #[test]
    fn judge_adaptor_metric_names_returns_rubric_ids() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "r1"
title = "R1"
instruction = "Score."
score_range = { min = 1, max = 5 }
[[measure.adaptor.rubrics]]
id = "r2"
title = "R2"
instruction = "Score."
score_range = { min = 1, max = 5 }
[score]
type = "weighted_sum"
primary_metrics = [
  { name = "r1", direction = "Maximize" },
  { name = "r2", direction = "Maximize" },
]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        config.validate().unwrap();
        let names = config.adaptor_metric_names(&config.measure[0].adaptor);
        assert!(names.contains(&"r1".to_string()));
        assert!(names.contains(&"r2".to_string()));
    }

    #[test]
    fn judge_rubric_score_range_min_gt_max_fails_validation() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "q"
title = "Q"
instruction = "Score."
score_range = { min = 5, max = 1 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "q", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("min") && err.to_string().contains("max"),
            "error: {err}"
        );
    }

    #[test]
    fn judge_rubric_duplicate_id_fails_validation() {
        let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "q"
title = "Q1"
instruction = "Score."
score_range = { min = 1, max = 5 }
[[measure.adaptor.rubrics]]
id = "q"
title = "Q2"
instruction = "Score again."
score_range = { min = 1, max = 5 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "q", direction = "Maximize" }]
"#;
        let config: AutotuneConfig = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("duplicate") && err.to_string().contains("q"),
            "error: {err}"
        );
    }

    #[test]
    fn overlay_uses_role_values_over_defaults() {
        let defaults = AgentRoleConfig {
            backend: Some("claude".to_string()),
            model: Some("sonnet".to_string()),
            max_turns: Some(12),
            reasoning_effort: Some(ReasoningEffort::Low),
            max_fix_attempts: Some(3),
            max_fresh_spawns: Some(1),
        };
        let role = AgentRoleConfig {
            backend: Some("codex".to_string()),
            model: None,
            max_turns: Some(7),
            reasoning_effort: Some(ReasoningEffort::High),
            max_fix_attempts: None,
            max_fresh_spawns: Some(0),
        };
        let effective = role.overlay(&defaults);
        assert_eq!(effective.backend.as_deref(), Some("codex"));
        assert_eq!(effective.model.as_deref(), Some("sonnet"));
        assert_eq!(effective.max_turns, Some(7));
        assert_eq!(effective.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(effective.max_fix_attempts, Some(3));
        assert_eq!(effective.max_fresh_spawns, Some(0));
    }
}
