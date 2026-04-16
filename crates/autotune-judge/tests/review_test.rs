use autotune_judge::{
    Assessment, ExampleStore, JsonlExampleStore, Review, ReviewInput, ReviewPrompter, Rubric,
    ScoreRange, StoredExample, Subject,
};

fn sample_rubric() -> Rubric {
    Rubric {
        id: "trait-extensibility".into(),
        title: "Trait extensibility".into(),
        persona: "Rust integrator adding a backend".into(),
        score_range: ScoreRange::new(0, 10).unwrap(),
        instruction: "Score how easy it is to extend the trait system.".into(),
        guidance: None,
    }
}

fn sample_assessment() -> Assessment {
    Assessment::new(
        "trait-extensibility",
        7,
        "Extension requires little core modification.",
        "mock",
        Some("test-model".into()),
        None,
    )
    .unwrap()
}

fn sample_example() -> StoredExample {
    let review = Review::approved(sample_assessment(), None);
    StoredExample::new(sample_rubric(), Subject::new("API", "summary"), review)
}

#[test]
fn mock_review_prompter_accepts_draft_verbatim() {
    let input = ReviewInput::new(sample_rubric(), sample_assessment());
    let review = autotune_judge::review::MockReviewPrompter::accept()
        .review(&input)
        .unwrap();

    assert_eq!(review.approved_score, 7);
    assert_eq!(
        review.approved_reason,
        "Extension requires little core modification."
    );
    assert!(!review.score_edited);
    assert!(!review.reason_edited);
}

#[test]
fn mock_review_prompter_marks_score_and_reason_edits() {
    let input = ReviewInput::new(sample_rubric(), sample_assessment());
    let review = autotune_judge::review::MockReviewPrompter::edited(9, "Human corrected reason.")
        .review(&input)
        .unwrap();

    assert_eq!(review.approved_score, 9);
    assert_eq!(review.approved_reason, "Human corrected reason.");
    assert!(review.score_edited);
    assert!(review.reason_edited);
}

#[test]
fn mock_review_prompter_rejects_multiline_override_reason() {
    let input = ReviewInput::new(sample_rubric(), sample_assessment());
    let err = autotune_judge::review::MockReviewPrompter::edited(9, "first\nsecond")
        .review(&input)
        .unwrap_err();
    assert!(err.to_string().contains("invalid assessment"));
}

#[test]
fn jsonl_store_load_returns_empty_when_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.jsonl");
    let store = JsonlExampleStore::new(path);

    let items = store.load_examples("anything", 10).unwrap();
    assert!(items.is_empty());
}

#[test]
fn jsonl_store_appends_and_loads_by_rubric_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("examples.jsonl");
    let store = JsonlExampleStore::new(path);

    store.append_example(&sample_example()).unwrap();
    let items = store.load_examples("trait-extensibility", 10).unwrap();

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].rubric.id, "trait-extensibility");
}

#[test]
fn jsonl_store_filters_other_rubric_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("examples.jsonl");
    let store = JsonlExampleStore::new(path);

    store.append_example(&sample_example()).unwrap();

    // Second example with a different rubric id
    let mut other_rubric = sample_rubric();
    other_rubric.id = "ergonomics".into();
    let review2 = Review::approved(sample_assessment(), None);
    let other = StoredExample::new(other_rubric, Subject::new("API", "summary"), review2);
    store.append_example(&other).unwrap();

    let items = store.load_examples("trait-extensibility", 10).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].rubric.id, "trait-extensibility");
}

#[test]
fn jsonl_store_caps_to_limit_and_returns_most_recent_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("examples.jsonl");
    let store = JsonlExampleStore::new(path);

    // Append 3 examples, same rubric id, reasons differ so we can order-check.
    for i in 1..=3 {
        let assessment = Assessment::new(
            "trait-extensibility",
            i,
            format!("reason {i}"),
            "mock",
            None,
            None,
        )
        .unwrap();
        let review = Review::approved(assessment, None);
        let example = StoredExample::new(sample_rubric(), Subject::new("API", "summary"), review);
        store.append_example(&example).unwrap();
    }

    let items = store.load_examples("trait-extensibility", 2).unwrap();
    assert_eq!(items.len(), 2);
    // Most-recent (appended last) is first.
    assert_eq!(items[0].review.approved_score, 3);
    assert_eq!(items[1].review.approved_score, 2);
}
