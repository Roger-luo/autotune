---
title: Getting started
description: Install Autotune and run your first tuning loop.
section: Guides
order: 1
---

This guide walks through installing Autotune and running a tuning loop against
a local project.

## Prerequisites

- A Rust toolchain (edition 2024).
- The `claude` CLI on your `PATH` for the default agent backend.
- A project with a test command and at least one measurable metric.

## Install

Build from source:

```bash
cargo build --release
```

The binary is produced at `target/release/autotune`.

## Initialize a project

Run the agent-assisted initializer from your project root:

```bash
autotune init
```

This inspects the repository and writes a starter `.autotune.toml` describing
how to build, test, and measure your code.

## Run a tune

```bash
autotune run
```

Autotune drives the loop — planning, implementing, testing, measuring,
scoring, and integrating — until a stop condition is met. If a run is
interrupted, resume exactly where it stopped:

```bash
autotune resume
```

During the run the research agent may ask to use additional tools (e.g.
`Bash`); Autotune prompts you to approve each. To run unattended (CI, a
scheduler, or piped output), set `AUTOTUNE_AUTO_APPROVE=1` so requests are
granted without prompting. See [Tool approval](/configuration/#tool-approval)
for the full behavior.
