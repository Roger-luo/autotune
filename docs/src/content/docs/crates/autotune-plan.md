---
title: autotune-plan
description: Drives the Planning phase — a persistent research agent proposes the next tuning hypothesis via plan_next().
section: Crates
order: 11
---

`autotune-plan` implements the Planning phase of the tune loop. It builds an iteration-aware prompt from task history, calls a persistent research agent, resolves any runtime tool requests, and parses a structured `Hypothesis` out of the agent's `<plan>` fragment — retrying on malformed responses.

## When to use it

- Reach for this crate when you need the CLI to ask the research agent "what should we try next?" and get back a typed hypothesis.
- It is the layer that turns ledger/log history plus the last iteration's results into a planning prompt, and the agent's reply back into a `Hypothesis { approach, hypothesis, files_to_modify }`.
- It also owns the read-only tool policy for the research role (Read/Glob/Grep allowed; Edit/Write/Agent permanently denied).

## Public API

- `plan_next(...)` — Prompts the agent for the next iteration and returns a parsed `Hypothesis`; routes tool requests through an approver and retries malformed responses up to `MAX_PLAN_ATTEMPTS`.
- `build_planning_prompt(store, last_iteration, iteration_count, description)` — Assembles the iteration-delta prompt from the last iteration, ledger history, raw measure-output references, and the durable log.
- `parse_hypothesis(response)` — Leniently extracts a `Hypothesis` from a `<plan>` fragment embedded in arbitrary prose; errors if `<approach>` or `<hypothesis>` is missing.
- `handle_tool_requests(agent, session, response, event_handler, approver)` — Walks `<request-tool>` fragments, grants approved permissions to the session, and re-sends until no requests remain (deny-all when `approver` is `None`).
- `Hypothesis` — The planning result: `approach: String`, `hypothesis: String`, `files_to_modify: Vec<String>`.
- `PlanError` — Error enum: `Agent`, `ParseHypothesis`, `State`.
- `ApprovalDecision` — `Approve` / `Deny` outcome returned by a `ToolApprover`.
- `ToolApprover` — Trait the CLI implements to approve or deny a `ToolRequest`.
- `research_agent_permissions()` — Returns the read-only `ToolPermission`s (Read, Glob, Grep) for the research agent.
- `is_denied_for_research(tool)` — Returns `true` for `Edit`, `Write`, `Agent` — tools never granted to the research role.
- `MAX_PLAN_ATTEMPTS` — Maximum planning attempts (currently `3`) before a parse error bubbles up.

## Usage

```rust
use autotune_agent::{Agent, AgentSession};
use autotune_plan::{plan_next, Hypothesis, PlanError};
use autotune_state::{IterationRecord, TaskStore};

fn plan_iteration(
    agent: &dyn Agent,
    session: &AgentSession,
    store: &TaskStore,
    last: Option<&IterationRecord>,
    iteration_count: usize,
) -> Result<Hypothesis, PlanError> {
    // No event handler, deny-all approver (None) — the research agent
    // gets only its built-in read-only tools.
    let hypothesis = plan_next(
        agent,
        session,
        store,
        last,
        iteration_count,
        "Reduce p99 request latency",
        None, // event_handler
        None, // approver: None => deny all runtime tool requests
    )?;

    println!("next approach: {}", hypothesis.approach);
    for file in &hypothesis.files_to_modify {
        println!("  will touch: {file}");
    }
    Ok(hypothesis)
}
```

## Internal dependencies

- `autotune-agent` — `Agent` trait, sessions, streaming, the XML tool/`<plan>` protocol, and trace recording.
- `autotune-state` — `TaskStore`, `IterationRecord`, and ledger/log access used to build the prompt.

## Notes

The research agent session is persistent: the initial spawn already conveyed the task goal, scoring config, baseline metrics, and `<plan>` schema, so `build_planning_prompt` re-emits only iteration-delta info plus a one-line task recall as cheap insurance against session compaction. `Edit`, `Write`, and `Agent` are hard-denied for the research role even if a `ToolApprover` would approve them — that policy lives in `is_denied_for_research` and is enforced inside `handle_tool_requests`.
