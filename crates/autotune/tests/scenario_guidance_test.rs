//! Scenario tests: the research-agent prompt specializes its hypothesis
//! guidance by DERIVING from the declared measures, with no task-kind flag.
//!
//! A coverage-style task (regex over a measure command) must NOT offer the
//! profiler or "hot path" framing; a criterion-benchmark task must. The
//! perf-vs-generic decision is surfaced via the `research.prompt` trace event
//! (`AUTOTUNE_TRACE_FILE`), so these tests drive the compiled binary and read
//! the trace rather than peering inside the in-process agent.
//!
//! Requires: `cargo nextest run --features mock -E 'test(scenario_guidance_)'`

#![cfg(feature = "mock")]

use assert_cmd::Command;
use scenario::Project;
use std::path::Path;
use std::time::Duration;

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

/// Run `autotune run` against `config_toml` with a tracing file, then return
/// the single `research.prompt` trace payload.
fn run_and_capture_research_prompt_trace(config_toml: &str) -> serde_json::Value {
    let project = Project::empty()
        .file(".autotune.toml", config_toml)
        .file("src/lib.rs", "pub fn hello() -> u64 { 42 }\n")
        .build()
        .unwrap();
    git_init(project.path());

    let script = write_script(
        &project,
        &[
            "Ready to plan.",
            "<plan>\
               <approach>touch-src</approach>\
               <hypothesis>harmless edit to verify the run completes</hypothesis>\
               <files-to-modify><file>src/lib.rs</file></files-to-modify>\
             </plan>",
        ],
    );

    let trace_path = project.path().join(".autotune-trace.jsonl");

    let output = Command::cargo_bin("autotune")
        .unwrap()
        .arg("run")
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &script)
        .env("AUTOTUNE_TRACE_FILE", &trace_path)
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

    let trace = std::fs::read_to_string(&trace_path).expect("trace file written");
    trace
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["category"] == serde_json::json!("research.prompt"))
        .map(|v| v["payload"].clone())
        .expect("research.prompt trace event present")
}

const COVERAGE_CONFIG_TOML: &str = r#"
[task]
name = "coverage-task"
description = "raise line coverage"
canonical_branch = "main"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[test]]
name = "always-pass"
command = ["true"]
timeout = 10

[[measure]]
name = "coverage"
command = ["sh", "-c", "echo 'coverage: 80.0'"]
timeout = 30
adaptor = { type = "regex", patterns = [{ name = "line_coverage", pattern = "coverage: ([0-9.]+)" }] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "line_coverage", direction = "Maximize", weight = 1.0 }]
guardrail_metrics = []
"#;

const CRITERION_CONFIG_TOML: &str = r#"
[task]
name = "perf-task"
description = "speed up the gate bench"
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

/// A coverage task (regex measure, no criterion bench) gets the GENERIC,
/// metric-agnostic guidance: the research prompt must NOT offer a profiler or
/// mention a hot path. This is the core de-bias fix — the prompt specializes by
/// the declared measures, not a task-kind flag.
#[test]
fn scenario_guidance_coverage_task_omits_profiler() {
    let payload = run_and_capture_research_prompt_trace(COVERAGE_CONFIG_TOML);
    assert_eq!(
        payload["optimizes_runtime_perf"],
        serde_json::json!(false),
        "coverage task must not be classified as runtime-perf.\npayload:\n{payload}"
    );
    assert_eq!(
        payload["offers_profiler"],
        serde_json::json!(false),
        "coverage research prompt must NOT offer a profiler.\npayload:\n{payload}"
    );
    assert_eq!(
        payload["mentions_hot_path"],
        serde_json::json!(false),
        "coverage research prompt must NOT mention a hot path.\npayload:\n{payload}"
    );
}

/// A criterion-benchmark task IS a runtime-perf task: the research prompt keeps
/// the profiler offer and hot-path framing.
#[test]
fn scenario_guidance_criterion_task_offers_profiler() {
    let payload = run_and_capture_research_prompt_trace(CRITERION_CONFIG_TOML);
    assert_eq!(
        payload["optimizes_runtime_perf"],
        serde_json::json!(true),
        "criterion task must be classified as runtime-perf.\npayload:\n{payload}"
    );
    assert_eq!(
        payload["offers_profiler"],
        serde_json::json!(true),
        "criterion research prompt must offer a profiler.\npayload:\n{payload}"
    );
    assert_eq!(
        payload["mentions_hot_path"],
        serde_json::json!(true),
        "criterion research prompt must mention a hot path.\npayload:\n{payload}"
    );
}
