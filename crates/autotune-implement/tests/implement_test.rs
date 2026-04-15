use autotune_agent::ToolPermission;
use autotune_implement::{
    Hypothesis, build_implementation_prompt, implementation_agent_permissions,
};

fn sample_hypothesis() -> Hypothesis {
    Hypothesis {
        approach: "loop-unrolling".to_string(),
        hypothesis: "Unrolling the inner loop will reduce branch overhead".to_string(),
        files_to_modify: vec!["src/engine.rs".to_string(), "src/utils.rs".to_string()],
    }
}

#[test]
fn prompt_includes_hypothesis_details() {
    let h = sample_hypothesis();
    let prompt = build_implementation_prompt(&h, "", &[]);

    assert!(prompt.contains("loop-unrolling"));
    assert!(prompt.contains("Unrolling the inner loop will reduce branch overhead"));
    assert!(prompt.contains("src/engine.rs"));
    assert!(prompt.contains("src/utils.rs"));
}

#[test]
fn prompt_includes_log_content_when_nonempty() {
    let h = sample_hypothesis();
    let prompt = build_implementation_prompt(&h, "Previous run showed 5% regression.", &[]);

    assert!(prompt.contains("Prior findings from log.md"));
    assert!(prompt.contains("Previous run showed 5% regression."));
}

#[test]
fn prompt_excludes_log_section_when_empty() {
    let h = sample_hypothesis();
    let prompt = build_implementation_prompt(&h, "", &[]);

    assert!(!prompt.contains("Prior findings"));
}

#[test]
fn prompt_includes_rules() {
    let h = sample_hypothesis();
    let prompt = build_implementation_prompt(&h, "", &[]);

    assert!(prompt.contains("Do NOT run tests"));
    assert!(prompt.contains("Do NOT try to commit"));
    assert!(prompt.contains("SUMMARY:"));
}

#[test]
fn prompt_includes_denied_paths_when_present() {
    let h = sample_hypothesis();
    let denied = vec!["**/tests/**".to_string(), "benches/**".to_string()];
    let prompt = build_implementation_prompt(&h, "", &denied);

    assert!(prompt.contains("denied patterns"));
    assert!(prompt.contains("`**/tests/**`"));
    assert!(prompt.contains("`benches/**`"));
}

#[test]
fn prompt_omits_denied_section_when_empty() {
    let h = sample_hypothesis();
    let prompt = build_implementation_prompt(&h, "", &[]);

    assert!(!prompt.contains("denied"));
}

#[test]
fn permissions_have_correct_counts() {
    let tunable = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let perms = implementation_agent_permissions(&tunable);

    let allow_count = perms
        .iter()
        .filter(|p| matches!(p, ToolPermission::Allow(_)))
        .count();
    let scoped_count = perms
        .iter()
        .filter(|p| matches!(p, ToolPermission::AllowScoped(_, _)))
        .count();
    let deny_count = perms
        .iter()
        .filter(|p| matches!(p, ToolPermission::Deny(_)))
        .count();

    assert_eq!(allow_count, 3, "should have 3 Allow permissions");
    assert_eq!(
        scoped_count,
        tunable.len() * 2,
        "should have N*2 AllowScoped permissions"
    );
    assert_eq!(deny_count, 4, "should have 4 Deny permissions");
}

#[test]
fn setup_worktree_creates_branch_and_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    // Initialise a git repo with at least one commit.
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::fs::write(repo.join("dummy.txt"), "hello").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&repo)
        .output()
        .unwrap();

    let worktree_parent = tmp.path().join("worktrees");
    std::fs::create_dir_all(&worktree_parent).unwrap();

    let (wt_path, branch) =
        autotune_implement::setup_worktree(&repo, "demo", "fast-path", &worktree_parent, "main")
            .unwrap();

    assert_eq!(branch, "autotune/demo/fast-path");
    assert!(wt_path.exists(), "worktree directory should exist");
    assert!(
        wt_path.join("dummy.txt").exists(),
        "worktree should contain repo files"
    );
}

/// Approach names with spaces, commas, and special characters are slugified
/// into valid git branch names.
#[test]
fn setup_worktree_slugifies_approach_name() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::fs::write(repo.join("dummy.txt"), "hello").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let _ = std::process::Command::new("git")
        .args(["branch", "-M", "main"])
        .current_dir(&repo)
        .output();

    let worktree_parent = tmp.path().join("worktrees");
    std::fs::create_dir_all(&worktree_parent).unwrap();

    let (_wt_path, branch) = autotune_implement::setup_worktree(
        &repo,
        "demo",
        "Add unit tests for X, Y & Z!",
        &worktree_parent,
        "main",
    )
    .unwrap();

    // Should be slugified: lowercase, hyphens, no spaces/commas/special chars
    assert_eq!(branch, "autotune/demo/add-unit-tests-for-x-y-z");
}
