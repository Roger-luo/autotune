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
use autotune::machine::{self, RunContext};
use autotune::resume;
use autotune_mock::{MOCK_RESEARCH_SESSION_ID, MockAgent};
use autotune_score::weighted_sum::{
    Direction as ScoreDirection, PrimaryMetricDef, WeightedSumScorer,
};
use autotune_state::{IterationRecord, IterationStatus, Phase, TaskState, TaskStore};
use chrono::Utc;
use scenario::{Project, Scenario, Terminal};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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

fn seed_resume_task(project: &Project, state: TaskState) {
    let task_dir = project.path().join(".autotune/tasks/scenario-task");
    let store = TaskStore::new(&task_dir).unwrap();
    store.save_config_snapshot(CONFIG_TOML).unwrap();
    store
        .append_ledger(&IterationRecord {
            iteration: 0,
            approach: "baseline".to_string(),
            status: IterationStatus::Baseline,
            hypothesis: None,
            metrics: HashMap::from([("metric_value".to_string(), 42.0)]),
            rank: 0.0,
            score: None,
            reason: None,
            fix_attempts: 0,
            fresh_spawns: 0,
            commit_sha: None,
            reverted_iteration: None,
            timestamp: Utc::now(),
        })
        .unwrap();
    store.save_state(&state).unwrap();

    std::process::Command::new("git")
        .args(["branch", "autotune/scenario-task-main"])
        .current_dir(project.path())
        .output()
        .expect("failed to seed advancing branch");
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

    // The exact implementer prompt must be persisted to prompt.md (CLAUDE.md
    // documents this artifact; it was previously never written).
    let prompt_md = project
        .path()
        .join(".autotune/tasks/scenario-task/iterations/001-touch-src/prompt.md");
    assert!(prompt_md.exists(), "prompt.md should be persisted");
    let prompt = std::fs::read_to_string(&prompt_md).unwrap();
    assert!(
        prompt.contains("# Approach: touch-src"),
        "prompt.md should contain the implementer prompt.\nprompt:\n{prompt}"
    );
}

