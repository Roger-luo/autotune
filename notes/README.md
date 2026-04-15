# Notes

Knowledge base for agents working on this repo. AGENTS.md stays lean; detailed
gotchas and design rationale live here.

Each note exists because re-deriving it would waste a non-trivial amount of
time or risk re-introducing a bug. Add a new note when you hit something that
took you a while to figure out and would not be obvious from reading the code.

## Index

- [agent-subprocess.md](agent-subprocess.md) — How the CLI spawns `claude`,
  which flags are required, why `--permission-mode dontAsk` and `--bare` don't
  work for us, and how tool scoping is enforced.
- [agent-protocol.md](agent-protocol.md) — The XML protocol used by research,
  implementation, and init agents. MockAgent response format. Common pitfalls.
- [git-integration.md](git-integration.md) — Advancing branch model, rebase-based
  integration, worktree branch namespacing, conflict resolution by the research
  agent.
- [config-and-tasks.md](config-and-tasks.md) — Global vs project config merge
  rules, task auto-forking, how the implementation agent receives project
  instructions (AGENTS.md / CLAUDE.md).
