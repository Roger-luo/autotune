use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use chrono::Utc;

use autotune_agent::{Agent, AgentSession};
use autotune_config::AutotuneConfig;
use autotune_implement::ImplementError;
use autotune_score::{ScoreCalculator, ScoreInput};
use autotune_state::{
    ApproachState, ExperimentState, ExperimentStore, IterationRecord, IterationStatus, Phase,
};

pub type ShutdownFlag = AtomicBool;

/// Execute exactly one phase transition, returning the expected phase that was
/// executed. Returns `Ok(true)` if the experiment has reached the Done phase.
pub fn run_single_phase(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    scorer: &dyn ScoreCalculator,
    repo_root: &Path,
    store: &ExperimentStore,
    state: &mut ExperimentState,
) -> Result<bool> {
    let research_session = AgentSession {
        session_id: state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };

    match state.current_phase {
        Phase::Planning => {
            run_planning(config, agent, store, state, &research_session)?;
        }
        Phase::Implementing => {
            run_implementing(config, agent, store, state)?;
        }
        Phase::Testing => {
            run_testing(config, store, state)?;
        }
        Phase::Benchmarking => {
            run_benchmarking(config, store, state)?;
        }
        Phase::Scoring => {
            run_scoring(scorer, store, state)?;
        }
        Phase::Integrating => {
            run_integrating(repo_root, store, state)?;
        }
        Phase::Recorded => {
            run_recorded(config, store, state)?;
        }
        Phase::Done => {
            println!("[autotune] experiment complete");
            return Ok(true);
        }
    }

    Ok(state.current_phase == Phase::Done)
}

pub fn run_experiment(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    scorer: &dyn ScoreCalculator,
    repo_root: &Path,
    store: &ExperimentStore,
    shutdown: &ShutdownFlag,
) -> Result<()> {
    let mut state = store
        .load_state()
        .context("failed to load experiment state")?;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            println!("[autotune] shutdown requested, saving state and exiting");
            store.save_state(&state)?;
            return Ok(());
        }

        let done = run_single_phase(config, agent, scorer, repo_root, store, &mut state)?;
        if done {
            break;
        }
    }

    Ok(())
}

fn run_planning(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    store: &ExperimentStore,
    state: &mut ExperimentState,
    research_session: &AgentSession,
) -> Result<()> {
    println!(
        "[autotune] iteration {} — planning",
        state.current_iteration
    );

    let ledger = store.load_ledger()?;
    let last_iteration = ledger.last();
    let description = config
        .experiment
        .description
        .as_deref()
        .unwrap_or(&config.experiment.name);

    let hypothesis = autotune_plan::plan_next(
        agent,
        research_session,
        store,
        last_iteration,
        state.current_iteration,
        description,
    )
    .context("planning failed")?;

    // Set up worktree
    let worktree_parent = store.root().join("worktrees");
    std::fs::create_dir_all(&worktree_parent)?;
    let repo_root =
        autotune_git::repo_root(store.root()).unwrap_or_else(|_| store.root().to_path_buf());
    let (worktree_path, branch_name) =
        autotune_implement::setup_worktree(&repo_root, &hypothesis.approach, &worktree_parent)
            .context("failed to set up worktree")?;

    state.current_approach = Some(ApproachState {
        name: hypothesis.approach.clone(),
        hypothesis: hypothesis.hypothesis.clone(),
        worktree_path,
        branch_name,
        commit_sha: None,
        test_results: Vec::new(),
        metrics: None,
        rank: None,
    });
    state.current_phase = Phase::Implementing;
    store.save_state(state)?;
    Ok(())
}

fn run_implementing(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    store: &ExperimentStore,
    state: &mut ExperimentState,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Implementing phase")?;
    println!(
        "[autotune] iteration {} — implementing '{}'",
        state.current_iteration, approach.name
    );

    let impl_hypothesis = autotune_implement::Hypothesis {
        approach: approach.name.clone(),
        hypothesis: approach.hypothesis.clone(),
        files_to_modify: Vec::new(), // agent figures this out
    };

    let log_content = store.read_log().unwrap_or_default();
    let impl_model = config
        .agent
        .implementation
        .as_ref()
        .and_then(|c| c.model.as_deref());
    let impl_max_turns = config
        .agent
        .implementation
        .as_ref()
        .and_then(|c| c.max_turns);

    match autotune_implement::run_implementation(
        agent,
        &impl_hypothesis,
        &approach.worktree_path,
        &approach.branch_name,
        &config.paths.tunable,
        &log_content,
        impl_model,
        impl_max_turns,
    ) {
        Ok(result) => {
            let approach_mut = state.current_approach.as_mut().unwrap();
            approach_mut.commit_sha = Some(result.commit_sha);
            state.current_phase = Phase::Testing;
            store.save_state(state)?;
        }
        Err(ImplementError::NoCommit) => {
            println!(
                "[autotune] iteration {} — implementation produced no commit, recording as crash",
                state.current_iteration
            );
            record_crash(state, store)?;
        }
        Err(e) => {
            return Err(e).context("implementation failed");
        }
    }
    Ok(())
}

