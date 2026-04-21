# Init Agent: LLM Judge Rubric Flow — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the `autotune init` agent-driven workflow to help users design LLM judge rubrics through a guided interview, with per-rubric approval before finalizing the judge measure config.

**Architecture:** Two new top-level XML fragment types (`<rubric>` and `<rubrics-done></rubrics-done>`) are added to the agent protocol. The init lib accumulates approved rubrics in a `PendingJudgeMeasure` struct until `<rubrics-done>` signals finalization, at which point a complete `MeasureConfig` with `AdaptorConfig::Judge` is assembled. The init prompt documents the new schema and interview flow. No changes to the config crate or main binary.

**Tech Stack:** Rust 2024 edition, `quick-xml`, `autotune-agent` (protocol), `autotune-init` (lib, prompt, tests), `autotune-config` (existing `AdaptorConfig::Judge`, `RubricConfig`, `ScoreRangeConfig`).

---

## File Map

| File | Change |
|---|---|
| `crates/autotune-agent/src/protocol.rs` | Add `RubricProposal` struct; add `Rubric`/`RubricsDone` variants to `AgentFragment`; add `parse_rubric()`; add `parse_i32()`; extend `parse_adaptor()` with `"judge"` arm; extend `KNOWN` and `WRAPPER_TAGS`; add protocol-level tests |
| `crates/autotune-init/src/lib.rs` | Add `PendingJudgeMeasure`, `RubricOutcome`; extend `ConfigAccumulator`; update `is_complete()` and `missing_sections()`; add `show_rubric_proposal()`; update `validate_measure()`; extend fragment dispatch with `Rubric`/`RubricsDone` arms; update `Measure` arm for judge; update `is_protocol_tag_start()`; add imports; add unit tests |
| `crates/autotune-init/src/prompt.rs` | Document `<rubric>`, `<rubrics-done>`, judge `<measure>`, and the 5-step interview script |
| `crates/autotune-init/tests/init_test.rs` | End-to-end integration test for the full judge init flow |

---

## Background: Key Existing Types

**`AgentFragment`** (protocol.rs:197–218): enum with variants `Message`, `Question`, `Task`, `Paths`, `Test`, `Measure`, `Score`, `Agent`.

**`parse_agent_response`** (protocol.rs:231–276): collects all known top-level tags, sorts by byte offset, dispatches to per-tag parsers. The `KNOWN` constant at line 232 lists recognised tags.

**`ConfigAccumulator`** (lib.rs:33–41): struct with `task`, `paths`, `tests`, `measures`, `score`, `agent` fields. `is_complete()` at line 44 and `missing_sections()` at line 76.

**Fragment dispatch loop** (lib.rs:663–757): matches on `AgentFragment` variant, calls `validate_*`, pushes to accumulator.

**`validate_measure`** (lib.rs:162–204): validates a `MeasureConfig`, checks command, duplicate metric names.

**`UserInput`** (input.rs:48–63): `prompt_text`, `prompt_select`, `prompt_approve`. `MockInput` always returns first option key for `prompt_select`.

**`RubricConfig`** (autotune-config): `{ id, title, instruction, score_range: ScoreRangeConfig, guidance: Option<String> }`. `ScoreRangeConfig`: `{ min: i32, max: i32 }`.

---

## Task 1: Protocol layer — `RubricProposal`, new `AgentFragment` variants, judge adaptor

**Files:**
- Modify: `crates/autotune-agent/src/protocol.rs`

- [ ] **Step 1: Write failing tests**

Add to `mod tests` at the bottom of `crates/autotune-agent/src/protocol.rs`:

