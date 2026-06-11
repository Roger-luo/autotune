//! Styling for Autotune's own user-facing messages.
//!
//! The CLI interleaves two very different kinds of text in the terminal:
//!
//! - **What the user types** — free-form input at a `> ` prompt, plus the
//!   agent's streamed prose (rendered as markdown). These keep the terminal's
//!   default foreground color.
//! - **Messages Autotune emits about itself** — the `[autotune]`-tagged status
//!   lines (planning, testing, scoring, integration, warnings, …). These are
//!   rendered in an accent color (orange) so they're instantly distinguishable
//!   from the user's own input.
//!
//! Use the [`aprintln!`](crate::aprintln) / [`aeprintln!`](crate::aeprintln)
//! macros instead of `println!` / `eprintln!` for any `[autotune]`-tagged line.
//! They behave exactly like the std macros but wrap the whole rendered line in
//! the accent color when the target stream is a TTY (and `NO_COLOR` is unset).
//!
//! Color is **gated on the destination being a terminal**, so piped/redirected
//! output and the test runner see plain text — exact string assertions and
//! machine-readable pipes are unaffected.

use std::io::IsTerminal;

/// ANSI SGR for Autotune's accent color: 256-color 208 (orange).
const ACCENT: &str = "\x1b[38;5;208m";
/// ANSI SGR reset.
const RESET: &str = "\x1b[0m";

/// The single color-gating policy: emit accent color only when the destination
/// is a terminal AND the `NO_COLOR` convention (<https://no-color.org/>) hasn't
/// opted out. `stdout_color`/`stderr_color` both route through here so the rule
/// lives in exactly one place.
fn color_enabled(is_tty: bool) -> bool {
    is_tty && std::env::var_os("NO_COLOR").is_none()
}

/// Whether accent color should be emitted on stdout right now.
#[doc(hidden)]
pub fn stdout_color() -> bool {
    color_enabled(std::io::stdout().is_terminal())
}

/// Whether accent color should be emitted on stderr right now.
#[doc(hidden)]
pub fn stderr_color() -> bool {
    color_enabled(std::io::stderr().is_terminal())
}

/// Wrap `msg` in the accent color when `enabled`, otherwise return it verbatim.
///
/// Kept as a pure function of an explicit `enabled` flag so the rendering is
/// unit-testable without depending on whether the test runner is attached to a
/// TTY. The macros supply the flag from [`stdout_color`] / [`stderr_color`].
#[doc(hidden)]
pub fn accent(msg: &str, enabled: bool) -> String {
    if enabled {
        format!("{ACCENT}{msg}{RESET}")
    } else {
        msg.to_string()
    }
}

/// Like [`println!`], but renders the line in Autotune's accent color when
/// stdout is a TTY. Use for `[autotune]`-tagged status lines on stdout.
#[macro_export]
macro_rules! aprintln {
    ($($arg:tt)*) => {
        println!(
            "{}",
            $crate::style::accent(&format!($($arg)*), $crate::style::stdout_color())
        )
    };
}

/// Like [`eprintln!`], but renders the line in Autotune's accent color when
/// stderr is a TTY. Use for `[autotune]`-tagged status lines on stderr.
#[macro_export]
macro_rules! aeprintln {
    ($($arg:tt)*) => {
        eprintln!(
            "{}",
            $crate::style::accent(&format!($($arg)*), $crate::style::stderr_color())
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_wraps_when_enabled() {
        let out = accent("[autotune] hello", true);
        assert_eq!(out, "\x1b[38;5;208m[autotune] hello\x1b[0m");
        assert!(out.starts_with(ACCENT));
        assert!(out.ends_with(RESET));
        assert!(out.contains("[autotune] hello"));
    }

    #[test]
    fn accent_is_verbatim_when_disabled() {
        // The exact-string assertions throughout the suite rely on this: when
        // color is off (non-TTY / NO_COLOR), the message is byte-for-byte the
        // original with no escape sequences.
        let msg = "[autotune] hello";
        assert_eq!(accent(msg, false), msg);
        assert!(!accent(msg, false).contains('\x1b'));
    }

    #[test]
    fn accent_preserves_leading_newline() {
        let out = accent("\n[autotune] done", true);
        assert!(out.contains("\n[autotune] done"));
    }
}
