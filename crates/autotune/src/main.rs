mod cli;

use autotune::agent_factory::{AgentRole, build_agent_for_backend, resolve_backend_name};
use autotune::machine;
use autotune::resume;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;

use autotune_agent::Agent;
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

fn global_user_config_path() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(path) = test_support::global_user_config_path_override() {
        return Ok(path);
    }

    GlobalConfig::user_config_path().context("could not determine user config directory")
}

fn load_global_config() -> Result<GlobalConfig> {
    #[cfg(test)]
    if let Some(path) = test_support::global_user_config_path_override() {
        return GlobalConfig::load_from(&path).context("failed to load global config");
    }

    GlobalConfig::load().context("failed to load global config")
}

fn configured_editor() -> Result<String> {
    #[cfg(test)]
    if let Some(editor) = test_support::editor_override() {
        return Ok(editor);
    }

    std::env::var("EDITOR")
        .context("$EDITOR is not set. Set it to your preferred editor (e.g. export EDITOR=vim)")
}

fn codex_reasoning_effort(effort: Option<autotune_config::ReasoningEffort>) -> Option<String> {
    effort
        .map(|effort| match effort {
            autotune_config::ReasoningEffort::Low => "low",
            autotune_config::ReasoningEffort::Medium => "medium",
            autotune_config::ReasoningEffort::High => "high",
        })
        .map(str::to_string)
}

/// Find the next available task name by appending `-2`, `-3`, ... to the base
/// name. A name is "available" when both its task directory and its advancing
/// git branch (`autotune/<name>-main`) don't exist yet. The `-main` suffix
/// keeps the advancing branch out of the `autotune/<task>/<slug>` worktree
/// namespace — git refuses to create a branch when another branch occupies
/// a prefix path.
fn next_available_task_name(repo_root: &Path, base: &str) -> Result<String> {
    let tasks_dir = repo_root.join(".autotune").join("tasks");
    for n in 2..10_000 {
        let candidate = format!("{base}-{n}");
        let dir_taken = tasks_dir.join(&candidate).exists();
        let branch_taken =
            autotune_git::branch_exists(repo_root, &format!("autotune/{candidate}-main"))
                .unwrap_or(false);
        if !dir_taken && !branch_taken {
            return Ok(candidate);
        }
    }
    bail!("could not find an available fork name for task '{base}' after 10000 attempts");
}

fn prepare_run_task_dir(repo_root: &Path, config: &mut AutotuneConfig) -> Result<PathBuf> {
    let mut task_dir = config.task_dir(repo_root);
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
            // Task already exists — auto-fork by appending a numeric suffix so
            // each `run` invocation starts fresh. Users who want to continue
            // the existing task should use `resume` instead.
            let original_name = config.task.name.clone();
            let forked_name = next_available_task_name(repo_root, &original_name)?;
            println!(
                "[autotune] task '{}' already exists — forking as '{}' (use 'resume' to continue the existing task)",
                original_name, forked_name
            );
            config.task.name = forked_name;
            task_dir = config.task_dir(repo_root);
        }
    }

    Ok(task_dir)
}

/// Fill in missing agent role settings from the global user config.
///
/// Precedence is:
/// global `[agent]` < global `[agent.<role>]` < project `[agent]` <
/// project `[agent.<role>]`.
fn apply_global_agent_defaults(config: &mut AutotuneConfig, global: &GlobalConfig) {
    let Some(global_agent) = &global.agent else {
        return;
    };

    fn agent_defaults(agent: &autotune_config::AgentConfig) -> autotune_config::AgentRoleConfig {
        autotune_config::AgentRoleConfig {
            backend: agent.backend.clone(),
            model: agent.model.clone(),
            max_turns: agent.max_turns,
            reasoning_effort: agent.reasoning_effort,
            max_fix_attempts: agent.max_fix_attempts,
            max_fresh_spawns: agent.max_fresh_spawns,
        }
    }

    fn empty_role() -> autotune_config::AgentRoleConfig {
        autotune_config::AgentRoleConfig {
            backend: None,
            model: None,
            max_turns: None,
            reasoning_effort: None,
            max_fix_attempts: None,
            max_fresh_spawns: None,
        }
    }

    let global_defaults = agent_defaults(global_agent);
    let project_defaults = agent_defaults(&config.agent).overlay(&global_defaults);

    config.agent.backend = project_defaults.backend.clone();
    config.agent.model = project_defaults.model.clone();
    config.agent.max_turns = project_defaults.max_turns;
    config.agent.reasoning_effort = project_defaults.reasoning_effort;
    config.agent.max_fix_attempts = project_defaults.max_fix_attempts;
    config.agent.max_fresh_spawns = project_defaults.max_fresh_spawns;

    fn merge_role(
        project: &mut Option<autotune_config::AgentRoleConfig>,
        global: &Option<autotune_config::AgentRoleConfig>,
        project_defaults: &autotune_config::AgentRoleConfig,
        global_defaults: &autotune_config::AgentRoleConfig,
    ) {
        let global_role = global
            .as_ref()
            .map(|role| role.overlay(global_defaults))
            .unwrap_or_else(|| global_defaults.clone());
        let project_role = project
            .as_ref()
            .cloned()
            .unwrap_or_else(empty_role)
            .overlay(project_defaults);
        *project = Some(project_role.overlay(&global_role));
    }

    merge_role(
        &mut config.agent.research,
        &global_agent.research,
        &project_defaults,
        &global_defaults,
    );
    merge_role(
        &mut config.agent.implementation,
        &global_agent.implementation,
        &project_defaults,
        &global_defaults,
    );
    merge_role(
        &mut config.agent.init,
        &global_agent.init,
        &project_defaults,
        &global_defaults,
    );
    merge_role(
        &mut config.agent.judge,
        &global_agent.judge,
        &project_defaults,
        &global_defaults,
    );
}

fn global_backend_name(global_config: &GlobalConfig, role: AgentRole) -> Option<&str> {
    global_config
        .agent
        .as_ref()
        .and_then(|agent_config| resolve_backend_name(agent_config, role))
}

fn build_agent(config: &AutotuneConfig, role: AgentRole) -> Result<Box<dyn Agent>> {
    #[cfg(feature = "mock")]
    if std::env::var("AUTOTUNE_MOCK").is_ok() {
        eprintln!("[autotune] using mock agent (AUTOTUNE_MOCK is set)");
        let mut builder = autotune_mock::MockAgent::builder();

        // Scenario tests can drive the research agent by pointing
        // `AUTOTUNE_MOCK_RESEARCH_SCRIPT` at a file whose contents are the
        // verbatim response texts for spawn+send calls, separated by a line
        // containing only `---`. This lets tests inject arbitrary XML
        // (`<plan>`, `<request-tool>`, malformed, etc.) to exercise the
        // CLI's parsing + approval logic end-to-end.
        if let Ok(path) = std::env::var("AUTOTUNE_MOCK_RESEARCH_SCRIPT")
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            for entry in content.split("\n---\n") {
                let entry = entry.trim_end_matches('\n');
                if !entry.is_empty() {
                    builder = builder.research_response(entry);
                }
            }
        } else {
            builder = builder.hypothesis(
                "mock-approach",
                "mock hypothesis for testing",
                &["src/lib.rs"],
            );
        }

        // Judge-agent mock: when building a Judge-role mock, load responses
        // from AUTOTUNE_MOCK_JUDGE_SCRIPT. Each `---`-separated entry is a
        // verbatim batch response the mock will return for a judge spawn call.
        if role == AgentRole::Judge {
            let mut judge_builder = autotune_mock::MockAgent::builder();
            if let Ok(path) = std::env::var("AUTOTUNE_MOCK_JUDGE_SCRIPT")
                && let Ok(content) = std::fs::read_to_string(&path)
            {
                for entry in content.split("\n---\n") {
                    let entry = entry.trim_end_matches('\n');
                    if !entry.is_empty() {
                        judge_builder = judge_builder.research_response(entry);
                    }
                }
            }
            return Ok(Box::new(judge_builder.build()));
        }

        // Implementer-script support: each entry is a shell command run by
        // the mock implementer on its next turn (spawn or fix-turn send).
        // Empty entries simulate unproductive turns — they trigger the
        // fresh-respawn path in the fixing state machine.
        if let Ok(path) = std::env::var("AUTOTUNE_MOCK_IMPL_SCRIPT")
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            for entry in content.split("\n---\n") {
                // Preserve empty entries — they're meaningful (unproductive
                // turn). Only strip a single trailing newline so heredocs
                // stay intact.
                let entry = entry.strip_suffix('\n').unwrap_or(entry);
                builder = builder.implementation_script_entry(entry);
            }
        }
        return Ok(Box::new(builder.build()));
    }

    let backend = resolve_backend_name(&config.agent, role);
    build_agent_for_backend(backend.unwrap_or("claude"))
}

fn has_judge_measure(config: &AutotuneConfig) -> bool {
    config
        .measure
        .iter()
        .any(|m| matches!(m.adaptor, autotune_config::AdaptorConfig::Judge { .. }))
}

fn judge_agent_session_config(
    config: &AutotuneConfig,
    repo_root: &Path,
) -> autotune_agent::AgentConfig {
    autotune_agent::AgentConfig {
        prompt: String::new(),
        allowed_tools: vec![],
        working_directory: repo_root.to_path_buf(),
        model: config.agent.judge.as_ref().and_then(|j| j.model.clone()),
        max_turns: Some(1),
        reasoning_effort: None,
    }
}

fn research_agent_session_config(
    config: &AutotuneConfig,
    repo_root: &Path,
) -> autotune_agent::AgentConfig {
    autotune_agent::AgentConfig {
        prompt: String::new(),
        allowed_tools: autotune_plan::research_agent_permissions(),
        working_directory: repo_root.to_path_buf(),
        model: config.agent.research.as_ref().and_then(|r| r.model.clone()),
        max_turns: config.agent.research.as_ref().and_then(|r| r.max_turns),
        reasoning_effort: codex_reasoning_effort(
            config
                .agent
                .research
                .as_ref()
                .and_then(|r| r.reasoning_effort),
        ),
    }
}

fn build_agent_from_global(
    global_config: &GlobalConfig,
    role: AgentRole,
) -> Result<Box<dyn Agent>> {
    #[cfg(feature = "mock")]
    if std::env::var("AUTOTUNE_MOCK").is_ok() {
        eprintln!("[autotune] using mock agent (AUTOTUNE_MOCK is set)");
        return Ok(Box::new(mock_init_agent()));
    }

    let backend = global_backend_name(global_config, role).unwrap_or("claude");
    build_agent_for_backend(backend)
}