```rust
#[test]
fn parse_rubric_fragment() {
    let xml = r#"<rubric>
        <id>correctness</id>
        <title>Correctness</title>
        <instruction><![CDATA[Does the code produce correct results?]]></instruction>
        <score-range><min>1</min><max>5</max></score-range>
    </rubric>"#;
    let frags = parse_agent_response(xml).unwrap();
    assert_eq!(frags.len(), 1);
    match &frags[0] {
        AgentFragment::Rubric(r) => {
            assert_eq!(r.id, "correctness");
            assert_eq!(r.title, "Correctness");
            assert_eq!(r.instruction, "Does the code produce correct results?");
            assert_eq!(r.score_min, 1);
            assert_eq!(r.score_max, 5);
        }
        _ => panic!("expected Rubric"),
    }
}

#[test]
fn parse_rubric_missing_id_errors() {
    let xml = r#"<rubric><title>T</title><instruction>I</instruction><score-range><min>1</min><max>5</max></score-range></rubric>"#;
    let err = parse_agent_response(xml).unwrap_err();
    assert!(err.to_string().contains("missing <id>"), "error: {err}");
}

#[test]
fn parse_rubric_invalid_score_range_errors() {
    let xml = r#"<rubric><id>x</id><title>T</title><instruction>I</instruction><score-range><min>5</min><max>1</max></score-range></rubric>"#;
    let err = parse_agent_response(xml).unwrap_err();
    assert!(err.to_string().contains("score-range"), "error: {err}");
}

#[test]
fn parse_rubrics_done_fragment() {
    let xml = r#"<rubrics-done></rubrics-done>"#;
    let frags = parse_agent_response(xml).unwrap();
    assert_eq!(frags.len(), 1);
    assert!(matches!(frags[0], AgentFragment::RubricsDone));
}

#[test]
fn parse_judge_adaptor() {
    let xml = r#"<measure><name>quality</name><adaptor><type>judge</type><persona><![CDATA[A senior engineer]]></persona></adaptor></measure>"#;
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Measure(m) => {
            assert_eq!(m.name, "quality");
            assert!(m.command.is_none());
            match &m.adaptor {
                autotune_config::AdaptorConfig::Judge { persona, rubrics } => {
                    assert_eq!(persona, "A senior engineer");
                    assert!(rubrics.is_empty());
                }
                _ => panic!("expected Judge adaptor"),
            }
        }
        _ => panic!("expected Measure"),
    }
}

#[test]
fn parse_judge_adaptor_missing_persona_errors() {
    let xml = r#"<measure><name>q</name><adaptor><type>judge</type></adaptor></measure>"#;
    let err = parse_agent_response(xml).unwrap_err();
    assert!(err.to_string().contains("persona"), "error: {err}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p autotune-agent -E 'test(parse_rubric|parse_rubrics_done|parse_judge)'
```

Expected: compile error — `AgentFragment::Rubric`, `AgentFragment::RubricsDone`, `RubricProposal` do not exist yet.

- [ ] **Step 3: Add `RubricProposal` struct and new `AgentFragment` variants**

In `protocol.rs`, after line 217 (the `Agent(Box<AgentSectionConfig>)` variant), add to the `AgentFragment` enum:

```rust
/// A proposed rubric for the pending judge measure, shown to the user for approval.
Rubric(RubricProposal),
/// Signals the agent has finished proposing rubrics for the current judge measure.
RubricsDone,
```

Add the `RubricProposal` struct before the `AgentFragment` enum definition (around line 195):

```rust
/// A rubric proposed by the agent during the judge measure interview.
#[derive(Debug, Clone, PartialEq)]
pub struct RubricProposal {
    pub id: String,
    pub title: String,
    pub instruction: String,
    pub score_min: i32,
    pub score_max: i32,
}
```

- [ ] **Step 4: Add `parse_i32` helper**

Add after the `parse_f64` function (around line 914):

```rust
fn parse_i32(s: &str) -> Result<i32, AgentError> {
    s.trim().parse::<i32>().map_err(|e| AgentError::ParseFailed {
        message: format!("invalid integer '{s}': {e}"),
    })
}
```

- [ ] **Step 5: Add `parse_rubric` function**

Add after `parse_pattern` (around line 555):

```rust
fn parse_rubric(reader: &mut Reader<&[u8]>) -> Result<RubricProposal, AgentError> {
    let mut id = String::new();
    let mut title = String::new();
    let mut instruction = String::new();
    let mut score_min = 1i32;
    let mut score_max = 5i32;

    walk_children(reader, "rubric", |tag, reader| {
        match tag {
            "id" => id = read_text(reader, "id")?,
            "title" => title = read_text(reader, "title")?,
            "instruction" => instruction = read_text(reader, "instruction")?,
            "score-range" => {
                walk_children(reader, "score-range", |child, reader| {
                    match child {
                        "min" => score_min = parse_i32(&read_text(reader, "min")?)?,
                        "max" => score_max = parse_i32(&read_text(reader, "max")?)?,
                        other => skip_element(reader, other)?,
                    }
                    Ok(())
                })?;
            }
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    if id.is_empty() {
        return Err(AgentError::ParseFailed {
            message: "<rubric> missing <id>".to_string(),
        });
    }
    if score_min >= score_max {
        return Err(AgentError::ParseFailed {
            message: format!(
                "rubric '{id}': score-range min ({score_min}) must be less than max ({score_max})"
            ),
        });
    }

    Ok(RubricProposal {
        id,
        title,
        instruction,
        score_min,
        score_max,
    })
}
```

- [ ] **Step 6: Extend `parse_adaptor` with judge arm**

In `parse_adaptor` (line 500), add `persona` to the local variables (after `script_command`):

```rust
let mut persona: Option<String> = None;
```

Add `"persona"` arm to the `walk_children` closure (after the `"command"` arm):

```rust
"persona" => persona = Some(read_text(reader, "persona")?),
```

Add `"judge"` to the match at the end of `parse_adaptor` (after the `"script"` arm, before the `other` arm):

```rust
"judge" => {
    let persona = persona.ok_or_else(|| AgentError::ParseFailed {
        message: "adaptor type=judge requires <persona>".to_string(),
    })?;
    Ok(AdaptorConfig::Judge {
        persona,
        rubrics: vec![],
    })
}
```

