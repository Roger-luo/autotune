//! Scenario tests for the agent-assisted init pipeline.
//!
//! Requires: `cargo nextest run --features mock -E 'test(scenario_)'`
//!
//! Uses `scenario` crate for PTY-based interactive tests and `assert_cmd`
//! for piped stdin tests.

#![cfg(feature = "mock")]

use assert_cmd::Command;
use scenario::{Project, Scenario, Terminal};
use std::path::Path;
use std::time::Duration;

fn autotune_bin() -> String {
    env!("CARGO_BIN_EXE_autotune").to_string()
}

/// Build a mock project fixture with git init.
fn mock_project() -> Project {
    let project = Project::empty()
        .file(
            "Cargo.toml",
            r#"[package]
name = "sample-project"
version = "0.1.0"
edition = "2021"
"#,
        )
        .file(
            "src/lib.rs",
            "pub fn compute(n: u64) -> u64 { (0..n).sum() }\n",
        )
        .file(
            "src/main.rs",
            "fn main() { println!(\"result: {}\", sample_project::compute(1000)); }\n",
        )
        .build()
        .unwrap();

    // Initialize git repo (required by autotune's find_repo_root)
    git_init(project.path());
    project
}

fn git_init(dir: &Path) {
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

// --- Piped stdin tests (non-interactive, via assert_cmd) ---

#[test]
fn scenario_init_creates_config_and_baseline() {
    let project = mock_project();

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .write_stdin("perf\nbench\nyes\n")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "autotune init failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    assert!(
        stderr.contains("mock"),
        "expected mock agent indicator.\nstderr:\n{stderr}"
    );

    // Verify .autotune.toml was written with correct content
    let config_path = project.path().join(".autotune.toml");
    assert!(config_path.exists(), ".autotune.toml should exist");
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    assert!(config_content.contains("mock-experiment"));

    // Verify experiment initialized
    let experiment_dir = project.path().join(".autotune/experiments/mock-experiment");
    assert!(experiment_dir.exists(), "experiment directory should exist");
    assert!(
        experiment_dir.join("ledger.json").exists(),
        "ledger should exist"
    );
}

#[test]
fn scenario_init_with_existing_config_skips_agent() {
    let project = mock_project();

    std::fs::write(
        project.path().join(".autotune.toml"),
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
        .current_dir(project.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("existing-exp") && stdout.contains("initialized"),
        "expected direct init.\nstdout:\n{stdout}"
    );
}

#[test]
fn scenario_init_graceful_exit_on_eof() {
    let project = mock_project();

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .write_stdin("")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !format!("{stdout}{stderr}").contains("panicked"),
        "should not panic on EOF.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

// --- PTY-based interactive tests (via scenario crate) ---

#[test]
fn scenario_pty_question_shows_text_and_options() {
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Wait for the first question — should contain context about the codebase
    session.expect("What metric").unwrap();

    // Verify option labels are shown
    session.expect("Runtime performance").unwrap();

    // Select first option by pressing Enter (default selection)
    session.send_line("").unwrap();

    // Wait for the second question
    session.expect("How should we measure").unwrap();

    // Select first option
    session.send_line("").unwrap();

    // Wait for the config approval prompt
    session.expect("Approve").unwrap();

    // Approve
    session.send_line("y").unwrap();

    // Wait for completion
    let output = session.wait().unwrap();
    assert!(
        output.success(),
        "init should succeed.\noutput:\n{}",
        output.stdout()
    );
}

#[test]
fn scenario_pty_arrow_keys_navigate_options() {
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Wait for first question
    session.expect("What metric").unwrap();

    // Press Down arrow twice to move to third option, then Enter
    session.send(b"\x1b[B").unwrap(); // Down
    session.send(b"\x1b[B").unwrap(); // Down
    session.send(b"\r").unwrap(); // Enter

    // Wait for second question
    session.expect("How should we measure").unwrap();

    // Select first option
    session.send(b"\r").unwrap();

    // Approve config
    session.expect("Approve").unwrap();
    session.send_line("y").unwrap();

    let output = session.wait().unwrap();
    assert!(
        output.success(),
        "init should succeed.\noutput:\n{}",
        output.stdout()
    );
}

#[test]
fn scenario_pty_free_text_input() {
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Wait for first question
    session.expect("What metric").unwrap();

    // Navigate to "Type your own answer..." (last option)
    for _ in 0..5 {
        session.send(b"\x1b[B").unwrap(); // Down
    }
    // Press Enter to activate text input
    session.send(b"\r").unwrap();

    // Type a custom answer
    std::thread::sleep(Duration::from_millis(100));
    session.send_line("memory usage").unwrap();

    // Wait for second question
    session.expect("How should we measure").unwrap();

    // Select first option
    session.send(b"\r").unwrap();

    // Approve config
    session.expect("Approve").unwrap();
    session.send_line("y").unwrap();

    let output = session.wait().unwrap();
    assert!(
        output.success(),
        "init should succeed with free text.\noutput:\n{}",
        output.stdout()
    );
}

#[test]
fn scenario_pty_ctrl_c_cancels_cleanly() {
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Wait for first question to appear
    session.expect("What metric").unwrap();

    // Send Ctrl+C
    session.send(b"\x03").unwrap();

    // Process should exit
    let output = session.wait().unwrap();

    // Should show cancellation message, not crash
    let text = output.stdout();
    assert!(
        !text.contains("panicked"),
        "should not panic on Ctrl+C.\noutput:\n{text}"
    );
    assert!(
        text.contains("cancelled") || text.contains("canceled"),
        "expected cancellation message.\noutput:\n{text}"
    );
}
