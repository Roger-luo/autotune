use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::JudgeError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScoreRange {
    pub min: i32,
    pub max: i32,
}

impl ScoreRange {
    pub fn new(min: i32, max: i32) -> Result<Self, JudgeError> {
        if min > max {
            return Err(JudgeError::InvalidRubric {
                message: format!("score range min {min} is greater than max {max}"),
            });
        }
        Ok(Self { min, max })
    }

    pub fn contains(&self, score: i32) -> bool {
        score >= self.min && score <= self.max
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubjectContextKind {
    SourceSnippet,
    FilePath,
    Note,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubjectContext {
    pub kind: SubjectContextKind,
    pub label: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Subject {
    pub title: String,
    pub summary: String,
    pub context: Vec<SubjectContext>,
}

impl Subject {
    pub fn new(title: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            summary: summary.into(),
            context: Vec::new(),
        }
    }

    pub fn with_context(mut self, context: Vec<SubjectContext>) -> Self {
        self.context = context;
        self
    }

    /// Render the context vector as a newline-joined block for prompt use.
    /// Empty when there is no context.
    pub fn render_context(&self) -> String {
        self.context
            .iter()
            .map(|c| {
                let kind = match c.kind {
                    SubjectContextKind::SourceSnippet => "source",
                    SubjectContextKind::FilePath => "path",
                    SubjectContextKind::Note => "note",
                };
                format!("- [{kind}] {label}: {body}", label = c.label, body = c.body)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rubric {
    pub id: String,
    pub title: String,
    pub persona: String,
    pub score_range: ScoreRange,
    pub instruction: String,
    pub guidance: Option<String>,
}

impl Rubric {
    pub fn validate(&self) -> Result<(), JudgeError> {
        if self.id.trim().is_empty() {
            return Err(JudgeError::InvalidRubric {
                message: "rubric id cannot be empty".into(),
            });
        }
        if self.title.trim().is_empty() || self.persona.trim().is_empty() {
            return Err(JudgeError::InvalidRubric {
                message: "rubric title and persona cannot be empty".into(),
            });
        }
        if self.instruction.trim().is_empty() {
            return Err(JudgeError::InvalidRubric {
                message: "rubric instruction cannot be empty".into(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Assessment {
    pub rubric_id: String,
    pub score: i32,
    pub reason: String,
    pub backend_name: String,
    pub model_name: Option<String>,
    pub trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Assessment {
    pub fn new(
        rubric_id: impl Into<String>,
        score: i32,
        reason: impl Into<String>,
        backend_name: impl Into<String>,
        model_name: Option<String>,
        trace_id: Option<String>,
    ) -> Result<Self, JudgeError> {
        let reason = reason.into();
        if reason.trim().is_empty() || reason.contains('\n') {
            return Err(JudgeError::InvalidAssessment {
                message: "assessment reason must be one non-empty line".into(),
            });
        }
        Ok(Self {
            rubric_id: rubric_id.into(),
            score,
            reason,
            backend_name: backend_name.into(),
            model_name,
            trace_id,
            created_at: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Review {
    pub assessment: Assessment,
    pub approved_score: i32,
    pub approved_reason: String,
    pub score_edited: bool,
    pub reason_edited: bool,
    pub reviewer: Option<String>,
    pub reviewed_at: DateTime<Utc>,
}

impl Review {
    /// Reviewer accepted draft score + reason verbatim.
    pub fn approved(assessment: Assessment, reviewer: Option<String>) -> Self {
        let approved_score = assessment.score;
        let approved_reason = assessment.reason.clone();
        Self {
            assessment,
            approved_score,
            approved_reason,
            score_edited: false,
            reason_edited: false,
            reviewer,
            reviewed_at: Utc::now(),
        }
    }

    /// Reviewer supplied overrides. Edit flags are derived from the deltas.
    /// Reason is validated using the same rule as `Assessment::new`.
    pub fn edited(
        assessment: Assessment,
        approved_score: i32,
        approved_reason: impl Into<String>,
        reviewer: Option<String>,
    ) -> Result<Self, JudgeError> {
        let approved_reason = approved_reason.into();
        if approved_reason.trim().is_empty() || approved_reason.contains('\n') {
            return Err(JudgeError::InvalidAssessment {
                message: "approved reason must be one non-empty line".into(),
            });
        }
        let score_edited = approved_score != assessment.score;
        let reason_edited = approved_reason != assessment.reason;
        Ok(Self {
            assessment,
            approved_score,
            approved_reason,
            score_edited,
            reason_edited,
            reviewer,
            reviewed_at: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredExample {
    pub rubric: Rubric,
    pub subject: Subject,
    pub review: Review,
}

impl StoredExample {
    pub fn new(rubric: Rubric, subject: Subject, review: Review) -> Self {
        Self {
            rubric,
            subject,
            review,
        }
    }
}