fn run_testing(
    config: &AutotuneConfig,
    store: &ExperimentStore,
    state: &mut ExperimentState,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Testing phase")?;
    println!(
        "[autotune] iteration {} — testing '{}'",
        state.current_iteration, approach.name
    );

    let test_results = autotune_test::run_all_tests(&config.test, &approach.worktree_path)
        .context("test execution failed")?;

    let all_pass = autotune_test::all_passed(&test_results);

    // Convert test results to state format
    let state_test_results: Vec<autotune_state::TestResult> = test_results
        .iter()
        .map(|r| autotune_state::TestResult {
            name: r.name.clone(),
            passed: r.passed,
            duration_secs: r.duration_secs,
            output: Some(format!("{}\n{}", r.stdout, r.stderr)),
        })
        .collect();

    let approach_mut = state.current_approach.as_mut().unwrap();
    approach_mut.test_results = state_test_results;

    if all_pass {
        state.current_phase = Phase::Benchmarking;
        store.save_state(state)?;
    } else {
        println!(
            "[autotune] iteration {} — tests failed, discarding",
            state.current_iteration
        );

        // Save test output
        let test_output: String = test_results
            .iter()
            .map(|r| {
                format!(
                    "=== {} ({}) ===\nstdout:\n{}\nstderr:\n{}\n",
                    r.name,
                    if r.passed { "PASS" } else { "FAIL" },
                    r.stdout,
                    r.stderr
                )
            })
            .collect();
        let _ = store.save_test_output(state.current_iteration, &approach_mut.name, &test_output);

        record_discard(state, store, "tests failed")?;
    }
    Ok(())
}

fn run_benchmarking(
    config: &AutotuneConfig,
    store: &ExperimentStore,
    state: &mut ExperimentState,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Benchmarking phase")?;
    println!(
        "[autotune] iteration {} — benchmarking '{}'",
        state.current_iteration, approach.name
    );

    let metrics =
        autotune_benchmark::run_all_benchmarks(&config.benchmark, &approach.worktree_path)
            .context("benchmarking failed")?;

    let approach_mut = state.current_approach.as_mut().unwrap();
    approach_mut.metrics = Some(metrics);
    state.current_phase = Phase::Scoring;
    store.save_state(state)?;
    Ok(())
}

fn run_scoring(
    scorer: &dyn ScoreCalculator,
    store: &ExperimentStore,
    state: &mut ExperimentState,
) -> Result<()> {
    let approach_name = state
        .current_approach
        .as_ref()
        .map(|a| a.name.clone())
        .context("no current approach in Scoring phase")?;
    println!(
        "[autotune] iteration {} — scoring '{}'",
        state.current_iteration, approach_name
    );

    let candidate_metrics = state
        .current_approach
        .as_ref()
        .and_then(|a| a.metrics.clone())
        .context("no metrics in Scoring phase")?;

    let ledger = store.load_ledger()?;
    let baseline_metrics = ledger
        .iter()
        .find(|r| r.status == IterationStatus::Baseline)
        .map(|r| r.metrics.clone())
        .unwrap_or_default();

    // Best = last kept iteration's metrics, or baseline
    let best_metrics = ledger
        .iter()
        .rev()
        .find(|r| r.status == IterationStatus::Kept || r.status == IterationStatus::Baseline)
        .map(|r| r.metrics.clone())
        .unwrap_or_else(|| baseline_metrics.clone());

    let score_input = ScoreInput {
        baseline: baseline_metrics,
        candidate: candidate_metrics.clone(),
        best: best_metrics,
    };

    let score_output = scorer.calculate(&score_input).context("scoring failed")?;

    let approach_mut = state.current_approach.as_mut().unwrap();
    approach_mut.rank = Some(score_output.rank);

    println!(
        "[autotune] iteration {} — score: rank={:.4}, decision={}, reason={}",
        state.current_iteration, score_output.rank, score_output.decision, score_output.reason
    );

    if score_output.decision == "keep" {
        state.current_phase = Phase::Integrating;
        store.save_state(state)?;
    } else {
        // Save metrics before discarding
        let _ = store.save_iteration_metrics(
            state.current_iteration,
            &approach_name,
            &candidate_metrics,
        );
        record_discard(state, store, &score_output.reason)?;
    }
    Ok(())
}

