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

/// Reproduces the Clifford-gate noise episode end-to-end: a candidate that
/// genuinely improves the primary metric but shows a "regression" on a second
/// metric that is within the criterion confidence interval. The within-noise
/// metric is a guardrail; under naive scoring its apparent regression would
/// trip the guardrail and DISCARD the iteration. With noise-aware scoring the
/// within-CI swing is ignored and the iteration is KEPT.
///
/// The measure command writes the candidate estimates.json conditionally on a
/// marker the implementation agent adds to `src/lib.rs`, so baseline and
/// candidate measurements differ within one fixed command.
#[test]
fn scenario_within_noise_regression_is_not_counted() {
    const NOISE_CONFIG_TOML: &str = r#"
[task]
name = "noise-task"
description = "noise-aware scoring pipeline test"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

# The measure writes BOTH benchmarks' estimates.json. The `fast` (primary)
# benchmark genuinely improves when the marker is present (100 -> 80). The
# `noisy` (guardrail) benchmark "regresses" (100 -> 131) but with a CI of
# [60,140] on both runs, so the 31-unit swing is well within the noise
# envelope (half-width 40 + 40 = 80).
[[measure]]
name = "benches"
command = ["sh", "-c", """
mkdir -p target/criterion/fast/new target/criterion/noisy/new
if grep -q OPTIMIZED src/lib.rs; then
  fast=80.0
  noisy=131.0
else
  fast=100.0
  noisy=100.0
fi
printf '{"mean":{"confidence_interval":{"confidence_level":0.95,"lower_bound":%s,"upper_bound":%s},"point_estimate":%s},"median":{"point_estimate":%s},"std_dev":{"point_estimate":5.0}}' "$fast" "$fast" "$fast" "$fast" > target/criterion/fast/new/estimates.json
printf '{"mean":{"confidence_interval":{"confidence_level":0.95,"lower_bound":60.0,"upper_bound":140.0},"point_estimate":%s},"median":{"point_estimate":%s},"std_dev":{"point_estimate":40.0}}' "$noisy" "$noisy" > target/criterion/noisy/new/estimates.json
"""]
timeout = 30
adaptor = { type = "criterion", benchmarks = [{ name = "fast_ns", group = "fast", stat = "mean" }, { name = "noisy_ns", group = "noisy", stat = "mean" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "fast_ns", direction = "Minimize", weight = 1.0 }]
guardrail_metrics = [{ name = "noisy_ns", direction = "Minimize", max_regression = 0.05 }]
"#;

    let project = Project::empty()
        .file(".autotune.toml", NOISE_CONFIG_TOML)
        .file("src/lib.rs", "pub fn hello() -> u64 { 42 }\n")
        .build()
        .unwrap();
    git_init(project.path());

    // The implementation agent (a mock that runs the script entry with
    // `sh -c` in the worktree) adds the OPTIMIZED marker to src/lib.rs and
    // commits, so the conditional measure command produces the candidate
    // (improved-fast, noisy-regressed) estimates on the iteration run.
    let impl_script = project.path().join(".mock-impl-script");
    std::fs::write(
        &impl_script,
        "printf '// OPTIMIZED\\n' >> src/lib.rs && git add -A && git commit -q -m 'optimize fast path'",
    )
    .unwrap();

    let script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>optimize-fast</approach>\
               <hypothesis>speed up the fast path; the noisy bench is unrelated jitter</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .env("AUTOTUNE_MOCK_IMPL_SCRIPT", &impl_script)
        .current_dir(project.path())
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "run should complete.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let ledger_path = project
        .path()
        .join(".autotune/tasks/noise-task/ledger.json");
    let ledger_text = std::fs::read_to_string(&ledger_path).unwrap();
    let ledger: serde_json::Value = serde_json::from_str(&ledger_text).unwrap();
    let rows = ledger.as_array().expect("ledger is an array");

    // The iteration row (not the baseline) must be KEPT: the within-noise
    // guardrail regression must NOT have discarded it.
    let iter = rows
        .iter()
        .find(|r| r["iteration"] == serde_json::json!(1))
        .expect("iteration 1 row present");
    assert_eq!(
        iter["status"],
        serde_json::json!("kept"),
        "within-noise guardrail regression must not discard the iteration.\nledger:\n{ledger_text}"
    );

    // The breakdown must flag the noisy metric as within_noise and the fast
    // metric as significant.
    let metrics = iter["score_breakdown"]["metrics"]
        .as_array()
        .expect("breakdown metrics present");
    let noisy = metrics
        .iter()
        .find(|m| m["name"] == serde_json::json!("noisy_ns"))
        .expect("noisy_ns in breakdown");
    assert_eq!(
        noisy["within_noise"],
        serde_json::json!(true),
        "noisy_ns delta is within the CI envelope.\nbreakdown:\n{}",
        serde_json::to_string_pretty(&iter["score_breakdown"]).unwrap()
    );
    let fast = metrics
        .iter()
        .find(|m| m["name"] == serde_json::json!("fast_ns"))
        .expect("fast_ns in breakdown");
    assert_eq!(
        fast["within_noise"],
        serde_json::json!(false),
        "fast_ns is a real 20% improvement, not noise"
    );

    // The kept iteration must carry the captured per-metric variances and the
    // copied estimates.json survive in the iteration dir.
    assert!(
        iter["variances"]["noisy_ns"].is_object(),
        "variances persisted on the kept row.\nrow:\n{}",
        serde_json::to_string_pretty(iter).unwrap()
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
