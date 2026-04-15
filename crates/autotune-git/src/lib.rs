mod error;

pub use error::GitError;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of running a git command.
struct GitOutput {
    stdout: String,
    stderr: String,
}

/// Run a git command in the given directory and return stdout.
fn git(dir: &Path, args: &[&OsStr]) -> Result<GitOutput, GitError> {
    let output = Command::new("git")
        .args(args.iter().copied())
        .current_dir(dir)
        .output()
        .map_err(|source| GitError::Io { source })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(GitError::CommandFailed {
            command: format!(
                "git {}",
                args.iter()
                    .map(|arg| arg.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            stderr,
        });
    }

    Ok(GitOutput { stdout, stderr })
}

/// Find the root of the git repository containing `dir`.
pub fn repo_root(dir: &Path) -> Result<PathBuf, GitError> {
    let GitOutput { stdout, stderr } = git(
        dir,
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
    )?;
    let _ = stderr;
    Ok(PathBuf::from(stdout.trim_end_matches(['\n', '\r'])))
}

/// Get the current HEAD commit SHA (short).
pub fn head_sha(dir: &Path) -> Result<String, GitError> {
    let output = git(
        dir,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--short"),
            OsStr::new("HEAD"),
        ],
    )?;
    Ok(output.stdout.trim().to_string())
}

/// Get the current branch name.
pub fn current_branch(dir: &Path) -> Result<String, GitError> {
    let output = git(
        dir,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--abbrev-ref"),
            OsStr::new("HEAD"),
        ],
    )?;
    Ok(output.stdout.trim().to_string())
}

/// Create a new branch from HEAD without switching to it.
pub fn create_branch(dir: &Path, branch_name: &str) -> Result<(), GitError> {
    git(dir, &[OsStr::new("branch"), OsStr::new(branch_name)])?;
    Ok(())
}

/// Returns true if a branch with the given name exists (local ref).
pub fn branch_exists(dir: &Path, branch_name: &str) -> Result<bool, GitError> {
    let result = git(
        dir,
        &[
            OsStr::new("show-ref"),
            OsStr::new("--verify"),
            OsStr::new("--quiet"),
            OsStr::new(&format!("refs/heads/{branch_name}")),
        ],
    );
    Ok(result.is_ok())
}

/// Create a new branch starting from a specific base branch.
pub fn create_branch_from(
    dir: &Path,
    branch_name: &str,
    start_point: &str,
) -> Result<(), GitError> {
    git(
        dir,
        &[
            OsStr::new("branch"),
            OsStr::new(branch_name),
            OsStr::new(start_point),
        ],
    )?;
    Ok(())
}

/// Create a git worktree at `worktree_path` on `branch_name`.
/// The branch must already exist.
pub fn create_worktree(
    dir: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<(), GitError> {
    git(
        dir,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            worktree_path.as_os_str(),
            OsStr::new(branch_name),
        ],
    )?;
    Ok(())
}

/// Remove a git worktree. Uses --force to handle dirty worktrees.
pub fn remove_worktree(dir: &Path, worktree_path: &Path) -> Result<(), GitError> {
    git(
        dir,
        &[
            OsStr::new("worktree"),
            OsStr::new("remove"),
            worktree_path.as_os_str(),
            OsStr::new("--force"),
        ],
    )?;
    Ok(())
}

/// Cherry-pick a commit onto the current branch.
pub fn cherry_pick(dir: &Path, commit_sha: &str) -> Result<(), GitError> {
    git(dir, &[OsStr::new("cherry-pick"), OsStr::new(commit_sha)])?;
    Ok(())
}

fn head_is_merge_commit(dir: &Path) -> Result<bool, GitError> {
    let output = git(
        dir,
        &[
            OsStr::new("rev-list"),
            OsStr::new("--parents"),
            OsStr::new("-n"),
            OsStr::new("1"),
            OsStr::new("HEAD"),
        ],
    )?;
    Ok(output.stdout.split_whitespace().count() > 2)
}

/// Revert the most recent commit.
pub fn revert_last(dir: &Path) -> Result<(), GitError> {
    if head_is_merge_commit(dir)? {
        git(
            dir,
            &[
                OsStr::new("revert"),
                OsStr::new("--no-edit"),
                OsStr::new("-m"),
                OsStr::new("1"),
                OsStr::new("HEAD"),
            ],
        )?;
    } else {
        git(
            dir,
            &[
                OsStr::new("revert"),
                OsStr::new("--no-edit"),
                OsStr::new("HEAD"),
            ],
        )?;
    }
    Ok(())
}

/// Check if a branch has any commits ahead of another branch.
pub fn has_commits_ahead(dir: &Path, base: &str, branch: &str) -> Result<bool, GitError> {
    let range = format!("{}..{}", base, branch);
    let output = git(
        dir,
        &[
            OsStr::new("rev-list"),
            OsStr::new("--count"),
            OsStr::new(&range),
        ],
    )?;
    let count: u64 = output.stdout.trim().parse().unwrap_or(0);
    Ok(count > 0)
}

/// Get the full SHA of the latest commit on a branch in the given directory.
pub fn latest_commit_sha(dir: &Path) -> Result<String, GitError> {
    let output = git(dir, &[OsStr::new("rev-parse"), OsStr::new("HEAD")])?;
    Ok(output.stdout.trim().to_string())
}

