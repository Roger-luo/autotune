# Height-aware live-tail renderer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three duplicated rolling-tail renderers with one shared, terminal-height-bounded renderer in `autotune_agent::terminal`, fixing the physical-vs-logical-row erase bug that lets dim subprocess output flood the screen.

**Architecture:** A pure, unit-testable `TailState` (rolling line buffer + rendered-row count) plus two free functions (`rows_for_height`, `sanitize_line`) form the core. A thread-safe `LiveTail` wrapper drives `TailState` against stderr for the two subprocess sites; `stream_ui` embeds `TailState` directly for the agent tool tail. Terminal size comes from the `terminal_size` crate; each render truncates lines to the live terminal width (so one logical line = one physical row, making the cursor-up erase exact) and caps the visible count to `(height/4).clamp(3,8)`.

**Tech Stack:** Rust 2024, `terminal_size` crate, existing `autotune_agent::{terminal, style}` modules.

**Spec:** `docs/superpowers/specs/2026-06-11-live-tail-renderer-design.md`

**Branch:** `feat/live-tail-renderer` (already created; spec already committed there).

---

## File Structure

- **Modify** `crates/autotune-agent/Cargo.toml` — add `terminal_size` dependency.
- **Modify** `crates/autotune-agent/src/terminal.rs` — add `rows_for_height`, `strip_ansi`, `sanitize_line`, `TailState`, `stderr_size`, `LiveTail`, and their unit tests. (Currently 163 lines; grows to ~330. Stays cohesive — all terminal/CSI logic lives here per CLAUDE.md.)
- **Modify** `crates/autotune-benchmark/src/lib.rs` — `run_command_with_timeout` (~L335) uses `LiveTail`.
- **Modify** `crates/autotune/src/main.rs` — `run_with_live_tail` (~L1271) uses `LiveTail`.
- **Modify** `crates/autotune/src/stream_ui.rs` — `StreamState` embeds `TailState`.
- **Create** `notes/live-tail-rendering.md` + **Modify** `notes/README.md` — document the erase footgun and height policy.

---

## Task 1: Dependency + `rows_for_height`

**Files:**
- Modify: `crates/autotune-agent/Cargo.toml`
- Modify: `crates/autotune-agent/src/terminal.rs`

- [ ] **Step 1: Add the `terminal_size` dependency**

In `crates/autotune-agent/Cargo.toml`, under `[dependencies]`, add the line (keep alphabetical-ish ordering near the other small crates):

```toml
terminal_size = "0.4"
```

The `[dependencies]` block becomes:

```toml
[dependencies]
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
quick-xml = "0.39"
autotune-config = { path = "../autotune-config" }
dirs = "6"
terminal_size = "0.4"
```

- [ ] **Step 2: Add module-level imports to `terminal.rs`**

At the top of `crates/autotune-agent/src/terminal.rs`, the only import today is `use std::sync::Once;` (line 54). Replace it with:

```rust
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex, Once};
```

- [ ] **Step 3: Write the failing test for `rows_for_height`**

Add to the `#[cfg(test)] mod tests` block at the bottom of `terminal.rs` (after the existing tests, before the closing `}`):

```rust
#[test]
fn rows_for_height_clamps_low() {
    assert_eq!(rows_for_height(0), 3);
    assert_eq!(rows_for_height(4), 3);
    assert_eq!(rows_for_height(10), 3); // 10/4 = 2 -> clamped to 3
}

#[test]
fn rows_for_height_scales_in_band() {
    assert_eq!(rows_for_height(24), 6); // 24/4 = 6
    assert_eq!(rows_for_height(28), 7);
}

#[test]
fn rows_for_height_clamps_high() {
    assert_eq!(rows_for_height(40), 8); // 40/4 = 10 -> clamped to 8
    assert_eq!(rows_for_height(200), 8);
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo nextest run -p autotune-agent -E 'test(rows_for_height)'`
Expected: FAIL to compile — `cannot find function rows_for_height`.

- [ ] **Step 5: Implement `rows_for_height`**

Add to `terminal.rs` (above the `#[cfg(test)]` module, e.g. right after `install_panic_hook`):

