# Iteration Metrics Status Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Print every measured metric for the current iteration immediately after the scoring summary line.

**Architecture:** Keep the scoring summary line in `run_scoring()` unchanged and add a second deterministic metrics line rendered from the existing `candidate_metrics` map. Implement a small formatting helper in `machine.rs` and pin the behavior with a unit test that checks sorted output.

**Tech Stack:** Rust, cargo test, existing `autotune` unit tests

---

### Task 1: Add a failing output-format test

**Files:**
- Modify: `crates/autotune/src/machine.rs`
- Test: `crates/autotune/src/machine.rs`

- [ ] **Step 1: Write the failing test**

Add a unit test near the existing `machine.rs` tests that asserts a deterministic metrics status string:

```rust
#[test]
fn format_metrics_status_sorts_all_metrics() {
    let metrics = std::collections::HashMap::from([
        ("throughput".to_string(), 88.0),
        ("latency_ms".to_string(), 12.3),
    ]);

    assert_eq!(
        format_metrics_status(&metrics),
        "latency_ms=12.3000, throughput=88.0000"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p autotune format_metrics_status_sorts_all_metrics -- --exact`
Expected: FAIL with an error that `format_metrics_status` does not exist yet.

- [ ] **Step 3: Write minimal implementation**

Add a helper in `crates/autotune/src/machine.rs`:

```rust
fn format_metrics_status(metrics: &std::collections::HashMap<String, f64>) -> String {
    let mut entries: Vec<_> = metrics.iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    entries
        .into_iter()
        .map(|(name, value)| format!("{name}={value:.4}"))
        .collect::<Vec<_>>()
        .join(", ")
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p autotune format_metrics_status_sorts_all_metrics -- --exact`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/autotune/src/machine.rs
git commit -m "test: add metrics status formatter coverage"
```

### Task 2: Print the metrics line after scoring

**Files:**
- Modify: `crates/autotune/src/machine.rs`
- Test: `crates/autotune/src/machine.rs`

- [ ] **Step 1: Write the failing test**

Add a narrow test around the scoring-phase output path that expects both lines:

```rust
assert!(output.contains("[autotune] iteration 3 — score: rank=0.1234"));
assert!(output.contains(
    "[autotune] iteration 3 — metrics: latency_ms=12.3000, throughput=88.0000"
));
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p autotune run_scoring_prints_metrics_status -- --exact`
Expected: FAIL because the metrics line is not printed yet.

- [ ] **Step 3: Write minimal implementation**

In `run_scoring()`, keep the existing score line and add:

```rust
println!(
    "[autotune] iteration {} — metrics: {}",
    state.current_iteration,
    format_metrics_status(&candidate_metrics)
);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p autotune run_scoring_prints_metrics_status -- --exact`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/autotune/src/machine.rs
git commit -m "feat: print measured metrics after scoring"
```

### Task 3: Run focused and full verification

**Files:**
- Modify: `crates/autotune/src/machine.rs`
- Test: `crates/autotune/src/machine.rs`

- [ ] **Step 1: Run focused crate tests**

Run: `cargo test -p autotune`
Expected: PASS

- [ ] **Step 2: Run repository pre-commit checks**

Run: `cargo fmt --all`
Expected: exit 0

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: exit 0

Run: `cargo nextest run`
Expected: all tests pass

- [ ] **Step 3: Commit verified implementation**

```bash
git add crates/autotune/src/machine.rs
git commit -m "feat: show iteration metrics in scoring output"
```
