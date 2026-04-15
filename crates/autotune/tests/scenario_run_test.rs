//! Scenario tests for the `autotune run` loop against a mock research agent.
//!
//! Requires: `cargo nextest run --features mock -E 'test(scenario_run_)'`
//!
//! Each test writes a response script to a temp file, points the mock agent
//! at it via `AUTOTUNE_MOCK_RESEARCH_SCRIPT`, and asserts the CLI reacts
//! correctly to the injected XML (or malformed input).
//!
//! Script format: response texts for the research agent's spawn + send
//! calls, concatenated in order and separated by a line containing only
//! `---`. The first entry is returned by `spawn()`; subsequent entries by
//! successive `send()` calls.

#![cfg(feature = "mock")]

use assert_cmd::Command;
use scenario::{Project, Scenario, Terminal};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Minimal autotune config: one always-passing test, one `echo`-based
/// measure producing a scalar metric, weighted-sum scoring.
const CONFIG_TOML: &str = r#"
[task]
name = "scenario-task"
description = "scenario test task"
canonical_branch = "main"
max_iterations = "1"

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

fn scenario_project() -> Project {
    let project = Project::empty()
        .file(".autotune.toml", CONFIG_TOML)
        .file("src/lib.rs", "pub fn hello() -> &'static str { \"hi\" }\n")
        .build()
        .unwrap();
    git_init(project.path());
    project
}

fn git_init(dir: &Path) {
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@test.com"],
        vec!["config", "user.name", "Test"],
        vec!["add", "."],
        vec!["commit", "-m", "initial"],
        vec!["branch", "-M", "main"],
    ] {
        std::process::Command::new("git")
            .args(&args)
            .current_dir(dir)
            .output()
            .expect("git setup step failed");
    }
}

fn write_script(project: &Project, entries: &[&str]) -> PathBuf {
    let path = project.path().join(".mock-script");
    std::fs::write(&path, entries.join("\n---\n")).unwrap();
    path
}

fn autotune_bin() -> String {
    env!("CARGO_BIN_EXE_autotune").to_string()
}

// ---------------------------------------------------------------------------
// XML response type coverage
// ---------------------------------------------------------------------------

/// A plain `<plan>` on the first planning send drives the loop through one
/// full iteration and exits cleanly (max_iterations = 1).
#[test]
fn scenario_run_plain_plan_completes_iteration() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            // 1. Initial spawn: just prose — no tool requests, no plan.
            "Ready to plan.",
            // 2. First send (planning turn): a complete <plan>.
            "<plan>\
               <approach>touch-src</approach>\
               <hypothesis>a harmless edit to verify the loop drives to completion</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected clean exit.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Ledger should have baseline + 1 iteration.
    let ledger = project
        .path()
        .join(".autotune/tasks/scenario-task/ledger.json");
    assert!(ledger.exists(), "ledger should be written");
    let text = std::fs::read_to_string(&ledger).unwrap();
    assert!(
        text.contains("touch-src"),
        "ledger should record the planned approach.\nledger:\n{text}"
    );
}

/// Malformed XML on the planning turn should surface as a parse error
/// without panicking, and the CLI should exit with a non-zero status.
#[test]
fn scenario_run_malformed_plan_surfaces_error() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            "Ready.",
            // Missing closing tag — quick_xml should fail to parse.
            "<plan><approach>oops</approach><hypothesis>broken",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !combined.contains("panicked"),
        "must not panic on malformed XML.\noutput:\n{combined}"
    );
    assert!(
        !output.status.success(),
        "CLI should fail on malformed plan.\noutput:\n{combined}"
    );
}

/// A `<plan>`-free planning response (just prose) should also fail the
/// planning step — nothing for the parser to extract.
#[test]
fn scenario_run_prose_only_plan_fails() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &["Ready.", "I don't have a suggestion right now, sorry!"],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains("panicked"), "must not panic");
    assert!(
        !output.status.success(),
        "CLI should fail when no <plan> is produced.\noutput:\n{combined}"
    );
}

