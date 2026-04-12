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
        Ok("yes".to_string())
    })
    .unwrap();

    // Verify all sections are present and correct
    assert_eq!(config.experiment.name, "perf-opt");
    assert_eq!(
        config.experiment.description.as_deref(),
        Some("Optimize performance")
    );
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
        Ok("yes".to_string())
    })
    .unwrap();

    assert_eq!(config.experiment.name, "test-exp");
}
