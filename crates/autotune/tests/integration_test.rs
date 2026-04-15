use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use autotune_agent::ToolPermission;
use autotune_benchmark::run_all_measures;
use autotune_config::AutotuneConfig;
use autotune_mock::{ImplBehavior, MockAgent};
use autotune_score::weighted_sum::{
    Direction, GuardrailMetricDef, PrimaryMetricDef, WeightedSumScorer,
};
use autotune_score::{ScoreCalculator, ScoreInput};
use autotune_state::{IterationRecord, IterationStatus, Phase, TaskState, TaskStore};
use chrono::Utc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn init_temp_repo(dir: &Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init failed");

    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();

    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("README.md"), "# test repo\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "// placeholder\n").unwrap();

    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", "initial commit"])
        .current_dir(dir)
        .output()
        .unwrap();

    // Ensure branch is named "main"
    let _ = Command::new("git")
        .args(["branch", "-M", "main"])
        .current_dir(dir)
        .output();
}

fn write_config_with_iterations(dir: &Path, max_iterations: &str) {
    let config = format!(
        r#"
[task]
name = "test-task"
description = "integration test task"
canonical_branch = "main"
max_iterations = "{max_iterations}"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "echo-bench"
command = ["sh", "-c", "echo 'metric_value: 42.0'"]
timeout = 10
adaptor = {{ type = "regex", patterns = [{{ name = "metric_value", pattern = "metric_value: ([0-9.]+)" }}] }}

[score]
type = "weighted_sum"
primary_metrics = [{{ name = "metric_value", direction = "Minimize", weight = 1.0 }}]
guardrail_metrics = []
"#
    );
    std::fs::write(dir.join(".autotune.toml"), config).unwrap();
}

fn write_config(dir: &Path) {
    write_config_with_iterations(dir, "1");
}

fn write_config_with_failing_test(dir: &Path) {
    // `max_fix_attempts = 0` disables the Fixing-phase retry loop so this
    // legacy test keeps asserting the direct "tests fail → discard" path.
    // The fix-retry flow is covered by the `scenario_run_fix_retry_*`
    // tests; this test pins the disabled-budget contract.
    let config = r#"
[task]
name = "test-task"
description = "integration test with failing test"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[agent.implementation]
max_fix_attempts = 0

[[test]]
name = "always-fail"
command = ["sh", "-c", "echo 'test failed' >&2; exit 1"]
timeout = 10

[[measure]]
name = "echo-bench"
command = ["sh", "-c", "echo 'metric_value: 42.0'"]
timeout = 10
adaptor = { type = "regex", patterns = [{ name = "metric_value", pattern = "metric_value: ([0-9.]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric_value", direction = "Minimize", weight = 1.0 }]
guardrail_metrics = []
"#;
    std::fs::write(dir.join(".autotune.toml"), config).unwrap();
}

fn load_test_config(dir: &Path) -> AutotuneConfig {
    AutotuneConfig::load(&dir.join(".autotune.toml")).expect("failed to load test config")
}

fn build_test_scorer() -> WeightedSumScorer {
    WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "metric_value".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![],
    )
}

/// Set up a full task ready for run_task():
/// baseline recorded, state at Planning, research session mocked.
fn setup_task(repo_root: &Path, config: &AutotuneConfig) -> TaskStore {
    let task_dir = config.task_dir(repo_root);
    let store = TaskStore::new(&task_dir).expect("failed to create store");

    let baseline = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: HashMap::from([("metric_value".to_string(), 100.0)]),
        rank: 0.0,
        score: None,
        reason: None,
        fix_attempts: 0,
        fresh_spawns: 0,
        timestamp: Utc::now(),
    };
    store.append_ledger(&baseline).unwrap();

    // Create the advancing branch (mirrors what cmd_run does).
    let advancing_branch = format!("autotune-{}", config.task.name);
    Command::new("git")
        .args(["branch", &advancing_branch, "main"])
        .current_dir(repo_root)
        .output()
        .expect("failed to create advancing branch");

    let initial_state = TaskState {
        task_name: config.task.name.clone(),
        canonical_branch: "main".to_string(),
        advancing_branch,
        research_session_id: "mock-session-001".to_string(),
        current_iteration: 1,
        current_phase: Phase::Planning,
        current_approach: None,
    };
    store.save_state(&initial_state).unwrap();

    store
}

