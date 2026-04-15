use std::path::{Path, PathBuf};

use autotune_agent::{
    Agent, AgentConfig, AgentConfigWithEvents, AgentError, AgentResponse, AgentSession,
    EventHandler, ToolPermission,
};
use autotune_git::GitError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Load project-level instructions for the implementation agent.
///
/// Checks AGENTS.md first (preferred), then CLAUDE.md as fallback.
/// Returns `None` if neither exists.
fn load_project_instructions(worktree_path: &Path) -> Option<String> {
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = worktree_path.join(name);
        if let Ok(content) = std::fs::read_to_string(&path)
            && !content.trim().is_empty()
        {
            return Some(content);
        }
    }
    None
}

/// A hypothesis for a tuning approach.
///
/// This is a local copy of the type that will eventually live in `autotune-plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub approach: String,
    pub hypothesis: String,
    pub files_to_modify: Vec<String>,
}

/// Result of a successful implementation run.
#[derive(Debug, Clone)]
pub struct ImplementResult {
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub commit_sha: String,
    pub agent_response: AgentResponse,
    /// Session id of the implementer spawn, so callers can `send_streaming`
    /// follow-up fix turns into the same context without re-paying for the
    /// prompt prefix or re-loading AGENTS.md.
    pub session_id: String,
}

/// Outcome of a single fix-retry turn (session continuation or fresh
/// respawn). Distinguishes "implementer edited files and we committed"
/// from "implementer made no edits" — the latter is the trigger for a
/// tier-2 fresh respawn or (budget permitting) a final discard.
#[derive(Debug, Clone)]
pub enum FixOutcome {
    /// New commit written on the worktree branch with the given SHA.
    Committed {
        commit_sha: String,
        session_id: String,
    },
    /// Implementer produced no edits. Session id is returned for callers
    /// that want to keep the session alive for another turn, though in
    /// practice this is the signal to clear the session and respawn.
    NoEdits { session_id: String },
}

/// Errors that can occur during implementation.
#[derive(Debug, Error)]
pub enum ImplementError {
    #[error("agent error: {source}")]
    Agent {
        #[from]
        source: AgentError,
    },

    #[error("git error: {source}")]
    Git {
        #[from]
        source: GitError,
    },

    #[error("implementation agent did not commit any changes")]
    NoCommit,
}

/// Build sandboxed tool permissions for the implementation agent.
///
/// The agent is allowed to read freely but can only edit/write to the specified
/// tunable paths. Running commands, spawning sub-agents, and web access are
/// denied.
pub fn implementation_agent_permissions(tunable_paths: &[String]) -> Vec<ToolPermission> {
    let mut perms = Vec::new();

    // Unrestricted read tools
    perms.push(ToolPermission::Allow("Read".to_string()));
    perms.push(ToolPermission::Allow("Glob".to_string()));
    perms.push(ToolPermission::Allow("Grep".to_string()));

    // Scoped write tools — one entry per tunable path
    for path in tunable_paths {
        perms.push(ToolPermission::AllowScoped(
            "Edit".to_string(),
            path.clone(),
        ));
        perms.push(ToolPermission::AllowScoped(
            "Write".to_string(),
            path.clone(),
        ));
    }

    // Deny dangerous tools
    perms.push(ToolPermission::Deny("Bash".to_string()));
    perms.push(ToolPermission::Deny("Agent".to_string()));
    perms.push(ToolPermission::Deny("WebFetch".to_string()));
    perms.push(ToolPermission::Deny("WebSearch".to_string()));

    perms
}