- [ ] **Step 7: Extend `KNOWN` and match dispatch in `parse_agent_response`**

Change the `KNOWN` constant at line 232 to:

```rust
const KNOWN: &[&str] = &[
    "message", "question", "task", "paths", "test", "measure", "score", "agent",
    "rubric", "rubrics-done",
];
```

In the `for (_, tag, outer) in all` loop, add two new match arms after the `"agent"` arm (before `_ => unreachable!()`):

```rust
"rubric" => parse_fragment_strict(outer, "rubric", |r| {
    Ok(AgentFragment::Rubric(parse_rubric(r)?))
})?,
"rubrics-done" => parse_fragment_strict(outer, "rubrics-done", |r| {
    walk_children(r, "rubrics-done", |_, _| Ok(()))?;
    Ok(AgentFragment::RubricsDone)
})?,
```

- [ ] **Step 8: Update `WRAPPER_TAGS`**

Change line 108–110:

```rust
const WRAPPER_TAGS: &[&str] = &[
    "plan", "message", "question", "task", "paths", "test", "measure", "score", "agent",
    "rubric", "rubrics-done",
];
```

- [ ] **Step 9: Run tests to verify they pass**

```bash
cargo nextest run -p autotune-agent -E 'test(parse_rubric|parse_rubrics_done|parse_judge)'
```

Expected: all 6 new tests pass.

- [ ] **Step 10: Run full autotune-agent suite to check for regressions**

```bash
cargo nextest run -p autotune-agent
```

Expected: all tests pass.

- [ ] **Step 11: Commit**

```bash
git add crates/autotune-agent/src/protocol.rs
git commit -m "feat(protocol): add Rubric/RubricsDone fragments and judge adaptor parsing"
```

---

## Task 2: Accumulator state and fragment dispatch in lib.rs

**Files:**
- Modify: `crates/autotune-init/src/lib.rs`

- [ ] **Step 1: Write failing unit tests**

Add to the `mod tests` block at the bottom of `lib.rs` (after the existing tests):

```rust
fn judge_measure_config(persona: &str) -> MeasureConfig {
    MeasureConfig {
        name: "quality".to_string(),
        command: None,
        timeout: 600,
        adaptor: AdaptorConfig::Judge {
            persona: persona.to_string(),
            rubrics: vec![],
        },
    }
}

fn minimal_rubric_proposal(id: &str) -> autotune_agent::protocol::RubricProposal {
    autotune_agent::protocol::RubricProposal {
        id: id.to_string(),
        title: format!("{id} title"),
        instruction: format!("{id} instruction"),
        score_min: 1,
        score_max: 5,
    }
}

#[test]
fn judge_measure_creates_pending_state() {
    let mut acc = ConfigAccumulator::default();
    let measure = judge_measure_config("A senior engineer");
    match validate_measure(&measure, &acc) {
        FragmentOutcome::Accepted(_) => {
            acc.pending_judge = Some(PendingJudgeMeasure {
                name: measure.name.clone(),
                persona: "A senior engineer".to_string(),
                command: None,
                approved_rubrics: vec![],
            });
        }
        FragmentOutcome::Rejected(e) => panic!("unexpected rejection: {e}"),
    }
    assert!(acc.pending_judge.is_some());
    assert!(acc.measures.is_empty());
}

#[test]
fn unfinalized_judge_blocks_completion() {
    let mut acc = complete_accumulator();
    acc.measures.clear();
    acc.pending_judge = Some(PendingJudgeMeasure {
        name: "quality".to_string(),
        persona: "reviewer".to_string(),
        command: None,
        approved_rubrics: vec![autotune_config::RubricConfig {
            id: "correctness".to_string(),
            title: "Correctness".to_string(),
            instruction: "Is it correct?".to_string(),
            score_range: autotune_config::ScoreRangeConfig { min: 1, max: 5 },
            guidance: None,
        }],
    });
    assert!(!acc.is_complete());
    assert!(acc.missing_sections().iter().any(|s| s.contains("rubrics-done")));
}

#[test]
fn pending_judge_missing_section_hides_generic_measure_missing() {
    let mut acc = ConfigAccumulator {
        task: Some(minimal_task()),
        paths: Some(minimal_paths()),
        ..Default::default()
    };
    acc.pending_judge = Some(PendingJudgeMeasure {
        name: "q".to_string(),
        persona: "p".to_string(),
        command: None,
        approved_rubrics: vec![],
    });
    let missing = acc.missing_sections();
    assert!(!missing.contains(&"measure (at least one)"),
        "should not show generic measure missing when judge is pending: {missing:?}");
    assert!(missing.iter().any(|s| s.contains("rubrics-done")));
}

#[test]
fn rubrics_done_finalizes_measure() {
    let rubric = autotune_config::RubricConfig {
        id: "correctness".to_string(),
        title: "Correctness".to_string(),
        instruction: "Is it correct?".to_string(),
        score_range: autotune_config::ScoreRangeConfig { min: 1, max: 5 },
        guidance: None,
    };
    let mut acc = ConfigAccumulator::default();
    acc.pending_judge = Some(PendingJudgeMeasure {
        name: "quality".to_string(),
        persona: "A senior engineer".to_string(),
        command: None,
        approved_rubrics: vec![rubric],
    });
    // Simulate RubricsDone processing
    let pending = acc.pending_judge.take().unwrap();
    let assembled = MeasureConfig {
        name: pending.name,
        command: pending.command,
        timeout: 600,
        adaptor: AdaptorConfig::Judge {
            persona: pending.persona,
            rubrics: pending.approved_rubrics,
        },
    };
    acc.measures.push(assembled);

    assert!(acc.pending_judge.is_none());
    assert_eq!(acc.measures.len(), 1);
    match &acc.measures[0].adaptor {
        AdaptorConfig::Judge { persona, rubrics } => {
            assert_eq!(persona, "A senior engineer");
            assert_eq!(rubrics.len(), 1);
            assert_eq!(rubrics[0].id, "correctness");
        }
        _ => panic!("expected Judge adaptor"),
    }
}

#[test]
fn validate_measure_rejects_second_judge_when_pending() {
    let mut acc = ConfigAccumulator::default();
    acc.pending_judge = Some(PendingJudgeMeasure {
        name: "existing".to_string(),
        persona: "p".to_string(),
        command: None,
        approved_rubrics: vec![],
    });
    let measure = judge_measure_config("Another persona");
    match validate_measure(&measure, &acc) {
        FragmentOutcome::Rejected(msg) => {
            assert!(msg.contains("already pending"), "msg: {msg}");
        }
        FragmentOutcome::Accepted(_) => panic!("should have rejected second judge measure"),
    }
}

#[test]
fn show_rubric_proposal_accept_path() {
    use autotune_agent::protocol::QuestionOption;
    // MockInput returns first option key = "accept"
    let input = MockInput::new("yes");
    let rubric = minimal_rubric_proposal("correctness");
    let outcome = show_rubric_proposal(&rubric, &input).unwrap();
    assert!(matches!(outcome, RubricOutcome::Accepted));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p autotune-init -E 'test(judge|rubrics_done|pending_judge|show_rubric)'
```

