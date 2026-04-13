use autotune_plan::{PlanError, build_planning_prompt, parse_hypothesis};
use autotune_state::{IterationRecord, IterationStatus, Metrics, TaskStore};
use chrono::Utc;

#[test]
fn parse_hypothesis_clean_json() {
    let json = r#"{"approach":"batch-read","hypothesis":"Batching reads reduces syscalls","files_to_modify":["src/io.rs"]}"#;
    let h = parse_hypothesis(json).unwrap();
    assert_eq!(h.approach, "batch-read");
    assert_eq!(h.hypothesis, "Batching reads reduces syscalls");
    assert_eq!(h.files_to_modify, vec!["src/io.rs"]);
}

#[test]
fn parse_hypothesis_with_surrounding_text() {
    let response = "After reviewing the codebase I think we should try:\n\n\
        ```json\n\
        {\"approach\": \"prefetch\", \"hypothesis\": \"Prefetching data improves latency\", \"files_to_modify\": [\"src/fetch.rs\", \"src/lib.rs\"]}\n\
        ```\n\n\
        Let me know if you'd like me to elaborate.";
    let h = parse_hypothesis(response).unwrap();
    assert_eq!(h.approach, "prefetch");
    assert_eq!(h.files_to_modify, vec!["src/fetch.rs", "src/lib.rs"]);
}

#[test]
fn parse_hypothesis_no_json_errors() {
    let response = "I have no suggestions at this time.";
    let err = parse_hypothesis(response).unwrap_err();
    assert!(matches!(err, PlanError::ParseHypothesis { .. }));
}

#[test]
fn build_planning_prompt_includes_description() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();
    let prompt = build_planning_prompt(&store, None, 1, "Optimize compile times").unwrap();
    assert!(prompt.contains("Optimize compile times"));
}

#[test]
fn build_planning_prompt_includes_last_iteration() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();

    let mut metrics = Metrics::new();
    metrics.insert("latency_ms".to_string(), 42.0);

    let record = IterationRecord {
        iteration: 1,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: Some("initial run".to_string()),
        metrics,
        rank: 1.0,
        score: None,
        reason: Some("first attempt".to_string()),
        timestamp: Utc::now(),
    };

    let prompt = build_planning_prompt(&store, Some(&record), 2, "Optimize compile times").unwrap();
    assert!(prompt.contains("baseline"));
    assert!(prompt.contains("initial run"));
    assert!(prompt.contains("first attempt"));
    assert!(prompt.contains("latency_ms"));
    assert!(prompt.contains("Iteration: 2"));
}

#[test]
fn build_planning_prompt_includes_log_content() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();
    store
        .append_log("## Iteration 0\nBaseline established.")
        .unwrap();

    let prompt = build_planning_prompt(&store, None, 1, "Speed up tests").unwrap();
    assert!(prompt.contains("Baseline established."));
}
