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
