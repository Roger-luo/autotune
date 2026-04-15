use autotune_git::{
    branch_exists, checkout, conclude_merge, create_branch, create_branch_from, create_worktree,
    has_merge_conflicts, has_uncommitted_changes, list_conflicted_files, merge_abort,
    merge_ff_only, merge_or_conflict, rebase, rebase_abort, rebase_continue, remove_worktree,
    stage_all_and_commit,
};
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} failed", args);
}

fn write_file(path: &Path, content: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    init_repo_at(dir.path());
    dir
}

fn init_repo_at(path: &Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test User"]);
    git(path, &["config", "commit.gpgsign", "false"]);
    write_file(&path.join("README.md"), "base\n");
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-m", "initial"]);
}

#[test]
fn branch_exists_true_and_false() {
    let repo = init_repo();
    assert!(!branch_exists(repo.path(), "nonexistent").unwrap());
    create_branch(repo.path(), "my-branch").unwrap();
    assert!(branch_exists(repo.path(), "my-branch").unwrap());
}

#[test]
fn create_branch_from_start_point() {
    let repo = init_repo();
    create_branch_from(repo.path(), "from-main", "main").unwrap();
    assert!(branch_exists(repo.path(), "from-main").unwrap());
}

#[test]
fn has_uncommitted_changes_clean_and_dirty() {
    let repo = init_repo();
    assert!(!has_uncommitted_changes(repo.path()).unwrap());
    write_file(&repo.path().join("new.txt"), "content");
    assert!(has_uncommitted_changes(repo.path()).unwrap());
}

#[test]
fn stage_all_and_commit_leaves_clean() {
    let repo = init_repo();
    write_file(&repo.path().join("new.txt"), "content");
    assert!(has_uncommitted_changes(repo.path()).unwrap());
    stage_all_and_commit(repo.path(), "add new.txt").unwrap();
    assert!(!has_uncommitted_changes(repo.path()).unwrap());
}

#[test]
fn merge_or_conflict_clean_returns_true() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();
    checkout(repo.path(), "feature").unwrap();
    write_file(&repo.path().join("feat.txt"), "feature");
    git(repo.path(), &["add", "feat.txt"]);
    git(repo.path(), &["commit", "-m", "add feat"]);
    checkout(repo.path(), "main").unwrap();
    assert!(merge_or_conflict(repo.path(), "feature", "merge feature").unwrap());
}