Expected: compile error — `PendingJudgeMeasure`, `RubricOutcome`, `show_rubric_proposal` do not exist yet.

- [ ] **Step 3: Add imports and new types to `lib.rs`**

At the top of `lib.rs`, change the `autotune_agent::protocol` import (line 9):

```rust
use autotune_agent::protocol::{AgentFragment, RubricProposal, parse_agent_response};
```

Change the `autotune_config` import (lines 15–17):

```rust
use autotune_config::{
    AdaptorConfig, AutotuneConfig, MeasureConfig, PathsConfig, RubricConfig, ScoreConfig,
    ScoreRangeConfig, TaskConfig, TestConfig,
};
```

Add the new types after the `FragmentOutcome` enum (around line 118):

```rust
/// State accumulated while the agent interviews the user about judge rubrics.
struct PendingJudgeMeasure {
    name: String,
    persona: String,
    command: Option<Vec<String>>,
    approved_rubrics: Vec<RubricConfig>,
}

/// Outcome of showing a rubric proposal to the user.
enum RubricOutcome {
    Accepted,
    Rejected,
    Modified(String),
}
```

- [ ] **Step 4: Add `pending_judge` to `ConfigAccumulator` and update `is_complete` / `missing_sections`**

Change `ConfigAccumulator` (lines 33–41):

```rust
#[derive(Default)]
struct ConfigAccumulator {
    task: Option<TaskConfig>,
    paths: Option<PathsConfig>,
    tests: Vec<TestConfig>,
    measures: Vec<MeasureConfig>,
    score: Option<ScoreConfig>,
    agent: Option<autotune_config::AgentConfig>,
    pending_judge: Option<PendingJudgeMeasure>,
}
```

Note: remove the `Clone` derive since `PendingJudgeMeasure` is not `Clone`. Update `clone_assemble` (line 61) to work without cloning the whole struct — it already only uses `task`, `paths`, `measures`, `score`, `agent` which are all `Clone`. No change needed to `clone_assemble`.

Actually, `ConfigAccumulator` used to `#[derive(Clone, Default)]` and `clone_assemble` calls `self.clone_assemble()` which does a field-by-field clone. Since `PendingJudgeMeasure` doesn't derive `Clone`, we need to either implement `Clone` manually for `ConfigAccumulator` or remove the derive and implement it.

