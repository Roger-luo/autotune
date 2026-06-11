---
title: autotune-implement
description: Drives the Implementing phase — an ephemeral, sandboxed agent writes code in a git worktree with scoped Edit/Write tools.
section: Crates
order: 7
---

The `autotune-implement` crate owns the Implementing phase of the tune loop. It sets up an isolated git worktree, spawns an ephemeral implementation agent that is allowed to read freely but can only edit files within configured tunable paths (no Bash, no sub-agents, no web), and stages/commits whatever the agent produced. It also supports fix-retry turns and fresh respawns when a candidate fails its tests.

## When to use it

- You're building or extending the state-machine transition that turns a planned `Hypothesis` into a committed code change on a worktree branch.
- You need the exact tool-permission and prompt-construction logic that keeps the implementation agent sandboxed to tunable paths.
- You want to drive a fix turn (continue the same session against new test output) or a tier-2 respawn (fresh session re-injecting hypothesis + failure history) after a test failure.

## Public API

- `Hypothesis` — the tuning approach to implement: `approach`, `hypothesis`, `files_to_modify` (Serialize/Deserialize).
- `ImplementResult` — successful run output: `worktree_path`, `branch_name`, `commit_sha`, `agent_response`, `session_id`.
- `FixOutcome` — result of a fix turn: `Committed { commit_sha, session_id }` or `NoEdits { session_id }`.
- `ImplementError` — error enum: `Agent`, `Git`, and `NoCommit` (agent made no edits).
- `implementation_agent_permissions(tunable_paths)` — builds the sandboxed `Vec<ToolPermission>`: allow Read/Glob/Grep, scoped Edit/Write per path, deny Bash/Agent/WebFetch/WebSearch.
- `build_implementation_prompt(hypothesis, log_content, denied_paths)` — composes the system prompt (approach, files, rules, `SUMMARY:` expectation, prior log findings).
- `build_fix_prompt(fix_history, latest_test_output)` — terse fix-turn prompt for an existing session.
- `build_respawn_prompt(hypothesis, log_content, denied_paths, prior_commits, fix_history)` — full prompt for a fresh tier-2 respawn.
- `setup_worktree(repo_root, task_name, approach_name, worktree_parent, start_branch)` — creates branch `autotune/<task>/<slug>` and its worktree; returns `(worktree_path, branch_name)`.
- `run_implementation(agent, hypothesis, worktree_path, branch_name, tunable_paths, denied_paths, log_content, model, max_turns, reasoning_effort, event_handler)` — spawns the agent, commits its edits, returns `ImplementResult`.
- `run_fix_turn(agent, session, worktree_path, fix_history, latest_test_output, event_handler)` — continues a session with a fix prompt; returns `FixOutcome`.
- `run_fix_respawn(agent, hypothesis, worktree_path, tunable_paths, denied_paths, log_content, prior_commits, fix_history, model, max_turns, reasoning_effort, event_handler)` — fresh respawn on the same worktree; returns `FixOutcome`.

## Usage

```rust
use autotune_implement::{
    run_implementation, setup_worktree, Hypothesis, ImplementResult,
};

fn implement(
    agent: &dyn autotune_agent::Agent,
    repo_root: &std::path::Path,
    worktree_parent: &std::path::Path,
) -> Result<ImplementResult, autotune_implement::ImplementError> {
    let hypothesis = Hypothesis {
        approach: "inline-hot-loop".to_string(),
        hypothesis: "Inlining the inner loop removes a branch in the hot path.".to_string(),
        files_to_modify: vec!["src/engine.rs".to_string()],
    };

    // Create the branch + worktree the ephemeral agent will edit in.
    let (worktree_path, branch_name) = setup_worktree(
        repo_root,
        "throughput",          // task name (namespaces the branch)
        &hypothesis.approach,  // becomes the branch slug
        worktree_parent,
        "main",                // start branch
    )?;

    // Spawn the sandboxed agent; it may only Edit/Write under the tunable paths.
    run_implementation(
        agent,
        &hypothesis,
        &worktree_path,
        &branch_name,
        &["src/**/*.rs".to_string()], // tunable paths
        &["target/**".to_string()],   // denied paths
        "",                            // log.md content
        Some("opus"),                  // model
        Some(40),                      // max_turns
        None,                          // reasoning_effort
        None,                          // event_handler
    )
}
```

## Internal dependencies

- `autotune-agent` — agent trait, configs, streaming spawn/send, tool permissions, and trace recording.
- `autotune-git` — branch/worktree creation, commit-SHA inspection, and staged commits.

## Notes

The agent prompt instructs the implementer not to commit; the crate detects new commits by comparing SHAs and otherwise stages all uncommitted changes itself, deriving the commit subject from the agent's `SUMMARY:` line (falling back to the approach name). A run that produces neither a commit nor uncommitted changes returns `ImplementError::NoCommit`; a fix turn in that situation returns `FixOutcome::NoEdits`, which is the caller's signal to respawn a fresh session. Tunable globs are rewritten to absolute paths anchored at the worktree before being passed to the agent, because the Claude CLI matches Edit/Write scopes against absolute paths.
