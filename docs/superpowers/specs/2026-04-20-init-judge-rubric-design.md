# Init Agent: LLM Judge Rubric Design

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extend the `autotune init` agent-assisted workflow to help users design rubrics for the LLM judge adaptor through a guided interview, with per-rubric approval before finalizing the measure config.

**Architecture:** Two new XML fragment types (`<rubric>` and `<rubrics-done/>`) extend the init protocol. The CLI accumulates approved rubrics in a `PendingJudgeMeasure` staging area attached to `ConfigAccumulator`. The init agent follows a documented 5-step interview script in the system prompt. No changes to the config crate or main binary.

**Tech Stack:** Rust, `quick-xml`, `autotune-init`, `autotune-config` (existing `AdaptorConfig::Judge`, `RubricConfig`).

---

## Fragment Protocol Extensions

### `<measure type="judge">` (modified)

Used to declare a judge measure's metadata. Rubrics are **not** included here — they arrive via `<rubric>` fragments. Fields: `name` (required), `persona` (required), `command` (optional, array of `<segment>`).

```xml
<measure>
  <name>code-quality</name>
  <command><segment>cargo</segment><segment>test</segment></command>
  <adaptor>
    <type>judge</type>
    <persona><![CDATA[A senior Rust engineer who values correctness and clarity]]></persona>
  </adaptor>
</measure>
```

When the CLI receives this, it creates a `PendingJudgeMeasure` (not yet added to the measure list). A second `<measure type="judge">` before `<rubrics-done/>` is a protocol error.

### `<rubric>` (new)

Proposes one rubric for user review. All fields required.

```xml
<rubric>
  <id>correctness</id>
  <title>Correctness</title>
  <instruction><![CDATA[Does the implementation produce correct results for all valid inputs, including edge cases?]]></instruction>
  <score-range><min>1</min><max>5</max></score-range>
</rubric>
```

The CLI validates the fragment, displays a formatted rubric preview, and prompts the user with three options:

- **Accept** — store rubric as-is in `pending_judge.approved_rubrics`
- **Reject** — discard; report `"Rubric 'id' rejected by user."` to agent
- **Modify** — user types a replacement instruction; store with the modified text; report `"Rubric 'id' accepted with modified instruction: [...]"`

Rubric IDs must be unique within a pending judge measure. Duplicate IDs are rejected with an error reported to the agent.

### `<rubrics-done/>` (new)

Signals the agent is finished proposing rubrics. The CLI:

1. Requires `pending_judge` to be non-None (error otherwise)
2. Requires `approved_rubrics` to be non-empty (error otherwise)
3. Assembles `MeasureConfig { adaptor: AdaptorConfig::Judge { persona, rubrics }, ... }`
4. Pushes the completed measure to `accumulator.measures`
5. Clears `pending_judge`

After this, the agent proceeds to emit `<score>` using the approved rubric IDs as metric names.

---

## Accumulator State

`ConfigAccumulator` gains one new field:

```rust
pending_judge: Option<PendingJudgeMeasure>,
```

```rust
struct PendingJudgeMeasure {
    name: String,
    persona: String,
    command: Option<Vec<String>>,
    approved_rubrics: Vec<RubricConfig>,
}
```

`is_complete()` gains an extra guard: `self.pending_judge.is_none()`. An unfinalized judge measure (measure declared but `<rubrics-done/>` not yet received) blocks the init flow from completing.

`missing_sections()` reports `"judge rubrics (use <rubrics-done/> to finalize)"` when `pending_judge` is Some.

---

## User Interaction: Rubric Approval

When a `<rubric>` fragment is received, the CLI calls a new function `show_rubric_proposal(rubric, input) -> RubricOutcome`:

```
[autotune] Proposed rubric:
  ID:          correctness
  Title:       Correctness
  Instruction: Does the implementation produce correct results for all valid inputs?
  Score range: 1–5

  > Accept
    Reject
    Modify (enter new instruction)
```

If the user selects **Modify**, a follow-up free-text prompt collects the replacement instruction. The rubric is stored with the user's text.

The outcome is reported to the agent on the next turn as a single line prepended to any other feedback, e.g.:
- `"Rubric 'correctness' (score 1–5): accepted."`
- `"Rubric 'readability': rejected by user."`
- `"Rubric 'clarity' accepted with modified instruction: 'Is the code easy to read without comments?'"`

---

## Init Agent Interview Script (prompt.rs additions)

The system prompt gains a **Judge Rubric Design** section that instructs the agent to follow this 5-step flow when the user wants LLM judge evaluation:

1. **Interview** — Emit a `<question>` asking what quality dimensions matter (free-response allowed). Example dimensions: correctness, performance, readability, safety, test coverage, API ergonomics.
2. **Emit judge measure header** — Emit `<measure type="judge">` with `name` and `persona` derived from the user's goal. No rubrics yet.
3. **Propose rubrics one at a time** — For each dimension identified, emit one `<rubric>` and wait for CLI feedback before proceeding to the next. Propose 3–5 rubrics total.
4. **Check satisfaction** — After proposing all rubrics, emit a `<question>` with options: `"Add more dimensions"` / `"These look good, finalize"`.
5. **Finalize** — If done, emit `<rubrics-done/>`. Then emit `<score>` using the approved rubric IDs as `primary_metrics` with `direction = "Maximize"` and equal weights.

The prompt instructs the agent to incorporate user modifications (from step 3 feedback) when deciding whether to propose more rubrics on the same theme.

---

## Score Config Guidance

After `<rubrics-done/>`, the agent emits `<score>` referencing only the **approved** rubric IDs (those reported back as accepted or modified — not rejected ones). The prompt instructs the agent to use this exact list from the CLI feedback messages.

---

## Validation Rules

| Condition | Error |
|---|---|
| `<rubric>` with no `pending_judge` | Protocol error: no active judge measure |
| `<rubric>` with duplicate `id` | Protocol error: duplicate rubric id |
| `<rubric>` with `score-range min >= max` | Validation error |
| `<rubrics-done/>` with no `pending_judge` | Protocol error |
| `<rubrics-done/>` with zero approved rubrics | Error: at least one rubric required |
| `<measure type="judge">` while `pending_judge` is Some | Protocol error: previous judge measure not finalized |

All errors are reported to the agent as feedback on the next turn (same mechanism as existing validation errors).

---

## Testing

New tests in `crates/autotune-init/tests/init_test.rs`:

1. **`rubric_accept_stores_rubric`** — Agent emits one `<rubric>`, mock user accepts; verify `pending_judge.approved_rubrics` has one entry.
2. **`rubric_reject_discards_rubric`** — Agent emits `<rubric>`, mock user rejects; verify `approved_rubrics` is empty and agent receives rejection feedback.
3. **`rubric_modify_stores_with_new_instruction`** — Mock user selects modify and types new text; verify stored rubric has the user's instruction.
4. **`rubrics_done_assembles_measure`** — Full sequence: judge measure header + two accepted rubrics + `<rubrics-done/>`; verify `accumulator.measures` contains a complete `AdaptorConfig::Judge` with both rubrics.
5. **`rubrics_done_with_no_rubrics_is_error`** — `<rubrics-done/>` with zero approved rubrics reports error to agent.
6. **`unfinalized_judge_blocks_completion`** — Judge measure header emitted but no `<rubrics-done/>`; verify `is_complete()` returns false.
7. **`full_judge_init_flow`** — End-to-end: MockAgent emits question → judge measure → 3 rubrics → finalize question → `<rubrics-done/>` → score; verify final config has correct `AdaptorConfig::Judge` and score metrics match approved rubric IDs.
