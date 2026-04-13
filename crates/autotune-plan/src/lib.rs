use autotune_agent::protocol::{ToolRequest, parse_tool_requests};
use autotune_agent::{
    Agent, AgentError, AgentResponse, AgentSession, EventHandler, ToolPermission,
};
use autotune_state::{IterationRecord, StateError, TaskStore};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("agent error: {source}")]
    Agent {
        #[from]
        source: AgentError,
    },

    #[error("failed to parse hypothesis from agent response: {message}")]
    ParseHypothesis { message: String },

    #[error("state error: {source}")]
    State {
        #[from]
        source: StateError,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hypothesis {
    pub approach: String,
    pub hypothesis: String,
    pub files_to_modify: Vec<String>,
}

/// Returns read-only tool permissions for the research agent.
pub fn research_agent_permissions() -> Vec<ToolPermission> {
    vec![
        ToolPermission::Allow("Read".to_string()),
        ToolPermission::Allow("Glob".to_string()),
        ToolPermission::Allow("Grep".to_string()),
    ]
}

/// Tools the research agent can NEVER be granted at runtime, even if the user
/// approves. The research role is read-only by design — file edits belong to
/// the implementation agent, and sub-agents open an unbounded execution path.
pub fn is_denied_for_research(tool: &str) -> bool {
    matches!(tool, "Edit" | "Write" | "Agent")
}

/// How to handle a `ToolRequest` from the research agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Grant the permission for the remainder of this task run.
    Approve,
    /// Refuse; the agent is told to proceed without it.
    Deny,
}

/// Interface the CLI implements to approve/deny runtime tool requests.
pub trait ToolApprover {
    fn approve(&self, req: &ToolRequest) -> std::io::Result<ApprovalDecision>;
}

/// Walk any `<request-tool>` fragments in the agent's response, prompt the user
/// for each, grant approved permissions to the session, and send the agent the
/// outcome. Loops until the agent's response contains no more requests.
///
/// Returns the final response (which should contain whatever structured
/// output the caller is waiting for, e.g., a hypothesis JSON).
///
/// When `approver` is `None`, all requests are denied automatically.
pub fn handle_tool_requests(
    agent: &dyn Agent,
    session: &AgentSession,
    mut response: AgentResponse,
    event_handler: Option<&EventHandler>,
    approver: Option<&dyn ToolApprover>,
) -> Result<AgentResponse, PlanError> {
    loop {
        let requests = parse_tool_requests(&response.text)?;
        if requests.is_empty() {
            return Ok(response);
        }

        let mut reply_lines: Vec<String> = Vec::new();
        for req in &requests {
            let label = format_request_label(req);

            if is_denied_for_research(&req.tool) {
                reply_lines.push(format!(
                    "DENIED (hardcoded for research role): {label} — {} is never available to the research agent.",
                    req.tool
                ));
                continue;
            }

            let decision = match approver {
                Some(a) => a.approve(req).map_err(|e| PlanError::Agent {
                    source: AgentError::Io { source: e },
                })?,
                None => ApprovalDecision::Deny,
            };

            match decision {
                ApprovalDecision::Approve => {
                    let perm = match &req.scope {
                        Some(scope) if !scope.is_empty() => {
                            ToolPermission::AllowScoped(req.tool.clone(), scope.clone())
                        }
                        _ => ToolPermission::Allow(req.tool.clone()),
                    };
                    agent.grant_session_permission(session, perm)?;
                    reply_lines.push(format!("GRANTED: {label}"));
                }
                ApprovalDecision::Deny => {
                    reply_lines.push(format!("DENIED: {label}"));
                }
            }
        }

        let reply = format!(
            "Tool-approval results:\n{}\n\nProceed with your task using whatever tools are now available. Do not re-request denied tools.",
            reply_lines.join("\n")
        );
        response = agent.send_streaming(session, &reply, event_handler)?;
    }
}

fn format_request_label(req: &ToolRequest) -> String {
    match &req.scope {
        Some(s) if !s.is_empty() => format!("{}({s})", req.tool),
        _ => req.tool.clone(),
    }
}

