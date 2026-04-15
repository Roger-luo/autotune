use std::path::Path;

use anyhow::{Context, Result};

use autotune_state::{Phase, TaskState, TaskStore};

/// Prepare a task for resumption by recovering from any incomplete phase.
///
/// Returns the state ready to be fed into `run_task`.
pub fn prepare_resume(store: &TaskStore, repo_root: &Path) -> Result<TaskState> {
    let mut state = store
        .load_state()
        .context("failed to load task state for resume")?;

    match state.current_phase {
        Phase::Planning => {
            // Safe to restart planning from scratch
            println!("[resume] resuming from Planning phase — will re-plan");
        }

        Phase::Implementing => {
            // Implementation may have been interrupted.
            // Check if the worktree has a new commit.
            if let Some(ref approach) = state.current_approach {
                if approach.commit_sha.is_some() {
                    // Commit was recorded, move to Testing
                    println!("[resume] implementation had commit, moving to Testing");
                    state.current_phase = Phase::Testing;
                } else {
                    // No commit — clean up worktree and restart planning
                    println!(
                        "[resume] implementation incomplete, cleaning up and returning to Planning"
                    );
                    let _ = autotune_git::remove_worktree(repo_root, &approach.worktree_path);
                    state.current_approach = None;
                    state.current_phase = Phase::Planning;
                }
            } else {
                state.current_phase = Phase::Planning;
            }
        }

        Phase::Testing => {
            // Re-run tests from the beginning
            println!("[resume] resuming from Testing phase — will re-run tests");
        }

        Phase::Fixing => {
            // A crashed fix turn is safe to retry: state holds the session id
            // (if session-continuation) and the fix history, so the next run
            // replays the same prompt. Re-running tests first is cheaper
            // than re-invoking the implementer, and the tests may now pass
            // if a previous fix did commit before the crash.
            println!("[resume] resuming from Fixing phase — will re-run tests before next fix");
            state.current_phase = Phase::Testing;
        }

        Phase::Measuring => {
            // Re-run measurement tasks from the beginning
            println!("[resume] resuming from Measuring phase — will re-run tasks");
        }

        Phase::Scoring => {
            // If we have metrics, re-score; otherwise go back to measuring
            if let Some(ref approach) = state.current_approach {
                if approach.metrics.is_some() {
                    println!("[resume] resuming from Scoring phase — will re-score");
                } else {
                    println!("[resume] no metrics in Scoring phase, going back to Measuring");
                    state.current_phase = Phase::Measuring;
                }
            }
        }

        Phase::Integrating => {
            // Integration may have partially completed. Check if the approach
            // commits are already on the advancing branch.
            if let Some(ref approach) = state.current_approach {
                if let Some(ref sha) = approach.commit_sha {
                    let on_advancing = autotune_git::has_commits_ahead(
                        repo_root,
                        &format!("{sha}~1"),
                        &state.advancing_branch,
                    )
                    .unwrap_or(false);

                    if on_advancing {
                        println!(
                            "[resume] rebase already landed on advancing branch, moving to Recorded"
                        );
                        state.current_phase = Phase::Recorded;
                    } else {
                        println!("[resume] resuming from Integrating phase — will retry rebase");
                    }
                } else {
                    println!("[resume] no commit SHA in Integrating phase, going back to Planning");
                    state.current_approach = None;
                    state.current_phase = Phase::Planning;
                }
            }
        }

        Phase::Recorded => {
            println!("[resume] resuming from Recorded phase — will check stop conditions");
        }

        Phase::Done => {
            println!("[resume] task already done");
        }
    }

    store.save_state(&state)?;
    Ok(state)
}
