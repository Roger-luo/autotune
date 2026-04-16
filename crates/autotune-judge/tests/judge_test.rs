use autotune_judge::{
    Assessment, Review, Rubric, ScoreRange, StoredExample, Subject, SubjectContext,
    SubjectContextKind,
};

#[test]
fn rubric_rejects_empty_id() {
    let rubric = Rubric {
        id: String::new(),
        title: "Trait extensibility".into(),
        persona: "Rust integrator".into(),
        score_range: ScoreRange::new(0, 10).unwrap(),
        instruction: "Score how easy it is to extend the trait system.".into(),
        guidance: None,
    };

    let err = rubric.validate().unwrap_err();
    assert!(err.to_string().contains("invalid rubric"));
}

#[test]
fn assessment_reason_must_be_single_line() {
    let err = Assessment::new(
        "trait-extensibility",
        7,
        "first line\nsecond line",
        "mock-backend",
        None,
        None,
    )
    .unwrap_err();

    assert!(err.to_string().contains("invalid assessment"));
}

#[test]
fn score_range_rejects_inverted_bounds() {
    let err = ScoreRange::new(5, 3).unwrap_err();
    assert!(err.to_string().contains("invalid rubric"));
}

#[test]
fn score_range_contains_inclusive() {
    let range = ScoreRange::new(0, 10).unwrap();
    assert!(range.contains(0));
    assert!(range.contains(10));
    assert!(range.contains(5));
    assert!(!range.contains(-1));
    assert!(!range.contains(11));
}

#[test]
fn rubric_validate_accepts_complete_rubric() {
    let rubric = Rubric {
        id: "trait-extensibility".into(),
        title: "Trait extensibility".into(),
        persona: "Rust integrator".into(),
        score_range: ScoreRange::new(0, 10).unwrap(),
        instruction: "Score how easy it is to extend the trait system.".into(),
        guidance: Some("Consider crate boundaries.".into()),
    };
    assert!(rubric.validate().is_ok());
}

#[test]
fn rubric_validate_rejects_blank_instruction() {
    let rubric = Rubric {
        id: "ok-id".into(),
        title: "A title".into(),
        persona: "A persona".into(),
        score_range: ScoreRange::new(0, 10).unwrap(),
        instruction: "   ".into(),
        guidance: None,
    };
    let err = rubric.validate().unwrap_err();
    assert!(err.to_string().contains("instruction"));
}

#[test]
fn rubric_validate_rejects_blank_title_or_persona() {
    let rubric = Rubric {
        id: "ok-id".into(),
        title: "".into(),
        persona: "Persona".into(),
        score_range: ScoreRange::new(0, 10).unwrap(),
        instruction: "Do the thing".into(),
        guidance: None,
    };
    assert!(rubric.validate().is_err());

    let rubric = Rubric {
        id: "ok-id".into(),
        title: "Title".into(),
        persona: "".into(),
        score_range: ScoreRange::new(0, 10).unwrap(),
        instruction: "Do the thing".into(),
        guidance: None,
    };
    assert!(rubric.validate().is_err());
}

#[test]
fn assessment_reason_must_not_be_blank() {
    let err =
        Assessment::new("trait-extensibility", 7, "   ", "mock-backend", None, None).unwrap_err();
    assert!(err.to_string().contains("invalid assessment"));
}

#[test]
fn assessment_accepts_valid_single_line_reason() {
    let a = Assessment::new(
        "trait-extensibility",
        7,
        "clearly extensible through the Agent trait",
        "mock-backend",
        Some("mock-model".into()),
        Some("trace-123".into()),
    )
    .unwrap();
    assert_eq!(a.rubric_id, "trait-extensibility");
    assert_eq!(a.score, 7);
    assert_eq!(a.model_name.as_deref(), Some("mock-model"));
    assert_eq!(a.trace_id.as_deref(), Some("trace-123"));
}

#[test]
fn review_approved_preserves_draft_and_flags_unedited() {
    let a = Assessment::new(
        "trait-extensibility",
        6,
        "decent extension story",
        "mock-backend",
        None,
        None,
    )
    .unwrap();
    let review = Review::approved(a.clone(), Some("reviewer@example.com".into()));
    assert_eq!(review.approved_score, 6);
    assert_eq!(review.approved_reason, "decent extension story");
    assert!(!review.score_edited);
    assert!(!review.reason_edited);
    assert_eq!(review.assessment, a);
}

