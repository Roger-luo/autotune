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

fn git_with_env(dir: &Path, args: &[&OsStr], envs: &[(&str, &str)]) -> Result<GitOutput, GitError> {
    let mut command = Command::new("git");
    command.args(args.iter().copied()).current_dir(dir);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().map_err(|source| GitError::Io { source })?;

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

/// Return `git log --oneline` for commits between `base` and `HEAD` in
/// `dir`, one commit per returned `String` (newest first). Used by the
/// fix-respawn path to show a fresh implementer what's already been done
/// on the worktree branch.
pub fn log_oneline(dir: &Path, base: &str) -> Result<Vec<String>, GitError> {
    let range = format!("{base}..HEAD");
    let output = git(
        dir,
        &[
            OsStr::new("log"),
            OsStr::new("--oneline"),
            OsStr::new("--no-decorate"),
            OsStr::new(&range),
        ],
    )?;
    Ok(output
        .stdout
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .collect())
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
    let result = git_with_env(
        dir,
        &[OsStr::new("rebase"), OsStr::new("--continue")],
        &[("GIT_EDITOR", "true")],
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a temporary git repository with an initial commit.
    /// Returns the TempDir (caller must hold it to keep the directory alive).
    fn make_repo() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("git command");
            assert!(
                status.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&status.stderr)
            );
        };

        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);

        fs::write(path.join("README.md"), b"hello").expect("write file");

        run(&["add", "-A"]);
        run(&["commit", "-m", "initial commit"]);

        dir
    }

    #[test]
    fn repo_root_returns_dir() {
        let dir = make_repo();
        let root = repo_root(dir.path()).unwrap();
        // The root should be the same directory (canonicalized paths may differ on macOS due to /private)
        assert!(
            root.ends_with(dir.path().file_name().unwrap()),
            "repo_root {root:?} should end with temp dir name"
        );
    }

    #[test]
    fn head_sha_returns_nonempty() {
        let dir = make_repo();
        let sha = head_sha(dir.path()).unwrap();
        assert!(!sha.is_empty());
        // Short SHA is 7+ hex characters
        assert!(sha.len() >= 7);
    }

    #[test]
    fn current_branch_returns_string() {
        let dir = make_repo();
        let branch = current_branch(dir.path()).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn latest_commit_sha_returns_nonempty() {
        let dir = make_repo();
        let sha = latest_commit_sha(dir.path()).unwrap();
        assert!(!sha.is_empty());
        // Full SHA is 40 hex characters
        assert_eq!(sha.len(), 40);
    }

    #[test]
    fn has_uncommitted_changes_false_on_clean() {
        let dir = make_repo();
        let dirty = has_uncommitted_changes(dir.path()).unwrap();
        assert!(!dirty);
    }

    #[test]
    fn has_uncommitted_changes_true_after_write() {
        let dir = make_repo();
        fs::write(dir.path().join("new_file.txt"), b"content").unwrap();
        let dirty = has_uncommitted_changes(dir.path()).unwrap();
        assert!(dirty);
    }

    #[test]
    fn stage_all_and_commit_creates_commit() {
        let dir = make_repo();
        let sha_before = latest_commit_sha(dir.path()).unwrap();
        fs::write(dir.path().join("extra.txt"), b"extra").unwrap();
        stage_all_and_commit(dir.path(), "add extra file").unwrap();
        let sha_after = latest_commit_sha(dir.path()).unwrap();
        assert_ne!(sha_before, sha_after, "commit should advance HEAD");
    }

    #[test]
    fn branch_exists_false_for_missing() {
        let dir = make_repo();
        let exists = branch_exists(dir.path(), "nonexistent-branch-xyz").unwrap();
        assert!(!exists);
    }

    #[test]
    fn create_branch_makes_branch_exist() {
        let dir = make_repo();
        assert!(!branch_exists(dir.path(), "feature-x").unwrap());
        create_branch(dir.path(), "feature-x").unwrap();
        assert!(branch_exists(dir.path(), "feature-x").unwrap());
    }

    #[test]
    fn log_oneline_empty_at_base() {
        let dir = make_repo();
        // HEAD..HEAD range produces no commits
        let lines = log_oneline(dir.path(), "HEAD").unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn has_commits_ahead_false_same_branch() {
        let dir = make_repo();
        // HEAD vs HEAD: zero commits ahead
        let ahead = has_commits_ahead(dir.path(), "HEAD", "HEAD").unwrap();
        assert!(!ahead);
    }

    #[test]
    fn checkout_switches_branch() {
        let dir = make_repo();
        create_branch(dir.path(), "feature-checkout").unwrap();
        checkout(dir.path(), "feature-checkout").unwrap();
        let branch = current_branch(dir.path()).unwrap();
        assert_eq!(branch, "feature-checkout");
    }

    #[test]
    fn create_branch_from_creates_branch() {
        let dir = make_repo();
        assert!(!branch_exists(dir.path(), "from-main").unwrap());
        create_branch_from(dir.path(), "from-main", "main").unwrap();
        assert!(branch_exists(dir.path(), "from-main").unwrap());
    }

    #[test]
    fn cherry_pick_applies_commit_to_main() {
        let dir = make_repo();
        // Create a feature branch and add a commit on it
        create_branch(dir.path(), "cherry-src").unwrap();
        checkout(dir.path(), "cherry-src").unwrap();
        fs::write(dir.path().join("cherry.txt"), b"cherry content").unwrap();
        stage_all_and_commit(dir.path(), "add cherry file").unwrap();
        let cherry_sha = latest_commit_sha(dir.path()).unwrap();

        // Go back to main and cherry-pick
        checkout(dir.path(), "main").unwrap();
        let sha_before = latest_commit_sha(dir.path()).unwrap();
        cherry_pick(dir.path(), &cherry_sha).unwrap();
        let sha_after = latest_commit_sha(dir.path()).unwrap();
        assert_ne!(
            sha_before, sha_after,
            "cherry-pick should advance HEAD on main"
        );
    }

    #[test]
    fn has_commits_ahead_true_when_feature_has_commit() {
        let dir = make_repo();
        create_branch(dir.path(), "ahead-feature").unwrap();
        checkout(dir.path(), "ahead-feature").unwrap();
        fs::write(dir.path().join("ahead.txt"), b"ahead").unwrap();
        stage_all_and_commit(dir.path(), "add ahead file").unwrap();
        // Switch back to main so we can compare
        checkout(dir.path(), "main").unwrap();
        let ahead = has_commits_ahead(dir.path(), "main", "ahead-feature").unwrap();
        assert!(ahead, "ahead-feature should have commits ahead of main");
    }

    #[test]
    fn log_oneline_returns_nonempty_after_second_commit() {
        let dir = make_repo();
        // Add a second commit on main
        fs::write(dir.path().join("second.txt"), b"second").unwrap();
        stage_all_and_commit(dir.path(), "second commit").unwrap();
        // log_oneline from HEAD~1 should return the second commit
        let lines = log_oneline(dir.path(), "HEAD~1").unwrap();
        assert!(!lines.is_empty(), "should have at least one log line");
    }

    #[test]
    fn merge_creates_merge_commit() {
        let dir = make_repo();
        // Create a feature branch with one commit
        create_branch(dir.path(), "merge-feature").unwrap();
        checkout(dir.path(), "merge-feature").unwrap();
        fs::write(dir.path().join("merge_file.txt"), b"merged content").unwrap();
        stage_all_and_commit(dir.path(), "feature commit for merge").unwrap();
        let feature_sha = latest_commit_sha(dir.path()).unwrap();

        // Merge with --no-ff into main
        checkout(dir.path(), "main").unwrap();
        let sha_before = latest_commit_sha(dir.path()).unwrap();
        merge(dir.path(), "merge-feature", "merge feature into main").unwrap();
        let sha_after = latest_commit_sha(dir.path()).unwrap();
        assert_ne!(sha_before, sha_after, "main should have a new commit");
        // The feature commit should not be HEAD — a merge commit sits on top
        assert_ne!(
            sha_after, feature_sha,
            "HEAD should be the merge commit, not the feature commit"
        );
        // Verify it is a merge commit (has two parents)
        assert!(
            head_is_merge_commit(dir.path()).unwrap(),
            "HEAD should be a merge commit"
        );
    }

    #[test]
    fn revert_last_regular_commit() {
        let dir = make_repo();
        // Add a second commit to revert
        fs::write(dir.path().join("to_revert.txt"), b"content").unwrap();
        stage_all_and_commit(dir.path(), "commit to revert").unwrap();
        let sha_before_revert = latest_commit_sha(dir.path()).unwrap();
        revert_last(dir.path()).unwrap();
        let sha_after = latest_commit_sha(dir.path()).unwrap();
        assert_ne!(
            sha_before_revert, sha_after,
            "revert should create a new commit"
        );
    }

    #[test]
    fn revert_last_merge_commit() {
        let dir = make_repo();
        // Create a merge commit
        create_branch(dir.path(), "revert-feature").unwrap();
        checkout(dir.path(), "revert-feature").unwrap();
        fs::write(dir.path().join("revert_merge.txt"), b"content").unwrap();
        stage_all_and_commit(dir.path(), "feature commit").unwrap();
        checkout(dir.path(), "main").unwrap();
        merge(dir.path(), "revert-feature", "merge revert-feature").unwrap();
        assert!(head_is_merge_commit(dir.path()).unwrap());
        let sha_before = latest_commit_sha(dir.path()).unwrap();
        revert_last(dir.path()).unwrap();
        let sha_after = latest_commit_sha(dir.path()).unwrap();
        assert_ne!(
            sha_before, sha_after,
            "reverting a merge commit should create a new revert commit"
        );
    }

    #[test]
    fn merge_or_conflict_returns_true_on_clean_merge() {
        let dir = make_repo();
        create_branch(dir.path(), "clean-feature").unwrap();
        checkout(dir.path(), "clean-feature").unwrap();
        fs::write(dir.path().join("clean_feature.txt"), b"no conflict here").unwrap();
        stage_all_and_commit(dir.path(), "clean feature commit").unwrap();
        checkout(dir.path(), "main").unwrap();
        let result = merge_or_conflict(dir.path(), "clean-feature", "merge clean feature").unwrap();
        assert!(result, "clean merge should return true");
    }

    #[test]
    fn list_conflicted_files_empty_in_clean_repo() {
        let dir = make_repo();
        let files = list_conflicted_files(dir.path()).unwrap();
        assert!(files.is_empty(), "no conflicted files in a clean repo");
    }

    /// Helper: create a repo where `main` and `conflict-branch` have diverging
    /// edits to the same line, ready to trigger a conflict on merge.
    fn make_conflicted_repo() -> (TempDir, String) {
        let dir = make_repo();
        let path = dir.path();

        // Add a file on main
        fs::write(path.join("conflict.txt"), b"line from main\n").unwrap();
        stage_all_and_commit(path, "add conflict.txt on main").unwrap();

        // Branch off and change the same file
        create_branch(path, "conflict-branch").unwrap();
        checkout(path, "conflict-branch").unwrap();
        fs::write(path.join("conflict.txt"), b"line from branch\n").unwrap();
        stage_all_and_commit(path, "change conflict.txt on branch").unwrap();

        // Go back to main and also change the same file (different content → conflict)
        checkout(path, "main").unwrap();
        fs::write(path.join("conflict.txt"), b"line from main edit\n").unwrap();
        stage_all_and_commit(path, "change conflict.txt on main too").unwrap();

        (dir, "conflict-branch".to_string())
    }

    #[test]
    fn merge_or_conflict_returns_false_on_conflict() {
        let (dir, branch) = make_conflicted_repo();
        let result = merge_or_conflict(dir.path(), &branch, "merge conflicting branch").unwrap();
        assert!(!result, "conflicting merge should return false");
    }

    #[test]
    fn merge_abort_restores_clean_state() {
        let (dir, branch) = make_conflicted_repo();
        // Trigger conflict
        let result = merge_or_conflict(dir.path(), &branch, "trigger conflict").unwrap();
        assert!(!result);
        // Abort should succeed and leave repo without conflicts
        merge_abort(dir.path()).unwrap();
        assert!(
            !has_merge_conflicts(dir.path()).unwrap(),
            "no conflicts after abort"
        );
    }

    #[test]
    fn conclude_merge_creates_commit_after_resolution() {
        let (dir, branch) = make_conflicted_repo();
        // Trigger conflict
        let result = merge_or_conflict(dir.path(), &branch, "merge with conflict").unwrap();
        assert!(!result);
        let sha_before = latest_commit_sha(dir.path()).unwrap();
        // Resolve by overwriting the conflicted file
        fs::write(dir.path().join("conflict.txt"), b"resolved content\n").unwrap();
        conclude_merge(dir.path(), "resolve conflict").unwrap();
        let sha_after = latest_commit_sha(dir.path()).unwrap();
        assert_ne!(
            sha_before, sha_after,
            "conclude_merge should create a new commit"
        );
    }

    #[test]
    fn merge_ff_only_advances_main() {
        let dir = make_repo();
        // Create feature branch with one commit
        create_branch(dir.path(), "ff-feature").unwrap();
        checkout(dir.path(), "ff-feature").unwrap();
        fs::write(dir.path().join("ff.txt"), b"ff content").unwrap();
        stage_all_and_commit(dir.path(), "ff commit").unwrap();
        let feature_sha = latest_commit_sha(dir.path()).unwrap();

        // Switch back to main and fast-forward
        checkout(dir.path(), "main").unwrap();
        let sha_before = latest_commit_sha(dir.path()).unwrap();
        merge_ff_only(dir.path(), "ff-feature").unwrap();
        let sha_after = latest_commit_sha(dir.path()).unwrap();
        assert_ne!(sha_before, sha_after, "main should have advanced");
        assert_eq!(
            sha_after, feature_sha,
            "main should point to the feature commit"
        );
    }
}
