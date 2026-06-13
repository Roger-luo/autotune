//! Scenario test for `autotune revert`. Gated behind `--features mock` to sit
//! with the other scenario tests; this particular test drives the compiled
//! binary directly and exercises the no-conflict revert path (no mock agent).
#![cfg(feature = "mock")]

use assert_cmd::Command;
use std::path::Path;
use std::process::Command as StdCommand;

const CONFIG_TOML: &str = r#"
[task]
name = "revert-task"
description = "revert scenario"
canonical_branch = "main"
max_iterations = "10"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "echo-bench"
command = ["sh", "-c", "echo 'metric_value: 42.0'"]
timeout = 10
adaptor = { type = "regex", patterns = [{ name = "metric_value", pattern = "metric_value: ([0-9.]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric_value", direction = "Minimize", weight = 1.0 }]
guardrail_metrics = []
"#;

fn git(dir: &Path, args: &[&str]) {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev_parse_head(dir: &Path) -> String {
    let out = StdCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn scenario_revert_undoes_iteration_and_records_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Project files. .gitignore keeps .autotune/ out of the working tree status
    // so cmd_revert's clean-tree guard passes (real projects gitignore it).
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn v() -> i32 { 1 }\n").unwrap();
    std::fs::write(root.join(".autotune.toml"), CONFIG_TOML).unwrap();
    std::fs::write(root.join(".gitignore"), ".autotune/\ntarget/\n").unwrap();

    git(root, &["init"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial"]);
    git(root, &["branch", "-M", "main"]);

    // Advancing branch with one kept-iteration commit (changes v from 1 -> 2).
    git(root, &["checkout", "-b", "autotune/revert-task-main"]);
    std::fs::write(root.join("src/lib.rs"), "pub fn v() -> i32 { 2 }\n").unwrap();
    git(root, &["commit", "-am", "autotune: iteration 1 change"]);
    let kept_sha = rev_parse_head(root);

    // Seed task state + ledger AFTER commits (so the gitignored .autotune/
    // doesn't dirty the tree). Baseline + kept iter1 carrying the real SHA.
    let task_dir = root.join(".autotune/tasks/revert-task");
    std::fs::create_dir_all(task_dir.join("iterations")).unwrap();
    std::fs::write(task_dir.join("config_snapshot.toml"), CONFIG_TOML).unwrap();
    std::fs::write(
        task_dir.join("state.json"),
        r#"{"task_name":"revert-task","canonical_branch":"main","advancing_branch":"autotune/revert-task-main","research_session_id":"sid","research_backend":"claude","current_iteration":2,"current_phase":"planning","current_approach":null}"#,
    )
    .unwrap();
    let ledger = format!(
        r#"[{{"iteration":0,"approach":"baseline","status":"baseline","metrics":{{"metric_value":50.0}},"rank":0.0,"timestamp":"2026-04-15T00:00:00Z"}},
{{"iteration":1,"approach":"bump v","status":"kept","metrics":{{"metric_value":42.0}},"rank":0.1,"commit_sha":"{kept_sha}","timestamp":"2026-04-15T00:00:01Z"}}]"#
    );
    std::fs::write(task_dir.join("ledger.json"), ledger).unwrap();

    // Run the revert.
    let output = Command::cargo_bin("autotune")
        .unwrap()
        .args([
            "revert",
            "1",
            "--task",
            "revert-task",
            "--reason",
            "test revert",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "revert failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // (a) An inverse commit is on the advancing branch (file restored to 1).
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn v() -> i32 { 1 }\n"
    );

    // (b) A Reverted checkpoint row was appended pointing at iteration 1, with
    //     fresh re-measured metrics (42.0 from the echo bench). Parse the JSON.
    let ledger_after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(task_dir.join("ledger.json")).unwrap())
            .unwrap();
    let rows = ledger_after.as_array().unwrap();
    let last = rows.last().unwrap();
    assert_eq!(last["status"], "reverted", "ledger:\n{ledger_after:#}");
    assert_eq!(last["reverted_iteration"], 1, "ledger:\n{ledger_after:#}");
    assert_eq!(
        last["metrics"]["metric_value"], 42.0,
        "ledger:\n{ledger_after:#}"
    );
}

#[test]
fn scenario_revert_middle_iteration_no_conflict() {
    // Two kept iterations touch DIFFERENT files so reverting the first (middle)
    // does not conflict with the second. Proves the non-tip revert path end-to-end.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "1\n").unwrap();
    std::fs::write(root.join("src/b.rs"), "1\n").unwrap();
    std::fs::write(root.join(".autotune.toml"), CONFIG_TOML).unwrap();
    std::fs::write(root.join(".gitignore"), ".autotune/\ntarget/\n").unwrap();

    git(root, &["init"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial"]);
    git(root, &["branch", "-M", "main"]);

    // Advancing branch: commit 1 changes src/a.rs only (iter1).
    git(root, &["checkout", "-b", "autotune/revert-task-main"]);
    std::fs::write(root.join("src/a.rs"), "2\n").unwrap();
    git(
        root,
        &["commit", "-am", "autotune: iteration 1 change (a.rs)"],
    );
    let sha_a = rev_parse_head(root);

    // Commit 2 changes src/b.rs only (iter2).
    std::fs::write(root.join("src/b.rs"), "2\n").unwrap();
    git(
        root,
        &["commit", "-am", "autotune: iteration 2 change (b.rs)"],
    );
    let sha_b = rev_parse_head(root);

    // Seed task state + ledger after commits.
    let task_dir = root.join(".autotune/tasks/revert-task");
    std::fs::create_dir_all(task_dir.join("iterations")).unwrap();
    std::fs::write(task_dir.join("config_snapshot.toml"), CONFIG_TOML).unwrap();
    std::fs::write(
        task_dir.join("state.json"),
        r#"{"task_name":"revert-task","canonical_branch":"main","advancing_branch":"autotune/revert-task-main","research_session_id":"sid","research_backend":"claude","current_iteration":3,"current_phase":"planning","current_approach":null}"#,
    )
    .unwrap();
    let ledger = format!(
        r#"[{{"iteration":0,"approach":"baseline","status":"baseline","metrics":{{"metric_value":50.0}},"rank":0.0,"timestamp":"2026-04-15T00:00:00Z"}},
{{"iteration":1,"approach":"change-a","status":"kept","metrics":{{"metric_value":45.0}},"rank":0.05,"commit_sha":"{sha_a}","timestamp":"2026-04-15T00:00:01Z"}},
{{"iteration":2,"approach":"change-b","status":"kept","metrics":{{"metric_value":42.0}},"rank":0.08,"commit_sha":"{sha_b}","timestamp":"2026-04-15T00:00:02Z"}}]"#
    );
    std::fs::write(task_dir.join("ledger.json"), ledger).unwrap();

    // Run: revert the MIDDLE iteration (iter1, which changed src/a.rs).
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("autotune"))
        .args([
            "revert",
            "1",
            "--task",
            "revert-task",
            "--reason",
            "middle revert test",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "revert failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // src/a.rs should be restored to "1" (iter1 reverted).
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.rs")).unwrap(),
        "1\n",
        "src/a.rs should be restored to original after revert of iter1"
    );

    // src/b.rs should still be "2" (iter2 untouched).
    assert_eq!(
        std::fs::read_to_string(root.join("src/b.rs")).unwrap(),
        "2\n",
        "src/b.rs should remain at 2 — iter2 was not reverted"
    );

    // The ledger's last row should be a Reverted checkpoint pointing at iter1.
    let ledger_after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(task_dir.join("ledger.json")).unwrap())
            .unwrap();
    let rows = ledger_after.as_array().unwrap();
    let last = rows.last().unwrap();
    assert_eq!(last["status"], "reverted", "ledger:\n{ledger_after:#}");
    assert_eq!(last["reverted_iteration"], 1, "ledger:\n{ledger_after:#}");
}
