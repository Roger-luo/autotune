use crate::error::JudgeError;
use crate::model::{Assessment, Rubric, Subject};
use crate::prompt::render_assessment_prompt;
use crate::store::ExampleStore;

pub struct BackendRequest {
    pub prompt: String,
}

pub struct BackendResponse {
    pub score: i32,
    pub reason: String,
    pub backend_name: String,
    pub model_name: Option<String>,
    pub trace_id: Option<String>,
}

pub trait JudgeBackend {
    fn evaluate(&self, request: BackendRequest) -> Result<BackendResponse, JudgeError>;
}

pub trait Judge {
    fn assess(&self, subject: &Subject, rubric: &Rubric) -> Result<Assessment, JudgeError>;
}

/// Composes a `JudgeBackend` with an optional `ExampleStore` to implement `Judge`.
///
/// `S` is a type parameter rather than `dyn` so callers control example-store
/// representation. When you don't want examples, pass `None` and use `NoStore`
/// as the phantom type:
///
/// ```ignore
/// use autotune_judge::{AgentJudge, NoStore};
/// let judge = AgentJudge::<_, NoStore>::new(backend, None, 0);
/// ```
///
/// `example_limit` is ignored when `store` is `None` or zero.
pub struct AgentJudge<B, S> {
    backend: B,
    store: Option<S>,
    example_limit: usize,
}

impl<B, S> AgentJudge<B, S>
where
    B: JudgeBackend,
    S: ExampleStore,
{
    pub fn new(backend: B, store: Option<S>, example_limit: usize) -> Self {
        Self {
            backend,
            store,
            example_limit,
        }
    }
}

impl<B, S> Judge for AgentJudge<B, S>
where
    B: JudgeBackend,
    S: ExampleStore,
{
    fn assess(&self, subject: &Subject, rubric: &Rubric) -> Result<Assessment, JudgeError> {
        rubric.validate()?;
        let examples = match (&self.store, self.example_limit) {
            (Some(store), limit) if limit > 0 => store.load_examples(&rubric.id, limit)?,
            _ => Vec::new(),
        };
        let prompt = render_assessment_prompt(subject, rubric, &examples);
        let response = self.backend.evaluate(BackendRequest { prompt })?;

        if !rubric.score_range.contains(response.score) {
            return Err(JudgeError::BackendParse {
                message: format!("score {} outside rubric range", response.score),
            });
        }

        Assessment::new(
            rubric.id.clone(),
            response.score,
            response.reason,
            response.backend_name,
            response.model_name,
            response.trace_id,
        )
    }
}

pub(crate) fn parse_backend_text(
    backend_name: impl Into<String>,
    model_name: Option<String>,
    trace_id: Option<String>,
    text: &str,
) -> Result<BackendResponse, JudgeError> {
    let mut lines = text.trim().lines();
    let score_line = lines.next().ok_or_else(|| JudgeError::BackendParse {
        message: "response missing score line".into(),
    })?;
    let reason_line = lines.next().ok_or_else(|| JudgeError::BackendParse {
        message: "response missing reason line".into(),
    })?;
    if lines.next().is_some() {
        return Err(JudgeError::BackendParse {
            message: "response must be exactly two lines".into(),
        });
    }

    let score_value = score_line
        .strip_prefix("score:")
        .ok_or_else(|| JudgeError::BackendParse {
            message: "first line must start with 'score:'".into(),
        })?
        .trim();
    let score: i32 = score_value.parse().map_err(|_| JudgeError::BackendParse {
        message: format!("score value '{score_value}' is not an integer"),
    })?;

    let reason = reason_line
        .strip_prefix("reason:")
        .ok_or_else(|| JudgeError::BackendParse {
            message: "second line must start with 'reason:'".into(),
        })?
        .trim()
        .to_string();
    if reason.is_empty() {
        return Err(JudgeError::BackendParse {
            message: "reason must be non-empty".into(),
        });
    }

    Ok(BackendResponse {
        score,
        reason,
        backend_name: backend_name.into(),
        model_name,
        trace_id,
    })
}

