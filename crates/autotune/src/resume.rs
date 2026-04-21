use std::path::Path;

use anyhow::{Context, Result};

use autotune_state::{Phase, TaskState, TaskStore};

fn reset_to_planning(state: &mut TaskState, message: &str) {
    println!("{message}");
    state.current_approach = None;
    state.current_phase = Phase::Planning;
}

fn reset_to_testing(state: &mut TaskState, message: &str) {
    println!("{message}");
    state.current_phase = Phase::Testing;
}

fn reset_to_measuring(state: &mut TaskState, message: &str) {
    println!("{message}");
    state.current_phase = Phase::Measuring;
}

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
            let worktree_path = state
                .current_approach
                .as_ref()
                .map(|approach| approach.worktree_path.clone());
            let has_commit = state
                .current_approach
                .as_ref()
                .is_some_and(|approach| approach.commit_sha.is_some());
            if let Some(worktree_path) = worktree_path {
                if has_commit {
                    // Commit was recorded, move to Testing
                    println!("[resume] implementation had commit, moving to Testing");
                    state.current_phase = Phase::Testing;
                } else {
                    // No commit — clean up worktree and restart planning
                    reset_to_planning(
                        &mut state,
                        "[resume] implementation incomplete, cleaning up and returning to Planning"
                    );
                    let _ = autotune_git::remove_worktree(repo_root, &worktree_path);
                }
            } else {
                reset_to_planning(
                    &mut state,
                    "[resume] inconsistent state: Implementing phase had no current approach, returning to Planning",
                );
            }
        }

        Phase::Testing => {
            if state.current_approach.is_some() {
                // Re-run tests from the beginning
                println!("[resume] resuming from Testing phase — will re-run tests");
            } else {
                reset_to_planning(
                    &mut state,
                    "[resume] inconsistent state: Testing phase had no current approach, returning to Planning",
                );
            }
        }

        Phase::Fixing => {
            // A crashed fix turn is safe to retry: state holds the session id
            // (if session-continuation) and the fix history, so the next run
            // replays the same prompt. Re-running tests first is cheaper
            // than re-invoking the implementer, and the tests may now pass
            // if a previous fix did commit before the crash.
            let has_approach = state.current_approach.is_some();
            let has_fix_history = state
                .current_approach
                .as_ref()
                .is_some_and(|approach| !approach.fix_history.is_empty());
            if !has_approach {
                reset_to_planning(
                    &mut state,
                    "[resume] inconsistent state: Fixing phase had no current approach, returning to Planning",
                );
            } else if !has_fix_history {
                reset_to_testing(
                    &mut state,
                    "[resume] inconsistent state: Fixing phase had no failure history, going back to Testing",
                );
            } else {
                reset_to_testing(
                    &mut state,
                    "[resume] resuming from Fixing phase — will re-run tests before next fix",
                );
            }
        }

        Phase::Measuring => {
            if state.current_approach.is_some() {
                // Re-run measurement tasks from the beginning
                println!("[resume] resuming from Measuring phase — will re-run tasks");
            } else {
                reset_to_planning(
                    &mut state,
                    "[resume] inconsistent state: Measuring phase had no current approach, returning to Planning",
                );
            }
        }

        Phase::Scoring => {
            // If we have metrics, re-score; otherwise go back to measuring
            let has_approach = state.current_approach.is_some();
            let has_metrics = state
                .current_approach
                .as_ref()
                .is_some_and(|approach| approach.metrics.is_some());
            if !has_approach {
                reset_to_planning(
                    &mut state,
                    "[resume] inconsistent state: Scoring phase had no current approach, returning to Planning",
                );
            } else if has_metrics {
                println!("[resume] resuming from Scoring phase — will re-score");
            } else {
                reset_to_measuring(
                    &mut state,
                    "[resume] inconsistent state: Scoring phase had no metrics, going back to Measuring",
                );
            }
        }

        Phase::Integrating => {
            // Integration may have partially completed. Check if the approach
            // commits are already on the advancing branch.
            let commit_sha = state
                .current_approach
                .as_ref()
                .and_then(|approach| approach.commit_sha.clone());
            if let Some(sha) = commit_sha {
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
            } else if state.current_approach.is_some() {
                reset_to_planning(
                    &mut state,
                    "[resume] inconsistent state: Integrating phase had no commit SHA, returning to Planning",
                );
            } else {
                reset_to_planning(
                    &mut state,
                    "[resume] inconsistent state: Integrating phase had no current approach, returning to Planning",
                );
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

#[cfg(test)]
mod tests {
    use autotune_state::{ApproachState, Phase, TaskState, TaskStore};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    use super::*;

    fn make_state(phase: Phase, approach: Option<ApproachState>) -> TaskState {
        TaskState {
            task_name: "t".to_string(),
            canonical_branch: "main".to_string(),
            advancing_branch: "autotune/t-main".to_string(),
            research_session_id: "sess-1".to_string(),
            research_backend: "claude".to_string(),
            current_iteration: 1,
            current_phase: phase,
            current_approach: approach,
        }
    }

    fn make_approach(
        commit_sha: Option<&str>,
        metrics: Option<HashMap<String, f64>>,
        worktree_path: PathBuf,
    ) -> ApproachState {
        ApproachState {
            name: "opt".to_string(),
            hypothesis: "h".to_string(),
            worktree_path,
            branch_name: "autotune/t/opt".to_string(),
            commit_sha: commit_sha.map(|s| s.to_string()),
            test_results: vec![],
            metrics,
            rank: None,
            files_to_modify: vec![],
            impl_session_id: None,
            impl_backend: None,
            fix_attempts: 0,
            fresh_spawns: 0,
            fix_history: vec![],
            score_reason: None,
        }
    }

    #[test]
    fn resume_planning_stays_at_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Planning, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
    }

    #[test]
    fn resume_implementing_with_commit_advances_to_testing() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(Some("abc123"), None, tmp.path().join("wt"));
        let state = make_state(Phase::Implementing, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Testing);
        assert!(result.current_approach.is_some());
    }

    #[test]
    fn resume_implementing_without_commit_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(None, None, tmp.path().join("nonexistent"));
        let state = make_state(Phase::Implementing, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
        assert!(result.current_approach.is_none());
    }

    #[test]
    fn resume_implementing_without_approach_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Implementing, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
    }

    #[test]
    fn resume_testing_stays_at_testing() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(None, None, tmp.path().join("wt"));
        let state = make_state(Phase::Testing, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Testing);
    }

    #[test]
    fn resume_testing_without_approach_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Testing, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
        assert!(result.current_approach.is_none());
    }

    #[test]
    fn resume_fixing_with_history_advances_to_testing() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let mut approach = make_approach(None, None, tmp.path().join("wt"));
        approach.fix_history.push("failed test output".to_string());
        let state = make_state(Phase::Fixing, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Testing);
    }

    #[test]
    fn resume_fixing_without_history_falls_back_to_testing() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(None, None, tmp.path().join("wt"));
        let state = make_state(Phase::Fixing, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Testing);
    }

    #[test]
    fn resume_fixing_without_approach_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Fixing, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
    }

    #[test]
    fn resume_measuring_stays_at_measuring() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(None, None, tmp.path().join("wt"));
        let state = make_state(Phase::Measuring, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Measuring);
    }

    #[test]
    fn resume_measuring_without_approach_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Measuring, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
        assert!(result.current_approach.is_none());
    }

    #[test]
    fn resume_scoring_with_metrics_stays_at_scoring() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(None, Some(HashMap::new()), tmp.path().join("wt"));
        let state = make_state(Phase::Scoring, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Scoring);
    }

    #[test]
    fn resume_scoring_without_metrics_falls_back_to_measuring() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(None, None, tmp.path().join("wt"));
        let state = make_state(Phase::Scoring, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Measuring);
    }

    #[test]
    fn resume_scoring_without_approach_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Scoring, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
        assert!(result.current_approach.is_none());
    }

    #[test]
    fn resume_integrating_with_commit_stays_at_integrating() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(Some("abc"), None, tmp.path().join("wt"));
        let state = make_state(Phase::Integrating, Some(approach));
        store.save_state(&state).unwrap();
        // tmp.path() is not a git repo, so has_commits_ahead returns Err → unwrap_or(false) → stays at Integrating
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Integrating);
    }

    #[test]
    fn resume_integrating_without_commit_sha_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let approach = make_approach(None, None, tmp.path().join("wt"));
        let state = make_state(Phase::Integrating, Some(approach));
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
        assert!(result.current_approach.is_none());
    }

    #[test]
    fn resume_integrating_without_approach_resets_to_planning() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Integrating, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Planning);
        assert!(result.current_approach.is_none());
    }

    #[test]
    fn resume_recorded_stays_at_recorded() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Recorded, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Recorded);
    }

    #[test]
    fn resume_done_stays_at_done() {
        let tmp = tempdir().unwrap();
        let store = TaskStore::new(&tmp.path().join("task")).unwrap();
        let state = make_state(Phase::Done, None);
        store.save_state(&state).unwrap();
        let result = prepare_resume(&store, tmp.path()).unwrap();
        assert_eq!(result.current_phase, Phase::Done);
    }
}
