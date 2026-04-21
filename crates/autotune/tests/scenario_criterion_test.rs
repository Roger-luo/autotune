//! Integration tests: criterion adaptor end-to-end through the autotune run loop.
//!
//! Verifies that when a `.autotune.toml` uses `adaptor = { type = "criterion", ... }`,
//! the pipeline correctly reads `target/criterion/<group>/new/estimates.json` and
//! records the extracted metrics in the ledger — instead of requiring a Python script.
//!
//! Requires: `cargo nextest run --features mock -E 'test(scenario_criterion_)'`

#![cfg(feature = "mock")]

use assert_cmd::Command;
use scenario::Project;
use std::path::Path;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Autotune config using the built-in criterion adaptor.
/// The measure command writes a fixed estimates.json so the adaptor has
/// something to read regardless of whether cargo bench was actually run.
const CRITERION_CONFIG_TOML: &str = r#"
[task]
name = "criterion-task"
description = "criterion adaptor pipeline test"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "gate-bench"
command = ["sh", "-c", "mkdir -p target/criterion/gate_bench/new && printf '{\"mean\":{\"point_estimate\":100.0},\"median\":{\"point_estimate\":98.0},\"std_dev\":{\"point_estimate\":5.0}}' > target/criterion/gate_bench/new/estimates.json"]
timeout = 30
adaptor = { type = "criterion", benchmarks = [{ name = "gate_mean_ns", group = "gate_bench", stat = "mean" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "gate_mean_ns", direction = "Minimize", weight = 1.0 }]
guardrail_metrics = []
"#;

fn criterion_project() -> Project {
    let project = Project::empty()
        .file(".autotune.toml", CRITERION_CONFIG_TOML)
        .file("src/lib.rs", "pub fn hello() -> u64 { 42 }\n")
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

fn write_script(project: &Project, entries: &[&str]) -> std::path::PathBuf {
    let path = project.path().join(".mock-script");
    std::fs::write(&path, entries.join("\n---\n")).unwrap();
    path
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Full pipeline run with criterion adaptor: measure command writes estimates.json,
/// adaptor reads it, and the ledger records the correct metric value.
#[test]
fn scenario_criterion_extracts_metrics_from_estimates_json() {
    let project = criterion_project();
    let script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>touch-src</approach>\
               <hypothesis>harmless edit to verify criterion pipeline completes</hypothesis>\
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
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "criterion pipeline should complete without error.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // The ledger must contain baseline record with the criterion metric.
    let ledger_path = project
        .path()
        .join(".autotune/tasks/criterion-task/ledger.json");
    assert!(ledger_path.exists(), "ledger should be written");
    let ledger_text = std::fs::read_to_string(&ledger_path).unwrap();

    // Baseline record must contain gate_mean_ns extracted from estimates.json.
    assert!(
        ledger_text.contains("gate_mean_ns"),
        "ledger should contain the criterion metric name.\nledger:\n{ledger_text}"
    );
    assert!(
        ledger_text.contains("100.0"),
        "ledger should contain the mean point_estimate (100.0) from estimates.json.\nledger:\n{ledger_text}"
    );
}

/// Criterion adaptor produces an error (not a Python-script workaround) when
/// estimates.json is missing, causing the measure phase to fail gracefully
/// rather than silently returning zero.
#[test]
fn scenario_criterion_fails_gracefully_when_estimates_missing() {
    // Use a no-op command so estimates.json is never written.
    const BAD_CONFIG: &str = r#"
[task]
name = "criterion-missing"
description = "criterion missing estimates test"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "gate-bench"
command = ["true"]
timeout = 10
adaptor = { type = "criterion", benchmarks = [{ name = "gate_mean_ns", group = "gate_bench", stat = "mean" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "gate_mean_ns", direction = "Minimize", weight = 1.0 }]
guardrail_metrics = []
"#;

    let project = Project::empty()
        .file(".autotune.toml", BAD_CONFIG)
        .file("src/lib.rs", "pub fn hello() -> u64 { 42 }\n")
        .build()
        .unwrap();
    git_init(project.path());

    let script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>noop</approach>\
               <hypothesis>criterion missing should error gracefully</hypothesis>\
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
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap();

    // Baseline measurement will fail because estimates.json doesn't exist.
    // autotune should exit with a non-zero status and report the criterion path.
    assert!(
        !output.status.success(),
        "should fail when criterion estimates.json is missing"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("criterion") || combined.contains("estimates"),
        "error output should mention criterion or estimates path.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