enum InitFlowOutcome {
    Config(Box<AutotuneConfig>),
    Cancelled,
}

fn run_agent_assisted_init(repo_root: &Path) -> Result<InitFlowOutcome> {
    #[cfg(test)]
    if let Some(outcome) = test_support::take_init_override() {
        return Ok(outcome);
    }

    let global_config = GlobalConfig::load().context("failed to load global config")?;
    let agent = build_agent_from_global(&global_config, AgentRole::Init)?;

    let validator_root = repo_root.to_path_buf();
    let validator =
        move |config: &AutotuneConfig| -> Result<std::collections::HashMap<String, f64>, String> {
            validate_measure_config(&config.measure, &validator_root)
        };

    let terminal_input = autotune_init::TerminalInput;
    match autotune_init::run_init(
        &*agent,
        &global_config,
        repo_root,
        &terminal_input,
        Some(&validator),
    ) {
        Ok(result) => Ok(InitFlowOutcome::Config(Box::new(result.config))),
        Err(autotune_init::InitError::UserAborted) => Ok(InitFlowOutcome::Cancelled),
        Err(e) => Err(e).context("agent-assisted init failed"),
    }
}

#[cfg(feature = "mock")]
fn mock_init_agent() -> autotune_mock::MockAgent {
    autotune_mock::MockAgent::builder()
        // First: ask what metric to optimize.
        .init_response(
            r#"<question>
  <text>What metric would you like autotune to improve?</text>
  <option><key>perf</key><label>Runtime performance</label><description>execution speed</description></option>
  <option><key>size</key><label>Binary size</label><description>size of the compiled binary</description></option>
  <option><key>coverage</key><label>Test coverage</label><description>line coverage via cargo-llvm-cov</description></option>
  <option><key>compile</key><label>Compilation time</label><description>cargo build wall-clock time</description></option>
  <allow-free-response>true</allow-free-response>
</question>"#,
        )
        // Then: ask about the measure command.
        .init_response(
            r#"<question>
  <text>How should we measure the target metric?</text>
  <option><key>bench</key><label>cargo bench</label><description>add a Criterion harness</description></option>
  <option><key>custom</key><label>Custom command</label><description>shell command that prints the metric</description></option>
  <option><key>script</key><label>External script</label><description>Python/shell script extractor</description></option>
  <allow-free-response>true</allow-free-response>
</question>"#,
        )
        // Propose config sections based on "answers" — all four in one response
        // so the accumulator completes in a single turn.
        .init_response(
            r#"<task>
  <name>mock-task</name>
  <description><![CDATA[Mock task for testing]]></description>
  <canonical-branch>main</canonical-branch>
  <max-iterations>5</max-iterations>
</task>
<paths>
  <tunable>src/**</tunable>
</paths>
<measure>
  <name>mock-bench</name>
  <command><segment>echo</segment><segment>time: 100.0 us</segment></command>
  <adaptor>
    <type>regex</type>
    <pattern>
      <name>time_us</name>
      <regex><![CDATA[time: ([0-9.]+)]]></regex>
    </pattern>
  </adaptor>
</measure>
<score>
  <type>weighted_sum</type>
  <primary-metric>
    <name>time_us</name>
    <direction>Minimize</direction>
    <weight>1.0</weight>
  </primary-metric>
</score>"#,
        )
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

fn apply_resume_stop_condition_overrides(
    config: &mut AutotuneConfig,
    max_iterations: Option<u64>,
    max_duration: Option<String>,
    target_improvement: Option<f64>,
) {
    if let Some(max) = max_iterations {
        config.task.max_iterations = Some(autotune_config::StopValue::Finite(max));
    }
    if let Some(duration) = max_duration {
        config.task.max_duration = Some(duration);
    }
    if let Some(target) = target_improvement {
        config.task.target_improvement = Some(target);
    }
}

fn build_baseline_record(
    baseline_metrics: std::collections::HashMap<String, f64>,
    timestamp: chrono::DateTime<Utc>,
) -> IterationRecord {
    IterationRecord {
        iteration: 0,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: None,
        metrics: baseline_metrics,
        rank: 0.0,
        score: None,
        reason: None,
        fix_attempts: 0,
        fresh_spawns: 0,
        timestamp,
    }
}

fn build_initial_task_state(
    task_name: &str,
    canonical_branch: &str,
    research_session_id: &str,
    research_backend: &str,
) -> TaskState {
    TaskState {
        task_name: task_name.to_string(),
        canonical_branch: canonical_branch.to_string(),
        advancing_branch: format!("autotune/{task_name}-main"),
        research_session_id: research_session_id.to_string(),
        research_backend: research_backend.to_string(),
        current_iteration: 1,
        current_phase: Phase::Planning,
        current_approach: None,
    }
}

fn completion_messages(task_name: &str, resumed: bool, handover_command: &str) -> (String, String) {
    let status = if resumed {
        format!("\n[autotune] task '{task_name}' resumed and complete")
    } else {
        format!("\n[autotune] task '{task_name}' complete")
    };
    let handover = format!("[autotune] research agent handover: {handover_command}");
    (status, handover)
}

fn cmd_run(task_name_override: Option<String>) -> Result<()> {
    let repo_root = find_repo_root()?;
    let mut config = load_config(&repo_root)?;

    // Merge global user config as defaults for agent role settings.
    // Project-level settings in .autotune.toml win; global config fills gaps.
    let global_config = GlobalConfig::load().context("failed to load global config")?;
    apply_global_agent_defaults(&mut config, &global_config);

    // Apply task name override
    if let Some(name) = task_name_override {
        config.task.name = name;
    }

    let task_dir = prepare_run_task_dir(&repo_root, &mut config)?;

    let store = TaskStore::new(&task_dir).context("failed to create task store")?;

    // Snapshot config
    let config_content = std::fs::read_to_string(repo_root.join(".autotune.toml"))
        .context("failed to read config")?;
    store
        .save_config_snapshot(&config_content)
        .context("failed to save config snapshot")?;

    let research_backend = resolve_backend_name(&config.agent, AgentRole::Research)
        .unwrap_or("claude")
        .to_string();
    let agent = build_agent(&config, AgentRole::Research)?;
    let scorer = build_scorer(&config);

    // Build judge agent early so it's available for the baseline measurement.
    let judge_agent = if has_judge_measure(&config) {
        Some(build_agent(&config, AgentRole::Judge)?)
    } else {
        None
    };
    let judge_agent_cfg = judge_agent_session_config(&config, &repo_root);
    let judge_ctx = judge_agent
        .as_ref()
        .map(|a| autotune_benchmark::JudgeContext {
            agent: a.as_ref(),
            agent_config: judge_agent_cfg,
        });

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
    let (baseline_metrics, baseline_reports) = autotune_benchmark::run_all_measures_with_output(
        &config.measure,
        &repo_root,
        "baseline",
        0,
        judge_ctx.as_ref(),
    )
    .context("baseline measures failed")?;
    println!("[autotune] baseline metrics: {:?}", baseline_metrics);

    // Persist raw baseline stdout/stderr per measure so the research agent
    // can look up detailed reports (e.g. coverage output) on demand.
    let mut baseline_output_files: Vec<std::path::PathBuf> = Vec::new();
    for report in &baseline_reports {
        if let Ok(written) =
            store.save_measure_output(0, "baseline", &report.name, &report.stdout, &report.stderr)
        {
            baseline_output_files.extend(written.into_iter().map(|(_stream, path)| path));
        }
    }
    baseline_output_files.sort();

    // Score baseline against itself (rank=0)
    let baseline_record = build_baseline_record(baseline_metrics.clone(), Utc::now());
    store
        .append_ledger(&baseline_record)
        .context("failed to record baseline")?;

    // Spawn research agent
    let research_model = config.agent.research.as_ref().and_then(|r| r.model.clone());
    println!(
        "[autotune] spawning research agent: model={}",
        research_model.as_deref().unwrap_or("default"),
    );
    let research_prompt =
        build_research_agent_prompt(&config, &baseline_metrics, &baseline_output_files);

    let mut research_config = research_agent_session_config(&config, &repo_root);
    research_config.prompt = research_prompt;

    // Forward streaming events (text, tool use) to stderr.
    let research_stream = autotune::stream_ui::Stream::research("exploring codebase...");
    let research_config_with_events = autotune_agent::AgentConfigWithEvents::new(research_config)
        .with_event_handler(research_stream.handler());
    let research_response = agent
        .spawn_streaming(research_config_with_events)
        .context("failed to spawn research agent")?;
    research_stream.finish();

    // Handle any tool-access requests the agent emitted during initial exploration.
    let tool_approver = autotune::stream_ui::TerminalToolApprover;
    let initial_session = autotune_agent::AgentSession {
        session_id: research_response.session_id.clone(),
        backend: research_backend.clone(),
    };
    let spawn_stream = autotune::stream_ui::Stream::research("processing...");
    let spawn_handler = spawn_stream.handler();
    let _research_response = autotune_plan::handle_tool_requests(
        agent.as_ref(),
        &initial_session,
        research_response.clone(),
        Some(&spawn_handler),
        Some(&tool_approver),
    )
    .context("failed to handle research agent tool requests")?;
    spawn_stream.finish();
    let research_response = _research_response;

    println!(
        "[autotune] research agent session: {}",
        research_response.session_id
    );

    // Create the advancing branch where kept iterations accumulate.
    // The user can later PR this branch into the canonical branch. The
    // `-main` suffix is deliberate: worktree branches live at
    // `autotune/<task>/<slug>`, and git refuses to create a branch whose
    // name is a prefix of another existing branch — so the advancing
    // branch must sit alongside, not above, the worktree namespace.
    let advancing_branch = format!("autotune/{}-main", config.task.name);
    autotune_git::create_branch_from(&repo_root, &advancing_branch, &config.task.canonical_branch)
        .context("failed to create advancing branch")?;
    println!(
        "[autotune] created advancing branch '{}' from '{}'",
        advancing_branch, config.task.canonical_branch
    );

    // Initialize state
    let initial_state = build_initial_task_state(
        &config.task.name,
        &config.task.canonical_branch,
        &research_response.session_id,
        &research_backend,
    );
    debug_assert_eq!(initial_state.advancing_branch, advancing_branch);
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
        &autotune::machine::RunContext {
            approver: Some(&tool_approver),
            judge_ctx: judge_ctx.as_ref(),
        },
    )?;

    // Print handover info
    let final_state = store.load_state()?;
    let research_session = autotune_agent::AgentSession {
        session_id: final_state.research_session_id.clone(),
        backend: final_state.research_backend.clone(),
    };
    let (status_line, handover_line) = completion_messages(
        &config.task.name,
        false,
        &agent.handover_command(&research_session),
    );
    println!("{status_line}");
    println!("{handover_line}");
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
    apply_resume_stop_condition_overrides(
        &mut config,
        max_iterations,
        max_duration,
        target_improvement,
    );

    let persisted_state = store.load_state().context("failed to load task state")?;
    let agent = build_agent_for_backend(&persisted_state.research_backend)?;
    let research_session = autotune_agent::AgentSession {
        session_id: persisted_state.research_session_id.clone(),
        backend: persisted_state.research_backend.clone(),
    };
    agent.hydrate_session(
        &research_session,
        &research_agent_session_config(&config, &repo_root),
    )?;
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

    // Build judge agent if any measure uses the judge adaptor.
    let judge_agent = if has_judge_measure(&config) {
        Some(build_agent(&config, AgentRole::Judge)?)
    } else {
        None
    };
    let judge_agent_cfg = judge_agent_session_config(&config, &repo_root);
    let judge_ctx = judge_agent
        .as_ref()
        .map(|a| autotune_benchmark::JudgeContext {
            agent: a.as_ref(),
            agent_config: judge_agent_cfg,
        });

    // Run state machine
    machine::run_task(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &shutdown,
        &autotune::machine::RunContext {
            approver: Some(&tool_approver),
            judge_ctx: judge_ctx.as_ref(),
        },
    )?;

    // Print handover info
    let final_state = store.load_state()?;
    let research_session = autotune_agent::AgentSession {
        session_id: final_state.research_session_id.clone(),
        backend: final_state.research_backend.clone(),
    };
    let (status_line, handover_line) =
        completion_messages(&task_name, true, &agent.handover_command(&research_session));
    println!("{status_line}");
    println!("{handover_line}");

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
            let report = build_report_json(&name, &state, &ledger);
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ReportFormat::Table => {
            print!("{}", render_report_table(&name, &state, &ledger));
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

    let mut rows = Vec::with_capacity(tasks.len());
    for name in &tasks {
        let dir = autotune_dir.join("tasks").join(name);
        let store = TaskStore::open(&dir);
        match store.and_then(|s| s.load_state().map(|st| (s, st))) {
            Ok((_store, state)) => rows.push((name.clone(), Some(state))),
            Err(_) => rows.push((name.clone(), None)),
        }
    }
    print!("{}", render_task_list_table(&rows));

    Ok(())
}

fn cmd_init(name_override: Option<String>) -> Result<()> {
    let repo_root = find_repo_root()?;
    let config_path = repo_root.join(".autotune.toml");
    let mut should_write = false;

    let mut config = if config_path.exists() {
        load_config(&repo_root)?
    } else {
        // Agent-assisted init
        println!("[autotune] no .autotune.toml found — starting agent-assisted init");

        let config = match run_agent_assisted_init(&repo_root)? {
            InitFlowOutcome::Config(config) => *config,
            InitFlowOutcome::Cancelled => {
                println!("\n[autotune] init cancelled");
                return Ok(());
            }
        };
        should_write = true;
        config
    };

    if let Some(name) = name_override
        && config.task.name != name
    {
        config.task.name = name;
        should_write = true;
    }

    if should_write {
        let toml_content = toml::to_string_pretty(&config).context("failed to serialize config")?;
        std::fs::write(&config_path, &toml_content).context("failed to write .autotune.toml")?;
        println!("[autotune] wrote .autotune.toml");
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
    baseline_output_files: &[std::path::PathBuf],
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
        let editable_suites: Vec<&str> = config
            .test
            .iter()
            .filter(|t| t.allow_test_edits)
            .map(|t| t.name.as_str())
            .collect();
        if editable_suites.is_empty() {
            p.push_str("(The implementation agent must not modify test files. Tests must still pass after changes.)\n\n");
        } else {
            writeln!(
                p,
                "(The implementation agent may modify test files when needed. Suites that allow test edits: {}. Tests must still pass after changes.)\n",
                editable_suites.join(", ")
            )
            .ok();
        }
        for t in &config.test {
            writeln!(p, "- {}: `{}`", t.name, t.command.join(" ")).ok();
        }
    }

    p.push_str("\n# Measures run by the CLI to score each approach\n\n");
    p.push_str("(The CLI runs these, NOT you. Do not try to re-run them.)\n\n");
    for m in &config.measure {
        if let Some(cmd) = &m.command {
            writeln!(p, "- {}: `{}`", m.name, cmd.join(" ")).ok();
        } else {
            writeln!(p, "- {}:", m.name).ok();
        }
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
            autotune_config::AdaptorConfig::Judge { persona, rubrics } => {
                writeln!(p, "  - judge adaptor (persona: {persona})").ok();
                for r in rubrics {
                    writeln!(
                        p,
                        "  - rubric `{}`: {} (score {}-{})",
                        r.id, r.title, r.score_range.min, r.score_range.max
                    )
                    .ok();
                }
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

    if !baseline_output_files.is_empty() {
        p.push_str("\n## Baseline raw measure output (on-demand reference)\n\n");
        p.push_str(
            "The full stdout/stderr from each baseline measure was captured \
             to the files below. Read them if the headline metrics above \
             don't give you enough detail (e.g. you need the per-file coverage \
             breakdown from a `cargo llvm-cov` run). Do NOT re-run the measure \
             commands — read the captured output instead.\n\n",
        );
        for path in baseline_output_files {
            writeln!(p, "- `{}`", path.display()).ok();
        }
    }

    p.push_str("\n# What to do\n\n");
    p.push_str(
        "- Do NOT run the measure, test, or build commands listed above. The CLI owns that.\n",
    );
    p.push_str("- Do NOT re-collect the baseline — it's already done.\n");
    p.push_str("- Use Read/Glob/Grep to understand the code that produces the target metric(s).\n");
    p.push_str("- When the CLI asks you to plan the next iteration, propose a concrete, scoped hypothesis with specific files to modify.\n");
    p.push_str("- Your planning response format is an XML `<plan>` fragment with `<approach>`, `<hypothesis>`, and a `<files-to-modify>` list of `<file>` entries. The CLI will tell you when to emit one.\n");
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
    p.push_str("You may emit multiple `<request-tool>` fragments in one response. The CLI will prompt the user for each and reply with a summary of what was granted/denied. Once granted, the tool is available for the rest of this task run. If denied, do NOT re-request the same tool — find another path.\n\n");
    p.push_str(
        "**Critical**: after emitting one or more `<request-tool>` fragments you MUST end your turn immediately. Do not continue using other tools (no more Read/Glob/Grep calls), do not keep typing prose like \"while waiting...\", and do not emit a `<plan>`. The CLI only parses tool requests once your turn ends, so anything you do after the fragments delays approval and wastes work. Emit the request(s), then stop — the CLI will reply with what was granted and you can continue in the next turn.\n",
    );

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
        // Judge measures are evaluated by an LLM agent, not a command — skip them
        // during the trial run. There is nothing to execute and build_adaptor panics
        // for the Judge variant. Rubric IDs are inserted with a 0.0 placeholder so
        // downstream score config sees the expected metric names.
        if let autotune_config::AdaptorConfig::Judge { rubrics, .. } = &measure.adaptor {
            for rubric in rubrics {
                all_metrics.insert(rubric.id.clone(), 0.0);
            }
            continue;
        }

        let command = measure
            .command
            .as_deref()
            .ok_or_else(|| format!("measure '{}' requires a command", measure.name))?;
        let program = command
            .first()
            .ok_or_else(|| format!("measure '{}' has empty command", measure.name))?;
        let args = &command[1..];

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
            let config = load_global_config()?;
            match get_config_value(&config, &key) {
                Some(value) => println!("{}", value),
                None => bail!("key '{}' is not set", key),
            }
        }
        ConfigCommands::Set { key, value } => {
            let path = global_user_config_path()?;
            let mut doc = load_or_create_toml_doc(&path)?;
            set_toml_value(&mut doc, &key, &value)?;
            write_toml_doc(&path, &doc)?;
            println!("{} = {}", key, value);
        }
        ConfigCommands::Unset { key } => {
            let path = global_user_config_path()?;
            if !path.exists() {
                bail!("no user config file exists");
            }
            let mut doc = load_or_create_toml_doc(&path)?;
            unset_toml_value(&mut doc, &key)?;
            write_toml_doc(&path, &doc)?;
            println!("unset {}", key);
        }
        ConfigCommands::List => {
            let config = load_global_config()?;
            if let Ok(path) = global_user_config_path() {
                println!("# {}", path.display());
                println!();
            }
            print_config(&config);
        }
        ConfigCommands::Edit => {
            let path = global_user_config_path()?;
            // Ensure parent dir and file exist
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).context("failed to create config directory")?;
            }
            if !path.exists() {
                std::fs::write(&path, CONFIG_TEMPLATE).context("failed to create config file")?;
            }
            let editor = configured_editor()?;
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

#[cfg(test)]
mod test_support {
    use super::{AutotuneConfig, InitFlowOutcome};
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    #[derive(Default)]
    struct TestOverrides {
        user_config_path: Option<PathBuf>,
        editor: Option<String>,
        init_outcome: Option<InitFlowOutcome>,
    }

    fn overrides() -> &'static Mutex<TestOverrides> {
        static OVERRIDES: OnceLock<Mutex<TestOverrides>> = OnceLock::new();
        OVERRIDES.get_or_init(|| Mutex::new(TestOverrides::default()))
    }

    pub fn set_user_config_path_override(path: PathBuf) {
        overrides().lock().unwrap().user_config_path = Some(path);
    }

    pub fn clear_user_config_path_override() {
        overrides().lock().unwrap().user_config_path = None;
    }

    pub fn global_user_config_path_override() -> Option<PathBuf> {
        overrides().lock().unwrap().user_config_path.clone()
    }

    pub fn set_editor_override(editor: impl Into<String>) {
        overrides().lock().unwrap().editor = Some(editor.into());
    }

    pub fn clear_editor_override() {
        overrides().lock().unwrap().editor = None;
    }

    pub fn editor_override() -> Option<String> {
        overrides().lock().unwrap().editor.clone()
    }

    pub fn set_init_override_config(config: AutotuneConfig) {
        overrides().lock().unwrap().init_outcome = Some(InitFlowOutcome::Config(Box::new(config)));
    }

    pub fn set_init_override_cancelled() {
        overrides().lock().unwrap().init_outcome = Some(InitFlowOutcome::Cancelled);
    }

    pub fn clear_init_override() {
        overrides().lock().unwrap().init_outcome = None;
    }

    pub fn take_init_override() -> Option<InitFlowOutcome> {
        overrides().lock().unwrap().init_outcome.take()
    }
}

const CONFIG_TEMPLATE: &str = r#"# Autotune global config
# Default agent settings used across all tasks.
# Uncomment and edit the values you want to set.

# [agent]
# backend = "claude"            # LLM backend (supported: claude, codex)

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
#[derive(Clone, Copy)]
enum ConfigValueKind {
    String,
    Integer,
}

struct ConfigKeyDef {
    key: &'static str,
    kind: ConfigValueKind,
    get: fn(&GlobalConfig) -> Option<String>,
}

const CONFIG_KEYS: &[ConfigKeyDef] = &[
    ConfigKeyDef {
        key: "agent.backend",
        kind: ConfigValueKind::String,
        get: |config| config.agent.as_ref()?.backend.clone(),
    },
    ConfigKeyDef {
        key: "agent.research.model",
        kind: ConfigValueKind::String,
        get: |config| config.agent.as_ref()?.research.as_ref()?.model.clone(),
    },
    ConfigKeyDef {
        key: "agent.research.max_turns",
        kind: ConfigValueKind::Integer,
        get: |config| {
            config
                .agent
                .as_ref()?
                .research
                .as_ref()?
                .max_turns
                .map(|value| value.to_string())
        },
    },
    ConfigKeyDef {
        key: "agent.research.backend",
        kind: ConfigValueKind::String,
        get: |config| config.agent.as_ref()?.research.as_ref()?.backend.clone(),
    },
    ConfigKeyDef {
        key: "agent.implementation.model",
        kind: ConfigValueKind::String,
        get: |config| {
            config
                .agent
                .as_ref()?
                .implementation
                .as_ref()?
                .model
                .clone()
        },
    },
    ConfigKeyDef {
        key: "agent.implementation.max_turns",
        kind: ConfigValueKind::Integer,
        get: |config| {
            config
                .agent
                .as_ref()?
                .implementation
                .as_ref()?
                .max_turns
                .map(|value| value.to_string())
        },
    },
    ConfigKeyDef {
        key: "agent.implementation.backend",
        kind: ConfigValueKind::String,
        get: |config| {
            config
                .agent
                .as_ref()?
                .implementation
                .as_ref()?
                .backend
                .clone()
        },
    },
    ConfigKeyDef {
        key: "agent.init.model",
        kind: ConfigValueKind::String,
        get: |config| config.agent.as_ref()?.init.as_ref()?.model.clone(),
    },
    ConfigKeyDef {
        key: "agent.init.max_turns",
        kind: ConfigValueKind::Integer,
        get: |config| {
            config
                .agent
                .as_ref()?
                .init
                .as_ref()?
                .max_turns
                .map(|value| value.to_string())
        },
    },
    ConfigKeyDef {
        key: "agent.init.backend",
        kind: ConfigValueKind::String,
        get: |config| config.agent.as_ref()?.init.as_ref()?.backend.clone(),
    },
];

fn config_key_def(key: &str) -> Option<&'static ConfigKeyDef> {
    CONFIG_KEYS.iter().find(|def| def.key == key)
}

fn validate_key(key: &str) -> Result<()> {
    if config_key_def(key).is_some() {
        Ok(())
    } else {
        bail!(
            "unknown config key '{}'. Valid keys:\n  {}",
            key,
            CONFIG_KEYS
                .iter()
                .map(|def| def.key)
                .collect::<Vec<_>>()
                .join("\n  ")
        )
    }
}

fn get_config_value(config: &GlobalConfig, key: &str) -> Option<String> {
    config_key_def(key).and_then(|def| (def.get)(config))
}

fn print_config(config: &GlobalConfig) {
    let mut printed_any = false;
    for def in CONFIG_KEYS {
        if let Some(value) = (def.get)(config) {
            println!("{} = {}", def.key, value);
            printed_any = true;
        }
    }

    if !printed_any {
        println!("(no config set)");
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

fn split_config_key(key: &str) -> Result<Vec<&str>> {
    validate_key(key)?;
    Ok(key.split('.').collect())
}

fn navigate_config_table_mut<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    parts: &[&str],
    create_missing: bool,
) -> Result<&'a mut toml_edit::Table> {
    let mut table = doc.as_table_mut();
    for &part in parts {
        if create_missing && !table.contains_key(part) {
            table.insert(part, toml_edit::Item::Table(toml_edit::Table::new()));
        }

        let item = match table.get_mut(part) {
            Some(item) => item,
            None => bail!("key '{}' is not set", parts.join(".")),
        };

        table = item
            .as_table_mut()
            .with_context(|| format!("'{}' is not a table in config", part))?;
    }
    Ok(table)
}

fn set_toml_value(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<()> {
    let parts = split_config_key(key)?;
    let def = config_key_def(key).expect("validated config key must exist");
    let table = navigate_config_table_mut(doc, &parts[..parts.len() - 1], true)?;
    let leaf = parts[parts.len() - 1];

    let toml_value = match def.kind {
        ConfigValueKind::Integer => {
            let n: u64 = value
                .parse()
                .with_context(|| format!("'{}' must be an integer", key))?;
            toml_edit::value(n as i64)
        }
        ConfigValueKind::String => toml_edit::value(value),
    };

    table.insert(leaf, toml_value);
    Ok(())
}

fn unset_toml_value(doc: &mut toml_edit::DocumentMut, key: &str) -> Result<()> {
    let parts = split_config_key(key)?;
    let table = navigate_config_table_mut(doc, &parts[..parts.len() - 1], false)?;
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

    let agent = build_agent_for_backend(&state.research_backend)?;
    let research_session = autotune_agent::AgentSession {
        session_id: state.research_session_id.clone(),
        backend: state.research_backend.clone(),
    };
    agent.hydrate_session(
        &research_session,
        &research_agent_session_config(&config, &repo_root),
    )?;
    let scorer = build_scorer(&config);

    let tool_approver = autotune::stream_ui::TerminalToolApprover;

    let judge_agent = if has_judge_measure(&config) {
        Some(build_agent(&config, AgentRole::Judge)?)
    } else {
        None
    };
    let judge_agent_cfg = judge_agent_session_config(&config, &repo_root);
    let judge_ctx = judge_agent
        .as_ref()
        .map(|a| autotune_benchmark::JudgeContext {
            agent: a.as_ref(),
            agent_config: judge_agent_cfg,
        });

    machine::run_single_phase(
        &config,
        agent.as_ref(),
        scorer.as_ref(),
        &repo_root,
        &store,
        &mut state,
        &autotune::machine::RunContext {
            approver: Some(&tool_approver),
            judge_ctx: judge_ctx.as_ref(),
        },
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

    let export = build_export_json(&task_name, &config_toml, &ledger, &log, &state);

    let json = serde_json::to_string_pretty(&export).context("failed to serialize export")?;
    std::fs::write(&output_path, &json)
        .with_context(|| format!("failed to write export to {}", output_path))?;

    println!(
        "[autotune] exported task '{}' to {}",
        task_name, output_path
    );

    Ok(())
}

fn build_report_json(
    task_name: &str,
    state: &TaskState,
    ledger: &[IterationRecord],
) -> serde_json::Value {
    serde_json::json!({
        "task": task_name,
        "phase": format!("{}", state.current_phase),
        "iteration": state.current_iteration,
        "ledger": ledger,
    })
}

fn render_report_table(task_name: &str, state: &TaskState, ledger: &[IterationRecord]) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    writeln!(output, "Task: {task_name}").ok();
    writeln!(output, "Phase: {}", state.current_phase).ok();
    writeln!(output, "Iteration: {}", state.current_iteration).ok();
    output.push('\n');
    writeln!(
        output,
        "{:<6} {:<20} {:<10} {:<10} Reason",
        "Iter", "Approach", "Status", "Rank"
    )
    .ok();
    writeln!(output, "{}", "-".repeat(70)).ok();
    for record in ledger {
        writeln!(
            output,
            "{:<6} {:<20} {:<10} {:<10.4} {}",
            record.iteration,
            truncate(&record.approach, 18),
            format!("{:?}", record.status),
            record.rank,
            record.reason.as_deref().unwrap_or("")
        )
        .ok();
        writeln!(output, "       metrics:").ok();
        for (name, value) in sorted_metrics(&record.metrics) {
            writeln!(output, "         {name}={value:.4}").ok();
        }
    }
    output
}

fn sorted_metrics(metrics: &std::collections::HashMap<String, f64>) -> Vec<(&str, f64)> {
    let mut entries: Vec<_> = metrics
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    entries.sort_by_key(|(left, _)| *left);
    entries
}

fn render_task_list_table(rows: &[(String, Option<TaskState>)]) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    writeln!(output, "{:<30} {:<15} {:<6}", "Name", "Phase", "Iter").ok();
    writeln!(output, "{}", "-".repeat(55)).ok();
    for (name, state) in rows {
        match state {
            Some(state) => writeln!(
                output,
                "{:<30} {:<15} {:<6}",
                name, state.current_phase, state.current_iteration
            )
            .ok(),
            None => writeln!(output, "{:<30} {:<15} {:<6}", name, "unknown", "-").ok(),
        };
    }
    output
}

fn build_export_json(
    task_name: &str,
    config_toml: &str,
    ledger: &[IterationRecord],
    log: &str,
    state: &TaskState,
) -> serde_json::Value {
    serde_json::json!({
        "task_name": task_name,
        "config": config_toml,
        "ledger": ledger,
        "log": log,
        "state": state,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autotune::agent_factory::AgentRole;
    use autotune_agent::ToolPermission;
    use autotune_score::{ScoreError, ScoreInput};
    use serde_json::json;
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn sample_config() -> AutotuneConfig {
        AutotuneConfig {
            task: autotune_config::TaskConfig {
                name: "coverage-task".to_string(),
                description: Some("Improve line coverage".to_string()),
                canonical_branch: "main".to_string(),
                max_iterations: Some(autotune_config::StopValue::Infinite),
                target_improvement: Some(0.25),
                max_duration: Some("2h".to_string()),
                target_metric: vec![autotune_config::TargetMetric {
                    name: "line_coverage".to_string(),
                    value: 80.0,
                    direction: autotune_config::Direction::Maximize,
                }],
            },
            paths: autotune_config::PathsConfig {
                tunable: vec!["src/**".to_string(), "crates/**".to_string()],
                denied: vec!["tests/**".to_string()],
            },
            test: vec![autotune_config::TestConfig {
                name: "unit".to_string(),
                command: vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "autotune".to_string(),
                ],
                timeout: 300,
                allow_test_edits: false,
            }],
            measure: vec![
                autotune_config::MeasureConfig {
                    name: "coverage".to_string(),
                    command: Some(vec!["cargo".to_string(), "llvm-cov".to_string()]),
                    timeout: 600,
                    adaptor: autotune_config::AdaptorConfig::Regex {
                        patterns: vec![
                            autotune_config::RegexPattern {
                                name: "line_coverage".to_string(),
                                pattern: "coverage: ([0-9.]+)".to_string(),
                            },
                            autotune_config::RegexPattern {
                                name: "runtime_ms".to_string(),
                                pattern: "runtime_ms: ([0-9.]+)".to_string(),
                            },
                        ],
                    },
                },
                autotune_config::MeasureConfig {
                    name: "criterion".to_string(),
                    command: Some(vec!["cargo".to_string(), "bench".to_string()]),
                    timeout: 600,
                    adaptor: autotune_config::AdaptorConfig::Criterion {
                        measure_name: "throughput".to_string(),
                    },
                },
                autotune_config::MeasureConfig {
                    name: "scripted".to_string(),
                    command: Some(vec!["./bench.sh".to_string()]),
                    timeout: 600,
                    adaptor: autotune_config::AdaptorConfig::Script {
                        command: vec!["python3".to_string(), "extract.py".to_string()],
                    },
                },
            ],
            score: autotune_config::ScoreConfig::WeightedSum {
                primary_metrics: vec![autotune_config::PrimaryMetric {
                    name: "line_coverage".to_string(),
                    direction: autotune_config::Direction::Maximize,
                    weight: 1.5,
                }],
                guardrail_metrics: vec![autotune_config::GuardrailMetric {
                    name: "runtime_ms".to_string(),
                    direction: autotune_config::Direction::Minimize,
                    max_regression: 0.1,
                }],
            },
            agent: autotune_config::AgentConfig::default(),
        }
    }

    fn sample_state() -> TaskState {
        TaskState {
            task_name: "coverage-task".to_string(),
            canonical_branch: "main".to_string(),
            advancing_branch: "autotune/coverage-task-main".to_string(),
            research_session_id: "session-123".to_string(),
            research_backend: "codex".to_string(),
            current_iteration: 3,
            current_phase: Phase::Scoring,
            current_approach: Some(autotune_state::ApproachState {
                name: "raise-line-coverage".to_string(),
                hypothesis: "Add tests around reporting formatters".to_string(),
                worktree_path: PathBuf::from("/tmp/coverage-task"),
                branch_name: "autotune/coverage-task/raise-line-coverage".to_string(),
                commit_sha: Some("abc123".to_string()),
                test_results: vec![autotune_state::TestResult {
                    name: "unit".to_string(),
                    passed: true,
                    duration_secs: 1.2,
                    output: None,
                }],
                metrics: Some(HashMap::from([("line_coverage".to_string(), 78.9)])),
                rank: Some(0.064),
                files_to_modify: vec!["crates/autotune/src/main.rs".to_string()],
                impl_session_id: Some("impl-123".to_string()),
                impl_backend: Some("codex".to_string()),
                fix_attempts: 1,
                fresh_spawns: 0,
                fix_history: vec![],
                score_reason: Some("coverage improved".to_string()),
            }),
        }
    }

    fn sample_ledger() -> Vec<IterationRecord> {
        vec![
            IterationRecord {
                iteration: 0,
                approach: "baseline".to_string(),
                status: IterationStatus::Baseline,
                hypothesis: None,
                metrics: HashMap::from([("line_coverage".to_string(), 72.4)]),
                rank: 0.0,
                score: None,
                reason: None,
                fix_attempts: 0,
                fresh_spawns: 0,
                timestamp: Utc::now(),
            },
            IterationRecord {
                iteration: 2,
                approach: "very-long-coverage-approach-name".to_string(),
                status: IterationStatus::Kept,
                hypothesis: Some("Add missing formatter tests".to_string()),
                metrics: HashMap::from([("line_coverage".to_string(), 78.9)]),
                rank: 0.064,
                score: Some("keep".to_string()),
                reason: Some("coverage improved".to_string()),
                fix_attempts: 1,
                fresh_spawns: 0,
                timestamp: Utc::now(),
            },
        ]
    }

    fn score_input(best: &[(&str, f64)], candidate: &[(&str, f64)]) -> ScoreInput {
        let to_map = |pairs: &[(&str, f64)]| -> HashMap<String, f64> {
            pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
        };

        ScoreInput {
            baseline: to_map(best),
            candidate: to_map(candidate),
            best: to_map(best),
        }
    }

    fn command_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct CommandTestGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous_cwd: PathBuf,
    }

    impl CommandTestGuard {
        fn enter(repo_root: &Path) -> Self {
            let lock = command_test_lock().lock().unwrap();
            let previous_cwd = std::env::current_dir().unwrap();
            std::env::set_current_dir(repo_root).unwrap();
            test_support::clear_user_config_path_override();
            test_support::clear_editor_override();
            test_support::clear_init_override();
            Self {
                _lock: lock,
                previous_cwd,
            }
        }
    }

    impl Drop for CommandTestGuard {
        fn drop(&mut self) {
            test_support::clear_user_config_path_override();
            test_support::clear_editor_override();
            test_support::clear_init_override();
            std::env::set_current_dir(&self.previous_cwd).unwrap();
        }
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_repo() -> tempfile::TempDir {
        let repo = tempdir().unwrap();
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["checkout", "-B", "main"]);
        run_git(repo.path(), &["config", "user.name", "Autotune Tests"]);
        run_git(
            repo.path(),
            &["config", "user.email", "autotune-tests@example.com"],
        );
        std::fs::write(repo.path().join("README.md"), "seed\n").unwrap();
        run_git(repo.path(), &["add", "README.md"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);
        repo
    }

    fn write_project_config(repo_root: &Path, task_name: &str) -> AutotuneConfig {
        let mut config = sample_config();
        config.task.name = task_name.to_string();
        config.task.canonical_branch = "main".to_string();
        config.test.clear();
        config.measure = vec![autotune_config::MeasureConfig {
            name: "coverage".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'line_coverage: 72\\nruntime_ms: 100\\n'".to_string(),
            ]),
            timeout: 30,
            adaptor: autotune_config::AdaptorConfig::Regex {
                patterns: vec![
                    autotune_config::RegexPattern {
                        name: "line_coverage".to_string(),
                        pattern: "line_coverage: ([0-9.]+)".to_string(),
                    },
                    autotune_config::RegexPattern {
                        name: "runtime_ms".to_string(),
                        pattern: "runtime_ms: ([0-9.]+)".to_string(),
                    },
                ],
            },
        }];
        std::fs::write(
            repo_root.join(".autotune.toml"),
            toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
        config
    }

    fn write_task_fixture(repo_root: &Path, task_name: &str, phase: Phase) -> TaskStore {
        let mut state = sample_state();
        state.task_name = task_name.to_string();
        state.current_phase = phase;
        state.advancing_branch = format!("autotune/{task_name}-main");

        let mut config = sample_config();
        config.task.name = task_name.to_string();
        config.task.canonical_branch = "main".to_string();

        let store = TaskStore::new(&repo_root.join(".autotune/tasks").join(task_name)).unwrap();
        store.save_state(&state).unwrap();
        store
            .save_config_snapshot(&toml::to_string_pretty(&config).unwrap())
            .unwrap();
        for record in sample_ledger() {
            store.append_ledger(&record).unwrap();
        }
        store.append_log("investigation notes").unwrap();
        store
    }

    #[test]
    fn global_backend_name_prefers_init_override() {
        let global = GlobalConfig {
            agent: Some(autotune_config::AgentConfig {
                backend: Some("claude".to_string()),
                model: None,
                max_turns: None,
                reasoning_effort: None,
                max_fix_attempts: None,
                max_fresh_spawns: None,
                research: None,
                implementation: None,
                init: Some(autotune_config::AgentRoleConfig {
                    backend: Some("codex".to_string()),
                    model: None,
                    max_turns: None,
                    reasoning_effort: None,
                    max_fix_attempts: None,
                    max_fresh_spawns: None,
                }),
                judge: None,
            }),
        };

        assert_eq!(global_backend_name(&global, AgentRole::Init), Some("codex"));
    }

    #[test]
    fn apply_global_agent_defaults_copies_backend_overrides() {
        let mut config: AutotuneConfig = toml::from_str(
            r#"
[task]
name = "demo"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[measure]]
name = "bench"
command = ["echo", "metric: 1"]
adaptor = { type = "regex", patterns = [{ name = "metric", pattern = "metric: ([0-9]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric", direction = "Minimize" }]
"#,
        )
        .unwrap();
        let global = GlobalConfig {
            agent: Some(autotune_config::AgentConfig {
                backend: Some("codex".to_string()),
                model: None,
                max_turns: None,
                reasoning_effort: None,
                max_fix_attempts: None,
                max_fresh_spawns: None,
                research: Some(autotune_config::AgentRoleConfig {
                    backend: Some("codex".to_string()),
                    model: None,
                    max_turns: None,
                    reasoning_effort: None,
                    max_fix_attempts: None,
                    max_fresh_spawns: None,
                }),
                implementation: None,
                init: None,
                judge: None,
            }),
        };

        apply_global_agent_defaults(&mut config, &global);

        assert_eq!(config.agent.backend.as_deref(), Some("codex"));
        assert_eq!(
            config
                .agent
                .research
                .as_ref()
                .and_then(|r| r.backend.as_deref()),
            Some("codex")
        );
    }

    #[test]
    fn apply_global_agent_defaults_leaves_project_backend_unset_without_global_backend() {
        let mut config: AutotuneConfig = toml::from_str(
            r#"
[task]
name = "demo"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[measure]]
name = "bench"
command = ["echo", "metric: 1"]
adaptor = { type = "regex", patterns = [{ name = "metric", pattern = "metric: ([0-9]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric", direction = "Minimize" }]
"#,
        )
        .unwrap();
        let global = GlobalConfig::default();

        apply_global_agent_defaults(&mut config, &global);

        assert_eq!(config.agent.backend, None);
    }

    #[test]
    fn apply_global_agent_defaults_respects_global_role_and_project_precedence_for_all_roles() {
        let mut config: AutotuneConfig = toml::from_str(
            r#"
[task]
name = "demo"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[measure]]
name = "bench"
command = ["echo", "metric: 1"]
adaptor = { type = "regex", patterns = [{ name = "metric", pattern = "metric: ([0-9]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric", direction = "Minimize" }]

[agent]
model = "project-top"
reasoning_effort = "high"

[agent.research]
reasoning_effort = "low"
"#,
        )
        .unwrap();
        let global = GlobalConfig {
            agent: Some(autotune_config::AgentConfig {
                backend: Some("codex".to_string()),
                model: Some("global-top".to_string()),
                max_turns: None,
                reasoning_effort: Some(autotune_config::ReasoningEffort::Medium),
                max_fix_attempts: None,
                max_fresh_spawns: None,
                research: Some(autotune_config::AgentRoleConfig {
                    backend: Some("codex".to_string()),
                    model: Some("global-research".to_string()),
                    max_turns: None,
                    reasoning_effort: None,
                    max_fix_attempts: None,
                    max_fresh_spawns: None,
                }),
                implementation: None,
                init: Some(autotune_config::AgentRoleConfig {
                    backend: None,
                    model: None,
                    max_turns: Some(7),
                    reasoning_effort: Some(autotune_config::ReasoningEffort::Low),
                    max_fix_attempts: None,
                    max_fresh_spawns: None,
                }),
                judge: None,
            }),
        };

        apply_global_agent_defaults(&mut config, &global);

        assert_eq!(config.agent.backend.as_deref(), Some("codex"));

        let research = config.agent.research.as_ref().expect("research role");
        assert_eq!(research.backend.as_deref(), Some("codex"));
        assert_eq!(research.model.as_deref(), Some("project-top"));
        assert_eq!(
            research.reasoning_effort,
            Some(autotune_config::ReasoningEffort::Low)
        );

        let implementation = config
            .agent
            .implementation
            .as_ref()
            .expect("implementation role");
        assert_eq!(implementation.backend.as_deref(), Some("codex"));
        assert_eq!(implementation.model.as_deref(), Some("project-top"));
        assert_eq!(
            implementation.reasoning_effort,
            Some(autotune_config::ReasoningEffort::High)
        );

        let init = config.agent.init.as_ref().expect("init role");
        assert_eq!(init.backend.as_deref(), Some("codex"));
        assert_eq!(init.model.as_deref(), Some("project-top"));
        assert_eq!(init.max_turns, Some(7));
        assert_eq!(
            init.reasoning_effort,
            Some(autotune_config::ReasoningEffort::High)
        );
    }

    #[test]
    fn get_config_value_uses_shared_key_table_for_role_values() {
        let global = GlobalConfig {
            agent: Some(autotune_config::AgentConfig {
                backend: Some("claude".to_string()),
                model: None,
                max_turns: None,
                reasoning_effort: None,
                max_fix_attempts: None,
                max_fresh_spawns: None,
                research: None,
                implementation: Some(autotune_config::AgentRoleConfig {
                    backend: Some("codex".to_string()),
                    model: Some("gpt-5".to_string()),
                    max_turns: Some(42),
                    reasoning_effort: None,
                    max_fix_attempts: None,
                    max_fresh_spawns: None,
                }),
                init: None,
                judge: None,
            }),
        };

        assert_eq!(
            get_config_value(&global, "agent.implementation.backend").as_deref(),
            Some("codex")
        );
        assert_eq!(
            get_config_value(&global, "agent.implementation.model").as_deref(),
            Some("gpt-5")
        );
        assert_eq!(
            get_config_value(&global, "agent.implementation.max_turns").as_deref(),
            Some("42")
        );
    }

    #[test]
    fn set_and_unset_toml_value_share_dotted_path_navigation() {
        let mut doc: toml_edit::DocumentMut = "[agent]\nbackend = \"claude\"\n".parse().unwrap();

        set_toml_value(&mut doc, "agent.research.max_turns", "7").unwrap();
        set_toml_value(&mut doc, "agent.research.model", "opus").unwrap();

        assert_eq!(doc["agent"]["research"]["max_turns"].as_integer(), Some(7));
        assert_eq!(doc["agent"]["research"]["model"].as_str(), Some("opus"));

        unset_toml_value(&mut doc, "agent.research.max_turns").unwrap();
        assert!(doc["agent"]["research"].get("max_turns").is_none());
        assert_eq!(doc["agent"]["research"]["model"].as_str(), Some("opus"));
    }

    #[test]
    fn validate_key_rejects_unknown_keys() {
        let err = validate_key("agent.unknown").unwrap_err();
        let message = err.to_string();

        assert!(message.contains("unknown config key 'agent.unknown'"));
        assert!(message.contains("agent.backend"));
        assert!(message.contains("agent.research.model"));
    }

    #[test]
    fn set_toml_value_rejects_non_integer_values_for_integer_keys() {
        let mut doc = toml_edit::DocumentMut::new();

        let err = set_toml_value(&mut doc, "agent.research.max_turns", "abc").unwrap_err();

        assert_eq!(
            err.to_string(),
            "'agent.research.max_turns' must be an integer"
        );
    }

    #[test]
    fn unset_toml_value_errors_when_parent_path_is_missing() {
        let mut doc = toml_edit::DocumentMut::new();

        let err = unset_toml_value(&mut doc, "agent.research.model").unwrap_err();

        assert_eq!(err.to_string(), "key 'agent.research' is not set");
    }

    #[test]
    fn unset_toml_value_errors_when_leaf_is_missing() {
        let mut doc: toml_edit::DocumentMut =
            "[agent.research]\nmodel = \"gpt-5\"\n".parse().unwrap();

        let err = unset_toml_value(&mut doc, "agent.research.max_turns").unwrap_err();

        assert_eq!(err.to_string(), "key 'agent.research.max_turns' is not set");
    }

    #[test]
    fn navigate_config_table_mut_errors_when_path_segment_is_not_a_table() {
        let mut doc: toml_edit::DocumentMut = "agent = \"claude\"\n".parse().unwrap();

        let err = navigate_config_table_mut(&mut doc, &["agent"], false).unwrap_err();

        assert_eq!(err.to_string(), "'agent' is not a table in config");
    }

    #[test]
    fn load_or_create_toml_doc_returns_empty_document_for_missing_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");

        let doc = load_or_create_toml_doc(&path).unwrap();

        assert!(doc.as_table().is_empty());
    }

    #[test]
    fn load_or_create_toml_doc_surfaces_parse_errors() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[agent\nbackend = \"claude\"\n").unwrap();

        let err = load_or_create_toml_doc(&path).unwrap_err();

        assert!(err.to_string().contains("failed to parse config file"));
    }

    #[test]
    fn write_toml_doc_creates_parent_directories() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("nested/config.toml");
        let mut doc = toml_edit::DocumentMut::new();
        set_toml_value(&mut doc, "agent.backend", "codex").unwrap();

        write_toml_doc(&path, &doc).unwrap();

        let written: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(written["agent"]["backend"].as_str(), Some("codex"));
    }

    #[test]
    fn config_template_mentions_claude_and_codex() {
        assert!(CONFIG_TEMPLATE.contains("backend = \"claude\""));
        assert!(CONFIG_TEMPLATE.contains("claude, codex"));
    }

    #[test]
    fn build_research_agent_prompt_includes_high_value_context() {
        let config = sample_config();
        let baseline_metrics = HashMap::from([
            ("runtime_ms".to_string(), 12.0),
            ("line_coverage".to_string(), 78.5),
        ]);
        let baseline_output_files = vec![
            PathBuf::from(".autotune/tasks/coverage-task/iterations/000-baseline/coverage.stdout"),
            PathBuf::from(".autotune/tasks/coverage-task/iterations/000-baseline/coverage.stderr"),
        ];

        let prompt =
            build_research_agent_prompt(&config, &baseline_metrics, &baseline_output_files);

        assert!(prompt.contains("- Name: coverage-task"));
        assert!(prompt.contains("- Description: Improve line coverage"));
        assert!(prompt.contains("- max_iterations: inf (no hard cap)"));
        assert!(prompt.contains("- target_improvement: rank >= 0.25"));
        assert!(prompt.contains("- max_duration: 2h"));
        assert!(prompt.contains("- target_metric: line_coverage >= 80"));
        assert!(prompt.contains("Tunable globs"));
        assert!(prompt.contains("- src/**"));
        assert!(prompt.contains("Denied globs"));
        assert!(prompt.contains("- tests/**"));
        assert!(prompt.contains("- unit: `cargo test -p autotune`"));
        assert!(prompt.contains("- coverage: `cargo llvm-cov`"));
        assert!(prompt.contains("extracts `line_coverage` via regex"));
        assert!(prompt.contains("extracts criterion metrics from `throughput`"));
        assert!(prompt.contains("extracts metrics via script: `python3 extract.py`"));
        assert!(prompt.contains("Score is a weighted sum"));
        assert!(prompt.contains("- line_coverage (Maximize, weight=1.5)"));
        assert!(prompt.contains("- runtime_ms (Minimize, max_regression=0.1)"));
        assert!(prompt.contains("- line_coverage: 78.5"));
        assert!(prompt.contains("- runtime_ms: 12"));
        assert!(prompt.contains("Do NOT re-run the measure commands"));
        assert!(prompt.contains("<request-tool>"));
        assert!(prompt.contains(&format!("- `{}`", baseline_output_files[0].display())));
    }

    #[test]
    fn build_research_agent_prompt_handles_missing_optional_sections() {
        let mut config = sample_config();
        config.task.description = None;
        config.task.max_iterations = None;
        config.task.target_improvement = None;
        config.task.max_duration = None;
        config.task.target_metric.clear();
        config.paths.denied.clear();
        config.test.clear();
        config.measure.truncate(1);
        config.score = autotune_config::ScoreConfig::Threshold {
            conditions: vec![autotune_config::ThresholdCondition {
                metric: "line_coverage".to_string(),
                direction: autotune_config::Direction::Maximize,
                threshold: 85.0,
            }],
        };

        let prompt = build_research_agent_prompt(&config, &HashMap::new(), &[]);

        assert!(prompt.contains("- (none configured)"));
        assert!(!prompt.contains("Denied globs"));
        assert!(!prompt.contains("# Test suites run by the CLI after each approach"));
        assert!(prompt.contains("(no baseline metrics were extracted)"));
        assert!(prompt.contains("Score uses thresholds:"));
        assert!(prompt.contains("- line_coverage Maximize 85"));
        assert!(!prompt.contains("Baseline raw measure output"));
    }

    #[test]
    fn build_research_agent_prompt_forbids_test_edits_by_default() {
        let prompt = build_research_agent_prompt(&sample_config(), &HashMap::new(), &[]);

        assert!(prompt.contains("must not modify test files"));
        assert!(!prompt.contains("may modify test files"));
    }

    #[test]
    fn build_research_agent_prompt_allows_test_edits_when_enabled() {
        let mut config = sample_config();
        config.test[0].allow_test_edits = true;

        let prompt = build_research_agent_prompt(&config, &HashMap::new(), &[]);

        assert!(prompt.contains("may modify test files"));
        assert!(!prompt.contains("must not modify test files"));
    }

    #[test]
    fn build_report_json_serializes_task_state_and_ledger() {
        let state = sample_state();
        let ledger = sample_ledger();

        let report = build_report_json("coverage-task", &state, &ledger);

        assert_eq!(report["task"], json!("coverage-task"));
        assert_eq!(report["phase"], json!("Scoring"));
        assert_eq!(report["iteration"], json!(3));
        assert_eq!(report["ledger"][0]["approach"], json!("baseline"));
        assert_eq!(report["ledger"][1]["reason"], json!("coverage improved"));
    }

    #[test]
    fn render_report_table_formats_header_and_truncates_approach_names() {
        let state = sample_state();
        let ledger = sample_ledger();

        let table = render_report_table("coverage-task", &state, &ledger);

        assert!(table.contains("Task: coverage-task"));
        assert!(table.contains("Phase: Scoring"));
        assert!(table.contains("Iteration: 3"));
        assert!(table.contains("Iter   Approach             Status     Rank"));
        assert!(table.contains("baseline"));
        assert!(table.contains(&truncate("very-long-coverage-approach-name", 18)));
        assert!(table.contains("0.0640"));
        assert!(table.contains("coverage improved"));
        assert!(table.contains("metrics:"));
        assert!(table.contains("line_coverage=72.4000"));
        assert!(table.contains("line_coverage=78.9000"));
    }

    #[test]
    fn render_task_list_table_handles_known_and_unknown_state_rows() {
        let state = sample_state();
        let table = render_task_list_table(&[
            ("coverage-task".to_string(), Some(state)),
            ("broken-task".to_string(), None),
        ]);

        assert!(table.contains("Name"));
        assert!(table.contains("coverage-task"));
        assert!(table.contains("Scoring"));
        assert!(table.contains("3"));
        assert!(table.contains("broken-task"));
        assert!(table.contains("unknown"));
    }

    #[test]
    fn build_export_json_includes_snapshot_log_and_state() {
        let state = sample_state();
        let ledger = sample_ledger();

        let export = build_export_json(
            "coverage-task",
            "[task]\nname = \"coverage-task\"\n",
            &ledger,
            "investigation notes",
            &state,
        );

        assert_eq!(export["task_name"], json!("coverage-task"));
        assert_eq!(
            export["config"],
            json!("[task]\nname = \"coverage-task\"\n")
        );
        assert_eq!(export["log"], json!("investigation notes"));
        assert_eq!(export["state"]["current_phase"], json!("scoring"));
        assert_eq!(export["ledger"][1]["status"], json!("kept"));
    }

    #[test]
    fn validate_measure_config_extracts_metrics_from_successful_commands() {
        let workdir = tempdir().unwrap();
        let measures = vec![autotune_config::MeasureConfig {
            name: "echo-metric".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'metric: 41.5\\n'".to_string(),
            ]),
            timeout: 600,
            adaptor: autotune_config::AdaptorConfig::Regex {
                patterns: vec![autotune_config::RegexPattern {
                    name: "metric".to_string(),
                    pattern: "metric: ([0-9.]+)".to_string(),
                }],
            },
        }];

        let metrics = validate_measure_config(&measures, workdir.path()).unwrap();

        assert_eq!(metrics.get("metric"), Some(&41.5));
    }

    #[test]
    fn validate_measure_config_surfaces_command_failure_output() {
        let workdir = tempdir().unwrap();
        let measures = vec![autotune_config::MeasureConfig {
            name: "broken".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'out\\n'; printf 'err\\n' 1>&2; exit 7".to_string(),
            ]),
            timeout: 600,
            adaptor: autotune_config::AdaptorConfig::Regex {
                patterns: vec![autotune_config::RegexPattern {
                    name: "metric".to_string(),
                    pattern: "metric: ([0-9.]+)".to_string(),
                }],
            },
        }];

        let err = validate_measure_config(&measures, workdir.path()).unwrap_err();

        assert!(err.contains("measure 'broken' command failed (exit code 7)"));
        assert!(err.contains("stdout:\nout"));
        assert!(err.contains("stderr:\nerr"));
    }

    #[test]
    fn validate_measure_config_surfaces_extraction_failure_output() {
        let workdir = tempdir().unwrap();
        let measures = vec![autotune_config::MeasureConfig {
            name: "missing-metric".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'no metrics here\\n'; printf 'debug\\n' 1>&2".to_string(),
            ]),
            timeout: 600,
            adaptor: autotune_config::AdaptorConfig::Regex {
                patterns: vec![autotune_config::RegexPattern {
                    name: "metric".to_string(),
                    pattern: "metric: ([0-9.]+)".to_string(),
                }],
            },
        }];

        let err = validate_measure_config(&measures, workdir.path()).unwrap_err();

        assert!(err.contains("metric extraction failed for measure 'missing-metric'"));
        assert!(err.contains("Measure command output (stdout):\nno metrics here"));
        assert!(err.contains("Measure command output (stderr):\ndebug"));
    }

    #[test]
    fn codex_reasoning_effort_maps_all_variants() {
        assert_eq!(codex_reasoning_effort(None), None);
        assert_eq!(
            codex_reasoning_effort(Some(autotune_config::ReasoningEffort::Low)),
            Some("low".to_string())
        );
        assert_eq!(
            codex_reasoning_effort(Some(autotune_config::ReasoningEffort::Medium)),
            Some("medium".to_string())
        );
        assert_eq!(
            codex_reasoning_effort(Some(autotune_config::ReasoningEffort::High)),
            Some("high".to_string())
        );
    }

    #[test]
    fn research_agent_session_config_uses_research_role_settings() {
        let repo = tempdir().unwrap();
        let mut config = sample_config();
        config.agent.research = Some(autotune_config::AgentRoleConfig {
            backend: Some("codex".to_string()),
            model: Some("gpt-5.4".to_string()),
            max_turns: Some(12),
            reasoning_effort: Some(autotune_config::ReasoningEffort::High),
            max_fix_attempts: None,
            max_fresh_spawns: None,
        });

        let session_config = research_agent_session_config(&config, repo.path());

        assert_eq!(session_config.prompt, "");
        assert_eq!(session_config.working_directory, repo.path());
        assert_eq!(session_config.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(session_config.max_turns, Some(12));
        assert_eq!(session_config.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(session_config.allowed_tools.len(), 3);
        assert!(matches!(
            session_config.allowed_tools.as_slice(),
            [
                ToolPermission::Allow(read),
                ToolPermission::Allow(glob),
                ToolPermission::Allow(grep),
            ] if read == "Read" && glob == "Glob" && grep == "Grep"
        ));
    }

    #[test]
    fn research_agent_session_config_leaves_optional_settings_unset() {
        let repo = tempdir().unwrap();
        let config = sample_config();

        let session_config = research_agent_session_config(&config, repo.path());

        assert_eq!(session_config.model, None);
        assert_eq!(session_config.max_turns, None);
        assert_eq!(session_config.reasoning_effort, None);
    }

    #[test]
    fn apply_resume_stop_condition_overrides_updates_only_requested_fields() {
        let mut config = sample_config();
        config.task.max_iterations = Some(autotune_config::StopValue::Infinite);
        config.task.max_duration = Some("2h".to_string());
        config.task.target_improvement = Some(0.25);

        apply_resume_stop_condition_overrides(&mut config, Some(7), None, Some(0.4));

        assert!(matches!(
            config.task.max_iterations,
            Some(autotune_config::StopValue::Finite(7))
        ));
        assert_eq!(config.task.max_duration.as_deref(), Some("2h"));
        assert_eq!(config.task.target_improvement, Some(0.4));
    }

    #[test]
    fn build_baseline_record_sets_baseline_defaults() {
        let timestamp = Utc::now();
        let metrics = HashMap::from([("line_coverage".to_string(), 72.4)]);

        let record = build_baseline_record(metrics.clone(), timestamp);

        assert_eq!(record.iteration, 0);
        assert_eq!(record.approach, "baseline");
        assert_eq!(record.status, IterationStatus::Baseline);
        assert_eq!(record.hypothesis, None);
        assert_eq!(record.metrics, metrics);
        assert_eq!(record.rank, 0.0);
        assert_eq!(record.score, None);
        assert_eq!(record.reason, None);
        assert_eq!(record.fix_attempts, 0);
        assert_eq!(record.fresh_spawns, 0);
        assert_eq!(record.timestamp, timestamp);
    }

    #[test]
    fn build_initial_task_state_seeds_planning_iteration_one() {
        let state = build_initial_task_state("coverage-task", "main", "session-123", "codex");

        assert_eq!(state.task_name, "coverage-task");
        assert_eq!(state.canonical_branch, "main");
        assert_eq!(state.advancing_branch, "autotune/coverage-task-main");
        assert_eq!(state.research_session_id, "session-123");
        assert_eq!(state.research_backend, "codex");
        assert_eq!(state.current_iteration, 1);
        assert_eq!(state.current_phase, Phase::Planning);
        assert!(state.current_approach.is_none());
    }

    #[test]
    fn completion_messages_render_run_and_resume_variants() {
        let (run_status, run_handover) =
            completion_messages("coverage-task", false, "codex resume");
        let (resume_status, resume_handover) =
            completion_messages("coverage-task", true, "codex resume");

        assert_eq!(run_status, "\n[autotune] task 'coverage-task' complete");
        assert_eq!(
            resume_status,
            "\n[autotune] task 'coverage-task' resumed and complete"
        );
        assert_eq!(
            run_handover,
            "[autotune] research agent handover: codex resume"
        );
        assert_eq!(
            resume_handover,
            "[autotune] research agent handover: codex resume"
        );
    }

    #[test]
    fn build_scorer_weighted_sum_keeps_improving_candidate() {
        let scorer = build_scorer(&sample_config());

        let output = scorer
            .calculate(&score_input(
                &[("line_coverage", 80.0), ("runtime_ms", 100.0)],
                &[("line_coverage", 88.0), ("runtime_ms", 105.0)],
            ))
            .unwrap();

        assert_eq!(output.decision, "keep");
        assert!((output.rank - 0.15).abs() < 1e-9);
        assert_eq!(output.reason, "line_coverage: 10.00%");
    }

    #[test]
    fn build_scorer_weighted_sum_discards_guardrail_regression() {
        let scorer = build_scorer(&sample_config());

        let output = scorer
            .calculate(&score_input(
                &[("line_coverage", 80.0), ("runtime_ms", 100.0)],
                &[("line_coverage", 88.0), ("runtime_ms", 111.0)],
            ))
            .unwrap();

        assert_eq!(output.decision, "discard");
        assert!((output.rank + 0.11).abs() < 1e-9);
        assert_eq!(
            output.reason,
            "guardrail 'runtime_ms' failed: regression 11.00% exceeds max 10.00%"
        );
    }

    #[test]
    fn build_scorer_weighted_sum_surfaces_missing_metrics() {
        let scorer = build_scorer(&sample_config());

        let err = scorer
            .calculate(&score_input(
                &[("line_coverage", 80.0)],
                &[("line_coverage", 88.0)],
            ))
            .unwrap_err();

        assert!(matches!(err, ScoreError::MissingMetric { ref name } if name == "runtime_ms"));
    }

    #[test]
    fn build_scorer_threshold_uses_threshold_conditions() {
        let mut config = sample_config();
        config.score = autotune_config::ScoreConfig::Threshold {
            conditions: vec![
                autotune_config::ThresholdCondition {
                    metric: "line_coverage".to_string(),
                    direction: autotune_config::Direction::Maximize,
                    threshold: 2.0,
                },
                autotune_config::ThresholdCondition {
                    metric: "runtime_ms".to_string(),
                    direction: autotune_config::Direction::Minimize,
                    threshold: 5.0,
                },
            ],
        };
        let scorer = build_scorer(&config);

        let output = scorer
            .calculate(&score_input(
                &[("line_coverage", 80.0), ("runtime_ms", 100.0)],
                &[("line_coverage", 83.5), ("runtime_ms", 93.0)],
            ))
            .unwrap();

        assert_eq!(output.decision, "keep");
        assert!((output.rank - 10.5).abs() < 1e-9);
        assert_eq!(
            output.reason,
            "line_coverage: passed (+3.5000), runtime_ms: passed (+7.0000)"
        );
    }

    #[test]
    fn build_scorer_script_and_command_delegate_to_script_scorer() {
        let output_program = r#"printf '{"rank":1.25,"decision":"keep","reason":"script ok"}'"#;

        let mut script_config = sample_config();
        script_config.score = autotune_config::ScoreConfig::Script {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                output_program.to_string(),
            ],
        };

        let mut command_config = sample_config();
        command_config.score = autotune_config::ScoreConfig::Command {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                output_program.to_string(),
            ],
        };

        let input = score_input(&[("line_coverage", 80.0)], &[("line_coverage", 88.0)]);
        let script_output = build_scorer(&script_config).calculate(&input).unwrap();
        let command_output = build_scorer(&command_config).calculate(&input).unwrap();

        assert_eq!(script_output.rank, 1.25);
        assert_eq!(script_output.decision, "keep");
        assert_eq!(script_output.reason, "script ok");
        assert_eq!(command_output.rank, 1.25);
        assert_eq!(command_output.decision, "keep");
        assert_eq!(command_output.reason, "script ok");
    }

    #[test]
    fn truncate_preserves_short_strings_and_ellipsizes_longer_ones() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    #[test]
    fn load_config_reads_autotune_toml_from_repo_root() {
        let repo = tempdir().unwrap();
        std::fs::write(
            repo.path().join(".autotune.toml"),
            r#"
[task]
name = "demo"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[measure]]
name = "bench"
command = ["echo", "metric: 1"]
adaptor = { type = "regex", patterns = [{ name = "metric", pattern = "metric: ([0-9]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric", direction = "Minimize" }]
"#,
        )
        .unwrap();

        let config = load_config(repo.path()).unwrap();

        assert_eq!(config.task.name, "demo");
        assert_eq!(config.paths.tunable, vec!["src/**"]);
        assert_eq!(config.measure.len(), 1);
    }

    #[test]
    fn load_config_surfaces_missing_file_path() {
        let repo = tempdir().unwrap();

        let err = load_config(repo.path()).unwrap_err();

        assert!(err.to_string().contains(&format!(
            "failed to load config from {}",
            repo.path().join(".autotune.toml").display()
        )));
    }

    #[test]
    fn prepare_run_task_dir_removes_incomplete_task_directory() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        write_project_config(repo.path(), "demo");

        let incomplete_dir = repo.path().join(".autotune/tasks/demo");
        std::fs::create_dir_all(&incomplete_dir).unwrap();
        std::fs::write(incomplete_dir.join("stale.txt"), "leftover").unwrap();

        let mut config = load_config(repo.path()).unwrap();
        let task_dir = prepare_run_task_dir(repo.path(), &mut config).unwrap();

        assert_eq!(config.task.name, "demo");
        assert_eq!(task_dir, incomplete_dir);
        assert!(!task_dir.exists());
    }

    #[test]
    fn prepare_run_task_dir_auto_forks_existing_task() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        write_project_config(repo.path(), "demo");
        write_task_fixture(repo.path(), "demo", Phase::Planning);
        std::fs::create_dir_all(repo.path().join(".autotune/tasks/demo-2")).unwrap();

        let mut config = load_config(repo.path()).unwrap();
        let task_dir = prepare_run_task_dir(repo.path(), &mut config).unwrap();

        assert_eq!(config.task.name, "demo-3");
        assert_eq!(task_dir, repo.path().join(".autotune/tasks/demo-3"));
    }

    #[test]
    fn cmd_report_supports_table_and_json_formats() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        write_project_config(repo.path(), "report-task");
        write_task_fixture(repo.path(), "report-task", Phase::Scoring);

        cmd_report(None, ReportFormat::Table).unwrap();
        cmd_report(Some("report-task".to_string()), ReportFormat::Json).unwrap();
    }

    #[test]
    fn cmd_list_handles_empty_and_populated_task_sets() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        write_project_config(repo.path(), "list-task");

        cmd_list().unwrap();

        write_task_fixture(repo.path(), "list-task", Phase::Planning);
        std::fs::create_dir_all(repo.path().join(".autotune/tasks/broken-task")).unwrap();

        cmd_list().unwrap();
    }

    #[test]
    fn cmd_export_writes_task_snapshot_to_json_file() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        write_project_config(repo.path(), "export-task");
        write_task_fixture(repo.path(), "export-task", Phase::Scoring);

        let output_path = repo.path().join("export.json");
        cmd_export("export-task".to_string(), output_path.display().to_string()).unwrap();

        let export: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(output_path).unwrap()).unwrap();
        assert_eq!(export["task_name"], json!("export-task"));
        assert_eq!(export["log"], json!("investigation notes\n"));
        assert_eq!(export["state"]["current_phase"], json!("scoring"));
    }

    #[test]
    fn cmd_step_rejects_unexpected_phase_before_running_machine() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        write_project_config(repo.path(), "step-task");
        write_task_fixture(repo.path(), "step-task", Phase::Scoring);

        let err = cmd_step("step-task".to_string(), Phase::Planning).unwrap_err();

        assert!(err.to_string().contains(
            "task 'step-task' is in phase Scoring, but this command requires phase Planning"
        ));
    }

    #[test]
    fn cmd_config_set_get_list_and_unset_use_overridden_user_config_path() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        let user_config = repo.path().join(".config/autotune/config.toml");
        test_support::set_user_config_path_override(user_config.clone());

        cmd_config(ConfigCommands::Set {
            key: "agent.research.model".to_string(),
            value: "gpt-5.4".to_string(),
        })
        .unwrap();
        cmd_config(ConfigCommands::Get {
            key: "agent.research.model".to_string(),
        })
        .unwrap();
        cmd_config(ConfigCommands::List).unwrap();
        cmd_config(ConfigCommands::Unset {
            key: "agent.research.model".to_string(),
        })
        .unwrap();

        let doc: toml_edit::DocumentMut = std::fs::read_to_string(user_config)
            .unwrap()
            .parse()
            .unwrap();
        assert!(doc["agent"]["research"].get("model").is_none());
    }

    #[test]
    fn cmd_config_edit_creates_template_and_uses_editor_override() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        let user_config = repo.path().join(".config/autotune/config.toml");
        test_support::set_user_config_path_override(user_config.clone());
        test_support::set_editor_override("true");

        cmd_config(ConfigCommands::Edit).unwrap();

        let content = std::fs::read_to_string(user_config).unwrap();
        assert!(content.contains("Autotune global config"));
    }

    #[test]
    fn cmd_init_writes_generated_config_and_persists_name_override() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        let mut config = sample_config();
        config.task.name = "generated-task".to_string();
        test_support::set_init_override_config(config);

        cmd_init(Some("cli-name".to_string())).unwrap();

        let written = load_config(repo.path()).unwrap();
        assert_eq!(written.task.name, "cli-name");
    }

    #[test]
    fn cmd_init_updates_existing_config_when_name_is_overridden() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        write_project_config(repo.path(), "original-name");

        cmd_init(Some("renamed-task".to_string())).unwrap();

        let written = load_config(repo.path()).unwrap();
        assert_eq!(written.task.name, "renamed-task");
    }

    #[test]
    fn cmd_init_cancelled_flow_leaves_config_absent() {
        let repo = init_git_repo();
        let _guard = CommandTestGuard::enter(repo.path());
        test_support::set_init_override_cancelled();

        cmd_init(None).unwrap();

        assert!(!repo.path().join(".autotune.toml").exists());
    }

    #[test]
    fn next_available_task_name_skips_taken_task_directories() {
        let repo = tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        std::fs::create_dir_all(repo.path().join(".autotune/tasks/demo-2")).unwrap();
        std::fs::create_dir_all(repo.path().join(".autotune/tasks/demo-3")).unwrap();

        let name = next_available_task_name(repo.path(), "demo").unwrap();

        assert_eq!(name, "demo-4");
    }
}
