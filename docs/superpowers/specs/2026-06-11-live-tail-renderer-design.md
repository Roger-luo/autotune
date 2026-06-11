# Height-aware live-tail renderer

**Date:** 2026-06-11
**Status:** Approved — ready for implementation plan

## Problem

When Autotune runs a task it forwards the child process's stdout/stderr as a
dimmed rolling "tail" so the user can see progress without the raw output
dominating the screen. Today this is implemented three times, each with the
same two defects:

1. **Unbounded screen flood.** Each renderer keeps the last 3 *logical* lines
   and erases them with `\x1b[{N}A\x1b[J` (move cursor up N rows, clear to end),
   where `N` is the logical-line count. But the terminal erases *physical* rows.
   When a forwarded line is wider than the terminal it wraps onto multiple
   physical rows, so the cursor-up count under-counts and stale rows are never
   cleared. They accumulate and eventually fill the screen, drowning the
   `[autotune]` status lines and the user's prior input. Lines are truncated to
   a hardcoded 120 chars, which does nothing when the terminal is narrower than
   120 columns.
2. **No adaptation to terminal height.** The visible count is hardcoded to 3
   regardless of how tall (or short) the terminal is.

A latent third defect: truncation uses `&line[..120]`, which panics when byte
120 falls inside a multi-byte UTF-8 character.

The three current copies:

| Site | File | Function | Content |
|---|---|---|---|
| Task measuring output | `crates/autotune-benchmark/src/lib.rs` | `run_command_with_timeout` (~L335) | subprocess stdout/stderr |
| Test / baseline runner | `crates/autotune/src/main.rs` | `run_with_live_tail` (~L1271) | subprocess stdout/stderr |
| Agent tool-use tail | `crates/autotune/src/stream_ui.rs` | `StreamState::{erase_tail,draw_tail}` (~L192) | agent tool-use descriptions, interleaved with markdown |

## Goals

- Eliminate the screen flood: the dim tail occupies a bounded number of
  physical rows and is fully erased when the command finishes.
- Scale the visible line count to the terminal height.
- Remove the triplicated logic — one renderer, used by all three sites — per
  CLAUDE.md's rule: "Don't sprinkle terminal CSI sequences elsewhere; centralize
  in `autotune_agent::terminal`."

## Non-goals

