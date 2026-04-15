# Coverage Autotune Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a repository `.autotune.toml` that tunes for improved full-workspace line coverage measured with `cargo llvm-cov`.

**Architecture:** Keep the configuration minimal and repository-local. Use the standard full-workspace `cargo nextest run` as the explicit test gate, then measure workspace coverage with `cargo llvm-cov nextest --workspace --summary-only` and extract the line coverage percentage with a regex adaptor.

**Tech Stack:** TOML config, `cargo nextest`, `cargo llvm-cov`

---

### Task 1: Add The Coverage Tuning Config

**Files:**
- Create: `.autotune.toml`
- Modify: `docs/superpowers/specs/2026-04-15-coverage-autotune-config-design.md`

- [ ] **Step 1: Align the spec with the final extraction approach**

Update the measure section in `docs/superpowers/specs/2026-04-15-coverage-autotune-config-design.md` to say the config uses the built-in regex adaptor against `cargo llvm-cov nextest --workspace --summary-only`, not a helper script.

- [ ] **Step 2: Write the config**

Create `.autotune.toml` with:

```toml
[task]
name = "test-coverage"
description = "Improve full-workspace line coverage measured with cargo llvm-cov"
canonical_branch = "main"
max_iterations = "10"

[paths]
tunable = ["crates/**"]

[[test]]
name = "workspace-tests"
command = ["cargo", "nextest", "run"]
timeout = 1800

[[measure]]
name = "workspace-line-coverage"
command = [
  "cargo",
  "llvm-cov",
  "nextest",
  "--workspace",
  "--summary-only",
]
timeout = 3600
adaptor = { type = "regex", patterns = [
  { name = "line_coverage", pattern = 'TOTAL\\s+\\d+\\s+\\d+\\s+[\\d.]+%\\s+\\d+\\s+\\d+\\s+[\\d.]+%\\s+\\d+\\s+\\d+\\s+([\\d.]+)%' },
] }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "line_coverage", direction = "Maximize", weight = 1.0 }]
```

- [ ] **Step 3: Verify the file contents load cleanly**

Run: `cargo test -p autotune-config parse_minimal_valid_config -- --exact`

Expected: PASS for the targeted config parser test, confirming the config crate remains healthy after the new file is added.
