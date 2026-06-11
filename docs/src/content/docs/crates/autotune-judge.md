---
title: autotune-judge
description: Rubric-driven LLM-as-judge evaluation of artifacts, with human review and persisted examples for few-shot prompting.
section: Crates
order: 9
---

`autotune-judge` provides rubric-driven LLM judging with optional human correction. It evaluates an artifact (a `Subject`) from a declared persona's perspective, one narrow `Rubric` at a time, enforces a strict two-line `score:` / `reason:` response contract, and can persist the human-approved outcome for later audit or few-shot prompting. The crate is library-first with no CLI integration yet.

## When to use it

- You need an LLM to score a candidate, plan, or piece of work against an explicit rubric and produce an integer score plus a one-sentence justification.
- You want a human-in-the-loop step that can accept or override the model's draft score and reason before it counts.
- You want approved judgments saved as examples (JSONL) so future judging can be conditioned on prior human-corrected decisions.

## Public API

- `Subject` / `SubjectContext` / `SubjectContextKind` — the artifact under judgment: title, summary, and a vector of typed context entries (`SourceSnippet`, `FilePath`, `Note`).
- `Rubric` — a single judging criterion: `id`, `title`, `persona`, `ScoreRange`, `instruction`, optional `guidance`; `validate()` enforces non-empty fields.
- `ScoreRange` — inclusive `min`/`max` bounds with `new()` validation and `contains()`.
- `Assessment` — a draft judgment (rubric id, score, one-line reason, backend/model/trace attribution, timestamp); `new()` validates the reason is a single non-empty line.
- `Review` — the finalized outcome wrapping an `Assessment` with `approved_score`/`approved_reason` and edit flags; `approved()` accepts verbatim, `edited()` records overrides.
- `StoredExample` — a `Rubric` + `Subject` + `Review` triple, the persisted few-shot unit.
- `Judge` — trait with `assess(&Subject, &Rubric) -> Result<Assessment, JudgeError>`.
- `AgentJudge<B, S>` — the concrete `Judge`: composes a `JudgeBackend` with an optional `ExampleStore` and an `example_limit`.
- `JudgeBackend` / `BackendRequest` / `BackendResponse` — backend abstraction returning a parsed score, reason, and attribution.
- `AgentJudgeBackend` — adapter over `autotune_agent::Agent`; reuses a caller-supplied `AgentConfig`, swapping only the prompt per evaluation.
- `MockJudgeBackend` — test backend returning fixed response text through the real parser.
- `render_batch_prompt` — render one prompt that scores many rubrics under a shared persona.
- `parse_batch_response` — parse a blank-line-separated multi-rubric agent reply into `Vec<Assessment>` (order-independent; errors on missing, unknown, duplicate, malformed, or out-of-range blocks).
- `ReviewPrompter` / `ReviewInput` / `TerminalReviewPrompter` — human review step; the terminal prompter uses `dialoguer` and holds an `autotune_agent::terminal::Guard`.
- `ExampleStore` / `JsonlExampleStore` / `NoStore` — example persistence: JSONL-backed (most-recent-first, capped to `limit`) or a no-op phantom store.
- `JudgeError` — the crate's `thiserror` error enum.

## Usage

```rust
use autotune_judge::{
    AgentJudge, Judge, MockJudgeBackend, NoStore,
    Rubric, ScoreRange, Subject,
};

// Define one rubric and the artifact to evaluate.
let rubric = Rubric {
    id: "clarity".into(),
    title: "Explanation clarity".into(),
    persona: "a senior code reviewer".into(),
    score_range: ScoreRange::new(1, 5)?,
    instruction: "Rate how clearly the change is explained.".into(),
    guidance: None,
};
let subject = Subject::new("PR #42", "Refactors the scoring loop for readability.");

// Back the judge with an agent (here a mock returns a fixed two-line reply).
let backend = MockJudgeBackend::new(
    4,
    "Mostly clear, but one helper is unnamed.",
    "mock",
    None,
    None,
);
let judge = AgentJudge::<_, NoStore>::new(backend, None, 0);

let assessment = judge.assess(&subject, &rubric)?;
assert_eq!(assessment.score, 4);
println!("{}: {}", assessment.score, assessment.reason);
# Ok::<(), autotune_judge::JudgeError>(())
```

## Internal dependencies

- `autotune-agent` — the `AgentJudgeBackend` drives an `autotune_agent::Agent` (via `spawn` + `AgentConfig`), and `TerminalReviewPrompter` uses `autotune_agent::terminal::Guard` for terminal restoration.

## Notes

- The backend contract is strict: a single-rubric response must be exactly two lines, `score: <int>` then `reason: <sentence>`; any third line, missing prefix, non-integer score, empty reason, or score outside the rubric's range is rejected as `JudgeError::BackendParse`. `AgentJudge::assess` also re-checks the score against the rubric range after parsing.
- `Assessment` and `Review` reasons must be a single non-empty line (no embedded newlines).
- `JsonlExampleStore::load_examples` filters by `rubric_id`, returns matches most-recent-first capped to `limit`, and treats a missing file as empty rather than an error; example loading is skipped entirely when the store is `None` or `example_limit` is zero.
