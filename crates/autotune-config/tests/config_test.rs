use autotune_config::{AutotuneConfig, ConfigError};
use std::io::Write;

#[test]
fn roundtrip_serialize_deserialize() {
    let f = write_config(
        r#"
[task]
name = "roundtrip"
max_iterations = "10"

[paths]
tunable = ["src/**"]

[[measure]]
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
    assert_eq!(reparsed.task.name, "roundtrip");
    assert_eq!(reparsed.measure.len(), 1);
}

fn write_config(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn parse_minimal_valid_config() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "10"

[paths]
tunable = ["src/**"]

[[measure]]
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
    assert_eq!(config.task.name, "test-exp");
    assert_eq!(config.measure.len(), 1);
    assert_eq!(config.test.len(), 0);
}

#[test]
fn parse_infinite_iterations() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "inf"

[paths]
tunable = ["src/**"]

[[measure]]
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
        config.task.max_iterations,
        Some(autotune_config::StopValue::Infinite)
    ));
}

#[test]
fn parse_target_metric_satisfies_stop_condition() {
    // target_metric alone should satisfy the "at least one stop condition" rule.
    let f = write_config(
        r#"
[task]
name = "coverage-task"

[[task.target_metric]]
name = "line_coverage"
value = 95.0
direction = "Maximize"

[paths]
tunable = ["src/**"]

[[measure]]
name = "b"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "line_coverage", pattern = "x" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "line_coverage", direction = "Maximize" }]
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.task.target_metric.len(), 1);
    assert_eq!(config.task.target_metric[0].name, "line_coverage");
    assert_eq!(config.task.target_metric[0].value, 95.0);
    assert!(matches!(
        config.task.target_metric[0].direction,
        autotune_config::Direction::Maximize
    ));
}

#[test]
fn error_no_stop_condition() {
    let f = write_config(
        r#"
[task]
name = "test-exp"

[paths]
tunable = ["src/**"]

[[measure]]
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
    let err =
        AutotuneConfig::load(std::path::Path::new("/nonexistent/.autotune.toml")).unwrap_err();
    assert!(matches!(err, ConfigError::NotFound { .. }));
}

#[test]
fn error_empty_task_command() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
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
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
name = "b1"
command = ["echo"]
adaptor = { type = "regex", patterns = [{ name = "time", pattern = "x" }] }

[[measure]]
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
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
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
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
name = "b"
command = ["echo"]
adaptor = { type = "script", command = ["python", "extract.py"] }

[score]
type = "script"
command = ["python", "judge.py"]
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert!(matches!(
        config.score,
        autotune_config::ScoreConfig::Script { .. }
    ));
}

#[test]
fn parse_multiple_tests() {
    let f = write_config(
        r#"
[task]
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

[[measure]]
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
backend = "codex"

[agent.research]
backend = "codex"
model = "gpt-5"

[agent.implementation]
backend = "claude"
model = "sonnet"

[agent.init]
backend = "codex"
model = "gpt-5-mini"
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.agent.backend, "codex");
    let research = config.agent.research.unwrap();
    assert_eq!(research.backend.as_deref(), Some("codex"));
    assert_eq!(research.model.as_deref(), Some("gpt-5"));
    let implementation = config.agent.implementation.unwrap();
    assert_eq!(implementation.backend.as_deref(), Some("claude"));
    assert_eq!(implementation.model.as_deref(), Some("sonnet"));
    let init = config.agent.init.unwrap();
    assert_eq!(init.backend.as_deref(), Some("codex"));
    assert_eq!(init.model.as_deref(), Some("gpt-5-mini"));
}

#[test]
fn parse_codex_reasoning_effort_config() {
    let content = r#"
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
"#;
    let f = write_config(content);
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.agent.backend, "codex");
    assert_eq!(config.agent.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(
        config.agent.reasoning_effort,
        Some(autotune_config::ReasoningEffort::Medium)
    );
    let research = config.agent.research.expect("research role");
    assert_eq!(
        research.reasoning_effort,
        Some(autotune_config::ReasoningEffort::High)
    );
}

#[test]
fn codex_rejects_max_turns() {
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
backend = "codex"
max_turns = 10
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(matches!(err, ConfigError::Validation { .. }));
    let msg = err.to_string();
    assert!(msg.contains("max_turns"), "error: {msg}");
    assert!(msg.contains("codex"), "error: {msg}");
}

#[test]
fn codex_role_rejects_inherited_max_turns() {
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
backend = "codex"

[agent.research]
max_turns = 10
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(matches!(err, ConfigError::Validation { .. }));
    let msg = err.to_string();
    assert!(msg.contains("agent.research.max_turns"), "error: {msg}");
    assert!(msg.contains("codex"), "error: {msg}");
}

#[test]
fn claude_rejects_reasoning_effort() {
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
reasoning_effort = "medium"
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(matches!(err, ConfigError::Validation { .. }));
    let msg = err.to_string();
    assert!(msg.contains("reasoning_effort"), "error: {msg}");
    assert!(msg.contains("claude"), "error: {msg}");
}

#[test]
fn claude_role_rejects_inherited_reasoning_effort() {
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
backend = "codex"
reasoning_effort = "medium"

[agent.research]
backend = "claude"
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(matches!(err, ConfigError::Validation { .. }));
    let msg = err.to_string();
    assert!(
        msg.contains("agent.research.reasoning_effort"),
        "error: {msg}"
    );
    assert!(msg.contains("claude"), "error: {msg}");
}

#[test]
fn parse_criterion_task_with_mean_metric() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
name = "criterion-bench"
command = ["cargo", "bench"]
adaptor = { type = "criterion", measure_name = "my_bench" }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "mean", direction = "Minimize" }]
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.measure.len(), 1);
}

#[test]
fn error_criterion_task_with_unsupported_metric() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
name = "criterion-bench"
command = ["cargo", "bench"]
adaptor = { type = "criterion", measure_name = "my_bench" }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "variance", direction = "Minimize" }]
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(matches!(err, ConfigError::Validation { .. }));
    assert!(err.to_string().contains("variance"));
}

#[test]
fn error_empty_script_adaptor_command() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
name = "script-bench"
command = ["echo"]
adaptor = { type = "script", command = [] }

[score]
type = "script"
command = ["python", "judge.py"]
"#,
    );
    let err = AutotuneConfig::load(f.path()).unwrap_err();
    assert!(matches!(err, ConfigError::Validation { .. }));
    assert!(err.to_string().contains("empty script adaptor command"));
}

#[test]
fn error_invalid_denied_glob() {
    let f = write_config(
        r#"
[task]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]
denied = ["["]

[[measure]]
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
    assert!(err.to_string().contains("invalid denied glob"));
}
