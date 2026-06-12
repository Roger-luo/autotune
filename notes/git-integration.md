# Git integration — advancing branch & rebase

Autotune never mutates the user's canonical branch (typically `main`).
Each task accumulates its kept iterations on a per-task **advancing branch**;
the user PRs that branch into canonical when ready.

## Project git hooks are part of the validation harness

Autotune does **not** bypass the project's git hooks. The implementer commits
its candidate through `autotune_git::stage_all_and_commit`, which runs the
project's `pre-commit`/`commit-msg` hooks normally. The rationale (per a
deliberate design decision): the hooks *are* the project's definition of valid
code — license headers, linters, formatters. Bypassing them (`--no-verify` /
`core.hooksPath=/dev/null`) would let the implementer "succeed" with code the
project considers broken, and autotune would keep it.

So a hook-rejected commit is treated like a failed test:

- `stage_all_and_commit` surfaces the hook output as a `GitError::CommandFailed`.
- `autotune-implement` maps that to `ImplementError::CommitRejected { output, .. }`
  (initial commit) or `FixOutcome::HookRejected { output, .. }` (a fix turn),
  carrying the implementer's session so the fix can continue in-context.
- `machine::run_implementing` / `run_fixing` feed the hook output back to the
  implementer via the **Fixing** loop (`route_commit_rejection` →
  `hook_failure_feedback`), bounded by the same `max_fix_attempts` budget as
  test failures. If the implementer can't satisfy the hooks within budget, the
  candidate is **discarded** (not crashed).

### Candidate commits exclude transient artifacts

`stage_all_and_commit` does not blindly `git add -A`. It excludes a built-in set
of never-commit globs (`NEVER_COMMIT_GLOBS`: `*.snap.new`, `*.orig`, `*.rej`)
via `:(exclude,glob)` pathspecs. These are written by test/tooling runs (insta
pending snapshots, patch/merge rejects), not source edits; staging them polluted
candidate commits and — when a later step deleted a committed `.snap.new` — left
the worktree dirty enough to break the integration rebase. They stay in the
worktree as untracked files; `reset_to_head` cleans them before the rebase.

### Worktree environment setup (`[worktree] setup`)

For the project's hooks to actually *run* in a fresh worktree, the environment
sometimes needs preparation — e.g. `mise` refuses to load the worktree's
`mise.toml` until its new path is trusted. This is **configurable, not
hard-coded to any one tool**: `[worktree] setup` in `.autotune.toml` is a list
of commands run (in order, in the new worktree) right after creation, before
the implementer runs. `machine::run_worktree_setup` executes them; a non-zero
exit aborts the run. Example:

```toml
[worktree]
setup = [["mise", "trust"]]
```

## Branch layout

```
main (canonical, untouched)
  └── autotune-<task-name>           # advancing branch, created at task start
       ├── autotune/<task>/approach-1  # worktree branch, iteration 1
       ├── autotune/<task>/approach-2  # worktree branch, iteration 2
       └── ...
```

- **Canonical branch** (`state.canonical_branch`, from config): the user's
  trunk. Autotune only reads from it.
- **Advancing branch** (`state.advancing_branch`, `autotune/<task>-main`):
  created from canonical at task start. Each kept iteration advances this
  branch linearly. The `-main` suffix is load-bearing: worktree branches
  live at `autotune/<task>/<slug>`, and git refuses to create a branch whose
  parent path is already occupied by another branch's ref file. Naming the
  advancing branch `autotune/<task>-main` keeps it a sibling of the
  worktree namespace, not a prefix of it.
- **Worktree branch** (`autotune/<task>/<approach-slug>`): one per iteration,
  created from the advancing branch. Namespaced under the task so worktree
  branches from different task forks don't collide on matching approach names.

## Integration flow

`run_integrating` in `crates/autotune/src/machine.rs`:

1. **Rebase the worktree branch onto the advancing branch** — run the rebase
   inside the worktree dir (the branch is checked out there; you can't
   checkout a worktree-attached branch from the main repo).
2. If conflicts: the research agent is granted `Edit` permission and asked to
   resolve the conflict markers. Loops `rebase --continue` up to
   `MAX_CONFLICT_ROUNDS` times (each commit being replayed may conflict
   separately).
3. **Remove the worktree** — detaches the branch.
4. **Fast-forward the advancing branch** onto the rebased commits (`merge --ff-only`).

Result: linear history, no merge commits, canonical untouched.

## Why rebase instead of cherry-pick

For single-commit iterations (our current case, since the implementation agent
produces one commit per iteration) rebase and cherry-pick are functionally
identical. Rebase is a future-proofing choice: if the implementation agent
ever produces multiple commits, rebase replays all of them in order, while
cherry-pick only moves one.

## Conflict resolution by the research agent

`resolve_rebase_conflicts` in `machine.rs`:

- Grants `Edit` to the existing research agent session (via
  `agent.grant_session_permission`). Read-only tools stay granted.
- Sends a conflict-resolution prompt listing the conflicted files.
- After the agent turn, verifies conflicts are resolved via
  `autotune_git::has_merge_conflicts`. If yes, calls `rebase_continue`.
- Repeats up to `MAX_CONFLICT_ROUNDS = 10` iterations. Gives up → discards
  the iteration and aborts the rebase.

## Baseline is measured against the working tree, not canonical

`cmd_run` measures the baseline before creating the advancing branch, in
whatever state the working tree happens to be in — no checkout, no reset.
This matters when a prior task's advancing branch has been merged (or not)
back into canonical: the new run's baseline will only reflect those prior
wins if canonical is both updated *and* currently checked out. Full matrix
in `config-and-tasks.md` § "Baseline semantics on fork".

## Resume behavior

If the CLI crashes during integration, `resume` checks whether the approach's
commit SHA is already reachable from the advancing branch. If yes, it moves
straight to `Recorded`. Otherwise it retries the rebase. See
`crates/autotune/src/resume.rs`.

## Test fixtures that touch branches

Integration tests (`crates/autotune/tests/integration_test.rs`) create the
advancing branch manually in `setup_task()` — `cmd_run` does this in the real
path, but the tests call `run_task` directly, starting at `Planning`. If
you add a test that exercises `Integrating`, make sure the advancing branch
exists before calling `run_task`.
