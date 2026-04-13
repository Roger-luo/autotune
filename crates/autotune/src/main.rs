mod cli;

use autotune::machine;
use autotune::resume;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;

use autotune_agent::Agent;
use autotune_agent::claude::ClaudeAgent;
use autotune_config::global::GlobalConfig;
use autotune_config::{AutotuneConfig, ScoreConfig};
use autotune_score::ScoreCalculator;
use autotune_score::script::ScriptScorer;
use autotune_score::threshold::{ThresholdConditionDef, ThresholdScorer};
use autotune_score::weighted_sum::{GuardrailMetricDef, PrimaryMetricDef, WeightedSumScorer};
use autotune_state::{IterationRecord, IterationStatus, Phase, TaskState, TaskStore};

use cli::{Cli, Commands, ConfigCommands, ReportFormat};

fn main() -> Result<()> {
    // Layer 2: catch panics that escape a Guard (e.g., panics in non-guarded code paths).
    autotune_agent::terminal::install_panic_hook();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run { task } => cmd_run(task),
        Commands::Resume {
            task,
            max_iterations,
            max_duration,
            target_improvement,
        } => cmd_resume(task, max_iterations, max_duration, target_improvement),
        Commands::Report { task, format } => cmd_report(task, format),
        Commands::List => cmd_list(),
        Commands::Init { name } => cmd_init(name),
        Commands::Plan { task } => cmd_step(task, Phase::Planning),
        Commands::Implement { task } => cmd_step(task, Phase::Implementing),
        Commands::Test { task } => cmd_step(task, Phase::Testing),
        Commands::Measure { task } => cmd_step(task, Phase::Measuring),
        Commands::Record { task } => cmd_step(task, Phase::Scoring),
        Commands::Apply { task } => cmd_step(task, Phase::Integrating),
        Commands::Config(sub) => cmd_config(sub),
        Commands::Export { task, output } => cmd_export(task, output),
    }
}

fn find_repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to get current directory")?;
    autotune_git::repo_root(&cwd).context("not in a git repository")
}

fn load_config(repo_root: &Path) -> Result<AutotuneConfig> {
    let config_path = repo_root.join(".autotune.toml");
    AutotuneConfig::load(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))
}

fn build_agent(_config: &AutotuneConfig) -> Box<dyn Agent> {
    #[cfg(feature = "mock")]
    if std::env::var("AUTOTUNE_MOCK").is_ok() {
        eprintln!("[autotune] using mock agent (AUTOTUNE_MOCK is set)");
        return Box::new(
            autotune_mock::MockAgent::builder()
                .hypothesis(
                    "mock-approach",
                    "mock hypothesis for testing",
                    &["src/lib.rs"],
                )
                .build(),
        );
    }
    Box::new(ClaudeAgent::new())
}

fn build_agent_from_global(_global_config: &GlobalConfig) -> Box<dyn Agent> {
    #[cfg(feature = "mock")]
    if std::env::var("AUTOTUNE_MOCK").is_ok() {
        eprintln!("[autotune] using mock agent (AUTOTUNE_MOCK is set)");
        return Box::new(mock_init_agent());
    }
    Box::new(ClaudeAgent::new())
}

