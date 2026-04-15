use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use chrono::Utc;

use autotune_agent::{Agent, AgentSession};
use autotune_config::AutotuneConfig;
use autotune_implement::ImplementError;
use autotune_plan::ToolApprover;
use autotune_score::{ScoreCalculator, ScoreInput};
use autotune_state::{
    ApproachState, IterationRecord, IterationStatus, Phase, TaskState, TaskStore,
};

pub type ShutdownFlag = AtomicBool;

/// Execute exactly one phase transition, returning the expected phase that was
/// executed. Returns `Ok(true)` if the task has reached the Done phase.
pub fn run_single_phase(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    scorer: &dyn ScoreCalculator,
    repo_root: &Path,
    store: &TaskStore,
    state: &mut TaskState,
    approver: Option<&dyn ToolApprover>,
) -> Result<bool> {
    let research_session = AgentSession {
        session_id: state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };

    autotune_agent::trace::record(
        "phase.enter",
        serde_json::json!({
            "iteration": state.current_iteration,
            "phase": state.current_phase.to_string(),
            "approach": state.current_approach.as_ref().map(|a| a.name.clone()),
        }),
    );

    match state.current_phase {
        Phase::Planning => {
            run_planning(config, agent, store, state, &research_session, approver)?;
        }
        Phase::Implementing => {
            run_implementing(config, agent, store, state)?;
        }
        Phase::Testing => {
            run_testing(config, store, state)?;
        }
        Phase::Measuring => {
            run_measuring(config, store, state)?;
        }
        Phase::Scoring => {
            run_scoring(scorer, store, state)?;
        }
        Phase::Integrating => {
            run_integrating(config, agent, repo_root, store, state, &research_session)?;
        }
        Phase::Recorded => {
            run_recorded(config, store, state)?;
        }
        Phase::Done => {
            println!("[autotune] task complete");
            return Ok(true);
        }
    }

    Ok(state.current_phase == Phase::Done)
}

pub fn run_task(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    scorer: &dyn ScoreCalculator,
    repo_root: &Path,
    store: &TaskStore,
    shutdown: &ShutdownFlag,
    approver: Option<&dyn ToolApprover>,
) -> Result<()> {
    let mut state = store.load_state().context("failed to load task state")?;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            println!("[autotune] shutdown requested, saving state and exiting");
            store.save_state(&state)?;
            return Ok(());
        }

        match run_single_phase(
            config, agent, scorer, repo_root, store, &mut state, approver,
        ) {
            Ok(true) => break,
            Ok(false) => continue,
            Err(e) => {
                // If the user pressed Ctrl+C while an agent call was in-flight,
                // the subprocess dies with SIGINT and the phase returns an
                // error. Recognize that and exit cleanly with saved state.
                if shutdown.load(Ordering::SeqCst) || is_interrupt_error(&e) {
                    println!("[autotune] interrupted — saving state and exiting");
                    shutdown.store(true, Ordering::SeqCst);
                    store.save_state(&state)?;
                    return Ok(());
                }
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Detect whether an error chain carries an `AgentError::Interrupted`.
fn is_interrupt_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<autotune_agent::AgentError>()
            .is_some_and(|a| matches!(a, autotune_agent::AgentError::Interrupted))
    })
}

