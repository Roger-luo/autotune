use autotune_agent::{Agent, AgentError, AgentResponse, AgentSession, ToolPermission};
use autotune_state::{ExperimentStore, IterationRecord, StateError};
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

/// Builds the planning prompt for the research agent.
pub fn build_planning_prompt(
    store: &ExperimentStore,
    last_iteration: Option<&IterationRecord>,
    iteration_count: usize,
    description: &str,
) -> Result<String, PlanError> {
    let mut prompt = String::new();

    prompt.push_str("# Experiment Goal\n\n");
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

    // Include log contents
    let log = store.read_log()?;
    if !log.is_empty() {
        prompt.push_str("# Log\n\n");
        prompt.push_str(&log);
        prompt.push_str("\n\n");
    }

    prompt.push_str("# Instructions\n\n");
    prompt.push_str(
        "Based on the experiment goal and history above, propose the next approach to try.\n\
         Output your response as a JSON object with the following fields:\n\
         - \"approach\": a short name for the approach\n\
         - \"hypothesis\": what you expect this approach to achieve and why\n\
         - \"files_to_modify\": list of file paths that will need changes\n\n\
         You may include explanation before or after the JSON, but the JSON must be present.\n",
    );

    Ok(prompt)
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
pub fn plan_next(
    agent: &dyn Agent,
    session: &AgentSession,
    store: &ExperimentStore,
    last_iteration: Option<&IterationRecord>,
    iteration_count: usize,
    description: &str,
) -> Result<Hypothesis, PlanError> {
    let prompt = build_planning_prompt(store, last_iteration, iteration_count, description)?;
    let response: AgentResponse = agent.send(session, &prompt)?;
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
