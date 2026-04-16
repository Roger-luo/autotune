use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Days, FixedOffset, NaiveTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;

use crate::agent_factory::{AgentRole, build_agent_for_backend, resolve_backend_name};
use autotune_agent::{Agent, AgentSession};
use autotune_config::AutotuneConfig;
use autotune_implement::{FixOutcome, ImplementError};
use autotune_plan::ToolApprover;
use autotune_score::{ScoreCalculator, ScoreInput};
use autotune_state::{
    ApproachState, IterationRecord, IterationStatus, Phase, TaskState, TaskStore,
};

pub type ShutdownFlag = AtomicBool;

#[derive(Debug)]
enum PhaseFailure {
    ExitCleanly,
    WaitAndRetry { until: DateTime<Utc> },
    Fatal(anyhow::Error),
}

struct CachedImplementationAgent {
    backend: String,
    agent: Box<dyn Agent>,
}

thread_local! {
    static IMPLEMENTATION_AGENT_CACHE: RefCell<HashMap<PathBuf, CachedImplementationAgent>> =
        RefCell::new(HashMap::new());
}

fn codex_reasoning_effort(
    effort: Option<autotune_config::ReasoningEffort>,
) -> Option<&'static str> {
    match effort {
        Some(autotune_config::ReasoningEffort::Low) => Some("low"),
        Some(autotune_config::ReasoningEffort::Medium) => Some("medium"),
        Some(autotune_config::ReasoningEffort::High) => Some("high"),
        None => None,
    }
}

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
    let research_session = research_session_from_state(state);

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
        Phase::Fixing => {
            run_fixing(config, agent, store, state)?;
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
                match classify_phase_failure(e, shutdown.load(Ordering::SeqCst), Utc::now()) {
                    // If the user pressed Ctrl+C while an agent call was in-flight,
                    // the subprocess dies with SIGINT and the phase returns an
                    // error. Recognize that and exit cleanly with saved state.
                    PhaseFailure::ExitCleanly => {
                        println!("[autotune] interrupted — saving state and exiting");
                        shutdown.store(true, Ordering::SeqCst);
                        store.save_state(&state)?;
                        return Ok(());
                    }
                    PhaseFailure::WaitAndRetry { until } => {
                        println!(
                            "[autotune] agent rate limited — waiting until {} and retrying",
                            until.format("%Y-%m-%d %H:%M:%S UTC")
                        );
                        store.save_state(&state)?;
                        sleep_until(until, Utc::now());
                        continue;
                    }
                    PhaseFailure::Fatal(e) => return Err(e),
                }
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

fn classify_phase_failure(
    err: anyhow::Error,
    shutdown_requested: bool,
    now: DateTime<Utc>,
) -> PhaseFailure {
    if shutdown_requested || is_interrupt_error(&err) {
        return PhaseFailure::ExitCleanly;
    }
    if let Some(until) = extract_rate_limit_reset(&err, now) {
        return PhaseFailure::WaitAndRetry { until };
    }
    PhaseFailure::Fatal(err)
}

fn extract_rate_limit_reset(err: &anyhow::Error, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    err.chain().find_map(|cause| {
        cause
            .downcast_ref::<autotune_agent::AgentError>()
            .and_then(|agent_err| match agent_err {
                autotune_agent::AgentError::CommandFailed { message } => {
                    parse_rate_limit_reset(message, now)
                }
                _ => None,
            })
    })
}

fn parse_rate_limit_reset(message: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let reset_idx = message.find("resets ")?;
    let tail = &message[reset_idx + "resets ".len()..];
    let clause = tail
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())?;

    let (time_str, timezone) = if let Some(paren_start) = clause.find('(') {
        let paren_end = clause[paren_start..].find(')')? + paren_start;
        (
            clause[..paren_start].trim(),
            Some(clause[paren_start + 1..paren_end].trim()),
        )
    } else {
        (clause, None)
    };

    let time = parse_reset_time(time_str)?;
    match timezone {
        Some(tz_name) if !tz_name.is_empty() => parse_reset_in_timezone(time, tz_name, now),
        _ => parse_reset_in_offset(time, now.fixed_offset().offset().fix(), now),
    }
}

