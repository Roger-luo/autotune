mod error;

pub use error::GitError;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of running a git command.
struct GitOutput {
    stdout: String,
    stderr: String,
}

/// Run a git command in the given directory and return stdout.
fn git(dir: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|source| GitError::Io { source })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(GitError::CommandFailed {
            command: format!("git {}", args.join(" ")),
            stderr,
        });
    }

    Ok(GitOutput { stdout, stderr })
}

/// Find the root of the git repository containing `dir`.
pub fn repo_root(dir: &Path) -> Result<PathBuf, GitError> {
    let GitOutput { stdout, stderr } = git(dir, &["rev-parse", "--show-toplevel"])?;
    let _ = stderr;
    Ok(PathBuf::from(stdout.trim()))
}

/// Get the current HEAD commit SHA (short).
pub fn head_sha(dir: &Path) -> Result<String, GitError> {
    let output = git(dir, &["rev-parse", "--short", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

/// Get the current branch name.
pub fn current_branch(dir: &Path) -> Result<String, GitError> {
    let output = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

/// Create a new branch from HEAD without switching to it.
pub fn create_branch(dir: &Path, branch_name: &str) -> Result<(), GitError> {
    git(dir, &["branch", branch_name])?;
    Ok(())
}

/// Create a git worktree at `worktree_path` on `branch_name`.
/// The branch must already exist.
pub fn create_worktree(
    dir: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<(), GitError> {
    let wt = worktree_path.to_str().unwrap_or_default();
    git(dir, &["worktree", "add", wt, branch_name])?;
    Ok(())
}

/// Remove a git worktree. Uses --force to handle dirty worktrees.
pub fn remove_worktree(dir: &Path, worktree_path: &Path) -> Result<(), GitError> {
    let wt = worktree_path.to_str().unwrap_or_default();
    git(dir, &["worktree", "remove", wt, "--force"])?;
    Ok(())
}

/// Cherry-pick a commit onto the current branch.
pub fn cherry_pick(dir: &Path, commit_sha: &str) -> Result<(), GitError> {
    git(dir, &["cherry-pick", commit_sha])?;
    Ok(())
}

/// Revert the most recent commit (merge-aware: -m 1).
pub fn revert_last(dir: &Path) -> Result<(), GitError> {
    git(dir, &["revert", "HEAD", "--no-edit"])?;
    Ok(())
}

/// Check if a branch has any commits ahead of another branch.
pub fn has_commits_ahead(dir: &Path, base: &str, branch: &str) -> Result<bool, GitError> {
    let range = format!("{}..{}", base, branch);
    let output = git(dir, &["rev-list", "--count", &range])?;
    let count: u64 = output.stdout.trim().parse().unwrap_or(0);
    Ok(count > 0)
}

/// Get the full SHA of the latest commit on a branch in the given directory.
pub fn latest_commit_sha(dir: &Path) -> Result<String, GitError> {
    let output = git(dir, &["rev-parse", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

/// Checkout a branch in the given directory.
pub fn checkout(dir: &Path, branch: &str) -> Result<(), GitError> {
    git(dir, &["checkout", branch])?;
    Ok(())
}

/// Merge a branch into the current branch with a merge commit.
pub fn merge(dir: &Path, branch: &str, message: &str) -> Result<(), GitError> {
    git(dir, &["merge", branch, "--no-ff", "-m", message])?;
    Ok(())
}
