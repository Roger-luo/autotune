use autotune_config::global::GlobalConfig;
use autotune_init::{MockInput, run_init};
use autotune_mock::MockAgent;
use std::path::PathBuf;

fn complete_init_agent() -> MockAgent {
    MockAgent::builder()
        .init_response(
            r#"{"type":"message","text":"I found a Rust project with Cargo.toml."}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"task","name":"test-exp","max_iterations":"10","canonical_branch":"main"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"measure","name":"bench1","command":["cargo","bench"],"adaptor":{"type":"regex","patterns":[{"name":"time_us","pattern":"time:\\s+([0-9.]+)"}]}}}"#,
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
    let input = MockInput::new("yes");
    let result = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        &input,
        None,
    )
    .unwrap();

    assert_eq!(result.config.task.name, "test-exp");
    assert_eq!(result.config.paths.tunable, vec!["src/**"]);
    assert_eq!(result.config.measure.len(), 1);
    assert_eq!(result.config.measure[0].name, "bench1");
    assert!(result.baseline_metrics.is_none());
}

#[test]
fn run_init_missing_required_sections_keeps_going() {
    let agent = MockAgent::builder()
        .init_response(
            r#"{"type":"config","section":{"type":"task","name":"test-exp","max_iterations":"10"}}"#,
        )
        .init_response(
            r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#,
        )
        .build();

    let global = GlobalConfig::default();
    let input = MockInput::new("yes");
    let result = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        &input,
        None,
    );

    // Should error because we never get measure + score sections
    assert!(result.is_err());
}