fn parse_reset_time(raw: &str) -> Option<NaiveTime> {
    let normalized = raw.to_ascii_lowercase().replace(' ', "");

    if let Some(suffix) = normalized
        .strip_suffix("am")
        .map(|rest| (rest, "am"))
        .or_else(|| normalized.strip_suffix("pm").map(|rest| (rest, "pm")))
    {
        let (rest, meridiem) = suffix;
        let (hour_str, minute_str) = rest.split_once(':').map_or((rest, "0"), |(h, m)| (h, m));
        let mut hour: u32 = hour_str.parse().ok()?;
        let minute: u32 = minute_str.parse().ok()?;
        if hour > 12 || minute > 59 {
            return None;
        }
        hour %= 12;
        if meridiem == "pm" {
            hour += 12;
        }
        return NaiveTime::from_hms_opt(hour, minute, 0);
    }

    if let Some((hour_str, minute_str)) = normalized.split_once(':') {
        let hour: u32 = hour_str.parse().ok()?;
        let minute: u32 = minute_str.parse().ok()?;
        return NaiveTime::from_hms_opt(hour, minute, 0);
    }

    normalized
        .parse::<u32>()
        .ok()
        .and_then(|hour| NaiveTime::from_hms_opt(hour, 0, 0))
}

fn parse_reset_in_timezone(
    time: NaiveTime,
    timezone_name: &str,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let tz: Tz = timezone_name.parse().ok()?;
    let local_now = now.with_timezone(&tz);
    let mut date = local_now.date_naive();

    loop {
        let local_dt = date.and_time(time);
        if let Some(candidate) = tz
            .from_local_datetime(&local_dt)
            .earliest()
            .or_else(|| tz.from_local_datetime(&local_dt).latest())
            && candidate > local_now
        {
            return Some(candidate.with_timezone(&Utc));
        }
        date = date.checked_add_days(Days::new(1))?;
    }
}

fn parse_reset_in_offset(
    time: NaiveTime,
    offset: FixedOffset,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let local_now = now.with_timezone(&offset);
    let mut date = local_now.date_naive();

    loop {
        let local_dt = date.and_time(time);
        if let Some(candidate) = offset.from_local_datetime(&local_dt).single()
            && candidate > local_now
        {
            return Some(candidate.with_timezone(&Utc));
        }
        date = date.checked_add_days(Days::new(1))?;
    }
}

fn sleep_until(until: DateTime<Utc>, now: DateTime<Utc>) {
    let wait = wait_duration(now, until);
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
}

fn wait_duration(now: DateTime<Utc>, until: DateTime<Utc>) -> Duration {
    (until - now).to_std().unwrap_or_default()
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
        "[autotune] created branch '{}' from '{}'",
        branch_name, state.advancing_branch
    );
    println!(
        "[autotune] created worktree at {} on branch '{}'",
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
        impl_session_id: None,
        impl_backend: Some(implementation_backend_from_config(config)),
        fix_attempts: 0,
        fresh_spawns: 0,
        fix_history: Vec::new(),
    });
    state.current_phase = Phase::Implementing;
    store.save_state(state)?;
    Ok(())
}