// ===========================================================================
// Test: single iteration, kept
// ===========================================================================

#[test]
fn test_single_iteration_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config(repo_root);
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    // Task produces metric_value=42.0, baseline=100.0, Minimize → improvement → keep
    let agent = MockAgent::builder()
        .hypothesis("opt-1", "reduce allocations", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .expect("run_task failed");

    let ledger = store.load_ledger().unwrap();
    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger[0].status, IterationStatus::Baseline);
    assert_eq!(ledger[1].status, IterationStatus::Kept);
    assert_eq!(ledger[1].approach, "opt-1");
    assert!(ledger[1].rank > 0.0);
    assert!(ledger[1].metrics.contains_key("metric_value"));
    assert_eq!(ledger[1].metrics["metric_value"], 42.0);

    let state = store.load_state().unwrap();
    assert_eq!(state.current_phase, Phase::Done);

    // The mock agent should have been called:
    // 1 send (planning) + 1 spawn (implementation)
    assert_eq!(agent.send_count(), 1);
    assert!(agent.spawn_count() >= 1);

    // Advancing branch should exist and be ahead of canonical (main).
    let advancing = &state.advancing_branch;
    assert!(
        autotune_git::has_commits_ahead(repo_root, "main", advancing).unwrap(),
        "advancing branch should be ahead of main"
    );
}

// ===========================================================================
// Test: multiple iterations (mix of keep and discard)
// ===========================================================================

#[test]
fn test_multiple_iterations_with_discards() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config_with_iterations(repo_root, "3");
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    // 3 unique hypotheses. The measure always returns metric_value=42.0.
    // Iteration 1: 42 vs baseline 100 → improvement → kept
    // Iteration 2: 42 vs best 42 → no improvement → discarded
    // Iteration 3: 42 vs best 42 → no improvement → discarded
    let agent = MockAgent::builder()
        .hypothesis("opt-1", "reduce allocations", &["src/lib.rs"])
        .hypothesis("opt-2", "use SIMD", &["src/simd.rs"])
        .hypothesis("opt-3", "cache results", &["src/cache.rs"])
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .expect("run_task failed");

    let ledger = store.load_ledger().unwrap();
    // baseline + 3 iterations = 4 entries
    assert_eq!(
        ledger.len(),
        4,
        "expected baseline + 3 iterations, got {}",
        ledger.len()
    );
    assert_eq!(ledger[0].status, IterationStatus::Baseline);
    assert_eq!(ledger[1].status, IterationStatus::Kept);
    assert_eq!(ledger[1].approach, "opt-1");
    // Iterations 2 and 3 compare 42 vs best=42 → rank=0 → discard
    assert_eq!(ledger[2].status, IterationStatus::Discarded);
    assert_eq!(ledger[2].approach, "opt-2");
    assert_eq!(ledger[3].status, IterationStatus::Discarded);
    assert_eq!(ledger[3].approach, "opt-3");

    let state = store.load_state().unwrap();
    assert_eq!(state.current_phase, Phase::Done);
    assert_eq!(agent.send_count(), 3);
}

// ===========================================================================
// Test: implementation crash (NoCommit) → discard, continue
// ===========================================================================

#[test]
fn test_no_commit_records_crash_and_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config_with_iterations(repo_root, "2");
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    // First hypothesis: agent does NOT commit (crash). Second: commits normally.
    // We use a stateful closure to commit on the second call only.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    let agent = MockAgent::builder()
        .hypothesis("crash-attempt", "this will crash", &["src/lib.rs"])
        .hypothesis("good-attempt", "this will work", &["src/lib.rs"])
        .implementation_behavior(ImplBehavior::Custom(Box::new(move |dir| {
            let n = call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                // First implementation: don't commit (simulates crash)
                return;
            }
            // Subsequent: create a commit
            let dummy = dir.join("mock_change.txt");
            std::fs::write(&dummy, format!("change #{n}")).unwrap();
            Command::new("git")
                .args(["add", "."])
                .current_dir(dir)
                .output()
                .unwrap();
            Command::new("git")
                .args(["commit", "-m", &format!("mock impl #{n}")])
                .current_dir(dir)
                .output()
                .unwrap();
        })))
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .expect("run_task failed");

    let ledger = store.load_ledger().unwrap();
    // baseline + crash + kept = 3 entries, but max_iterations=2 counts non-baseline iterations
    assert!(
        ledger.len() >= 3,
        "expected at least 3 ledger entries, got {}",
        ledger.len()
    );

    // First non-baseline should be crash
    assert_eq!(ledger[1].status, IterationStatus::Crash);
    assert_eq!(ledger[1].approach, "crash-attempt");

    // Second non-baseline should be kept
    assert_eq!(ledger[2].status, IterationStatus::Kept);
    assert_eq!(ledger[2].approach, "good-attempt");

    let state = store.load_state().unwrap();
    assert_eq!(state.current_phase, Phase::Done);
}