- Correct display-width handling of wide characters (CJK, emoji). We truncate by
  `char` count, so a line of all-wide characters can still be ~2× the column
  budget. This is bounded (worst case a few wrapped rows, never the unbounded
  flood we're fixing) and rare for build/test/benchmark output. Tracked as a
  follow-up Ion issue (would need `unicode-width`).
- Changing *what* content each site shows, or the non-TTY path (output is still
  fully collected and returned; nothing is rendered when not a TTY).

## Design

### Location & dependency

The renderer lives in **`autotune_agent::terminal`** (new sibling logic next to
the existing `Guard`/`restore` machinery). `autotune-agent` is a leaf crate that
`autotune-benchmark` and the `autotune` binary already depend on, so all three
call sites can reach it.

Add the **`terminal_size`** crate to `autotune-agent` for width/height. It is
tiny and purpose-built; preferred over pulling the much heavier `crossterm`
(currently only an `autotune-init` dependency) into a widely-depended-on leaf
crate. No `unsafe` in our code (the ioctl is inside the dependency).

### Core: `TailState` (pure, no I/O ownership, unit-testable)

A plain struct holding the rolling buffer and the count of physical rows last
drawn. All methods take the terminal dimensions and a `&mut impl Write` as
parameters, so tests drive them with injected dimensions and a `Vec<u8>` sink —
no real TTY required.

```rust
pub struct TailState {
    lines: VecDeque<String>,   // most-recent lines, already capped to the budget
    rendered: usize,           // physical rows drawn by the last `draw`
}

impl TailState {
    pub fn new() -> Self;

    /// Append a line, capping the buffer to `rows_for_height(height)`.
    pub fn push(&mut self, line: &str, height: u16);

    /// Erase the previously drawn rows: `\x1b[{rendered}A\x1b[J` (no-op if 0).
    pub fn erase(&mut self, out: &mut impl Write);

    /// Draw the current buffer: each line sanitized to the width budget,
    /// prefixed with two spaces, dimmed when `color`. Sets `rendered`.
    pub fn draw(&mut self, out: &mut impl Write, width: u16, color: bool);

    /// Convenience: `erase` then `draw` (the common per-line update).
    pub fn redraw(&mut self, out: &mut impl Write, width: u16, color: bool);
}
```

`erase` and `draw` are exposed separately (not just `redraw`) because
`stream_ui` interleaves permanent markdown between them: erase tail → render
markdown block → draw tail.

Free functions (also unit-tested directly):

- `pub fn rows_for_height(height: u16) -> usize` → `((height / 4) as usize).clamp(3, 8)`.
- `fn sanitize_line(line: &str, max_cols: usize) -> String` → strip ANSI escape
  sequences (a small inline CSI/SGR-skipping pass, no new dependency), then
  truncate to `max_cols` **chars** (char-boundary safe). Stripping ANSI both
  prevents truncating inside an escape sequence and ensures our dim styling
  isn't cancelled by a reset embedded in the child output. `max_cols` =
  `width.saturating_sub(indent + 1)` (indent = 2 spaces; the `+1` is a one-column
  safety margin so an exactly-full line can't wrap on terminals that wrap at the
  last column).

### Threaded wrapper: `LiveTail` (for the two subprocess sites)

`run_command_with_timeout` and `run_with_live_tail` read stdout and stderr on
two threads that both push lines. They need a cloneable, thread-safe handle that
owns its stderr writing and queries the live terminal size on each update (so a
mid-run resize is handled):

```rust
#[derive(Clone)]
pub struct LiveTail { inner: Arc<Mutex<TailState>>, enabled: bool, color: bool }

impl LiveTail {
    /// Detects TTY + `NO_COLOR` from stderr. When stderr is not a TTY, every
    /// method is a no-op.
    pub fn stderr() -> Self;

    pub fn push_line(&self, line: &str);  // push + redraw, querying current size
    pub fn finish(&self);                 // erase the whole block on completion
}
```

`enabled` mirrors today's `is_tty` gate. `color` follows `style.rs`'s policy
(TTY and `NO_COLOR` unset) so the dim SGR is dropped under `NO_COLOR` while
truncation/erase still happen.

### Call-site integration

- **`run_command_with_timeout`** (benchmark): replace the inline `Tail` struct,
  `redraw`/`redraw2` closures, and final erase with a `LiveTail::stderr()`; the
  two `spawn_line_reader` callbacks call `tail.push_line(line)`; after the child
  exits, `tail.finish()`.
- **`run_with_live_tail`** (main.rs): same substitution — drop the local
  `tail` / `rendered_lines` / `draw_tail` closures in favor of `LiveTail`.
- **`stream_ui::StreamState`**: replace the `tool_tail: VecDeque<String>` and
  `rendered_tail_count: usize` fields with a single `tail: TailState`, and
  rewrite `erase_tail`/`draw_tail` to delegate to `TailState::erase`/`draw`
  (passing the queried width/height and the `style` color flag). `push_tool_use`
  pushes via `TailState::push`. The dim status header in `Stream::new` is
  unchanged.

## Testing

Unit tests in `autotune-agent` (no TTY needed — drive `TailState` with a
`Vec<u8>` writer and fixed dimensions):

- `rows_for_height`: clamps to 3 at small heights (e.g. 4, 10), to 8 at large
  heights (e.g. 40, 100), and scales in between (e.g. 24 → 6).
- `sanitize_line`: strips ANSI escapes; truncates by char count without
  panicking on a multi-byte boundary; respects the width budget.
- `TailState`: after N pushes, `draw` emits exactly `min(N, budget)` rows; a
  following `erase` emits a cursor-up count equal to what was drawn; `redraw`
  erases the prior block before drawing the new one.
- Color gating: with `color = false`, no `\x1b[2m` SGR appears but rows are
  still drawn and erased.

Per the bug-fix workflow, the externally-visible "no flood" behavior is hard to
assert deterministically through the PTY scenario harness; the unit tests above
pin the invariant (erase count always equals drawn rows, both bounded). A
PTY-level no-leak scenario test is noted as a follow-up Ion issue.

## Follow-up Ion issues

- Wide-character (CJK/emoji) display-width truncation via `unicode-width`.
- PTY scenario coverage asserting the dim tail never exceeds its row budget and
  is fully cleared after a command with very wide / very many output lines.

## Notes update

After implementation, add a short note (or extend `notes/`) capturing the
physical-vs-logical-row erase footgun and the height-budget policy, since it's a
non-obvious terminal-rendering constraint a future contributor would otherwise
re-derive.
