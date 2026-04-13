mod error;
pub mod input;
mod prompt;
mod select;

pub use error::InitError;
pub use input::{MockInput, TerminalInput, UserInput};
pub use prompt::build_init_prompt;

use autotune_agent::protocol::{AgentRequest, ConfigSection, parse_agent_request};
use autotune_agent::{
    Agent, AgentConfig, AgentConfigWithEvents, AgentEvent, AgentSession, EventHandler,
    ToolPermission,
};
use autotune_config::global::GlobalConfig;
use autotune_config::{
    AutotuneConfig, MeasureConfig, PathsConfig, ScoreConfig, TaskConfig, TestConfig,
};

use std::collections::HashMap;
use std::path::Path;

/// Maximum conversation turns before giving up.
const MAX_TURNS: usize = 50;

/// Callback to validate a proposed config before finalizing.
///
/// Called after the user approves the assembled config. Typically runs the
/// measure commands and tries metric extraction. Returns extracted metrics
/// on success, or a detailed error string (including measure output) on failure.
pub type ConfigValidator = dyn Fn(&AutotuneConfig) -> Result<HashMap<String, f64>, String>;

/// Accumulated config sections during the init conversation.
#[derive(Clone, Default)]
struct ConfigAccumulator {
    task: Option<TaskConfig>,
    paths: Option<PathsConfig>,
    tests: Vec<TestConfig>,
    measures: Vec<MeasureConfig>,
    score: Option<ScoreConfig>,
    agent: Option<autotune_config::AgentConfig>,
}

impl ConfigAccumulator {
    fn is_complete(&self) -> bool {
        self.task.is_some()
            && self.paths.is_some()
            && !self.measures.is_empty()
            && self.score.is_some()
    }

    /// Render a TOML preview of the current accumulated config for user approval.
    fn assemble_preview(&self) -> String {
        if let Some(config) = self.clone_assemble() {
            toml::to_string_pretty(&config)
                .unwrap_or_else(|_| "failed to render preview".to_string())
        } else {
            "incomplete config".to_string()
        }
    }

    fn clone_assemble(&self) -> Option<AutotuneConfig> {
        Some(AutotuneConfig {
            task: self.task.clone()?,
            paths: self.paths.clone()?,
            test: self.tests.clone(),
            measure: if self.measures.is_empty() {
                return None;
            } else {
                self.measures.clone()
            },
            score: self.score.clone()?,
            agent: self.agent.clone().unwrap_or_default(),
        })
    }

    fn missing_sections(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.task.is_none() {
            missing.push("task");
        }
        if self.paths.is_none() {
            missing.push("paths");
        }
        if self.measures.is_empty() {
            missing.push("measure (at least one)");
        }
        if self.score.is_none() {
            missing.push("score");
        }
        missing
    }

    /// Try to assemble a complete AutotuneConfig. Returns None if required sections are missing.
    fn assemble(self) -> Option<AutotuneConfig> {
        let task = self.task?;
        let paths = self.paths?;
        if self.measures.is_empty() {
            return None;
        }
        let score = self.score?;
        let agent = self.agent.unwrap_or_default();

        Some(AutotuneConfig {
            task,
            paths,
            test: self.tests,
            measure: self.measures,
            score,
            agent,
        })
    }
}