/// Builds the planning prompt for the research agent.
pub fn build_planning_prompt(
    store: &TaskStore,
    last_iteration: Option<&IterationRecord>,
    iteration_count: usize,
    description: &str,
) -> Result<String, PlanError> {
    let mut prompt = String::new();

    prompt.push_str("# Task Goal\n\n");
    prompt.push_str(description);
    prompt.push_str("\n\n");

    prompt.push_str(&format!("# Current Iteration: {}\n\n", iteration_count));

    if let Some(last) = last_iteration {
        prompt.push_str("# Last Iteration Results\n\n");
        prompt.push_str(&format!("- Approach: {}\n", last.approach));
        prompt.push_str(&format!("- Status: {:?}\n", last.status));
        prompt.push_str(&format!("- Rank: {}\n", last.rank));
        if let Some(ref hypothesis) = last.hypothesis {
            prompt.push_str(&format!("- Hypothesis: {}\n", hypothesis));
        }
        if let Some(ref reason) = last.reason {
            prompt.push_str(&format!("- Reason: {}\n", reason));
        }
        if !last.metrics.is_empty() {
            prompt.push_str("- Metrics:\n");
            for (key, value) in &last.metrics {
                prompt.push_str(&format!("  - {}: {}\n", key, value));
            }
        }
        prompt.push('\n');
    }

    // Include ledger history
    let ledger = store.load_ledger()?;
    if !ledger.is_empty() {
        prompt.push_str("# Ledger History\n\n");
        for record in &ledger {
            prompt.push_str(&format!(
                "- Iteration {}: approach={}, status={:?}, rank={}\n",
                record.iteration, record.approach, record.status, record.rank
            ));
        }
        prompt.push('\n');
    }

    // Advertise raw measure output files as on-demand references. The agent
    // gets to see summary metrics and ledger history up front; these files
    // (e.g. coverage reports, verbose benchmark logs) are only worth reading
    // when that summary isn't enough.
    let mut detail_sources: Vec<(String, Vec<std::path::PathBuf>)> = Vec::new();
    if let Some(files) = collect_measure_output_files(store, 0, "baseline")
        && !files.is_empty()
    {
        detail_sources.push(("baseline".to_string(), files));
    }
    if let Some(last) = last_iteration
        && let Some(files) = collect_measure_output_files(store, last.iteration, &last.approach)
        && !files.is_empty()
    {
        detail_sources.push((
            format!("iteration {} ({})", last.iteration, last.approach),
            files,
        ));
    }
    if !detail_sources.is_empty() {
        prompt.push_str("# Raw Measure Output (on-demand reference)\n\n");
        prompt.push_str(
            "The files below contain the full stdout/stderr captured when each measure \
             ran. They are NOT part of the metric values above — scoring already used \
             whatever the adaptor extracted. Only open a file here if the headline \
             metrics and ledger history leave you unable to form a concrete hypothesis \
             (e.g. you need to see which code paths a coverage report flagged, or which \
             benchmark case regressed). Most iterations should not need these.\n\n",
        );
        for (label, files) in &detail_sources {
            prompt.push_str(&format!("- {}:\n", label));
            for path in files {
                prompt.push_str(&format!("  - `{}`\n", path.display()));
            }
        }
        prompt.push('\n');
    }

    // Include log contents
    let log = store.read_log()?;
    if !log.is_empty() {
        prompt.push_str("# Log\n\n");
        prompt.push_str(&log);
        prompt.push_str("\n\n");
    }

    prompt.push_str("# Instructions\n\n");
    prompt.push_str(
        "Based on the task goal and history above, propose the next approach to try.\n\
         Output your response as a JSON object with the following fields:\n\
         - \"approach\": a short name for the approach\n\
         - \"hypothesis\": what you expect this approach to achieve and why\n\
         - \"files_to_modify\": list of file paths that will need changes\n\n\
         You may include explanation before or after the JSON, but the JSON must be present.\n",
    );

    Ok(prompt)
}

/// List raw measure-output files (stdout/stderr) saved for a given iteration,
/// sorted for stable prompt output. Returns `None` if the directory does not
/// exist, which keeps the planning prompt terse when nothing was captured.
fn collect_measure_output_files(
    store: &TaskStore,
    iteration: usize,
    approach: &str,
) -> Option<Vec<std::path::PathBuf>> {
    let dir = store.measure_output_dir(iteration, approach);
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    files.sort();
    Some(files)
}

/// Parses a `Hypothesis` from an agent response that may contain surrounding prose.
pub fn parse_hypothesis(response: &str) -> Result<Hypothesis, PlanError> {
    // Try to find JSON object in the response
    // Look for the outermost { ... } that parses as a valid Hypothesis
    let mut depth = 0i32;
    let mut start = None;

    for (i, ch) in response.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        let candidate = &response[s..=i];
                        if let Ok(hypothesis) = serde_json::from_str::<Hypothesis>(candidate) {
                            return Ok(hypothesis);
                        }
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }

    Err(PlanError::ParseHypothesis {
        message: "no valid JSON hypothesis found in agent response".to_string(),
    })
}

/// Calls the agent to plan the next iteration and parses the hypothesis.
///
/// Streaming events (text, tool use) are forwarded to `event_handler` if
/// provided. Any `<request-tool>` fragments in the agent response are routed
/// through `approver` (defaulting to deny-all when `None`); the loop continues
/// until the agent produces a response free of tool requests.
#[allow(clippy::too_many_arguments)]
pub fn plan_next(
    agent: &dyn Agent,
    session: &AgentSession,
    store: &TaskStore,
    last_iteration: Option<&IterationRecord>,
    iteration_count: usize,
    description: &str,
    event_handler: Option<&EventHandler>,
    approver: Option<&dyn ToolApprover>,
) -> Result<Hypothesis, PlanError> {
    let prompt = build_planning_prompt(store, last_iteration, iteration_count, description)?;
    let response: AgentResponse = agent.send_streaming(session, &prompt, event_handler)?;
    let response = handle_tool_requests(agent, session, response, event_handler, approver)?;
    parse_hypothesis(&response.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hypothesis_clean_json() {
        let json = r#"{"approach": "inline-cache", "hypothesis": "Inlining the cache will reduce overhead", "files_to_modify": ["src/cache.rs", "src/main.rs"]}"#;
        let h = parse_hypothesis(json).unwrap();
        assert_eq!(h.approach, "inline-cache");
        assert_eq!(h.hypothesis, "Inlining the cache will reduce overhead");
        assert_eq!(h.files_to_modify, vec!["src/cache.rs", "src/main.rs"]);
    }

    #[test]
    fn parse_hypothesis_with_surrounding_text() {
        let response = r#"Here is my analysis of the codebase.

Based on the results, I suggest the following approach:

{"approach": "loop-unroll", "hypothesis": "Unrolling the inner loop should improve throughput", "files_to_modify": ["src/engine.rs"]}

This should give us a 10% improvement."#;
        let h = parse_hypothesis(response).unwrap();
        assert_eq!(h.approach, "loop-unroll");
        assert_eq!(h.files_to_modify, vec!["src/engine.rs"]);
    }

    #[test]
    fn parse_hypothesis_no_json() {
        let response = "I don't have a specific suggestion right now.";
        let err = parse_hypothesis(response).unwrap_err();
        assert!(matches!(err, PlanError::ParseHypothesis { .. }));
    }
}
