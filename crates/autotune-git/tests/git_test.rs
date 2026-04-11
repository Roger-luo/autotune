use autotune_git::{
    checkout, cherry_pick, create_branch, create_worktree, current_branch, has_commits_ahead,
    head_sha, latest_commit_sha, repo_root,
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
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test User"]);
    write_file(&dir.path().join("README.md"), "base\n");
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "-m", "initial"]);
    dir
}

#[test]
fn repo_root_and_heads() {
    let repo = init_repo();
    let nested = repo.path().join("nested").join("dir");
    std::fs::create_dir_all(&nested).unwrap();

    assert_eq!(
        std::fs::canonicalize(repo_root(&nested).unwrap()).unwrap(),
        std::fs::canonicalize(repo.path()).unwrap()
    );
    assert_eq!(current_branch(repo.path()).unwrap(), "main");
    assert_eq!(head_sha(repo.path()).unwrap().len(), 7);
    assert_eq!(latest_commit_sha(repo.path()).unwrap().len(), 40);
}

#[test]
fn create_branch_worktree_and_ahead() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();

    let worktree = tempfile::tempdir().unwrap();
    create_worktree(repo.path(), worktree.path(), "feature").unwrap();
    assert_eq!(current_branch(worktree.path()).unwrap(), "feature");

    write_file(&worktree.path().join("README.md"), "feature\n");
    git(worktree.path(), &["add", "README.md"]);
    git(worktree.path(), &["commit", "-m", "feature change"]);

    assert!(has_commits_ahead(repo.path(), "main", "feature").unwrap());
}

#[test]
fn cherry_pick_commit() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();
    checkout(repo.path(), "feature").unwrap();

    write_file(&repo.path().join("README.md"), "feature\n");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "feature change"]);
    let commit = latest_commit_sha(repo.path()).unwrap();

    checkout(repo.path(), "main").unwrap();
    cherry_pick(repo.path(), &commit).unwrap();

    let content = std::fs::read_to_string(repo.path().join("README.md")).unwrap();
    assert_eq!(content, "feature\n");
}