/// Adapter over `autotune_agent::Agent`. Uses the agent's `backend_name()` for
/// attribution and reuses the caller-supplied `AgentConfig` for working dir /
/// model / tool permissions, swapping only the `prompt` per evaluation.
pub struct AgentJudgeBackend<'a> {
    agent: &'a dyn autotune_agent::Agent,
    config: autotune_agent::AgentConfig,
    model_name: Option<String>,
}

impl<'a> AgentJudgeBackend<'a> {
    pub fn new(agent: &'a dyn autotune_agent::Agent, config: autotune_agent::AgentConfig) -> Self {
        let model_name = config.model.clone();
        Self {
            agent,
            config,
            model_name,
        }
    }
}

impl JudgeBackend for AgentJudgeBackend<'_> {
    fn evaluate(&self, request: BackendRequest) -> Result<BackendResponse, JudgeError> {
        let mut config = self.config.clone();
        config.prompt = request.prompt;
        let response = self
            .agent
            .spawn(&config)
            .map_err(|err| JudgeError::BackendCall {
                message: err.to_string(),
            })?;
        parse_backend_text(
            self.agent.backend_name(),
            self.model_name.clone(),
            Some(response.session_id),
            &response.text,
        )
    }
}

/// Test-only backend that returns a fixed raw response text, funnelled through
/// the same `parse_backend_text` as the real adapter.
pub struct MockJudgeBackend {
    text: String,
    backend_name: String,
    model_name: Option<String>,
    trace_id: Option<String>,
}

impl MockJudgeBackend {
    pub fn raw(
        text: impl Into<String>,
        backend_name: impl Into<String>,
        model_name: Option<String>,
        trace_id: Option<String>,
    ) -> Self {
        Self {
            text: text.into(),
            backend_name: backend_name.into(),
            model_name,
            trace_id,
        }
    }

    pub fn new(
        score: i32,
        reason: impl Into<String>,
        backend_name: impl Into<String>,
        model_name: Option<String>,
        trace_id: Option<String>,
    ) -> Self {
        Self::raw(
            format!("score: {score}\nreason: {}", reason.into()),
            backend_name,
            model_name,
            trace_id,
        )
    }
}

impl JudgeBackend for MockJudgeBackend {
    fn evaluate(&self, _request: BackendRequest) -> Result<BackendResponse, JudgeError> {
        parse_backend_text(
            self.backend_name.clone(),
            self.model_name.clone(),
            self.trace_id.clone(),
            &self.text,
        )
    }
}

