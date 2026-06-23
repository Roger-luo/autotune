use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[cfg(test)]
use std::cell::RefCell;

pub type Metrics = HashMap<String, f64>;

fn default_backend() -> String {
    "claude".to_string()
}

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
    #[serde(default = "default_backend")]
    pub research_backend: String,
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
    /// Backend used for the implementation agent session. `None` keeps
    /// older state files loadable and falls back to the configured default.
    #[serde(default)]
    pub impl_backend: Option<String>,
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
    /// Scorer-provided reason persisted between Scoring and Integrating so
    /// kept ledger rows can retain the explanation that produced the rank.
    #[serde(default)]
    pub score_reason: Option<String>,
    /// Structured per-metric score breakdown produced at the Scoring phase,
    /// carried through to Integrating/recording so the kept (or discarded)
    /// ledger row can persist it. `#[serde(default)]` keeps older state files
    /// loadable.
    #[serde(default)]
    pub score_breakdown: Option<ScoreBreakdown>,
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
    /// The commit this row corresponds to on the advancing branch:
    /// - `Kept` rows: the post-integration advancing-branch HEAD.
    /// - `Reverted` rows: the `git revert` (inverse) commit.
    ///
    /// `None` on rows written before SHA tracking existed (and on
    /// discarded/crash rows, which were never integrated).
    #[serde(default)]
    pub commit_sha: Option<String>,
    /// Set only on `Reverted` rows: the iteration number this revert undid.
    #[serde(default)]
    pub reverted_iteration: Option<usize>,
    /// Structured, per-metric breakdown of how this iteration was scored
    /// (baseline/candidate/best values, deltas, direction, and — for the
    /// weighted-sum scorer — weight + weighted contribution), plus the overall
    /// keep/discard decision. Populated at the Scoring phase. `None` on rows
    /// written before this feature existed, on baseline rows (nothing to score
    /// against), and on crash rows (no metrics were ever taken).
    #[serde(default)]
    pub score_breakdown: Option<ScoreBreakdown>,
    /// Files this iteration's commit changed relative to its parent on the
    /// advancing branch (`git diff --name-only <sha>^..<sha>`). Lets a
    /// downstream analyzer judge whether a metric delta is causally plausible
    /// (e.g. flag a speedup attributed to a diff that never touched the hot
    /// path). Populated for `Kept` rows at integration time. `None` on rows
    /// written before this feature, and on rows that never produced a commit.
    #[serde(default)]
    pub changed_files: Option<Vec<String>>,
    pub timestamp: DateTime<Utc>,
}

/// Structured score breakdown for one iteration: the overall decision plus a
/// per-metric breakdown. Persisted on `IterationRecord` so the analysis
/// artifact can be assembled from the ledger alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreBreakdown {
    /// Overall keep/discard decision the scorer returned for this iteration.
    pub decision: String,
    /// One entry per primary/scored metric. Order follows the scorer's metric
    /// definition order.
    pub metrics: Vec<MetricBreakdown>,
}