/// Regression (end-to-end): a research-agent approach name with a `/`
/// (e.g. "rx/rzz") must not split the iteration directory into accidental
/// nested subdirectories — `metrics.json`/`prompt.md` must land in a single
/// slugified `NNN-<slug>` dir. Reproduces the real ppvm run where approaches
/// like "Speed up rehash in rx/rzz (map_insert)" produced a nested `rzz`
/// child dir holding the artifacts.
#[test]
fn scenario_run_slash_approach_does_not_nest_iteration_dir() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>speed up foo/bar in baz</approach>\
               <hypothesis>a harmless edit to verify slug sanitization</hypothesis>\
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

    let iterations = project
        .path()
        .join(".autotune/tasks/scenario-task/iterations");
    // The slugified single-component dir must exist and hold the artifacts.
    let slug_dir = iterations.join("001-speed-up-foo-bar-in-baz");
    assert!(
        slug_dir.is_dir(),
        "expected slugified iteration dir at {slug_dir:?}; iterations contained: {:?}",
        std::fs::read_dir(&iterations)
            .map(|rd| rd
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    // The buggy raw name would have created `001-speed up foo/` with a `bar`
    // child — assert no such partial-name parent leaked.
    assert!(
        !iterations.join("001-speed up foo").exists(),
        "approach '/' split the iteration dir into nested subdirectories"
    );
}

/// Regression (end-to-end): a bare project `[agent]` table must not let the
/// global *general* `[agent].model` shadow the global per-role
/// `[agent.research].model`. Reproduces a real bug where a global config with
/// `model = "sonnet"` plus `[agent.research] model = "opus"` spawned the
/// research agent as `sonnet` whenever the project's `[agent]` table was empty
/// (the shape left by `autotune init`). Drives the compiled binary with
/// `AUTOTUNE_GLOBAL_CONFIG` pointing at a fake global config.
#[test]
fn scenario_run_global_research_model_overrides_general_default() {
    // Project config with a *bare* [agent] table — the exact shape that
    // triggered the precedence inversion.
    let project = Project::empty()
        .file(".autotune.toml", format!("{CONFIG_TOML}\n[agent]\n"))
        .file("src/lib.rs", "pub fn hello() -> &'static str { \"hi\" }\n")
        .build()
        .unwrap();
    git_init(project.path());

    let script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>touch-src</approach>\
               <hypothesis>verify the research model resolves to opus</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    // Fake global config: general default `sonnet`, research override `opus`.
    let global_dir = tempfile::tempdir().unwrap();
    let global_config = global_dir.path().join("config.toml");
    std::fs::write(
        &global_config,
        "[agent]\nbackend = \"claude\"\nmodel = \"sonnet\"\n\n[agent.research]\nmodel = \"opus\"\n",
    )
    .unwrap();

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .env("AUTOTUNE_GLOBAL_CONFIG", &global_config)
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
    assert!(
        stdout.contains("spawning research agent: model=opus"),
        "research model should resolve to the global [agent.research] override \
         'opus', not the general default 'sonnet'.\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("spawning research agent: model=sonnet"),
        "research model must not fall back to the general default 'sonnet'.\n\
         stdout:\n{stdout}"
    );
}

#[test]
fn scenario_resume_repairs_testing_without_approach() {
    let project = scenario_project();
    seed_resume_task(
        &project,
        TaskState {
            task_name: "scenario-task".to_string(),
            canonical_branch: "main".to_string(),
            advancing_branch: "autotune/scenario-task-main".to_string(),
            research_session_id: MOCK_RESEARCH_SESSION_ID.to_string(),
            research_backend: "mock".to_string(),
            current_iteration: 1,
            current_phase: Phase::Testing,
            current_approach: None,
        },
    );

    let store = TaskStore::open(&project.path().join(".autotune/tasks/scenario-task")).unwrap();
    let repaired = resume::prepare_resume(&store, project.path()).unwrap();
    assert_eq!(repaired.current_phase, Phase::Planning);

    let config: autotune_config::AutotuneConfig = toml::from_str(CONFIG_TOML).unwrap();
    let agent = MockAgent::builder()
        .research_response(
            "<plan>\
               <approach>resume-repaired</approach>\
               <hypothesis>resume should recover from malformed persisted state</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        )
        .build();
    let scorer = WeightedSumScorer::new(
        vec![PrimaryMetricDef {
            name: "metric_value".to_string(),
            direction: ScoreDirection::Minimize,
            weight: 1.0,
        }],
        vec![],
    );
    let shutdown = AtomicBool::new(false);

    machine::run_task(
        &config,
        &agent,
        &scorer,
        project.path(),
        &store,
        &shutdown,
        &RunContext {
            approver: None,
            judge_ctx: None,
        },
    )
    .unwrap();

    let final_state = store.load_state().unwrap();
    assert_eq!(final_state.current_phase, Phase::Done);
    assert!(final_state.current_approach.is_none());

    let ledger = std::fs::read_to_string(
        project
            .path()
            .join(".autotune/tasks/scenario-task/ledger.json"),
    )
    .unwrap();
    assert!(
        ledger.contains("resume-repaired"),
        "resume should complete a repaired iteration.\nledger:\n{ledger}"
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

/// Regression: a *soft* tool request (e.g. `Bash`) under non-interactive
/// stdin must NOT crash the run trying to read an approval prompt. It should
/// auto-deny (the safe default) and continue. Before the fix, `dialoguer`
/// errored on the closed/redirected stdin and aborted the whole run — exactly
/// what blocked unattended `autotune run` once the research agent asked for
/// Bash.
#[test]
fn scenario_run_soft_tool_request_auto_denies_when_non_interactive() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            "<request-tool>\
               <tool>Bash</tool>\
               <scope>cargo tree:*</scope>\
               <reason>need dep graph for analysis</reason>\
             </request-tool>",
            "Ok, proceeding without Bash.",
            "<plan>\
               <approach>no-bash</approach>\
               <hypothesis>proceed with read-only tools</hypothesis>\
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

    assert!(
        !combined.contains("panicked"),
        "must not panic on non-interactive approval.\noutput:\n{combined}"
    );
    assert!(
        combined.contains("auto-denying tool request"),
        "should announce the non-interactive auto-deny.\noutput:\n{combined}"
    );
    assert!(
        output.status.success(),
        "run should complete through the auto-deny.\noutput:\n{combined}"
    );
}

/// With `AUTOTUNE_AUTO_APPROVE=1`, a soft tool request is granted without a
/// prompt even under non-interactive stdin — the unattended/CI path.
#[test]
fn scenario_run_auto_approve_env_grants_soft_tool() {
    let project = scenario_project();
    let script = write_script(
        &project,
        &[
            "<request-tool>\
               <tool>Bash</tool>\
               <scope>cargo tree:*</scope>\
               <reason>need dep graph for analysis</reason>\
             </request-tool>",
            "Thanks, got the dep graph.",
            "<plan>\
               <approach>with-bash</approach>\
               <hypothesis>used the approved tool</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .env("AUTOTUNE_AUTO_APPROVE", "1")
        .current_dir(project.path())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        combined.contains("auto-approving"),
        "should announce auto-approval.\noutput:\n{combined}"
    );
    assert!(
        output.status.success(),
        "run should complete with the tool approved.\noutput:\n{combined}"
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

// ---------------------------------------------------------------------------
// Live-tail flood regression
// ---------------------------------------------------------------------------

/// Config identical to CONFIG_TOML except the measure command floods stdout
/// with 80 wide lines (>40 cols) before emitting the metric. Used to verify
/// the live tail erase math stays correct under wrapping-width pressure.
const NOISY_CONFIG_TOML: &str = r#"
[task]
name = "noisy-task"
description = "noisy bench scenario"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "noisy-bench"
command = ["sh", "-c", "i=0; while [ $i -lt 80 ]; do printf 'NOISELINE_%03d_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' \"$i\"; i=$((i+1)); done; echo 'metric_value: 42.0'"]
timeout = 10
adaptor = { type = "regex", patterns = [{ name = "metric_value", pattern = "metric_value: ([0-9.]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "metric_value", direction = "Minimize", weight = 1.0 }]
guardrail_metrics = []
"#;

fn noisy_project() -> Project {
    let project = Project::empty()
        .file(".autotune.toml", NOISY_CONFIG_TOML)
        .file("src/lib.rs", "pub fn hello() -> &'static str { \"hi\" }\n")
        .build()
        .unwrap();
    git_init(project.path());
    project
}

/// End-to-end guard: when a measure command floods stdout with lines wider
/// than the terminal, the dimmed live tail must NOT leave un-erased rows on
/// screen. Rendered in a narrow PTY (forces wrapping if the erase math were
/// wrong) that is tall enough not to scroll, the final screen must contain no
/// leftover `NOISELINE` rows — the tail is erased on completion.
#[test]
fn scenario_run_live_tail_does_not_flood_narrow_terminal() {
    use scenario::ScreenBuffer;

    let project = noisy_project();
    let script = write_script(
        &project,
        &[
            // 1. Initial spawn: just prose.
            "Ready to plan.",
            // 2. First send (planning turn): a complete <plan>.
            "<plan>\
               <approach>noop</approach>\
               <hypothesis>measure only</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    let session = Scenario::new(autotune_bin())
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env(
            "AUTOTUNE_MOCK_RESEARCH_SCRIPT",
            script.to_string_lossy().as_ref(),
        )
        .current_dir(project.path())
        .terminal(Terminal::pty(40, 100)) // narrow (forces wrap if buggy) + tall (no scroll)
        .timeout(Duration::from_secs(60))
        .spawn()
        .unwrap();

    let output = session.wait().unwrap();
    let text = output.stdout();
    assert!(
        !text.contains("panicked"),
        "must not panic.\noutput:\n{text}"
    );

    // Render the full captured PTY stream the way a real 40×100 terminal
    // would, applying every cursor-up / erase the live tail emitted.
    // ScreenBuffer::new takes (rows, cols).
    let mut screen = ScreenBuffer::new(100, 40);
    screen.process(output.stdout_raw());
    let lines = screen.lines();

    // The fix erases the tail on completion; on a non-scrolling screen no
    // NOISELINE row may survive. (Allow <=1 for the documented bottom-of-screen
    // scroll edge.)
    let noise_rows = lines.iter().filter(|l| l.contains("NOISELINE")).count();
    assert!(
        noise_rows <= 1,
        "live tail left {noise_rows} un-erased NOISELINE rows on a 40-col terminal:\n{}",
        lines.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Judge adaptor scenario
// ---------------------------------------------------------------------------

/// A judge adaptor measure produces rubric metrics that appear in the ledger.
#[test]
fn scenario_run_judge_adaptor_produces_rubric_metrics_in_ledger() {
    const JUDGE_CONFIG: &str = r#"
[task]
name = "judge-task"
description = "judge adaptor scenario"
canonical_branch = "main"
max_iterations = "1"

[agent]
backend = "claude"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A strict reviewer"
[[measure.adaptor.rubrics]]
id = "quality"
title = "Quality"
instruction = "Score quality 1-5."
score_range = { min = 1, max = 5 }
[[measure.adaptor.rubrics]]
id = "correctness"
title = "Correctness"
instruction = "Score correctness 1-5."
score_range = { min = 1, max = 5 }

[score]
type = "weighted_sum"
primary_metrics = [
  { name = "quality",     direction = "Maximize", weight = 1.0 },
  { name = "correctness", direction = "Maximize", weight = 1.0 },
]
"#;

    let project = Project::empty()
        .file(".autotune.toml", JUDGE_CONFIG)
        .file("src/lib.rs", "pub fn hello() -> &'static str { \"hi\" }\n")
        .build()
        .unwrap();
    git_init(project.path());

    // Research script: one plan driving a single iteration.
    let research_script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>judge-test-approach</approach>\
               <hypothesis>test hypothesis for judge measure</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    // Judge script: batch response with quality and correctness scores.
    let judge_script_path = project.path().join(".mock-judge-script");
    std::fs::write(
        &judge_script_path,
        "quality\nscore: 4\nreason: Good quality overall.\n\ncorrectness\nscore: 5\nreason: Fully correct.",
    )
    .unwrap();

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &research_script)
        .env("AUTOTUNE_MOCK_JUDGE_SCRIPT", &judge_script_path)
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

    let ledger_path = project
        .path()
        .join(".autotune/tasks/judge-task/ledger.json");
    assert!(ledger_path.exists(), "ledger should be written");
    let ledger = std::fs::read_to_string(&ledger_path).unwrap();

    assert!(
        ledger.contains("\"quality\""),
        "ledger should contain quality metric.\nledger:\n{ledger}"
    );
    assert!(
        ledger.contains("\"correctness\""),
        "ledger should contain correctness metric.\nledger:\n{ledger}"
    );
    assert!(
        ledger.contains("4.0") || ledger.contains('4'),
        "ledger should record quality score 4.\nledger:\n{ledger}"
    );
    assert!(
        ledger.contains("5.0") || ledger.contains('5'),
        "ledger should record correctness score 5.\nledger:\n{ledger}"
    );
}

// ---------------------------------------------------------------------------
// Commit-harness preflight
// ---------------------------------------------------------------------------

/// `autotune run` aborts at the commit-harness preflight when the canonical
/// branch can't pass its own pre-commit hooks — before spending any iteration
/// (so the loop never burns research/implementation on commits the harness
/// would reject for reasons the implementer can't fix).
#[cfg(unix)]
#[test]
fn scenario_run_aborts_when_canonical_fails_precommit_hooks() {
    use std::os::unix::fs::PermissionsExt;

    let project = Project::empty()
        .file(".autotune.toml", CONFIG_TOML)
        .file("src/lib.rs", "pub fn hello() -> &'static str { \"hi\" }\n")
        // A pre-commit config is what makes autotune look for a runner.
        .file("prek.toml", "# stand-in prek config\n")
        .build()
        .unwrap();
    git_init(project.path());

    // A fake `prek` on PATH that always fails — stands in for a canonical tree
    // that can't pass its own hooks (e.g. pre-existing fmt/clippy drift).
    let bin_dir = project.path().join(".fakebin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_prek = bin_dir.join("prek");
    std::fs::write(
        &fake_prek,
        "#!/bin/sh\necho 'cargo clippy.....Failed' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_prek, std::fs::Permissions::from_mode(0o755)).unwrap();

    // The plan is never reached (the preflight aborts first), but provide one
    // so agent construction is happy.
    let script = write_script(
        &project,
        &[
            "<plan><approach>x</approach><hypothesis>h</hypothesis><files-to-modify><file>src/lib.rs</file></files-to-modify></plan>",
        ],
    );

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .current_dir(project.path())
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "run should abort at the commit-harness preflight"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("does not pass its own pre-commit hooks"),
        "expected the preflight abort message, got:\n{combined}"
    );
    // It must abort before measuring the baseline.
    assert!(
        !combined.contains("collecting baseline metrics"),
        "preflight must abort before baseline, got:\n{combined}"
    );
}

/// The preflight is a no-op when the runner reports success: `autotune run`
/// proceeds past it (here it goes on to fail later for an unrelated reason,
/// proving only that the preflight itself didn't block a green harness).
#[cfg(unix)]
#[test]
fn scenario_run_proceeds_when_precommit_hooks_pass() {
    use std::os::unix::fs::PermissionsExt;

    let project = Project::empty()
        .file(".autotune.toml", CONFIG_TOML)
        .file("src/lib.rs", "pub fn hello() -> &'static str { \"hi\" }\n")
        .file("prek.toml", "# stand-in prek config\n")
        .build()
        .unwrap();
    git_init(project.path());

    let bin_dir = project.path().join(".fakebin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_prek = bin_dir.join("prek");
    // A green runner: ignores args, exits 0.
    std::fs::write(&fake_prek, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&fake_prek, std::fs::Permissions::from_mode(0o755)).unwrap();

    let script = write_script(
        &project,
        &[
            "<plan><approach>x</approach><hypothesis>h</hypothesis><files-to-modify><file>src/lib.rs</file></files-to-modify></plan>",
        ],
    );

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .current_dir(project.path())
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .env("PATH", path)
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The preflight passed (green runner), so the run advanced to the baseline
    // rather than aborting with the harness error.
    assert!(
        !combined.contains("does not pass its own pre-commit hooks"),
        "a green harness must not trip the preflight, got:\n{combined}"
    );
    assert!(
        combined.contains("collecting baseline metrics"),
        "run should proceed past the preflight to baseline, got:\n{combined}"
    );
}

/// The preflight must skip the `no-commit-to-branch` branch-guard hook: it runs
/// on the canonical branch (which that hook protects), but candidate commits
/// land on worktree branches and never trip it. The fake runner here *fails*
/// unless `SKIP=no-commit-to-branch` is set, so the run only proceeds if the
/// preflight passed that env through.
#[cfg(unix)]
#[test]
fn scenario_run_preflight_skips_branch_guard_hook() {
    use std::os::unix::fs::PermissionsExt;

    let project = Project::empty()
        .file(".autotune.toml", CONFIG_TOML)
        .file("src/lib.rs", "pub fn hello() -> &'static str { \"hi\" }\n")
        .file("prek.toml", "# stand-in prek config\n")
        .build()
        .unwrap();
    git_init(project.path());

    let bin_dir = project.path().join(".fakebin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_prek = bin_dir.join("prek");
    // Passes only when the branch-guard hook is in $SKIP (as the preflight sets).
    std::fs::write(
        &fake_prek,
        "#!/bin/sh\ncase \"$SKIP\" in\n  *no-commit-to-branch*) exit 0 ;;\n  *) echo 'no-commit-to-branch.....Failed' >&2; exit 1 ;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_prek, std::fs::Permissions::from_mode(0o755)).unwrap();

    let script = write_script(
        &project,
        &[
            "<plan><approach>x</approach><hypothesis>h</hypothesis><files-to-modify><file>src/lib.rs</file></files-to-modify></plan>",
        ],
    );
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .current_dir(project.path())
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .env("PATH", path)
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("does not pass its own pre-commit hooks"),
        "preflight must skip no-commit-to-branch and pass, got:\n{combined}"
    );
    assert!(
        combined.contains("collecting baseline metrics"),
        "run should proceed past the preflight to baseline, got:\n{combined}"
    );
}