/// Parse a batched response from a judge agent into a `Vec<Assessment>`.
///
/// Blocks may appear in any order. Returns an error if any rubric is missing,
/// any rubric ID is unrecognised, any score is out of range, or any block is
/// malformed.
pub fn parse_batch_response(rubrics: &[Rubric], text: &str) -> Result<Vec<Assessment>, JudgeError> {
    use std::collections::HashMap;

    let rubric_map: HashMap<&str, &Rubric> = rubrics.iter().map(|r| (r.id.as_str(), r)).collect();

    let mut results: HashMap<String, Assessment> = HashMap::new();

    for block in text.trim().split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut lines = block.lines();
        let id = lines
            .next()
            .ok_or_else(|| JudgeError::BackendParse {
                message: "empty block in batch response".into(),
            })?
            .trim();

        let rubric = rubric_map.get(id).ok_or_else(|| JudgeError::BackendParse {
            message: format!("unknown rubric id '{id}' in batch response"),
        })?;

        let score_line = lines.next().ok_or_else(|| JudgeError::BackendParse {
            message: format!("block for '{id}' missing score line"),
        })?;
        let reason_line = lines.next().ok_or_else(|| JudgeError::BackendParse {
            message: format!("block for '{id}' missing reason line"),
        })?;

        let score_value = score_line
            .strip_prefix("score:")
            .ok_or_else(|| JudgeError::BackendParse {
                message: format!(
                    "block for '{id}': expected 'score:' on second line, got: {score_line}"
                ),
            })?
            .trim();
        let score: i32 = score_value.parse().map_err(|_| JudgeError::BackendParse {
            message: format!("block for '{id}': score '{score_value}' is not an integer"),
        })?;

        if !rubric.score_range.contains(score) {
            return Err(JudgeError::BackendParse {
                message: format!(
                    "block for '{id}': score {score} outside range [{}, {}]",
                    rubric.score_range.min, rubric.score_range.max
                ),
            });
        }

        let reason = reason_line
            .strip_prefix("reason:")
            .ok_or_else(|| JudgeError::BackendParse {
                message: format!(
                    "block for '{id}': expected 'reason:' on third line, got: {reason_line}"
                ),
            })?
            .trim()
            .to_string();

        if reason.is_empty() {
            return Err(JudgeError::BackendParse {
                message: format!("block for '{id}': reason must be non-empty"),
            });
        }

        if results.contains_key(id) {
            return Err(JudgeError::BackendParse {
                message: format!("duplicate block for rubric '{id}' in batch response"),
            });
        }

        results.insert(
            id.to_string(),
            Assessment::new(id, score, reason, "batch", None, None)?,
        );
    }

    for rubric in rubrics {
        if !results.contains_key(rubric.id.as_str()) {
            return Err(JudgeError::BackendParse {
                message: format!("batch response missing block for rubric '{}'", rubric.id),
            });
        }
    }

    Ok(rubrics
        .iter()
        .map(|r| results.remove(r.id.as_str()).unwrap())
        .collect())
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::model::{Rubric, ScoreRange};

    fn rubric(id: &str, min: i32, max: i32) -> Rubric {
        Rubric {
            id: id.to_string(),
            title: id.to_string(),
            persona: String::new(),
            score_range: ScoreRange { min, max },
            instruction: String::new(),
            guidance: None,
        }
    }

    #[test]
    fn parse_batch_happy_path() {
        let rubrics = vec![rubric("r1", 1, 5), rubric("r2", 1, 3)];
        let text = "r1\nscore: 4\nreason: Good but one edge case missing.\n\nr2\nscore: 2\nreason: Needs improvement.";
        let assessments = parse_batch_response(&rubrics, text).unwrap();
        assert_eq!(assessments.len(), 2);
        let r1 = assessments.iter().find(|a| a.rubric_id == "r1").unwrap();
        assert_eq!(r1.score, 4);
        assert_eq!(r1.reason, "Good but one edge case missing.");
        let r2 = assessments.iter().find(|a| a.rubric_id == "r2").unwrap();
        assert_eq!(r2.score, 2);
    }

    #[test]
    fn parse_batch_order_independent() {
        let rubrics = vec![rubric("r1", 1, 5), rubric("r2", 1, 5)];
        let text = "r2\nscore: 3\nreason: Average.\n\nr1\nscore: 5\nreason: Perfect.";
        let assessments = parse_batch_response(&rubrics, text).unwrap();
        assert_eq!(assessments.len(), 2);
        assert_eq!(
            assessments
                .iter()
                .find(|a| a.rubric_id == "r1")
                .unwrap()
                .score,
            5
        );
        assert_eq!(
            assessments
                .iter()
                .find(|a| a.rubric_id == "r2")
                .unwrap()
                .score,
            3
        );
    }

    #[test]
    fn parse_batch_missing_rubric_errors() {
        let rubrics = vec![rubric("r1", 1, 5), rubric("r2", 1, 5)];
        let text = "r1\nscore: 4\nreason: Good.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("r2"), "error: {err}");
    }

    #[test]
    fn parse_batch_unknown_rubric_id_errors() {
        let rubrics = vec![rubric("r1", 1, 5)];
        let text = "r1\nscore: 4\nreason: Good.\n\nunknown\nscore: 3\nreason: Extra.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("unknown"), "error: {err}");
    }

    #[test]
    fn parse_batch_out_of_range_score_errors() {
        let rubrics = vec![rubric("r1", 1, 5)];
        let text = "r1\nscore: 9\nreason: Way too high.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("9"), "error: {err}");
    }

    #[test]
    fn parse_batch_malformed_block_errors() {
        let rubrics = vec![rubric("r1", 1, 5)];
        let text = "r1\nnot-score: 4\nreason: Bad.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("score:"), "error: {err}");
    }
}
