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
        .write_stdin("optimize performance\nperf\nbench\nyes\n")
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
    assert!(config_content.contains("mock-task"));

    // Verify task initialized
    let task_dir = project.path().join(".autotune/tasks/mock-task");
    assert!(task_dir.exists(), "task directory should exist");
    assert!(task_dir.join("ledger.json").exists(), "ledger should exist");
}

#[test]
fn scenario_init_with_existing_config_skips_agent() {
    let project = mock_project();

    std::fs::write(
        project.path().join(".autotune.toml"),
        r#"[task]
name = "existing-exp"
max_iterations = "5"

[paths]
tunable = ["src/**"]

[[measure]]
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

    // Answer the user goal prompt first
    session
        .expect("What would you like autotune to do")
        .unwrap();
    session.send_line("optimize performance").unwrap();

    // Wait for the first agent question — should contain context about the codebase
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

    // Answer user goal prompt
    session
        .expect("What would you like autotune to do")
        .unwrap();
    session.send_line("optimize performance").unwrap();

    // Wait for first question
    session.expect("What metric").unwrap();

    // Press Down arrow twice to move to third option, then Enter
    session.send(b"\x1b[B").unwrap(); // Down
    session.send(b"\x1b[B").unwrap(); // Down
    session.send(b"\r").unwrap(); // Enter

    // Wait for second question
    session.expect("How should we measure").unwrap();

    // After the menu clears and the next question renders, the old menu
    // options should not be visible in subsequent output. Check that the
    // first question's options don't leak into the second question's area.
    let output_so_far = session.current_output();
    // Find text after "How should we measure" — first question options shouldn't appear there
    if let Some(pos) = output_so_far.find("How should we measure") {
        let after_second_q = &output_so_far[pos..];
        assert!(
            !after_second_q.contains("Runtime performance"),
            "first question's options leaked into second question area.\noutput:\n{output_so_far}"
        );
    }

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

    // Answer user goal prompt
    session
        .expect("What would you like autotune to do")
        .unwrap();
    session.send_line("optimize performance").unwrap();

    // Wait for first question
    session.expect("What metric").unwrap();
    session.expect("Type your own answer").unwrap();

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

    // After the first menu clears, its options should not appear in the
    // second question's output area. This catches rendering artifacts.
    let output_so_far = session.current_output();
    if let Some(pos) = output_so_far.find("How should we measure") {
        let after_second_q = &output_so_far[pos..];
        assert!(
            !after_second_q.contains("Runtime performance"),
            "first question's options leaked after text input.\noutput:\n{output_so_far}"
        );
    }

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

    // Answer user goal prompt to get to the agent conversation
    session
        .expect("What would you like autotune to do")
        .unwrap();
    session.send_line("optimize performance").unwrap();

    // Wait for first agent question to appear
    session.expect("What metric").unwrap();

    // Send Ctrl+C
    session.send(b"\x03").unwrap();

    // Process should exit
    let output = session.wait().unwrap();

    // Should not crash or corrupt terminal
    let text = output.stdout();
    assert!(
        !text.contains("panicked"),
        "should not panic on Ctrl+C.\noutput:\n{text}"
    );
}

#[test]
fn scenario_pty_ctrl_c_during_text_input() {
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Answer user goal prompt
    session
        .expect("What would you like autotune to do")
        .unwrap();
    session.send_line("optimize performance").unwrap();

    // Wait for first question
    session.expect("What metric").unwrap();
    session.expect("Type your own answer").unwrap();

    // Navigate to "Type your own answer..." and enter text mode
    for _ in 0..5 {
        session.send(b"\x1b[B").unwrap(); // Down
    }
    session.send(b"\r").unwrap(); // Enter to activate text input

    // Start typing
    std::thread::sleep(Duration::from_millis(100));
    session.send(b"some text").unwrap();
    std::thread::sleep(Duration::from_millis(100));

    // Ctrl+C while in text input mode
    session.send(b"\x03").unwrap();

    let output = session.wait().unwrap();
    let text = output.stdout();
    assert!(
        !text.contains("panicked"),
        "should not panic on Ctrl+C during text input.\noutput:\n{text}"
    );
}

#[test]
fn scenario_pty_ctrl_c_during_approval_prompt() {
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Answer user goal prompt
    session
        .expect("What would you like autotune to do")
        .unwrap();
    session.send_line("optimize performance").unwrap();

    // Answer both questions to reach the approval prompt
    session.expect("What metric").unwrap();
    session.send(b"\r").unwrap(); // Select first option

    session.expect("How should we measure").unwrap();
    session.send(b"\r").unwrap(); // Select first option

    // Wait for approval prompt
    session.expect("Approve").unwrap();

    // Ctrl+C during the approval prompt (dialoguer Confirm)
    session.send(b"\x03").unwrap();

    let output = session.wait().unwrap();
    let text = output.stdout();
    assert!(
        !text.contains("panicked"),
        "should not panic on Ctrl+C during approval.\noutput:\n{text}"
    );
}

#[test]
fn scenario_pty_ctrl_c_at_user_goal_prompt() {
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Wait for the user goal prompt
    session
        .expect("What would you like autotune to do")
        .unwrap();

    // Ctrl+C before typing anything
    session.send(b"\x03").unwrap();

    let output = session.wait().unwrap();
    let text = output.stdout();
    assert!(
        !text.contains("panicked"),
        "should not panic on Ctrl+C at user goal prompt.\noutput:\n{text}"
    );
}

#[test]
fn scenario_pty_narrow_terminal_completes_without_corruption() {
    // Regression test: long option text in a narrow terminal should be
    // truncated and not corrupt the rendering. Verifies the select widget
    // works in a 60-column terminal with arrow key navigation.
    let project = mock_project();

    let mut session = Scenario::new(autotune_bin())
        .arg("init")
        .env("AUTOTUNE_MOCK", "1")
        .current_dir(project.path())
        .terminal(Terminal::pty(60, 40))
        .timeout(Duration::from_secs(10))
        .spawn()
        .unwrap();

    // Answer user goal prompt
    session
        .expect("What would you like autotune to do")
        .unwrap();
    session.send_line("optimize performance").unwrap();

    // Wait for first question with options
    session.expect("What metric").unwrap();

    // Press arrow keys several times — this would corrupt a non-truncated menu
    for _ in 0..6 {
        session.send(b"\x1b[B").unwrap(); // Down
        std::thread::sleep(Duration::from_millis(50));
    }
    for _ in 0..6 {
        session.send(b"\x1b[A").unwrap(); // Up
        std::thread::sleep(Duration::from_millis(50));
    }

    // Select first option and complete the flow
    session.send(b"\r").unwrap();
    session.expect("How should we measure").unwrap();
    session.send(b"\r").unwrap();
    session.expect("Approve").unwrap();
    session.send_line("y").unwrap();

    let output = session.wait().unwrap();
    assert!(
        output.success(),
        "init should succeed in narrow terminal.\noutput:\n{}",
        output.stdout()
    );
}
