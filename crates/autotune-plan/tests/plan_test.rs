use std::sync::Mutex;

use autotune_agent::{Agent, AgentError, AgentResponse, AgentSession};
use autotune_plan::{
    MAX_PLAN_ATTEMPTS, PlanError, build_planning_prompt, is_denied_for_research, parse_hypothesis,
    plan_next,
};
use autotune_state::{IterationRecord, IterationStatus, Metrics, TaskStore};
use chrono::Utc;

/// Minimal scripted `Agent` used by the retry tests. Each `send_streaming`
/// call pops the next entry from `responses` (repeating the last one if the
/// queue is drained). The spawned session is a noop — the real contract under
/// test is `plan_next`, which only calls `send_streaming`.
struct ScriptedAgent {
    responses: Mutex<std::collections::VecDeque<String>>,
    send_count: Mutex<usize>,
}

impl ScriptedAgent {
    fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(|s| s.to_string()).collect()),
            send_count: Mutex::new(0),
        }
    }

    fn send_count(&self) -> usize {
        *self.send_count.lock().unwrap()
    }
}

impl Agent for ScriptedAgent {
    fn spawn(&self, _config: &autotune_agent::AgentConfig) -> Result<AgentResponse, AgentError> {
        Ok(AgentResponse {
            text: String::new(),
            session_id: "scripted".into(),
        })
    }

    fn send(&self, _session: &AgentSession, _message: &str) -> Result<AgentResponse, AgentError> {
        *self.send_count.lock().unwrap() += 1;
        let mut q = self.responses.lock().unwrap();
        let text = if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().cloned().unwrap_or_default()
        };
        Ok(AgentResponse {
            text,
            session_id: "scripted".into(),
        })
    }

    fn backend_name(&self) -> &str {
        "scripted"
    }

    fn handover_command(&self, _session: &AgentSession) -> String {
        String::new()
    }
}

fn scripted_session() -> AgentSession {
    AgentSession {
        session_id: "scripted".into(),
        backend: "scripted".into(),
    }
}

#[test]
fn parse_hypothesis_clean_xml() {
    let xml = "<plan>\
        <approach>batch-read</approach>\
        <hypothesis>Batching reads reduces syscalls</hypothesis>\
        <files-to-modify><file>src/io.rs</file></files-to-modify>\
        </plan>";
    let h = parse_hypothesis(xml).unwrap();
    assert_eq!(h.approach, "batch-read");
    assert_eq!(h.hypothesis, "Batching reads reduces syscalls");
    assert_eq!(h.files_to_modify, vec!["src/io.rs"]);
}

#[test]
fn parse_hypothesis_with_surrounding_text() {
    let response = "After reviewing the codebase I think we should try:\n\n\
        ```xml\n\
        <plan>\n\
          <approach>prefetch</approach>\n\
          <hypothesis>Prefetching data improves latency</hypothesis>\n\
          <files-to-modify>\n\
            <file>src/fetch.rs</file>\n\
            <file>src/lib.rs</file>\n\
          </files-to-modify>\n\
        </plan>\n\
        ```\n\n\
        Let me know if you'd like me to elaborate.";
    let h = parse_hypothesis(response).unwrap();
    assert_eq!(h.approach, "prefetch");
    assert_eq!(h.files_to_modify, vec!["src/fetch.rs", "src/lib.rs"]);
}

#[test]
fn parse_hypothesis_no_plan_errors() {
    let response = "I have no suggestions at this time.";
    let err = parse_hypothesis(response).unwrap_err();
    assert!(matches!(err, PlanError::ParseHypothesis { .. }));
}

#[test]
fn research_denylist_blocks_write_tools() {
    assert!(is_denied_for_research("Edit"));
    assert!(is_denied_for_research("Write"));
    assert!(is_denied_for_research("Agent"));
}

#[test]
fn research_denylist_allows_read_and_bash() {
    assert!(!is_denied_for_research("Bash"));
    assert!(!is_denied_for_research("WebFetch"));
    assert!(!is_denied_for_research("WebSearch"));
    assert!(!is_denied_for_research("Read"));
}