/// Build the system prompt sent to the implementation agent.
pub fn build_implementation_prompt(
    hypothesis: &Hypothesis,
    log_content: &str,
    denied_paths: &[String],
) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!("# Approach: {}\n\n", hypothesis.approach));
    prompt.push_str(&format!("## Hypothesis\n\n{}\n\n", hypothesis.hypothesis));

    prompt.push_str("## Files to create or modify\n\n");
    for file in &hypothesis.files_to_modify {
        prompt.push_str(&format!("- {}\n", file));
    }
    prompt.push('\n');

    prompt.push_str("## Tools\n\n");
    prompt.push_str(
        "You have these tools pre-configured — use them directly, do NOT ask for permission:\n",
    );
    prompt.push_str("- **Read, Glob, Grep** — unrestricted, use freely to explore the codebase.\n");
    prompt.push_str("- **Edit** — modify existing files (scoped to the paths above).\n");
    prompt.push_str(
        "- **Write** — create new files or overwrite existing ones (scoped to the paths above).\n",
    );
    prompt.push('\n');

    prompt.push_str("## Rules\n\n");
    prompt.push_str("- Do NOT run tests or measures.\n");
    prompt.push_str("- Do NOT try to commit — the system stages and commits your changes automatically. Bash is not available to you.\n");
    prompt.push_str("- Only create or modify the files listed above.\n");
    if !denied_paths.is_empty() {
        prompt.push_str("- Do NOT modify files matching these denied patterns:\n");
        for p in denied_paths {
            prompt.push_str(&format!("  - `{p}`\n"));
        }
    }
    prompt.push_str(
        "- Do NOT ask for permission — your tools are already configured. Just use them.\n",
    );
    prompt.push('\n');

    prompt.push_str("## Output\n\n");
    prompt.push_str("After you finish editing, end your response with a single line starting with `SUMMARY:` that describes, in one sentence, what you changed. This line becomes the commit subject. Example:\n\n");
    prompt.push_str(
        "    SUMMARY: extract hot loop in foo::bar into inlined helper to reduce branching\n\n",
    );

    if !log_content.is_empty() {
        prompt.push_str("## Prior findings from log.md\n\n");
        prompt.push_str(log_content);
        prompt.push('\n');
    }

    prompt
}

/// Turn an arbitrary approach name into a valid git branch component.
///
/// Lowercases, replaces non-alphanumeric runs with a single hyphen,
/// trims leading/trailing hyphens, and caps at 60 characters.
fn slugify(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive hyphens and trim edges.
    let mut out = String::new();
    for ch in slug.chars() {
        if ch == '-' && out.ends_with('-') {
            continue;
        }
        out.push(ch);
    }
    let out = out.trim_matches('-');
    if out.len() > 60 {
        out[..60].trim_end_matches('-').to_string()
    } else {
        out.to_string()
    }
}

/// Create a branch and worktree for the given approach.
///
/// Returns `(worktree_path, branch_name)`.
pub fn setup_worktree(
    repo_root: &Path,
    task_name: &str,
    approach_name: &str,
    worktree_parent: &Path,
    start_branch: &str,
) -> Result<(PathBuf, String), ImplementError> {
    let slug = slugify(approach_name);
    // Namespace under the task so worktree branches don't collide across
    // task forks (e.g. `autotune/test-coverage/inline-tests` vs
    // `autotune/test-coverage-2/inline-tests`).
    let branch_name = format!("autotune/{task_name}/{slug}");
    let worktree_path = worktree_parent.join(&slug);

    autotune_git::create_branch_from(repo_root, &branch_name, start_branch)?;
    autotune_git::create_worktree(repo_root, &worktree_path, &branch_name)?;

    Ok((worktree_path, branch_name))
}