fn run_planning(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    store: &TaskStore,
    state: &mut TaskState,
    research_session: &AgentSession,
    approver: Option<&dyn ToolApprover>,
) -> Result<()> {
    println!(
        "[autotune] iteration {} — planning",
        state.current_iteration
    );

    let ledger = store.load_ledger()?;
    let last_iteration = ledger.last();
    let description = config
        .task
        .description
        .as_deref()
        .unwrap_or(&config.task.name);

    let planning_stream = crate::stream_ui::Stream::research(&format!(
        "planning iteration {}...",
        state.current_iteration
    ));
    let planning_handler = planning_stream.handler();
    let hypothesis = autotune_plan::plan_next(
        agent,
        research_session,
        store,
        last_iteration,
        state.current_iteration,
        description,
        Some(&planning_handler),
        approver,
    )
    .context("planning failed")?;
    planning_stream.finish();

    // Show the user what the research agent chose before we advance into
    // implementation — otherwise the planning phase would silently transition
    // and the hypothesis would only surface later in the ledger.
    crate::stream_ui::render_hypothesis(state.current_iteration, &hypothesis);

    // Set up worktree
    let worktree_parent = store.root().join("worktrees");
    std::fs::create_dir_all(&worktree_parent)?;
    let repo_root =
        autotune_git::repo_root(store.root()).unwrap_or_else(|_| store.root().to_path_buf());
    let (worktree_path, branch_name) = autotune_implement::setup_worktree(
        &repo_root,
        &state.task_name,
        &hypothesis.approach,
        &worktree_parent,
        &state.advancing_branch,
    )
    .context("failed to set up worktree")?;
    println!(
        "[autotune] worktree: {} (branch {})",
        worktree_path.display(),
        branch_name
    );

    state.current_approach = Some(ApproachState {
        name: hypothesis.approach.clone(),
        hypothesis: hypothesis.hypothesis.clone(),
        worktree_path,
        branch_name,
        commit_sha: None,
        test_results: Vec::new(),
        metrics: None,
        rank: None,
        files_to_modify: hypothesis.files_to_modify.clone(),
    });
    state.current_phase = Phase::Implementing;
    store.save_state(state)?;
    Ok(())
}

fn run_implementing(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    store: &TaskStore,
    state: &mut TaskState,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Implementing phase")?;
    let impl_model = config
        .agent
        .implementation
        .as_ref()
        .and_then(|c| c.model.as_deref());
    println!(
        "[autotune] iteration {} — implementing '{}': model={}",
        state.current_iteration,
        approach.name,
        impl_model.unwrap_or("default"),
    );

    let impl_hypothesis = autotune_implement::Hypothesis {
        approach: approach.name.clone(),
        hypothesis: approach.hypothesis.clone(),
        files_to_modify: approach.files_to_modify.clone(),
    };

    let log_content = store.read_log().unwrap_or_default();
    let impl_max_turns = config
        .agent
        .implementation
        .as_ref()
        .and_then(|c| c.max_turns);

    let impl_stream = crate::stream_ui::Stream::implementation(&format!(
        "iteration {} — implementing '{}'...",
        state.current_iteration, approach.name
    ));

    let result = autotune_implement::run_implementation(
        agent,
        &impl_hypothesis,
        &approach.worktree_path,
        &approach.branch_name,
        &config.paths.tunable,
        &config.paths.denied,
        &log_content,
        impl_model,
        impl_max_turns,
        Some(impl_stream.handler()),
    );
    impl_stream.finish();

    match result {
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
            autotune_agent::trace::record(
                "phase.decision",
                serde_json::json!({
                    "phase": "Implementing",
                    "branch": "crash",
                    "reason": "implementation produced no commit",
                }),
            );
            record_crash(state, store)?;
        }
        Err(e) => {
            return Err(e).context("implementation failed");
        }
    }
    Ok(())
}