#[test]
fn review_edited_flips_score_edited_flag() {
    let a = Assessment::new(
        "trait-extensibility",
        6,
        "decent extension story",
        "mock-backend",
        None,
        None,
    )
    .unwrap();
    let review = Review::edited(a.clone(), 8, "decent extension story", None).unwrap();
    assert!(review.score_edited);
    assert!(!review.reason_edited);
    assert_eq!(review.approved_score, 8);
    assert_eq!(review.approved_reason, "decent extension story");
}

#[test]
fn review_edited_flips_reason_edited_flag() {
    let a = Assessment::new(
        "trait-extensibility",
        6,
        "decent extension story",
        "mock-backend",
        None,
        None,
    )
    .unwrap();
    let review = Review::edited(a.clone(), 6, "actually brittle in practice", None).unwrap();
    assert!(!review.score_edited);
    assert!(review.reason_edited);
}

#[test]
fn review_edited_rejects_multiline_reason() {
    let a = Assessment::new(
        "trait-extensibility",
        6,
        "decent extension story",
        "mock-backend",
        None,
        None,
    )
    .unwrap();
    let err = Review::edited(a, 6, "line one\nline two", None).unwrap_err();
    assert!(err.to_string().contains("invalid assessment"));
}

#[test]
fn subject_render_context_joins_entries() {
    let subject = Subject::new("Agent trait", "How extensible is it?").with_context(vec![
        SubjectContext {
            kind: SubjectContextKind::SourceSnippet,
            label: "trait".into(),
            body: "pub trait Agent {}".into(),
        },
        SubjectContext {
            kind: SubjectContextKind::FilePath,
            label: "file".into(),
            body: "crates/autotune-agent/src/lib.rs".into(),
        },
        SubjectContext {
            kind: SubjectContextKind::Note,
            label: "nb".into(),
            body: "see notes/agent-protocol.md".into(),
        },
    ]);
    let rendered = subject.render_context();
    assert!(rendered.contains("- [source] trait: pub trait Agent {}"));
    assert!(rendered.contains("- [path] file: crates/autotune-agent/src/lib.rs"));
    assert!(rendered.contains("- [note] nb: see notes/agent-protocol.md"));
    assert_eq!(rendered.lines().count(), 3);
}

#[test]
fn subject_render_context_empty_when_no_context() {
    let subject = Subject::new("T", "S");
    assert_eq!(subject.render_context(), "");
}

#[test]
fn assessment_serializes_and_roundtrips_via_serde_json() {
    let a = Assessment::new(
        "trait-extensibility",
        7,
        "clearly extensible",
        "mock-backend",
        Some("mock-model".into()),
        None,
    )
    .unwrap();
    let json = serde_json::to_string(&a).unwrap();
    let back: Assessment = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
}

#[test]
fn review_serializes_and_roundtrips_via_serde_json() {
    let a = Assessment::new(
        "trait-extensibility",
        7,
        "clearly extensible",
        "mock-backend",
        None,
        None,
    )
    .unwrap();
    let review = Review::edited(a, 8, "actually very extensible", Some("rev".into())).unwrap();
    let json = serde_json::to_string(&review).unwrap();
    let back: Review = serde_json::from_str(&json).unwrap();
    assert_eq!(review, back);
}

#[test]
fn stored_example_bundles_rubric_subject_and_review() {
    let rubric = Rubric {
        id: "trait-extensibility".into(),
        title: "Trait extensibility".into(),
        persona: "Rust integrator".into(),
        score_range: ScoreRange::new(0, 10).unwrap(),
        instruction: "Score extensibility".into(),
        guidance: None,
    };
    let subject = Subject::new("Agent trait", "Summary");
    let assessment = Assessment::new(
        "trait-extensibility",
        7,
        "clearly extensible",
        "mock-backend",
        None,
        None,
    )
    .unwrap();
    let review = Review::approved(assessment, None);

    let example = StoredExample::new(rubric.clone(), subject.clone(), review.clone());
    assert_eq!(example.rubric, rubric);
    assert_eq!(example.subject, subject);
    assert_eq!(example.review, review);

    let json = serde_json::to_string(&example).unwrap();
    let back: StoredExample = serde_json::from_str(&json).unwrap();
    assert_eq!(example, back);
}
