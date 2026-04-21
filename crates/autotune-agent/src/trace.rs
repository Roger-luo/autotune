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
//! | `init.start`          | `{config_exists, repo_root}`                       |
//! | `init.user_input`     | `{prompt, value}`                                  |
//! | `init.fragment`       | `{kind, outcome, name?, error?}`                   |
//! | `init.approval`       | `{approved}`                                       |
//! | `init.validation`     | `{outcome, metrics? | error?}`                     |
//!
//! Payloads are logged verbatim (including full prompts and responses). The
//! trace file is meant for local debugging, not shipping off-box.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Holds `Some(writer)` when tracing is enabled, `None` otherwise.
/// Set explicitly by `init()` or lazily by the first `record()` call.
static WRITER: OnceLock<Option<Mutex<BufWriter<File>>>> = OnceLock::new();

/// Error returned when `init()` cannot set up the trace file.
#[derive(Debug, thiserror::Error)]
pub enum TraceInitError {
    #[error(
        "trace file already exists: {path:?} — delete it or choose a different path"
    )]
    AlreadyExists { path: OsString },
    #[error("failed to create trace file {path:?}: {source}")]
    Create {
        path: OsString,
        #[source]
        source: io::Error,
    },
    #[error("trace already initialized before init() was called")]
    AlreadyInitialized,
}

/// Explicitly initialize tracing from `AUTOTUNE_TRACE_FILE`.
///
/// Must be called once, before any `record()` call. Returns an error if:
/// - the file already exists (`AlreadyExists`) — delete it or use a new path,
/// - the path is unwritable / the parent directory is missing (`Create`),
/// - `init()` was called after `record()` already lazily initialized the
///   writer (`AlreadyInitialized`).
///
/// When `AUTOTUNE_TRACE_FILE` is not set, `init()` is a no-op and returns `Ok`.
pub fn init() -> Result<(), TraceInitError> {
    let Some(path) = std::env::var_os("AUTOTUNE_TRACE_FILE") else {
        let _ = WRITER.set(None);
        return Ok(());
    };
    open_trace_file(&path)
}

/// Create the trace file at `path` and install it as the global writer.
/// Errors if the file already exists or the path is unwritable.
fn open_trace_file(path: &std::ffi::OsStr) -> Result<(), TraceInitError> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                TraceInitError::AlreadyExists { path: path.to_owned() }
            } else {
                TraceInitError::Create { path: path.to_owned(), source: e }
            }
        })?;

    WRITER
        .set(Some(Mutex::new(BufWriter::new(file))))
        .map_err(|_| TraceInitError::AlreadyInitialized)
}

fn writer() -> Option<&'static Mutex<BufWriter<File>>> {
    WRITER.get().and_then(|opt| opt.as_ref())
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

    // --- open_trace_file() / init() tests ---
    //
    // nextest runs each test in its own process, so OnceLock state does not
    // bleed between these tests even though WRITER is a static.
    // We call the private `open_trace_file` directly to avoid env-var mutation
    // (unsafe in Rust 2024).

    /// Opening a fresh path creates the file and enables record().
    #[test]
    fn open_trace_file_creates_file_and_enables_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");

        open_trace_file(path.as_os_str()).expect("should succeed for a new path");

        assert!(is_enabled(), "tracing should be enabled");
        assert!(path.exists(), "trace file should be created");

        record("test.event", json!({"key": "value"}));

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["category"], "test.event");
        assert_eq!(parsed["payload"]["key"], "value");
    }

    /// Opening a path where a file already exists returns AlreadyExists.
    #[test]
    fn open_trace_file_errors_if_file_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.jsonl");
        std::fs::write(&path, b"old content\n").unwrap();

        let err = open_trace_file(path.as_os_str())
            .expect_err("should fail when file already exists");

        assert!(
            matches!(err, TraceInitError::AlreadyExists { .. }),
            "expected AlreadyExists, got: {err}"
        );
        // Existing content must not be touched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old content\n");
    }

    /// Opening a path whose parent directory does not exist returns Create.
    #[test]
    fn open_trace_file_errors_if_directory_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent").join("trace.jsonl");

        let err = open_trace_file(path.as_os_str())
            .expect_err("should fail for missing parent dir");

        assert!(
            matches!(err, TraceInitError::Create { .. }),
            "expected Create, got: {err}"
        );
    }
}