// ===========================================================================
// Test: test failure → discard, continue
// ===========================================================================

#[test]
fn test_failure_discards_and_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config_with_failing_test(repo_root);
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    let agent = MockAgent::builder()
        .hypothesis("doomed", "this will fail tests", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .expect("run_task failed");

    let ledger = store.load_ledger().unwrap();
    assert_eq!(ledger.len(), 2, "expected baseline + 1 discarded");
    assert_eq!(ledger[1].status, IterationStatus::Discarded);
    assert_eq!(ledger[1].approach, "doomed");
    assert!(
        ledger[1].reason.as_deref().unwrap_or("").contains("test"),
        "discard reason should mention test failure"
    );

    // Test output artifact should exist
    let test_output_path = store.iteration_dir(1, "doomed").join("test_output.txt");
    assert!(
        test_output_path.exists(),
        "test_output.txt should be saved on failure"
    );
}

// ===========================================================================
// Test: graceful shutdown via flag
// ===========================================================================

#[test]
fn test_shutdown_flag_stops_task() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config_with_iterations(repo_root, "inf");
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "optimize", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();

    // Set shutdown before running — should exit immediately
    let shutdown = AtomicBool::new(true);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .expect("run_task failed");

    // No iterations should have run
    let ledger = store.load_ledger().unwrap();
    assert_eq!(ledger.len(), 1, "only baseline should exist");

    let state = store.load_state().unwrap();
    assert_eq!(
        state.current_phase,
        Phase::Planning,
        "should still be at Planning"
    );
}

// ===========================================================================
// Test: state persistence across crash recovery
// ===========================================================================

#[test]
fn test_state_persisted_at_each_phase() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config(repo_root);
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "optimize", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();

    // Run phase by phase using run_single_phase to verify state persistence
    let mut state = store.load_state().unwrap();
    assert_eq!(state.current_phase, Phase::Planning);

    // Planning → Implementing
    autotune::machine::run_single_phase(
        &config, &agent, &scorer, repo_root, &store, &mut state, None,
    )
    .unwrap();
    assert_eq!(state.current_phase, Phase::Implementing);
    // Verify state was persisted
    let persisted = store.load_state().unwrap();
    assert_eq!(persisted.current_phase, Phase::Implementing);
    assert!(persisted.current_approach.is_some());

    // Implementing → Testing
    autotune::machine::run_single_phase(
        &config, &agent, &scorer, repo_root, &store, &mut state, None,
    )
    .unwrap();
    assert_eq!(state.current_phase, Phase::Testing);
    let persisted = store.load_state().unwrap();
    assert!(
        persisted
            .current_approach
            .as_ref()
            .unwrap()
            .commit_sha
            .is_some()
    );

    // Testing → Measuring
    autotune::machine::run_single_phase(
        &config, &agent, &scorer, repo_root, &store, &mut state, None,
    )
    .unwrap();
    assert_eq!(state.current_phase, Phase::Measuring);

    // Measuring → Scoring
    autotune::machine::run_single_phase(
        &config, &agent, &scorer, repo_root, &store, &mut state, None,
    )
    .unwrap();
    assert_eq!(state.current_phase, Phase::Scoring);
    let persisted = store.load_state().unwrap();
    assert!(
        persisted
            .current_approach
            .as_ref()
            .unwrap()
            .metrics
            .is_some()
    );

    // Scoring → Integrating (measure=42.0 < baseline=100.0, kept)
    autotune::machine::run_single_phase(
        &config, &agent, &scorer, repo_root, &store, &mut state, None,
    )
    .unwrap();
    assert_eq!(state.current_phase, Phase::Integrating);

    // Integrating → Recorded
    autotune::machine::run_single_phase(
        &config, &agent, &scorer, repo_root, &store, &mut state, None,
    )
    .unwrap();
    assert_eq!(state.current_phase, Phase::Recorded);

    // Recorded → Done (max_iterations=1)
    autotune::machine::run_single_phase(
        &config, &agent, &scorer, repo_root, &store, &mut state, None,
    )
    .unwrap();
    assert_eq!(state.current_phase, Phase::Done);
}

