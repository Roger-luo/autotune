//! Commit-harness preflight.
//!
//! The tune loop commits every candidate change through the project's git
//! hooks (the validation harness — rustfmt/clippy checks, license headers,
//! linters, …). If the *canonical* branch can't already pass those hooks for
//! the kind of files this task modifies, then **every** candidate commit will
//! be rejected for reasons the implementation agent cannot fix — and the loop
//! burns research + implementation on iterations that can only ever discard.
//!
//! Rather than discover this one wasted iteration at a time, we run the
//! project's pre-commit framework once up front (preferring `prek`, falling
//! back to `pre-commit`) and abort with a clear, actionable error if it fails.
//!
//! The check is scoped to the files matching the task's tunable globs (via
//! `git ls-files` pathspecs) so it exercises exactly the hooks a candidate
//! commit would trigger — e.g. a Rust-only task isn't blocked by a repo's
//! unrelated Python hooks, since the candidate commits never stage Python.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use autotune_agent::aprintln;

/// Setting this (to anything) skips the preflight — an escape hatch for repos
/// whose pre-commit runner can't be invoked headlessly, or where the check is
/// a known false positive.
const SKIP_ENV: &str = "AUTOTUNE_SKIP_HOOK_PREFLIGHT";

/// Cap on the number of files passed to the runner, to keep the command line
/// bounded. The workspace-wide hooks (`cargo fmt`/`clippy`, `pass_filenames =
/// false`) run regardless of how many files are passed, so a sample suffices.
const MAX_PROBE_FILES: usize = 100;

/// Is an executable named `prog` findable on `PATH`?
fn on_path(prog: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(prog).is_file())
}

/// Does the repo configure a pre-commit framework we know how to drive?
fn has_precommit_config(repo_root: &Path) -> bool {
    [
        "prek.toml",
        ".pre-commit-config.yaml",
        ".pre-commit-config.yml",
    ]
    .iter()
    .any(|f| repo_root.join(f).is_file())
}

/// Resolve the pre-commit runner to use for this repo, if any.
///
/// Returns `None` when the repo has no pre-commit config, or when neither
/// `prek` nor `pre-commit` is installed — in both cases there's nothing for
/// the preflight to verify.
pub fn resolve_precommit_runner(repo_root: &Path) -> Option<&'static str> {
    if !has_precommit_config(repo_root) {
        return None;
    }
    if on_path("prek") {
        Some("prek")
    } else if on_path("pre-commit") {
        Some("pre-commit")
    } else {
        None
    }
}

/// Build a `GlobSet` from gitignore-style globs (`*` is single-segment, `**`
/// crosses directories). Invalid globs are skipped; an all-skip build yields an
/// empty set that matches nothing.
fn build_globset(globs: &[String]) -> globset::GlobSet {
    let mut builder = globset::GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = globset::GlobBuilder::new(g).literal_separator(true).build() {
            builder.add(glob);
        }
    }
    builder
        .build()
        .unwrap_or_else(|_| globset::GlobSet::empty())
}

/// Tracked files matching the `tunable` globs, with the `denied` globs filtered
/// out. Capped to `limit`.
///
/// The positive globs are matched by `git ls-files` (so only *tracked* files
/// count); `denied` is then applied in Rust with `globset`. We deliberately do
/// NOT pass git `:(exclude,…)` pathspecs: git 2.50 (Apple) was observed in a
/// large repo to return **zero** results whenever any exclude pathspec is
/// combined with a `:(glob)` positive — even a non-matching exclude — which
/// silently emptied this list and made the whole preflight a no-op for every
/// config that sets `denied` paths. Filtering in Rust is portable and doesn't
/// depend on that pathspec behavior.
fn tunable_files(
    repo_root: &Path,
    tunable: &[String],
    denied: &[String],
    limit: usize,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["ls-files".into(), "-z".into(), "--".into()];
    for g in tunable {
        args.push(format!(":(glob){g}"));
    }
    let Ok(out) = Command::new("git")
        .args(&args)
        .current_dir(repo_root)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let denied_set = build_globset(denied);
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .filter(|&p| !denied_set.is_match(p))
        .take(limit)
        .map(str::to_string)
        .collect()
}

/// Result of invoking the pre-commit runner.
#[derive(Debug)]
pub enum PrecommitOutcome {
    Passed,
    Failed { output: String },
}

/// Run `<runner> run --files <files…>` in `repo_root`, returning whether all
/// hooks passed and (on failure) the captured output.
pub fn run_precommit_check(
    repo_root: &Path,
    runner: &str,
    files: &[String],
) -> Result<PrecommitOutcome> {
    let mut cmd = Command::new(runner);
    cmd.arg("run").arg("--files");
    cmd.args(files);
    cmd.current_dir(repo_root);
    let out = cmd
        .output()
        .with_context(|| format!("failed to run pre-commit runner '{runner}'"))?;
    if out.status.success() {
        Ok(PrecommitOutcome::Passed)
    } else {
        let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.trim().is_empty() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&stderr);
        }
        Ok(PrecommitOutcome::Failed { output })
    }
}