/// Spawn a sandboxed implementation agent and validate it committed changes.
///
/// When `event_handler` is provided, streaming text and tool-use events are
/// forwarded to it in real time (same mechanism used by the research/init
/// agents). The implementer does not converse with the user — the handler is
/// purely for visibility.
#[allow(clippy::too_many_arguments)]
pub fn run_implementation(
    agent: &dyn Agent,
    hypothesis: &Hypothesis,
    worktree_path: &Path,
    branch_name: &str,
    tunable_paths: &[String],
    denied_paths: &[String],
    log_content: &str,
    model: Option<&str>,
    max_turns: Option<u64>,
    event_handler: Option<EventHandler>,
) -> Result<ImplementResult, ImplementError> {
    let mut prompt = String::new();

    // Load project instructions (AGENTS.md first, CLAUDE.md fallback) so the
    // implementation agent follows the same conventions as a human developer.
    // We read from the worktree since --bare skips CLAUDE.md auto-discovery.
    if let Some(instructions) = load_project_instructions(worktree_path) {
        prompt.push_str(&instructions);
        prompt.push_str("\n\n");
    }

    prompt.push_str(&build_implementation_prompt(
        hypothesis,
        log_content,
        denied_paths,
    ));

    // Resolve tunable globs to absolute paths anchored at the worktree.
    // The Claude CLI matches `--allowedTools Edit:<glob>` against the
    // absolute file paths the agent passes to Edit/Write. Relative globs
    // like `crates/**/*.rs` don't match absolute paths, so we prepend the
    // worktree root to each glob.
    let abs_tunable: Vec<String> = tunable_paths
        .iter()
        .map(|p| worktree_path.join(p).to_string_lossy().into_owned())
        .collect();
    let permissions = implementation_agent_permissions(&abs_tunable);

    autotune_agent::trace::record(
        "implement.prompt",
        serde_json::json!({
            "approach": hypothesis.approach,
            "files_to_modify": hypothesis.files_to_modify,
            "prompt": prompt,
            "worktree": worktree_path.display().to_string(),
        }),
    );

    let config = AgentConfig {
        prompt,
        allowed_tools: permissions,
        working_directory: worktree_path.to_path_buf(),
        model: model.map(String::from),
        max_turns,
    };

    // Record the commit SHA before the agent runs so we can detect new commits.
    let sha_before = autotune_git::latest_commit_sha(worktree_path)?;

    let mut config_with_events = AgentConfigWithEvents::new(config);
    if let Some(handler) = event_handler {
        config_with_events = config_with_events.with_event_handler(handler);
    }
    let response = agent.spawn_streaming(config_with_events)?;

    let sha_after = autotune_git::latest_commit_sha(worktree_path)?;

    let commit_sha = if sha_before != sha_after {
        // Agent somehow committed on its own (e.g., permissions allowed it).
        sha_after
    } else if autotune_git::has_uncommitted_changes(worktree_path)? {
        // Agent made file edits but didn't commit — the CLI owns the commit.
        let summary =
            extract_summary(&response.text).unwrap_or_else(|| hypothesis.approach.clone());
        let message = format!("autotune: {}\n\n{}", summary, response.text.trim());
        autotune_git::stage_all_and_commit(worktree_path, &message)?;
        autotune_git::latest_commit_sha(worktree_path)?
    } else {
        // No commit and no uncommitted changes — the agent made no edits.
        autotune_agent::trace::record(
            "implement.result",
            serde_json::json!({
                "approach": hypothesis.approach,
                "outcome": "no_commit",
            }),
        );
        return Err(ImplementError::NoCommit);
    };

    autotune_agent::trace::record(
        "implement.result",
        serde_json::json!({
            "approach": hypothesis.approach,
            "outcome": "committed",
            "commit_sha": commit_sha,
        }),
    );

    Ok(ImplementResult {
        worktree_path: worktree_path.to_path_buf(),
        branch_name: branch_name.to_string(),
        commit_sha,
        session_id: response.session_id.clone(),
        agent_response: response,
    })
}

/// Build the fix-turn prompt sent into an existing implementer session.
///
/// The implementer already has full context from its prior spawn — this
/// prompt only layers on the new information (latest failure plus any
/// prior failures already tried). Kept deliberately terse because the
/// implementer's context is finite.
pub fn build_fix_prompt(fix_history: &[String], latest_test_output: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str("# Fix required — tests failed after your last edit\n\n");
    prompt.push_str(
        "The tests configured for this task did not pass against your latest \
         changes. Diagnose the failure and edit the same files (or add edits \
         within the tunable paths) to make them pass. The project's scope \
         rules, tunable paths, and tool permissions are unchanged — keep using \
         Edit/Write as before, do NOT run Bash.\n\n",
    );
    if !fix_history.is_empty() {
        prompt.push_str("## Prior failure history (oldest → newest)\n\n");
        for (i, entry) in fix_history.iter().enumerate() {
            prompt.push_str(&format!("### Attempt {} failure\n\n", i + 1));
            prompt.push_str("```\n");
            prompt.push_str(entry);
            if !entry.ends_with('\n') {
                prompt.push('\n');
            }
            prompt.push_str("```\n\n");
        }
    }
    prompt.push_str("## Latest test output\n\n```\n");
    prompt.push_str(latest_test_output);
    if !latest_test_output.ends_with('\n') {
        prompt.push('\n');
    }
    prompt.push_str("```\n\n");
    prompt.push_str(
        "After editing, end your response with a line starting `SUMMARY:` \
         describing the fix in one sentence. If you genuinely cannot make \
         progress, reply with no edits and an explanation — the system will \
         respawn a fresh session.\n",
    );
    prompt
}

