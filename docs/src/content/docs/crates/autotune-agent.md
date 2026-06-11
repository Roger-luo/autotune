---
title: autotune-agent
description: The Agent trait and backends (Claude, Codex) for LLM interaction, plus terminal restoration and styled-output macros.
section: Crates
order: 3
---

`autotune-agent` defines the backend-agnostic `Agent` trait (`spawn`/`send`) that the rest of Autotune uses to drive an LLM, the `ClaudeAgent` and `CodexAgent` implementations that shell out to their respective CLIs, the XML fragment protocol parser, and the supporting `terminal` (state restoration) and `style` (accent-colored status lines) modules.

## When to use it

- You're wiring an LLM into the tune loop and need a uniform interface over different CLI backends.
- You're adding a new agent backend — implement the `Agent` trait and the rest of the system uses it unchanged.
- You're writing user-facing CLI output and need `[autotune]`-tagged status lines (`aprintln!` / `aeprintln!`) or terminal-cleanup guarantees (`terminal::Guard`).
- You're parsing the agent's XML response fragments (`<plan>`, `<task>`, etc.) into typed values.

## Public API

- `Agent` — core trait: `spawn(&AgentConfig)`, `send(&AgentSession, &str)`, streaming variants `spawn_streaming`/`send_streaming`, plus `backend_name`, `handover_command`, `hydrate_session`, and `grant_session_permission` (default errors).
- `AgentConfig` — prompt, `allowed_tools`, `working_directory`, optional `model`, `max_turns`, `reasoning_effort`.
- `AgentConfigWithEvents` — wraps an `AgentConfig` with an optional `EventHandler`; built via `new` / `with_event_handler`.
- `AgentSession` — `session_id` + `backend`, returned for resuming a conversation.
- `AgentResponse` — `text` + `session_id` produced by a turn.
- `AgentEvent` — streaming event, either `ToolUse { tool, input_summary }` or `Text(String)`.
- `EventHandler` — `Box<dyn Fn(AgentEvent) + Send + Sync>` callback invoked per streaming event.
- `ToolPermission` — `Allow`, `AllowScoped(tool, path)`, or `Deny` tool gating.
- `AgentError` — `CommandFailed`, `ParseFailed`, `Timeout`, `Interrupted`, `Io`.
- `claude::ClaudeAgent` — Claude CLI backend; `new()` or `with_command(PathBuf)` for a custom binary.
- `codex::CodexAgent` — Codex CLI backend; `new()`, `with_command`, `with_command_and_codex_home`.
- `protocol::parse_agent_response` — parse a response into `Vec<AgentFragment>` (`Message`, `Question`, `Task`, `Paths`, `Test`, `Measure`, `Score`, `Agent`, `Rubric`, `RubricsDone`).
- `protocol::parse_tool_requests`, `lenient_find_all`, `ToolRequest`, `QuestionOption`, `RubricProposal`, `TagMatch` — protocol parsing helpers and types.
- `terminal::Guard` — RAII guard that calls `restore()` on drop.
- `terminal::restore` — write terminal-restore CSI sequences (no-op when stderr isn't a TTY).
- `terminal::install_panic_hook` — install an idempotent panic hook that restores the terminal before delegating.
- `terminal::LiveTail` / `TailState` — render a dimmed live tail of recent subprocess output lines; `rows_for_height`, `stderr_size` helpers.
- `trace::init` / `is_enabled` / `record` — optional tracing of agent activity.
- `aprintln!` / `aeprintln!` — `println!`/`eprintln!` replacements that wrap `[autotune]` lines in the accent color on a TTY; backed by `style::accent`, `stdout_color`, `stderr_color`.

## Usage

```rust
use autotune_agent::{
    aprintln,
    claude::ClaudeAgent,
    terminal, Agent, AgentConfig, ToolPermission,
};
use std::path::PathBuf;

fn run() -> Result<(), autotune_agent::AgentError> {
    // Restore terminal modes on every exit path from this scope.
    let _guard = terminal::Guard::new();

    let agent = ClaudeAgent::new();
    let config = AgentConfig {
        prompt: "Propose the next optimization hypothesis.".to_string(),
        allowed_tools: vec![
            ToolPermission::Allow("Read".to_string()),
            ToolPermission::Deny("Bash".to_string()),
        ],
        working_directory: PathBuf::from("."),
        model: None,
        max_turns: None,
        reasoning_effort: None,
    };

    // First turn creates a session.
    let first = agent.spawn(&config)?;
    aprintln!("[autotune] planning session: {}", first.session_id);

    // Continue the same conversation.
    let session = autotune_agent::AgentSession {
        session_id: first.session_id,
        backend: agent.backend_name().to_string(),
    };
    let reply = agent.send(&session, "Refine that into a concrete change.")?;
    println!("{}", reply.text);
    Ok(())
}
```

## Internal dependencies

- `autotune-config` — agent backends emit/consume config section types (`TaskConfig`, `PathsConfig`, `TestConfig`, `MeasureConfig`, `ScoreConfig`, `AgentSectionConfig`) surfaced through the protocol fragments.

## Notes

- **Session-context model:** `ClaudeAgent` keeps per-session state (allowed tools, working directory, model, turns, reasoning effort) in a `Mutex<HashMap>`, looked up by `session_id` on each `send`. Don't construct a fresh agent between `spawn` and `send` or the context is lost. After a crash/resume, call `hydrate_session` to re-seed that context; `grant_session_permission` adds a tool to a live session for subsequent sends.
- **Permission flags:** the Claude backend uses `--dangerously-skip-permissions` (not `--permission-mode dontAsk`) so scoped `Tool:path` permissions like `Edit:/worktree/**/*.rs` are honored, and avoids `--bare` so OAuth/keychain auth keeps working. `--disallowedTools` still fully blocks denied tools.
- **Terminal restoration discipline:** restoration relies on three overlapping layers — a `Guard` held around any terminal-mutating call, `install_panic_hook()` (call once in `main`), and an explicit `restore()` in signal handlers before `std::process::exit` (Drop and the panic hook don't run on that path). All restore sequences live only in `terminal::restore` — extend that function rather than scattering CSI sequences.
- **Color gating:** accent color and the live tail are emitted only when the destination stream is a TTY and `NO_COLOR` is unset, so piped/redirected output and the test runner see byte-for-byte plain text.
