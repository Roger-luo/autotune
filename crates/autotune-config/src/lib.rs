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
}

fn default_test_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasureConfig {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_measure_timeout")]
    pub timeout: u64,
    pub adaptor: AdaptorConfig,
}

fn default_measure_timeout() -> u64 {
    600
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

        // Each measure command non-empty
        for b in &self.measure {
            if b.command.is_empty() {
                return Err(ConfigError::Validation {
                    message: format!("measure '{}' has empty command", b.name),
                });
            }
            if let AdaptorConfig::Script { command } = &b.adaptor
                && command.is_empty()
            {
                return Err(ConfigError::Validation {
                    message: format!("measure '{}' has empty script adaptor command", b.name),
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
        }
    }

    /// Resolve the task directory path: `.autotune/tasks/<name>/`
    pub fn task_dir(&self, root: &Path) -> PathBuf {
        root.join(".autotune").join("tasks").join(&self.task.name)
    }
}
