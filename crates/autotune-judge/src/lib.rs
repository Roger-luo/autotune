//! `autotune-judge` provides rubric-driven LLM judging with human correction.
//!
//! This crate is library-first; no CLI integration yet. Its purpose is to
//! evaluate an artifact ("subject") from a declared persona's perspective,
//! one narrow rubric at a time, and preserve the human-approved outcome for
//! later audit or few-shot prompting.
//!
//! # Typical flow
//!
//! 1. Build a [`Rubric`] (persona + score range + one-sentence instruction)
//!    and a [`Subject`] (title + summary + optional context).
//! 2. Call a [`Judge`] — the only concrete implementation in v1 is
//!    [`AgentJudge`], backed by a [`JudgeBackend`] that wraps an
//!    `autotune_agent::Agent`. The backend enforces the strict two-line
//!    `score:` / `reason:` contract.
//! 3. Run a [`ReviewPrompter`] on the draft [`Assessment`] to get the final
//!    [`Review`]. [`TerminalReviewPrompter`] is a reusable terminal UX; mocks
//!    are provided for tests.
//! 4. Persist a [`StoredExample`] via an [`ExampleStore`] implementation such
//!    as [`JsonlExampleStore`] for future in-context prompting.
//!
//! # Structure
//!
//! - [`model`] — typed `Subject` / `Rubric` / `Assessment` / `Review` / `StoredExample`.
//! - [`prompt`] — rubric prompt rendering.
//! - [`judge`] — `Judge` / `JudgeBackend` traits, `AgentJudge` composition, and
//!   the `AgentJudgeBackend` adapter over `autotune_agent::Agent`.
//! - [`review`] — `ReviewPrompter` trait and the `TerminalReviewPrompter`.
//! - [`store`] — `ExampleStore` trait, `JsonlExampleStore`, and a no-op `NoStore`.

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
