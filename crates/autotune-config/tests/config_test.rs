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
    let err =
        AutotuneConfig::load(std::path::Path::new("/nonexistent/.autotune.toml")).unwrap_err();
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
    assert!(matches!(
        config.score,
        autotune_config::ScoreConfig::Script { .. }
    ));
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

#[test]
fn parse_criterion_benchmark_with_mean_metric() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "criterion-bench"
command = ["cargo", "bench"]
adaptor = { type = "criterion", benchmark_name = "my_bench" }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "mean", direction = "Minimize" }]
"#,
    );
    let config = AutotuneConfig::load(f.path()).unwrap();
    assert_eq!(config.benchmark.len(), 1);
}

#[test]
fn error_empty_script_adaptor_command() {
    let f = write_config(
        r#"
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
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
[experiment]
name = "test-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]
denied = ["["]

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
    assert!(err.to_string().contains("invalid denied glob"));
}