fn run_testing(config: &AutotuneConfig, store: &TaskStore, state: &mut TaskState) -> Result<()> {
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

    autotune_agent::trace::record(
        "phase.decision",
        serde_json::json!({
            "phase": "Testing",
            "branch": if all_pass { "pass" } else { "fail" },
            "results": test_results.iter().map(|r| serde_json::json!({
                "name": r.name,
                "passed": r.passed,
                "duration_secs": r.duration_secs,
            })).collect::<Vec<_>>(),
        }),
    );

    if all_pass {
        state.current_phase = Phase::Measuring;
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

fn run_measuring(config: &AutotuneConfig, store: &TaskStore, state: &mut TaskState) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Measuring phase")?;
    println!(
        "[autotune] iteration {} — measuring '{}'",
        state.current_iteration, approach.name
    );

    let (metrics, reports) =
        autotune_benchmark::run_all_measures_with_output(&config.measure, &approach.worktree_path)
            .context("measuring failed")?;

    // Persist raw stdout/stderr per measure so the research agent can fetch
    // extra detail on demand (only the metric values feed scoring).
    for report in &reports {
        let _ = store.save_measure_output(
            state.current_iteration,
            &approach.name,
            &report.name,
            &report.stdout,
            &report.stderr,
        );
    }

    let approach_mut = state.current_approach.as_mut().unwrap();
    approach_mut.metrics = Some(metrics);
    state.current_phase = Phase::Scoring;
    store.save_state(state)?;
    Ok(())
}

fn run_scoring(
    scorer: &dyn ScoreCalculator,
    store: &TaskStore,
    state: &mut TaskState,
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
    autotune_agent::trace::record(
        "phase.decision",
        serde_json::json!({
            "phase": "Scoring",
            "branch": score_output.decision,
            "rank": score_output.rank,
            "reason": score_output.reason,
            "metrics": candidate_metrics,
        }),
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
    _config: &AutotuneConfig,
    agent: &dyn Agent,
    repo_root: &Path,
    store: &TaskStore,
    state: &mut TaskState,
    research_session: &AgentSession,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Integrating phase")?;
    println!(
        "[autotune] iteration {} — integrating '{}'",
        state.current_iteration, approach.name
    );

    // Rebase the worktree branch onto the advancing branch. Run the rebase
    // inside the worktree directory since the branch is checked out there
    // (can't checkout a worktree-attached branch from the main repo).
    let wt = &approach.worktree_path;
    let clean = autotune_git::rebase(wt, &state.advancing_branch)
        .context("rebase onto advancing branch failed")?;

    if !clean && let Err(e) = resolve_rebase_conflicts(agent, wt, research_session) {
        println!("[autotune] conflict resolution failed: {e}, discarding");
        let _ = autotune_git::rebase_abort(wt);
        return record_discard(state, store, &format!("rebase conflict: {e}"));
    }

    // Remove worktree first so the branch is no longer attached, then
    // fast-forward the advancing branch to the rebased commits.
    let _ = autotune_git::remove_worktree(repo_root, &approach.worktree_path);
    autotune_git::checkout(repo_root, &state.advancing_branch)?;
    autotune_git::merge_ff_only(repo_root, &approach.branch_name)
        .context("fast-forward advancing branch failed")?;

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

    state.current_phase = Phase::Recorded;
    store.save_state(state)?;
    Ok(())
}

/// Resolve rebase conflicts by repeatedly asking the research agent to fix
/// conflicted files and continuing the rebase until it completes or fails.
fn resolve_rebase_conflicts(
    agent: &dyn Agent,
    repo_root: &Path,
    research_session: &AgentSession,
) -> Result<()> {
    // Grant Edit so the research agent can resolve conflict markers.
    if let Err(e) = agent.grant_session_permission(
        research_session,
        autotune_agent::ToolPermission::Allow("Edit".into()),
    ) {
        println!("[autotune] warning: could not grant Edit to research session: {e}");
    }

    // A rebase may hit multiple conflict steps (one per commit being replayed).
    // Loop until the rebase completes or we give up.
    const MAX_CONFLICT_ROUNDS: usize = 10;
    for round in 0..MAX_CONFLICT_ROUNDS {
        let conflicted = autotune_git::list_conflicted_files(repo_root).unwrap_or_default();
        if conflicted.is_empty() {
            break;
        }
        println!(
            "[autotune] rebase conflict round {} — {} file(s)",
            round + 1,
            conflicted.len()
        );

        let prompt = build_conflict_resolution_prompt(&conflicted, repo_root);
        let stream = crate::stream_ui::Stream::research("resolving rebase conflicts...");
        let handler = stream.handler();
        let result = agent.send_streaming(research_session, &prompt, Some(&handler));
        stream.finish();
        result.context("research agent failed during conflict resolution")?;

        // Check the agent actually resolved the conflicts.
        if autotune_git::has_merge_conflicts(repo_root).unwrap_or(true) {
            anyhow::bail!(
                "research agent did not resolve all conflicts (round {})",
                round + 1
            );
        }

        // Continue the rebase — may hit the next commit's conflicts.
        match autotune_git::rebase_continue(repo_root)? {
            true => return Ok(()), // Rebase completed.
            false => continue,     // Another conflict to resolve.
        }
    }

    anyhow::bail!("exceeded {MAX_CONFLICT_ROUNDS} conflict resolution rounds");
}

fn build_conflict_resolution_prompt(conflicted_files: &[String], repo_root: &Path) -> String {
    let mut prompt = String::new();
    prompt.push_str("# Merge Conflict Resolution\n\n");
    prompt.push_str("The iteration's branch is being merged into the canonical branch, but there are merge conflicts.\n\n");
    prompt.push_str("## Conflicted files\n\n");
    for f in conflicted_files {
        prompt.push_str(&format!("- `{f}`\n"));
    }
    prompt.push_str("\n## Instructions\n\n");
    prompt.push_str("1. Read each conflicted file to understand the conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`).\n");
    prompt.push_str(
        "2. Use Edit to resolve each conflict, keeping the intent of BOTH sides where possible.\n",
    );
    prompt.push_str(&format!(
        "3. The working directory is `{}`.\n",
        repo_root.display()
    ));
    prompt
        .push_str("4. Do NOT run any commands. Just resolve the conflicts by editing the files.\n");
    prompt.push_str("5. After resolving, end your response with `RESOLVED` on its own line.\n");
    prompt
}

fn run_recorded(config: &AutotuneConfig, store: &TaskStore, state: &mut TaskState) -> Result<()> {
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

fn record_crash(state: &mut TaskState, store: &TaskStore) -> Result<()> {
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

fn record_discard(state: &mut TaskState, store: &TaskStore, reason: &str) -> Result<()> {
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
    state.current_phase = Phase::Recorded;
    store.save_state(state)?;
    Ok(())
}

fn should_stop(config: &AutotuneConfig, store: &TaskStore) -> Result<bool> {
    let ledger = store.load_ledger()?;

    // Check max_iterations
    if let Some(ref max_iter) = config.task.max_iterations {
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
    if let Some(target) = config.task.target_improvement
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

    // Check target_metric — absolute thresholds on specific metrics.
    // Uses the most recent measured metrics (baseline or any iteration with metrics).
    if !config.task.target_metric.is_empty()
        && let Some(latest) = ledger.iter().rev().find(|r| !r.metrics.is_empty())
    {
        let all_met = config.task.target_metric.iter().all(|tm| {
            latest
                .metrics
                .get(&tm.name)
                .is_some_and(|v| match tm.direction {
                    autotune_config::Direction::Maximize => *v >= tm.value,
                    autotune_config::Direction::Minimize => *v <= tm.value,
                })
        });
        if all_met {
            let summary: Vec<String> = config
                .task
                .target_metric
                .iter()
                .map(|tm| {
                    let cur = latest.metrics.get(&tm.name).copied().unwrap_or(f64::NAN);
                    let op = match tm.direction {
                        autotune_config::Direction::Maximize => ">=",
                        autotune_config::Direction::Minimize => "<=",
                    };
                    format!("{}={:.4} {} {:.4}", tm.name, cur, op, tm.value)
                })
                .collect();
            println!(
                "[autotune] stop: target metric(s) reached ({})",
                summary.join(", ")
            );
            return Ok(true);
        }
    }

    Ok(false)
}
