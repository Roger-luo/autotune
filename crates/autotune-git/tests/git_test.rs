use autotune_git::{
    GitError, checkout, cherry_pick, create_branch, create_worktree, current_branch,
    has_commits_ahead, head_sha, latest_commit_sha, merge, remove_worktree, repo_root, revert_last,
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
    write_file(&path.join("README.md"), "base\n");
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-m", "initial"]);
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
    let short_sha = head_sha(repo.path()).unwrap();
    let full_sha = latest_commit_sha(repo.path()).unwrap();
    assert!(!short_sha.is_empty());
    assert!(full_sha.starts_with(&short_sha));
    assert!(short_sha.len() <= full_sha.len());
    assert_eq!(full_sha.len(), 40);
}

#[test]
fn repo_root_rejects_non_repo() {
    let dir = tempfile::tempdir().unwrap();
    let err = repo_root(dir.path()).unwrap_err();
    assert!(matches!(err, GitError::CommandFailed { .. }));
}

#[test]
fn repo_root_preserves_path_whitespace() {
    let base = tempfile::tempdir().unwrap();
    let repo_path = base.path().join(" repo with spaces ");
    std::fs::create_dir(&repo_path).unwrap();
    init_repo_at(&repo_path);

    assert_eq!(
        std::fs::canonicalize(repo_root(&repo_path).unwrap()).unwrap(),
        std::fs::canonicalize(&repo_path).unwrap()
    );
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

#[test]
fn revert_last_reverts_merge_commit() {
    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();

    checkout(repo.path(), "feature").unwrap();
    write_file(&repo.path().join("feature.txt"), "feature\n");
    git(repo.path(), &["add", "feature.txt"]);
    git(repo.path(), &["commit", "-m", "feature commit"]);

    checkout(repo.path(), "main").unwrap();
    merge(repo.path(), "feature", "merge feature").unwrap();
    let merge_commit = latest_commit_sha(repo.path()).unwrap();

    revert_last(repo.path()).unwrap();

    let content = std::fs::read_to_string(repo.path().join("feature.txt")).unwrap_or_default();
    assert!(content.is_empty());
    assert_ne!(latest_commit_sha(repo.path()).unwrap(), merge_commit);
}

#[cfg(unix)]
#[test]
fn worktree_paths_allow_non_utf8() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo = init_repo();
    create_branch(repo.path(), "feature").unwrap();

    let mut raw = repo.path().as_os_str().as_encoded_bytes().to_vec();
    raw.push(b'/');
    raw.extend_from_slice(b"wt-\xff");
    let worktree_path = std::path::PathBuf::from(OsString::from_vec(raw));

    let create_err = create_worktree(repo.path(), &worktree_path, "feature").unwrap_err();
    if let GitError::CommandFailed { command, .. } = create_err {
        assert!(command.contains("wt-"));
        assert_ne!(command, "git worktree add  feature");
    } else {
        panic!("expected command failure");
    }

    let remove_err = remove_worktree(repo.path(), &worktree_path).unwrap_err();
    if let GitError::CommandFailed { command, .. } = remove_err {
        assert!(command.contains("wt-"));
        assert_ne!(command, "git worktree remove  --force");
    } else {
        panic!("expected command failure");
    }
}
