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

    // NOTE: the research agent session is persistent — the initial spawn
    // already told the agent the task goal, measure/scoring configuration,
    // baseline metrics, and the `<plan>` response schema. We only re-emit
    // iteration-delta info here plus a one-line task recall as cheap
    // insurance against session compaction on long runs.
    prompt.push_str(&format!(
        "# Iteration {} — plan next approach\n\nTask (recall): {}\n\n",
        iteration_count, description
    ));

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

    // Advertise raw measure output files for the LATEST iteration as on-demand
    // references. (Baseline files were already advertised in the initial spawn
    // prompt — the persistent session means the agent still knows those paths.)
    if let Some(last) = last_iteration
        && let Some(files) = collect_measure_output_files(store, last.iteration, &last.approach)
        && !files.is_empty()
    {
        prompt.push_str("# Raw Measure Output (on-demand reference)\n\n");
        prompt.push_str(
            "Full stdout/stderr captured for this iteration's measures. Scoring \
             already consumed the extracted metrics; open these only if you need \
             more detail than the headline metrics convey.\n\n",
        );
        prompt.push_str(&format!(
            "- iteration {} ({}):\n",
            last.iteration, last.approach
        ));
        for path in &files {
            prompt.push_str(&format!("  - `{}`\n", path.display()));
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

    prompt.push_str(
        "# Instructions\n\n\
         Propose the next approach. Emit a `<plan>` fragment in the schema \
         you were given at session start. If you need a tool you don't yet \
         have, emit `<request-tool>` and end your turn.\n",
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

/// Parses a `Hypothesis` from an agent response that may contain surrounding
/// prose.
///
/// Uses [`autotune_agent::protocol::lenient_find_all`] at every level — both
/// the outer `<plan>` and its children (`<approach>`, `<hypothesis>`,
/// `<files-to-modify>` → `<file>`). This makes the parser immune to
/// unescaped `<`, `&`, and other non-XML content that agents routinely embed
/// in hypothesis prose (Rust type signatures, markdown, code snippets).
pub fn parse_hypothesis(response: &str) -> Result<Hypothesis, PlanError> {
    use autotune_agent::protocol::lenient_find_all;

    let plan_inner = lenient_find_all(response, "plan")
        .first()
        .map(|m| m.inner)
        .ok_or_else(|| PlanError::ParseHypothesis {
            message: "no <plan> fragment found in agent response".to_string(),
        })?;

    let approach = lenient_find_all(plan_inner, "approach")
        .first()
        .map(|m| m.inner.trim().to_string())
        .unwrap_or_default();

    let hypothesis = lenient_find_all(plan_inner, "hypothesis")
        .first()
        .map(|m| m.inner.trim().to_string())
        .unwrap_or_default();

    let files_to_modify = lenient_find_all(plan_inner, "files-to-modify")
        .first()
        .map(|m| {
            lenient_find_all(m.inner, "file")
                .iter()
                .map(|f| f.inner.trim().to_string())
                .collect()
        })
        .unwrap_or_default();

    if approach.is_empty() {
        return Err(PlanError::ParseHypothesis {
            message: "<plan> missing <approach>".to_string(),
        });
    }
    if hypothesis.is_empty() {
        return Err(PlanError::ParseHypothesis {
            message: "<plan> missing <hypothesis>".to_string(),
        });
    }

    Ok(Hypothesis {
        approach,
        hypothesis,
        files_to_modify,
    })
}

/// Maximum number of planning attempts before bubbling the parse error up. The
/// research agent occasionally emits malformed XML (truncated fragments,
/// mismatched tags). Re-prompting with the specific parse error almost always
/// recovers; hard-failing after three tries keeps a broken model from spinning
/// forever.
pub const MAX_PLAN_ATTEMPTS: usize = 3;

/// Classify a `PlanError` as recoverable-by-retry.
///
/// `ParseHypothesis` (final `<plan>` parse) and `AgentError::ParseFailed`
/// (raised while walking tool requests over malformed XML) can be fixed by the
/// agent with a corrected response. IO / command / interrupt failures cannot.
fn is_retryable(err: &PlanError) -> bool {
    matches!(
        err,
        PlanError::ParseHypothesis { .. }
            | PlanError::Agent {
                source: AgentError::ParseFailed { .. },
            }
    )
}

/// Build the re-prompt sent to the agent after a malformed response. Embeds
/// the parser error verbatim so the model can target its fix.
fn build_plan_correction_prompt(err: &PlanError) -> String {
    format!(
        "Your previous response could not be parsed.\n\n\
         Error: {err}\n\n\
         Please respond again. The response must contain a well-formed \
         `<plan>` fragment in the schema you were given at session start:\n\n\
         <plan>\n  <approach>short-name</approach>\n  \
         <hypothesis>your hypothesis in prose</hypothesis>\n  \
         <files-to-modify>\n    <file>relative/path.rs</file>\n  \
         </files-to-modify>\n</plan>\n\n\
         Every opening tag must have a matching closing tag. If you need a \
         tool you don't yet have, emit a single `<request-tool>` fragment \
         instead of a `<plan>`. Do not emit any other XML fragments."
    )
}

/// Attempt a single round of tool-request resolution + hypothesis parse on
/// an agent response. Returns the hypothesis or a `PlanError` that the caller
/// can inspect to decide whether to retry.
fn try_resolve_plan(
    agent: &dyn Agent,
    session: &AgentSession,
    response: AgentResponse,
    event_handler: Option<&EventHandler>,
    approver: Option<&dyn ToolApprover>,
) -> Result<Hypothesis, PlanError> {
    let resolved = handle_tool_requests(agent, session, response, event_handler, approver)?;
    parse_hypothesis(&resolved.text)
}

/// Calls the agent to plan the next iteration and parses the hypothesis.
///
/// Streaming events (text, tool use) are forwarded to `event_handler` if
/// provided. Any `<request-tool>` fragments in the agent response are routed
/// through `approver` (defaulting to deny-all when `None`); the loop continues
/// until the agent produces a response free of tool requests.
///
/// If the agent emits an unparseable response — either malformed XML that
/// breaks the tool-request walk, or a `<plan>` missing required children — the
/// loop re-prompts the agent with the specific error up to
/// [`MAX_PLAN_ATTEMPTS`] times before giving up. Non-parse errors (IO, timeout,
/// interrupt) are returned immediately.
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
    let mut response: AgentResponse = agent.send_streaming(session, &prompt, event_handler)?;

    for attempt in 1..=MAX_PLAN_ATTEMPTS {
        match try_resolve_plan(agent, session, response, event_handler, approver) {
            Ok(hypothesis) => {
                autotune_agent::trace::record(
                    "plan.attempt",
                    serde_json::json!({
                        "attempt": attempt,
                        "result": "ok",
                        "approach": hypothesis.approach,
                        "files_to_modify": hypothesis.files_to_modify,
                    }),
                );
                return Ok(hypothesis);
            }
            Err(err) if attempt < MAX_PLAN_ATTEMPTS && is_retryable(&err) => {
                eprintln!(
                    "[autotune] planning response invalid (attempt {attempt}/{MAX_PLAN_ATTEMPTS}): {err} — asking agent to retry"
                );
                let correction = build_plan_correction_prompt(&err);
                autotune_agent::trace::record(
                    "plan.retry",
                    serde_json::json!({
                        "attempt": attempt,
                        "error": err.to_string(),
                        "correction_prompt": correction,
                    }),
                );
                response = agent.send_streaming(session, &correction, event_handler)?;
            }
            Err(err) => {
                autotune_agent::trace::record(
                    "plan.attempt",
                    serde_json::json!({
                        "attempt": attempt,
                        "result": "fatal",
                        "error": err.to_string(),
                    }),
                );
                return Err(err);
            }
        }
    }
    unreachable!("plan_next loop returns on every branch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hypothesis_clean_xml() {
        let xml = r#"<plan>
  <approach>inline-cache</approach>
  <hypothesis>Inlining the cache will reduce overhead</hypothesis>
  <files-to-modify>
    <file>src/cache.rs</file>
    <file>src/main.rs</file>
  </files-to-modify>
</plan>"#;
        let h = parse_hypothesis(xml).unwrap();
        assert_eq!(h.approach, "inline-cache");
        assert_eq!(h.hypothesis, "Inlining the cache will reduce overhead");
        assert_eq!(h.files_to_modify, vec!["src/cache.rs", "src/main.rs"]);
    }

    #[test]
    fn parse_hypothesis_with_surrounding_text() {
        let response = r#"Here is my analysis of the codebase.

Based on the results, I suggest the following approach:

<plan>
  <approach>loop-unroll</approach>
  <hypothesis>Unrolling the inner loop should improve throughput</hypothesis>
  <files-to-modify>
    <file>src/engine.rs</file>
  </files-to-modify>
</plan>

This should give us a 10% improvement."#;
        let h = parse_hypothesis(response).unwrap();
        assert_eq!(h.approach, "loop-unroll");
        assert_eq!(h.files_to_modify, vec!["src/engine.rs"]);
    }

    #[test]
    fn parse_hypothesis_no_plan() {
        let response = "I don't have a specific suggestion right now.";
        let err = parse_hypothesis(response).unwrap_err();
        assert!(matches!(err, PlanError::ParseHypothesis { .. }));
    }
}
