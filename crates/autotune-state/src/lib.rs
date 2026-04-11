use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub type Metrics = HashMap<String, f64>;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("experiment not found: {name}")]
    NotFound { name: String },

    #[error("invalid phase transition: {from} -> {to}")]
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
    Benchmarking,
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
            Phase::Benchmarking => write!(f, "Benchmarking"),
            Phase::Scoring => write!(f, "Scoring"),
            Phase::Integrating => write!(f, "Integrating"),
            Phase::Recorded => write!(f, "Recorded"),
            Phase::Done => write!(f, "Done"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExperimentState {
    pub experiment_name: String,
    pub canonical_branch: String,
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
pub struct ExperimentStore {
    root: PathBuf,
}

impl ExperimentStore {
    pub fn new(experiment_dir: &Path) -> Result<Self, StateError> {
        fs::create_dir_all(experiment_dir)?;
        fs::create_dir_all(experiment_dir.join("iterations"))?;
        Ok(Self {
            root: experiment_dir.to_path_buf(),
        })
    }

    pub fn open(experiment_dir: &Path) -> Result<Self, StateError> {
        if !experiment_dir.exists() {
            return Err(StateError::NotFound {
                name: experiment_dir.display().to_string(),
            });
        }
        Ok(Self {
            root: experiment_dir.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn save_state(&self, state: &ExperimentState) -> Result<(), StateError> {
        atomic_write(&self.state_path(), &serde_json::to_string_pretty(state)?)
    }

    pub fn load_state(&self) -> Result<ExperimentState, StateError> {
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
        fs::create_dir_all(&dir)?;
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
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("prompt.md"), prompt)
    }

    pub fn save_test_output(
        &self,
        iteration: usize,
        approach: &str,
        output: &str,
    ) -> Result<(), StateError> {
        let dir = self.iteration_dir(iteration, approach);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("test_output.txt"), output)
    }

    pub fn list_experiments(autotune_dir: &Path) -> Result<Vec<String>, StateError> {
        let experiments_dir = autotune_dir.join("experiments");
        if !experiments_dir.exists() {
            return Ok(Vec::new());
        }

        let mut names = Vec::new();
        for entry in fs::read_dir(experiments_dir)? {
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

pub fn atomic_write(path: &Path, content: &str) -> Result<(), StateError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(content.as_bytes())?;
    tmp.persist(path).map_err(|error| StateError::Io {
        source: error.error,
    })?;
    Ok(())
}