// ===========================================================================
// Test: iteration artifacts saved
// ===========================================================================

#[test]
fn test_iteration_artifacts_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config(repo_root);
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "optimize", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .unwrap();

    // metrics.json should exist for the kept iteration
    let metrics_path = store.iteration_dir(1, "opt-1").join("metrics.json");
    assert!(
        metrics_path.exists(),
        "metrics.json should be saved for kept iterations"
    );

    let content = std::fs::read_to_string(&metrics_path).unwrap();
    let metrics: HashMap<String, f64> = serde_json::from_str(&content).unwrap();
    assert_eq!(metrics["metric_value"], 42.0);
}

// ===========================================================================
// Test: mock agent tracking
// ===========================================================================

#[test]
fn test_mock_agent_tracks_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config_with_iterations(repo_root, "2");
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "optimize 1", &["src/lib.rs"])
        .hypothesis("opt-2", "optimize 2", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .unwrap();

    // 2 planning calls (send). Only 1st iteration produces an improvement (42 vs 100),
    // 2nd compares 42 vs best=42 → discard → no implementation spawn for discarded.
    // But implementation spawn happens before testing, so both iterations get spawned.
    assert_eq!(agent.send_count(), 2, "should have 2 planning calls");
    assert_eq!(
        agent.spawn_count(),
        2,
        "should have 2 implementation spawns"
    );

    // Last send message should contain task context
    let last_msg = agent.last_send_message().unwrap();
    assert!(
        last_msg.contains("integration test task"),
        "planning prompt should include description"
    );

    // Last spawn config should have sandboxed permissions
    let last_config = agent.last_spawn_config().unwrap();
    assert!(!last_config.prompt.is_empty());
}

// ===========================================================================
// Test: scorer pipeline validation (unit-level)
// ===========================================================================

#[test]
fn test_scorer_pipeline_keep() {
    let scorer = build_test_scorer();
    let baseline = HashMap::from([("metric_value".to_string(), 100.0)]);
    let candidate = HashMap::from([("metric_value".to_string(), 80.0)]);

    let output = scorer
        .calculate(&ScoreInput {
            baseline: baseline.clone(),
            candidate,
            best: baseline,
        })
        .unwrap();
    assert_eq!(output.decision, "keep");
    assert!(
        (output.rank - 0.2).abs() < 0.001,
        "20% improvement expected"
    );
}

#[test]
fn test_scorer_pipeline_discard() {
    let scorer = build_test_scorer();
    let baseline = HashMap::from([("metric_value".to_string(), 100.0)]);
    let candidate = HashMap::from([("metric_value".to_string(), 120.0)]);

    let output = scorer
        .calculate(&ScoreInput {
            baseline: baseline.clone(),
            candidate,
            best: baseline,
        })
        .unwrap();
    assert_eq!(output.decision, "discard");
    assert!(output.rank < 0.0);
}

#[test]
fn test_scorer_guardrail_blocks_improvement() {
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "time".to_string(),
            direction: Direction::Minimize,
            weight: 1.0,
        }],
        vec![GuardrailMetricDef {
            name: "accuracy".to_string(),
            direction: Direction::Maximize,
            max_regression: 0.01,
        }],
    );

    let baseline = HashMap::from([("time".to_string(), 100.0), ("accuracy".to_string(), 0.99)]);
    let candidate = HashMap::from([("time".to_string(), 50.0), ("accuracy".to_string(), 0.90)]);

    let output = scorer
        .calculate(&ScoreInput {
            baseline: baseline.clone(),
            candidate,
            best: baseline,
        })
        .unwrap();
    // Time improved massively but accuracy regressed 9% (exceeds 1% guardrail)
    assert_eq!(output.decision, "discard");
    assert!(output.reason.contains("guardrail"));
}