/// Verify the canonical working tree passes the project's pre-commit hooks for
/// the file types this task modifies. Returns `Ok(())` when the hooks pass, or
/// when there's nothing to check (no framework, no runner, no tunable files, or
/// the check was skipped via [`SKIP_ENV`]). Returns `Err` with an actionable
/// message when the hooks reject the (unmodified) canonical tree.
pub fn check_commit_harness(repo_root: &Path, tunable: &[String], denied: &[String]) -> Result<()> {
    if std::env::var_os(SKIP_ENV).is_some() {
        aprintln!(
            "[autotune] skipping commit-harness preflight ({} is set)",
            SKIP_ENV
        );
        return Ok(());
    }
    let Some(runner) = resolve_precommit_runner(repo_root) else {
        // No pre-commit framework configured, or no runner installed — the
        // candidate commits won't be gated by a framework we can verify.
        return Ok(());
    };
    let files = tunable_files(repo_root, tunable, denied, MAX_PROBE_FILES);
    if files.is_empty() {
        // Nothing to scope the check to; skip rather than fall back to the
        // whole-repo suite (which may include hooks unrelated to this task).
        return Ok(());
    }
    aprintln!(
        "[autotune] preflight: checking the commit harness ({}) against {} tunable file(s)...",
        runner,
        files.len()
    );
    match run_precommit_check(repo_root, runner, &files)? {
        PrecommitOutcome::Passed => {
            aprintln!("[autotune] preflight: commit harness is green");
            Ok(())
        }
        PrecommitOutcome::Failed { output } => {
            bail!(
                "the canonical branch does not pass its own pre-commit hooks ({runner}) for the \
                 file types this task modifies, so every candidate commit would be rejected by \
                 the validation harness for reasons the implementation agent cannot fix.\n\n\
                 Fix the failing hooks on the canonical branch first, then re-run. To skip this \
                 preflight, set {SKIP_ENV}=1.\n\n\
                 --- {runner} run --files (scoped to this task's tunable files) ---\n{output}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t.t"]);
        git(dir, &["config", "user.name", "t"]);
    }

    #[test]
    fn resolve_runner_is_none_without_config() {
        let dir = tempfile::tempdir().unwrap();
        // No prek.toml / .pre-commit-config.yaml present.
        assert!(resolve_precommit_runner(dir.path()).is_none());
    }

    #[test]
    fn tunable_files_matches_globs_and_filters_denied() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/snapshots")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("src/sub/b.rs"), "fn b() {}\n").unwrap();
        std::fs::write(dir.path().join("src/snapshots/snap.rs"), "// snap\n").unwrap();
        std::fs::write(dir.path().join("src/c.txt"), "no\n").unwrap();
        std::fs::write(dir.path().join("target/d.rs"), "fn d() {}\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "init"]);

        // `src/snapshots/snap.rs` matches the tunable glob but is denied, so the
        // Rust-side filter (not a git exclude pathspec) must drop it.
        let files = tunable_files(
            dir.path(),
            &["src/**/*.rs".to_string()],
            &["target/**".to_string(), "src/snapshots/**".to_string()],
            100,
        );
        assert!(files.contains(&"src/a.rs".to_string()), "got {files:?}");
        assert!(files.contains(&"src/sub/b.rs".to_string()), "got {files:?}");
        assert!(
            !files.iter().any(|f| f.contains("snapshots")),
            "denied snapshots path must be filtered, got {files:?}"
        );
        assert!(!files.iter().any(|f| f.ends_with(".txt")), "got {files:?}");
        assert!(
            !files.iter().any(|f| f.starts_with("target/")),
            "got {files:?}"
        );
    }

    #[test]
    fn run_precommit_check_passes_on_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        // `true` ignores its args and exits 0 — stands in for a green runner.
        let outcome = run_precommit_check(dir.path(), "true", &["x.rs".to_string()]).unwrap();
        assert!(matches!(outcome, PrecommitOutcome::Passed));
    }

    #[test]
    fn run_precommit_check_fails_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        // `false` ignores its args and exits 1 — stands in for a red runner.
        let outcome = run_precommit_check(dir.path(), "false", &["x.rs".to_string()]).unwrap();
        assert!(matches!(outcome, PrecommitOutcome::Failed { .. }));
    }

    #[test]
    fn check_commit_harness_ok_when_no_framework() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        // No prek.toml / .pre-commit-config.yaml → nothing to verify → Ok.
        check_commit_harness(dir.path(), &["src/**/*.rs".to_string()], &[]).unwrap();
    }
}