/// Validate a single config section against the accumulator's current state.
/// Returns Ok(description) on success or Err(message) on validation failure.
fn validate_section(section: &ConfigSection, acc: &ConfigAccumulator) -> Result<String, String> {
    match section {
        ConfigSection::Task(exp) => {
            if exp.name.is_empty() {
                return Err("task name must not be empty".to_string());
            }
            if exp.max_iterations.is_none()
                && exp.target_improvement.is_none()
                && exp.max_duration.is_none()
            {
                return Err(
                    "at least one stop condition required (max_iterations, target_improvement, or max_duration)".to_string(),
                );
            }
            Ok(format!("task '{}' accepted", exp.name))
        }
        ConfigSection::Paths(paths) => {
            if paths.tunable.is_empty() {
                return Err("paths.tunable must contain at least one glob pattern".to_string());
            }
            for pattern in &paths.tunable {
                globset::Glob::new(pattern)
                    .map_err(|e| format!("invalid tunable glob '{}': {}", pattern, e))?;
            }
            for pattern in &paths.denied {
                globset::Glob::new(pattern)
                    .map_err(|e| format!("invalid denied glob '{}': {}", pattern, e))?;
            }
            Ok("paths accepted".to_string())
        }
        ConfigSection::Test(test) => {
            if test.command.is_empty() {
                return Err(format!("test '{}' has empty command", test.name));
            }
            Ok(format!("test '{}' accepted", test.name))
        }
        ConfigSection::Measure(measure) => {
            if measure.command.is_empty() {
                return Err(format!("measure '{}' has empty command", measure.name));
            }
            // Check metric name uniqueness against accumulated measures
            let new_names = adaptor_metric_names(&measure.adaptor);
            let existing_names: std::collections::HashSet<String> = acc
                .measures
                .iter()
                .flat_map(|b| adaptor_metric_names(&b.adaptor))
                .collect();
            for name in &new_names {
                if existing_names.contains(name) {
                    return Err(format!("duplicate metric name '{}' across measures", name));
                }
            }
            Ok(format!("measure '{}' accepted", measure.name))
        }
        ConfigSection::Score { value } => {
            // Validate that referenced metrics exist in accumulated measures
            let metric_names: std::collections::HashSet<String> = acc
                .measures
                .iter()
                .flat_map(|b| adaptor_metric_names(&b.adaptor))
                .collect();

            match value {
                ScoreConfig::WeightedSum {
                    primary_metrics,
                    guardrail_metrics,
                } => {
                    for pm in primary_metrics {
                        if !metric_names.contains(&pm.name) {
                            return Err(format!(
                                "primary metric '{}' not produced by any measure adaptor",
                                pm.name
                            ));
                        }
                    }
                    for gm in guardrail_metrics {
                        if !metric_names.contains(&gm.name) {
                            return Err(format!(
                                "guardrail metric '{}' not produced by any measure adaptor",
                                gm.name
                            ));
                        }
                    }
                }
                ScoreConfig::Threshold { conditions } => {
                    for c in conditions {
                        if !metric_names.contains(&c.metric) {
                            return Err(format!(
                                "threshold metric '{}' not produced by any measure adaptor",
                                c.metric
                            ));
                        }
                    }
                }
                ScoreConfig::Script { command } | ScoreConfig::Command { command } => {
                    if command.is_empty() {
                        return Err("score script/command must not be empty".to_string());
                    }
                }
            }
            Ok("score accepted".to_string())
        }
        ConfigSection::Agent(_) => Ok("agent config accepted".to_string()),
    }
}

/// Extract metric names that an adaptor config will produce.
fn adaptor_metric_names(adaptor: &autotune_config::AdaptorConfig) -> Vec<String> {
    match adaptor {
        autotune_config::AdaptorConfig::Regex { patterns } => {
            patterns.iter().map(|p| p.name.clone()).collect()
        }
        autotune_config::AdaptorConfig::Criterion { .. } => {
            vec![
                "mean".to_string(),
                "median".to_string(),
                "std_dev".to_string(),
            ]
        }
        autotune_config::AdaptorConfig::Script { .. } => vec![],
    }
}

/// Permissions for the init agent: read-only access.
fn init_agent_permissions() -> Vec<ToolPermission> {
    vec![
        ToolPermission::Allow("Read".to_string()),
        ToolPermission::Allow("Glob".to_string()),
        ToolPermission::Allow("Grep".to_string()),
    ]
}

/// Map IO errors to InitError, converting Interrupted to UserAborted.
fn map_io(e: std::io::Error) -> InitError {
    if e.kind() == std::io::ErrorKind::Interrupted {
        InitError::UserAborted
    } else {
        InitError::Io { source: e }
    }
}

/// Print a concise trial failure summary. Only the first non-empty line of the
/// error (the core issue) is shown; the full output is sent to the agent separately.
fn print_trial_failure(err: &str) {
    let summary = err
        .lines()
        .find(|l| !l.is_empty())
        .unwrap_or("unknown error");
    println!("[autotune] trial run failed: {}", summary);
}