#[test]
fn build_planning_prompt_includes_description() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();
    let prompt = build_planning_prompt(&store, None, 1, "Optimize compile times").unwrap();
    assert!(prompt.contains("Optimize compile times"));
}

#[test]
fn build_planning_prompt_includes_last_iteration() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();

    let mut metrics = Metrics::new();
    metrics.insert("latency_ms".to_string(), 42.0);

    let record = IterationRecord {
        iteration: 1,
        approach: "baseline".to_string(),
        status: IterationStatus::Baseline,
        hypothesis: Some("initial run".to_string()),
        metrics,
        rank: 1.0,
        score: None,
        reason: Some("first attempt".to_string()),
        timestamp: Utc::now(),
    };

    let prompt = build_planning_prompt(&store, Some(&record), 2, "Optimize compile times").unwrap();
    assert!(prompt.contains("baseline"));
    assert!(prompt.contains("initial run"));
    assert!(prompt.contains("first attempt"));
    assert!(prompt.contains("latency_ms"));
    assert!(prompt.contains("Iteration 2"));
}

#[test]
fn build_planning_prompt_includes_log_content() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();
    store
        .append_log("## Iteration 0\nBaseline established.")
        .unwrap();

    let prompt = build_planning_prompt(&store, None, 1, "Speed up tests").unwrap();
    assert!(prompt.contains("Baseline established."));
}

/// The agent's first response is missing `<hypothesis>`, so parse_hypothesis
/// returns a retryable error. The second response is well-formed and succeeds.
#[test]
fn plan_next_retries_on_malformed_xml_then_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();

    // Missing <hypothesis> — lenient parser finds the plan and approach but
    // still requires hypothesis to be non-empty.
    let bad = "<plan><approach>a</approach></plan>";
    let good = "<plan>\
                <approach>cache-warm</approach>\
                <hypothesis>warm the cache</hypothesis>\
                <files-to-modify><file>src/cache.rs</file></files-to-modify>\
                </plan>";
    let agent = ScriptedAgent::new(vec![bad, good]);
    let session = scripted_session();

    let h = plan_next(&agent, &session, &store, None, 1, "task", None, None).unwrap();
    assert_eq!(h.approach, "cache-warm");
    // Initial send + one retry.
    assert_eq!(agent.send_count(), 2);
}

/// A `<plan>` missing required children (e.g. no `<hypothesis>`) is a
/// `ParseHypothesis` error rather than an `AgentError::ParseFailed`, but both
/// should be retried.
#[test]
fn plan_next_retries_on_missing_hypothesis_children() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();

    let bad = "<plan><approach>only</approach></plan>";
    let good = "<plan>\
                <approach>fix</approach>\
                <hypothesis>tighten the loop</hypothesis>\
                <files-to-modify></files-to-modify>\
                </plan>";
    let agent = ScriptedAgent::new(vec![bad, good]);
    let session = scripted_session();

    let h = plan_next(&agent, &session, &store, None, 1, "task", None, None).unwrap();
    assert_eq!(h.approach, "fix");
    assert_eq!(agent.send_count(), 2);
}

/// After `MAX_PLAN_ATTEMPTS` consecutive bad responses, `plan_next` gives up
/// and surfaces the last parse error rather than looping forever.
#[test]
fn plan_next_gives_up_after_max_attempts() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TaskStore::new(tmp.path()).unwrap();

    let bad = "not even xml, just prose";
    let agent = ScriptedAgent::new(vec![bad]);
    let session = scripted_session();

    let err = plan_next(&agent, &session, &store, None, 1, "task", None, None).unwrap_err();
    assert!(
        matches!(err, PlanError::ParseHypothesis { .. }),
        "expected ParseHypothesis, got {err:?}"
    );
    assert_eq!(agent.send_count(), MAX_PLAN_ATTEMPTS);
}