/// Build the prompt for a fresh respawn (tier-2). The respawned implementer
/// has no session memory, so we re-send the original hypothesis, the list of
/// commits already on the branch, and the full failure history so it can
/// decide whether to continue atop the prior work or rewrite from scratch.
pub fn build_respawn_prompt(
    hypothesis: &Hypothesis,
    log_content: &str,
    denied_paths: &[String],
    prior_commits: &[String],
    fix_history: &[String],
) -> String {
    // Reuse the normal implementation prompt as the base so AGENTS.md /
    // approach / hypothesis / rules / SUMMARY expectations are all present.
    let mut prompt = build_implementation_prompt(hypothesis, log_content, denied_paths);

    prompt.push_str("## Prior attempts on this iteration\n\n");
    if prior_commits.is_empty() {
        prompt.push_str("- (no commits yet)\n");
    } else {
        prompt.push_str(
            "An earlier implementer session already made the following commits \
             on this worktree branch — you can see the code they produced with \
             Read/Grep. The previous session went unproductive (no edits on \
             the last fix turn) so the system is respawning you with a clean \
             context. Decide whether to amend on top of the existing work or \
             rewrite it.\n\n",
        );
        for c in prior_commits {
            prompt.push_str(&format!("- {c}\n"));
        }
        prompt.push('\n');
    }

    if !fix_history.is_empty() {
        prompt.push_str("## Test-failure history (oldest → newest)\n\n");
        for (i, entry) in fix_history.iter().enumerate() {
            prompt.push_str(&format!("### Attempt {} failure\n\n", i + 1));
            prompt.push_str("```\n");
            prompt.push_str(entry);
            if !entry.ends_with('\n') {
                prompt.push('\n');
            }
            prompt.push_str("```\n\n");
        }
    }

    prompt
}

/// Continue an existing implementer session with a fix-turn prompt. If the
/// agent edits files the CLI commits them and returns
/// [`FixOutcome::Committed`]; if it produces no edits, returns
/// [`FixOutcome::NoEdits`] so the caller can fall through to a fresh
/// respawn.
pub fn run_fix_turn(
    agent: &dyn Agent,
    session: &AgentSession,
    worktree_path: &Path,
    fix_history: &[String],
    latest_test_output: &str,
    event_handler: Option<EventHandler>,
) -> Result<FixOutcome, ImplementError> {
    let prompt = build_fix_prompt(fix_history, latest_test_output);

    autotune_agent::trace::record(
        "implement.fix_turn.prompt",
        serde_json::json!({
            "session_id": session.session_id,
            "worktree": worktree_path.display().to_string(),
            "prompt": prompt,
            "history_len": fix_history.len(),
        }),
    );

    let sha_before = autotune_git::latest_commit_sha(worktree_path)?;
    let response = agent.send_streaming(session, &prompt, event_handler.as_ref())?;

    commit_if_edited(
        worktree_path,
        sha_before,
        &response,
        session.session_id.clone(),
    )
}

/// Respawn a fresh implementer on the same worktree with the hypothesis +
/// failure history re-injected. Used when a session-continuation fix turn
/// produced no edits, which we treat as "context exhausted / session stuck".
#[allow(clippy::too_many_arguments)]
pub fn run_fix_respawn(
    agent: &dyn Agent,
    hypothesis: &Hypothesis,
    worktree_path: &Path,
    tunable_paths: &[String],
    denied_paths: &[String],
    log_content: &str,
    prior_commits: &[String],
    fix_history: &[String],
    model: Option<&str>,
    max_turns: Option<u64>,
    event_handler: Option<EventHandler>,
) -> Result<FixOutcome, ImplementError> {
    let mut prompt = String::new();
    if let Some(instructions) = load_project_instructions(worktree_path) {
        prompt.push_str(&instructions);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&build_respawn_prompt(
        hypothesis,
        log_content,
        denied_paths,
        prior_commits,
        fix_history,
    ));

    let abs_tunable: Vec<String> = tunable_paths
        .iter()
        .map(|p| worktree_path.join(p).to_string_lossy().into_owned())
        .collect();
    let permissions = implementation_agent_permissions(&abs_tunable);

    autotune_agent::trace::record(
        "implement.fix_respawn.prompt",
        serde_json::json!({
            "approach": hypothesis.approach,
            "worktree": worktree_path.display().to_string(),
            "prompt": prompt,
            "prior_commits": prior_commits,
            "history_len": fix_history.len(),
        }),
    );

    let config = AgentConfig {
        prompt,
        allowed_tools: permissions,
        working_directory: worktree_path.to_path_buf(),
        model: model.map(String::from),
        max_turns,
    };

    let sha_before = autotune_git::latest_commit_sha(worktree_path)?;
    let mut config_with_events = AgentConfigWithEvents::new(config);
    if let Some(handler) = event_handler {
        config_with_events = config_with_events.with_event_handler(handler);
    }
    let response = agent.spawn_streaming(config_with_events)?;

    commit_if_edited(
        worktree_path,
        sha_before,
        &response,
        response.session_id.clone(),
    )
}

