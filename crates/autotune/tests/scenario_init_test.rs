//! Scenario test for the agent-assisted init pipeline.
//!
//! Requires: `cargo nextest run --features mock -E 'test(scenario_)'`

#![cfg(feature = "mock")]

use assert_cmd::Command;
use std::path::Path;

/// Set up a temporary directory as a git repo with some source files.
fn setup_mock_project(dir: &Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "sample-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn compute(n: u64) -> u64 { (0..n).sum() }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() { println!(\"result: {}\", sample_project::compute(1000)); }\n",
    )
    .unwrap();

    // Initialize git repo (required by autotune's find_repo_root)
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init failed");
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .expect("git add failed");
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(dir)
        .output()
        .expect("git commit failed");
}

#[test]
fn scenario_init_creates_config_and_baseline() {
    let dir = tempfile::tempdir().unwrap();
    setup_mock_project(dir.path());

    // The mock agent conversation needs user input:
    // 1. Answer to "what to optimize?" question (select by key)
    // 2. Answer to "how to measure?" question (select by key)
    // 3. Approve the final config ("yes")
    // Config sections are auto-accepted by the CLI without user input.
    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(dir.path())
        .write_stdin("perf\nbench\nyes\n")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "autotune init failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Verify mock agent was used
    assert!(
        stderr.contains("mock"),
        "expected mock agent indicator in stderr.\nstderr:\n{stderr}"
    );

    // Verify config sections were accepted
    assert!(
        stdout.contains("experiment") && stdout.contains("accepted"),
        "expected experiment accepted.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("paths") && stdout.contains("accepted"),
        "expected paths accepted.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("benchmark") && stdout.contains("accepted"),
        "expected benchmark accepted.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("score") && stdout.contains("accepted"),
        "expected score accepted.\nstdout:\n{stdout}"
    );

    // Verify .autotune.toml was written
    let config_path = dir.path().join(".autotune.toml");
    assert!(config_path.exists(), ".autotune.toml should exist");
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    assert!(config_content.contains("mock-experiment"));
    assert!(config_content.contains("time_us"));

    // Verify the config is valid TOML that parses
    let parsed: toml::Value = toml::from_str(&config_content).unwrap();
    assert_eq!(
        parsed["experiment"]["name"].as_str().unwrap(),
        "mock-experiment"
    );

    // Verify experiment was initialized (baseline recorded)
    assert!(
        stdout.contains("experiment") && stdout.contains("initialized"),
        "expected experiment initialization.\nstdout:\n{stdout}"
    );

    // Verify experiment directory and ledger
    let experiment_dir = dir
        .path()
        .join(".autotune")
        .join("experiments")
        .join("mock-experiment");
    assert!(experiment_dir.exists(), "experiment directory should exist");

    let ledger_path = experiment_dir.join("ledger.json");
    assert!(ledger_path.exists(), "ledger.json should exist");
    let ledger_content = std::fs::read_to_string(&ledger_path).unwrap();
    assert!(ledger_content.contains("baseline"));
    assert!(ledger_content.contains("time_us"));
}

#[test]
fn scenario_init_with_existing_config_skips_agent() {
    let dir = tempfile::tempdir().unwrap();
    setup_mock_project(dir.path());

    // Write a pre-existing .autotune.toml
    std::fs::write(
        dir.path().join(".autotune.toml"),
        r#"[experiment]
name = "existing-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[benchmark]]
name = "bench1"
command = ["echo", "time: 42.0 us"]
adaptor = { type = "regex", patterns = [{ name = "time_us", pattern = 'time: ([0-9.]+)' }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "time_us", direction = "Minimize" }]
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "autotune init failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Should NOT have triggered agent-assisted init
    assert!(
        !stdout.contains("agent-assisted init"),
        "should skip agent when config exists.\nstdout:\n{stdout}"
    );

    // Should have initialized the experiment directly
    assert!(
        stdout.contains("existing-exp") && stdout.contains("initialized"),
        "expected direct init.\nstdout:\n{stdout}"
    );
}

#[test]
fn scenario_init_failure_shows_useful_error() {
    let dir = tempfile::tempdir().unwrap();
    setup_mock_project(dir.path());

    // Run without AUTOTUNE_MOCK but with a bogus claude path so the agent fails.
    // This tests that the error message is informative.
    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("PATH", "") // claude won't be found
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should fail, not succeed
    assert!(
        !output.status.success(),
        "expected failure when agent backend is unavailable"
    );

    // Error should mention something useful — not just "exit status: 1"
    let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), stderr);
    assert!(
        combined.contains("Error") || combined.contains("error"),
        "expected error in output.\nstdout+stderr:\n{combined}"
    );
}

#[test]
fn scenario_init_question_shows_text_and_options() {
    let dir = tempfile::tempdir().unwrap();
    setup_mock_project(dir.path());

    // Provide full piped input: answer to Q1, answer to Q2, approval
    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(dir.path())
        .write_stdin("perf\nbench\nyes\n")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "autotune init failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Question text should contain reasoning context (not just bare options)
    assert!(
        stdout.contains("Rust workspace") || stdout.contains("crates"),
        "question text should mention codebase findings.\nstdout:\n{stdout}"
    );

    // Options should be rendered with labels and descriptions
    assert!(
        stdout.contains("Runtime performance"),
        "expected option label in output.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Binary size"),
        "expected option label in output.\nstdout:\n{stdout}"
    );
}

#[test]
fn scenario_init_graceful_exit_on_eof() {
    let dir = tempfile::tempdir().unwrap();
    setup_mock_project(dir.path());

    // Provide no stdin input — the process will get EOF when trying to read
    // user answers. This simulates the user closing the terminal / pipe.
    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(dir.path())
        .write_stdin("") // EOF immediately
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should exit without panic or crash — either clean cancellation or an error
    // The key assertion: no panic backtrace in output
    let combined = format!("{stdout}{stderr}");
    assert!(
        !combined.contains("panicked") && !combined.contains("RUST_BACKTRACE"),
        "should not panic on EOF.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

// TODO: test real Ctrl+C (SIGINT) via PTY once the `scenario` crate
// publishes its interactive session support (Roger-luo/Ion#118).
// That would test: spawn PTY → expect question text → send Ctrl+C →
// verify "[autotune] init cancelled" in output.
