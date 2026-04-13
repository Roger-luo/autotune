use std::path::{Path, PathBuf};

use autotune_agent::{Agent, AgentConfig, AgentError, AgentResponse, ToolPermission};
use autotune_git::GitError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
pub fn build_implementation_prompt(hypothesis: &Hypothesis, log_content: &str) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!("# Approach: {}\n\n", hypothesis.approach));
    prompt.push_str(&format!("## Hypothesis\n\n{}\n\n", hypothesis.hypothesis));

    prompt.push_str("## Files to modify\n\n");
    for file in &hypothesis.files_to_modify {
        prompt.push_str(&format!("- {}\n", file));
    }
    prompt.push('\n');

    prompt.push_str("## Rules\n\n");
    prompt.push_str("- Do NOT run tests or measures.\n");
    prompt.push_str("- Do NOT modify test files.\n");
    prompt.push_str("- Commit your changes when done.\n");
    prompt.push_str("- Only modify the files listed above.\n");
    prompt.push('\n');

    if !log_content.is_empty() {
        prompt.push_str("## Prior findings from log.md\n\n");
        prompt.push_str(log_content);
        prompt.push('\n');
    }

    prompt
}

/// Create a branch and worktree for the given approach.
///
/// Returns `(worktree_path, branch_name)`.
pub fn setup_worktree(
    repo_root: &Path,
    approach_name: &str,
    worktree_parent: &Path,
) -> Result<(PathBuf, String), ImplementError> {
    let branch_name = format!("autotune/{}", approach_name);
    let worktree_path = worktree_parent.join(approach_name);

    autotune_git::create_branch(repo_root, &branch_name)?;
    autotune_git::create_worktree(repo_root, &worktree_path, &branch_name)?;

    Ok((worktree_path, branch_name))
}

/// Spawn a sandboxed implementation agent and validate it committed changes.
#[allow(clippy::too_many_arguments)]
pub fn run_implementation(
    agent: &dyn Agent,
    hypothesis: &Hypothesis,
    worktree_path: &Path,
    branch_name: &str,
    tunable_paths: &[String],
    log_content: &str,
    model: Option<&str>,
    max_turns: Option<u64>,
) -> Result<ImplementResult, ImplementError> {
    let prompt = build_implementation_prompt(hypothesis, log_content);
    let permissions = implementation_agent_permissions(tunable_paths);

    let config = AgentConfig {
        prompt,
        allowed_tools: permissions,
        working_directory: worktree_path.to_path_buf(),
        model: model.map(String::from),
        max_turns,
    };

    // Record the commit SHA before the agent runs so we can detect new commits.
    let sha_before = autotune_git::latest_commit_sha(worktree_path)?;

    let response = agent.spawn(&config)?;

    let sha_after = autotune_git::latest_commit_sha(worktree_path)?;

    if sha_before == sha_after {
        return Err(ImplementError::NoCommit);
    }

    Ok(ImplementResult {
        worktree_path: worktree_path.to_path_buf(),
        branch_name: branch_name.to_string(),
        commit_sha: sha_after,
        agent_response: response,
    })
}