#[cfg(feature = "mock")]
fn mock_init_agent() -> autotune_mock::MockAgent {
    autotune_mock::MockAgent::builder()
        // First: ask what the user wants to optimize
        .init_response(r#"{"type":"question","text":"I found a Rust workspace with 13 crates under crates/, a state machine architecture in the main binary, and cargo-nextest for testing. There are no existing measures or criterion dependency.\n\nWhat metric would you like autotune to improve?","options":[{"key":"perf","label":"Runtime performance","description":"execution speed and throughput of the state machine"},{"key":"size","label":"Binary size","description":"size of the compiled autotune CLI executable"},{"key":"coverage","label":"Test coverage","description":"line/branch coverage measured via cargo-tarpaulin or cargo-llvm-cov"},{"key":"compile","label":"Compilation time","description":"cargo build / cargo check wall-clock time"}],"allow_free_response":true}"#)
        // Then: ask about the measure command
        .init_response(r#"{"type":"question","text":"Since there are no existing measures in the project, we need to set up a measure command.\n\nHow should we measure the target metric?","options":[{"key":"bench","label":"cargo bench","description":"add a Criterion or built-in bench harness to the project"},{"key":"custom","label":"Custom command","description":"run a shell command that prints the metric to stdout"},{"key":"script","label":"External script","description":"use a Python/shell script that extracts metrics from command output"}],"allow_free_response":true}"#)
        // Propose config sections based on "answers"
        .init_response(r#"{"type":"config","section":{"type":"task","name":"mock-task","description":"Mock task for testing","max_iterations":"5","canonical_branch":"main"}}"#)
        .init_response(r#"{"type":"config","section":{"type":"paths","tunable":["src/**"]}}"#)
        .init_response(r#"{"type":"config","section":{"type":"measure","name":"mock-bench","command":["echo","time: 100.0 us"],"adaptor":{"type":"regex","patterns":[{"name":"time_us","pattern":"time: ([0-9.]+)"}]}}}"#)
        .init_response(r#"{"type":"config","section":{"type":"score","value":{"type":"weighted_sum","primary_metrics":[{"name":"time_us","direction":"Minimize"}]}}}"#)
        .build()
}

fn map_direction_weighted(
    d: autotune_config::Direction,
) -> autotune_score::weighted_sum::Direction {
    match d {
        autotune_config::Direction::Minimize => autotune_score::weighted_sum::Direction::Minimize,
        autotune_config::Direction::Maximize => autotune_score::weighted_sum::Direction::Maximize,
    }
}

fn map_direction_threshold(d: autotune_config::Direction) -> autotune_score::threshold::Direction {
    match d {
        autotune_config::Direction::Minimize => autotune_score::threshold::Direction::Minimize,
        autotune_config::Direction::Maximize => autotune_score::threshold::Direction::Maximize,
    }
}

fn build_scorer(config: &AutotuneConfig) -> Box<dyn ScoreCalculator> {
    match &config.score {
        ScoreConfig::WeightedSum {
            primary_metrics,
            guardrail_metrics,
        } => {
            let primary: Vec<PrimaryMetricDef> = primary_metrics
                .iter()
                .map(|m| PrimaryMetricDef {
                    name: m.name.clone(),
                    direction: map_direction_weighted(m.direction),
                    weight: m.weight,
                })
                .collect();
            let guardrails: Vec<GuardrailMetricDef> = guardrail_metrics
                .iter()
                .map(|m| GuardrailMetricDef {
                    name: m.name.clone(),
                    direction: map_direction_weighted(m.direction),
                    max_regression: m.max_regression,
                })
                .collect();
            Box::new(WeightedSumScorer::new(primary, guardrails))
        }
        ScoreConfig::Threshold { conditions } => {
            let conds: Vec<ThresholdConditionDef> = conditions
                .iter()
                .map(|c| ThresholdConditionDef {
                    metric: c.metric.clone(),
                    direction: map_direction_threshold(c.direction),
                    threshold: c.threshold,
                })
                .collect();
            Box::new(ThresholdScorer::new(conds))
        }
        ScoreConfig::Script { command } | ScoreConfig::Command { command } => {
            Box::new(ScriptScorer::new(command.clone()))
        }
    }
}

fn cmd_run(task_name_override: Option<String>) -> Result<()> {
    let repo_root = find_repo_root()?;
    let mut config = load_config(&repo_root)?;

    // Apply task name override
    if let Some(name) = task_name_override {
        config.task.name = name;
    }

    let task_dir = config.task_dir(&repo_root);
    if task_dir.exists() {
        // If state.json is missing, this is leftover from a failed previous
        // run (crashed before state was persisted). Clean it up and retry.
        if !task_dir.join("state.json").exists() {
            println!(
                "[autotune] removing incomplete task state at {}",
                task_dir.display()
            );
            std::fs::remove_dir_all(&task_dir)
                .context("failed to remove incomplete task directory")?;
        } else {
            bail!(
                "task '{}' already exists at {}. Use 'resume' to continue it.",
                config.task.name,
                task_dir.display()
            );
        }
    }

    let store = TaskStore::new(&task_dir).context("failed to create task store")?;

    // Snapshot config
    let config_content = std::fs::read_to_string(repo_root.join(".autotune.toml"))
        .context("failed to read config")?;
    store
        .save_config_snapshot(&config_content)
        .context("failed to save config snapshot")?;

    let agent = build_agent(&config);
    let scorer = build_scorer(&config);

    // Run sanity tests
    if !config.test.is_empty() {
        println!("[autotune] running sanity tests...");
        let test_results = autotune_test::run_all_tests(&config.test, &repo_root)
            .context("sanity tests failed to execute")?;
        if !autotune_test::all_passed(&test_results) {
            let failed: Vec<_> = test_results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| r.name.as_str())
                .collect();
            bail!("sanity tests failed: {}", failed.join(", "));
        }
        println!("[autotune] sanity tests passed");
    }

    // Take baseline measurements
    println!("[autotune] collecting baseline metrics...");
    let (baseline_metrics, baseline_reports) =
        autotune_benchmark::run_all_measures_with_output(&config.measure, &repo_root)
            .context("baseline measures failed")?;
    println!("[autotune] baseline metrics: {:?}", baseline_metrics);

    // Persist raw baseline stdout/stderr per measure so the research agent
    // can look up detailed reports (e.g. coverage output) on demand.
    for report in &baseline_reports {
        let _ =
            store.save_measure_output(0, "baseline", &report.name, &report.stdout, &report.stderr);
    }

    // Score baseline against itself (rank=0)
    let baseline_record = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: baseline_metrics.clone(),
        rank: 0.0,
        score: None,
        reason: None,
        timestamp: Utc::now(),
    };
    store
        .append_ledger(&baseline_record)
        .context("failed to record baseline")?;

    // Spawn research agent
    println!("[autotune] spawning research agent...");
    let research_prompt = build_research_agent_prompt(&config, &baseline_metrics);

    let research_permissions = autotune_plan::research_agent_permissions();
    let research_config = autotune_agent::AgentConfig {
        prompt: research_prompt,
        allowed_tools: research_permissions,
        working_directory: repo_root.clone(),
        model: config.agent.research.as_ref().and_then(|r| r.model.clone()),
        max_turns: config.agent.research.as_ref().and_then(|r| r.max_turns),
    };

    // Forward streaming events (text, tool use) to stderr.
    let research_handler =
        autotune::stream_ui::make_research_event_handler("exploring codebase...");
    let research_config_with_events = autotune_agent::AgentConfigWithEvents::new(research_config)
        .with_event_handler(research_handler);
    let research_response = agent
        .spawn_streaming(research_config_with_events)
        .context("failed to spawn research agent")?;
    autotune::stream_ui::clear_status();

    // Handle any tool-access requests the agent emitted during initial exploration.
    let tool_approver = autotune::stream_ui::TerminalToolApprover;
    let initial_session = autotune_agent::AgentSession {
        session_id: research_response.session_id.clone(),
        backend: agent.backend_name().to_string(),
    };
    let spawn_handler = autotune::stream_ui::make_research_event_handler("processing...");
    let _research_response = autotune_plan::handle_tool_requests(
        agent.as_ref(),
        &initial_session,
        research_response.clone(),
        Some(&spawn_handler),
        Some(&tool_approver),
    )
    .context("failed to handle research agent tool requests")?;
    autotune::stream_ui::clear_status();
    let research_response = _research_response;

    println!(
        "[autotune] research agent session: {}",
        research_response.session_id
    );

    // Initialize state
    let initial_state = TaskState {
        task_name: config.task.name.clone(),
        canonical_branch: config.task.canonical_branch.clone(),
        research_session_id: research_response.session_id.clone(),
        current_iteration: 1,
        current_phase: Phase::Planning,
        current_approach: None,
    };
    store.save_state(&initial_state)?;

    // Set up Ctrl+C handler
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    ctrlc::set_handler(move || {
        println!("\n[autotune] received Ctrl+C, shutting down gracefully...");
        shutdown_clone.store(true, Ordering::SeqCst);
    })
    .context("failed to set Ctrl+C handler")?;

    // Run state machine
    machine::run_task(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &shutdown,
        Some(&tool_approver),
    )?;

    // Print handover info
    let final_state = store.load_state()?;
    let research_session = autotune_agent::AgentSession {
        session_id: final_state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };
    println!("\n[autotune] task '{}' complete", config.task.name);
    println!(
        "[autotune] research agent handover: {}",
        agent.handover_command(&research_session)
    );
    println!("[autotune] results at: {}", task_dir.display());

    Ok(())
}

fn cmd_resume(
    task_name: String,
    max_iterations: Option<u64>,
    max_duration: Option<String>,
    target_improvement: Option<f64>,
) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");
    let task_dir = autotune_dir.join("tasks").join(&task_name);

    let store = TaskStore::open(&task_dir)
        .with_context(|| format!("task '{}' not found at {}", task_name, task_dir.display()))?;

    // Load frozen config from snapshot
    let config_snapshot = store
        .load_config_snapshot()
        .context("failed to load config snapshot")?;
    let mut config: AutotuneConfig =
        toml::from_str(&config_snapshot).context("failed to parse frozen config")?;

    // Apply transient stop-condition overrides
    if let Some(max) = max_iterations {
        config.task.max_iterations = Some(autotune_config::StopValue::Finite(max));
    }
    if let Some(duration) = max_duration {
        config.task.max_duration = Some(duration);
    }
    if let Some(target) = target_improvement {
        config.task.target_improvement = Some(target);
    }

    let agent = build_agent(&config);
    let scorer = build_scorer(&config);

    // Prepare resume
    let _state = resume::prepare_resume(&store, &repo_root)?;

    // Set up Ctrl+C handler
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    ctrlc::set_handler(move || {
        println!("\n[autotune] received Ctrl+C, shutting down gracefully...");
        shutdown_clone.store(true, Ordering::SeqCst);
    })
    .context("failed to set Ctrl+C handler")?;

    let tool_approver = autotune::stream_ui::TerminalToolApprover;

    // Run state machine
    machine::run_task(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &shutdown,
        Some(&tool_approver),
    )?;

    // Print handover info
    let final_state = store.load_state()?;
    let research_session = autotune_agent::AgentSession {
        session_id: final_state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };
    println!("\n[autotune] task '{}' resumed and complete", task_name);
    println!(
        "[autotune] research agent handover: {}",
        agent.handover_command(&research_session)
    );

    Ok(())
}

fn cmd_report(task_name: Option<String>, format: ReportFormat) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");

    let name = match task_name {
        Some(n) => n,
        None => {
            // Try to load from config
            let config = load_config(&repo_root)?;
            config.task.name
        }
    };

    let task_dir = autotune_dir.join("tasks").join(&name);
    let store = TaskStore::open(&task_dir).with_context(|| format!("task '{}' not found", name))?;

    let ledger = store.load_ledger().context("failed to load ledger")?;
    let state = store.load_state().context("failed to load state")?;

    match format {
        ReportFormat::Json => {
            let report = serde_json::json!({
                "task": name,
                "phase": format!("{}", state.current_phase),
                "iteration": state.current_iteration,
                "ledger": ledger,
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ReportFormat::Table => {
            println!("Task: {}", name);
            println!("Phase: {}", state.current_phase);
            println!("Iteration: {}", state.current_iteration);
            println!();
            println!(
                "{:<6} {:<20} {:<10} {:<10} Reason",
                "Iter", "Approach", "Status", "Rank"
            );
            println!("{}", "-".repeat(70));
            for record in &ledger {
                println!(
                    "{:<6} {:<20} {:<10} {:<10.4} {}",
                    record.iteration,
                    truncate(&record.approach, 18),
                    format!("{:?}", record.status),
                    record.rank,
                    record.reason.as_deref().unwrap_or("")
                );
            }
        }
    }

    Ok(())
}

fn cmd_list() -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");

    let tasks = TaskStore::list_tasks(&autotune_dir).context("failed to list tasks")?;

    if tasks.is_empty() {
        println!("No tasks found.");
        return Ok(());
    }

    println!("{:<30} {:<15} {:<6}", "Name", "Phase", "Iter");
    println!("{}", "-".repeat(55));
    for name in &tasks {
        let dir = autotune_dir.join("tasks").join(name);
        let store = TaskStore::open(&dir);
        match store.and_then(|s| s.load_state().map(|st| (s, st))) {
            Ok((_store, state)) => {
                println!(
                    "{:<30} {:<15} {:<6}",
                    name, state.current_phase, state.current_iteration
                );
            }
            Err(_) => {
                println!("{:<30} {:<15} {:<6}", name, "unknown", "-");
            }
        }
    }

    Ok(())
}

fn cmd_init(name_override: Option<String>) -> Result<()> {
    let repo_root = find_repo_root()?;
    let config_path = repo_root.join(".autotune.toml");

    let mut config = if config_path.exists() {
        load_config(&repo_root)?
    } else {
        // Agent-assisted init
        println!("[autotune] no .autotune.toml found — starting agent-assisted init");

        let global_config = GlobalConfig::load().context("failed to load global config")?;

        let agent = build_agent_from_global(&global_config);

        // Build a validator that trial-runs tasks so the agent can fix bad config
        let validator_root = repo_root.clone();
        let validator =
            move |config: &AutotuneConfig| -> Result<std::collections::HashMap<String, f64>, String> {
                validate_measure_config(&config.measure, &validator_root)
            };

        let terminal_input = autotune_init::TerminalInput;
        let result = match autotune_init::run_init(
            &*agent,
            &global_config,
            &repo_root,
            &terminal_input,
            Some(&validator),
        ) {
            Ok(result) => result,
            Err(autotune_init::InitError::UserAborted) => {
                println!("\n[autotune] init cancelled");
                return Ok(());
            }
            Err(e) => return Err(e).context("agent-assisted init failed"),
        };

        // Write .autotune.toml
        let toml_content =
            toml::to_string_pretty(&result.config).context("failed to serialize config")?;
        std::fs::write(&config_path, &toml_content).context("failed to write .autotune.toml")?;
        println!("[autotune] wrote .autotune.toml");

        result.config
    };

    if let Some(name) = name_override {
        config.task.name = name;
    }

    println!();
    println!("[autotune] task '{}' configured", config.task.name);
    println!("[autotune] run `autotune run` to start the tune loop");

    Ok(())
}

/// Run each measure command and try metric extraction, returning detailed
/// errors (including the actual command output) so the init agent can fix the config.
/// Build the initial prompt for the research agent at task spawn time.
///
/// Front-loads everything the agent needs so it doesn't re-explore setup or
/// re-run the measure command:
/// - Task goal and stop criteria
/// - Which files it can propose changes to (tunable/denied)
/// - Which test and measure commands the CLI will run (agent does NOT run them)
/// - How metrics are extracted and scored
/// - Baseline metric values already collected
fn build_research_agent_prompt(
    config: &autotune_config::AutotuneConfig,
    baseline_metrics: &std::collections::HashMap<String, f64>,
) -> String {
    use std::fmt::Write as _;

    let mut p = String::new();
    p.push_str("You are the research agent for the autotune performance-tuning system.\n\n");
    p.push_str("The CLI drives the tune loop. Your job, each iteration, is to analyze the codebase and propose ONE concrete approach (a hypothesis + files to modify). The CLI handles running tests, running measures, scoring, and integrating changes — do not run them yourself.\n\n");

    p.push_str("# Task\n\n");
    writeln!(p, "- Name: {}", config.task.name).ok();
    if let Some(desc) = &config.task.description {
        writeln!(p, "- Description: {desc}").ok();
    }
    writeln!(p, "- Canonical branch: {}", config.task.canonical_branch).ok();

    p.push_str("\n# Stop criteria\n\n");
    let mut any_stop = false;
    if let Some(ref max_iter) = config.task.max_iterations {
        match max_iter {
            autotune_config::StopValue::Finite(n) => {
                writeln!(p, "- max_iterations: {n}").ok();
            }
            autotune_config::StopValue::Infinite => {
                writeln!(p, "- max_iterations: inf (no hard cap)").ok();
            }
        }
        any_stop = true;
    }
    if let Some(t) = config.task.target_improvement {
        writeln!(
            p,
            "- target_improvement: rank >= {t} (relative improvement over baseline)"
        )
        .ok();
        any_stop = true;
    }
    if let Some(ref d) = config.task.max_duration {
        writeln!(p, "- max_duration: {d}").ok();
        any_stop = true;
    }
    for tm in &config.task.target_metric {
        let op = match tm.direction {
            autotune_config::Direction::Maximize => ">=",
            autotune_config::Direction::Minimize => "<=",
        };
        writeln!(p, "- target_metric: {} {} {}", tm.name, op, tm.value).ok();
        any_stop = true;
    }
    if !any_stop {
        p.push_str("- (none configured)\n");
    }

    p.push_str("\n# Paths you may propose changes to\n\n");
    p.push_str("Tunable globs (you may propose edits to files matching these):\n");
    for g in &config.paths.tunable {
        writeln!(p, "- {g}").ok();
    }
    if !config.paths.denied.is_empty() {
        p.push_str("\nDenied globs (do NOT read or propose changes to):\n");
        for g in &config.paths.denied {
            writeln!(p, "- {g}").ok();
        }
    }

    if !config.test.is_empty() {
        p.push_str("\n# Test suites run by the CLI after each approach\n\n");
        p.push_str("(The implementation agent must not modify test files. Tests must still pass after changes.)\n\n");
        for t in &config.test {
            writeln!(p, "- {}: `{}`", t.name, t.command.join(" ")).ok();
        }
    }

    p.push_str("\n# Measures run by the CLI to score each approach\n\n");
    p.push_str("(The CLI runs these, NOT you. Do not try to re-run them.)\n\n");
    for m in &config.measure {
        writeln!(p, "- {}: `{}`", m.name, m.command.join(" ")).ok();
        match &m.adaptor {
            autotune_config::AdaptorConfig::Regex { patterns } => {
                for pat in patterns {
                    writeln!(p, "  - extracts `{}` via regex: {}", pat.name, pat.pattern).ok();
                }
            }
            autotune_config::AdaptorConfig::Criterion { measure_name } => {
                writeln!(p, "  - extracts criterion metrics from `{measure_name}`").ok();
            }
            autotune_config::AdaptorConfig::Script { command } => {
                writeln!(
                    p,
                    "  - extracts metrics via script: `{}`",
                    command.join(" ")
                )
                .ok();
            }
        }
    }

    p.push_str("\n# Scoring\n\n");
    match &config.score {
        autotune_config::ScoreConfig::WeightedSum {
            primary_metrics,
            guardrail_metrics,
        } => {
            p.push_str("Score is a weighted sum of primary metrics (relative to baseline):\n");
            for m in primary_metrics {
                writeln!(p, "- {} ({:?}, weight={})", m.name, m.direction, m.weight).ok();
            }
            if !guardrail_metrics.is_empty() {
                p.push_str("\nGuardrails (an approach is discarded if any guardrail regresses past its limit):\n");
                for g in guardrail_metrics {
                    writeln!(
                        p,
                        "- {} ({:?}, max_regression={})",
                        g.name, g.direction, g.max_regression
                    )
                    .ok();
                }
            }
        }
        autotune_config::ScoreConfig::Threshold { conditions } => {
            p.push_str("Score uses thresholds:\n");
            for c in conditions {
                writeln!(p, "- {} {:?} {}", c.metric, c.direction, c.threshold).ok();
            }
        }
        autotune_config::ScoreConfig::Script { command }
        | autotune_config::ScoreConfig::Command { command } => {
            writeln!(p, "Score computed via: `{}`", command.join(" ")).ok();
        }
    }

    p.push_str("\n# Baseline metrics (already collected)\n\n");
    if baseline_metrics.is_empty() {
        p.push_str("(no baseline metrics were extracted)\n");
    } else {
        let mut keys: Vec<&String> = baseline_metrics.keys().collect();
        keys.sort();
        for k in keys {
            writeln!(p, "- {}: {}", k, baseline_metrics[k]).ok();
        }
    }

    p.push_str("\n# What to do\n\n");
    p.push_str(
        "- Do NOT run the measure, test, or build commands listed above. The CLI owns that.\n",
    );
    p.push_str("- Do NOT re-collect the baseline — it's already done.\n");
    p.push_str("- Use Read/Glob/Grep to understand the code that produces the target metric(s).\n");
    p.push_str("- When the CLI asks you to plan the next iteration, propose a concrete, scoped hypothesis with specific files to modify.\n");
    p.push_str("- Your planning response format is JSON: `{\"approach\": \"...\", \"hypothesis\": \"...\", \"files_to_modify\": [\"...\"]}`. The CLI will tell you when to emit one.\n");
    p.push_str("- The `hypothesis` string is the main prompt passed to the implementation agent, along with the `files_to_modify` list. Write it as concrete instructions: what to change and why. Anything you want the implementer to know must go there.\n");

    p.push_str("\n# Requesting additional tools\n\n");
    p.push_str("You start with read-only tools (Read, Glob, Grep). If you need a tool that isn't available — for example `Bash` to run `cargo tree`, `cargo metadata`, or `git log` — you can request it by emitting an XML fragment in your response:\n\n");
    p.push_str("```xml\n");
    p.push_str("<request-tool>\n");
    p.push_str("  <tool>Bash</tool>\n");
    p.push_str("  <scope>cargo tree:*</scope>\n");
    p.push_str("  <reason>need the dependency graph to identify heavy crates</reason>\n");
    p.push_str("</request-tool>\n");
    p.push_str("```\n\n");
    p.push_str("- `<tool>`: the tool name (e.g., `Bash`, `WebFetch`). `Edit`, `Write`, and `Agent` are hard-denied for the research role and will be rejected.\n");
    p.push_str("- `<scope>`: optional scope string (e.g., `cargo tree:*` to narrow Bash). Always prefer the narrowest scope that meets your need; the user is more likely to approve.\n");
    p.push_str("- `<reason>`: required — one sentence the user reads to decide. Be specific.\n\n");
    p.push_str("You may emit multiple `<request-tool>` fragments in one response. The CLI will prompt the user for each and reply with a summary of what was granted/denied. Once granted, the tool is available for the rest of this task run. If denied, do NOT re-request the same tool — find another path.\n");
    p.push_str("When you emit tool requests, do NOT also emit a hypothesis in the same response — wait for approval first, then produce the hypothesis with the new tools.\n");

    p
}

fn validate_measure_config(
    measures: &[autotune_config::MeasureConfig],
    working_dir: &Path,
) -> Result<std::collections::HashMap<String, f64>, String> {
    use autotune_benchmark::MeasureOutput;
    use std::process::{Command, Stdio};

    let mut all_metrics = std::collections::HashMap::new();

    for measure in measures {
        let program = measure
            .command
            .first()
            .ok_or_else(|| format!("measure '{}' has empty command", measure.name))?;
        let args = &measure.command[1..];

        let output = Command::new(program)
            .args(args)
            .current_dir(working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to run measure '{}': {}", measure.name, e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(format!(
                "measure '{}' command failed (exit code {})\n\nstdout:\n{}\n\nstderr:\n{}",
                measure.name,
                output.status.code().unwrap_or(-1),
                stdout,
                stderr,
            ));
        }

        let measure_output = MeasureOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        };

        let adaptor = autotune_benchmark::build_adaptor(&measure.adaptor, working_dir);
        let metrics = adaptor.extract(&measure_output).map_err(|e| {
            format!(
                "metric extraction failed for measure '{}': {}\n\nMeasure command output (stdout):\n{}\n\nMeasure command output (stderr):\n{}",
                measure.name, e, stdout, stderr,
            )
        })?;

        all_metrics.extend(metrics);
    }

    Ok(all_metrics)
}

fn cmd_config(sub: ConfigCommands) -> Result<()> {
    match sub {
        ConfigCommands::Get { key } => {
            let config = GlobalConfig::load().context("failed to load global config")?;
            match get_config_value(&config, &key) {
                Some(value) => println!("{}", value),
                None => bail!("key '{}' is not set", key),
            }
        }
        ConfigCommands::Set { key, value } => {
            let path = GlobalConfig::user_config_path()
                .context("could not determine user config directory")?;
            let mut doc = load_or_create_toml_doc(&path)?;
            set_toml_value(&mut doc, &key, &value)?;
            write_toml_doc(&path, &doc)?;
            println!("{} = {}", key, value);
        }
        ConfigCommands::Unset { key } => {
            let path = GlobalConfig::user_config_path()
                .context("could not determine user config directory")?;
            if !path.exists() {
                bail!("no user config file exists");
            }
            let mut doc = load_or_create_toml_doc(&path)?;
            unset_toml_value(&mut doc, &key)?;
            write_toml_doc(&path, &doc)?;
            println!("unset {}", key);
        }
        ConfigCommands::List => {
            let config = GlobalConfig::load().context("failed to load global config")?;
            if let Some(p) = GlobalConfig::user_config_path() {
                println!("# {}", p.display());
                println!();
            }
            print_config(&config);
        }
        ConfigCommands::Edit => {
            let path = GlobalConfig::user_config_path()
                .context("could not determine user config directory")?;
            // Ensure parent dir and file exist
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).context("failed to create config directory")?;
            }
            if !path.exists() {
                std::fs::write(&path, CONFIG_TEMPLATE).context("failed to create config file")?;
            }
            let editor = std::env::var("EDITOR").context(
                "$EDITOR is not set. Set it to your preferred editor (e.g. export EDITOR=vim)",
            )?;
            let status = std::process::Command::new(&editor)
                .arg(&path)
                .status()
                .with_context(|| format!("failed to launch editor '{}'", editor))?;
            if !status.success() {
                bail!("editor exited with {}", status);
            }
        }
    }
    Ok(())
}