/// Shared tail for `run_fix_turn` and `run_fix_respawn`: stage+commit any
/// uncommitted edits the implementer made, else report no edits.
fn commit_if_edited(
    worktree_path: &Path,
    sha_before: String,
    response: &AgentResponse,
    session_id: String,
) -> Result<FixOutcome, ImplementError> {
    let sha_after = autotune_git::latest_commit_sha(worktree_path)?;
    if sha_before != sha_after {
        return Ok(FixOutcome::Committed {
            commit_sha: sha_after,
            session_id,
        });
    }
    if autotune_git::has_uncommitted_changes(worktree_path)? {
        let summary = extract_summary(&response.text).unwrap_or_else(|| "fix".to_string());
        let message = format!("autotune(fix): {}\n\n{}", summary, response.text.trim());
        autotune_git::stage_all_and_commit(worktree_path, &message)?;
        let commit_sha = autotune_git::latest_commit_sha(worktree_path)?;
        return Ok(FixOutcome::Committed {
            commit_sha,
            session_id,
        });
    }
    Ok(FixOutcome::NoEdits { session_id })
}

/// Extract a one-line commit subject from the agent response.
/// Looks for a line starting with `SUMMARY:`, falling back to None.
fn extract_summary(response: &str) -> Option<String> {
    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("SUMMARY:") {
            let s = rest.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_prompt_includes_latest_output_and_is_terse_without_history() {
        // With no prior history, the fix prompt must still embed the latest
        // test output verbatim — the implementer's existing session already
        // knows the hypothesis and file list.
        let prompt = build_fix_prompt(&[], "TEST FAIL: expected 42, got 41\n");
        assert!(prompt.contains("tests failed"));
        assert!(prompt.contains("TEST FAIL: expected 42, got 41"));
        assert!(prompt.contains("Latest test output"));
        assert!(!prompt.contains("Prior failure history"));
    }

    #[test]
    fn fix_prompt_lists_prior_attempts_in_order() {
        let history = vec![
            "attempt 1 failure stdout".to_string(),
            "attempt 2 failure stdout".to_string(),
        ];
        let prompt = build_fix_prompt(&history, "latest failure");
        assert!(prompt.contains("Attempt 1 failure"));
        assert!(prompt.contains("Attempt 2 failure"));
        assert!(prompt.contains("attempt 1 failure stdout"));
        assert!(prompt.contains("attempt 2 failure stdout"));
        assert!(prompt.contains("latest failure"));
        // Ordering: attempt 1 block must appear before attempt 2 block.
        assert!(prompt.find("Attempt 1").unwrap() < prompt.find("Attempt 2").unwrap());
    }

    #[test]
    fn respawn_prompt_layers_prior_commits_onto_implementation_base() {
        // A fresh spawn re-injects the full implementation prompt (so
        // AGENTS.md / approach / rules are present) then adds the
        // prior-commits section and failure history. Without prior
        // commits, we emit an explicit "(no commits yet)" marker so the
        // agent knows the slate is clean.
        let hyp = Hypothesis {
            approach: "inline-cache".to_string(),
            hypothesis: "inline the cache to cut branching".to_string(),
            files_to_modify: vec!["src/cache.rs".to_string()],
        };
        let prompt_without_commits =
            build_respawn_prompt(&hyp, "", &[], &[], &["failure A".to_string()]);
        assert!(prompt_without_commits.contains("inline-cache"));
        assert!(prompt_without_commits.contains("Prior attempts on this iteration"));
        assert!(prompt_without_commits.contains("(no commits yet)"));
        assert!(prompt_without_commits.contains("failure A"));

        let prompt_with_commits = build_respawn_prompt(
            &hyp,
            "",
            &[],
            &[
                "abc123 initial attempt".to_string(),
                "def456 fix turn 1".to_string(),
            ],
            &["failure A".to_string(), "failure B".to_string()],
        );
        assert!(prompt_with_commits.contains("abc123 initial attempt"));
        assert!(prompt_with_commits.contains("def456 fix turn 1"));
        assert!(!prompt_with_commits.contains("(no commits yet)"));
        assert!(prompt_with_commits.contains("Attempt 1 failure"));
        assert!(prompt_with_commits.contains("Attempt 2 failure"));
    }

    #[test]
    fn extract_summary_reads_first_summary_line() {
        let response = "blah\nSUMMARY: fixed the off-by-one\nmore blah";
        assert_eq!(
            extract_summary(response),
            Some("fixed the off-by-one".to_string())
        );
    }

    #[test]
    fn extract_summary_returns_none_when_absent() {
        assert_eq!(extract_summary("no summary here"), None);
    }
}
