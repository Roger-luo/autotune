---
title: Crates
description: Guides for each crate in the Autotune workspace.
section: Crates
sidebarLabel: Overview
order: 0
---

Autotune is a Cargo workspace. The `autotune` binary composes a set of focused
library crates, each with one clear responsibility. These guides explain how to
use each crate; the [API reference](/api/) lists their types and functions.

## Orchestrator

- [`autotune`](/crates/autotune/) — the binary + library that owns the
  crash-recoverable tune-loop state machine and wires everything together.

## Phase drivers

- [`autotune-plan`](/crates/autotune-plan/) — Planning: the research agent
  proposes the next hypothesis.
- [`autotune-implement`](/crates/autotune-implement/) — Implementing: an
  ephemeral, sandboxed agent writes code in a worktree.
- [`autotune-test`](/crates/autotune-test/) — Testing: runs configured test
  commands; a failure discards the candidate.
- [`autotune-benchmark`](/crates/autotune-benchmark/) — Measuring: runs measure
  commands and extracts metrics.
- [`autotune-init`](/crates/autotune-init/) — agent-assisted project
  initialization.

## Pluggable traits & leaves

- [`autotune-config`](/crates/autotune-config/) — parses and validates
  `.autotune.toml`.
- [`autotune-state`](/crates/autotune-state/) — persistent, crash-recoverable
  task state.
- [`autotune-agent`](/crates/autotune-agent/) — the `Agent` trait and CLI
  backends, plus terminal restoration.
- [`autotune-adaptor`](/crates/autotune-adaptor/) — the `MetricAdaptor` trait
  and built-in extractors.
- [`autotune-score`](/crates/autotune-score/) — the `ScoreCalculator` trait and
  built-in scorers.
- [`autotune-git`](/crates/autotune-git/) — git worktree and branch plumbing.
- [`autotune-judge`](/crates/autotune-judge/) — rubric-driven LLM-as-judge
  evaluation.

## Testing

- [`autotune-mock`](/crates/autotune-mock/) — a dev-only mock agent for
  end-to-end scenario tests.
