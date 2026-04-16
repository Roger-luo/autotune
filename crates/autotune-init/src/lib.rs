mod error;
pub mod input;
mod prompt;

pub use error::InitError;
pub use input::{MockInput, TerminalInput, UserInput};
pub use prompt::build_init_prompt;

use autotune_agent::protocol::{AgentFragment, parse_agent_response};
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

/// Validation outcome for a single fragment.
enum FragmentOutcome {
    Accepted(String),
    Rejected(String),
}

fn validate_task(task: &TaskConfig) -> FragmentOutcome {
    if task.name.is_empty() {
        return FragmentOutcome::Rejected("task name must not be empty".to_string());
    }
    if task.max_iterations.is_none()
        && task.target_improvement.is_none()
        && task.max_duration.is_none()
        && task.target_metric.is_empty()
    {
        return FragmentOutcome::Rejected(
            "at least one stop condition required (max_iterations, target_improvement, max_duration, or target_metric)".to_string(),
        );
    }
    FragmentOutcome::Accepted(format!("task '{}' accepted", task.name))
}

fn validate_paths(paths: &PathsConfig) -> FragmentOutcome {
    if paths.tunable.is_empty() {
        return FragmentOutcome::Rejected(
            "paths.tunable must contain at least one glob pattern".to_string(),
        );
    }
    for pattern in &paths.tunable {
        if let Err(e) = globset::Glob::new(pattern) {
            return FragmentOutcome::Rejected(format!("invalid tunable glob '{pattern}': {e}"));
        }
    }
    for pattern in &paths.denied {
        if let Err(e) = globset::Glob::new(pattern) {
            return FragmentOutcome::Rejected(format!("invalid denied glob '{pattern}': {e}"));
        }
    }
    FragmentOutcome::Accepted("paths accepted".to_string())
}

fn validate_test(test: &TestConfig) -> FragmentOutcome {
    if test.command.is_empty() {
        return FragmentOutcome::Rejected(format!("test '{}' has empty command", test.name));
    }
    FragmentOutcome::Accepted(format!("test '{}' accepted", test.name))
}

fn validate_measure(measure: &MeasureConfig, acc: &ConfigAccumulator) -> FragmentOutcome {
    if measure.command.is_empty() {
        return FragmentOutcome::Rejected(format!("measure '{}' has empty command", measure.name));
    }
    let new_names = adaptor_metric_names(&measure.adaptor);
    let existing_names: std::collections::HashSet<String> = acc
        .measures
        .iter()
        .flat_map(|b| adaptor_metric_names(&b.adaptor))
        .collect();
    for name in &new_names {
        if existing_names.contains(name) {
            return FragmentOutcome::Rejected(format!(
                "duplicate metric name '{name}' across measures"
            ));
        }
    }
    FragmentOutcome::Accepted(format!("measure '{}' accepted", measure.name))
}

