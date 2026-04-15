use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

pub type Metrics = HashMap<String, f64>;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("task not found: {name}")]
    NotFound { name: String },

    #[error("invalid phase transition: {from} → {to}")]
    InvalidTransition { from: String, to: String },

    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("JSON error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Planning,
    Implementing,
    Testing,
    /// The implementer is being re-invoked with the failing test output to
    /// attempt a repair. Entered from `Testing` when tests fail and the
    /// iteration's fix budget is not exhausted. Transitions back to
    /// `Testing` after a successful edit, or to discard if the budget runs
    /// out or the implementer produces no edits on a fresh respawn.
    Fixing,
    Measuring,
    Scoring,
    Integrating,
    Recorded,
    Done,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Phase::Planning => write!(f, "Planning"),
            Phase::Implementing => write!(f, "Implementing"),
            Phase::Testing => write!(f, "Testing"),
            Phase::Fixing => write!(f, "Fixing"),
            Phase::Measuring => write!(f, "Measuring"),
            Phase::Scoring => write!(f, "Scoring"),
            Phase::Integrating => write!(f, "Integrating"),
            Phase::Recorded => write!(f, "Recorded"),
            Phase::Done => write!(f, "Done"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskState {
    pub task_name: String,
    pub canonical_branch: String,
    /// The branch where kept iterations accumulate (e.g.
    /// `autotune/<task>-main`). Created from `canonical_branch` at task
    /// start so the user can PR it. The `-main` suffix keeps this branch
    /// off the `autotune/<task>/<slug>` worktree prefix git would otherwise
    /// refuse to occupy.
    #[serde(default)]
    pub advancing_branch: String,
    pub research_session_id: String,
    pub current_iteration: usize,
    pub current_phase: Phase,
    pub current_approach: Option<ApproachState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApproachState {
    pub name: String,
    pub hypothesis: String,
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub commit_sha: Option<String>,
    pub test_results: Vec<TestResult>,
    pub metrics: Option<Metrics>,
    pub rank: Option<f64>,
    /// Files the research agent proposed for the implementation agent to
    /// modify. Persisted on the approach so a crash between Planning and
    /// Implementing doesn't lose the file list. `#[serde(default)]` keeps
    /// older state files loadable.
    #[serde(default)]
    pub files_to_modify: Vec<String>,
    /// Session id of the implementer the CLI is currently conversing with
    /// for fix-retry. `None` either because we haven't spawned yet or
    /// because the previous session went unproductive and we're about to
    /// fall through to a fresh respawn.
    #[serde(default)]
    pub impl_session_id: Option<String>,
    /// Total fix attempts consumed so far for this iteration (both
    /// session-continuation and fresh-respawn paths). Checked against
    /// `agent.implementation.max_fix_attempts`.
    #[serde(default)]
    pub fix_attempts: u32,
    /// Number of fresh implementer respawns used for this iteration.
    /// Checked against `agent.implementation.max_fresh_spawns`.
    #[serde(default)]
    pub fresh_spawns: u32,
    /// Concatenated test failure history fed back to the implementer on
    /// the next fix turn. Appended each time tests fail so a fresh respawn
    /// sees the full trail, not just the latest.
    #[serde(default)]
    pub fix_history: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub duration_secs: f64,
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IterationRecord {
    pub iteration: usize,
    pub approach: String,
    pub status: IterationStatus,
    #[serde(default)]
    pub hypothesis: Option<String>,
    pub metrics: Metrics,
    pub rank: f64,
    #[serde(default)]
    pub score: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Fix-retry bookkeeping copied off `ApproachState` when the iteration
    /// is recorded, so the ledger carries enough history for the planner
    /// (and humans reading reports) to see when an approach needed repair.
    #[serde(default)]
    pub fix_attempts: u32,
    #[serde(default)]
    pub fresh_spawns: u32,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IterationStatus {
    Baseline,
    Kept,
    Discarded,
    Crash,
}

#[derive(Debug, Clone)]
pub struct TaskStore {
    root: PathBuf,
}

impl TaskStore {
    pub fn new(task_dir: &Path) -> Result<Self, StateError> {
        create_dir_all_and_sync_parent(task_dir)?;
        create_dir_all_and_sync_parent(&task_dir.join("iterations"))?;
        Ok(Self {
            root: task_dir.to_path_buf(),
        })
    }

    pub fn open(task_dir: &Path) -> Result<Self, StateError> {
        if !task_dir.exists() {
            return Err(StateError::NotFound {
                name: task_dir.display().to_string(),
            });
        }
        Ok(Self {
            root: task_dir.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn save_state(&self, state: &TaskState) -> Result<(), StateError> {
        atomic_write(&self.state_path(), &serde_json::to_string_pretty(state)?)
    }

    pub fn load_state(&self) -> Result<TaskState, StateError> {
        let content = fs::read_to_string(self.state_path())?;
        Ok(serde_json::from_str(&content)?)
    }

    fn ledger_path(&self) -> PathBuf {
        self.root.join("ledger.json")
    }

    pub fn load_ledger(&self) -> Result<Vec<IterationRecord>, StateError> {
        let path = self.ledger_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn append_ledger(&self, record: &IterationRecord) -> Result<(), StateError> {
        let mut ledger = self.load_ledger()?;
        ledger.push(record.clone());
        atomic_write(&self.ledger_path(), &serde_json::to_string_pretty(&ledger)?)
    }

    pub fn save_config_snapshot(&self, content: &str) -> Result<(), StateError> {
        atomic_write(&self.root.join("config_snapshot.toml"), content)
    }

    pub fn load_config_snapshot(&self) -> Result<String, StateError> {
        Ok(fs::read_to_string(self.root.join("config_snapshot.toml"))?)
    }

    fn log_path(&self) -> PathBuf {
        self.root.join("log.md")
    }

    pub fn read_log(&self) -> Result<String, StateError> {
        let path = self.log_path();
        if !path.exists() {
            return Ok(String::new());
        }
        Ok(fs::read_to_string(path)?)
    }

    pub fn append_log(&self, entry: &str) -> Result<(), StateError> {
        let mut content = self.read_log()?;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(entry);
        content.push('\n');
        atomic_write(&self.log_path(), &content)
    }

    pub fn iteration_dir(&self, iteration: usize, approach: &str) -> PathBuf {
        self.root
            .join("iterations")
            .join(format!("{:03}-{}", iteration, approach))
    }

    pub fn save_iteration_metrics(
        &self,
        iteration: usize,
        approach: &str,
        metrics: &Metrics,
    ) -> Result<(), StateError> {
        let dir = self.iteration_dir(iteration, approach);
        create_dir_all_and_sync_parent(&dir)?;
        atomic_write(
            &dir.join("metrics.json"),
            &serde_json::to_string_pretty(metrics)?,
        )
    }

    pub fn save_iteration_prompt(
        &self,
        iteration: usize,
        approach: &str,
        prompt: &str,
    ) -> Result<(), StateError> {
        let dir = self.iteration_dir(iteration, approach);
        create_dir_all_and_sync_parent(&dir)?;
        atomic_write(&dir.join("prompt.md"), prompt)
    }

    /// Directory that holds raw per-measure stdout/stderr captures for an
    /// iteration. Paths returned here are intended to be referenced from the
    /// research agent's planning prompt as on-demand lookups.
    pub fn measure_output_dir(&self, iteration: usize, approach: &str) -> PathBuf {
        self.iteration_dir(iteration, approach)
            .join("measure_output")
    }

    /// Save the raw stdout and/or stderr of a single measure. Empty streams
    /// are skipped (no file written) so callers can cheaply advertise only
    /// the paths that actually have content. Returns the list of files
    /// written, in (stream-name, path) pairs.
    pub fn save_measure_output(
        &self,
        iteration: usize,
        approach: &str,
        measure_name: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<Vec<(&'static str, PathBuf)>, StateError> {
        let mut written = Vec::new();
        if stdout.is_empty() && stderr.is_empty() {
            return Ok(written);
        }
        let dir = self.measure_output_dir(iteration, approach);
        create_dir_all_and_sync_parent(&dir)?;
        if !stdout.is_empty() {
            let path = dir.join(format!("{}.stdout.txt", measure_name));
            atomic_write(&path, stdout)?;
            written.push(("stdout", path));
        }
        if !stderr.is_empty() {
            let path = dir.join(format!("{}.stderr.txt", measure_name));
            atomic_write(&path, stderr)?;
            written.push(("stderr", path));
        }
        Ok(written)
    }

    pub fn save_test_output(
        &self,
        iteration: usize,
        approach: &str,
        output: &str,
    ) -> Result<(), StateError> {
        let dir = self.iteration_dir(iteration, approach);
        create_dir_all_and_sync_parent(&dir)?;
        atomic_write(&dir.join("test_output.txt"), output)
    }

    pub fn list_tasks(autotune_dir: &Path) -> Result<Vec<String>, StateError> {
        let tasks_dir = autotune_dir.join("tasks");
        if !tasks_dir.exists() {
            return Ok(Vec::new());
        }

        let mut names = Vec::new();
        for entry in fs::read_dir(tasks_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }
}

fn atomic_write(path: &Path, content: &str) -> Result<(), StateError> {
    let dir = parent_directory(path);
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(content.as_bytes())?;
    tmp.as_file_mut().sync_all()?;
    sync_directory(dir)?;
    tmp.persist(path).map_err(|error| StateError::Io {
        source: error.error,
    })?;
    sync_directory(dir)?;
    Ok(())
}

fn create_dir_all_and_sync_parent(path: &Path) -> Result<(), StateError> {
    if path.exists() {
        fs::create_dir_all(path)?;
        return Ok(());
    }

    let mut missing = Vec::new();
    let mut current = path;

    while !current.exists() {
        missing.push(current.to_path_buf());
        current = parent_directory(current);
    }

    missing.reverse();

    for dir in missing {
        fs::create_dir(&dir)?;
        sync_directory(parent_directory(&dir))?;
    }

    Ok(())
}

fn parent_directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn sync_directory(path: &Path) -> Result<(), StateError> {
    let dir = fs::File::open(path)?;
    dir.sync_all()?;
    record_synced_directory(path);
    Ok(())
}

#[cfg(test)]
fn record_synced_directory(path: &Path) {
    let synced = SYNCED_DIRECTORIES.get_or_init(|| Mutex::new(Vec::new()));
    synced.lock().unwrap().push(path.to_path_buf());
}

#[cfg(not(test))]
fn record_synced_directory(_path: &Path) {}

#[cfg(test)]
static SYNCED_DIRECTORIES: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

#[cfg(test)]
fn take_synced_directories() -> Vec<PathBuf> {
    let synced = SYNCED_DIRECTORIES.get_or_init(|| Mutex::new(Vec::new()));
    std::mem::take(&mut *synced.lock().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_dir_all_and_sync_parent_syncs_each_new_component_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("tasks").join("demo").join("iterations");

        create_dir_all_and_sync_parent(&nested).unwrap();

        assert_eq!(
            take_synced_directories(),
            vec![
                temp.path().to_path_buf(),
                temp.path().join("tasks"),
                temp.path().join("tasks").join("demo"),
            ]
        );
    }

    #[test]
    fn phase_fixing_display_matches_variant() {
        assert_eq!(Phase::Fixing.to_string(), "Fixing");
    }

    /// Old state.json files written before the Fixing phase existed must
    /// still load — the new fields on `ApproachState` and `IterationRecord`
    /// rely on `#[serde(default)]`. This pins that guarantee.
    #[test]
    fn legacy_approach_state_deserializes_with_defaults() {
        let legacy_json = r#"{
            "name": "old",
            "hypothesis": "h",
            "worktree_path": "/tmp/wt",
            "branch_name": "b",
            "commit_sha": null,
            "test_results": [],
            "metrics": null,
            "rank": null
        }"#;
        let approach: ApproachState = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(approach.files_to_modify, Vec::<String>::new());
        assert_eq!(approach.impl_session_id, None);
        assert_eq!(approach.fix_attempts, 0);
        assert_eq!(approach.fresh_spawns, 0);
        assert_eq!(approach.fix_history, Vec::<String>::new());
    }

    #[test]
    fn legacy_iteration_record_deserializes_with_defaults() {
        let legacy_json = r#"{
            "iteration": 1,
            "approach": "a",
            "status": "kept",
            "metrics": {},
            "rank": 0.0,
            "timestamp": "2026-04-15T00:00:00Z"
        }"#;
        let record: IterationRecord = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(record.fix_attempts, 0);
        assert_eq!(record.fresh_spawns, 0);
    }
}