/// Returns true if the working tree or index has any uncommitted changes
/// (including untracked files).
pub fn has_uncommitted_changes(dir: &Path) -> Result<bool, GitError> {
    let output = git(dir, &[OsStr::new("status"), OsStr::new("--porcelain")])?;
    Ok(!output.stdout.trim().is_empty())
}

/// Stage all changes (including untracked files) and create a commit.
pub fn stage_all_and_commit(dir: &Path, message: &str) -> Result<(), GitError> {
    git(dir, &[OsStr::new("add"), OsStr::new("-A")])?;
    git(
        dir,
        &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new(message)],
    )?;
    Ok(())
}

/// Checkout a branch in the given directory.
pub fn checkout(dir: &Path, branch: &str) -> Result<(), GitError> {
    git(dir, &[OsStr::new("checkout"), OsStr::new(branch)])?;
    Ok(())
}

/// Merge a branch into the current branch with a merge commit.
pub fn merge(dir: &Path, branch: &str, message: &str) -> Result<(), GitError> {
    git(
        dir,
        &[
            OsStr::new("merge"),
            OsStr::new(branch),
            OsStr::new("--no-ff"),
            OsStr::new("-m"),
            OsStr::new(message),
        ],
    )?;
    Ok(())
}

/// Attempt to merge a branch. Returns `Ok(true)` if the merge completed
/// cleanly, `Ok(false)` if there are conflicts that need resolution.
pub fn merge_or_conflict(dir: &Path, branch: &str, message: &str) -> Result<bool, GitError> {
    let result = git(
        dir,
        &[
            OsStr::new("merge"),
            OsStr::new(branch),
            OsStr::new("--no-ff"),
            OsStr::new("-m"),
            OsStr::new(message),
        ],
    );
    match result {
        Ok(_) => Ok(true),
        Err(_) => {
            // Check if we're in a conflicted merge state.
            if has_merge_conflicts(dir)? {
                Ok(false)
            } else {
                // Some other merge error (e.g. unrelated dirty files).
                Err(GitError::CommandFailed {
                    command: format!("git merge {} --no-ff -m '{}'", branch, message),
                    stderr: "merge failed for an unexpected reason".to_string(),
                })
            }
        }
    }
}

/// Returns true if there are unresolved merge conflicts in the working tree.
pub fn has_merge_conflicts(dir: &Path) -> Result<bool, GitError> {
    let output = git(dir, &[OsStr::new("diff"), OsStr::new("--check")]);
    match output {
        Ok(_) => Ok(false),
        // `git diff --check` exits non-zero when conflict markers are present.
        Err(_) => Ok(true),
    }
}

/// List files with unresolved merge conflicts.
pub fn list_conflicted_files(dir: &Path) -> Result<Vec<String>, GitError> {
    let output = git(
        dir,
        &[
            OsStr::new("diff"),
            OsStr::new("--name-only"),
            OsStr::new("--diff-filter=U"),
        ],
    )?;
    Ok(output
        .stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Abort an in-progress merge.
pub fn merge_abort(dir: &Path) -> Result<(), GitError> {
    git(dir, &[OsStr::new("merge"), OsStr::new("--abort")])?;
    Ok(())
}

/// Stage all changes and finalize the merge (commit the resolved merge).
pub fn conclude_merge(dir: &Path, message: &str) -> Result<(), GitError> {
    git(dir, &[OsStr::new("add"), OsStr::new("-A")])?;
    git(
        dir,
        &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new(message)],
    )?;
    Ok(())
}

/// Rebase the current branch onto `onto`. Returns `Ok(true)` if clean,
/// `Ok(false)` if there are conflicts to resolve.
pub fn rebase(dir: &Path, onto: &str) -> Result<bool, GitError> {
    let result = git(dir, &[OsStr::new("rebase"), OsStr::new(onto)]);
    match result {
        Ok(_) => Ok(true),
        Err(_) => {
            if has_merge_conflicts(dir)? {
                Ok(false)
            } else {
                Err(GitError::CommandFailed {
                    command: format!("git rebase {onto}"),
                    stderr: "rebase failed for an unexpected reason".to_string(),
                })
            }
        }
    }
}

/// Stage resolved files and continue an in-progress rebase.
/// Returns `Ok(true)` if rebase completed, `Ok(false)` if another
/// conflict was hit.
pub fn rebase_continue(dir: &Path) -> Result<bool, GitError> {
    git(dir, &[OsStr::new("add"), OsStr::new("-A")])?;
    let result = git(dir, &[OsStr::new("rebase"), OsStr::new("--continue")]);
    match result {
        Ok(_) => Ok(true),
        Err(_) => {
            if has_merge_conflicts(dir)? {
                Ok(false)
            } else {
                Err(GitError::CommandFailed {
                    command: "git rebase --continue".to_string(),
                    stderr: "rebase --continue failed unexpectedly".to_string(),
                })
            }
        }
    }
}

/// Abort an in-progress rebase.
pub fn rebase_abort(dir: &Path) -> Result<(), GitError> {
    git(dir, &[OsStr::new("rebase"), OsStr::new("--abort")])?;
    Ok(())
}

/// Fast-forward `branch` to the current HEAD. Fails if not a fast-forward.
pub fn merge_ff_only(dir: &Path, branch: &str) -> Result<(), GitError> {
    git(
        dir,
        &[
            OsStr::new("merge"),
            OsStr::new("--ff-only"),
            OsStr::new(branch),
        ],
    )?;
    Ok(())
}