/// Per-metric breakdown for the analysis artifact: the metric's value across
/// baseline/candidate/best, the deltas vs each, the optimization direction,
/// and (when the scorer is weighted-sum) the weight and weighted contribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricBreakdown {
    /// Metric name.
    pub name: String,
    /// Value at task baseline (the synthetic baseline ledger row).
    pub baseline: Option<f64>,
    /// This iteration's measured value.
    pub candidate: Option<f64>,
    /// Value of the current best (the metrics scoring used as `best`).
    pub best: Option<f64>,
    /// `candidate - baseline` (raw difference; not direction-normalized).
    pub delta_vs_baseline: Option<f64>,
    /// `candidate - best` (raw difference; not direction-normalized).
    pub delta_vs_best: Option<f64>,
    /// Optimization direction: `"higher"` if higher is better, `"lower"` if
    /// lower is better. `None` when the scorer doesn't declare a direction
    /// (e.g. a script scorer).
    pub direction: Option<String>,
    /// Weight applied to this metric in the weighted sum, if the scorer is
    /// weighted-sum. `None` for threshold/script scorers.
    pub weight: Option<f64>,
    /// Direction-normalized improvement vs `best`, as a fraction, if available
    /// from the scorer (weighted-sum). This is what feeds the rank.
    pub improvement_vs_best: Option<f64>,
    /// `weight * improvement_vs_best` — this metric's contribution to the rank,
    /// if available from the scorer (weighted-sum).
    pub contribution: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IterationStatus {
    Baseline,
    Kept,
    Discarded,
    Crash,
    Reverted,
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
            .join(format!("{:03}-{}", iteration, slug_component(approach)))
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

/// Collapse a free-form approach name into a single filesystem-safe path
/// component for iteration directories. An approach name is research-agent
/// prose — it can carry spaces, slashes, newlines, em-dashes, backticks —
/// and a raw `/` would split the iteration dir into accidental nested
/// subdirectories (orphaning `metrics.json` under a child dir).
///
/// The algorithm mirrors `autotune_implement::slugify` so an iteration's
/// on-disk `NNN-<slug>` directory correlates with its `autotune/<task>/<slug>`
/// worktree branch. Keep the two in sync if either changes. Empty results
/// (an approach with no alphanumerics) fall back to `approach` so the dir
/// never degenerates to a bare `NNN-`.
fn slug_component(name: &str) -> String {
    let mapped: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut out = String::new();
    for ch in mapped.chars() {
        if ch == '-' && out.ends_with('-') {
            continue;
        }
        out.push(ch);
    }
    let out = out.trim_matches('-');
    let out = if out.len() > 60 {
        out[..60].trim_end_matches('-')
    } else {
        out
    };
    if out.is_empty() {
        "approach".to_string()
    } else {
        out.to_string()
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
    SYNCED_DIRECTORIES.with(|synced| synced.borrow_mut().push(path.to_path_buf()));
}

#[cfg(not(test))]
fn record_synced_directory(_path: &Path) {}

#[cfg(test)]
thread_local! {
    static SYNCED_DIRECTORIES: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn take_synced_directories() -> Vec<PathBuf> {
    SYNCED_DIRECTORIES.with(|synced| std::mem::take(&mut *synced.borrow_mut()))
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

    #[test]
    fn task_store_open_returns_not_found_error() {
        let result = TaskStore::open(std::path::Path::new(
            "/nonexistent/path/that/does/not/exist",
        ));
        assert!(matches!(result, Err(StateError::NotFound { .. })));
    }

    #[test]
    fn task_store_open_succeeds_for_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        TaskStore::new(tmp.path()).unwrap();
        let result = TaskStore::open(tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn task_store_root_returns_path() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        assert_eq!(store.root(), tmp.path());
    }

    #[test]
    fn save_and_load_config_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        store.save_config_snapshot("config content").unwrap();
        let loaded = store.load_config_snapshot().unwrap();
        assert_eq!(loaded, "config content");
    }

    #[test]
    fn save_iteration_prompt_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        store.save_iteration_prompt(1, "opt", "my prompt").unwrap();
        let prompt_path = store.iteration_dir(1, "opt").join("prompt.md");
        let content = fs::read_to_string(prompt_path).unwrap();
        assert_eq!(content, "my prompt");
    }

    /// A research-agent approach name is free-form prose: it can contain
    /// spaces, slashes, newlines, em-dashes, parentheses. Used raw, a `/`
    /// silently splits the iteration directory into nested subdirectories
    /// (so `metrics.json` lands under an accidental child dir) and a newline
    /// produces an un-listable name. `iteration_dir` must collapse the
    /// approach into a single filesystem-safe component.
    #[test]
    fn iteration_dir_sanitizes_approach_with_slashes_and_newlines() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let messy = "Speed up Foo::bar() — the cost in rx/rzz\n(map_insert) path";
        let dir = store.iteration_dir(1, messy);

        // Exactly one component below `iterations/` — no nesting from `/`.
        let rel = dir.strip_prefix(tmp.path().join("iterations")).unwrap();
        assert_eq!(
            rel.components().count(),
            1,
            "approach with '/' must not create nested dirs, got {rel:?}"
        );

        let name = dir.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("001-"), "got {name}");
        assert!(!name.contains('/'), "slug leaked a slash: {name}");
        assert!(!name.contains('\n'), "slug leaked a newline: {name}");
        assert!(!name.contains(' '), "slug leaked a space: {name}");
    }

    /// The iteration-dir slug should match the branch slug
    /// (`autotune-implement::slugify`) so artifacts on disk correlate with
    /// the worktree branch name for the same approach.
    #[test]
    fn iteration_dir_slug_matches_branch_slug_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let dir = store.iteration_dir(2, "Specialize `rzz` on PauliSum");
        assert_eq!(dir.file_name().unwrap(), "002-specialize-rzz-on-paulisum");
    }

    /// The baseline approach name is already clean and must be preserved
    /// verbatim — downstream code and tests reference `000-baseline`.
    #[test]
    fn iteration_dir_preserves_clean_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let dir = store.iteration_dir(0, "baseline");
        assert_eq!(dir.file_name().unwrap(), "000-baseline");
    }

    /// Metrics for a messy approach name must round-trip back from the same
    /// `iteration_dir` — i.e. the write and the read agree on the sanitized
    /// path, and the file is a direct child (not nested under a `/` split).
    #[test]
    fn save_iteration_metrics_roundtrips_for_messy_approach() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let messy = "Drop the contains_with probe in map_insert/merge";
        let mut metrics = Metrics::new();
        metrics.insert("x".to_string(), 1.0);
        store.save_iteration_metrics(3, messy, &metrics).unwrap();
        let path = store.iteration_dir(3, messy).join("metrics.json");
        assert!(path.exists(), "metrics.json not at {path:?}");
    }

    #[test]
    fn save_measure_output_writes_both_streams() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let written = store
            .save_measure_output(1, "opt", "bench", "stdout content", "stderr content")
            .unwrap();
        assert_eq!(written.len(), 2);
        for (_, path) in &written {
            assert!(path.exists());
        }
    }

    #[test]
    fn save_measure_output_only_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let written = store
            .save_measure_output(1, "opt", "bench", "stdout content", "")
            .unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, "stdout");
    }

    #[test]
    fn save_measure_output_only_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let written = store
            .save_measure_output(1, "opt", "bench", "", "stderr content")
            .unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, "stderr");
    }

    #[test]
    fn save_measure_output_both_empty_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        let written = store
            .save_measure_output(1, "opt", "bench", "", "")
            .unwrap();
        assert_eq!(written.len(), 0);
        assert!(!store.measure_output_dir(1, "opt").exists());
    }

    #[test]
    fn append_log_creates_and_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskStore::new(tmp.path()).unwrap();
        store.append_log("first entry").unwrap();
        store.append_log("second entry").unwrap();
        let log = store.read_log().unwrap();
        assert!(log.contains("first entry"));
        assert!(log.contains("second entry"));
    }

    #[test]
    fn list_tasks_empty_when_no_tasks_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let names = TaskStore::list_tasks(tmp.path()).unwrap();
        assert_eq!(names, Vec::<String>::new());
    }

    #[test]
    fn list_tasks_returns_sorted_task_names() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join(".autotune").join("tasks");
        fs::create_dir_all(tasks_dir.join("beta-task")).unwrap();
        fs::create_dir_all(tasks_dir.join("alpha-task")).unwrap();
        let names = TaskStore::list_tasks(&tmp.path().join(".autotune")).unwrap();
        assert_eq!(
            names,
            vec!["alpha-task".to_string(), "beta-task".to_string()]
        );
    }

    /// New revert fields default to None so ledgers written before this feature
    /// still deserialize. Pins backward compatibility.
    #[test]
    fn legacy_iteration_record_without_revert_fields_deserializes() {
        let legacy_json = r#"{
            "iteration": 1,
            "approach": "a",
            "status": "kept",
            "metrics": {},
            "rank": 0.0,
            "timestamp": "2026-04-15T00:00:00Z"
        }"#;
        let record: IterationRecord = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(record.commit_sha, None);
        assert_eq!(record.reverted_iteration, None);
    }

    /// The new `Reverted` status round-trips through serde in snake_case.
    #[test]
    fn reverted_status_roundtrips_snake_case() {
        let json = serde_json::to_string(&IterationStatus::Reverted).unwrap();
        assert_eq!(json, "\"reverted\"");
        let back: IterationStatus = serde_json::from_str("\"reverted\"").unwrap();
        assert_eq!(back, IterationStatus::Reverted);
    }

    /// A fully-populated reverted row round-trips, carrying the inverse-commit SHA
    /// and the iteration it undid.
    #[test]
    fn reverted_record_roundtrips_with_fields() {
        let json = r#"{
            "iteration": 6,
            "approach": "revert of iteration 5",
            "status": "reverted",
            "metrics": {"x": 1.0},
            "rank": 0.0,
            "commit_sha": "deadbeef",
            "reverted_iteration": 5,
            "timestamp": "2026-04-15T00:00:00Z"
        }"#;
        let record: IterationRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.status, IterationStatus::Reverted);
        assert_eq!(record.commit_sha.as_deref(), Some("deadbeef"));
        assert_eq!(record.reverted_iteration, Some(5));
    }

    /// The analysis fields (`score_breakdown`, `changed_files`) default to None
    /// so a ledger written before this feature still deserializes. Pins the
    /// backward-compatibility guarantee for the analysis-artifact work.
    #[test]
    fn legacy_iteration_record_without_analysis_fields_deserializes() {
        let legacy_json = r#"{
            "iteration": 3,
            "approach": "specialize-rzz",
            "status": "kept",
            "metrics": {"latency": 12.0},
            "rank": 0.1,
            "commit_sha": "abc123",
            "reverted_iteration": null,
            "timestamp": "2026-04-15T00:00:00Z"
        }"#;
        let record: IterationRecord = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(record.score_breakdown, None);
        assert_eq!(record.changed_files, None);
        // The fields that did exist still load.
        assert_eq!(record.commit_sha.as_deref(), Some("abc123"));
    }

    /// A row carrying a full structured score breakdown + changed files
    /// round-trips through serde. Pins the on-disk shape the analysis artifact
    /// reads back.
    #[test]
    fn iteration_record_with_analysis_fields_roundtrips() {
        let record = IterationRecord {
            iteration: 2,
            approach: "trim-allocs".to_string(),
            status: IterationStatus::Kept,
            hypothesis: Some("fewer allocations".to_string()),
            metrics: HashMap::from([("latency".to_string(), 8.0)]),
            rank: 0.2,
            score: Some("keep".to_string()),
            reason: Some("latency: 20.00%".to_string()),
            fix_attempts: 0,
            fresh_spawns: 0,
            commit_sha: Some("def456".to_string()),
            reverted_iteration: None,
            score_breakdown: Some(ScoreBreakdown {
                decision: "keep".to_string(),
                metrics: vec![MetricBreakdown {
                    name: "latency".to_string(),
                    baseline: Some(10.0),
                    candidate: Some(8.0),
                    best: Some(10.0),
                    delta_vs_baseline: Some(-2.0),
                    delta_vs_best: Some(-2.0),
                    direction: Some("lower".to_string()),
                    weight: Some(1.0),
                    improvement_vs_best: Some(0.2),
                    contribution: Some(0.2),
                }],
            }),
            changed_files: Some(vec!["src/lib.rs".to_string()]),
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&record).unwrap();
        let back: IterationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
        let bd = back.score_breakdown.unwrap();
        assert_eq!(bd.decision, "keep");
        assert_eq!(bd.metrics[0].direction.as_deref(), Some("lower"));
        assert_eq!(bd.metrics[0].contribution, Some(0.2));
        assert_eq!(back.changed_files.unwrap(), vec!["src/lib.rs".to_string()]);
    }
}