const CONFIG_TEMPLATE: &str = r#"# Autotune global config
# Default agent settings used across all tasks.
# Uncomment and edit the values you want to set.

# [agent]
# backend = "claude"            # LLM backend (currently only "claude")

# # Research agent: persistent session that proposes optimization hypotheses.
# [agent.research]
# model = "opus"                # LLM model to use
# max_turns = 200               # Max agent tool-use turns per session

# # Implementation agent: ephemeral session that writes code in a worktree.
# [agent.implementation]
# model = "sonnet"
# max_turns = 50

# # Init agent: one-shot session that helps write .autotune.toml.
# [agent.init]
# model = "opus"
# max_turns = 200
"#;

/// Valid config keys and their dotted paths.
const VALID_KEYS: &[&str] = &[
    "agent.backend",
    "agent.research.model",
    "agent.research.max_turns",
    "agent.research.backend",
    "agent.implementation.model",
    "agent.implementation.max_turns",
    "agent.implementation.backend",
    "agent.init.model",
    "agent.init.max_turns",
    "agent.init.backend",
];

fn validate_key(key: &str) -> Result<()> {
    if VALID_KEYS.contains(&key) {
        Ok(())
    } else {
        bail!(
            "unknown config key '{}'. Valid keys:\n  {}",
            key,
            VALID_KEYS.join("\n  ")
        )
    }
}

