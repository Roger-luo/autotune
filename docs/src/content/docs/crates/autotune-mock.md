---
title: autotune-mock
description: A dev-only configurable MockAgent that drives the autotune pipeline end-to-end in tests without real LLM calls.
section: Crates
order: 10
---

`autotune-mock` provides `MockAgent`, a configurable, dev/test-only implementation of the `autotune-agent` `Agent` trait. It lets tests exercise the full tune loop without invoking a real LLM by returning canned responses: the research agent emits `<plan>` XML fragments (built from queued hypotheses or raw verbatim strings), while implementation spawns can commit dummy changes, do nothing, run a closure, or execute staged shell scripts per turn.

## When to use it

- Strictly for tests and scenario harnesses — it is never wired into production builds.
- Use it to drive the state machine deterministically: queue hypotheses, inject raw research XML, or stage implementer behavior across turns.
- Use its tracking accessors (`spawn_count`, `send_messages`, `granted_permissions`, etc.) to assert on how the pipeline called the agent.

## Public API

- `MockAgent` — the mock `Agent`; built via `MockAgent::builder()`. Implements `Agent` (`spawn`, `send`, `backend_name` returns `"mock"`, `handover_command` returns `"mock-handover"`, `grant_session_permission`).
- `MockAgent::builder()` — returns a `MockAgentBuilder`.
- `MockAgentBuilder::hypothesis(approach, hypothesis, files_to_modify)` — queue a hypothesis the research agent returns as `<plan>` XML.
- `MockAgentBuilder::research_response(raw)` — queue a verbatim research response (arbitrary XML/text); takes precedence over `hypothesis()`.
- `MockAgentBuilder::init_response(json)` — queue a JSON response for the init conversation.
- `MockAgentBuilder::implementation_behavior(ImplBehavior)` — set what an implementation `spawn()` does.
- `MockAgentBuilder::implementation_script_entry(script)` — append one `ImplBehavior::Script` shell entry (chainable).
- `MockAgentBuilder::build()` — construct the `MockAgent`.
- `ImplBehavior` — enum: `CommitDummy`, `NoCommit`, `Custom(Box<dyn Fn(&Path) + Send + Sync>)`, `Script(Vec<String>)`.
- Tracking accessors on `MockAgent`: `spawn_count`, `send_count`, `last_spawn_config`, `last_send_message`, `spawn_configs`, `send_messages`, `granted_permissions`.
- `MOCK_RESEARCH_SESSION_ID` — stable research session id (`"mock-session-001"`).
- `MOCK_IMPL_SESSION_PREFIX` — prefix (`"mock-impl-"`) for minted implementer session ids; used to tell implementer fix turns apart from research planning turns.

## Usage

```rust
use autotune_agent::{Agent, AgentConfig, AgentSession};
use autotune_mock::{MockAgent, MOCK_RESEARCH_SESSION_ID};

#[test]
fn research_agent_returns_a_plan() {
    let agent = MockAgent::builder()
        .hypothesis("inline-hot-loop", "inlining reduces call overhead", &["src/lib.rs"])
        .build();

    // First spawn (non-worktree) initializes the research agent.
    let tmp = tempfile::tempdir().unwrap();
    let config = AgentConfig {
        prompt: "ready".to_string(),
        allowed_tools: vec![],
        working_directory: tmp.path().to_path_buf(),
        model: None,
        max_turns: None,
        reasoning_effort: None,
    };
    agent.spawn(&config).unwrap();

    let session = AgentSession {
        session_id: MOCK_RESEARCH_SESSION_ID.to_string(),
        backend: "mock".to_string(),
    };
    let resp = agent.send(&session, "give me a plan").unwrap();

    assert!(resp.text.contains("<approach>inline-hot-loop</approach>"));
    assert!(resp.text.contains("<file>src/lib.rs</file>"));
    assert_eq!(agent.send_count(), 1);
}
```

## Internal dependencies

- `autotune-agent` — provides the `Agent` trait and the `AgentConfig` / `AgentResponse` / `AgentSession` / `ToolPermission` / `AgentError` types the mock implements and returns.

## Notes

- Research responses must be valid XML fragments per the agent protocol (`<plan>`, `<approach>`, `<hypothesis>`, `<files-to-modify>`/`<file>`); the mock builds these from hypotheses with XML-escaping, and `research_response()` lets you inject raw fragments (including malformed XML or `<request-tool>`) verbatim.
- Implementation vs. research routing is heuristic: a `spawn()` is treated as an implementation turn when its working directory is a git worktree (`.git` is a *file*, not a directory). The first non-worktree spawn (or any non-worktree spawn when `research_responses` are queued) takes the research path.
- Response queues (`research_responses`, `ImplBehavior::Script`) are consumed in order across `spawn()` + `send()`; once drained, research responses repeat the last entry and script turns become no-ops (which the state machine interprets as "implementer produced no edits").
