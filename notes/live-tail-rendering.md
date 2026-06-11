# Live-tail rendering

Long-running subprocess output (benchmark/test commands) and the agent's
tool-use stream are shown as a **dimmed rolling tail**: the last few lines,
redrawn in place, then erased when the command finishes. All of it lives in
`autotune_agent::terminal` — `TailState` (pure buffer + renderer), `LiveTail`
(thread-safe stderr wrapper for subprocess output), `rows_for_height`, and
`sanitize_line`. Three call sites use it: `autotune-benchmark`'s
`run_command_with_timeout`, `autotune`'s `run_with_live_tail`, and
`stream_ui::StreamState`.

## The footgun: physical rows vs. logical lines

The tail is erased with `\x1b[{N}A\x1b[J` — move the cursor up `N` rows, clear
to end of screen. `N` is the number of *logical* lines we drew, but the terminal
moves by *physical* rows. If a drawn line is wider than the terminal it wraps
onto multiple physical rows, so `N` under-counts, the erase leaves stale rows
behind, and they accumulate until the screen is full of dim text — drowning the
`[autotune]` status lines and the user's prior input.

The fix is to guarantee **one logical line == one physical row**: every line is
sanitized (`sanitize_line`) and truncated to `width - indent - 1` columns using
the *live* terminal width (`stderr_size`), so it can never wrap. Then the
cursor-up count is exact *for blocks that fit on screen without scrolling*
(see the scroll caveat below). Corollaries:

- Never truncate by byte index (`&line[..120]` panics on a UTF-8 boundary) — we
  truncate on a `char` boundary.
- Truncate by **display width**, not char count, via `unicode-width`: wide
  characters (CJK/emoji) count as 2 columns, so even an all-wide-char line is
  capped at the terminal width and can't wrap.
- Strip ANSI escapes from child output before truncating: truncating mid-escape
  corrupts the sequence, and a child-emitted reset would otherwise cancel our
  dim styling.

One known limitation where the cursor-up count can still be off by a bounded
amount (it does not flood the screen the way the original wrapping bug did):

- **Bottom-of-screen scroll.** When the drawn block sits against the last row,
  printing it scrolls the viewport; the rows that scrolled off the top are gone,
  but `\x1b[{N}A` still assumes they're there, so the next erase under-clears and
  can leave one stale dim row. This is inherent to any in-place cursor-up
  renderer (the old code had it too); fixing it properly needs a scroll region
  or save/restore-cursor, which we judged not worth the complexity here.

## Height policy

Visible rows = `rows_for_height(height)` = `(height / 4).clamp(3, 8)`. A small
fixed `3` floods nothing but wastes a tall terminal; an unbounded fraction would
dominate. The clamp keeps the tail informative without taking over the screen.

## Non-TTY

When stderr is not a TTY, `LiveTail` is a no-op and nothing is rendered during
streaming — the full output is still collected and returned for inspection. The
dim styling additionally honors `NO_COLOR` (consistent with `style.rs`).