/// `<plan>` embedded in surrounding prose should still parse successfully.
#[test]
fn scenario_run_plan_with_surrounding_prose_parses() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            "Ready.",
            "Based on the analysis, here is my plan:\n\n\
             <plan>\
               <approach>prose-sandwich</approach>\
               <hypothesis>plan is embedded in prose but still valid</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>\n\n\
             Hope this helps.",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "expected success.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let ledger = std::fs::read_to_string(
        project
            .path()
            .join(".autotune/tasks/scenario-task/ledger.json"),
    )
    .unwrap();
    assert!(ledger.contains("prose-sandwich"));
}

// ---------------------------------------------------------------------------
// PTY-based: tool-request approval flow
// ---------------------------------------------------------------------------

/// A `<request-tool>` fragment emitted on the initial spawn should trigger
/// the interactive approval prompt. Denying keeps the run going with
/// whatever tools the agent already has; a follow-up `<plan>` then drives
/// the iteration.
#[test]
fn scenario_run_request_tool_prompts_user_for_approval() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            // 1. Initial spawn: a single tool request — must end the turn.
            "<request-tool>\
               <tool>Bash</tool>\
               <scope>cargo tree:*</scope>\
               <reason>need dep graph for analysis</reason>\
             </request-tool>",
            // 2. Follow-up reply to CLI's "DENIED" feedback: proceed without.
            "Ok, proceeding without Bash.",
            // 3. Planning send: emit a plan.
            "<plan>\
               <approach>no-bash</approach>\
               <hypothesis>proceed with read-only tools</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    let mut session = Scenario::new(autotune_bin())
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env(
            "AUTOTUNE_MOCK_RESEARCH_SCRIPT",
            script.to_string_lossy().as_ref(),
        )
        .current_dir(project.path())
        .terminal(Terminal::pty(120, 40))
        .timeout(Duration::from_secs(30))
        .spawn()
        .unwrap();

    // The CLI should prompt for approval of the Bash tool.
    session.expect("research agent requests a tool").unwrap();
    session.expect("Bash").unwrap();
    session.expect("need dep graph").unwrap();

    // Deny (press Enter — default is "no").
    session.send_line("").unwrap();

    let output = session.wait().unwrap();
    let text = output.stdout();
    assert!(
        !text.contains("panicked"),
        "must not panic.\noutput:\n{text}"
    );
}

/// A hard-denied tool (`Edit` / `Write` / `Agent`) should be auto-denied
/// by the CLI without prompting the user at all.
#[test]
fn scenario_run_hard_denied_tool_is_auto_rejected() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            // 1. Initial spawn: requests Edit, which is hardcoded-denied for
            //    the research role. The CLI must NOT prompt the user.
            "<request-tool>\
               <tool>Edit</tool>\
               <reason>want to modify files directly</reason>\
             </request-tool>",
            // 2. Agent's next turn after CLI's auto-deny feedback.
            "Understood, staying read-only.",
            // 3. Plan.
            "<plan>\
               <approach>no-edit</approach>\
               <hypothesis>respect the research-role denylist</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // No interactive prompt should have blocked the piped-stdin invocation.
    assert!(
        !combined.contains("research agent requests a tool"),
        "hard-denied tools must not trigger an interactive prompt.\noutput:\n{combined}"
    );
    assert!(
        output.status.success(),
        "run should complete through auto-deny.\noutput:\n{combined}"
    );
}

/// Running `autotune run` when a task of the same name already exists
/// auto-forks to `<name>-2` instead of bailing.
#[test]
fn scenario_run_auto_forks_on_existing_task() {
    let project = scenario_project();

    // Build a research script that produces a valid plan on each invocation.
    // Since each `autotune run` is a fresh process, both runs read the same
    // script and will replay it from the start.
    let script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>first-pass</approach>\
               <hypothesis>initial edit to verify fork behavior</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    // First run: creates task `scenario-task`.
    let out1 = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();
    assert!(
        out1.status.success(),
        "first run should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out1.stdout),
        String::from_utf8_lossy(&out1.stderr)
    );

    // Second run: task `scenario-task` already exists, should fork to `-2`.
    let out2 = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();

    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let combined2 = format!("{stdout2}{stderr2}");

    assert!(
        out2.status.success(),
        "second run should succeed via auto-fork.\noutput:\n{combined2}"
    );
    assert!(
        combined2.contains("forking as 'scenario-task-2'"),
        "second run should announce the fork.\noutput:\n{combined2}"
    );

    // Both task directories should exist.
    assert!(
        project
            .path()
            .join(".autotune/tasks/scenario-task")
            .exists(),
        "original task dir should persist"
    );
    assert!(
        project
            .path()
            .join(".autotune/tasks/scenario-task-2")
            .exists(),
        "forked task dir should exist"
    );
}

