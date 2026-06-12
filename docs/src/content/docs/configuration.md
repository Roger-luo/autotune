---
title: Configuration
description: The .autotune.toml reference.
section: Guides
order: 2
---

Autotune reads its configuration from `.autotune.toml` in your project root.
Global defaults can be set once and merged per project. The full set of fields
is owned by the [`autotune-config`](/crates/autotune-config/) crate — see its
[API reference](/api/autotune-config/) for the exact types.

## Task commands

Define how to build, test, and measure the code under tuning. A test failure
discards a candidate before it is ever scored.

## Metric adaptors

An adaptor turns raw task output into a `name → number` map. Built-in adaptors:

- **Regex** — capture a number from stdout with a pattern.
- **Criterion** — read Rust Criterion benchmark results.
- **Script** — shell out to a custom extractor.

## Scoring

A score calculator compares baseline, candidate, and best metrics and returns a
rank plus a keep/discard decision. Built-in calculators include weighted-sum,
threshold, and script-based scorers. See [`autotune-score`](/crates/autotune-score/)
for how the rank is computed.

## Global config

Per-project settings live in `.autotune.toml`. Agent defaults you want to reuse
across every project go in the global config at
`~/.config/autotune/config.toml` (manage it with `autotune config get|set|edit`).

Settings merge with this precedence (lowest to highest):

```
global [agent]  <  global [agent.<role>]  <  project [agent]  <  project [agent.<role>]
```

So a global `[agent.research] model = "opus"` beats a global `[agent] model =
"sonnet"`, and any value set in the project's `.autotune.toml` wins over the
global config. Roles are `research`, `implementation`, `init`, and `judge`.

## Environment variables

| Variable | Effect |
|---|---|
| `AUTOTUNE_AUTO_APPROVE` | Set to `1`/`true` to auto-approve the research agent's runtime tool requests instead of prompting. Required for unattended/CI runs — see [Tool approval](#tool-approval). |
| `AUTOTUNE_GLOBAL_CONFIG` | Path to an alternate global config file, overriding `~/.config/autotune/config.toml`. |
| `AUTOTUNE_TRACE_FILE` | Path to a JSONL trace file. Autotune appends one record per agent call, phase transition, and approval decision — a replay log for debugging a run. The file must not already exist. |
| `NO_COLOR` | Disables Autotune's accent coloring of `[autotune]` status lines. |

## Tool approval

The research agent starts with read-only tools (`Read`, `Glob`, `Grep`) and can
request more (e.g. `Bash`) at runtime. By default Autotune prompts you to
approve each request for the rest of the run.

When stdin is **not** an interactive terminal (piped, redirected, or run from a
scheduler), there's no one to answer the prompt, so Autotune **auto-denies** the
request and continues — the agent proceeds with the tools it already has. To run
fully unattended and grant requests automatically, set `AUTOTUNE_AUTO_APPROVE=1`.
`Edit`, `Write`, and `Agent` are never granted to the research agent regardless.

## Git hooks and worktree setup

Autotune commits each candidate implementation in an isolated git worktree.
Your project's git hooks (`pre-commit`, `commit-msg`) **do run** on these
commits — they are part of your validation harness. If a hook rejects the
commit (a missing license header, a failing linter), Autotune treats the
candidate like a failed test: it feeds the hook output back to the
implementation agent to fix, retrying within the `max_fix_attempts` budget, and
discards the candidate if it still can't pass. Hooks are never bypassed, so a
kept candidate always satisfies the same gates a human commit would.

For hooks to run correctly in a fresh worktree, the environment sometimes needs
preparation — for example `mise` won't load a `mise.toml` until its (new)
worktree path is trusted. Use `[worktree] setup` to run any preparation
commands in the new worktree before the implementer runs:

```toml
[worktree]
setup = [
    ["mise", "trust"],
]
```

Each entry is a full command run with the worktree as its working directory, in
order; a non-zero exit aborts the run. This is tool-agnostic — use it for
whatever your project's hooks and tooling need.

Not installed yet? Start with the [getting started](/getting-started/) guide.
