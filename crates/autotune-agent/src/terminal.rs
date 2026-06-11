//! Centralized terminal state restoration.
//!
//! Subprocesses (the Claude CLI) and interactive prompt libraries (dialoguer,
//! crossterm) put the terminal into modes the parent shell doesn't expect:
//! Kitty keyboard protocol, bracketed paste, raw mode, hidden cursor, mouse
//! reporting. If we don't restore these on exit, the user's shell is left
//! typing garbage like `^[[99;5u` until they `reset`.
//!
//! # The pattern
//!
//! Rust can't enforce "every terminal-mutating operation holds a guard" at
//! the type level (short of threading a witness token through every API,
//! which would need to include third-party APIs we don't control). Instead we
//! make leaks impossible in practice with three overlapping layers:
//!
//! 1. [`Guard`] — RAII guard that calls [`restore`] on drop. Hold one in scope
//!    around any terminal-mutating code. Covers normal returns, `?`-error
//!    propagation, and unwinding panics.
//! 2. [`install_panic_hook`] — global hook that runs [`restore`] on panics
//!    that aren't caught by a [`Guard`]'s unwinding Drop (e.g., if a panic
//!    escapes `main`).
//! 3. [`restore`] — the free function callers must invoke explicitly in signal
//!    handlers or before `std::process::exit`, since neither path runs Drop.
//!
//! Any code that spawns the Claude CLI or calls `dialoguer`/`crossterm` should
//! hold a [`Guard`] for the duration of that call. Any signal or early-exit
//! path should call [`restore`] before terminating.
//!
//! # Current call sites holding a [`Guard`]
//!
//! Audit list — update when adding new terminal-mutating operations:
//!
//! - `autotune_agent::claude::ClaudeAgent::run_claude` (spawning the Claude CLI)
//! - `autotune_agent::claude::ClaudeAgent::run_claude_streaming` (same, streaming variant)
//! - `autotune::stream_ui::TerminalToolApprover::approve` (dialoguer Confirm for tool approval)
//! - `autotune_init::input::TerminalInput::prompt_approve` (dialoguer Confirm for config approval)
//! - `autotune_init::select::interactive_select` (manual crossterm raw mode)
//!
//! [`install_panic_hook`] is called once in `autotune::main`.
//! [`restore`] is also invoked by the Ctrl+C handler in `autotune_init::run_init`
//! before `std::process::exit(130)`.
//!
//! # Adding a new call site
//!
//! ```ignore
//! fn my_interactive_thing() -> Result<()> {
//!     let _guard = autotune_agent::terminal::Guard::new();
//!     // ...anything that might leave terminal in a weird state...
//!     // Guard's Drop restores on every exit path from this scope.
//!     Ok(())
//! }
//! ```

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex, Once};

static HOOK_ONCE: Once = Once::new();

/// Write terminal-restore CSI sequences to stderr, if stderr is a TTY.
///
/// No-op in non-interactive contexts (piped, redirected, test runner).
pub fn restore() {
    use std::io::{IsTerminal, Write};
    let mut stderr = std::io::stderr();
    if !stderr.is_terminal() {
        return;
    }
    // CSI < u ×2         — pop kitty keyboard enhancement flags (twice in case
    //                      multiple levels were pushed).
    // CSI ? 2004 l       — disable bracketed paste.
    // CSI ? 25 h         — show cursor.
    // CSI ? 1000/1002/1003/1006 l — disable mouse reporting variants.
    // CSI 0 m            — reset SGR (colors / attributes).
    let _ = write!(
        stderr,
        "\x1b[<u\x1b[<u\x1b[?2004l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[0m"
    );
    let _ = stderr.flush();
}

/// RAII guard that calls [`restore`] on drop.
///
/// Hold one in scope for the duration of any code that may alter terminal
/// state — spawning the Claude CLI, a `dialoguer` prompt, or a crossterm
/// raw-mode block. The guard fires on every way the scope can exit: normal
/// return, `?` propagation, and unwinding panics.
///
/// ```ignore
/// let _guard = autotune_agent::terminal::Guard::new();
/// run_some_interactive_subprocess()?;
/// // terminal is restored here when _guard drops
/// ```
pub struct Guard(());

impl Guard {
    pub fn new() -> Self {
        Self(())
    }
}

impl Default for Guard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Install a panic hook that calls [`restore`] before delegating to the
/// previous hook. Idempotent: safe to call multiple times.
///
/// Call this once early in `main()`. Combined with per-operation [`Guard`]
/// instances, the terminal is restored on every exit path *except* direct
/// `std::process::exit` — which neither runs Drop nor triggers the panic
/// hook. Signal handlers should call [`restore`] explicitly before exiting.
pub fn install_panic_hook() {
    HOOK_ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            prev(info);
        }));
    });
}

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

/// Number of physical rows the live tail may occupy, scaled to the terminal
/// height: `(height / 4)` clamped to `[3, 8]`. Keeps the dim tail visible
/// without letting it dominate the screen.
pub fn rows_for_height(height: u16) -> usize {
    ((height / 4) as usize).clamp(3, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_does_not_panic_in_non_tty_context() {
        // In the test runner stderr is not a TTY, so restore() should be a
        // no-op and must not panic.
        restore();
    }

    #[test]
    fn guard_new_drops_without_panic() {
        let _guard = Guard::new();
        // Drop happens here; must not panic even when stderr is not a TTY.
    }

    #[test]
    fn guard_default_works() {
        let _guard = Guard::default();
    }

    #[test]
    fn guard_drop_calls_restore() {
        // Create and immediately drop; no panic expected.
        drop(Guard::new());
    }

    #[test]
    fn install_panic_hook_is_idempotent() {
        // Calling twice must not panic or cause undefined behavior.
        install_panic_hook();
        install_panic_hook();
    }

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
}
