---
title: autotune-git
description: Thin git CLI wrapper for worktree creation, branch management, and rebase/merge integration.
section: Crates
order: 6
---

`autotune-git` is a leaf crate that wraps the `git` command-line tool in small, typed Rust functions. It backs Autotune's integration phase: creating per-iteration worktrees, managing the advancing branch, and replaying kept commits via rebase or merge while leaving the user's canonical branch untouched.

## When to use it

- You need to drive git from Rust without pulling in a full libgit2 binding — every function shells out to the system `git`.
- You're working on the state machine's worktree, branch, or integration logic.
- You want conflict-aware variants (`merge_or_conflict`, `rebase`) that distinguish a clean run from one that needs manual resolution, rather than a bare pass/fail.

## Public API

All functions take a working directory `dir: &Path` as their first argument and return `Result<_, GitError>`.

- `GitError` — error enum: `CommandFailed { command, stderr }`, `NotARepo { path }`, `Io { source }`.
- `repo_root` — find the repository toplevel containing `dir`.
- `head_sha` / `latest_commit_sha` — short HEAD SHA / full HEAD SHA.
- `current_branch` — current branch name (`--abbrev-ref HEAD`).
- `create_branch` — branch from HEAD without switching.
- `create_branch_from` — branch from an explicit start point.
- `branch_exists` — true if a local `refs/heads/<name>` exists.
- `delete_branch` — force-delete a local branch (`branch -D`).
- `list_branches_with_prefix` — local branches matching `refs/heads/<prefix>*`.
- `create_worktree` — add a worktree at a path on an existing branch.
- `remove_worktree` — remove a worktree (`--force`, handles dirty trees).
- `checkout` — switch branches.
- `has_uncommitted_changes` — true if working tree or index is dirty (includes untracked).
- `stage_all_and_commit` — `add -A` then commit with a message.
- `cherry_pick` — cherry-pick a commit onto the current branch.
- `revert_last` — revert the most recent commit (handles merge commits with `-m 1`).
- `has_commits_ahead` — true if `base..branch` contains any commits.
- `log_oneline` — `git log --oneline` for `base..HEAD`, one commit per `String`.
- `merge` — merge a branch with a forced merge commit (`--no-ff`).
- `merge_or_conflict` — merge; `Ok(true)` if clean, `Ok(false)` on conflicts.
- `merge_ff_only` — fast-forward a branch to HEAD; fails if not a fast-forward.
- `merge_abort` — abort an in-progress merge.
- `conclude_merge` — stage all and commit to finalize a resolved merge.
- `rebase` — rebase the current branch onto a target; `Ok(true)` clean, `Ok(false)` on conflicts.
- `rebase_continue` — stage resolved files and continue (`Ok(false)` if another conflict hit).
- `rebase_abort` — abort an in-progress rebase.
- `has_merge_conflicts` — true if conflict markers are present in the working tree.
- `list_conflicted_files` — files with unresolved conflicts (`--diff-filter=U`).

## Usage

```rust
use std::path::Path;
use autotune_git::{self as git, GitError};

fn integrate(repo: &Path, worktree: &Path, advancing: &str) -> Result<(), GitError> {
    // Rebase the worktree branch onto the advancing branch, inside the worktree.
    if !git::rebase(worktree, advancing)? {
        // Conflicts: an agent edits the files listed here, then we continue.
        for file in git::list_conflicted_files(worktree)? {
            eprintln!("conflict: {file}");
        }
        // ... resolve, then ...
        git::rebase_continue(worktree)?;
    }

    // Detach the branch by removing the worktree, then fast-forward.
    let branch = git::current_branch(worktree)?;
    git::remove_worktree(repo, worktree)?;
    git::checkout(repo, advancing)?;
    git::merge_ff_only(repo, &branch)?;
    Ok(())
}
```

## Internal dependencies

None — this is a leaf crate. It depends only on `thiserror` (and `tempfile` as a dev-dependency for tests).

## Notes

- The conflict-aware functions (`merge_or_conflict`, `rebase`, `rebase_continue`) only return `Ok(false)` when `has_merge_conflicts` confirms conflict markers; any other failure surfaces as `GitError::CommandFailed`.
- `rebase_continue` runs git with `GIT_EDITOR=true` so the rebase doesn't block on an interactive editor for the commit message.
- A rebase must be run inside the worktree directory where the branch is checked out — git refuses to operate on a worktree-attached branch from the main repo. The `-main` suffix on advancing branches keeps them siblings of (not path-prefixes of) the `autotune/<task>/<slug>` worktree branch namespace; see `notes/git-integration.md`.
