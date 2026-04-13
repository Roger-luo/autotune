use autotune_config::global::GlobalConfig;
use autotune_init::{MockInput, run_init};
use autotune_mock::MockAgent;
use std::path::PathBuf;

#[test]
fn agent_assisted_init_produces_valid_config() {
    let agent = MockAgent::builder()
        .init_response(r#"{"type":"message","text":"I see a Rust project."}"#)
        .init_response(
            r#"{"type":"config","section":{"type":"task","name":"perf-opt","description":"Optimize performance","max_iterations":"20","canonical_branch":"main"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**/*.rs"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"test","name":"rust","command":["cargo","test"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"measure","name":"bench1","command":["cargo","bench"],"adaptor":{"type":"regex","patterns":[{"name":"time_us","pattern":"time:\\s+([0-9.]+)"}]}}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"score","value":{"type":"weighted_sum","primary_metrics":[{"name":"time_us","direction":"Minimize"}]}}}"#,
        )
        .build();

    let global = GlobalConfig::default();
    let input = MockInput::new("yes");
    let result = run_init(&agent, &global, &PathBuf::from("/tmp/fake"), &input, None).unwrap();

    // Verify all sections are present and correct
    assert_eq!(result.config.task.name, "perf-opt");
    assert_eq!(
        result.config.task.description.as_deref(),
        Some("Optimize performance")
    );
    assert_eq!(result.config.paths.tunable, vec!["src/**/*.rs"]);
    assert_eq!(result.config.test.len(), 1);
    assert_eq!(result.config.test[0].name, "rust");
    assert_eq!(result.config.measure.len(), 1);
    assert_eq!(result.config.measure[0].name, "bench1");
    assert!(result.baseline_metrics.is_none());

    // Verify the config serializes to valid TOML that roundtrips
    let toml_str = toml::to_string_pretty(&result.config).unwrap();
    let reparsed: autotune_config::AutotuneConfig = toml::from_str(&toml_str).unwrap();
    reparsed.validate().unwrap();
    assert_eq!(reparsed.task.name, "perf-opt");
}

#[test]
fn agent_assisted_init_validates_sections_incrementally() {
    // Agent proposes an invalid task (no stop condition), then a valid one
    let agent = MockAgent::builder()
        .init_response(
            r#"{"type":"config","section":{"type":"task","name":"test-exp"}}"#,
        )
        // After validation error, agent retries with stop condition
        .init_response(
            r#"{"type":"config","section":{"type":"task","name":"test-exp","max_iterations":"5"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"measure","name":"b","command":["echo"],"adaptor":{"type":"regex","patterns":[{"name":"m","pattern":"x"}]}}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"score","value":{"type":"weighted_sum","primary_metrics":[{"name":"m","direction":"Maximize"}]}}}"#,
        )
        .build();

    let global = GlobalConfig::default();
    let input = MockInput::new("yes");
    let result = run_init(&agent, &global, &PathBuf::from("/tmp/fake"), &input, None).unwrap();

    assert_eq!(result.config.task.name, "test-exp");
}
