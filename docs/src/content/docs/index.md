---
title: Autotune
description: Autonomous, metric-driven tuning of codebases with LLM agents.
section: Guides
sidebarLabel: Overview
order: 0
---

Autotune is a Rust CLI that orchestrates autonomous, metric-driven tuning of
codebases using LLM agents. It owns the tuning loop as an explicit,
crash-recoverable state machine: it spawns agents to research and implement
changes while keeping deterministic control over testing, measurement, metric
extraction, scoring, and git integration.

## Why Autotune

- **Deterministic control loop.** The CLI — not the model — decides when to
  test, measure, score, keep, or discard. Every transition is persisted to
  disk, so an interrupted run resumes exactly where it left off.
- **Metric-driven.** A change is accepted only when it improves the metrics you
  define. Adaptors extract numbers from arbitrary task output.
- **Sandboxed implementation.** Each candidate is written by an ephemeral agent
  inside an isolated git worktree, then integrated only if it scores well.

## Next steps

- [Getting started](/getting-started/) — install and run your first tune.
- [Configuration](/configuration/) — the `.autotune.toml` reference.
- [Crates](/crates/) — guides for each workspace crate.
- [API reference](/api/) — types and functions generated from rustdoc.