#[test]
fn merge_or_conflict_conflict_and_abort() {
    let repo = init_repo();

    // Make branch-a change README.md
    create_branch(repo.path(), "branch-a").unwrap();
    checkout(repo.path(), "branch-a").unwrap();
    write_file(&repo.path().join("README.md"), "branch-a content\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "a changes"]);

    // Make branch-b change README.md from the same base
    checkout(repo.path(), "main").unwrap();
    create_branch(repo.path(), "branch-b").unwrap();
    checkout(repo.path(), "branch-b").unwrap();
    write_file(&repo.path().join("README.md"), "branch-b content\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "b changes"]);

    // Merge branch-a into main cleanly
    checkout(repo.path(), "main").unwrap();
    git(
        repo.path(),
        &["merge", "--no-ff", "-m", "merge a", "branch-a"],
    );

    // Clean repo has no conflicts
    assert!(!has_merge_conflicts(repo.path()).unwrap());
    let files = list_conflicted_files(repo.path()).unwrap();
    assert!(files.is_empty());

    // Merging branch-b into main (which already has a's README) causes conflict
    let result = merge_or_conflict(repo.path(), "branch-b", "merge b").unwrap();
    assert!(!result); // conflict

    // has_merge_conflicts now true
    assert!(has_merge_conflicts(repo.path()).unwrap());
    // list_conflicted_files contains README.md
    let files = list_conflicted_files(repo.path()).unwrap();
    assert!(files.iter().any(|f| f.contains("README")));

    // abort restores clean state
    merge_abort(repo.path()).unwrap();
    assert!(!has_merge_conflicts(repo.path()).unwrap());
}

#[test]
fn conclude_merge_resolves_conflict() {
    let repo = init_repo();

    create_branch(repo.path(), "branch-a").unwrap();
    checkout(repo.path(), "branch-a").unwrap();
    write_file(&repo.path().join("README.md"), "side-a\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "a"]);

    checkout(repo.path(), "main").unwrap();
    create_branch(repo.path(), "branch-b").unwrap();
    checkout(repo.path(), "branch-b").unwrap();
    write_file(&repo.path().join("README.md"), "side-b\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "b"]);

    checkout(repo.path(), "main").unwrap();
    git(
        repo.path(),
        &["merge", "--no-ff", "-m", "merge a", "branch-a"],
    );

    let ok = merge_or_conflict(repo.path(), "branch-b", "merge b").unwrap();
    assert!(!ok);

    // Resolve by writing a clean file
    write_file(&repo.path().join("README.md"), "resolved\n");
    conclude_merge(repo.path(), "resolved merge").unwrap();
    assert!(!has_uncommitted_changes(repo.path()).unwrap());
}

#[test]
fn rebase_clean_returns_true() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();
    checkout(repo.path(), "feature").unwrap();
    write_file(&repo.path().join("feat.txt"), "feature");
    git(repo.path(), &["add", "feat.txt"]);
    git(repo.path(), &["commit", "-m", "feature"]);
    // rebase feature onto main (which has no diverging commits — trivial rebase)
    let result = rebase(repo.path(), "main").unwrap();
    assert!(result);
}

#[test]
fn rebase_conflict_abort() {
    let repo = init_repo();

    create_branch(repo.path(), "feature").unwrap();

    // Add a commit to main that changes README.md
    checkout(repo.path(), "main").unwrap();
    write_file(&repo.path().join("README.md"), "main-update\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "main update"]);

    // On feature (which still points at initial): change README.md differently
    checkout(repo.path(), "feature").unwrap();
    write_file(&repo.path().join("README.md"), "feature-update\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "feature update"]);

    // Rebase feature onto main → conflict on README.md
    let result = rebase(repo.path(), "main").unwrap();
    assert!(!result); // conflict

    // Test rebase_abort
    rebase_abort(repo.path()).unwrap();
    // feature branch is restored to pre-rebase state
    let content = std::fs::read_to_string(repo.path().join("README.md")).unwrap();
    assert_eq!(content, "feature-update\n");
}

#[test]
fn rebase_continue_after_resolve() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();

    checkout(repo.path(), "main").unwrap();
    write_file(&repo.path().join("README.md"), "main-update\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "main update"]);

    checkout(repo.path(), "feature").unwrap();
    write_file(&repo.path().join("README.md"), "feature-update\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "feature update"]);

    let result = rebase(repo.path(), "main").unwrap();
    assert!(!result); // conflict

    // Resolve by writing clean content
    write_file(&repo.path().join("README.md"), "resolved\n");
    let done = rebase_continue(repo.path()).unwrap();
    assert!(done); // rebase completed
}

#[test]
fn merge_ff_only_fast_forwards() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();
    checkout(repo.path(), "feature").unwrap();
    write_file(&repo.path().join("feat.txt"), "feature");
    git(repo.path(), &["add", "feat.txt"]);
    git(repo.path(), &["commit", "-m", "add feat"]);
    checkout(repo.path(), "main").unwrap();
    merge_ff_only(repo.path(), "feature").unwrap();
    let content = std::fs::read_to_string(repo.path().join("feat.txt")).unwrap();
    assert_eq!(content, "feature");
}

#[test]
fn remove_worktree_succeeds() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();
    let wt_dir = tempfile::tempdir().unwrap();
    create_worktree(repo.path(), wt_dir.path(), "feature").unwrap();
    assert!(wt_dir.path().exists());
    remove_worktree(repo.path(), wt_dir.path()).unwrap();
    // After removal the directory is gone (git worktree remove deletes it)
    assert!(!wt_dir.path().exists());
    // Prevent tempdir from trying to remove a nonexistent directory
    std::mem::forget(wt_dir);
}