fn run_implementing(
    config: &AutotuneConfig,
    research_agent: &dyn Agent,
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
    let impl_reasoning_effort = codex_reasoning_effort(
        config
            .agent
            .implementation
            .as_ref()
            .and_then(|c| c.reasoning_effort),
    );

    let impl_stream = crate::stream_ui::Stream::implementation(&format!(
        "iteration {} — implementing '{}'...",
        state.current_iteration, approach.name
    ));
    let result =
        with_implementation_agent(config, research_agent, store, Some(approach), |agent| {
            autotune_implement::run_implementation(
                agent,
                &impl_hypothesis,
                &approach.worktree_path,
                &approach.branch_name,
                &config.paths.tunable,
                &config.paths.denied,
                &log_content,
                impl_model,
                impl_max_turns,
                impl_reasoning_effort,
                Some(impl_stream.handler()),
            )
            .map_err(anyhow::Error::from)
        });
    impl_stream.finish();

    match result {
        Ok(result) => {
            let approach_mut = state.current_approach.as_mut().unwrap();
            approach_mut.commit_sha = Some(result.commit_sha);
            // Remember the implementer's session id so any Fixing turns can
            // reuse the same context without a fresh prompt.
            approach_mut.impl_session_id = Some(result.session_id);
            state.current_phase = Phase::Testing;
            store.save_state(state)?;
        }
        Err(e)
            if e.downcast_ref::<ImplementError>()
                .is_some_and(|e| matches!(e, ImplementError::NoCommit)) =>
        {
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
        Err(e) => return Err(e).context("implementation failed"),
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

        // Decide whether to hand off to Fixing or discard outright. Budget
        // lives on the implementation role; absent config defaults are
        // supplied by `AgentRoleConfig::effective_max_fix_attempts`.
        let (max_fix, _max_fresh) = fix_budget(config);
        if max_fix == 0 || approach_mut.fix_attempts >= max_fix {
            let reason = if max_fix == 0 {
                "tests failed".to_string()
            } else {
                format!(
                    "tests failed after {} fix attempt(s); budget exhausted",
                    approach_mut.fix_attempts
                )
            };
            println!(
                "[autotune] iteration {} — tests failed ({}), discarding",
                state.current_iteration, reason
            );
            record_discard(state, store, &reason)?;
        } else {
            // Stash the failure in the approach's history so a fresh respawn
            // (tier-2) still sees all prior failures, not just the most
            // recent one the session already knows about.
            approach_mut.fix_history.push(test_output);
            println!(
                "[autotune] iteration {} — tests failed, entering Fixing (attempt {}/{})",
                state.current_iteration,
                approach_mut.fix_attempts + 1,
                max_fix
            );
            state.current_phase = Phase::Fixing;
            store.save_state(state)?;
        }
    }
    Ok(())
}

/// Resolve the implementer's fix-retry budget from config, returning
/// `(max_fix_attempts, max_fresh_spawns)`. When the `implementation` role
/// is absent, both defaults apply.
fn fix_budget(config: &AutotuneConfig) -> (u32, u32) {
    match &config.agent.implementation {
        Some(role) => (
            role.effective_max_fix_attempts(),
            role.effective_max_fresh_spawns(),
        ),
        None => (
            autotune_config::AgentRoleConfig::DEFAULT_MAX_FIX_ATTEMPTS,
            autotune_config::AgentRoleConfig::DEFAULT_MAX_FRESH_SPAWNS,
        ),
    }
}

/// Run one fix-retry turn: tier-1 (session continuation) if we still have a
/// live implementer session, otherwise tier-2 (fresh respawn). Each call
/// consumes exactly one fix-attempt budget slot; the caller loops back
/// through Testing afterwards.
fn run_fixing(
    config: &AutotuneConfig,
    research_agent: &dyn Agent,
    store: &TaskStore,
    state: &mut TaskState,
) -> Result<()> {
    let (max_fix, max_fresh) = fix_budget(config);
    let iteration = state.current_iteration;

    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Fixing phase")?;

    // Defence-in-depth: if we entered Fixing with no fix history we have
    // nothing to feed the implementer, which should not happen because
    // run_testing only transitions to Fixing after appending the failure.
    // Discard with a descriptive reason rather than send an empty prompt.
    if approach.fix_history.is_empty() {
        return record_discard(state, store, "entered Fixing with no test output");
    }
    let latest = approach.fix_history.last().cloned().unwrap_or_default();
    let worktree_path = approach.worktree_path.clone();
    let approach_name = approach.name.clone();
    let hypothesis = approach.hypothesis.clone();
    let files_to_modify = approach.files_to_modify.clone();
    let history = approach.fix_history.clone();
    let fresh_spawns = approach.fresh_spawns;

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
    let impl_reasoning_effort = codex_reasoning_effort(
        config
            .agent
            .implementation
            .as_ref()
            .and_then(|c| c.reasoning_effort),
    );

    // Same-process retries must reuse the original implementation-agent
    // instance because Claude/Codex keep session context in memory. After a
    // resume in a new process we still have the persisted session id, but not
    // the cached agent context, so we degrade to a fresh respawn instead of
    // trying a broken continuation.
    let reused_cached_agent =
        if std::env::var("AUTOTUNE_MOCK").is_ok() || research_agent.backend_name() == "mock" {
            approach.impl_session_id.is_some()
        } else {
            prepare_implementation_agent(config, store, Some(approach))?
        };
    let tier_one = can_continue_implementation_session(approach, reused_cached_agent);
    if !tier_one && fresh_spawns >= max_fresh {
        let reason = if approach.impl_session_id.is_some() && !reused_cached_agent {
            format!(
                "implementer session unavailable after restart and fresh-spawn budget ({max_fresh}) exhausted"
            )
        } else {
            format!(
                "implementer session unproductive and fresh-spawn budget ({max_fresh}) exhausted"
            )
        };
        println!("[autotune] iteration {iteration} — {reason}");
        return record_discard(state, store, &reason);
    }

    let stream_label = if tier_one {
        format!("iteration {iteration} — fixing '{approach_name}' (session continuation)")
    } else {
        format!("iteration {iteration} — fixing '{approach_name}' (fresh respawn)")
    };
    let impl_stream = crate::stream_ui::Stream::implementation(&stream_label);
    let outcome =
        with_implementation_agent(config, research_agent, store, Some(approach), |agent| {
            if tier_one {
                let session = implementation_session_from_approach(approach)
                    .context("missing implementation session in Fixing phase")?;
                autotune_implement::run_fix_turn(
                    agent,
                    &session,
                    &worktree_path,
                    &history[..history.len() - 1], // prior failures; latest is fed separately
                    &latest,
                    Some(impl_stream.handler()),
                )
                .context("implementer fix turn failed")
            } else {
                let log_content = store.read_log().unwrap_or_default();
                let prior_commits =
                    autotune_git::log_oneline(&worktree_path, &state.advancing_branch)
                        .unwrap_or_default();
                let impl_hypothesis = autotune_implement::Hypothesis {
                    approach: approach_name.clone(),
                    hypothesis: hypothesis.clone(),
                    files_to_modify: files_to_modify.clone(),
                };
                autotune_implement::run_fix_respawn(
                    agent,
                    &impl_hypothesis,
                    &worktree_path,
                    &config.paths.tunable,
                    &config.paths.denied,
                    &log_content,
                    &prior_commits,
                    &history,
                    impl_model,
                    impl_max_turns,
                    impl_reasoning_effort,
                    Some(impl_stream.handler()),
                )
                .context("implementer fix turn failed")
            }
        });

    impl_stream.finish();

    let outcome = outcome?;
    {
        let approach_mut = state.current_approach.as_mut().expect("approach set above");
        approach_mut.fix_attempts += 1;
        if !tier_one {
            approach_mut.fresh_spawns += 1;
        }
    }

    match outcome {
        FixOutcome::Committed {
            commit_sha,
            session_id,
        } => {
            let (fix_attempts, fresh_spawns) = {
                let approach_mut = state.current_approach.as_mut().unwrap();
                approach_mut.commit_sha = Some(commit_sha);
                approach_mut.impl_session_id = Some(session_id);
                (approach_mut.fix_attempts, approach_mut.fresh_spawns)
            };
            state.current_phase = Phase::Testing;
            store.save_state(state)?;
            autotune_agent::trace::record(
                "phase.decision",
                serde_json::json!({
                    "phase": "Fixing",
                    "branch": if tier_one { "continued" } else { "respawned" },
                    "fix_attempts": fix_attempts,
                    "fresh_spawns": fresh_spawns,
                }),
            );
        }
        FixOutcome::NoEdits { .. } => {
            if tier_one {
                let (fix_attempts, fresh_spawns) = {
                    let approach_mut = state.current_approach.as_mut().unwrap();
                    approach_mut.impl_session_id = None;
                    (approach_mut.fix_attempts, approach_mut.fresh_spawns)
                };
                println!(
                    "[autotune] iteration {iteration} — implementer session went unproductive; will respawn"
                );
                autotune_agent::trace::record(
                    "phase.decision",
                    serde_json::json!({
                        "phase": "Fixing",
                        "branch": "no_edits",
                        "tier_one": true,
                        "fix_attempts": fix_attempts,
                        "fresh_spawns": fresh_spawns,
                    }),
                );
                if fix_attempts >= max_fix || max_fresh == 0 {
                    let reason = if max_fresh == 0 {
                        "implementer session unproductive and respawn disabled".to_string()
                    } else {
                        format!(
                            "implementer session unproductive and fix-attempt budget ({max_fix}) exhausted"
                        )
                    };
                    return record_discard(state, store, &reason);
                }
                state.current_phase = Phase::Fixing;
                store.save_state(state)?;
            } else {
                // Fresh respawn also produced nothing — give up.
                let reason = "implementer produced no edits after fresh respawn".to_string();
                println!("[autotune] iteration {iteration} — {reason}");
                autotune_agent::trace::record(
                    "phase.decision",
                    serde_json::json!({
                        "phase": "Fixing",
                        "branch": "no_edits",
                        "tier_one": false,
                    }),
                );
                return record_discard(state, store, &reason);
            }
        }
    }
    Ok(())
}

fn research_session_from_state(state: &TaskState) -> AgentSession {
    AgentSession {
        session_id: state.research_session_id.clone(),
        backend: state.research_backend.clone(),
    }
}

fn implementation_session_from_approach(approach: &ApproachState) -> Option<AgentSession> {
    Some(AgentSession {
        session_id: approach.impl_session_id.clone()?,
        backend: approach
            .impl_backend
            .clone()
            .unwrap_or_else(|| "claude".to_string()),
    })
}

fn implementation_backend_from_config(config: &AutotuneConfig) -> String {
    resolve_backend_name(&config.agent, AgentRole::Implementation).to_string()
}

fn implementation_cache_backend(
    config: &AutotuneConfig,
    approach: Option<&ApproachState>,
) -> String {
    approach
        .and_then(|approach| approach.impl_backend.as_deref())
        .unwrap_or_else(|| resolve_backend_name(&config.agent, AgentRole::Implementation))
        .to_string()
}

fn prepare_implementation_agent(
    config: &AutotuneConfig,
    store: &TaskStore,
    approach: Option<&ApproachState>,
) -> Result<bool> {
    let cache_key = store.root().to_path_buf();
    let backend = implementation_cache_backend(config, approach);

    IMPLEMENTATION_AGENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let reused_cached_instance = cache
            .get(&cache_key)
            .is_some_and(|slot| slot.backend == backend);

        if !reused_cached_instance {
            cache.insert(
                cache_key,
                CachedImplementationAgent {
                    backend,
                    agent: build_implementation_agent(config, approach)?,
                },
            );
        }

        Ok(reused_cached_instance)
    })
}

