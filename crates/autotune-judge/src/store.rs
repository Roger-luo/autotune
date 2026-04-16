use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::error::JudgeError;
use crate::model::StoredExample;

pub trait ExampleStore {
    fn load_examples(
        &self,
        rubric_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredExample>, JudgeError>;
    fn append_example(&self, example: &StoredExample) -> Result<(), JudgeError>;
}

/// A no-op example store. Useful as a phantom type argument when constructing
/// an `AgentJudge` without examples. Its methods never return data and never
/// accept writes.
pub struct NoStore;

impl ExampleStore for NoStore {
    fn load_examples(
        &self,
        _rubric_id: &str,
        _limit: usize,
    ) -> Result<Vec<StoredExample>, JudgeError> {
        Ok(Vec::new())
    }
    fn append_example(&self, _example: &StoredExample) -> Result<(), JudgeError> {
        Ok(())
    }
}

pub struct JsonlExampleStore {
    path: PathBuf,
}

impl JsonlExampleStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ExampleStore for JsonlExampleStore {
    fn load_examples(
        &self,
        rubric_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredExample>, JudgeError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };

        let reader = std::io::BufReader::new(file);
        let mut items = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let example: StoredExample = serde_json::from_str(&line)?;
            if example.rubric.id == rubric_id {
                items.push(example);
            }
        }
        // Most-recent-first, then cap to `limit`.
        items.reverse();
        items.truncate(limit);
        Ok(items)
    }

    fn append_example(&self, example: &StoredExample) -> Result<(), JudgeError> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, example)?;
        writeln!(file)?;
        Ok(())
    }
}
