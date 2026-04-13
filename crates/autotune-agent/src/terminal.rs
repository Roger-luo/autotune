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

use std::sync::Once;

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