fn with_cached_implementation_agent<T, E>(
    store: &TaskStore,
    f: impl FnOnce(&dyn Agent) -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let cache_key = store.root().to_path_buf();

    IMPLEMENTATION_AGENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let slot = cache
            .get_mut(&cache_key)
            .expect("implementation agent not prepared for current task");
        f(slot.agent.as_ref())
    })
}

fn with_implementation_agent<T>(
    config: &AutotuneConfig,
    _research_agent: &dyn Agent,
    store: &TaskStore,
    approach: Option<&ApproachState>,
    f: impl FnOnce(&dyn Agent) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if std::env::var("AUTOTUNE_MOCK").is_ok() || _research_agent.backend_name() == "mock" {
        return f(_research_agent);
    }

    prepare_implementation_agent(config, store, approach)?;
    with_cached_implementation_agent(store, f)
}

fn can_continue_implementation_session(
    approach: &ApproachState,
    reused_cached_instance: bool,
) -> bool {
    approach.impl_session_id.is_some() && reused_cached_instance
}

fn build_implementation_agent(
    config: &AutotuneConfig,
    approach: Option<&ApproachState>,
) -> Result<Box<dyn Agent>> {
    #[cfg(feature = "mock")]
    if std::env::var("AUTOTUNE_MOCK").is_ok() {
        let mut builder = autotune_mock::MockAgent::builder();

        if let Ok(path) = std::env::var("AUTOTUNE_MOCK_IMPL_SCRIPT")
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            for entry in content.split("\n---\n") {
                let entry = entry.strip_suffix('\n').unwrap_or(entry);
                builder = builder.implementation_script_entry(entry);
            }
        }

        return Ok(Box::new(builder.build()));
    }

    let backend = implementation_cache_backend(config, approach);
    build_agent_for_backend(&backend)
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
        fix_attempts: approach.fix_attempts,
        fresh_spawns: approach.fresh_spawns,
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
        fix_attempts: approach.fix_attempts,
        fresh_spawns: approach.fresh_spawns,
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
        fix_attempts: approach.fix_attempts,
        fresh_spawns: approach.fresh_spawns,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_minimal_config(
        max_iterations: Option<autotune_config::StopValue>,
        target_improvement: Option<f64>,
    ) -> autotune_config::AutotuneConfig {
        autotune_config::AutotuneConfig {
            task: autotune_config::TaskConfig {
                name: "test-task".to_string(),
                description: None,
                canonical_branch: "main".to_string(),
                max_iterations,
                target_improvement,
                max_duration: None,
                target_metric: vec![],
            },
            paths: autotune_config::PathsConfig {
                tunable: vec!["**/*.rs".to_string()],
                denied: vec![],
            },
            test: vec![],
            measure: vec![autotune_config::MeasureConfig {
                name: "perf".to_string(),
                command: vec!["cargo".to_string(), "bench".to_string()],
                timeout: 600,
                adaptor: autotune_config::AdaptorConfig::Regex { patterns: vec![] },
            }],
            score: autotune_config::ScoreConfig::WeightedSum {
                primary_metrics: vec![autotune_config::PrimaryMetric {
                    name: "perf".to_string(),
                    direction: autotune_config::Direction::Maximize,
                    weight: 1.0,
                }],
                guardrail_metrics: vec![],
            },
            agent: autotune_config::AgentConfig::default(),
        }
    }

    fn make_kept_record(rank: f64) -> autotune_state::IterationRecord {
        autotune_state::IterationRecord {
            iteration: 1,
            approach: "test-approach".to_string(),
            status: autotune_state::IterationStatus::Kept,
            hypothesis: None,
            metrics: std::collections::HashMap::new(),
            rank,
            score: None,
            reason: None,
            fix_attempts: 0,
            fresh_spawns: 0,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn fix_budget_defaults_without_impl_config() {
        let config = make_minimal_config(None, Some(0.1));
        let (max_fix, max_fresh) = fix_budget(&config);
        assert_eq!(
            max_fix,
            autotune_config::AgentRoleConfig::DEFAULT_MAX_FIX_ATTEMPTS
        );
        assert_eq!(
            max_fresh,
            autotune_config::AgentRoleConfig::DEFAULT_MAX_FRESH_SPAWNS
        );
    }

    #[test]
    fn fix_budget_custom_values_from_impl_config() {
        let mut config = make_minimal_config(None, Some(0.1));
        config.agent.implementation = Some(autotune_config::AgentRoleConfig {
            backend: None,
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: Some(5),
            max_fresh_spawns: Some(2),
        });
        let (max_fix, max_fresh) = fix_budget(&config);
        assert_eq!(max_fix, 5);
        assert_eq!(max_fresh, 2);
    }

    #[test]
    fn should_stop_false_with_no_conditions() {
        let config = make_minimal_config(None, None);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        assert!(!should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_true_when_max_iterations_reached() {
        let config = make_minimal_config(Some(autotune_config::StopValue::Finite(2)), None);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        store.append_ledger(&make_kept_record(0.0)).unwrap();
        store.append_ledger(&make_kept_record(0.0)).unwrap();
        assert!(should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_false_when_below_max_iterations() {
        let config = make_minimal_config(Some(autotune_config::StopValue::Finite(5)), None);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        store.append_ledger(&make_kept_record(0.0)).unwrap();
        assert!(!should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_infinite_iterations_never_stops() {
        let config = make_minimal_config(Some(autotune_config::StopValue::Infinite), None);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        for _ in 0..10 {
            store.append_ledger(&make_kept_record(0.0)).unwrap();
        }
        assert!(!should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_true_when_target_improvement_reached() {
        let config = make_minimal_config(None, Some(0.1));
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        store.append_ledger(&make_kept_record(0.15)).unwrap();
        assert!(should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_false_when_target_improvement_not_reached() {
        let config = make_minimal_config(None, Some(0.5));
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        store.append_ledger(&make_kept_record(0.1)).unwrap();
        assert!(!should_stop(&config, &store).unwrap());
    }

    #[test]
    fn is_interrupt_error_true_for_interrupted() {
        let err = anyhow::Error::from(autotune_agent::AgentError::Interrupted);
        assert!(is_interrupt_error(&err));
    }

    #[test]
    fn is_interrupt_error_false_for_other_errors() {
        let err = anyhow::Error::from(autotune_agent::AgentError::CommandFailed {
            message: "something went wrong".to_string(),
        });
        assert!(!is_interrupt_error(&err));
    }

    #[test]
    fn parse_rate_limit_reset_parses_named_timezone() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 16, 30, 0).unwrap();
        let reset =
            parse_rate_limit_reset("You've hit your limit · resets 2pm (America/Toronto)", now)
                .unwrap();
        assert_eq!(reset, Utc.with_ymd_and_hms(2026, 4, 15, 18, 0, 0).unwrap());
    }

    #[test]
    fn classify_phase_failure_waits_and_retries_on_rate_limit() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 16, 30, 0).unwrap();
        let err = anyhow::Error::from(autotune_agent::AgentError::CommandFailed {
            message: "claude exited with exit status: 1\nstdout: You've hit your limit · resets 2pm (America/Toronto)"
                .to_string(),
        });

        match classify_phase_failure(err, false, now) {
            PhaseFailure::WaitAndRetry { until } => {
                assert_eq!(until, Utc.with_ymd_and_hms(2026, 4, 15, 18, 0, 0).unwrap());
            }
            other => panic!("expected WaitAndRetry, got {other:?}"),
        }
    }

    #[test]
    fn wait_duration_saturates_for_past_reset() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 16, 30, 0).unwrap();
        let earlier = Utc.with_ymd_and_hms(2026, 4, 15, 16, 0, 0).unwrap();
        assert_eq!(wait_duration(now, earlier), Duration::ZERO);
    }

    #[test]
    fn build_conflict_resolution_prompt_contains_files() {
        let files = vec!["src/foo.rs".to_string(), "src/bar.rs".to_string()];
        let repo = PathBuf::from("/tmp/repo");
        let prompt = build_conflict_resolution_prompt(&files, &repo);
        assert!(
            prompt.contains("src/foo.rs"),
            "prompt should mention foo.rs"
        );
        assert!(
            prompt.contains("src/bar.rs"),
            "prompt should mention bar.rs"
        );
        assert!(
            prompt.contains("RESOLVED"),
            "prompt should contain RESOLVED marker"
        );
    }

    #[test]
    fn build_conflict_resolution_prompt_with_empty_files() {
        let files: Vec<String> = vec![];
        let repo = PathBuf::from("/tmp/repo");
        let prompt = build_conflict_resolution_prompt(&files, &repo);
        assert!(
            !prompt.is_empty(),
            "prompt should be non-empty even with no files"
        );
        assert!(
            prompt.contains("RESOLVED"),
            "prompt should always contain RESOLVED marker"
        );
    }

    fn make_task_state(iteration: usize, phase: Phase) -> autotune_state::TaskState {
        autotune_state::TaskState {
            task_name: "test-task".to_string(),
            canonical_branch: "main".to_string(),
            advancing_branch: "autotune/test-task-main".to_string(),
            research_session_id: "session-1".to_string(),
            research_backend: "claude".to_string(),
            current_iteration: iteration,
            current_phase: phase,
            current_approach: None,
        }
    }

    #[test]
    fn persisted_research_session_uses_task_state_backend() {
        let state = autotune_state::TaskState {
            task_name: "bench".to_string(),
            canonical_branch: "main".to_string(),
            advancing_branch: "autotune/bench-main".to_string(),
            research_session_id: "research-1".to_string(),
            research_backend: "codex".to_string(),
            current_iteration: 1,
            current_phase: Phase::Planning,
            current_approach: None,
        };

        let session = research_session_from_state(&state);

        assert_eq!(session.session_id, "research-1");
        assert_eq!(session.backend, "codex");
    }

    #[test]
    fn persisted_implementation_session_uses_approach_backend() {
        let approach = ApproachState {
            name: "fast-path".to_string(),
            hypothesis: "trim allocations".to_string(),
            worktree_path: PathBuf::from("/tmp/worktree"),
            branch_name: "autotune/bench/fast-path".to_string(),
            commit_sha: None,
            test_results: vec![],
            metrics: None,
            rank: None,
            files_to_modify: vec![],
            impl_session_id: Some("impl-1".to_string()),
            impl_backend: Some("claude".to_string()),
            fix_attempts: 0,
            fresh_spawns: 0,
            fix_history: vec![],
        };

        let session = implementation_session_from_approach(&approach).unwrap();

        assert_eq!(session.session_id, "impl-1");
        assert_eq!(session.backend, "claude");
    }

    #[test]
    fn implementation_backend_from_config_prefers_role_override() {
        let mut config = make_minimal_config(None, None);
        config.agent.backend = "claude".to_string();
        config.agent.implementation = Some(autotune_config::AgentRoleConfig {
            backend: Some("codex".to_string()),
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: None,
            max_fresh_spawns: None,
        });

        assert_eq!(implementation_backend_from_config(&config), "codex");
    }

    #[test]
    fn prepare_implementation_agent_reuses_cached_instance_for_same_task() {
        let config = make_minimal_config(None, None);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();

        let reused = prepare_implementation_agent(&config, &store, None).unwrap();
        assert!(!reused);
        let first = with_cached_implementation_agent(&store, |agent| {
            Ok::<usize, anyhow::Error>(std::ptr::from_ref(agent).cast::<()>() as usize)
        })
        .unwrap();

        let reused = prepare_implementation_agent(&config, &store, None).unwrap();
        assert!(reused);
        let second = with_cached_implementation_agent(&store, |agent| {
            Ok::<usize, anyhow::Error>(std::ptr::from_ref(agent).cast::<()>() as usize)
        })
        .unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn implementation_session_continuation_requires_cached_agent() {
        let approach = ApproachState {
            name: "fast-path".to_string(),
            hypothesis: "trim allocations".to_string(),
            worktree_path: PathBuf::from("/tmp/worktree"),
            branch_name: "autotune/bench/fast-path".to_string(),
            commit_sha: None,
            test_results: vec![],
            metrics: None,
            rank: None,
            files_to_modify: vec![],
            impl_session_id: Some("impl-1".to_string()),
            impl_backend: Some("claude".to_string()),
            fix_attempts: 0,
            fresh_spawns: 0,
            fix_history: vec![],
        };

        assert!(!can_continue_implementation_session(&approach, false));
        assert!(can_continue_implementation_session(&approach, true));
    }

    #[test]
    fn run_recorded_transitions_to_done_when_should_stop() {
        // max_iterations = 1 and one ledger entry means should_stop returns true
        let config = make_minimal_config(Some(autotune_config::StopValue::Finite(1)), None);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        store.append_ledger(&make_kept_record(0.0)).unwrap();
        let mut state = make_task_state(1, Phase::Recorded);
        run_recorded(&config, &store, &mut state).unwrap();
        assert_eq!(state.current_phase, Phase::Done);
    }

    #[test]
    fn run_recorded_increments_iteration_and_goes_to_planning() {
        // no stop conditions set → should_stop returns false
        let config = make_minimal_config(None, None);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        let mut state = make_task_state(1, Phase::Recorded);
        run_recorded(&config, &store, &mut state).unwrap();
        assert_eq!(state.current_phase, Phase::Planning);
        assert_eq!(state.current_iteration, 2);
        assert!(state.current_approach.is_none());
    }

    fn make_config_with_target_metric(
        name: &str,
        value: f64,
        direction: autotune_config::Direction,
    ) -> autotune_config::AutotuneConfig {
        let mut config = make_minimal_config(None, None);
        config.task.target_metric = vec![autotune_config::TargetMetric {
            name: name.to_string(),
            value,
            direction,
        }];
        config
    }

    fn make_record_with_metrics(
        metrics: std::collections::HashMap<String, f64>,
    ) -> autotune_state::IterationRecord {
        autotune_state::IterationRecord {
            iteration: 1,
            approach: "test-approach".to_string(),
            status: autotune_state::IterationStatus::Kept,
            hypothesis: None,
            metrics,
            rank: 0.0,
            score: None,
            reason: None,
            fix_attempts: 0,
            fresh_spawns: 0,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn should_stop_true_when_maximize_target_metric_reached() {
        let config =
            make_config_with_target_metric("cov", 80.0, autotune_config::Direction::Maximize);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        let mut m = std::collections::HashMap::new();
        m.insert("cov".to_string(), 85.0);
        store.append_ledger(&make_record_with_metrics(m)).unwrap();
        assert!(should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_false_when_maximize_target_metric_not_reached() {
        let config =
            make_config_with_target_metric("cov", 80.0, autotune_config::Direction::Maximize);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        let mut m = std::collections::HashMap::new();
        m.insert("cov".to_string(), 70.0);
        store.append_ledger(&make_record_with_metrics(m)).unwrap();
        assert!(!should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_true_when_minimize_target_metric_reached() {
        let config =
            make_config_with_target_metric("latency", 10.0, autotune_config::Direction::Minimize);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        let mut m = std::collections::HashMap::new();
        m.insert("latency".to_string(), 8.0);
        store.append_ledger(&make_record_with_metrics(m)).unwrap();
        assert!(should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_false_when_minimize_target_metric_not_reached() {
        let config =
            make_config_with_target_metric("latency", 10.0, autotune_config::Direction::Minimize);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        let mut m = std::collections::HashMap::new();
        m.insert("latency".to_string(), 15.0);
        store.append_ledger(&make_record_with_metrics(m)).unwrap();
        assert!(!should_stop(&config, &store).unwrap());
    }

    #[test]
    fn should_stop_false_when_target_metric_has_no_ledger_entries_with_metrics() {
        let config =
            make_config_with_target_metric("cov", 80.0, autotune_config::Direction::Maximize);
        let tmp = tempfile::tempdir().unwrap();
        let store = autotune_state::TaskStore::new(tmp.path()).unwrap();
        // Append a record with empty metrics — the target_metric check should not fire
        store.append_ledger(&make_kept_record(0.5)).unwrap();
        assert!(!should_stop(&config, &store).unwrap());
    }
}