// ---------------------------------------------------------------------------
// Fix-retry loop
// ---------------------------------------------------------------------------

/// Config for the fix-retry scenario: a single test that checks the marker
/// token `"fixed"` appears in `src/lib.rs`. The mock implementer writes a
/// broken version on turn 0 (missing marker → test fails), then a correct
/// version on turn 1 (marker present → test passes). The CLI must surface
/// the test failure to the implementer via the Fixing phase rather than
/// discarding the iteration immediately.
const FIX_RETRY_CONFIG_TOML: &str = r#"
[task]
name = "fix-retry-task"
description = "fix-retry scenario"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[agent]

[agent.implementation]
max_fix_attempts = 10
max_fresh_spawns = 1

[[test]]
name = "marker-present"
command = ["sh", "-c", "grep -q 'fixed' src/lib.rs"]
timeout = 10

[[measure]]
name = "echo-bench"
command = ["sh", "-c", "echo 'metric_value: 1.0'"]
timeout = 10
adaptor = { type = "regex", patterns = [{ name = "metric_value", pattern = "metric_value: ([0-9.]+)" }] }

[score]
type = "threshold"
conditions = [{ metric = "metric_value", direction = "Minimize", threshold = -1000.0 }]
"#;

fn fix_retry_project() -> Project {
    // Baseline contains the marker so sanity tests pass; the mock
    // implementer's first turn removes it to simulate a broken edit.
    let project = Project::empty()
        .file(".autotune.toml", FIX_RETRY_CONFIG_TOML)
        .file(
            "src/lib.rs",
            "pub fn hello() -> &'static str { \"fixed\" }\n",
        )
        .build()
        .unwrap();
    git_init(project.path());
    project
}

fn write_impl_script(project: &Project, entries: &[&str]) -> PathBuf {
    let path = project.path().join(".mock-impl-script");
    std::fs::write(&path, entries.join("\n---\n")).unwrap();
    path
}