fn run_integrating(
    repo_root: &Path,
    store: &ExperimentStore,
    state: &mut ExperimentState,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Integrating phase")?;
    println!(
        "[autotune] iteration {} — integrating '{}'",
        state.current_iteration, approach.name
    );

    let commit_sha = approach
        .commit_sha
        .as_ref()
        .context("no commit SHA in Integrating phase")?;

    // Cherry-pick onto canonical branch
    autotune_git::checkout(repo_root, &state.canonical_branch)?;
    autotune_git::cherry_pick(repo_root, commit_sha)?;

    let metrics = approach.metrics.clone().unwrap_or_default();
    let rank = approach.rank.unwrap_or(0.0);

    // Save iteration metrics
    let _ = store.save_iteration_metrics(state.current_iteration, &approach.name, &metrics);

    // Record as kept in ledger
    let record = IterationRecord {
        iteration: state.current_iteration,
        approach: approach.name.clone(),
        status: IterationStatus::Kept,
        hypothesis: Some(approach.hypothesis.clone()),
        metrics,
        rank,
        score: Some("keep".to_string()),
        reason: None,
        timestamp: Utc::now(),
    };
    store.append_ledger(&record)?;

    // Clean up worktree
    let _ = autotune_git::remove_worktree(repo_root, &approach.worktree_path);

    state.current_phase = Phase::Recorded;
    store.save_state(state)?;
    Ok(())
}

fn run_recorded(
    config: &AutotuneConfig,
    store: &ExperimentStore,
    state: &mut ExperimentState,
) -> Result<()> {
    println!(
        "[autotune] iteration {} — recorded",
        state.current_iteration
    );

    if should_stop(config, store)? {
        state.current_phase = Phase::Done;
        store.save_state(state)?;
    } else {
        state.current_iteration += 1;
        state.current_approach = None;
        state.current_phase = Phase::Planning;
        store.save_state(state)?;
    }
    Ok(())
}

fn record_crash(state: &mut ExperimentState, store: &ExperimentStore) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach")?;

    let record = IterationRecord {
        iteration: state.current_iteration,
        approach: approach.name.clone(),
        status: IterationStatus::Crash,
        hypothesis: Some(approach.hypothesis.clone()),
        metrics: Default::default(),
        rank: 0.0,
        score: None,
        reason: Some("implementation produced no commit".to_string()),
        timestamp: Utc::now(),
    };
    store.append_ledger(&record)?;

    // Clean up worktree
    let repo_root =
        autotune_git::repo_root(store.root()).unwrap_or_else(|_| store.root().to_path_buf());
    let _ = autotune_git::remove_worktree(&repo_root, &approach.worktree_path);

    state.current_iteration += 1;
    state.current_approach = None;
    state.current_phase = Phase::Planning;
    store.save_state(state)?;
    Ok(())
}

fn record_discard(
    state: &mut ExperimentState,
    store: &ExperimentStore,
    reason: &str,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach")?;

    let metrics = approach.metrics.clone().unwrap_or_default();
    let rank = approach.rank.unwrap_or(0.0);

    let record = IterationRecord {
        iteration: state.current_iteration,
        approach: approach.name.clone(),
        status: IterationStatus::Discarded,
        hypothesis: Some(approach.hypothesis.clone()),
        metrics,
        rank,
        score: Some("discard".to_string()),
        reason: Some(reason.to_string()),
        timestamp: Utc::now(),
    };
    store.append_ledger(&record)?;

    // Clean up worktree
    let repo_root =
        autotune_git::repo_root(store.root()).unwrap_or_else(|_| store.root().to_path_buf());
    let _ = autotune_git::remove_worktree(&repo_root, &approach.worktree_path);

    state.current_iteration += 1;
    state.current_approach = None;
    state.current_phase = Phase::Planning;
    store.save_state(state)?;
    Ok(())
}

fn should_stop(config: &AutotuneConfig, store: &ExperimentStore) -> Result<bool> {
    let ledger = store.load_ledger()?;

    // Check max_iterations
    if let Some(ref max_iter) = config.experiment.max_iterations {
        match max_iter {
            autotune_config::StopValue::Finite(max) => {
                let non_baseline_count = ledger
                    .iter()
                    .filter(|r| r.status != IterationStatus::Baseline)
                    .count() as u64;
                if non_baseline_count >= *max {
                    println!("[autotune] stop: reached max iterations ({max})");
                    return Ok(true);
                }
            }
            autotune_config::StopValue::Infinite => {}
        }
    }

    // Check target_improvement
    if let Some(target) = config.experiment.target_improvement
        && let Some(last_kept) = ledger
            .iter()
            .rev()
            .find(|r| r.status == IterationStatus::Kept)
        && last_kept.rank >= target
    {
        println!(
            "[autotune] stop: target improvement reached (rank {:.4} >= {:.4})",
            last_kept.rank, target
        );
        return Ok(true);
    }

    Ok(false)
}