```rust
/// Number of physical rows the live tail may occupy, scaled to the terminal
/// height: `(height / 4)` clamped to `[3, 8]`. Keeps the dim tail visible
/// without letting it dominate the screen.
pub fn rows_for_height(height: u16) -> usize {
    ((height / 4) as usize).clamp(3, 8)
}
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo nextest run -p autotune-agent -E 'test(rows_for_height)'`
Expected: PASS (3 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/autotune-agent/Cargo.toml crates/autotune-agent/src/terminal.rs
git commit -m "feat(agent): add terminal_size dep and rows_for_height tail-budget policy"
```

---

## Task 2: `strip_ansi` + `sanitize_line`

**Files:**
- Modify: `crates/autotune-agent/src/terminal.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `terminal.rs`:

```rust
#[test]
fn sanitize_strips_sgr_color() {
    assert_eq!(sanitize_line("\x1b[31mred\x1b[0m", 80), "red");
    assert_eq!(sanitize_line("\x1b[1;38;5;208mhi\x1b[0m there", 80), "hi there");
}

#[test]
fn sanitize_strips_osc_sequence() {
    // OSC 0 ; title BEL  — a terminal-title escape some tools emit.
    assert_eq!(sanitize_line("\x1b]0;my title\x07ok", 80), "ok");
}

#[test]
fn sanitize_truncates_by_char_count() {
    assert_eq!(sanitize_line("abcdef", 3), "abc");
}

#[test]
fn sanitize_is_char_boundary_safe() {
    // Truncating "héllo" to 4 chars must not panic on the multi-byte 'é'.
    let out = sanitize_line("héllo", 4);
    assert_eq!(out.chars().count(), 4);
    assert_eq!(out, "héll");
}

#[test]
fn sanitize_truncates_multibyte_without_panic() {
    // 4-byte emoji, truncate to 1 char.
    assert_eq!(sanitize_line("😀x", 1), "😀");
}

#[test]
fn sanitize_drops_carriage_returns_and_controls() {
    assert_eq!(sanitize_line("a\rb", 80), "ab");
}

#[test]
fn sanitize_empty_budget_yields_empty() {
    assert_eq!(sanitize_line("anything", 0), "");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p autotune-agent -E 'test(sanitize)'`
Expected: FAIL to compile — `cannot find function sanitize_line`.

- [ ] **Step 3: Implement `strip_ansi` and `sanitize_line`**

Add to `terminal.rs` (near `rows_for_height`):

```rust
/// Remove ANSI escape sequences (CSI like `\x1b[…m` and OSC like
/// `\x1b]…\x07`) from `s`. A small hand-rolled pass — no extra dependency.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                // CSI: consume params/intermediates until a final byte 0x40..=0x7E.
                chars.next();
                while let Some(&pc) = chars.peek() {
                    chars.next();
                    if ('\u{40}'..='\u{7e}').contains(&pc) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: consume until BEL (\x07) or ST (ESC \).
                chars.next();
                while let Some(c2) = chars.next() {
                    if c2 == '\x07' {
                        break;
                    }
                    if c2 == '\x1b' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Any other escape form: drop the single following byte if present.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Strip ANSI escapes and remaining control characters (except tab), then
/// truncate to at most `max_cols` characters — char-boundary safe, so it never
/// panics on multi-byte UTF-8. Stripping escapes both prevents truncating
/// inside an escape sequence and stops a child-emitted reset from cancelling
/// our dim styling.
fn sanitize_line(line: &str, max_cols: usize) -> String {
    strip_ansi(line)
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .take(max_cols)
        .collect()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p autotune-agent -E 'test(sanitize)'`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/autotune-agent/src/terminal.rs
git commit -m "feat(agent): add ANSI-stripping, char-safe line sanitizer for tail"
```

---

## Task 3: `TailState` core

**Files:**
- Modify: `crates/autotune-agent/src/terminal.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `terminal.rs`:

```rust
fn drawn(buf: &[u8]) -> String {
    String::from_utf8(buf.to_vec()).unwrap()
}

#[test]
fn tailstate_push_caps_to_budget() {
    let mut t = TailState::new();
    for i in 0..10 {
        t.push(&format!("line {i}"), 24); // budget = 6
    }
    let mut buf = Vec::new();
    t.draw(&mut buf, 80, false);
    // 6 rows drawn, each terminated by '\n'.
    assert_eq!(drawn(&buf).matches('\n').count(), 6);
    // Newest line retained, oldest dropped.
    assert!(drawn(&buf).contains("line 9"));
    assert!(!drawn(&buf).contains("line 3"));
}

#[test]
fn tailstate_draw_emits_min_count() {
    let mut t = TailState::new();
    t.push("a", 24);
    t.push("b", 24);
    let mut buf = Vec::new();
    t.draw(&mut buf, 80, false);
    assert_eq!(drawn(&buf), "  a\n  b\n");
}

#[test]
fn tailstate_draw_color_wraps_in_dim() {
    let mut t = TailState::new();
    t.push("x", 24);
    let mut buf = Vec::new();
    t.draw(&mut buf, 80, true);
    assert_eq!(drawn(&buf), "  \x1b[2mx\x1b[0m\n");
}

#[test]
fn tailstate_erase_matches_drawn_rows() {
    let mut t = TailState::new();
    t.push("a", 24);
    t.push("b", 24);
    let mut buf = Vec::new();
    t.draw(&mut buf, 80, false); // rendered = 2
    let mut erase_buf = Vec::new();
    t.erase(&mut erase_buf);
    assert_eq!(drawn(&erase_buf), "\x1b[2A\x1b[J");
}

#[test]
fn tailstate_erase_noop_when_nothing_drawn() {
    let mut t = TailState::new();
    let mut buf = Vec::new();
    t.erase(&mut buf);
    assert!(buf.is_empty());
}

#[test]
fn tailstate_redraw_erases_prev_then_draws() {
    let mut t = TailState::new();
    t.push("a", 24);
    t.push("b", 24);
    let mut buf = Vec::new();
    t.draw(&mut buf, 80, false); // rendered = 2
    t.push("c", 24);
    let mut buf2 = Vec::new();
    t.redraw(&mut buf2, 80, false);
    assert_eq!(drawn(&buf2), "\x1b[2A\x1b[J  a\n  b\n  c\n");
}

#[test]
fn tailstate_draw_truncates_to_width() {
    let mut t = TailState::new();
    t.push("0123456789", 24);
    let mut buf = Vec::new();
    t.draw(&mut buf, 6, false); // max_cols = 6 - (2+1) = 3
    assert_eq!(drawn(&buf), "  012\n");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p autotune-agent -E 'test(tailstate)'`
Expected: FAIL to compile — `cannot find type TailState`.

- [ ] **Step 3: Implement `TailState`**

Add to `terminal.rs` (after `sanitize_line`):

```rust
/// Two-space indent applied to every tail line.
const TAIL_INDENT: usize = 2;

/// Rolling buffer of the most recent output lines plus the count of physical
/// rows last drawn, so the next erase removes exactly what was rendered.
///
/// Pure state + rendering: all I/O goes through a caller-supplied [`Write`] and
/// the terminal dimensions are passed in, so it is fully unit-testable without
/// a TTY. [`LiveTail`] drives this against stderr for subprocess output;
/// `stream_ui` embeds it for the agent tool tail.
#[derive(Default)]
pub struct TailState {
    lines: VecDeque<String>,
    rendered: usize,
}

impl TailState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `line`, capping the buffer to the row budget for `height`.
    pub fn push(&mut self, line: &str, height: u16) {
        let budget = rows_for_height(height);
        self.lines.push_back(line.to_owned());
        while self.lines.len() > budget {
            self.lines.pop_front();
        }
    }

    /// Erase the rows drawn by the previous [`draw`](Self::draw). No-op if none.
    pub fn erase(&mut self, out: &mut impl Write) {
        if self.rendered > 0 {
            let _ = write!(out, "\x1b[{}A\x1b[J", self.rendered);
            self.rendered = 0;
        }
    }

    /// Draw the current buffer: each line sanitized to the width budget,
    /// indented, dimmed when `color`. Records the row count so the next
    /// [`erase`](Self::erase) matches exactly.
    pub fn draw(&mut self, out: &mut impl Write, width: u16, color: bool) {
        let max_cols = (width as usize).saturating_sub(TAIL_INDENT + 1);
        for line in &self.lines {
            let text = sanitize_line(line, max_cols);
            if color {
                let _ = writeln!(out, "  \x1b[2m{text}\x1b[0m");
            } else {
                let _ = writeln!(out, "  {text}");
            }
        }
        self.rendered = self.lines.len();
    }

    /// Convenience: erase the previous block then draw the current one.
    pub fn redraw(&mut self, out: &mut impl Write, width: u16, color: bool) {
        self.erase(out);
        self.draw(out, width, color);
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p autotune-agent -E 'test(tailstate)'`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/autotune-agent/src/terminal.rs
git commit -m "feat(agent): add TailState rolling renderer with width-correct erase"
```

---

## Task 4: `stderr_size` + `LiveTail` wrapper

**Files:**
- Modify: `crates/autotune-agent/src/terminal.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `terminal.rs`:

```rust
#[test]
fn stderr_size_returns_positive_dims() {
    // Not a TTY in the test runner -> falls back to the default.
    let (w, h) = stderr_size();
    assert!(w > 0 && h > 0);
}

#[test]
fn livetail_noop_when_not_tty() {
    // In the test runner stderr is not a TTY, so push/finish are no-ops and
    // must not panic. Clones share state.
    let tail = LiveTail::stderr();
    tail.push_line("anything");
    let clone = tail.clone();
    clone.push_line("more");
    tail.finish();
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p autotune-agent -E 'test(stderr_size) + test(livetail)'`
Expected: FAIL to compile — `cannot find function stderr_size` / `cannot find type LiveTail`.

- [ ] **Step 3: Implement `stderr_size` and `LiveTail`**

Add to `terminal.rs` (after `TailState`):

```rust
/// Terminal dimensions `(width, height)` in cells for stderr, falling back to
/// `(80, 24)` when the size can't be queried (e.g. stderr is not a TTY).
pub fn stderr_size() -> (u16, u16) {
    terminal_size::terminal_size_of(std::io::stderr())
        .map(|(w, h)| (w.0, h.0))
        .unwrap_or((80, 24))
}

/// Thread-safe, cloneable handle that renders a live dimmed tail of recent
/// output lines to stderr, bounded to a fraction of the terminal height.
///
/// Created once per subprocess run; the stdout and stderr reader threads each
/// hold a clone and call [`push_line`](Self::push_line) per line. Call
/// [`finish`](Self::finish) after the child exits to erase the block. When
/// stderr is not a TTY every method is a no-op (output is collected elsewhere).
#[derive(Clone)]
pub struct LiveTail {
    inner: Arc<Mutex<TailState>>,
    enabled: bool,
    color: bool,
}

impl LiveTail {
    /// Build a handle bound to stderr, gating on whether stderr is a TTY and on
    /// the `NO_COLOR` convention (matching `style`).
    pub fn stderr() -> Self {
        use std::io::IsTerminal;
        let is_tty = std::io::stderr().is_terminal();
        Self {
            inner: Arc::new(Mutex::new(TailState::new())),
            enabled: is_tty,
            color: is_tty && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    /// Push one output line and redraw the tail. No-op when not a TTY.
    pub fn push_line(&self, line: &str) {
        if !self.enabled {
            return;
        }
        let (width, height) = stderr_size();
        let mut out = std::io::stderr();
        let mut state = self.inner.lock().unwrap();
        state.push(line, height);
        state.redraw(&mut out, width, self.color);
        let _ = out.flush();
    }

    /// Erase the whole tail block. Call once after the command finishes.
    pub fn finish(&self) {
        if !self.enabled {
            return;
        }
        let mut out = std::io::stderr();
        let mut state = self.inner.lock().unwrap();
        state.erase(&mut out);
        let _ = out.flush();
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p autotune-agent -E 'test(stderr_size) + test(livetail)'`
Expected: PASS (2 tests).

- [ ] **Step 5: Run the whole agent crate + clippy**

Run: `cargo nextest run -p autotune-agent && cargo clippy -p autotune-agent --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-agent/src/terminal.rs
git commit -m "feat(agent): add LiveTail stderr wrapper and stderr_size helper"
```

---

## Task 5: Wire `LiveTail` into the benchmark measuring renderer

**Files:**
- Modify: `crates/autotune-benchmark/src/lib.rs` (`run_command_with_timeout`, ~L335-445)

`autotune-benchmark` already depends on `autotune-agent`. The existing tests in this crate call `run_measure_with_output` (→ `run_command_with_timeout`) in a non-TTY context, so they exercise the no-op path and serve as the regression check.

- [ ] **Step 1: Remove the function-local imports no longer needed**

In `run_command_with_timeout`, delete these three lines near the top of the function (currently ~L339-341):

```rust
    use std::collections::VecDeque;
    use std::io::{IsTerminal, Write};
    use std::sync::{Arc, Mutex};
```

- [ ] **Step 2: Replace the tail machinery with `LiveTail`**

Replace everything from `let is_tty = std::io::stderr().is_terminal();` (currently ~L368) through the final `result` / closing brace of the function (currently ~L444) with:

```rust
    let tail = autotune_agent::terminal::LiveTail::stderr();

    let stdout_tail = tail.clone();
    let stdout_handle = spawn_line_reader(child.stdout.take(), move |line| {
        stdout_tail.push_line(line);
    });

    let stderr_tail = tail.clone();
    let stderr_handle = spawn_line_reader(child.stderr.take(), move |line| {
        stderr_tail.push_line(line);
    });

    let result = match wait_for_child(config, &mut child) {
        Ok(status) => collect_output(config, status, stdout_handle, stderr_handle),
        Err(err) => {
            let _ = join_reader(config, stdout_handle);
            let _ = join_reader(config, stderr_handle);
            Err(err)
        }
    };

    tail.finish();

    result
}
```

(This deletes the `Tail` struct, the `redraw`/`redraw2` closures, the `is_tty` gate, and the trailing manual erase — all now inside `LiveTail`.)

- [ ] **Step 3: Build to confirm no unused imports / type errors**

Run: `cargo build -p autotune-benchmark`
Expected: compiles clean. If the compiler flags an unused `use std::io::Read` or similar at module top, leave module-level imports alone — only the three function-local lines from Step 1 should have been removed. (Module top keeps `use std::io::{Read, Write};` — `Read` is used by `spawn_line_reader`'s bound; `Write` may now be unused at module scope: if clippy/`build` reports `Write` unused, change module-level `use std::io::{Read, Write};` to `use std::io::Read;`.)

- [ ] **Step 4: Run the benchmark crate tests + clippy**

Run: `cargo nextest run -p autotune-benchmark && cargo clippy -p autotune-benchmark --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/autotune-benchmark/src/lib.rs
git commit -m "refactor(benchmark): render measuring output via shared LiveTail"
```

---

## Task 6: Wire `LiveTail` into `run_with_live_tail`

**Files:**
- Modify: `crates/autotune/src/main.rs` (`run_with_live_tail`, ~L1271-1384)

- [ ] **Step 1: Replace the whole function body**

Replace the entire `run_with_live_tail` function (from its `fn` signature through its closing brace, currently ~L1271-1384) with:

```rust
/// Run a measure command, forwarding a dimmed live tail of its output to
/// stderr and collecting full stdout/stderr for later inspection.
///
/// Returns `(stdout_bytes, stderr_bytes, exit_status)`.
fn run_with_live_tail(
    program: &str,
    args: &[String],
    working_dir: &Path,
) -> Result<(Vec<u8>, Vec<u8>, std::process::ExitStatus), std::io::Error> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    let tail = autotune_agent::terminal::LiveTail::stderr();

    let mut child = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let child_stdout = child.stdout.take().expect("piped stdout");
    let child_stderr = child.stderr.take().expect("piped stderr");

    let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let stdout_buf2 = stdout_buf.clone();
    let tail_out = tail.clone();
    let stdout_thread = std::thread::spawn(move || {
        let reader = BufReader::new(child_stdout);
        for line in reader.lines().map_while(Result::ok) {
            {
                let mut b = stdout_buf2.lock().unwrap();
                b.extend_from_slice(line.as_bytes());
                b.push(b'\n');
            }
            tail_out.push_line(&line);
        }
    });

    let stderr_buf2 = stderr_buf.clone();
    let tail_err = tail.clone();
    let stderr_thread = std::thread::spawn(move || {
        let reader = BufReader::new(child_stderr);
        for line in reader.lines().map_while(Result::ok) {
            {
                let mut b = stderr_buf2.lock().unwrap();
                b.extend_from_slice(line.as_bytes());
                b.push(b'\n');
            }
            tail_err.push_line(&line);
        }
    });

    let status = child.wait()?;
    stdout_thread.join().ok();
    stderr_thread.join().ok();

    tail.finish();

    let out = Arc::try_unwrap(stdout_buf).unwrap().into_inner().unwrap();
    let err = Arc::try_unwrap(stderr_buf).unwrap().into_inner().unwrap();
    Ok((out, err, status))
}
```

- [ ] **Step 2: Build the binary crate**

Run: `cargo build -p autotune`
Expected: compiles clean (no unused-import warnings — `VecDeque`, `IsTerminal`, and `Write` are no longer imported in this function).

- [ ] **Step 3: Run the binary crate tests + clippy**

Run: `cargo nextest run -p autotune && cargo clippy -p autotune --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/autotune/src/main.rs
git commit -m "refactor(autotune): render baseline measure output via shared LiveTail"
```

---

## Task 7: Embed `TailState` in `stream_ui`

**Files:**
- Modify: `crates/autotune/src/stream_ui.rs`

- [ ] **Step 1: Drop the now-unused `VecDeque` import**

At the top of `stream_ui.rs`, remove:

```rust
use std::collections::VecDeque;
```

(`VecDeque` was used only by `tool_tail`, which this task replaces.)

- [ ] **Step 2: Replace the two tail fields with a `TailState`**

In `struct StreamState`, replace these two fields (currently ~L102-105):

```rust
    /// Rolling buffer of the last 3 tool-use descriptions currently shown.
    tool_tail: VecDeque<String>,
    /// How many dim lines we last rendered to stderr (so we can erase them).
    rendered_tail_count: usize,
```

with:

```rust
    /// Rolling tail of recent tool-use descriptions (height-bounded, dimmed).
    tail: autotune_agent::terminal::TailState,
```

- [ ] **Step 3: Update the constructor**

In `StreamState::new` (currently ~L114-126), replace these two initializers:

```rust
            tool_tail: VecDeque::new(),
            rendered_tail_count: 0,
```

with:

```rust
            tail: autotune_agent::terminal::TailState::new(),
```

- [ ] **Step 4: Rewrite `erase_tail` and `draw_tail` to delegate**

Replace `erase_tail` and `draw_tail` (currently ~L192-207) with:

```rust
    /// Erase the currently rendered tail lines from stderr.
    fn erase_tail(&mut self, stderr: &mut impl Write) {
        self.tail.erase(stderr);
    }

    /// Re-render the current tool tail (height-bounded, dimmed) to stderr.
    fn draw_tail(&mut self, stderr: &mut impl Write) {
        let (width, _) = autotune_agent::terminal::stderr_size();
        let color = autotune_agent::style::stderr_color();
        self.tail.draw(stderr, width, color);
    }
```

- [ ] **Step 5: Update `push_tool_use` to push through `TailState`**

In `push_tool_use` (currently ~L227-240), replace this block:

```rust
        let detail = describe_tool_use(tool, input_summary);
        self.tool_tail.push_back(detail);
        if self.tool_tail.len() > 3 {
            self.tool_tail.pop_front();
        }
        let mut stderr = std::io::stderr();
```

with:

```rust
        let detail = describe_tool_use(tool, input_summary);
        let (_, height) = autotune_agent::terminal::stderr_size();
        self.tail.push(&detail, height);
        let mut stderr = std::io::stderr();
```

(The subsequent `self.erase_tail(&mut stderr); self.draw_tail(&mut stderr); let _ = stderr.flush();` lines stay unchanged.)

- [ ] **Step 6: Build the binary crate**

Run: `cargo build -p autotune`
Expected: compiles clean. (If `Write` becomes unused in `stream_ui.rs`, it is still used by `erase_tail`/`draw_tail`'s `&mut impl Write` params and `flush_pending`, so the `use std::io::Write;` import stays.)

- [ ] **Step 7: Run the binary crate tests + clippy**

Run: `cargo nextest run -p autotune && cargo clippy -p autotune --all-targets -- -D warnings`
Expected: PASS — existing `stream_*` unit tests still pass (they run non-TTY; `TailState` writes to real stderr exactly as the old code did, and assert only no-panic / markdown content).

- [ ] **Step 8: Commit**

```bash
git add crates/autotune/src/stream_ui.rs
git commit -m "refactor(autotune): render agent tool tail via shared TailState"
```

---

## Task 8: Documentation, full verification, follow-up issues

**Files:**
- Create: `notes/live-tail-rendering.md`
- Modify: `notes/README.md`

- [ ] **Step 1: Write the note**

Create `notes/live-tail-rendering.md`:

```markdown
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
cursor-up count is exact. Corollaries:

- Never truncate by byte index (`&line[..120]` panics on a UTF-8 boundary) — we
  truncate by `char`.
- Strip ANSI escapes from child output before truncating: truncating mid-escape
  corrupts the sequence, and a child-emitted reset would otherwise cancel our
  dim styling. (Display-width of wide chars like CJK/emoji is *not* handled —
  truncation is by char count — so a pathological all-wide-char line can still
  wrap by a bounded amount. Tracked upstream.)

## Height policy

Visible rows = `rows_for_height(height)` = `(height / 4).clamp(3, 8)`. A small
fixed `3` floods nothing but wastes a tall terminal; an unbounded fraction would
dominate. The clamp keeps the tail informative without taking over the screen.

## Non-TTY

When stderr is not a TTY, `LiveTail` is a no-op and nothing is rendered during
streaming — the full output is still collected and returned for inspection. The
dim styling additionally honors `NO_COLOR` (consistent with `style.rs`).
```

- [ ] **Step 2: Add the note to the index**

In `notes/README.md`, add this bullet to the `## Index` list (after the `scoring-and-rank.md` entry):

```markdown
- [live-tail-rendering.md](live-tail-rendering.md) — How subprocess/agent output
  is shown as a dimmed rolling tail, the physical-vs-logical-row erase footgun,
  and the terminal-height line-count policy.
```

- [ ] **Step 3: Add the note to AGENTS.md "Further reading"**

In `CLAUDE.md` (the repo's `AGENTS.md`), under the "Further reading" list in the Architecture section, add:

```markdown
- [live-tail-rendering.md](notes/live-tail-rendering.md) — Dimmed rolling-tail rendering of subprocess/agent output, the cursor-up erase footgun, and the height-based line budget.
```

- [ ] **Step 4: Run the full pre-commit checklist**

Run:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run
```

Expected: formatted, no clippy warnings, all tests pass (166 baseline; +~19 new `autotune-agent` tests).

- [ ] **Step 5: Commit**

```bash
git add notes/live-tail-rendering.md notes/README.md CLAUDE.md
git commit -m "docs(notes): document live-tail rendering and the erase footgun"
```

- [ ] **Step 6: File follow-up Ion issues (optional — needs `gh` auth)**

Per the bug-fix workflow, file the deferred work on `Roger-luo/Ion`. Skip if `gh` is unavailable; otherwise run:

```bash
gh issue create --repo Roger-luo/Ion --label enhancement \
  --title "Live tail: handle wide-character display width" \
  --body "The shared live-tail renderer (autotune_agent::terminal) truncates lines by char count, so a line of wide characters (CJK/emoji) can occupy ~2x the column budget and wrap, slightly under-counting rows in the cursor-up erase. Adopt unicode-width for display-width-correct truncation. See notes/live-tail-rendering.md."

gh issue create --repo Roger-luo/Ion --label enhancement \
  --title "Scenario coverage: live-tail never floods the screen" \
  --body "Add a PTY scenario test asserting the dimmed live tail never exceeds its row budget and is fully cleared after a command emits very wide / very many output lines. The renderer's invariants are unit-tested in autotune-agent today; an end-to-end no-leak assertion is still missing."
```

- [ ] **Step 7: Manual smoke check (recommended)**

In a real terminal, run a task whose measure command produces lots of wide output and confirm: (a) the dim tail stays bounded to ~3-8 lines, (b) lines don't wrap, (c) the tail fully disappears when the command finishes, leaving the `[autotune]` status lines intact. Resize the terminal mid-run and confirm the tail adapts without leaving debris.

---

## Self-Review notes (already reconciled)

- **Spec coverage:** `rows_for_height` (Task 1) ⇒ height policy; `sanitize_line`/`strip_ansi` (Task 2) ⇒ ANSI strip + char-safe truncation; `TailState` (Task 3) ⇒ erase/draw/redraw + width-correct erase; `stderr_size`/`LiveTail` (Task 4) ⇒ size query, TTY/`NO_COLOR` gating, threaded wrapper; Tasks 5-7 ⇒ all three call sites; Task 8 ⇒ notes + follow-up Ion issues + non-goals documented.
- **Type consistency:** `TailState::{new,push,erase,draw,redraw}`, `LiveTail::{stderr,push_line,finish}`, `stderr_size() -> (u16,u16)`, `rows_for_height(u16) -> usize`, `sanitize_line(&str, usize) -> String` are referenced identically across tasks.
- **No placeholders:** every code step shows complete code; every run step shows the command and expected outcome.