/// Implementer first writes code lacking the expected marker (tests fail),
/// then, given the failure context via a session-continuation turn, writes
/// code containing the marker (tests pass). Iteration must end Kept with
/// `fix_attempts == 1` recorded on the ledger.
#[test]
fn scenario_run_fix_retry_recovers_in_same_session() {
    let project = fix_retry_project();

    let research_script = write_script(
        &project,
        &[
            "Ready.",
            "<plan>\
               <approach>add-marker</approach>\
               <hypothesis>add the required marker token to src/lib.rs</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    // Turn 0: write code missing the "fixed" marker — grep test fails.
    // Turn 1: rewrite with the marker — grep test passes.
    let impl_script = write_impl_script(
        &project,
        &[
            "cat > src/lib.rs <<'EOF'\n\
             pub fn hello() -> &'static str { \"broken\" }\n\
             EOF",
            "cat > src/lib.rs <<'EOF'\n\
             pub fn hello() -> &'static str { \"fixed\" }\n\
             EOF",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &research_script)
        .env("AUTOTUNE_MOCK_IMPL_SCRIPT", &impl_script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success(),
        "expected clean exit after fix-retry recovery.\noutput:\n{combined}"
    );

    let ledger_path = project
        .path()
        .join(".autotune/tasks/fix-retry-task/ledger.json");
    let ledger = std::fs::read_to_string(&ledger_path).unwrap();
    assert!(
        ledger.contains("add-marker") && ledger.contains("\"kept\""),
        "iteration must end Kept after fix-retry recovery.\nledger:\n{ledger}"
    );
    assert!(
        ledger.contains("\"fix_attempts\": 1"),
        "ledger must record fix_attempts=1.\nledger:\n{ledger}"
    );
}

/// When the implementer session stops producing edits (empty turn), the CLI
/// must respawn a fresh implementer session (tier-2) and retry. The fresh
/// spawn writes the marker; iteration ends Kept.
#[test]
fn scenario_run_fix_retry_respawns_on_unproductive_session() {
    let project = fix_retry_project();

    let research_script = write_script(
        &project,
        &[
            "Ready.",
            "<plan>\
               <approach>add-marker-respawn</approach>\
               <hypothesis>add the marker via fresh-spawn fallback</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    // Turn 0: initial spawn writes broken code (tests fail).
    // Turn 1: fix turn in same session — empty script, no edits → triggers respawn.
    // Turn 2: fresh spawn writes fixed code.
    let impl_script = write_impl_script(
        &project,
        &[
            "cat > src/lib.rs <<'EOF'\n\
             pub fn hello() -> &'static str { \"broken\" }\n\
             EOF",
            "",
            "cat > src/lib.rs <<'EOF'\n\
             pub fn hello() -> &'static str { \"fixed\" }\n\
             EOF",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &research_script)
        .env("AUTOTUNE_MOCK_IMPL_SCRIPT", &impl_script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "expected clean exit after respawn recovery.\noutput:\n{combined}"
    );

    let ledger = std::fs::read_to_string(
        project
            .path()
            .join(".autotune/tasks/fix-retry-task/ledger.json"),
    )
    .unwrap();
    assert!(
        ledger.contains("add-marker-respawn") && ledger.contains("\"kept\""),
        "iteration must end Kept after respawn.\nledger:\n{ledger}"
    );
    assert!(
        ledger.contains("\"fresh_spawns\": 1"),
        "ledger must record fresh_spawns=1.\nledger:\n{ledger}"
    );
}

/// When `max_fix_attempts` is exhausted and tests still fail, the iteration
/// is discarded with a reason identifying the exhausted budget.
#[test]
fn scenario_run_fix_retry_discards_when_budget_exhausted() {
    let project = Project::empty()
        .file(
            ".autotune.toml",
            FIX_RETRY_CONFIG_TOML
                .replace("max_fix_attempts = 10", "max_fix_attempts = 1")
                .replace("max_fresh_spawns = 1", "max_fresh_spawns = 0"),
        )
        .file(
            "src/lib.rs",
            "pub fn hello() -> &'static str { \"fixed\" }\n",
        )
        .build()
        .unwrap();
    git_init(project.path());

    let research_script = write_script(
        &project,
        &[
            "Ready.",
            "<plan>\
               <approach>stubborn</approach>\
               <hypothesis>implementer cannot fix this</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    // Every turn writes broken code — tests keep failing, budget exhausts.
    let impl_script = write_impl_script(
        &project,
        &[
            "cat > src/lib.rs <<'EOF'\n\
             pub fn hello() -> &'static str { \"broken-0\" }\n\
             EOF",
            "cat > src/lib.rs <<'EOF'\n\
             pub fn hello() -> &'static str { \"broken-1\" }\n\
             EOF",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &research_script)
        .env("AUTOTUNE_MOCK_IMPL_SCRIPT", &impl_script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // `max_iterations = 1` means the loop stops after the first (discarded)
    // iteration — CLI should exit cleanly even though the approach was
    // discarded.
    assert!(
        output.status.success(),
        "expected clean exit after budget exhaustion.\noutput:\n{combined}"
    );

    let ledger = std::fs::read_to_string(
        project
            .path()
            .join(".autotune/tasks/fix-retry-task/ledger.json"),
    )
    .unwrap();
    assert!(
        ledger.contains("stubborn") && ledger.contains("\"discarded\""),
        "iteration must end Discarded after budget exhaustion.\nledger:\n{ledger}"
    );
    assert!(
        ledger.contains("fix attempt"),
        "discard reason should mention fix attempt(s).\nledger:\n{ledger}"
    );
}