fn validate_score(score: &ScoreConfig, acc: &ConfigAccumulator) -> FragmentOutcome {
    let metric_names: std::collections::HashSet<String> = acc
        .measures
        .iter()
        .flat_map(|b| adaptor_metric_names(&b.adaptor))
        .collect();

    match score {
        ScoreConfig::WeightedSum {
            primary_metrics,
            guardrail_metrics,
        } => {
            for pm in primary_metrics {
                if !metric_names.contains(&pm.name) {
                    return FragmentOutcome::Rejected(format!(
                        "primary metric '{}' not produced by any measure adaptor",
                        pm.name
                    ));
                }
            }
            for gm in guardrail_metrics {
                if !metric_names.contains(&gm.name) {
                    return FragmentOutcome::Rejected(format!(
                        "guardrail metric '{}' not produced by any measure adaptor",
                        gm.name
                    ));
                }
            }
        }
        ScoreConfig::Threshold { conditions } => {
            for c in conditions {
                if !metric_names.contains(&c.metric) {
                    return FragmentOutcome::Rejected(format!(
                        "threshold metric '{}' not produced by any measure adaptor",
                        c.metric
                    ));
                }
            }
        }
        ScoreConfig::Script { command } | ScoreConfig::Command { command } => {
            if command.is_empty() {
                return FragmentOutcome::Rejected(
                    "score script/command must not be empty".to_string(),
                );
            }
        }
    }
    FragmentOutcome::Accepted("score accepted".to_string())
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

/// Detect whether a streaming text chunk begins a known protocol tag.
/// This is intentionally narrow to avoid hiding legitimate prose.
fn is_protocol_tag_start(s: &str) -> bool {
    const TAGS: &[&str] = &[
        "<message",
        "<question",
        "<task",
        "<paths",
        "<test",
        "<measure",
        "<score",
        "<agent",
    ];
    TAGS.iter().any(|t| s.starts_with(t))
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

fn format_validation_retry(rejection_lines: &[String], acc: &ConfigAccumulator) -> String {
    let still_missing = acc.missing_sections().join(", ");
    format!(
        "Validation errors:\n{}\nPlease correct and re-emit the affected fragments. Still needed: {}.",
        rejection_lines
            .iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        still_missing
    )
}

fn format_completion_ack() -> String {
    "All required sections accepted. If you want to add optional sections (test, agent), emit them now; otherwise await the preview.".to_string()
}

fn format_progress_ack(ack_lines: &[String], missing_sections: &[&str]) -> String {
    format!(
        "Accepted: {}. Still needed: {}. Please emit the next fragment(s).",
        ack_lines.join(", "),
        missing_sections.join(", ")
    )
}

fn build_next_turn_reply(
    rejection_lines: &[String],
    user_reply: Option<String>,
    ack_lines: &[String],
    acc: &ConfigAccumulator,
) -> String {
    if !rejection_lines.is_empty() {
        format_validation_retry(rejection_lines, acc)
    } else if let Some(reply) = user_reply {
        reply
    } else {
        let missing = acc.missing_sections();
        if missing.is_empty() {
            format_completion_ack()
        } else {
            format_progress_ack(ack_lines, &missing)
        }
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

/// Restore terminal to a clean state: disable raw mode, show cursor, reset
/// attributes, plus the full CSI sequence set handled by
/// `autotune_agent::terminal::restore` (kitty keyboard protocol, bracketed
/// paste, mouse reporting).
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
    // Emit the common CSI mode-restore sequences (kitty keyboard protocol,
    // bracketed paste, mouse reporting) handled by autotune-agent.
    autotune_agent::terminal::restore();
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
    let reasoning_effort = global_config
        .agent
        .as_ref()
        .and_then(|a| a.init.as_ref())
        .and_then(|i| i.reasoning_effort)
        .map(|effort| match effort {
            autotune_config::ReasoningEffort::Low => "low",
            autotune_config::ReasoningEffort::Medium => "medium",
            autotune_config::ReasoningEffort::High => "high",
        })
        .map(str::to_string);

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
        reasoning_effort,
    };

    fn make_event_handler(default_status: &str, quiet: bool) -> EventHandler {
        use std::sync::Mutex;
        // Track whether we've seen any text (to know if tool line needs clearing)
        let has_tool_line = std::sync::Arc::new(Mutex::new(false));
        // Once we see the start of an XML tag (protocol payload), suppress
        // all remaining text deltas for this response.
        let xml_started = std::sync::Arc::new(Mutex::new(false));

        // Print the default status as an ephemeral tool-style line
        {
            use std::io::Write;
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "\r\x1b[2K  \x1b[2m{default_status}\x1b[0m");
            let _ = stderr.flush();
        }
        *has_tool_line.lock().unwrap() = true;

        let htl = has_tool_line.clone();
        let xs = xml_started.clone();
        Box::new(move |event| {
            use std::io::Write;
            let mut stderr = std::io::stderr();
            let mut has_tl = htl.lock().unwrap();
            match event {
                AgentEvent::Text(text) => {
                    if quiet {
                        return;
                    }
                    let mut xml_flag = xs.lock().unwrap();
                    // Once we've entered the XML region, suppress everything
                    if *xml_flag {
                        return;
                    }
                    // Detect start of XML protocol payload (any known top-level tag)
                    let trimmed = text.trim_start();
                    if is_protocol_tag_start(trimmed) {
                        *xml_flag = true;
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

    'outer: loop {
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

        // Parse all fragments emitted by the agent this turn.
        let fragments = match parse_agent_response(&last_response_text) {
            Ok(fs) => fs,
            Err(e) => {
                // Malformed XML — retry once with a corrective prompt.
                turns += 1;
                let handler = make_event_handler("retrying...", false);
                let retry = agent.send_streaming(
                    &session,
                    &format!(
                        "Your previous response contained malformed XML: {e}. Please respond with well-formed XML tags matching the protocol schema. Use <![CDATA[...]]> for free-text fields containing `<` or `&`.",
                    ),
                    Some(&handler),
                )?;
                clear_status();
                match parse_agent_response(&retry.text) {
                    Ok(fs) => fs,
                    Err(e) => {
                        return Err(InitError::ProtocolFailure {
                            message: format!(
                                "agent failed to produce well-formed XML after retry: {e}"
                            ),
                        });
                    }
                }
            }
        };

        if fragments.is_empty() {
            // No tags at all — remind the agent and retry.
            let reply = "No XML tags found in your response. Please emit one or more top-level tags (<message>, <question>, <task>, <paths>, <test>, <measure>, <score>, <agent>) as described in the protocol.";
            let handler = make_event_handler("thinking...", false);
            let response = agent.send_streaming(&session, reply, Some(&handler))?;
            clear_status();
            last_response_text = response.text;
            continue;
        }

        // Process fragments in order. Config fragments accumulate; Message and
        // Question fragments pause for user input, which becomes the reply.
        let mut ack_lines: Vec<String> = Vec::new();
        let mut rejection_lines: Vec<String> = Vec::new();
        let mut user_reply: Option<String> = None;

        for frag in fragments {
            // If the agent mixes interactive fragments with config, we handle
            // the user interaction inline and keep accumulating configs.
            match frag {
                AgentFragment::Message(text) => {
                    let input = loop {
                        let s = user_input.prompt_text(&text).map_err(map_io)?;
                        if !s.is_empty() {
                            break s;
                        }
                        // Empty enter → re-prompt like a REPL
                    };
                    user_reply = Some(input);
                }
                AgentFragment::Question {
                    text,
                    options,
                    allow_free_response,
                } => {
                    let input = if options.is_empty() {
                        loop {
                            let s = user_input.prompt_text(&text).map_err(map_io)?;
                            if !s.is_empty() {
                                break s;
                            }
                        }
                    } else {
                        user_input
                            .prompt_select(&text, &options, allow_free_response)
                            .map_err(map_io)?
                    };
                    user_reply = Some(input);
                }
                AgentFragment::Task(task) => match validate_task(&task) {
                    FragmentOutcome::Accepted(msg) => {
                        println!("[autotune] {msg}");
                        ack_lines.push(msg);
                        acc.task = Some(task);
                    }
                    FragmentOutcome::Rejected(err) => {
                        println!("[autotune] validation error: {err}");
                        rejection_lines.push(format!("task: {err}"));
                    }
                },
                AgentFragment::Paths(paths) => match validate_paths(&paths) {
                    FragmentOutcome::Accepted(msg) => {
                        println!("[autotune] {msg}");
                        ack_lines.push(msg);
                        acc.paths = Some(paths);
                    }
                    FragmentOutcome::Rejected(err) => {
                        println!("[autotune] validation error: {err}");
                        rejection_lines.push(format!("paths: {err}"));
                    }
                },
                AgentFragment::Test(test) => match validate_test(&test) {
                    FragmentOutcome::Accepted(msg) => {
                        println!("[autotune] {msg}");
                        ack_lines.push(msg);
                        acc.tests.push(test);
                    }
                    FragmentOutcome::Rejected(err) => {
                        println!("[autotune] validation error: {err}");
                        rejection_lines.push(format!("test: {err}"));
                    }
                },
                AgentFragment::Measure(measure) => match validate_measure(&measure, &acc) {
                    FragmentOutcome::Accepted(msg) => {
                        println!("[autotune] {msg}");
                        ack_lines.push(msg);
                        acc.measures.push(measure);
                    }
                    FragmentOutcome::Rejected(err) => {
                        println!("[autotune] validation error: {err}");
                        rejection_lines.push(format!("measure: {err}"));
                    }
                },
                AgentFragment::Score(score) => match validate_score(&score, &acc) {
                    FragmentOutcome::Accepted(msg) => {
                        println!("[autotune] {msg}");
                        ack_lines.push(msg);
                        acc.score = Some(score);
                    }
                    FragmentOutcome::Rejected(err) => {
                        println!("[autotune] validation error: {err}");
                        rejection_lines.push(format!("score: {err}"));
                    }
                },
                AgentFragment::Agent(agent_cfg) => {
                    let msg = "agent config accepted".to_string();
                    println!("[autotune] {msg}");
                    ack_lines.push(msg);
                    acc.agent = Some(agent_cfg);
                }
            }
        }

        // If the config is now complete, move to approval/validation flow.
        if acc.is_complete() && rejection_lines.is_empty() {
            let preview = acc.assemble_preview();
            let display = format!("All required sections collected. Proposed config:\n\n{preview}");
            let approved = user_input.prompt_approve(&display).map_err(map_io)?;
            if !approved {
                let feedback = user_input
                    .prompt_text("What would you like to change?")
                    .map_err(map_io)?;
                let handler = make_event_handler("revising config...", false);
                let response = agent.send_streaming(
                    &session,
                    &format!(
                        "User rejected the config with feedback: {feedback}. Please revise the relevant sections by re-emitting the affected XML tags."
                    ),
                    Some(&handler),
                )?;
                clear_status();
                last_response_text = response.text;
                continue;
            }

            if let Some(validator) = config_validator {
                let trial_config = acc
                    .clone_assemble()
                    .expect("is_complete() was true but clone_assemble() returned None");
                println!("[autotune] validating config — running trial run...");
                match validator(&trial_config) {
                    Ok(metrics) => {
                        println!("[autotune] baseline metrics: {metrics:?}");
                        validated_metrics = Some(metrics);
                        break 'outer;
                    }
                    Err(err) => {
                        print_trial_failure(&err);
                        let retry = user_input
                            .prompt_approve("Let the agent revise the config?")
                            .map_err(map_io)?;
                        if !retry {
                            return Err(InitError::UserAborted);
                        }
                        acc.measures.clear();
                        acc.score = None;
                        let handler = make_event_handler("revising config...", true);
                        let response = agent.send_streaming(
                            &session,
                            &format!(
                                "The trial measure validation failed. The measure command ran but metric extraction did not work. Here is the error:\n\n{err}\n\nPlease re-emit <measure> and <score> fragments with corrected values."
                            ),
                            Some(&handler),
                        )?;
                        clear_status();
                        last_response_text = response.text;
                        continue;
                    }
                }
            } else {
                break 'outer;
            }
        }

        // Build the reply for the next turn.
        // Priority: rejection errors > user reply > ack + what's still missing.
        let reply = build_next_turn_reply(&rejection_lines, user_reply, &ack_lines, &acc);

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

#[cfg(test)]
mod tests {
    use super::*;
    use autotune_agent::ToolPermission;
    use autotune_config::{
        AdaptorConfig, Direction, GuardrailMetric, MeasureConfig, PathsConfig, PrimaryMetric,
        RegexPattern, ScoreConfig, StopValue, TaskConfig, TestConfig, ThresholdCondition,
    };
    use std::io;

    fn minimal_task() -> TaskConfig {
        TaskConfig {
            name: "my-task".to_string(),
            description: None,
            canonical_branch: "main".to_string(),
            max_iterations: Some(StopValue::Finite(10)),
            target_improvement: None,
            max_duration: None,
            target_metric: vec![],
        }
    }

    fn minimal_paths() -> PathsConfig {
        PathsConfig {
            tunable: vec!["src/**".to_string()],
            denied: vec![],
        }
    }

    fn minimal_test() -> TestConfig {
        TestConfig {
            name: "run-tests".to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
            timeout: 300,
        }
    }

    fn regex_measure(name: &str, metric_name: &str) -> MeasureConfig {
        MeasureConfig {
            name: name.to_string(),
            command: vec!["sh".to_string()],
            timeout: 600,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: metric_name.to_string(),
                    pattern: "([0-9.]+)".to_string(),
                }],
            },
        }
    }

    fn complete_accumulator() -> ConfigAccumulator {
        ConfigAccumulator {
            task: Some(minimal_task()),
            paths: Some(minimal_paths()),
            measures: vec![regex_measure("bench", "coverage")],
            score: Some(ScoreConfig::WeightedSum {
                primary_metrics: vec![PrimaryMetric {
                    name: "coverage".to_string(),
                    direction: Direction::Maximize,
                    weight: 1.0,
                }],
                guardrail_metrics: vec![],
            }),
            ..Default::default()
        }
    }

    // --- ConfigAccumulator tests ---

    #[test]
    fn accumulator_default_is_not_complete() {
        assert!(!ConfigAccumulator::default().is_complete());
    }

    #[test]
    fn accumulator_complete_when_all_sections_set() {
        assert!(complete_accumulator().is_complete());
    }

    #[test]
    fn accumulator_missing_sections_all() {
        let missing = ConfigAccumulator::default().missing_sections();
        assert!(missing.contains(&"task"));
        assert!(missing.contains(&"paths"));
        assert!(missing.contains(&"measure (at least one)"));
        assert!(missing.contains(&"score"));
    }

    #[test]
    fn accumulator_missing_sections_partial() {
        let acc = ConfigAccumulator {
            task: Some(minimal_task()),
            paths: Some(minimal_paths()),
            ..Default::default()
        };
        let missing = acc.missing_sections();
        assert!(!missing.contains(&"task"));
        assert!(!missing.contains(&"paths"));
        assert!(missing.contains(&"measure (at least one)"));
        assert!(missing.contains(&"score"));
    }

    #[test]
    fn accumulator_clone_assemble_returns_none_when_incomplete() {
        let acc = ConfigAccumulator {
            task: Some(minimal_task()),
            paths: Some(minimal_paths()),
            ..Default::default()
        };
        // no measures, no score
        assert!(acc.clone_assemble().is_none());
    }

    #[test]
    fn accumulator_clone_assemble_returns_some_when_complete() {
        let config = complete_accumulator()
            .clone_assemble()
            .expect("should be Some");
        assert_eq!(config.task.name, "my-task");
    }

    #[test]
    fn accumulator_assemble_preview_contains_task_name() {
        let preview = complete_accumulator().assemble_preview();
        assert!(preview.contains("my-task"), "preview: {preview}");
    }

    #[test]
    fn accumulator_assemble_consumes_self() {
        assert!(complete_accumulator().assemble().is_some());
    }

    // --- validate_task tests ---

    #[test]
    fn validate_task_rejects_empty_name() {
        let mut task = minimal_task();
        task.name = String::new();
        match validate_task(&task) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("empty"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_task_rejects_no_stop_conditions() {
        let task = TaskConfig {
            name: "t".to_string(),
            description: None,
            canonical_branch: "main".to_string(),
            max_iterations: None,
            target_improvement: None,
            max_duration: None,
            target_metric: vec![],
        };
        match validate_task(&task) {
            FragmentOutcome::Rejected(msg) => {
                assert!(msg.contains("stop condition"), "msg: {msg}")
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_task_accepts_with_max_iterations() {
        assert!(matches!(
            validate_task(&minimal_task()),
            FragmentOutcome::Accepted(_)
        ));
    }

    #[test]
    fn validate_task_accepts_with_target_improvement() {
        let task = TaskConfig {
            name: "t".to_string(),
            description: None,
            canonical_branch: "main".to_string(),
            max_iterations: None,
            target_improvement: Some(0.1),
            max_duration: None,
            target_metric: vec![],
        };
        assert!(matches!(validate_task(&task), FragmentOutcome::Accepted(_)));
    }

    #[test]
    fn validate_task_accepts_with_max_duration() {
        let task = TaskConfig {
            name: "t".to_string(),
            description: None,
            canonical_branch: "main".to_string(),
            max_iterations: None,
            target_improvement: None,
            max_duration: Some("30m".to_string()),
            target_metric: vec![],
        };
        assert!(matches!(validate_task(&task), FragmentOutcome::Accepted(_)));
    }

    #[test]
    fn validate_task_accepts_with_infinite_stop() {
        let task = TaskConfig {
            name: "t".to_string(),
            description: None,
            canonical_branch: "main".to_string(),
            max_iterations: Some(StopValue::Infinite),
            target_improvement: None,
            max_duration: None,
            target_metric: vec![],
        };
        assert!(matches!(validate_task(&task), FragmentOutcome::Accepted(_)));
    }

    // --- validate_paths tests ---

    #[test]
    fn validate_paths_rejects_empty_tunable() {
        let paths = PathsConfig {
            tunable: vec![],
            denied: vec![],
        };
        match validate_paths(&paths) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("tunable"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_paths_rejects_invalid_glob() {
        let paths = PathsConfig {
            tunable: vec!["[invalid".to_string()],
            denied: vec![],
        };
        match validate_paths(&paths) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("invalid"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_paths_rejects_invalid_denied_glob() {
        let paths = PathsConfig {
            tunable: vec!["src/**".to_string()],
            denied: vec!["[bad".to_string()],
        };
        match validate_paths(&paths) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("invalid"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_paths_accepts_valid_paths() {
        assert!(matches!(
            validate_paths(&minimal_paths()),
            FragmentOutcome::Accepted(_)
        ));
    }

    // --- validate_test tests ---

    #[test]
    fn validate_test_rejects_empty_command() {
        let test = TestConfig {
            name: "t".to_string(),
            command: vec![],
            timeout: 300,
        };
        match validate_test(&test) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("empty"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_test_accepts_valid() {
        assert!(matches!(
            validate_test(&minimal_test()),
            FragmentOutcome::Accepted(_)
        ));
    }

    // --- validate_measure tests ---

    #[test]
    fn validate_measure_rejects_empty_command() {
        let measure = MeasureConfig {
            name: "m".to_string(),
            command: vec![],
            timeout: 600,
            adaptor: AdaptorConfig::Script { command: vec![] },
        };
        match validate_measure(&measure, &ConfigAccumulator::default()) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("empty"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_measure_accepts_valid() {
        let measure = regex_measure("bench", "coverage");
        assert!(matches!(
            validate_measure(&measure, &ConfigAccumulator::default()),
            FragmentOutcome::Accepted(_)
        ));
    }

    #[test]
    fn validate_measure_rejects_duplicate_metric_name() {
        let mut acc = ConfigAccumulator::default();
        acc.measures.push(regex_measure("first", "coverage"));
        let duplicate = regex_measure("second", "coverage");
        match validate_measure(&duplicate, &acc) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("duplicate"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    // --- validate_score tests ---

    #[test]
    fn validate_score_accepts_weighted_sum_with_known_metric() {
        let mut acc = ConfigAccumulator::default();
        acc.measures.push(regex_measure("bench", "coverage"));
        let score = ScoreConfig::WeightedSum {
            primary_metrics: vec![PrimaryMetric {
                name: "coverage".to_string(),
                direction: Direction::Maximize,
                weight: 1.0,
            }],
            guardrail_metrics: vec![],
        };
        assert!(matches!(
            validate_score(&score, &acc),
            FragmentOutcome::Accepted(_)
        ));
    }

    #[test]
    fn validate_score_rejects_primary_metric_unknown() {
        let mut acc = ConfigAccumulator::default();
        acc.measures.push(regex_measure("bench", "coverage"));
        let score = ScoreConfig::WeightedSum {
            primary_metrics: vec![PrimaryMetric {
                name: "unknown_metric".to_string(),
                direction: Direction::Maximize,
                weight: 1.0,
            }],
            guardrail_metrics: vec![],
        };
        match validate_score(&score, &acc) {
            FragmentOutcome::Rejected(msg) => {
                assert!(msg.contains("unknown_metric"), "msg: {msg}")
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_score_rejects_guardrail_metric_unknown() {
        let mut acc = ConfigAccumulator::default();
        acc.measures.push(regex_measure("bench", "coverage"));
        let score = ScoreConfig::WeightedSum {
            primary_metrics: vec![PrimaryMetric {
                name: "coverage".to_string(),
                direction: Direction::Maximize,
                weight: 1.0,
            }],
            guardrail_metrics: vec![GuardrailMetric {
                name: "missing_metric".to_string(),
                direction: Direction::Minimize,
                max_regression: 0.05,
            }],
        };
        match validate_score(&score, &acc) {
            FragmentOutcome::Rejected(msg) => {
                assert!(msg.contains("missing_metric"), "msg: {msg}")
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_score_accepts_threshold_with_known_metric() {
        let mut acc = ConfigAccumulator::default();
        acc.measures.push(regex_measure("bench", "latency_ms"));
        let score = ScoreConfig::Threshold {
            conditions: vec![ThresholdCondition {
                metric: "latency_ms".to_string(),
                direction: Direction::Minimize,
                threshold: 5.0,
            }],
        };
        assert!(matches!(
            validate_score(&score, &acc),
            FragmentOutcome::Accepted(_)
        ));
    }

    #[test]
    fn validate_score_rejects_threshold_metric_unknown() {
        let mut acc = ConfigAccumulator::default();
        acc.measures.push(regex_measure("bench", "latency_ms"));
        let score = ScoreConfig::Threshold {
            conditions: vec![ThresholdCondition {
                metric: "unknown_threshold_metric".to_string(),
                direction: Direction::Minimize,
                threshold: 5.0,
            }],
        };
        match validate_score(&score, &acc) {
            FragmentOutcome::Rejected(msg) => {
                assert!(msg.contains("unknown_threshold_metric"), "msg: {msg}")
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_score_rejects_empty_script_command() {
        let score = ScoreConfig::Script { command: vec![] };
        match validate_score(&score, &ConfigAccumulator::default()) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("empty"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn validate_score_accepts_nonempty_script_command() {
        let score = ScoreConfig::Script {
            command: vec!["sh".to_string(), "-c".to_string(), "echo 1".to_string()],
        };
        assert!(matches!(
            validate_score(&score, &ConfigAccumulator::default()),
            FragmentOutcome::Accepted(_)
        ));
    }

    #[test]
    fn validate_score_rejects_empty_command_score() {
        let score = ScoreConfig::Command { command: vec![] };
        match validate_score(&score, &ConfigAccumulator::default()) {
            FragmentOutcome::Rejected(msg) => assert!(msg.contains("empty"), "msg: {msg}"),
            _ => panic!("expected Rejected"),
        }
    }

    // --- adaptor_metric_names tests ---

    #[test]
    fn adaptor_metric_names_regex_returns_pattern_names() {
        let adaptor = AdaptorConfig::Regex {
            patterns: vec![
                RegexPattern {
                    name: "a".to_string(),
                    pattern: "([0-9]+)".to_string(),
                },
                RegexPattern {
                    name: "b".to_string(),
                    pattern: "([0-9]+)".to_string(),
                },
            ],
        };
        let names = adaptor_metric_names(&adaptor);
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn adaptor_metric_names_criterion_returns_fixed_set() {
        let adaptor = AdaptorConfig::Criterion {
            measure_name: "bench/sort".to_string(),
        };
        let names = adaptor_metric_names(&adaptor);
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"mean".to_string()));
        assert!(names.contains(&"median".to_string()));
        assert!(names.contains(&"std_dev".to_string()));
    }

    #[test]
    fn adaptor_metric_names_script_returns_empty() {
        let adaptor = AdaptorConfig::Script {
            command: vec!["sh".to_string()],
        };
        assert!(adaptor_metric_names(&adaptor).is_empty());
    }

    // --- streaming helper tests ---

    #[test]
    fn protocol_tag_start_matches_known_tags() {
        assert!(is_protocol_tag_start("<message>Hello"));
        assert!(is_protocol_tag_start("<measure><![CDATA[x]]></measure>"));
        assert!(!is_protocol_tag_start("plain text <message"));
        assert!(!is_protocol_tag_start("<unknown>"));
    }

    #[test]
    fn describe_tool_use_handles_empty_input() {
        assert_eq!(describe_tool_use("Read", ""), "Read()");
    }

    #[test]
    fn describe_tool_use_truncates_long_input() {
        let long = "x".repeat(80);
        let described = describe_tool_use("Glob", &long);
        assert!(described.starts_with("Glob("));
        assert!(described.ends_with("...)"));
        assert!(described.len() < long.len() + "Glob()".len());
    }

    // --- next-turn reply tests ---

    #[test]
    fn build_next_turn_reply_prioritizes_validation_errors() {
        let acc = ConfigAccumulator {
            task: Some(minimal_task()),
            ..Default::default()
        };
        let reply = build_next_turn_reply(
            &[
                "task: bad".to_string(),
                "paths: also bad".to_string(),
            ],
            Some("user input".to_string()),
            &["task accepted".to_string()],
            &acc,
        );
        assert!(reply.contains("Validation errors:"));
        assert!(reply.contains("- task: bad"));
        assert!(reply.contains("Still needed: paths, measure (at least one), score."));
        assert!(!reply.contains("user input"));
    }

    #[test]
    fn build_next_turn_reply_uses_user_reply_when_no_rejections() {
        let reply = build_next_turn_reply(
            &[],
            Some("please change the timeout".to_string()),
            &["paths accepted".to_string()],
            &ConfigAccumulator::default(),
        );
        assert_eq!(reply, "please change the timeout");
    }

    #[test]
    fn build_next_turn_reply_reports_progress_when_sections_missing() {
        let acc = ConfigAccumulator {
            task: Some(minimal_task()),
            paths: Some(minimal_paths()),
            ..Default::default()
        };
        let reply = build_next_turn_reply(
            &[],
            None,
            &["task accepted".to_string(), "paths accepted".to_string()],
            &acc,
        );
        assert_eq!(
            reply,
            "Accepted: task accepted, paths accepted. Still needed: measure (at least one), score. Please emit the next fragment(s)."
        );
    }

    #[test]
    fn build_next_turn_reply_uses_completion_message_when_nothing_missing() {
        let reply = build_next_turn_reply(
            &[],
            None,
            &["score accepted".to_string()],
            &complete_accumulator(),
        );
        assert_eq!(
            reply,
            "All required sections accepted. If you want to add optional sections (test, agent), emit them now; otherwise await the preview."
        );
    }

    // --- map_io tests ---

    #[test]
    fn map_io_interrupted_becomes_user_aborted() {
        let err = io::Error::new(io::ErrorKind::Interrupted, "x");
        assert!(matches!(map_io(err), InitError::UserAborted));
    }

    #[test]
    fn map_io_other_becomes_io_variant() {
        let err = io::Error::new(io::ErrorKind::NotFound, "x");
        assert!(!matches!(map_io(err), InitError::UserAborted));
    }

    // --- init_agent_permissions tests ---

    #[test]
    fn init_agent_permissions_are_read_only() {
        let perms = init_agent_permissions();
        assert_eq!(perms.len(), 3);
        let tool_names: Vec<&str> = perms
            .iter()
            .map(|p| match p {
                ToolPermission::Allow(name) => name.as_str(),
                _ => panic!("expected only Allow permissions"),
            })
            .collect();
        assert!(tool_names.contains(&"Read"));
        assert!(tool_names.contains(&"Glob"));
        assert!(tool_names.contains(&"Grep"));
    }

    // --- print_trial_failure tests ---

    #[test]
    fn print_trial_failure_with_multiline() {
        print_trial_failure("line1\nline2");
    }

    #[test]
    fn print_trial_failure_with_empty_string() {
        print_trial_failure("");
    }
}
