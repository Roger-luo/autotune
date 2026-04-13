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

/// Parses a `Hypothesis` from an agent response that may contain surrounding prose.
///
/// Expects a `<plan>` XML fragment with `<approach>`, `<hypothesis>`, and
/// `<files-to-modify>` children. Prose outside the fragment is ignored.
pub fn parse_hypothesis(response: &str) -> Result<Hypothesis, PlanError> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(response);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = std::str::from_utf8(e.name().as_ref())
                    .map_err(|err| PlanError::ParseHypothesis {
                        message: format!("non-utf8 tag name: {err}"),
                    })?
                    .to_string();
                if name == "plan" {
                    return parse_plan(&mut reader);
                }
                skip_element(&mut reader, &name)?;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(PlanError::ParseHypothesis {
                    message: format!("XML parse error: {e}"),
                });
            }
        }
        buf.clear();
    }

    Err(PlanError::ParseHypothesis {
        message: "no <plan> fragment found in agent response".to_string(),
    })
}

fn parse_plan(reader: &mut quick_xml::Reader<&[u8]>) -> Result<Hypothesis, PlanError> {
    use quick_xml::events::Event;

    let mut approach = String::new();
    let mut hypothesis = String::new();
    let mut files_to_modify: Vec<String> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = std::str::from_utf8(e.name().as_ref())
                    .map_err(|err| PlanError::ParseHypothesis {
                        message: format!("non-utf8 tag name: {err}"),
                    })?
                    .to_string();
                match name.as_str() {
                    "approach" => approach = read_text(reader, "approach")?,
                    "hypothesis" => hypothesis = read_text(reader, "hypothesis")?,
                    "files-to-modify" => files_to_modify = parse_files_to_modify(reader)?,
                    other => skip_element(reader, other)?,
                }
            }
            Ok(Event::End(e)) => {
                let name = std::str::from_utf8(e.name().as_ref())
                    .map_err(|err| PlanError::ParseHypothesis {
                        message: format!("non-utf8 tag name: {err}"),
                    })?
                    .to_string();
                if name == "plan" {
                    break;
                }
                return Err(PlanError::ParseHypothesis {
                    message: format!("unexpected closing tag </{name}> while in <plan>"),
                });
            }
            Ok(Event::Eof) => {
                return Err(PlanError::ParseHypothesis {
                    message: "unexpected EOF inside <plan>".to_string(),
                });
            }
            Ok(_) => {}
            Err(e) => {
                return Err(PlanError::ParseHypothesis {
                    message: format!("XML parse error inside <plan>: {e}"),
                });
            }
        }
        buf.clear();
    }

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

fn parse_files_to_modify(reader: &mut quick_xml::Reader<&[u8]>) -> Result<Vec<String>, PlanError> {
    use quick_xml::events::Event;

    let mut files: Vec<String> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = std::str::from_utf8(e.name().as_ref())
                    .map_err(|err| PlanError::ParseHypothesis {
                        message: format!("non-utf8 tag name: {err}"),
                    })?
                    .to_string();
                if name == "file" {
                    files.push(read_text(reader, "file")?);
                } else {
                    skip_element(reader, &name)?;
                }
            }
            Ok(Event::End(e)) => {
                let name = std::str::from_utf8(e.name().as_ref())
                    .map_err(|err| PlanError::ParseHypothesis {
                        message: format!("non-utf8 tag name: {err}"),
                    })?
                    .to_string();
                if name == "files-to-modify" {
                    break;
                }
                return Err(PlanError::ParseHypothesis {
                    message: format!("unexpected closing tag </{name}> while in <files-to-modify>"),
                });
            }
            Ok(Event::Eof) => {
                return Err(PlanError::ParseHypothesis {
                    message: "unexpected EOF inside <files-to-modify>".to_string(),
                });
            }
            Ok(_) => {}
            Err(e) => {
                return Err(PlanError::ParseHypothesis {
                    message: format!("XML parse error inside <files-to-modify>: {e}"),
                });
            }
        }
        buf.clear();
    }

    Ok(files)
}

fn read_text(reader: &mut quick_xml::Reader<&[u8]>, tag: &str) -> Result<String, PlanError> {
    use quick_xml::events::Event;

    let mut out = String::new();
    let mut buf = Vec::new();
    let mut depth = 0i32;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(e)) => {
                let name = std::str::from_utf8(e.name().as_ref())
                    .map_err(|err| PlanError::ParseHypothesis {
                        message: format!("non-utf8 tag name: {err}"),
                    })?
                    .to_string();
                if depth == 0 {
                    if name == tag {
                        return Ok(out.trim().to_string());
                    }
                    return Err(PlanError::ParseHypothesis {
                        message: format!("unexpected closing tag </{name}> while reading <{tag}>"),
                    });
                }
                depth -= 1;
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Text(t)) => {
                let s = t.unescape().map_err(|e| PlanError::ParseHypothesis {
                    message: format!("text unescape failed in <{tag}>: {e}"),
                })?;
                out.push_str(&s);
            }
            Ok(Event::CData(c)) => {
                let s =
                    std::str::from_utf8(c.as_ref()).map_err(|e| PlanError::ParseHypothesis {
                        message: format!("CDATA utf8 error in <{tag}>: {e}"),
                    })?;
                out.push_str(s);
            }
            Ok(Event::Eof) => {
                return Err(PlanError::ParseHypothesis {
                    message: format!("unexpected EOF inside <{tag}>"),
                });
            }
            Ok(_) => {}
            Err(e) => {
                return Err(PlanError::ParseHypothesis {
                    message: format!("XML parse error inside <{tag}>: {e}"),
                });
            }
        }
        buf.clear();
    }
}

fn skip_element(reader: &mut quick_xml::Reader<&[u8]>, tag: &str) -> Result<(), PlanError> {
    use quick_xml::events::Event;

    let mut depth = 0i32;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(e)) => {
                if depth == 0 {
                    let name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|err| PlanError::ParseHypothesis {
                            message: format!("non-utf8 tag name: {err}"),
                        })?
                        .to_string();
                    if name == tag {
                        return Ok(());
                    }
                    return Err(PlanError::ParseHypothesis {
                        message: format!("unexpected closing tag </{name}> while skipping <{tag}>"),
                    });
                }
                depth -= 1;
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Eof) => {
                return Err(PlanError::ParseHypothesis {
                    message: format!("unexpected EOF while skipping <{tag}>"),
                });
            }
            Ok(_) => {}
            Err(e) => {
                return Err(PlanError::ParseHypothesis {
                    message: format!("XML parse error while skipping <{tag}>: {e}"),
                });
            }
        }
        buf.clear();
    }
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