/// Result of a successful init: the config and optional baseline metrics
/// (present when a config validator was provided and succeeded).
pub struct InitResult {
    pub config: AutotuneConfig,
    pub baseline_metrics: Option<HashMap<String, f64>>,
}

/// Run the agent-assisted init conversation.
///
/// `user_input` handles all user interaction (text prompts, option selection, approval).
/// Use `TerminalInput` for real CLI sessions or `MockInput` for testing.
///
/// If `config_validator` is provided, it is called after user approval to validate the
/// config (e.g., by running a trial measure). On failure, the user is asked whether
/// to let the agent revise the config.
pub fn run_init(
    agent: &dyn Agent,
    global_config: &GlobalConfig,
    repo_root: &Path,
    user_input: &dyn UserInput,
    config_validator: Option<&ConfigValidator>,
) -> Result<InitResult, InitError> {
    // Install a Ctrl+C handler that restores terminal state before exiting.
    // This ensures raw mode is disabled and the cursor is visible even if
    // the process is killed mid-interaction.
    let _ = ctrlc::set_handler(move || {
        restore_terminal();
        // Re-raise SIGINT with default handler to actually terminate
        // Exit cleanly with status 130 (128 + SIGINT). Using process::exit
        // instead of re-raising SIGINT ensures restore_terminal() completes
        // and stderr is flushed before the process terminates.
        std::process::exit(130);
    });

    // Run the init loop, ensuring terminal state is restored on any exit path.
    let result = run_init_inner(
        agent,
        global_config,
        repo_root,
        user_input,
        config_validator,
    );

    // Always restore terminal state
    restore_terminal();

    result
}

/// Restore terminal to a clean state: disable raw mode, show cursor, reset attributes.
fn restore_terminal() {
    use crossterm::{cursor, execute, terminal};
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        std::io::stderr(),
        cursor::Show,
        crossterm::style::SetAttribute(crossterm::style::Attribute::Reset)
    );
    // Clear any ephemeral status line
    eprint!("\r\x1b[2K");
    let _ = std::io::Write::flush(&mut std::io::stderr());
}

