//! Tracking and teardown of agent subprocesses (`claude` / `codex`).
//!
//! Agent CLIs are long-lived and spawn their own descendants (node, MCP
//! servers, tool subprocesses). If autotune is interrupted (Ctrl-C) or
//! terminated (SIGTERM) while an agent call is in flight, those subprocesses
//! would otherwise be reparented to init and keep running — orphaned, still
//! consuming the LLM budget.
//!
//! To prevent that, each agent subprocess is spawned as its **own process
//! group leader** (`CommandExt::process_group(0)`), and its pid (== pgid) is
//! registered here for the duration of the call. On shutdown the CLI calls
//! [`terminate_active_children`], which signals each registered group — taking
//! down the agent CLI *and* all of its descendants in one shot, without
//! touching autotune's own process group.
//!
//! SIGINT is used (not SIGKILL) so the agent exits the same way a Ctrl-C in an
//! interactive terminal would: `claude` reports a signal-2 exit, which
//! [`crate::AgentError::Interrupted`] maps to a clean shutdown.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

fn active() -> &'static Mutex<HashSet<u32>> {
    static ACTIVE: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Set once [`terminate_active_children`] runs. Sticky: a shutdown is
/// irreversible for the life of the process.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// True once shutdown teardown has begun. Agent backends consult this after a
/// subprocess exits: if we tore the child down, its exit (however it happened —
/// a clean exit-0, a signal, a half-written stream) must be reported as
/// [`crate::AgentError::Interrupted`], not parsed as a real (and likely empty)
/// response. That makes the planning/fix loops stop retrying and lets the run
/// exit cleanly instead of re-spawning agents after a shutdown request.
pub fn is_shutting_down() -> bool {
    SHUTTING_DOWN.load(Ordering::SeqCst)
}

/// Record a spawned agent subprocess (its pid, which is also its pgid because
/// it is spawned as a process-group leader). Prefer [`ChildGuard`] over calling
/// this directly so the pid is always removed when the call returns.
pub fn register_child(pid: u32) {
    if let Ok(mut set) = active().lock() {
        set.insert(pid);
    }
}

/// Stop tracking a subprocess that has exited / been waited on.
pub fn unregister_child(pid: u32) {
    if let Ok(mut set) = active().lock() {
        set.remove(&pid);
    }
}

/// Signal every tracked agent subprocess group so the CLIs and their
/// descendants exit. Best-effort: errors (a child that already exited) are
/// ignored. No-op on non-Unix platforms.
pub fn terminate_active_children() {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;

        let pids: Vec<u32> = match active().lock() {
            Ok(set) => set.iter().copied().collect(),
            Err(_) => return,
        };
        for pid in pids {
            // Children are their own process-group leaders, so pgid == pid.
            // SIGINT mirrors an interactive Ctrl-C, which the agents handle as
            // a clean interruption.
            let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGINT);
        }
    }
}

/// RAII guard: registers a child pid on construction and unregisters it on
/// drop, so a panic or early return during an agent call can't leak a tracked
/// pid.
pub struct ChildGuard(u32);

impl ChildGuard {
    pub fn new(pid: u32) -> Self {
        register_child(pid);
        ChildGuard(pid)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        unregister_child(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_unregister_roundtrip() {
        // Use a pid value unlikely to collide with a real process; we never
        // signal it here.
        let pid = 999_999_001;
        register_child(pid);
        assert!(active().lock().unwrap().contains(&pid));
        unregister_child(pid);
        assert!(!active().lock().unwrap().contains(&pid));
    }

    #[test]
    fn guard_unregisters_on_drop() {
        let pid = 999_999_002;
        {
            let _g = ChildGuard::new(pid);
            assert!(active().lock().unwrap().contains(&pid));
        }
        assert!(!active().lock().unwrap().contains(&pid));
    }

    #[test]
    fn terminate_with_no_children_is_noop() {
        // Must not panic when nothing is registered.
        terminate_active_children();
    }

    /// End-to-end: a real subprocess spawned as its own group leader is killed
    /// by `terminate_active_children`.
    #[cfg(unix)]
    #[test]
    fn terminate_kills_registered_process_group() {
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        cmd.process_group(0);
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id();
        register_child(pid);

        terminate_active_children();

        let status = child.wait().expect("wait sleep");
        unregister_child(pid);
        assert!(
            !status.success(),
            "sleep should have been terminated by the signal"
        );
    }
}
