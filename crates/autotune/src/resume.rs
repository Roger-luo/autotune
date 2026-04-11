use std::path::Path;

use anyhow::{Context, Result};

use autotune_state::{ExperimentState, ExperimentStore, Phase};

/// Prepare an experiment for resumption by recovering from any incomplete phase.
///
/// Returns the state ready to be fed into `run_experiment`.
pub fn prepare_resume(store: &ExperimentStore, repo_root: &Path) -> Result<ExperimentState> {
    let mut state = store
        .load_state()
        .context("failed to load experiment state for resume")?;

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

        Phase::Benchmarking => {
            // Re-run benchmarks from the beginning
            println!("[resume] resuming from Benchmarking phase — will re-run benchmarks");
        }

        Phase::Scoring => {
            // If we have metrics, re-score; otherwise go back to benchmarking
            if let Some(ref approach) = state.current_approach {
                if approach.metrics.is_some() {
                    println!("[resume] resuming from Scoring phase — will re-score");
                } else {
                    println!("[resume] no metrics in Scoring phase, going back to Benchmarking");
                    state.current_phase = Phase::Benchmarking;
                }
            }
        }

        Phase::Integrating => {
            // Integration may have partially completed. Check if cherry-pick landed.
            if let Some(ref approach) = state.current_approach {
                if let Some(ref sha) = approach.commit_sha {
                    // Check if the commit is already on the canonical branch
                    let on_canonical = autotune_git::has_commits_ahead(
                        repo_root,
                        &format!("{sha}~1"),
                        &state.canonical_branch,
                    )
                    .unwrap_or(false);

                    if on_canonical {
                        println!("[resume] cherry-pick already landed, moving to Recorded");
                        state.current_phase = Phase::Recorded;
                    } else {
                        println!(
                            "[resume] resuming from Integrating phase — will retry cherry-pick"
                        );
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
            println!("[resume] experiment already done");
        }
    }

    store.save_state(&state)?;
    Ok(state)
}