fn run_init_inner(
    agent: &dyn Agent,
    global_config: &GlobalConfig,
    repo_root: &Path,
    user_input: &dyn UserInput,
    config_validator: Option<&ConfigValidator>,
) -> Result<InitResult, InitError> {
    let prompt = build_init_prompt(repo_root);

    let model = global_config
        .agent
        .as_ref()
        .and_then(|a| a.init.as_ref())
        .and_then(|i| i.model.clone());

    let max_turns = global_config
        .agent
        .as_ref()
        .and_then(|a| a.init.as_ref())
        .and_then(|i| i.max_turns);

    // Show agent info
    let model_display = model.as_deref().unwrap_or("default");
    println!(
        "[autotune] init agent: backend={}, model={}",
        agent.backend_name(),
        model_display
    );

    let agent_config = AgentConfig {
        prompt,
        allowed_tools: init_agent_permissions(),
        working_directory: repo_root.to_path_buf(),
        model,
        max_turns,
    };

    fn make_event_handler(default_status: &str, quiet: bool) -> EventHandler {
        use std::sync::Mutex;
        // Track whether we've seen any text (to know if tool line needs clearing)
        let has_tool_line = std::sync::Arc::new(Mutex::new(false));

        // Print the default status as an ephemeral tool-style line
        {
            use std::io::Write;
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "\r\x1b[2K  \x1b[2m{default_status}\x1b[0m");
            let _ = stderr.flush();
        }
        *has_tool_line.lock().unwrap() = true;

        let htl = has_tool_line.clone();
        Box::new(move |event| {
            use std::io::Write;
            let mut stderr = std::io::stderr();
            let mut has_tl = htl.lock().unwrap();
            match event {
                AgentEvent::Text(text) => {
                    if quiet {
                        return;
                    }
                    // Skip JSON protocol payloads — those are parsed separately
                    let trimmed = text.trim();
                    if trimmed.starts_with('{') || trimmed.starts_with("```") {
                        return;
                    }
                    // Clear the tool/status line if present, then print text
                    if *has_tl {
                        let _ = write!(stderr, "\r\x1b[2K");
                        *has_tl = false;
                    }
                    // Stream text as-is (append, like a typewriter)
                    let _ = write!(stderr, "{text}");
                    let _ = stderr.flush();
                }
                AgentEvent::ToolUse {
                    tool,
                    input_summary,
                } => {
                    // Only show known user-facing tools
                    if !matches!(
                        tool.as_str(),
                        "Read" | "Glob" | "Grep" | "Bash" | "Edit" | "Write"
                    ) {
                        return;
                    }
                    // Clear previous tool line, show new one (dimmed)
                    if *has_tl {
                        let _ = write!(stderr, "\r\x1b[2K");
                    } else {
                        let _ = writeln!(stderr);
                    }
                    let detail = describe_tool_use(&tool, &input_summary);
                    let _ = write!(stderr, "  \x1b[2m{detail}\x1b[0m");
                    let _ = stderr.flush();
                    *has_tl = true;
                }
            }
        })
    }

    fn describe_tool_use(tool: &str, input: &str) -> String {
        if input.is_empty() {
            format!("{tool}()")
        } else {
            let summary = if input.len() > 60 {
                format!("{}...", &input[..57])
            } else {
                input.to_string()
            };
            format!("{tool}({summary})")
        }
    }

    fn clear_status() {
        // Clear the current ephemeral status line
        eprint!("\r\x1b[2K");
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }

    // Ask the user what they want to do before spawning the agent
    let user_goal = user_input
        .prompt_text("What would you like autotune to do in this project?")
        .map_err(map_io)?;

    // Append the user's goal to the agent prompt so it has context from the start
    let agent_config = {
        let mut cfg = agent_config;
        cfg.prompt.push_str(&format!(
            "\n\n## User Goal\nThe user said: \"{}\"\n\nUse this to guide your exploration and questions. You already know what the user wants — explore the codebase to figure out how best to measure it, then propose config sections.",
            user_goal
        ));
        cfg
    };

    // Spawn the init agent with event streaming
    let config_with_events = AgentConfigWithEvents::new(agent_config)
        .with_event_handler(make_event_handler("exploring project...", false));
    let response = agent.spawn_streaming(config_with_events)?;
    clear_status();

    let session = AgentSession {
        session_id: response.session_id,
        backend: agent.backend_name().to_string(),
    };

    let mut acc = ConfigAccumulator::default();
    let mut last_response_text = response.text;
    let mut turns = 0;
    let mut validated_metrics: Option<HashMap<String, f64>> = None;

    loop {
        if turns >= MAX_TURNS {
            return Err(InitError::ProtocolFailure {
                message: format!(
                    "exceeded {} conversation turns. Still missing: {}",
                    MAX_TURNS,
                    acc.missing_sections().join(", ")
                ),
            });
        }
        turns += 1;

        let request = match parse_agent_request(&last_response_text) {
            Ok(req) => req,
            Err(_) => {
                // Retry once with corrective prompt (counts as an extra turn)
                turns += 1;
                let handler = make_event_handler("retrying...", false);
                let retry = agent.send_streaming(
                    &session,
                    "Your previous response was not valid JSON. Please respond with exactly one JSON object matching the protocol schema.",
                    Some(&handler),
                )?;
                clear_status();
                match parse_agent_request(&retry.text) {
                    Ok(req) => req,
                    Err(e) => {
                        return Err(InitError::ProtocolFailure {
                            message: format!(
                                "agent failed to produce valid JSON after retry: {}",
                                e
                            ),
                        });
                    }
                }
            }
        };

        let reply = match request {
            AgentRequest::Message { text } => user_input.prompt_text(&text).map_err(map_io)?,
            AgentRequest::Question {
                text,
                options,
                allow_free_response,
            } => {
                if options.is_empty() {
                    // No options — treat as free-form text prompt
                    user_input.prompt_text(&text).map_err(map_io)?
                } else {
                    user_input
                        .prompt_select(&text, &options, allow_free_response)
                        .map_err(map_io)?
                }
            }
            AgentRequest::Config { section } => {
                match validate_section(&section, &acc) {
                    Ok(msg) => {
                        // Accumulate the valid section
                        match section {
                            ConfigSection::Task(exp) => {
                                println!("[autotune] {}", msg);
                                acc.task = Some(exp);
                            }
                            ConfigSection::Paths(paths) => {
                                println!("[autotune] {}", msg);
                                acc.paths = Some(paths);
                            }
                            ConfigSection::Test(test) => {
                                println!("[autotune] {}", msg);
                                acc.tests.push(test);
                            }
                            ConfigSection::Measure(measure) => {
                                println!("[autotune] {}", msg);
                                acc.measures.push(measure);
                            }
                            ConfigSection::Score { value } => {
                                println!("[autotune] {}", msg);
                                acc.score = Some(value);
                            }
                            ConfigSection::Agent(agent_cfg) => {
                                println!("[autotune] {}", msg);
                                acc.agent = Some(agent_cfg);
                            }
                        }

                        // Check if we have everything
                        if acc.is_complete() {
                            // Show assembled config for final approval
                            let preview = acc.assemble_preview();
                            let display = format!(
                                "All required sections collected. Proposed config:\n\n{preview}"
                            );
                            let approved = user_input.prompt_approve(&display).map_err(map_io)?;
                            if !approved {
                                // User rejected — ask for feedback
                                let feedback = user_input
                                    .prompt_text("What would you like to change?")
                                    .map_err(map_io)?;
                                let handler = make_event_handler("revising config...", false);
                                let response = agent.send_streaming(
                                    &session,
                                    &format!(
                                        "User rejected the config with feedback: {}. Please revise the relevant sections.",
                                        feedback
                                    ),
                                    Some(&handler),
                                )?;
                                clear_status();
                                last_response_text = response.text;
                                continue;
                            }

                            // Config approved — run validation if a validator is provided
                            if let Some(validator) = config_validator {
                                let trial_config = acc.clone_assemble().expect(
                                    "is_complete() was true but clone_assemble() returned None",
                                );
                                println!("[autotune] validating config — running trial run...");
                                match validator(&trial_config) {
                                    Ok(metrics) => {
                                        println!("[autotune] baseline metrics: {:?}", metrics);
                                        validated_metrics = Some(metrics);
                                        break;
                                    }
                                    Err(err) => {
                                        print_trial_failure(&err);
                                        let retry = user_input
                                            .prompt_approve("Let the agent revise the config?")
                                            .map_err(map_io)?;
                                        if !retry {
                                            return Err(InitError::UserAborted);
                                        }
                                        // Clear measure and score so agent can re-propose them
                                        acc.measures.clear();
                                        acc.score = None;
                                        let handler =
                                            make_event_handler("revising config...", true);
                                        let response = agent.send_streaming(
                                            &session,
                                            &format!(
                                                "The trial measure validation failed. The measure command ran but metric extraction did not work. Here is the error:\n\n{}\n\nPlease re-propose the measure and score sections with corrected config. You need to re-propose both sections.",
                                                err
                                            ),
                                            Some(&handler),
                                        )?;
                                        clear_status();
                                        last_response_text = response.text;
                                        continue;
                                    }
                                }
                            } else {
                                break;
                            }
                        }

                        let missing = acc.missing_sections();
                        format!(
                            "Section accepted. Still needed: {}. Please propose the next section.",
                            missing.join(", ")
                        )
                    }
                    Err(err) => {
                        println!("[autotune] validation error: {}", err);
                        format!("Validation error: {}. Please correct and re-propose.", err)
                    }
                }
            }
        };

        let handler = make_event_handler("thinking...", false);
        let response = agent.send_streaming(&session, &reply, Some(&handler))?;
        clear_status();
        last_response_text = response.text;
    }

    // Assemble and validate the full config
    let config = acc
        .assemble()
        .expect("is_complete() was true but assemble() returned None");

    // Run full validation as a final check
    config
        .validate()
        .map_err(|e| InitError::Config { source: e })?;

    Ok(InitResult {
        config,
        baseline_metrics: validated_metrics,
    })
}