Simplest fix: remove `Clone` from the derive, implement `Clone` manually for `ConfigAccumulator`, cloning only the fields that matter (pending_judge doesn't need to be cloned for the preview):

```rust
#[derive(Default)]
struct ConfigAccumulator {
    task: Option<TaskConfig>,
    paths: Option<PathsConfig>,
    tests: Vec<TestConfig>,
    measures: Vec<MeasureConfig>,
    score: Option<ScoreConfig>,
    agent: Option<autotune_config::AgentConfig>,
    pending_judge: Option<PendingJudgeMeasure>,
}

impl Clone for ConfigAccumulator {
    fn clone(&self) -> Self {
        ConfigAccumulator {
            task: self.task.clone(),
            paths: self.paths.clone(),
            tests: self.tests.clone(),
            measures: self.measures.clone(),
            score: self.score.clone(),
            agent: self.agent.clone(),
            pending_judge: None, // pending rubrics are not part of the preview
        }
    }
}
```

Change `is_complete` (lines 44–49):

```rust
fn is_complete(&self) -> bool {
    self.task.is_some()
        && self.paths.is_some()
        && !self.measures.is_empty()
        && self.score.is_some()
        && self.pending_judge.is_none()
}
```

Change `missing_sections` (lines 76–91):

```rust
fn missing_sections(&self) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if self.task.is_none() {
        missing.push("task");
    }
    if self.paths.is_none() {
        missing.push("paths");
    }
    if self.pending_judge.is_some() {
        missing.push("judge rubrics (use <rubrics-done></rubrics-done> to finalize)");
    } else if self.measures.is_empty() {
        missing.push("measure (at least one)");
    }
    if self.score.is_none() {
        missing.push("score");
    }
    missing
}
```

- [ ] **Step 5: Add `show_rubric_proposal` function**

Add after the `map_io` function (around line 366):

```rust
fn show_rubric_proposal(
    rubric: &RubricProposal,
    user_input: &dyn UserInput,
) -> Result<RubricOutcome, InitError> {
    use autotune_agent::protocol::QuestionOption;
    println!("[autotune] Proposed rubric:");
    println!("  ID:          {}", rubric.id);
    println!("  Title:       {}", rubric.title);
    println!("  Instruction: {}", rubric.instruction);
    println!("  Score range: {}–{}", rubric.score_min, rubric.score_max);

    let options = vec![
        QuestionOption {
            key: "accept".to_string(),
            label: "Accept".to_string(),
            description: None,
        },
        QuestionOption {
            key: "reject".to_string(),
            label: "Reject".to_string(),
            description: None,
        },
        QuestionOption {
            key: "modify".to_string(),
            label: "Modify (enter new instruction)".to_string(),
            description: None,
        },
    ];

    let choice = user_input
        .prompt_select("", &options, false)
        .map_err(map_io)?;

    match choice.as_str() {
        "reject" => Ok(RubricOutcome::Rejected),
        "modify" => {
            let new_instruction = user_input
                .prompt_text("Enter new instruction:")
                .map_err(map_io)?;
            Ok(RubricOutcome::Modified(new_instruction))
        }
        _ => Ok(RubricOutcome::Accepted),
    }
}
```

- [ ] **Step 6: Update `validate_measure` to reject a second pending judge**

In `validate_measure` (line 162), inside the `AdaptorConfig::Judge { .. }` match arm, add a conflict check at the top:

```rust
autotune_config::AdaptorConfig::Judge { .. } => {
    if acc.pending_judge.is_some() {
        return FragmentOutcome::Rejected(
            "a judge measure is already pending; finalize it with \
             <rubrics-done></rubrics-done> before adding another"
                .to_string(),
        );
    }
    if let Some(cmd) = &measure.command
        && cmd.is_empty()
    {
        return FragmentOutcome::Rejected(format!(
            "measure '{}' has empty command",
            measure.name
        ));
    }
}
```

- [ ] **Step 7: Extend fragment dispatch with `Rubric` and `RubricsDone` arms; update `Measure` arm**

In the `for frag in fragments` loop (line 663), update the `AgentFragment::Measure` arm to route judge measures to `pending_judge` instead of `measures`:

```rust
AgentFragment::Measure(measure) => match validate_measure(&measure, &acc) {
    FragmentOutcome::Accepted(msg) => {
        println!("[autotune] {msg}");
        ack_lines.push(msg);
        if matches!(&measure.adaptor, AdaptorConfig::Judge { .. }) {
            let persona = match &measure.adaptor {
                AdaptorConfig::Judge { persona, .. } => persona.clone(),
                _ => unreachable!(),
            };
            acc.pending_judge = Some(PendingJudgeMeasure {
                name: measure.name.clone(),
                persona,
                command: measure.command.clone(),
                approved_rubrics: vec![],
            });
        } else {
            acc.measures.push(measure);
        }
    }
    FragmentOutcome::Rejected(err) => {
        println!("[autotune] validation error: {err}");
        rejection_lines.push(format!("measure: {err}"));
    }
},
```

Add the `Rubric` arm after the `Measure` arm:

```rust
AgentFragment::Rubric(rubric) => {
    let error = if acc.pending_judge.is_none() {
        Some("rubric: no active judge measure — emit <measure> with <adaptor><type>judge</type> first".to_string())
    } else if acc
        .pending_judge
        .as_ref()
        .unwrap()
        .approved_rubrics
        .iter()
        .any(|r| r.id == rubric.id)
    {
        Some(format!(
            "rubric: duplicate id '{}' — rubric ids must be unique within a judge measure",
            rubric.id
        ))
    } else {
        None
    };

    if let Some(err) = error {
        println!("[autotune] validation error: {err}");
        rejection_lines.push(err);
    } else {
        match show_rubric_proposal(&rubric, user_input)? {
            RubricOutcome::Accepted => {
                let pending = acc.pending_judge.as_mut().unwrap();
                pending.approved_rubrics.push(RubricConfig {
                    id: rubric.id.clone(),
                    title: rubric.title.clone(),
                    instruction: rubric.instruction.clone(),
                    score_range: ScoreRangeConfig {
                        min: rubric.score_min,
                        max: rubric.score_max,
                    },
                    guidance: None,
                });
                ack_lines.push(format!(
                    "Rubric '{}' ({}–{}): accepted.",
                    rubric.id, rubric.score_min, rubric.score_max
                ));
            }
            RubricOutcome::Rejected => {
                ack_lines.push(format!("Rubric '{}': rejected by user.", rubric.id));
            }
            RubricOutcome::Modified(new_instruction) => {
                let pending = acc.pending_judge.as_mut().unwrap();
                pending.approved_rubrics.push(RubricConfig {
                    id: rubric.id.clone(),
                    title: rubric.title.clone(),
                    instruction: new_instruction.clone(),
                    score_range: ScoreRangeConfig {
                        min: rubric.score_min,
                        max: rubric.score_max,
                    },
                    guidance: None,
                });
                ack_lines.push(format!(
                    "Rubric '{}' accepted with modified instruction: '{new_instruction}'.",
                    rubric.id
                ));
            }
        }
    }
}
```

Add the `RubricsDone` arm after the `Rubric` arm:

```rust
AgentFragment::RubricsDone => {
    let error = if acc.pending_judge.is_none() {
        Some("rubrics-done: no active judge measure".to_string())
    } else if acc
        .pending_judge
        .as_ref()
        .unwrap()
        .approved_rubrics
        .is_empty()
    {
        Some(
            "rubrics-done: at least one rubric must be approved before finalizing"
                .to_string(),
        )
    } else {
        None
    };

    if let Some(err) = error {
        println!("[autotune] validation error: {err}");
        rejection_lines.push(err);
    } else {
        let pending = acc.pending_judge.take().unwrap();
        let n = pending.approved_rubrics.len();
        let measure = MeasureConfig {
            name: pending.name,
            command: pending.command,
            timeout: 600,
            adaptor: AdaptorConfig::Judge {
                persona: pending.persona,
                rubrics: pending.approved_rubrics,
            },
        };
        acc.measures.push(measure);
        ack_lines.push(format!("Judge measure finalized with {n} rubric(s)."));
    }
}
```

- [ ] **Step 8: Update `is_protocol_tag_start`**

Change the `TAGS` constant (around line 279):

```rust
const TAGS: &[&str] = &[
    "<message",
    "<question",
    "<task",
    "<paths",
    "<test",
    "<measure",
    "<score",
    "<agent",
    "<rubric",
    "<rubrics-done",
];
```

- [ ] **Step 9: Run the new unit tests**

```bash
cargo nextest run -p autotune-init -E 'test(judge|rubrics_done|pending_judge|show_rubric)'
```

Expected: all 7 new tests pass.

- [ ] **Step 10: Run full autotune-init test suite**

```bash
cargo nextest run -p autotune-init
```

Expected: all tests pass.

- [ ] **Step 11: Commit**

```bash
git add crates/autotune-init/src/lib.rs
git commit -m "feat(init): add PendingJudgeMeasure accumulator and Rubric/RubricsDone dispatch"
```

---

## Task 3: Update init prompt with judge/rubric documentation

**Files:**
- Modify: `crates/autotune-init/src/prompt.rs`

- [ ] **Step 1: Write a failing prompt-content test**

Add to `crates/autotune-init/tests/prompt_test.rs` (or create the file if it doesn't exist; check first with `ls crates/autotune-init/tests/`):

```rust
use autotune_init::build_init_prompt;
use std::path::Path;

#[test]
fn prompt_documents_judge_adaptor() {
    let prompt = build_init_prompt(Path::new("/repo"));
    assert!(prompt.contains("judge"), "prompt should mention judge adaptor");
    assert!(prompt.contains("<rubric>"), "prompt should document <rubric> fragment");
    assert!(prompt.contains("rubrics-done"), "prompt should document rubrics-done");
    assert!(prompt.contains("<persona>"), "prompt should document persona field");
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo nextest run -p autotune-init -E 'test(prompt_documents_judge)'
```

Expected: FAIL — prompt doesn't mention judge yet.

- [ ] **Step 3: Extend the `<measure>` section in `prompt.rs` with judge adaptor**

In `prompt.rs`, find the `<measure>` adaptor documentation (around line 128–131 which lists `regex`, `criterion`, `script`). Add the judge adaptor after the script entry:

```
  - `<type>judge</type>` + `<persona>` for an LLM-based rubric judge. The measure `<command>` is optional (if present, its stdout/stderr are passed to the judge as context). **Do not include rubrics in the `<adaptor>` — propose them via separate `<rubric>` fragments (see below).**
```

The full updated adaptor bullet list in the `<measure>` section:

```
- `<adaptor>`: how to extract metrics from the command output.
  - `<type>regex</type>` + one or more `<pattern>` children, each with `<name>` and `<regex>` (the regex must have one capture group; wrap in CDATA).
  - `<type>criterion</type>` + `<measure-name>` to parse `cargo bench` / criterion output.
  - `<type>script</type>` + `<command><segment>...</segment>...</command>` to pipe measure output through an external script that prints `metric_name=value` lines.
  - `<type>judge</type>` + `<persona>` for an LLM-based rubric evaluator. The measure `<command>` is optional (stdout/stderr become judge context when present). Do NOT put rubrics in the `<adaptor>` — propose them via `<rubric>` fragments after the measure is accepted (see "Judge Rubric Design" below).
```

- [ ] **Step 4: Add `<rubric>` and `<rubrics-done>` fragment documentation**

After the `<agent>` section (around line 162) and before the `## How the conversation flows` section, add:

```
#### `<rubric>` — propose one rubric for the pending judge measure

Only emit after a `<measure type="judge">` has been accepted. Propose one rubric at a time. The CLI shows it to the user and collects Accept / Reject / Modify. Wait for CLI feedback before proposing the next rubric.

```xml
<rubric>
  <id>correctness</id>
  <title>Correctness</title>
  <instruction><![CDATA[Does the implementation produce correct results for all valid inputs, including edge cases?]]></instruction>
  <score-range><min>1</min><max>5</max></score-range>
</rubric>
```

- `<id>`: short snake_case identifier — becomes the metric name in scoring (required).
- `<title>`: human-readable label (required).
- `<instruction>`: what the evaluator assesses — be specific and measurable (required, use CDATA).
- `<score-range>`: integer min and max (required; min must be less than max).

#### `<rubrics-done>` — finalize the pending judge measure

After the user is satisfied with the proposed rubrics, emit:
```xml
<rubrics-done></rubrics-done>
```
The CLI assembles the judge measure from all approved rubrics and adds it to the config. Emit `<score>` immediately after, using only the approved rubric IDs (the CLI reports which were accepted and which were rejected).
```

- [ ] **Step 5: Add the Judge Rubric Design conversation section**

Inside `## How the conversation flows`, add a new numbered item at the end (before the final `## Critical rules` section):

```
7. **If the user wants LLM judge evaluation, follow this 5-step rubric interview:**
   1. **Interview** — Emit a `<question>` asking which quality dimensions matter for their codebase (allow free response). Examples: correctness, performance, readability, safety, API ergonomics.
   2. **Emit judge measure header** — Emit `<measure>` with `<adaptor><type>judge</type><persona>...</persona></adaptor>` and an appropriate `<name>`. Use the user's goal to craft the persona. Do NOT include rubrics here.
   3. **Propose rubrics one at a time** — For each dimension identified, emit one `<rubric>` and wait for CLI feedback (the feedback line begins with "Rubric '...'"). Propose 3–5 rubrics total. If the user modifies an instruction, incorporate the change into subsequent rubrics if relevant.
   4. **Check satisfaction** — After proposing all rubrics, emit a `<question>`:
      ```xml
      <question>
        <text>Are these rubrics sufficient or would you like to add more dimensions?</text>
        <option><key>finalize</key><label>These look good, finalize</label></option>
        <option><key>more</key><label>Add more dimensions</label></option>
      </question>
      ```
   5. **Finalize** — If the user chooses finalize, emit `<rubrics-done></rubrics-done>` followed immediately by `<score>` listing only the approved rubric IDs (use only IDs reported as "accepted" or "modified" in the CLI feedback — skip rejected ones).
```

- [ ] **Step 6: Run the prompt test**

```bash
cargo nextest run -p autotune-init -E 'test(prompt_documents_judge)'
```

Expected: PASS.

- [ ] **Step 7: Run full init test suite**

```bash
cargo nextest run -p autotune-init
```

Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/autotune-init/src/prompt.rs crates/autotune-init/tests/prompt_test.rs
git commit -m "feat(init): document judge adaptor, rubric, and rubrics-done in init prompt"
```

---

## Task 4: End-to-end integration test for full judge init flow

**Files:**
- Modify: `crates/autotune-init/tests/init_test.rs`

- [ ] **Step 1: Write the failing integration test**

Add to `crates/autotune-init/tests/init_test.rs`:

```rust
/// A UserInput that pops responses from a queue, falling back to a default.
/// Needed for tests that go through multiple prompt_select / prompt_approve calls.
struct ScriptedInput {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    fallback: String,
}

impl ScriptedInput {
    fn new(responses: &[&str], fallback: &str) -> Self {
        ScriptedInput {
            responses: std::sync::Mutex::new(
                responses.iter().map(|s| s.to_string()).collect(),
            ),
            fallback: fallback.to_string(),
        }
    }
}

impl autotune_init::UserInput for ScriptedInput {
    fn prompt_text(&self, _: &str) -> Result<String, std::io::Error> {
        let mut q = self.responses.lock().unwrap();
        Ok(q.pop_front().unwrap_or_else(|| self.fallback.clone()))
    }

    fn prompt_select(
        &self,
        _: &str,
        _: &[autotune_agent::protocol::QuestionOption],
        _: bool,
    ) -> Result<String, std::io::Error> {
        let mut q = self.responses.lock().unwrap();
        Ok(q.pop_front().unwrap_or_else(|| self.fallback.clone()))
    }

    fn prompt_approve(&self, _: &str) -> Result<bool, std::io::Error> {
        let mut q = self.responses.lock().unwrap();
        let r = q.pop_front().unwrap_or_else(|| self.fallback.clone());
        Ok(r == "yes" || r == "y")
    }
}

#[test]
fn run_init_full_judge_flow() {
    // Agent turn 1 (spawn/init_response): emits judge measure header.
    // Agent turn 2 (first send): proposes correctness rubric.
    // Agent turn 3 (second send): proposes readability rubric.
    // Agent turn 4 (third send): rubrics-done + remaining config sections.

    let agent = MockAgent::builder()
        .init_response(
            r#"<measure>
  <name>code-quality</name>
  <adaptor>
    <type>judge</type>
    <persona><![CDATA[A senior Rust engineer who values correctness and clarity]]></persona>
  </adaptor>
</measure>"#,
        )
        .research_response(
            r#"<rubric>
  <id>correctness</id>
  <title>Correctness</title>
  <instruction><![CDATA[Does the implementation produce correct results for all inputs?]]></instruction>
  <score-range><min>1</min><max>5</max></score-range>
</rubric>"#,
        )
        .research_response(
            r#"<rubric>
  <id>readability</id>
  <title>Readability</title>
  <instruction><![CDATA[Is the code idiomatic and easy to follow?]]></instruction>
  <score-range><min>1</min><max>5</max></score-range>
</rubric>"#,
        )
        .research_response(
            r#"<rubrics-done></rubrics-done>
<task>
  <name>quality-task</name>
  <max-iterations>10</max-iterations>
</task>
<paths>
  <tunable>src/**</tunable>
</paths>
<score>
  <type>weighted_sum</type>
  <primary-metric>
    <name>correctness</name>
    <direction>Maximize</direction>
    <weight>1.0</weight>
  </primary-metric>
  <primary-metric>
    <name>readability</name>
    <direction>Maximize</direction>
    <weight>1.0</weight>
  </primary-metric>
</score>"#,
        )
        .build();

    // Responses in order:
    //   - "accept" for correctness rubric (prompt_select in show_rubric_proposal)
    //   - "accept" for readability rubric (prompt_select in show_rubric_proposal)
    //   - "yes" for config approval (prompt_approve)
    let input = ScriptedInput::new(&["accept", "accept", "yes"], "yes");

    let global = GlobalConfig::default();
    let result = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        &input,
        None,
    )
    .unwrap();

    assert_eq!(result.config.task.name, "quality-task");
    assert_eq!(result.config.measure.len(), 1);
    let measure = &result.config.measure[0];
    assert_eq!(measure.name, "code-quality");
    match &measure.adaptor {
        autotune_config::AdaptorConfig::Judge { persona, rubrics } => {
            assert!(persona.contains("Rust engineer"));
            assert_eq!(rubrics.len(), 2);
            assert_eq!(rubrics[0].id, "correctness");
            assert_eq!(rubrics[1].id, "readability");
        }
        _ => panic!("expected Judge adaptor, got {:?}", measure.adaptor),
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo nextest run -p autotune-init -E 'test(run_init_full_judge_flow)'
```

Expected: compile error — `ScriptedInput` doesn't implement `UserInput` yet (or it does, but the test logic fails). Fix any compile issues, then confirm the test fails at runtime if needed.

- [ ] **Step 3: Run the test to verify it passes**

After fixing any compile issues:

```bash
cargo nextest run -p autotune-init -E 'test(run_init_full_judge_flow)'
```

Expected: PASS.

- [ ] **Step 4: Run full suite**

```bash
cargo nextest run
```

Expected: all tests pass (PTY tests may be flaky — that's pre-existing).

- [ ] **Step 5: Run pre-commit checks**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run
```

Expected: no errors, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-init/tests/init_test.rs
git commit -m "test(init): end-to-end integration test for judge rubric init flow"
```
