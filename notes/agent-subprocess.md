# Agent subprocess — CLI flags and sandboxing

The `ClaudeAgent` backend shells out to the `claude` CLI (`-p` mode) for every
`spawn()` and `send()`. Getting the flag set right is non-obvious: several
seemingly-applicable flags break things in subtle ways. This note documents
why we pass what we pass.

See `crates/autotune-agent/src/claude.rs` → `build_args()` for the canonical list.

## Required flags

| Flag | Why |
|---|---|
| `-p <prompt>` | Non-interactive (print) mode. |
| `--output-format json` (or `stream-json`) | Parseable, deterministic output. |
| `--dangerously-skip-permissions` | Bypass the interactive permission prompt. Safe here because tool scoping still applies — see below. |
| `--disable-slash-commands` | Agents shouldn't invoke skills; prevents unexpected side effects. |
| `--allowedTools <name>` / `--allowedTools <name>:<path>` | Explicit allowlist, optionally path-scoped. |
| `--disallowedTools <name>` | Explicit denylist. Takes precedence over everything. |
| `--model` / `--max-turns` | From config; optional. |

## Why `--dangerously-skip-permissions` is safe (not a blanket bypass)

The name is alarming but misleading. Tested behavior:

- `--dangerously-skip-permissions` skips the **interactive permission prompt**
  only. It does NOT override tool-level rules.
- `--disallowedTools Bash` still blocks Bash. Verified: the agent reports
  "Bash tool isn't available in this session".
- `--allowedTools Edit:/path/**/*.rs` still scopes Edit to that glob.

Combined, agents run non-interactively with exactly the tools we allow — no
prompts, no escalation.

## Flags we deliberately DON'T use

### `--permission-mode dontAsk`

Looks ideal ("only approve what's in --allowedTools, deny everything else, no
prompts"), but it **does not support the scoped `Tool:path` syntax**. Passing
`--allowedTools Edit:/worktree/**/*.rs` to a `dontAsk`-mode agent causes Edit
to be rejected outright, even though it's in the allowed list. We need path
scoping, so we can't use `dontAsk`.

Tested manually with `claude -p "Write 'hello' to /tmp/x.txt" --permission-mode dontAsk --allowedTools "Write:/tmp/autotune-test-*.txt"` — the Write was denied with "Claude Code is running in 'don't ask' mode". Non-scoped `--allowedTools "Write"` works fine under `dontAsk`.

### `--bare`

Looks appealing — skips hooks, LSP, plugins, auto-memory, CLAUDE.md discovery,
attribution. But it **also disables OAuth and keychain reads**, requiring
`ANTHROPIC_API_KEY` to be set explicitly. Most users authenticate via OAuth or
the system keychain, so `--bare` causes a silent "Not logged in · Please run
/login" failure. We load project instructions explicitly instead (see
[config-and-tasks.md](config-and-tasks.md)).

## Grant permissions at runtime

`agent.grant_session_permission(session, permission)` adds a tool to an
existing session. Currently used during merge conflict resolution: the
research agent has read-only tools by default, but integration grants it
`Edit` so it can fix conflict markers. See
[git-integration.md](git-integration.md).

## Codex backend limitation

The local `codex` CLI does not currently expose Claude-style per-tool
allowlists in exec mode. That means we cannot precisely mirror Autotune's
`allowedTools` / `disallowedTools` contract for Codex sessions.

Current behavior is the strictest supported approximation:

- Run Codex with `-a untrusted` plus the sandbox/worktree restrictions Autotune
  already applies.
- Always mount Codex's own state directory (`CODEX_HOME` or `~/.codex`) via
  `--add-dir`. Without that, sandboxed `codex exec` can fail before the agent
  starts because it cannot create session files or read plugin/skill metadata.
- Persist the backend name in task state and explicitly re-hydrate the session
  on resume, because the CLI contract is session-based rather than
  re-derivable from a fresh process invocation alone.

This is intentionally narrower than Claude's explicit tool allowlisting, not
equivalent to it.

## Subprocess lifecycle: process groups & shutdown teardown

Agent CLIs are long-lived and spawn their own descendants (the node `claude`
binary, MCP servers, Bash-tool subprocesses). If autotune is interrupted while
an agent call is in flight, those would otherwise be reparented to init and keep
running — orphaned, still spending the LLM budget. (Observed: SIGTERM to the
autotune PID left a `claude -p` agent alive for minutes.)

Mechanism (`crates/autotune-agent/src/child.rs`, wired into `claude.rs`/`codex.rs`):

- Each agent subprocess is spawned as its **own process-group leader**
  (`CommandExt::process_group(0)`), and its pid (== pgid) is tracked via a
  `ChildGuard` for the duration of the call.
- The CLI's shutdown handler (`ctrlc`, with the `termination` feature so it
  also catches **SIGTERM**, not just SIGINT) calls `terminate_active_children()`,
  which `killpg`s each tracked group with SIGINT — taking down the CLI *and* its
  descendants without touching autotune's own group.
- Because the child is now in its own group, the terminal's Ctrl-C no longer
  reaches it directly; the handler is the single path that tears agents down,
  for both Ctrl-C and SIGTERM.
- `terminate_active_children()` also sets a sticky `is_shutting_down()` flag.
  After a child exits, the backends check it and return `AgentError::Interrupted`
  regardless of the child's exit status — a SIGINT'd `claude` often exits 0 with
  empty/partial output, which would otherwise look like an unparseable response
  and trip the planning/fix **retry** loop, re-spawning agents *after* a shutdown
  was requested. Reporting `Interrupted` makes `classify_phase_failure` exit
  cleanly instead. Net: SIGTERM mid-run kills the agent, leaves no orphan, does
  no retries, and exits in ~1s.
