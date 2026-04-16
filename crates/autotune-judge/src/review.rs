use crate::error::JudgeError;
use crate::model::{Assessment, Review, Rubric};

pub struct ReviewInput {
    pub rubric: Rubric,
    pub assessment: Assessment,
}

impl ReviewInput {
    pub fn new(rubric: Rubric, assessment: Assessment) -> Self {
        Self { rubric, assessment }
    }
}

pub trait ReviewPrompter {
    fn review(&self, input: &ReviewInput) -> Result<Review, JudgeError>;
}

/// Interactive terminal reviewer using `dialoguer`. Holds an
/// `autotune_agent::terminal::Guard` for the duration of each interact to
/// guarantee terminal restoration on exit — even on panic or `?` return.
pub struct TerminalReviewPrompter;

impl ReviewPrompter for TerminalReviewPrompter {
    fn review(&self, input: &ReviewInput) -> Result<Review, JudgeError> {
        let _guard = autotune_agent::terminal::Guard::new();

        let accept = dialoguer::Confirm::new()
            .with_prompt(format!(
                "Rubric '{}' scored {}/{}: {}. Accept?",
                input.rubric.title,
                input.assessment.score,
                input.rubric.score_range.max,
                input.assessment.reason
            ))
            .default(true)
            .interact()
            .map_err(|e| JudgeError::Io {
                source: std::io::Error::other(e),
            })?;

        if accept {
            return Ok(Review::approved(input.assessment.clone(), None));
        }

        let approved_score: i32 = dialoguer::Input::new()
            .with_prompt("Approved score")
            .with_initial_text(input.assessment.score.to_string())
            .interact_text()
            .map_err(|e| JudgeError::Io {
                source: std::io::Error::other(e),
            })?;

        let approved_reason: String = dialoguer::Input::new()
            .with_prompt("Approved reason")
            .with_initial_text(input.assessment.reason.clone())
            .interact_text()
            .map_err(|e| JudgeError::Io {
                source: std::io::Error::other(e),
            })?;

        Review::edited(
            input.assessment.clone(),
            approved_score,
            approved_reason,
            None,
        )
    }
}

/// Test-only prompter with predetermined behavior. No terminal I/O.
pub struct MockReviewPrompter {
    override_score: Option<i32>,
    override_reason: Option<String>,
    reviewer: Option<String>,
}

impl MockReviewPrompter {
    /// Accept the draft verbatim.
    pub fn accept() -> Self {
        Self {
            override_score: None,
            override_reason: None,
            reviewer: None,
        }
    }

    /// Override both score and reason (simulates a reviewer who edited both).
    pub fn edited(score: i32, reason: impl Into<String>) -> Self {
        Self {
            override_score: Some(score),
            override_reason: Some(reason.into()),
            reviewer: None,
        }
    }

    pub fn with_reviewer(mut self, name: impl Into<String>) -> Self {
        self.reviewer = Some(name.into());
        self
    }
}

impl ReviewPrompter for MockReviewPrompter {
    fn review(&self, input: &ReviewInput) -> Result<Review, JudgeError> {
        // Bind `assessment` so the compiler considers it used after we
        // pattern-match overrides.
        let assessment: &Assessment = &input.assessment;
        match (self.override_score, &self.override_reason) {
            (None, None) => Ok(Review::approved(assessment.clone(), self.reviewer.clone())),
            (score, reason) => Review::edited(
                assessment.clone(),
                score.unwrap_or(assessment.score),
                reason.clone().unwrap_or_else(|| assessment.reason.clone()),
                self.reviewer.clone(),
            ),
        }
    }
}