fn get_config_value(config: &GlobalConfig, key: &str) -> Option<String> {
    let agent = config.agent.as_ref()?;
    match key {
        "agent.backend" => Some(agent.backend.clone()),
        "agent.research.model" => agent.research.as_ref()?.model.clone(),
        "agent.research.max_turns" => agent.research.as_ref()?.max_turns.map(|v| v.to_string()),
        "agent.research.backend" => agent.research.as_ref()?.backend.clone(),
        "agent.implementation.model" => agent.implementation.as_ref()?.model.clone(),
        "agent.implementation.max_turns" => agent
            .implementation
            .as_ref()?
            .max_turns
            .map(|v| v.to_string()),
        "agent.implementation.backend" => agent.implementation.as_ref()?.backend.clone(),
        "agent.init.model" => agent.init.as_ref()?.model.clone(),
        "agent.init.max_turns" => agent.init.as_ref()?.max_turns.map(|v| v.to_string()),
        "agent.init.backend" => agent.init.as_ref()?.backend.clone(),
        _ => None,
    }
}

fn print_config(config: &GlobalConfig) {
    let agent = match &config.agent {
        Some(a) => a,
        None => {
            println!("(no config set)");
            return;
        }
    };

    println!("agent.backend = {}", agent.backend);

    for (name, role) in [
        ("research", &agent.research),
        ("implementation", &agent.implementation),
        ("init", &agent.init),
    ] {
        if let Some(r) = role {
            if let Some(ref b) = r.backend {
                println!("agent.{}.backend = {}", name, b);
            }
            if let Some(ref m) = r.model {
                println!("agent.{}.model = {}", name, m);
            }
            if let Some(t) = r.max_turns {
                println!("agent.{}.max_turns = {}", name, t);
            }
        }
    }
}

