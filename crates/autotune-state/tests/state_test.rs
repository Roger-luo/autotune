use autotune_state::{
    ApproachState, IterationRecord, IterationStatus, Metrics, Phase, StateError, TaskState,
    TaskStore, TestResult,
};
use chrono::{TimeZone, Utc};
use std::fs;

fn metrics(pairs: &[(&str, f64)]) -> Metrics {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), *value))
        .collect()
}

fn sample_state() -> TaskState {
    TaskState {
        task_name: "demo".to_string(),
        canonical_branch: "main".to_string(),
        research_session_id: "research-1".to_string(),
        current_iteration: 3,
        current_phase: Phase::Testing,
        current_approach: None,
    }
}

fn sample_approach() -> ApproachState {
    ApproachState {
        name: "optimize-cache".to_string(),
        hypothesis: "reduce cache misses".to_string(),
        worktree_path: "/tmp/autotune/demo".into(),
        branch_name: "codex/demo-cache".to_string(),
        commit_sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        test_results: vec![TestResult {
            name: "unit".to_string(),
            passed: true,
            duration_secs: 0.42,
            output: Some("ok".to_string()),
        }],
        metrics: Some(metrics(&[("time_us", 123.0)])),
        rank: Some(0.25),
    }
}

fn sample_record(iteration: usize, approach: &str) -> IterationRecord {
    IterationRecord {
        iteration,
        approach: approach.to_string(),
        status: IterationStatus::Kept,
        hypothesis: Some("improve throughput".to_string()),
        metrics: metrics(&[("time_us", 120.0), ("mem_mb", 64.0)]),
        rank: 0.33,
        score: Some("weighted_sum".to_string()),
        reason: Some("better runtime".to_string()),
        timestamp: Utc.with_ymd_and_hms(2026, 4, 11, 12, 0, 0).unwrap(),
    }
}

#[test]
fn roundtrip_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::new(dir.path()).unwrap();
    let state = sample_state();

    store.save_state(&state).unwrap();

    let loaded = store.load_state().unwrap();
    assert_eq!(loaded.task_name, state.task_name);
    assert_eq!(loaded.canonical_branch, state.canonical_branch);
    assert_eq!(loaded.research_session_id, state.research_session_id);
    assert_eq!(loaded.current_iteration, state.current_iteration);
    assert_eq!(loaded.current_phase, state.current_phase);
    assert_eq!(loaded.current_approach, state.current_approach);
}

#[test]
fn roundtrip_state_with_approach() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::new(dir.path()).unwrap();
    let mut state = sample_state();
    state.current_approach = Some(sample_approach());

    store.save_state(&state).unwrap();

    let loaded = store.load_state().unwrap();
    assert_eq!(loaded.current_approach, state.current_approach);
}

#[test]
fn new_creates_task_and_iterations_directories() {
    let dir = tempfile::tempdir().unwrap();
    let task_dir = dir.path().join("tasks").join("demo");

    let store = TaskStore::new(&task_dir).unwrap();

    assert_eq!(store.root(), task_dir.as_path());
    assert!(task_dir.exists());
    assert!(task_dir.join("iterations").exists());
}

#[test]
fn ledger_append_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::new(dir.path()).unwrap();
    let first = sample_record(1, "baseline");
    let second = sample_record(2, "candidate");

    store.append_ledger(&first).unwrap();
    store.append_ledger(&second).unwrap();

    let ledger = store.load_ledger().unwrap();
    assert_eq!(ledger, vec![first, second]);
}

#[test]
fn log_append_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::new(dir.path()).unwrap();

    assert_eq!(store.read_log().unwrap(), "");
    store.append_log("first entry").unwrap();
    store.append_log("second entry").unwrap();

    assert_eq!(store.read_log().unwrap(), "first entry\nsecond entry\n");
}

#[test]
fn iteration_artifacts_are_written() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::new(dir.path()).unwrap();
    let iteration_dir = store.iteration_dir(7, "candidate");

    store
        .save_iteration_metrics(7, "candidate", &metrics(&[("time_us", 99.0)]))
        .unwrap();
    store
        .save_iteration_prompt(7, "candidate", "generate a better cache strategy")
        .unwrap();
    store
        .save_test_output(7, "candidate", "tests passed")
        .unwrap();

    assert!(iteration_dir.exists());
    assert_eq!(
        fs::read_to_string(iteration_dir.join("metrics.json")).unwrap(),
        serde_json::to_string_pretty(&metrics(&[("time_us", 99.0)])).unwrap()
    );
    assert_eq!(
        fs::read_to_string(iteration_dir.join("prompt.md")).unwrap(),
        "generate a better cache strategy"
    );
    assert_eq!(
        fs::read_to_string(iteration_dir.join("test_output.txt")).unwrap(),
        "tests passed"
    );
}

#[test]
fn config_snapshot_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::new(dir.path()).unwrap();

    store
        .save_config_snapshot("[task]\nname = \"demo\"\n")
        .unwrap();

    assert_eq!(
        store.load_config_snapshot().unwrap(),
        "[task]\nname = \"demo\"\n"
    );
}

#[test]
fn list_tasks_sorts_directories() {
    let dir = tempfile::tempdir().unwrap();
    let autotune_dir = dir.path();
    fs::create_dir_all(autotune_dir.join("tasks").join("zeta")).unwrap();
    fs::create_dir_all(autotune_dir.join("tasks").join("alpha")).unwrap();
    fs::create_dir_all(autotune_dir.join("tasks").join("middle")).unwrap();
    fs::write(autotune_dir.join("tasks").join("ignore.txt"), "nope").unwrap();

    let names = TaskStore::list_tasks(autotune_dir).unwrap();
    assert_eq!(names, vec!["alpha", "middle", "zeta"]);
}

#[test]
fn open_nonexistent_task_fails() {
    let dir = tempfile::tempdir().unwrap();
    let err = TaskStore::open(&dir.path().join("missing")).unwrap_err();

    assert!(matches!(err, StateError::NotFound { .. }));
}
