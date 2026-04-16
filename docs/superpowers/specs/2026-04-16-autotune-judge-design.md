# autotune-judge Design

## Goal

Create a new workspace crate, `autotune-judge`, that provides a reusable "LLM as judge" framework for evaluating user-surface abstraction quality. The first target is Rust API ergonomics and extensibility, with a rubric-driven workflow where:

- a rubric defines what is being judged and from whose perspective;
- an LLM produces a compact assessment for one rubric at a time;
- a human reviews and may correct the score and reason;
- approved rubric/assessment pairs are stored as examples for future in-context learning.

The first version is intentionally a library crate, not a CLI command. It must be usable from the existing Autotune codebase later, but should not depend on current CLI or TUI flows.

## Non-Goals

- No integration into `autotune` task execution yet.
- No full-screen TUI.
- No multi-judge orchestration, voting, or ensemble scoring.
- No automatic model fine-tuning pipeline.
- No attempt to produce a large rubric matrix in one model call.

## Design Principles

1. Keep the unit of judgment small: one rubric in, one compact assessment out.
2. Keep the framework backend-agnostic: rubric semantics belong in `autotune-judge`, model execution belongs behind an adapter.
3. Treat LLM output as draft evaluation, not final truth.
4. Preserve enough structured data to support future few-shot prompting, auditing, and human calibration.
5. Make the first implementation concrete enough to exercise the abstractions immediately.

## User Model

The framework evaluates artifacts from the perspective of a declared user role. Examples:

- a Rust library maintainer extending a trait-based system;
- an integrator adding a new backend;
- a new contributor trying to understand the extension points;
- an experienced Rust user adopting the API in a new codebase.

This role is part of rubric definition, not part of the backend transport. The backend receives rendered prompts that already encode the intended perspective.

## Core Concepts

### Subject

The thing being judged. For the first version, a subject is generic structured input that may include:

- a title;
- a short description;
- source snippets or paths;
- supporting context such as task intent or architecture notes.

The subject model should avoid baking in Rust-specific semantics so the crate can later be reused for other judged artifacts.

### Rubric

A rubric defines one evaluation dimension. It should include:

- stable rubric identifier;
- short title;
- role/persona being simulated;
- score range, initially integer `0..=10`;
- one-sentence judging instruction;
- optional longer guidance for prompt rendering.

Each rubric is intentionally narrow. Example:

- `trait-extensibility`
- role: `Rust integrator adding a new backend`
- instruction: `Judge how easy it is to extend the trait system without modifying existing core code.`

### Assessment

The LLM-generated result for one rubric. It should include:

- rubric identifier;
- numeric score;
- one-sentence reason;
- backend metadata such as model/backend name;
- optional trace data such as prompt or session identifier;
- timestamp.

The crate should enforce compact output at the type level where practical. The intended result is "score plus short reason", not a multi-paragraph review.

### Review

The human-approved version of an assessment. It should include:

- the original assessment;
- final approved score;
- final approved reason;
- whether the human edited the score;
- whether the human edited the reason;
- reviewer metadata where available;
- timestamp.

The review record is the canonical outcome for downstream use.

### Example

An example is a persisted training/reference datum built from:

- rubric;
- subject summary or excerpt used for judgment;
- human-approved review.

Examples are used for future in-context learning, not as labels hidden inside opaque prompt logs.

## Crate Structure

The initial crate should have four focused modules:

1. `model`
   Defines `Subject`, `Rubric`, `Assessment`, `Review`, and supporting metadata types.
2. `judge`
   Defines the high-level judging traits and the concrete agent-backed judge.
3. `prompt`
   Defines typed prompt templates and rendering helpers for rubric-based judgments.
4. `review`
   Defines reusable interactive review components for terminal-based human correction.

Optional persistence can start as a fifth module:

5. `store`
   Defines example persistence traits plus a simple local append-only implementation.

## Trait Boundaries

### `JudgeBackend`

Low-level adapter responsible for sending rendered prompt input to a model and returning parsed structured output.

Responsibilities:

- execute a prompt via an LLM backend;
- return a structured score plus reason;
- expose backend/model metadata;
- surface parsing and backend failures cleanly.

Non-responsibilities:

- rubric design;
- persona semantics;
- human review decisions.

### `Judge`

High-level abstraction responsible for:

- accepting `Subject` and `Rubric`;
- optionally loading approved examples;
- rendering prompts;
- calling the backend;
- returning an `Assessment`.

