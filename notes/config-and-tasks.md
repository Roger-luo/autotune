# Config precedence, task forking, and project instructions

## Config layers (for `autotune run`)

1. **Project config** `.autotune.toml` — loaded by `load_config()`. Required.
2. **Global user config** `~/.config/autotune/config.toml` — agent defaults
   (model, max_turns). Merged by `apply_global_agent_defaults()` in `main.rs`
   so **project settings win; global fills gaps**.
3. **Task name override** from the CLI (`autotune run <name>`), if any.

For agent role settings specifically (`[agent.research]`, `[agent.implementation]`,
`[agent.init]`), `None` fields in project config fall back to the global config.
This lets a user set `model = "sonnet"` globally without editing every project.

## Task auto-forking

`autotune run` never overwrites an existing task. If a task directory with
state.json already exists, the run forks the task name by appending
`-2`, `-3`, etc. — see `next_available_task_name()` in `main.rs`. A fork is
"available" when both:
- the task dir `.autotune/tasks/<name>/` does not exist, AND
- the advancing branch `autotune-<name>` does not exist.

If the task dir exists but state.json is missing (crash before first save),
the directory is cleaned up and reused — no fork.

Users who want to continue the existing task should use `autotune resume`.

## Project instructions for the implementation agent

The implementation agent runs in an ephemeral worktree and receives its
full system prompt from the CLI — we don't rely on Claude CLI's implicit
CLAUDE.md discovery (would surprise users who rename things; also doesn't
compose well with our explicit prompt).

In `crates/autotune-implement/src/lib.rs::run_implementation`:

1. Read `AGENTS.md` from the worktree root. If missing, read `CLAUDE.md`. If
   both missing, skip.
2. Prepend the file's content to the implementation prompt.
3. Then append the hypothesis + rules + denied paths + tool guidance.

AGENTS.md is preferred because it's the emerging cross-tool convention. The
CLAUDE.md fallback is there for existing Claude Code users who haven't
migrated.

The research agent doesn't need this — it runs in the real repo root and
sees everything via its Read/Glob/Grep tools.