// ===========================================================================
// Test: config + measure end-to-end
// ===========================================================================

#[test]
fn test_config_loads_and_measures_run() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    write_config(dir);
    let config = load_test_config(dir);

    assert_eq!(config.task.name, "test-task");
    assert_eq!(config.measure.len(), 1);

    let metrics = run_all_measures(&config.measure, dir).expect("measures failed");
    assert!(metrics.contains_key("metric_value"));
    assert!((metrics["metric_value"] - 42.0).abs() < f64::EPSILON);
}

// ===========================================================================
// Test: implementation agent prompt and permissions
// ===========================================================================

#[test]
fn test_implementation_prompt_contains_instructions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config(repo_root);

    // Write an AGENTS.md so the implementation agent should pick it up.
    std::fs::write(
        repo_root.join("AGENTS.md"),
        "# Project Rules\n\nUse snake_case for all functions.\n",
    )
    .unwrap();
    Command::new("git")
        .args(["add", "AGENTS.md"])
        .current_dir(repo_root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "add AGENTS.md"])
        .current_dir(repo_root)
        .output()
        .unwrap();

    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "reduce allocations", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .unwrap();

    // run_task starts at Planning — the research agent was already spawned
    // in cmd_run, so the first spawn here is the implementation agent.
    let configs = agent.spawn_configs();
    assert!(!configs.is_empty(), "need at least 1 spawn");
    let impl_config = &configs[0]; // implementation agent

    // Prompt should contain AGENTS.md content
    assert!(
        impl_config.prompt.contains("snake_case"),
        "implementation prompt should include AGENTS.md content"
    );

    // Prompt should contain the hypothesis
    assert!(
        impl_config.prompt.contains("reduce allocations"),
        "implementation prompt should include hypothesis"
    );

    // Prompt should tell the agent not to ask for permission
    assert!(
        impl_config.prompt.contains("Do NOT ask for permission"),
        "implementation prompt should include permission guidance"
    );

    // Prompt should tell the agent to only modify listed files
    assert!(
        impl_config.prompt.contains("Only create or modify"),
        "implementation prompt should restrict file modifications"
    );

    // Permissions should include scoped Edit and Write
    let has_scoped_edit = impl_config
        .allowed_tools
        .iter()
        .any(|p| matches!(p, ToolPermission::AllowScoped(tool, _) if tool == "Edit"));
    let has_scoped_write = impl_config
        .allowed_tools
        .iter()
        .any(|p| matches!(p, ToolPermission::AllowScoped(tool, _) if tool == "Write"));
    assert!(has_scoped_edit, "should have scoped Edit permission");
    assert!(has_scoped_write, "should have scoped Write permission");

    // Should deny Bash and Agent
    let has_deny_bash = impl_config
        .allowed_tools
        .iter()
        .any(|p| matches!(p, ToolPermission::Deny(tool) if tool == "Bash"));
    let has_deny_agent = impl_config
        .allowed_tools
        .iter()
        .any(|p| matches!(p, ToolPermission::Deny(tool) if tool == "Agent"));
    assert!(has_deny_bash, "should deny Bash");
    assert!(has_deny_agent, "should deny Agent");
}

// ===========================================================================
// Test: research agent planning prompt content
// ===========================================================================

#[test]
fn test_research_planning_prompt_contains_context() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config(repo_root);
    let config = load_test_config(repo_root);
    let store = setup_task(repo_root, &config);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "optimize", &["src/lib.rs"])
        .build();
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_task(&config, &agent, &scorer, repo_root, &store, &shutdown, None)
        .unwrap();

    // The planning prompt is sent via send() — check the first message.
    let messages = agent.send_messages();
    assert!(!messages.is_empty(), "should have at least one send");
    let planning_msg = &messages[0];

    // Should contain iteration number and task description
    assert!(
        planning_msg.contains("Iteration 1"),
        "planning prompt should reference iteration number"
    );
    assert!(
        planning_msg.contains("integration test task"),
        "planning prompt should reference task description"
    );
}