This trait owns framework semantics. The first concrete implementation will be an `AgentJudge` built on the existing agent wrapper.

### `ExampleStore`

Persistence abstraction for retrieving and storing approved examples.

Operations:

- fetch examples relevant to a rubric;
- append a newly approved example.

The first implementation should be a simple local store, likely JSONL or line-delimited structured records, chosen for append-only behavior and easy inspection.

### `ReviewSession` or `ReviewUi`

Abstraction for human correction workflows. For the first version, this can be a library-facing review component rather than a deeply abstract trait, but it must keep UI concerns separate from judging concerns.

Responsibilities:

- present draft score/reason;
- allow accept/edit flows;
- return a `Review`.

## First Concrete Backend

### `AgentJudge`

The first concrete judge should use the existing agent wrapper stack rather than inventing a new transport. It should:

- render a compact rubric prompt;
- request strictly structured output;
- parse score and reason into typed values;
- attach backend metadata from the underlying agent configuration.

This implementation should be opinionated enough to be useful, but the crate API should still allow additional backends later.

## Prompting Strategy

The first version should support typed prompt templates rather than ad hoc strings spread through the codebase.

Prompt inputs should include:

- the rubric title and instruction;
- the role/persona;
- the score scale;
- the subject description and selected context;
- optional approved examples from the store;
- output constraints: exactly one score and one short reason.

Prompt outputs should be parsed into a narrow schema. The schema should reject verbose or malformed answers rather than silently accepting them.

### Few-shot Example Use

When examples are available, the judge may include a small number of human-approved examples in the prompt. Selection can initially be simple:

- same rubric id;
- most recent approved examples;
- fixed maximum count.

More advanced retrieval is out of scope for v1.

## Human Review Flow

The first review flow should be a reusable terminal interaction component, not a command wired into `autotune`.

Expected interaction:

1. Show rubric title and persona.
2. Show candidate score and one-sentence reason.
3. Ask the reviewer to accept or edit.
4. If editing, allow:
   - integer score adjustment within rubric bounds;
   - reason editing as short text.
5. Return a `Review` object.

This review component should follow existing terminal-safety conventions in the repo:

- hold `autotune_agent::terminal::Guard` around interactive terminal operations;
- avoid leaving the terminal in a broken state on interruption.

The initial UX can use the same prompt libraries already present in the repository, rather than introducing a new TUI framework.

## Data Model and Persistence

The crate should preserve a clear distinction between:

- draft LLM assessment;
- final human-approved review;
- derived example record for future prompting.

The initial append-only storage format should be readable and diffable. JSONL is a good default because:

- each approved example is one record;
- appends are straightforward;
- later tools can stream and filter records without loading a full database.

Each stored example should carry enough metadata to reconstruct evaluation context:

- rubric id and title;
- persona;
- score scale;
- subject summary or excerpt;
- original LLM score/reason;
- final approved score/reason;
- backend metadata;
- timestamps.

## Error Handling

Use `thiserror` for library error types and `anyhow` only at outer application boundaries.

Main error categories:

- prompt rendering failure;
- backend invocation failure;
- malformed backend response;
- review interaction failure;
- example store read/write failure.

Malformed model output should be treated as an error, not normalized implicitly.

## Testing Strategy

The first implementation should be tested at three levels:

1. Unit tests for model validation and prompt rendering.
2. Backend parsing tests for valid and invalid structured outputs.
3. Review flow tests for editing and approval logic, with UI-independent pieces isolated where possible.

Because this crate starts as a library, full scenario coverage through the `autotune` binary is out of scope for this first step.

## Recommended Implementation Sequence

1. Scaffold `autotune-judge` as a new workspace crate.
2. Add core model types and validation.
3. Add judge traits and prompt rendering abstractions.
4. Implement the first agent-backed judge.
5. Add terminal review components.
6. Add local example store implementation.
7. Add focused unit tests across all modules.

## Open Decisions Resolved for v1

- Score format: integer `0..=10`.
- Result verbosity: one numeric score plus one sentence reason.
- Human review outcome: final approved score and reason replace draft values for downstream use, while preserving the original draft for audit.
- Persistence format: append-only local structured records, preferably JSONL.
- Integration point: deferred; the crate remains library-first.

## Follow-up Work After v1

- integrate review flows into `autotune` CLI/TUI;
- add multi-judge orchestration;
- add better example retrieval and selection;
- explore pairwise or calibration-based judging modes;
- support richer subject loaders for Rust API surfaces.
