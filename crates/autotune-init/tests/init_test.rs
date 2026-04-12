use autotune_config::global::GlobalConfig;
use autotune_init::run_init;
use autotune_mock::MockAgent;
use std::path::PathBuf;

fn complete_init_agent() -> MockAgent {
    MockAgent::builder()
        .init_response(
            r#"{"type":"message","text":"I found a Rust project with Cargo.toml."}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"experiment","name":"test-exp","max_iterations":"10","canonical_branch":"main"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"benchmark","name":"bench1","command":["cargo","bench"],"adaptor":{"type":"regex","patterns":[{"name":"time_us","pattern":"time:\\s+([0-9.]+)"}]}}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"score","value":{"type":"weighted_sum","primary_metrics":[{"name":"time_us","direction":"Minimize"}]}}}"#,
        )
        .build()
}

#[test]
fn run_init_complete_conversation() {
    let agent = complete_init_agent();
    let global = GlobalConfig::default();
    // "yes" handles both conversation replies and final approval
    let config = run_init(&agent, &global, &PathBuf::from("/tmp/fake-repo"), || {
        Ok("yes".to_string())
    })
    .unwrap();

    assert_eq!(config.experiment.name, "test-exp");
    assert_eq!(config.paths.tunable, vec!["src/**"]);
    assert_eq!(config.benchmark.len(), 1);
    assert_eq!(config.benchmark[0].name, "bench1");
}

#[test]
fn run_init_missing_required_sections_keeps_going() {
    let agent = MockAgent::builder()
        .init_response(
            r#"{"type":"config","section":{"type":"experiment","name":"test-exp","max_iterations":"10"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#,
        )
        .build();

    let global = GlobalConfig::default();
    let result = run_init(&agent, &global, &PathBuf::from("/tmp/fake-repo"), || {
        Ok("yes".to_string())
    });

    // Should error because we never get benchmark + score sections
    assert!(result.is_err());
}
