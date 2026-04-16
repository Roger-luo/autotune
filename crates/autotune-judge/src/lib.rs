//! `autotune-judge` provides rubric-driven LLM judging with human correction.
//!
//! This crate is library-first; no CLI integration yet. See
//! `docs/superpowers/specs/2026-04-16-autotune-judge-design.md` for the full design.

pub mod error;

pub use crate::error::JudgeError;
