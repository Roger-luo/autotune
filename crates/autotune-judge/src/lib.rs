//! `autotune-judge` provides rubric-driven LLM judging with human correction.
//!
//! This crate is library-first; no CLI integration yet.

pub mod error;
pub mod judge;
pub mod model;
pub mod prompt;
pub mod review;
pub mod store;

pub use crate::error::JudgeError;
pub use crate::judge::{
    AgentJudge, AgentJudgeBackend, BackendRequest, BackendResponse, Judge, JudgeBackend,
};
pub use crate::model::{
    Assessment, Review, Rubric, ScoreRange, StoredExample, Subject, SubjectContext,
    SubjectContextKind,
};
pub use crate::review::{ReviewInput, ReviewPrompter, TerminalReviewPrompter};
pub use crate::store::{ExampleStore, JsonlExampleStore, NoStore};
