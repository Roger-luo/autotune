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
/// representation. When you don't want examples, pass `None` and pick any
/// `ExampleStore` type as the phantom:
///
/// ```ignore
/// let judge = AgentJudge::<_, JsonlExampleStore>::new(backend, None, 0);
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
