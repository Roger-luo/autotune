use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use autotune_agent::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession};
use autotune_benchmark::run_all_benchmarks;
use autotune_config::AutotuneConfig;
use autotune_score::weighted_sum::{Direction, PrimaryMetricDef, WeightedSumScorer};
use autotune_score::{ScoreCalculator, ScoreInput};
use autotune_state::{ExperimentState, ExperimentStore, IterationRecord, IterationStatus, Phase};
use chrono::Utc;

// ---------------------------------------------------------------------------
// MockAgent
// ---------------------------------------------------------------------------

struct MockAgent {
    hypotheses: Vec<String>,
    send_count: Mutex<usize>,
    spawn_count: Mutex<usize>,
}

impl MockAgent {
    fn new(hypotheses: Vec<String>) -> Self {
        Self {
            hypotheses,
            send_count: Mutex::new(0),
            spawn_count: Mutex::new(0),
        }
    }
}

impl Agent for MockAgent {
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        let mut count = self.spawn_count.lock().unwrap();
        let idx = *count;
        *count += 1;

        // If this looks like an implementation spawn (worktree directory),
        // create a file and commit it so the SHA-before != SHA-after check passes.
        let wd = &config.working_directory;
        if idx > 0 || is_worktree_dir(wd) {
            create_dummy_commit(wd);
        }

        Ok(AgentResponse {
            text: "agent ready".to_string(),
            session_id: "mock-session".to_string(),
        })
    }

    fn send(&self, _session: &AgentSession, _message: &str) -> Result<AgentResponse, AgentError> {
        let mut count = self.send_count.lock().unwrap();
        let idx = *count % self.hypotheses.len();
        *count += 1;

        Ok(AgentResponse {
            text: self.hypotheses[idx].clone(),
            session_id: "mock-session".to_string(),
        })
    }

    fn backend_name(&self) -> &str {
        "mock"
    }

    fn handover_command(&self, _session: &AgentSession) -> String {
        "mock-handover".to_string()
    }
}

fn is_worktree_dir(path: &Path) -> bool {
    let git_path = path.join(".git");
    git_path.is_file()
}

fn create_dummy_commit(dir: &Path) {
    let dummy = dir.join("dummy_change.txt");
    std::fs::write(&dummy, format!("change at {:?}", std::time::Instant::now())).unwrap();

    Command::new("git")
        .args(["add", "dummy_change.txt"])
        .current_dir(dir)
        .output()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", "mock implementation commit"])
        .current_dir(dir)
        .output()
        .unwrap();
}

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

fn write_config(dir: &Path) {
    let config = r#"
[experiment]
name = "test-experiment"
description = "integration test experiment"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[benchmark]]
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

// ---------------------------------------------------------------------------
// Test 1: full pipeline one iteration
// ---------------------------------------------------------------------------

#[test]
fn test_full_pipeline_one_iteration() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let repo_root = tmp.path();

    init_temp_repo(repo_root);
    write_config(repo_root);
    let config = load_test_config(repo_root);

    // Create ExperimentStore
    let experiment_dir = config.experiment_dir(repo_root);
    let store = ExperimentStore::new(&experiment_dir).expect("failed to create store");

    // Save baseline
    let baseline = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: HashMap::from([("metric_value".to_string(), 100.0)]),
        rank: 0.0,
        score: None,
        reason: None,
        timestamp: Utc::now(),
    };
    store.append_ledger(&baseline).unwrap();

    // Save initial state
    let initial_state = ExperimentState {
        experiment_name: config.experiment.name.clone(),
        canonical_branch: "main".to_string(),
        research_session_id: "mock-session".to_string(),
        current_iteration: 1,
        current_phase: Phase::Planning,
        current_approach: None,
    };
    store.save_state(&initial_state).unwrap();

    // MockAgent: benchmark will produce metric_value=42.0, baseline=100.0, direction=Minimize
    // So 42 < 100 → improvement → keep
    let hypothesis_json = serde_json::json!({
        "approach": "opt-1",
        "hypothesis": "optimize thing to reduce metric",
        "files_to_modify": ["src/lib.rs"]
    });
    let agent = MockAgent::new(vec![hypothesis_json.to_string()]);
    let scorer = build_test_scorer();
    let shutdown = AtomicBool::new(false);

    autotune::machine::run_experiment(&config, &agent, &scorer, repo_root, &store, &shutdown)
        .expect("run_experiment failed");

    // Assertions
    let ledger = store.load_ledger().unwrap();
    assert_eq!(ledger.len(), 2, "expected baseline + 1 iteration");
    assert_eq!(ledger[0].status, IterationStatus::Baseline);
    assert_eq!(ledger[1].status, IterationStatus::Kept);
    assert_eq!(ledger[1].approach, "opt-1");
    assert!(
        ledger[1].rank > 0.0,
        "rank should be positive for improvement"
    );

    let final_state = store.load_state().unwrap();
    assert_eq!(final_state.current_phase, Phase::Done);
}

// ---------------------------------------------------------------------------
// Test 2: scorer pipeline validation
// ---------------------------------------------------------------------------

#[test]
fn test_scorer_pipeline_validation() {
    let scorer = build_test_scorer();

    let baseline = HashMap::from([("metric_value".to_string(), 100.0)]);
    let candidate = HashMap::from([("metric_value".to_string(), 80.0)]);

    let input = ScoreInput {
        baseline: baseline.clone(),
        candidate,
        best: baseline,
    };

    let output = scorer.calculate(&input).expect("scoring failed");
    assert_eq!(output.decision, "keep");
    assert!(output.rank > 0.0);
}

#[test]
fn test_scorer_regression_discards() {
    let scorer = build_test_scorer();

    let baseline = HashMap::from([("metric_value".to_string(), 100.0)]);
    let candidate = HashMap::from([("metric_value".to_string(), 120.0)]);

    let input = ScoreInput {
        baseline: baseline.clone(),
        candidate,
        best: baseline,
    };

    let output = scorer.calculate(&input).expect("scoring failed");
    assert_eq!(output.decision, "discard");
    assert!(output.rank < 0.0);
}

// ---------------------------------------------------------------------------
// Test 3: config loads and benchmarks run
// ---------------------------------------------------------------------------

#[test]
fn test_config_loads_and_benchmarks_run() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let dir = tmp.path();

    write_config(dir);
    let config = load_test_config(dir);

    assert_eq!(config.experiment.name, "test-experiment");
    assert_eq!(config.benchmark.len(), 1);

    let metrics = run_all_benchmarks(&config.benchmark, dir).expect("benchmarks failed");
    assert!(metrics.contains_key("metric_value"));
    assert!((metrics["metric_value"] - 42.0).abs() < f64::EPSILON);
}