fn load_or_create_toml_doc(path: &Path) -> Result<toml_edit::DocumentMut> {
    if path.exists() {
        let content = std::fs::read_to_string(path).context("failed to read config file")?;
        content
            .parse::<toml_edit::DocumentMut>()
            .context("failed to parse config file")
    } else {
        Ok(toml_edit::DocumentMut::new())
    }
}

fn write_toml_doc(path: &Path, doc: &toml_edit::DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create config directory")?;
    }
    std::fs::write(path, doc.to_string()).context("failed to write config file")
}

fn set_toml_value(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<()> {
    validate_key(key)?;

    let parts: Vec<&str> = key.split('.').collect();

    // Navigate/create intermediate tables
    let mut table = doc.as_table_mut();
    for &part in &parts[..parts.len() - 1] {
        if !table.contains_key(part) {
            table.insert(part, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        table = table[part]
            .as_table_mut()
            .with_context(|| format!("'{}' is not a table in config", part))?;
    }

    let leaf = parts[parts.len() - 1];

    // Parse value: try integer first, then string
    let toml_value = if key.ends_with("max_turns") {
        let n: u64 = value
            .parse()
            .with_context(|| format!("'{}' must be an integer", key))?;
        toml_edit::value(n as i64)
    } else {
        toml_edit::value(value)
    };

    table.insert(leaf, toml_value);
    Ok(())
}

fn unset_toml_value(doc: &mut toml_edit::DocumentMut, key: &str) -> Result<()> {
    validate_key(key)?;

    let parts: Vec<&str> = key.split('.').collect();

    let mut table = doc.as_table_mut();
    for &part in &parts[..parts.len() - 1] {
        match table.get_mut(part) {
            Some(item) => {
                table = item
                    .as_table_mut()
                    .with_context(|| format!("'{}' is not a table in config", part))?;
            }
            None => bail!("key '{}' is not set", key),
        }
    }

    let leaf = parts[parts.len() - 1];
    if table.remove(leaf).is_none() {
        bail!("key '{}' is not set", key);
    }

    Ok(())
}

fn cmd_step(task_name: String, expected_phase: Phase) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");
    let task_dir = autotune_dir.join("tasks").join(&task_name);

    let store = TaskStore::open(&task_dir)
        .with_context(|| format!("task '{}' not found at {}", task_name, task_dir.display()))?;

    // Load frozen config from snapshot
    let config_snapshot = store
        .load_config_snapshot()
        .context("failed to load config snapshot")?;
    let config: AutotuneConfig =
        toml::from_str(&config_snapshot).context("failed to parse frozen config")?;

    let mut state = store.load_state().context("failed to load task state")?;

    // Validate phase
    if state.current_phase != expected_phase {
        bail!(
            "task '{}' is in phase {}, but this command requires phase {}",
            task_name,
            state.current_phase,
            expected_phase,
        );
    }

    let agent = build_agent(&config);
    let scorer = build_scorer(&config);

    let tool_approver = autotune::stream_ui::TerminalToolApprover;
    machine::run_single_phase(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &mut state,
        Some(&tool_approver),
    )?;

    println!(
        "[autotune] step complete — task '{}' is now in phase {}",
        task_name, state.current_phase
    );

    Ok(())
}

fn cmd_export(task_name: String, output_path: String) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");
    let task_dir = autotune_dir.join("tasks").join(&task_name);

    let store = TaskStore::open(&task_dir)
        .with_context(|| format!("task '{}' not found at {}", task_name, task_dir.display()))?;

    let state = store.load_state().context("failed to load state")?;
    let ledger = store.load_ledger().context("failed to load ledger")?;
    let log = store.read_log().unwrap_or_default();

    // Load raw config snapshot as a string
    let config_toml = store.load_config_snapshot().unwrap_or_default();

    let export = serde_json::json!({
        "task_name": task_name,
        "config": config_toml,
        "ledger": ledger,
        "log": log,
        "state": state,
    });

    let json = serde_json::to_string_pretty(&export).context("failed to serialize export")?;
    std::fs::write(&output_path, &json)
        .with_context(|| format!("failed to write export to {}", output_path))?;

    println!(
        "[autotune] exported task '{}' to {}",
        task_name, output_path
    );

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}
