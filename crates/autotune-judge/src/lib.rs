//! `autotune-judge` provides rubric-driven LLM judging with human correction.
//!
//! This crate is library-first; no CLI integration yet.

pub mod error;
pub mod model;

pub use crate::error::JudgeError;
pub use crate::model::{
    Assessment, Review, Rubric, ScoreRange, StoredExample, Subject, SubjectContext,
    SubjectContextKind,
};
