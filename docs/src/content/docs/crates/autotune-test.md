---
title: autotune-test
description: Runs configured test commands during the Testing phase; any failure discards the candidate before measurement.
section: Crates
order: 14
---

`autotune-test` executes the test commands configured for a task during the state machine's **Testing** phase. Each command runs as a subprocess with captured output and a hard timeout; a non-zero exit (or a timeout) marks the test as not passed, which the CLI treats as a signal to discard the candidate iteration before it ever reaches measurement.

## When to use it

- You need to run a project's test suite (or any pass/fail command) against a candidate change and get a structured result back.
- You want per-test stdout/stderr capture plus a wall-clock timeout, without a test failure crashing the caller (failures come back as `Ok(TestResult { passed: false, .. })`, not errors).
- You want fail-fast semantics across a list of tests: stop at the first failure rather than running the rest.

## Public API

- `run_test(config: &TestConfig, working_dir: &Path) -> Result<TestResult, TestError>` — Spawns one configured command in `working_dir`, streaming stdout/stderr on background threads and enforcing `config.timeout` seconds. Returns a populated `TestResult` for both pass and fail; only I/O problems and timeouts surface as `Err`.
- `run_all_tests(configs: &[TestConfig], working_dir: &Path) -> Result<Vec<TestResult>, TestError>` — Runs configs in order, short-circuiting after the first non-passing result. An empty slice yields an empty `Vec`.
- `all_passed(results: &[TestResult]) -> bool` — True if every result passed; true for an empty slice.
- `TestResult` — `{ name: String, passed: bool, duration_secs: f64, stdout: String, stderr: String }`.
- `TestError` — Error enum with variants `Failed { name, code, stdout, stderr }`, `Timeout { name, timeout }`, and `Io { name, source }`.

## Usage

```rust
use autotune_config::TestConfig;
use autotune_test::{all_passed, run_all_tests};
use std::path::Path;

let configs = vec![TestConfig {
    name: "unit".to_string(),
    command: vec!["cargo".to_string(), "test".to_string()],
    timeout: 300,
    allow_test_edits: false,
}];

let results = run_all_tests(&configs, Path::new("."))?;
if all_passed(&results) {
    println!("all {} test command(s) passed", results.len());
} else {
    // First failing result is the last entry; later configs were skipped.
    let failed = results.last().unwrap();
    eprintln!("test '{}' failed in {:.2}s", failed.name, failed.duration_secs);
}
# Ok::<(), autotune_test::TestError>(())
```

## Internal dependencies

- `autotune-config` — for the `TestConfig` input type (`name`, `command`, `timeout`, `allow_test_edits`).

## Notes

- A failing command does **not** produce a `TestError::Failed`; `run_test` returns `Ok` with `passed: false`. Callers decide pass/fail by inspecting `TestResult::passed` (or `all_passed`). The `Failed` variant exists on the error type but is not returned by `run_test`.
- The timeout is enforced by polling `try_wait` on a 10ms loop; on expiry the child is killed and the reader threads are dropped, so `run_test` returns promptly even if a descendant process keeps the output pipes open.
- Output readers run on dedicated threads, so verbose commands that produce large output won't deadlock the pipe or be misread as a hang.
