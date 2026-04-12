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
use autotune_state::{ExperimentState, ExperimentStore, IterationRecord, IterationStatus, Phase};

use cli::{Cli, Commands, ReportFormat};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { experiment } => cmd_run(experiment),
        Commands::Resume {
            experiment,
            max_iterations,
            max_duration,
            target_improvement,
        } => cmd_resume(experiment, max_iterations, max_duration, target_improvement),
        Commands::Report { experiment, format } => cmd_report(experiment, format),
        Commands::List => cmd_list(),
        Commands::Init { name } => cmd_init(name),
        Commands::Plan { experiment } => cmd_step(experiment, Phase::Planning),
        Commands::Implement { experiment } => cmd_step(experiment, Phase::Implementing),
        Commands::Test { experiment } => cmd_step(experiment, Phase::Testing),
        Commands::Benchmark { experiment } => cmd_step(experiment, Phase::Benchmarking),
        Commands::Record { experiment } => cmd_step(experiment, Phase::Scoring),
        Commands::Apply { experiment } => cmd_step(experiment, Phase::Integrating),
        Commands::Export { experiment, output } => cmd_export(experiment, output),
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
    // Currently only the Claude backend is supported.
    Box::new(ClaudeAgent::new())
}

fn build_agent_from_global(_global_config: &GlobalConfig) -> Box<dyn Agent> {
    // Currently only the Claude backend is supported.
    // In the future, read global_config.agent.backend to select backend.
    Box::new(ClaudeAgent::new())
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

fn cmd_run(experiment_name_override: Option<String>) -> Result<()> {
    let repo_root = find_repo_root()?;
    let mut config = load_config(&repo_root)?;

    // Apply experiment name override
    if let Some(name) = experiment_name_override {
        config.experiment.name = name;
    }

    let experiment_dir = config.experiment_dir(&repo_root);
    if experiment_dir.exists() {
        bail!(
            "experiment '{}' already exists at {}. Use 'resume' to continue it.",
            config.experiment.name,
            experiment_dir.display()
        );
    }

    let store =
        ExperimentStore::new(&experiment_dir).context("failed to create experiment store")?;

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

    // Take baseline benchmarks
    println!("[autotune] running baseline benchmarks...");
    let baseline_metrics = autotune_benchmark::run_all_benchmarks(&config.benchmark, &repo_root)
        .context("baseline benchmarks failed")?;
    println!("[autotune] baseline metrics: {:?}", baseline_metrics);

    // Score baseline against itself (rank=0)
    let baseline_record = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: baseline_metrics,
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
    let description = config
        .experiment
        .description
        .as_deref()
        .unwrap_or(&config.experiment.name);
    let research_prompt = format!(
        "You are a research agent for the autotune performance tuning system.\n\
         Experiment: {}\n\
         Description: {}\n\
         You will be asked to analyze code and propose optimization approaches.",
        config.experiment.name, description
    );

    let research_permissions = autotune_plan::research_agent_permissions();
    let research_config = autotune_agent::AgentConfig {
        prompt: research_prompt,
        allowed_tools: research_permissions,
        working_directory: repo_root.clone(),
        model: config.agent.research.as_ref().and_then(|r| r.model.clone()),
        max_turns: config.agent.research.as_ref().and_then(|r| r.max_turns),
    };

    let research_response = agent
        .spawn(&research_config)
        .context("failed to spawn research agent")?;

    println!(
        "[autotune] research agent session: {}",
        research_response.session_id
    );

    // Initialize state
    let initial_state = ExperimentState {
        experiment_name: config.experiment.name.clone(),
        canonical_branch: config.experiment.canonical_branch.clone(),
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
    machine::run_experiment(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &shutdown,
    )?;

    // Print handover info
    let final_state = store.load_state()?;
    let research_session = autotune_agent::AgentSession {
        session_id: final_state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };
    println!(
        "\n[autotune] experiment '{}' complete",
        config.experiment.name
    );
    println!(
        "[autotune] research agent handover: {}",
        agent.handover_command(&research_session)
    );
    println!("[autotune] results at: {}", experiment_dir.display());

    Ok(())
}

fn cmd_resume(
    experiment_name: String,
    max_iterations: Option<u64>,
    max_duration: Option<String>,
    target_improvement: Option<f64>,
) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");
    let experiment_dir = autotune_dir.join("experiments").join(&experiment_name);

    let store = ExperimentStore::open(&experiment_dir).with_context(|| {
        format!(
            "experiment '{}' not found at {}",
            experiment_name,
            experiment_dir.display()
        )
    })?;

    // Load frozen config from snapshot
    let config_snapshot = store
        .load_config_snapshot()
        .context("failed to load config snapshot")?;
    let mut config: AutotuneConfig =
        toml::from_str(&config_snapshot).context("failed to parse frozen config")?;

    // Apply transient stop-condition overrides
    if let Some(max) = max_iterations {
        config.experiment.max_iterations = Some(autotune_config::StopValue::Finite(max));
    }
    if let Some(duration) = max_duration {
        config.experiment.max_duration = Some(duration);
    }
    if let Some(target) = target_improvement {
        config.experiment.target_improvement = Some(target);
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

    // Run state machine
    machine::run_experiment(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &shutdown,
    )?;

    // Print handover info
    let final_state = store.load_state()?;
    let research_session = autotune_agent::AgentSession {
        session_id: final_state.research_session_id.clone(),
        backend: agent.backend_name().to_string(),
    };
    println!(
        "\n[autotune] experiment '{}' resumed and complete",
        experiment_name
    );
    println!(
        "[autotune] research agent handover: {}",
        agent.handover_command(&research_session)
    );

    Ok(())
}

fn cmd_report(experiment_name: Option<String>, format: ReportFormat) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");

    let name = match experiment_name {
        Some(n) => n,
        None => {
            // Try to load from config
            let config = load_config(&repo_root)?;
            config.experiment.name
        }
    };

    let experiment_dir = autotune_dir.join("experiments").join(&name);
    let store = ExperimentStore::open(&experiment_dir)
        .with_context(|| format!("experiment '{}' not found", name))?;

    let ledger = store.load_ledger().context("failed to load ledger")?;
    let state = store.load_state().context("failed to load state")?;

    match format {
        ReportFormat::Json => {
            let report = serde_json::json!({
                "experiment": name,
                "phase": format!("{}", state.current_phase),
                "iteration": state.current_iteration,
                "ledger": ledger,
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ReportFormat::Table => {
            println!("Experiment: {}", name);
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

    let experiments =
        ExperimentStore::list_experiments(&autotune_dir).context("failed to list experiments")?;

    if experiments.is_empty() {
        println!("No experiments found.");
        return Ok(());
    }

    println!("{:<30} {:<15} {:<6}", "Name", "Phase", "Iter");
    println!("{}", "-".repeat(55));
    for name in &experiments {
        let dir = autotune_dir.join("experiments").join(name);
        let store = ExperimentStore::open(&dir);
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

        let config = autotune_init::run_init(&*agent, &global_config, &repo_root, || {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            Ok(input.trim().to_string())
        })
        .context("agent-assisted init failed")?;

        // Write .autotune.toml
        let toml_content = toml::to_string_pretty(&config).context("failed to serialize config")?;
        std::fs::write(&config_path, &toml_content).context("failed to write .autotune.toml")?;
        println!("[autotune] wrote .autotune.toml");

        config
    };

    if let Some(name) = name_override {
        config.experiment.name = name;
    }

    let experiment_dir = config.experiment_dir(&repo_root);
    if experiment_dir.exists() {
        bail!(
            "experiment '{}' already exists at {}. Use 'resume' to continue it.",
            config.experiment.name,
            experiment_dir.display()
        );
    }

    let store =
        ExperimentStore::new(&experiment_dir).context("failed to create experiment store")?;

    // Snapshot config
    let config_content = std::fs::read_to_string(&config_path).context("failed to read config")?;
    store
        .save_config_snapshot(&config_content)
        .context("failed to save config snapshot")?;

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

    // Take baseline benchmarks
    println!("[autotune] running baseline benchmarks...");
    let baseline_metrics = autotune_benchmark::run_all_benchmarks(&config.benchmark, &repo_root)
        .context("baseline benchmarks failed")?;
    println!("[autotune] baseline metrics: {:?}", baseline_metrics);

    // Record baseline in ledger
    let baseline_record = IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: baseline_metrics,
        rank: 0.0,
        score: None,
        reason: None,
        timestamp: Utc::now(),
    };
    store
        .append_ledger(&baseline_record)
        .context("failed to record baseline")?;

    println!();
    println!(
        "[autotune] experiment '{}' initialized",
        config.experiment.name
    );
    println!("[autotune] results at: {}", experiment_dir.display());
    println!("[autotune] run `autotune run` to start the tune loop or use step commands");

    Ok(())
}

fn cmd_step(experiment_name: String, expected_phase: Phase) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");
    let experiment_dir = autotune_dir.join("experiments").join(&experiment_name);

    let store = ExperimentStore::open(&experiment_dir).with_context(|| {
        format!(
            "experiment '{}' not found at {}",
            experiment_name,
            experiment_dir.display()
        )
    })?;

    // Load frozen config from snapshot
    let config_snapshot = store
        .load_config_snapshot()
        .context("failed to load config snapshot")?;
    let config: AutotuneConfig =
        toml::from_str(&config_snapshot).context("failed to parse frozen config")?;

    let mut state = store
        .load_state()
        .context("failed to load experiment state")?;

    // Validate phase
    if state.current_phase != expected_phase {
        bail!(
            "experiment '{}' is in phase {}, but this command requires phase {}",
            experiment_name,
            state.current_phase,
            expected_phase,
        );
    }

    let agent = build_agent(&config);
    let scorer = build_scorer(&config);

    machine::run_single_phase(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &mut state,
    )?;

    println!(
        "[autotune] step complete — experiment '{}' is now in phase {}",
        experiment_name, state.current_phase
    );

    Ok(())
}

fn cmd_export(experiment_name: String, output_path: String) -> Result<()> {
    let repo_root = find_repo_root()?;
    let autotune_dir = repo_root.join(".autotune");
    let experiment_dir = autotune_dir.join("experiments").join(&experiment_name);

    let store = ExperimentStore::open(&experiment_dir).with_context(|| {
        format!(
            "experiment '{}' not found at {}",
            experiment_name,
            experiment_dir.display()
        )
    })?;

    let state = store.load_state().context("failed to load state")?;
    let ledger = store.load_ledger().context("failed to load ledger")?;
    let log = store.read_log().unwrap_or_default();

    // Load raw config snapshot as a string
    let config_toml = store.load_config_snapshot().unwrap_or_default();

    let export = serde_json::json!({
        "experiment_name": experiment_name,
        "config": config_toml,
        "ledger": ledger,
        "log": log,
        "state": state,
    });

    let json = serde_json::to_string_pretty(&export).context("failed to serialize export")?;
    std::fs::write(&output_path, &json)
        .with_context(|| format!("failed to write export to {}", output_path))?;

    println!(
        "[autotune] exported experiment '{}' to {}",
        experiment_name, output_path
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
