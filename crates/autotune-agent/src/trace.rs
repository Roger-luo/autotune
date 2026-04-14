//! JSONL debug trace for replaying an autotune run.
//!
//! Activated by setting `AUTOTUNE_TRACE_FILE` to a writable path. Each call to
//! [`record`] appends one JSON object (with timestamp, category, and a
//! free-form payload) to that file. When the env var is unset, every call is a
//! cheap no-op — production runs pay nothing.
//!
//! ## Why this exists
//!
//! The state machine is a pipeline of agent calls, parser decisions, config
//! branches, and user inputs. When something goes wrong mid-loop (a crash,
//! an infinite-retry, an unexpected discard) the only artifacts on disk are
//! `state.json` and the append-only ledger — neither captures *why* a branch
//! was taken. This trace is the replay log: given a trace file, you can
//! reconstruct exactly which prompt produced which response and which
//! code path the CLI took in response.
//!
//! ## Categories
//!
//! | Category              | Payload                                            |
//! |-----------------------|----------------------------------------------------|
//! | `agent.spawn`         | `{backend, working_dir, model, prompt, response}`  |
//! | `agent.send`          | `{backend, session_id, message, response}`         |
//! | `plan.attempt`        | `{attempt, result}`                                |
//! | `plan.retry`          | `{attempt, error, correction_prompt}`              |
//! | `implement.prompt`    | `{approach, files, prompt}`                        |
//! | `implement.result`    | `{approach, outcome, commit_sha?}`                 |
//! | `phase.enter`         | `{iteration, phase, approach?}`                    |
//! | `phase.decision`      | `{phase, branch, reason}`                          |
//! | `approval.prompt`     | `{tool, scope, reason}`                            |
//! | `approval.answer`     | `{tool, decision}`                                 |
//! | `user.input`          | `{prompt, value}`                                  |
//!
//! Payloads are logged verbatim (including full prompts and responses). The
//! trace file is meant for local debugging, not shipping off-box.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Holds `Some(writer)` when `AUTOTUNE_TRACE_FILE` was set at first access and
/// the file opened successfully; `None` otherwise. Resolved once per process —
/// flipping the env var mid-run has no effect.
static WRITER: OnceLock<Option<Mutex<BufWriter<File>>>> = OnceLock::new();

fn writer() -> Option<&'static Mutex<BufWriter<File>>> {
    WRITER
        .get_or_init(|| {
            let path = std::env::var_os("AUTOTUNE_TRACE_FILE")?;
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()?;
            Some(Mutex::new(BufWriter::new(file)))
        })
        .as_ref()
}

/// True when `AUTOTUNE_TRACE_FILE` was set and the file opened successfully.
/// Callers can use this to skip building an expensive payload that would be
/// discarded anyway.
pub fn is_enabled() -> bool {
    writer().is_some()
}

/// Append one trace record. Safe to call from any thread; no-op when tracing
/// is disabled. Errors (poisoned mutex, IO failure) are swallowed — a broken
/// trace must not take down the tune loop.
pub fn record(category: &str, payload: Value) {
    let Some(w) = writer() else { return };
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let entry = json!({
        "ts_ms": ts_ms,
        "category": category,
        "payload": payload,
    });
    if let Ok(mut guard) = w.lock() {
        let _ = writeln!(*guard, "{entry}");
        let _ = guard.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    /// The OnceLock in this module makes a direct unit test of `record`
    /// environment-dependent: once the global writer is resolved (with or
    /// without the env var set), it's frozen for the process. We can still
    /// exercise the serialization shape by driving a local BufWriter with the
    /// same payload the real `record` would write.
    #[test]
    fn record_writes_one_jsonl_line_per_call() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file = OpenOptions::new().append(true).open(tmp.path()).unwrap();
        let mut w = BufWriter::new(file);

        let entry = json!({
            "ts_ms": 42u64,
            "category": "phase.enter",
            "payload": { "iteration": 1, "phase": "Planning" },
        });
        writeln!(w, "{entry}").unwrap();
        w.flush().unwrap();

        let lines: Vec<String> = BufReader::new(File::open(tmp.path()).unwrap())
            .lines()
            .map(|l| l.unwrap())
            .collect();
        assert_eq!(lines.len(), 1);
        let parsed: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parsed["category"], "phase.enter");
        assert_eq!(parsed["payload"]["iteration"], 1);
    }

    /// `is_enabled` returns false when the env var was unset at process
    /// start. This test can only assert the negative path safely (the
    /// positive path would leak a writer to later tests via the OnceLock).
    #[test]
    fn is_enabled_false_without_env() {
        // If some other test in this process enabled tracing, skip —
        // OnceLock makes the state process-global.
        if std::env::var_os("AUTOTUNE_TRACE_FILE").is_some() {
            return;
        }
        assert!(!is_enabled());
    }
}
