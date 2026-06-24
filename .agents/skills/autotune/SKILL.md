---
name: autotune
description: >-
  Use when running autotune — the autonomous, metric-driven code-tuning CLI
  (assumed already installed) — on a project to improve a measurable metric such
  as benchmark speed, test coverage, binary size, or model accuracy (e.g. "use
  autotune to speed up X", "tune this project with autotune", "have autotune
  improve my benchmark/coverage"). Also use when an autotune run misbehaves
  (crashes, hangs, banks no real improvement, scores real wins as noise) or lacks
  a config option / adaptor / behavior your project needs.
allowed-tools: Bash, Read, Write, Edit
---

# Autotune

## Overview

Autotune is an autonomous, metric-driven tuning loop: you give it an
`.autotune.toml` (a metric to optimize, test commands for correctness, a scoring
rule) and it runs LLM agents that iteratively edit your code, measure the metric,
and keep only the changes that genuinely improve it.

**Your role is to drive autotune and judge its output — you are a *user* of
autotune, not a maintainer of it.** You tune *your* config and review *your*
results. You do **NOT** modify autotune's source, patch your installed binary, or
hack around its behavior. When autotune is wrong or missing something, **file an
issue** (see the last section — that boundary is the whole point of this skill).

## When to use

- "Use autotune to improve `<metric>` on `<project>`" — speed/benchmark,
  coverage, binary size, accuracy, any measurable objective.
- An autotune run is misbehaving (crash, hang, no real improvement, noise scored
  as signal) or lacks a feature your project needs.

**Not for:** developing/hardening autotune itself — that's a different goal (the
`dogfooding-autotune` skill). Not for a one-off tweak you'd just make by hand.

## Prerequisites

- `autotune` on PATH (verify with `autotune --help`). If it isn't installed,
  that's the user's setup, not something to work around here.
- A project with a **measurable metric** and a **correctness gate** (tests).
  Autotune optimizes exactly what you measure; if you can't measure it or can't
  tell when it broke, you can't tune it.

## Set up the task (`.autotune.toml`)

At the project root. Treat `autotune`'s own docs / `--help` as the authoritative
schema; the shape:

- `[task]` — `name`, `description` (what to optimize **and what MUST NOT
  change**), `canonical_branch`, stop conditions (`max_iterations` /
  `target_improvement`).
- `[paths]` — `tunable` globs autotune may edit; `denied` globs it must never
  touch (benchmarks, tests, generated code).
- `[[test]]` — the correctness gate: commands that must pass. A change that
  breaks tests is discarded.
- `[[measure]]` + adaptor — how the metric is extracted. **For runtime
  performance use the `criterion` adaptor** (it gives autotune variance/noise
  handling and profiler-first research guidance). Use regex/script/judge for
  coverage/size/accuracy/rubric metrics.
- `[score]` — `weighted_sum` (objective metrics + weights + direction) or
  `threshold`. Mark off-target metrics `guardrail = true` so they can only veto a
  regression, never distort the objective.

## Run, monitor, judge

- Run: `autotune run`. Unattended: `AUTOTUNE_TRACE_FILE=/tmp/at.jsonl
  AUTOTUNE_AUTO_APPROVE=1 autotune run > /tmp/at.log 2>&1 &`. Resume a stopped run
  with `autotune resume --task <name>`.
- Watch the **ledger** (`.autotune/tasks/<task>/ledger.json`) — one row per
  iteration (`kept`/`discarded`/`baseline`). `autotune analyze` emits a
  metric×iteration matrix.
- **Judge efficacy, not just completion.** A run that finishes without crashing
  but banks no real improvement is *also* a failure. Confirm kept changes (a)
  clear the noise envelope and reproduce, (b) keep tests/correctness intact, and
  (c) actually move the cumulative metric. A clean `discard` is a *valid* outcome
  (autotune correctly rejecting a regression or a sub-noise change), not a bug.
- When a run produces a real win: **review the diff yourself** — autotune "kept"
  means *tests passed + metric improved + confirmed*, **not** human-reviewed —
  then turn it into a PR.

## When something's off → file an issue; do NOT fix autotune

If autotune crashes, hangs, orphans processes, fills the disk, banks no real
improvement, scores genuine wins as noise (or noise as signal), behaves badly in
the agent loop, or **lacks a config option / adaptor / scoring behavior your
project needs** — that is an autotune **defect or gap**, and you are a user of a
released tool.

**Do NOT** edit autotune's source, patch or rebuild your installed binary, or
silently hack around the gap and move on. That forks you off the release, hides
the problem from the people who can fix it properly, and rots.

**DO** file an issue — `gh issue create --repo Roger-luo/autotune` — with:
1. **Use case** — the project + metric, the relevant `.autotune.toml` sections,
   and the exact command.
2. **What happened** — the malfunction or limitation, with concrete evidence
   (trace/log excerpts, the ledger rows, the scoring reason). A minimal repro if
   you can.
3. **Expected / desired behavior** — what autotune should have done, or the
   config/feature you needed and why.

Label `bug` for broken behavior, `enhancement` for a missing feature. Filing the
issue *is* the deliverable for an autotune problem — not a "parallel track" you
do after quietly working around it.

### Config-tune it yourself  vs.  file an issue

| You CAN fix this (it's YOUR config) | File an issue (it's autotune's) |
|---|---|
| loose/wrong scorer, noise threshold, weights, direction | scorer banks noise / rejects real wins **despite** correct config |
| wrong `tunable`/`denied` paths, test command, metric/adaptor choice | a config knob you need doesn't exist (e.g. per-benchmark weighting, an adaptor for your metric) |
| build/env the worktree needs (`[worktree] setup`) | autotune crashes / hangs / orphans / fills the disk |
| stop conditions, re-run / resume | the research or implement agent behaves badly regardless of config |

A reasonable config-level workaround (e.g. a `script` adaptor that combines
metrics into one pre-weighted number) is fine to *use* — **and** still worth an
issue if it papers over a genuine gap, so it gets fixed upstream rather than only
in your repo.

## Common mistakes

- **Fixing autotune instead of filing an issue.** You don't own it; patching the
  source or your install forks you off the release. File the issue.
- **"It ran to completion" ≠ success.** Judge whether the kept changes are
  *real* (cleared noise, reproduce, correctness intact).
- **Merging an autotune "kept" change unreviewed.** It passed tests + metric, not
  human review. Read the diff first.
- **Working around a bug/gap silently.** Use the workaround if you must, but file
  the issue so it's fixed properly.
