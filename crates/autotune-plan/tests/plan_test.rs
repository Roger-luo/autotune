use autotune_plan::{PlanError, build_planning_prompt, is_denied_for_research, parse_hypothesis};
use autotune_state::{IterationRecord, IterationStatus, Metrics, TaskStore};
use chrono::Utc;

#[test]
fn parse_hypothesis_clean_xml() {
    let xml = "<plan>\
        <approach>batch-read</approach>\
        <hypothesis>Batching reads reduces syscalls</hypothesis>\
        <files-to-modify><file>src/io.rs</file></files-to-modify>\
        </plan>";
    let h = parse_hypothesis(xml).unwrap();
    assert_eq!(h.approach, "batch-read");
    assert_eq!(h.hypothesis, "Batching reads reduces syscalls");
    assert_eq!(h.files_to_modify, vec!["src/io.rs"]);
}

#[test]
fn parse_hypothesis_with_surrounding_text() {
    let response = "After reviewing the codebase I think we should try:\n\n\
        ```xml\n\
        <plan>\n\
          <approach>prefetch</approach>\n\
          <hypothesis>Prefetching data improves latency</hypothesis>\n\
          <files-to-modify>\n\
            <file>src/fetch.rs</file>\n\
            <file>src/lib.rs</file>\n\
          </files-to-modify>\n\
        </plan>\n\
        ```\n\n\
        Let me know if you'd like me to elaborate.";
    let h = parse_hypothesis(response).unwrap();
    assert_eq!(h.approach, "prefetch");
    assert_eq!(h.files_to_modify, vec!["src/fetch.rs", "src/lib.rs"]);
}

#[test]
fn parse_hypothesis_no_plan_errors() {
    let response = "I have no suggestions at this time.";
    let err = parse_hypothesis(response).unwrap_err();
    assert!(matches!(err, PlanError::ParseHypothesis { .. }));
}

#[test]
fn research_denylist_blocks_write_tools() {
    assert!(is_denied_for_research("Edit"));
    assert!(is_denied_for_research("Write"));
    assert!(is_denied_for_research("Agent"));
}

#[test]
fn research_denylist_allows_read_and_bash() {
    assert!(!is_denied_for_research("Bash"));
    assert!(!is_denied_for_research("WebFetch"));
    assert!(!is_denied_for_research("WebSearch"));
    assert!(!is_denied_for_research("Read"));
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
    assert!(prompt.contains("Iteration 2"));
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
