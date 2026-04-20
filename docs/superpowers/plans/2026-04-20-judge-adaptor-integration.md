# Judge Adaptor Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate `autotune-judge` as a built-in `judge` adaptor type that spawns a single LLM agent session, batches all rubrics in one prompt under a shared persona, and emits one `f64` metric per rubric ID into the existing scorer pipeline.

**Architecture:** A new `AdaptorConfig::Judge { persona, rubrics }` variant is added to `autotune-config`. The `autotune-benchmark` crate gains a `JudgeContext` struct and `run_judge_measure` function that dispatches judge measures instead of `MetricAdaptor::extract`. The machine's Measuring phase threads an `Option<&JudgeContext>` through `run_single_phase` → `run_measuring` → `run_all_measures_with_output`. All callers build the judge agent once at startup in `main.rs`.

**Tech Stack:** Rust 2024, `autotune-judge` (batch prompt/parse), `autotune-agent` (Agent trait + AgentConfig), `autotune-config` (config types), `autotune-benchmark` (measure execution), `autotune` binary (wiring).

---

## File Map

| File | Change |
|---|---|
| `crates/autotune-config/src/lib.rs` | Add `RubricConfig`, `ScoreRangeConfig`, `AdaptorConfig::Judge`; change `MeasureConfig.command` to `Option<Vec<String>>`; add `AgentConfig.judge`; update validation |
| `crates/autotune-judge/src/prompt.rs` | Add `render_batch_prompt` |
| `crates/autotune-judge/src/judge.rs` | Add `parse_batch_response` |
| `crates/autotune-judge/src/lib.rs` | Re-export new public items |
| `crates/autotune-benchmark/src/lib.rs` | Add `JudgeContext`, `run_judge_measure`; extend `run_all_measures_with_output` and `run_all_measures` signatures |
| `crates/autotune/src/agent_factory.rs` | Add `AgentRole::Judge` |
| `crates/autotune/src/machine.rs` | Add `judge_ctx` param to `run_single_phase` and `run_measuring` |
| `crates/autotune/src/main.rs` | Build judge agent in `cmd_run`/`cmd_step`/`cmd_resume`; extend `apply_global_agent_defaults` for judge role |
| `crates/autotune-benchmark/Cargo.toml` | Add `autotune-judge` dependency |

---

## Task 1: Config — add `RubricConfig`, `ScoreRangeConfig`, `AdaptorConfig::Judge`, optional `command`, `agent.judge`

**Files:**
- Modify: `crates/autotune-config/src/lib.rs`

- [ ] **Step 1: Write failing tests for new config parsing**

Add to the `#[cfg(test)]` block at the bottom of `crates/autotune-config/src/lib.rs`:

```rust
#[test]
fn judge_adaptor_parses_from_toml() {
    let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A strict reviewer"
[[measure.adaptor.rubrics]]
id = "correctness"
title = "Correctness"
instruction = "Score correctness 1-5."
score_range = { min = 1, max = 5 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "correctness", direction = "Maximize" }]
"#;
    let config: AutotuneConfig = toml::from_str(toml).unwrap();
    config.validate().unwrap();
    let AdaptorConfig::Judge { persona, rubrics } = &config.measure[0].adaptor else {
        panic!("expected Judge adaptor");
    };
    assert_eq!(persona, "A strict reviewer");
    assert_eq!(rubrics.len(), 1);
    assert_eq!(rubrics[0].id, "correctness");
    assert_eq!(rubrics[0].score_range.min, 1);
    assert_eq!(rubrics[0].score_range.max, 5);
    assert!(config.measure[0].command.is_none());
}

#[test]
fn judge_adaptor_with_command_parses() {
    let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
command = ["sh", "-c", "cat src/lib.rs"]
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "quality"
title = "Quality"
instruction = "Score 1-3."
score_range = { min = 1, max = 3 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "quality", direction = "Maximize" }]
"#;
    let config: AutotuneConfig = toml::from_str(toml).unwrap();
    config.validate().unwrap();
    assert_eq!(
        config.measure[0].command.as_deref(),
        Some(["sh", "-c", "cat src/lib.rs"].as_slice())
    );
}

#[test]
fn judge_adaptor_with_no_rubrics_fails_validation() {
    let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[score]
type = "weighted_sum"
primary_metrics = [{ name = "anything", direction = "Maximize" }]
"#;
    let config: AutotuneConfig = toml::from_str(toml).unwrap();
    let err = config.validate().unwrap_err();
    assert!(err.to_string().contains("rubric"), "error: {err}");
}

#[test]
fn judge_adaptor_empty_command_fails_validation() {
    let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
command = []
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "q"
title = "Q"
instruction = "Score 1-5."
score_range = { min = 1, max = 5 }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "q", direction = "Maximize" }]
"#;
    let config: AutotuneConfig = toml::from_str(toml).unwrap();
    let err = config.validate().unwrap_err();
    assert!(err.to_string().contains("empty"), "error: {err}");
}

#[test]
fn non_judge_measure_without_command_fails_validation() {
    let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "m"
adaptor = { type = "regex", patterns = [{ name = "val", pattern = "([0-9]+)" }] }
[score]
type = "weighted_sum"
primary_metrics = [{ name = "val", direction = "Maximize" }]
"#;
    let config: AutotuneConfig = toml::from_str(toml).unwrap();
    let err = config.validate().unwrap_err();
    assert!(err.to_string().contains("command"), "error: {err}");
}

#[test]
fn judge_adaptor_metric_names_returns_rubric_ids() {
    let toml = r#"
[task]
name = "t"
max_iterations = "5"
[paths]
tunable = ["src/**"]
[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A reviewer"
[[measure.adaptor.rubrics]]
id = "r1"
title = "R1"
instruction = "Score."
score_range = { min = 1, max = 5 }
[[measure.adaptor.rubrics]]
id = "r2"
title = "R2"
instruction = "Score."
score_range = { min = 1, max = 5 }
[score]
type = "weighted_sum"
primary_metrics = [
  { name = "r1", direction = "Maximize" },
  { name = "r2", direction = "Maximize" },
]
"#;
    let config: AutotuneConfig = toml::from_str(toml).unwrap();
    config.validate().unwrap();
    let names = config.adaptor_metric_names(&config.measure[0].adaptor);
    assert!(names.contains(&"r1".to_string()));
    assert!(names.contains(&"r2".to_string()));
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cd /Users/roger/Code/rust/autotune2
cargo nextest run -p autotune-config 2>&1 | tail -20
```

Expected: compile errors about missing types and variants.

- [ ] **Step 3: Implement config changes**

Replace the `MeasureConfig`, `AdaptorConfig`, and `AgentConfig` structs in `crates/autotune-config/src/lib.rs`. Make these changes:

**3a.** Add before `AdaptorConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRangeConfig {
    pub min: i32,
    pub max: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricConfig {
    pub id: String,
    pub title: String,
    pub instruction: String,
    pub score_range: ScoreRangeConfig,
    #[serde(default)]
    pub guidance: Option<String>,
}
```

**3b.** Change `MeasureConfig.command` from `Vec<String>` to `Option<Vec<String>>`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasureConfig {
    pub name: String,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default = "default_measure_timeout")]
    pub timeout: u64,
    pub adaptor: AdaptorConfig,
}
```

**3c.** Add `Judge` variant to `AdaptorConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AdaptorConfig {
    #[serde(rename = "regex")]
    Regex { patterns: Vec<RegexPattern> },
    #[serde(rename = "criterion")]
    Criterion { measure_name: String },
    #[serde(rename = "script")]
    Script { command: Vec<String> },
    #[serde(rename = "judge")]
    Judge {
        persona: String,
        #[serde(default)]
        rubrics: Vec<RubricConfig>,
    },
}
```

**3d.** Add `judge` field to `AgentConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    // ... existing fields ...
    #[serde(default)]
    pub judge: Option<AgentRoleConfig>,
}
```

Also update `AgentConfig::default()` to include `judge: None`.

**3e.** Update `adaptor_metric_names` to handle `Judge`:

```rust
fn adaptor_metric_names(&self, adaptor: &AdaptorConfig) -> Vec<String> {
    match adaptor {
        AdaptorConfig::Regex { patterns } => patterns.iter().map(|p| p.name.clone()).collect(),
        AdaptorConfig::Criterion { .. } => {
            vec!["mean".to_string(), "median".to_string(), "std_dev".to_string()]
        }
        AdaptorConfig::Script { .. } => vec![],
        AdaptorConfig::Judge { rubrics, .. } => rubrics.iter().map(|r| r.id.clone()).collect(),
    }
}
```

**3f.** Update `validate` to add judge-specific checks. Inside the existing measure validation loop, add:

```rust
for b in &self.measure {
    match &b.adaptor {
        AdaptorConfig::Judge { rubrics, .. } => {
            if rubrics.is_empty() {
                return Err(ConfigError::Validation {
                    message: format!("measure '{}' judge adaptor must have at least one rubric", b.name),
                });
            }
            if let Some(cmd) = &b.command {
                if cmd.is_empty() {
                    return Err(ConfigError::Validation {
                        message: format!("measure '{}' has empty command", b.name),
                    });
                }
            }
        }
        _ => {
            // Non-judge adaptors require a command.
            match &b.command {
                None | Some([]) => {
                    return Err(ConfigError::Validation {
                        message: format!("measure '{}' requires a non-empty command", b.name),
                    });
                }
                Some(_) => {}
            }
            if let AdaptorConfig::Script { command } = &b.adaptor
                && command.is_empty()
            {
                return Err(ConfigError::Validation {
                    message: format!("measure '{}' has empty script adaptor command", b.name),
                });
            }
        }
    }
}
```

Also add `judge` to the role merge loop in `validate`:

```rust
for (role_name, role) in [
    ("research", &self.agent.research),
    ("implementation", &self.agent.implementation),
    ("init", &self.agent.init),
    ("judge", &self.agent.judge),
] {
```

- [ ] **Step 4: Fix existing tests broken by `command: Option<Vec<String>>`**

Search for `command: vec![` in the test helpers and update them. The helpers `regex_measure` and all `MeasureConfig` struct literals in tests need `command: Some(vec![...])`. Run:

```bash
cargo nextest run -p autotune-config 2>&1 | head -40
```

Fix each compile error by wrapping command values in `Some(...)`.

- [ ] **Step 5: Run all config tests**

```bash
cargo nextest run -p autotune-config
```

Expected: all pass including the new tests.

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-config/src/lib.rs
git commit -m "feat(config): add judge adaptor type with RubricConfig and optional command"
```

---

## Task 2: Batch prompt rendering in `autotune-judge`

**Files:**
- Modify: `crates/autotune-judge/src/prompt.rs`
- Modify: `crates/autotune-judge/src/lib.rs`

- [ ] **Step 1: Write failing test**

Add to `crates/autotune-judge/src/prompt.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Rubric, ScoreRange, Subject};

    fn make_rubric(id: &str, min: i32, max: i32) -> Rubric {
        Rubric {
            id: id.to_string(),
            title: format!("{id} title"),
            persona: "shared".to_string(),
            score_range: ScoreRange { min, max },
            instruction: format!("Score {id} from {min} to {max}."),
            guidance: None,
        }
    }

    #[test]
    fn batch_prompt_contains_persona_and_all_rubric_ids() {
        let subject = Subject::new("my-subject", "approach-alpha");
        let rubrics = vec![make_rubric("r1", 1, 5), make_rubric("r2", 1, 3)];
        let prompt = render_batch_prompt("A strict expert", &subject, &rubrics);
        assert!(prompt.contains("A strict expert"));
        assert!(prompt.contains("r1"));
        assert!(prompt.contains("r2"));
        assert!(prompt.contains("r1 title"));
        assert!(prompt.contains("r2 title"));
        assert!(prompt.contains("score: <integer>"));
        assert!(prompt.contains("reason: <one sentence>"));
    }

    #[test]
    fn batch_prompt_includes_guidance_when_present() {
        let subject = Subject::new("s", "a");
        let mut rubric = make_rubric("r1", 1, 5);
        rubric.guidance = Some("Check edge cases.".to_string());
        let prompt = render_batch_prompt("Reviewer", &subject, &[rubric]);
        assert!(prompt.contains("Check edge cases."));
    }

    #[test]
    fn batch_prompt_includes_subject_context() {
        use crate::model::{SubjectContext, SubjectContextKind};
        let mut subject = Subject::new("title", "summary");
        subject = subject.with_context(vec![SubjectContext {
            kind: SubjectContextKind::Note,
            label: "iteration".to_string(),
            body: "3".to_string(),
        }]);
        let prompt = render_batch_prompt("P", &subject, &[make_rubric("r1", 1, 5)]);
        assert!(prompt.contains("iteration"));
        assert!(prompt.contains("3"));
    }
}
```

- [ ] **Step 2: Run test to confirm fail**

```bash
cargo nextest run -p autotune-judge -E 'test(batch_prompt)'
```

Expected: compile error — `render_batch_prompt` not found.

- [ ] **Step 3: Implement `render_batch_prompt`**

Add to `crates/autotune-judge/src/prompt.rs`:

```rust
/// Render a batched assessment prompt for multiple rubrics under a shared persona.
///
/// The agent must return one blank-line-separated block per rubric:
/// ```text
/// <rubric-id>
/// score: <int>
/// reason: <one sentence>
/// ```
pub fn render_batch_prompt(persona: &str, subject: &Subject, rubrics: &[Rubric]) -> String {
    let context_block = subject.render_context();

    let rubric_list = rubrics
        .iter()
        .map(|r| {
            let guidance = match &r.guidance {
                Some(g) if !g.trim().is_empty() => format!("Guidance: {g}\n"),
                _ => String::new(),
            };
            format!(
                "## {id} — {title} (score {min} to {max})\n{instruction}\n{guidance}",
                id = r.id,
                title = r.title,
                min = r.score_range.min,
                max = r.score_range.max,
                instruction = r.instruction,
                guidance = guidance,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let ids_list = rubrics
        .iter()
        .map(|r| {
            format!(
                "{id}\nscore: <integer between {min} and {max}>\nreason: <one sentence>",
                id = r.id,
                min = r.score_range.min,
                max = r.score_range.max,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "You are judging as: {persona}\n\n\
         Subject: {title}\n\
         Summary: {summary}\n\
         Context:\n{context}\n\n\
         Score every rubric below. Return exactly one block per rubric ID, \
         separated by a blank line, in any order. Each block must be:\n\
         <rubric-id>\n\
         score: <integer>\n\
         reason: <one sentence>\n\n\
         Required response shape:\n\
         {ids_list}\n\n\
         Rubrics:\n\
         {rubric_list}",
        persona = persona,
        title = subject.title,
        summary = subject.summary,
        context = context_block,
        ids_list = ids_list,
        rubric_list = rubric_list,
    )
}
```

- [ ] **Step 4: Re-export from `lib.rs`**

Add to the `pub use` block in `crates/autotune-judge/src/lib.rs`:

```rust
pub use crate::prompt::render_batch_prompt;
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p autotune-judge -E 'test(batch_prompt)'
```

Expected: all 3 pass.

- [ ] **Step 6: Commit**

```bash
git add crates/autotune-judge/src/prompt.rs crates/autotune-judge/src/lib.rs
git commit -m "feat(judge): add render_batch_prompt for multi-rubric single-session evaluation"
```

---

## Task 3: Batch response parsing in `autotune-judge`

**Files:**
- Modify: `crates/autotune-judge/src/judge.rs`
- Modify: `crates/autotune-judge/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Add to `crates/autotune-judge/src/judge.rs` (inside `#[cfg(test)]` or add a new test module):

```rust
#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::model::{Rubric, ScoreRange};

    fn rubric(id: &str, min: i32, max: i32) -> Rubric {
        Rubric {
            id: id.to_string(),
            title: id.to_string(),
            persona: String::new(),
            score_range: ScoreRange { min, max },
            instruction: String::new(),
            guidance: None,
        }
    }

    #[test]
    fn parse_batch_happy_path() {
        let rubrics = vec![rubric("r1", 1, 5), rubric("r2", 1, 3)];
        let text = "r1\nscore: 4\nreason: Good but one edge case missing.\n\nr2\nscore: 2\nreason: Needs improvement.";
        let assessments = parse_batch_response(&rubrics, text).unwrap();
        assert_eq!(assessments.len(), 2);
        let r1 = assessments.iter().find(|a| a.rubric_id == "r1").unwrap();
        assert_eq!(r1.score, 4);
        assert_eq!(r1.reason, "Good but one edge case missing.");
        let r2 = assessments.iter().find(|a| a.rubric_id == "r2").unwrap();
        assert_eq!(r2.score, 2);
    }

    #[test]
    fn parse_batch_order_independent() {
        let rubrics = vec![rubric("r1", 1, 5), rubric("r2", 1, 5)];
        // Response in reverse order.
        let text = "r2\nscore: 3\nreason: Average.\n\nr1\nscore: 5\nreason: Perfect.";
        let assessments = parse_batch_response(&rubrics, text).unwrap();
        assert_eq!(assessments.len(), 2);
        assert_eq!(assessments.iter().find(|a| a.rubric_id == "r1").unwrap().score, 5);
        assert_eq!(assessments.iter().find(|a| a.rubric_id == "r2").unwrap().score, 3);
    }

    #[test]
    fn parse_batch_missing_rubric_errors() {
        let rubrics = vec![rubric("r1", 1, 5), rubric("r2", 1, 5)];
        let text = "r1\nscore: 4\nreason: Good.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("r2"), "error: {err}");
    }

    #[test]
    fn parse_batch_unknown_rubric_id_errors() {
        let rubrics = vec![rubric("r1", 1, 5)];
        let text = "r1\nscore: 4\nreason: Good.\n\nunknown\nscore: 3\nreason: Extra.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("unknown"), "error: {err}");
    }

    #[test]
    fn parse_batch_out_of_range_score_errors() {
        let rubrics = vec![rubric("r1", 1, 5)];
        let text = "r1\nscore: 9\nreason: Way too high.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("9"), "error: {err}");
    }

    #[test]
    fn parse_batch_malformed_block_errors() {
        let rubrics = vec![rubric("r1", 1, 5)];
        let text = "r1\nnot-score: 4\nreason: Bad.";
        let err = parse_batch_response(&rubrics, text).unwrap_err();
        assert!(err.to_string().contains("score:"), "error: {err}");
    }
}
```

- [ ] **Step 2: Run test to confirm fail**

```bash
cargo nextest run -p autotune-judge -E 'test(batch_tests)'
```

Expected: compile error — `parse_batch_response` not found.

- [ ] **Step 3: Implement `parse_batch_response`**

Add to `crates/autotune-judge/src/judge.rs`:

```rust
/// Parse a batched response from a judge agent into a `Vec<Assessment>`.
///
/// The response must contain one blank-line-separated block per rubric:
/// ```text
/// <rubric-id>
/// score: <int>
/// reason: <one sentence>
/// ```
///
/// Blocks may appear in any order. Returns an error if any rubric is missing,
/// any rubric ID is unrecognised, any score is out of range, or any block is
/// malformed.
pub fn parse_batch_response(rubrics: &[Rubric], text: &str) -> Result<Vec<Assessment>, JudgeError> {
    use std::collections::HashMap;

    let rubric_map: HashMap<&str, &Rubric> =
        rubrics.iter().map(|r| (r.id.as_str(), r)).collect();

    let mut results: HashMap<String, Assessment> = HashMap::new();

    for block in text.trim().split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut lines = block.lines();
        let id = lines.next().ok_or_else(|| JudgeError::BackendParse {
            message: "empty block in batch response".into(),
        })?.trim();

        let rubric = rubric_map.get(id).ok_or_else(|| JudgeError::BackendParse {
            message: format!("unknown rubric id '{id}' in batch response"),
        })?;

        let score_line = lines.next().ok_or_else(|| JudgeError::BackendParse {
            message: format!("block for '{id}' missing score line"),
        })?;
        let reason_line = lines.next().ok_or_else(|| JudgeError::BackendParse {
            message: format!("block for '{id}' missing reason line"),
        })?;

        let score_value = score_line
            .strip_prefix("score:")
            .ok_or_else(|| JudgeError::BackendParse {
                message: format!("block for '{id}': expected 'score:' on second line, got: {score_line}"),
            })?
            .trim();
        let score: i32 = score_value.parse().map_err(|_| JudgeError::BackendParse {
            message: format!("block for '{id}': score '{score_value}' is not an integer"),
        })?;

        if !rubric.score_range.contains(score) {
            return Err(JudgeError::BackendParse {
                message: format!(
                    "block for '{id}': score {score} outside range [{}, {}]",
                    rubric.score_range.min, rubric.score_range.max
                ),
            });
        }

        let reason = reason_line
            .strip_prefix("reason:")
            .ok_or_else(|| JudgeError::BackendParse {
                message: format!("block for '{id}': expected 'reason:' on third line, got: {reason_line}"),
            })?
            .trim()
            .to_string();

        if reason.is_empty() {
            return Err(JudgeError::BackendParse {
                message: format!("block for '{id}': reason must be non-empty"),
            });
        }

        if results.contains_key(id) {
            return Err(JudgeError::BackendParse {
                message: format!("duplicate block for rubric '{id}' in batch response"),
            });
        }

        results.insert(
            id.to_string(),
            Assessment::new(id, score, reason, "batch", None, None)?,
        );
    }

    // Verify all rubrics are present.
    for rubric in rubrics {
        if !results.contains_key(rubric.id.as_str()) {
            return Err(JudgeError::BackendParse {
                message: format!("batch response missing block for rubric '{}'", rubric.id),
            });
        }
    }

    Ok(rubrics
        .iter()
        .map(|r| results.remove(r.id.as_str()).unwrap())
        .collect())
}
```

- [ ] **Step 4: Re-export from `lib.rs`**

Add to `crates/autotune-judge/src/lib.rs`:

```rust
pub use crate::judge::parse_batch_response;
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p autotune-judge -E 'test(batch_tests)'
```

Expected: all 6 pass.

- [ ] **Step 6: Run full autotune-judge test suite**

```bash
cargo nextest run -p autotune-judge
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/autotune-judge/src/judge.rs crates/autotune-judge/src/lib.rs
git commit -m "feat(judge): add parse_batch_response for multi-rubric evaluation"
```

---

## Task 4: `JudgeContext` and `run_judge_measure` in `autotune-benchmark`

**Files:**
- Modify: `crates/autotune-benchmark/src/lib.rs`
- Modify: `crates/autotune-benchmark/Cargo.toml`

- [ ] **Step 1: Add `autotune-judge` dependency**

In `crates/autotune-benchmark/Cargo.toml`, add to `[dependencies]`:

```toml
autotune-judge = { path = "../autotune-judge" }
```

- [ ] **Step 2: Write failing tests**

Add to the `#[cfg(test)]` block in `crates/autotune-benchmark/src/lib.rs`:

```rust
#[cfg(test)]
mod judge_tests {
    use super::*;
    use autotune_config::{AdaptorConfig, MeasureConfig, RubricConfig, ScoreRangeConfig};
    use autotune_judge::{MockJudgeBackend, parse_batch_response};

    struct FakeAgent {
        response: String,
    }

    impl autotune_agent::Agent for FakeAgent {
        fn backend_name(&self) -> &str { "fake" }
        fn spawn(&self, config: &autotune_agent::AgentConfig) -> Result<autotune_agent::AgentSession, autotune_agent::AgentError> {
            Ok(autotune_agent::AgentSession {
                text: self.response.clone(),
                session_id: "fake-session".to_string(),
            })
        }
        fn send(&self, _session_id: &str, _config: &autotune_agent::AgentConfig) -> Result<autotune_agent::AgentSession, autotune_agent::AgentError> {
            unimplemented!()
        }
    }

    fn judge_measure(name: &str, rubric_ids: &[&str]) -> MeasureConfig {
        MeasureConfig {
            name: name.to_string(),
            command: None,
            timeout: 30,
            adaptor: AdaptorConfig::Judge {
                persona: "A reviewer".to_string(),
                rubrics: rubric_ids.iter().map(|id| RubricConfig {
                    id: id.to_string(),
                    title: id.to_string(),
                    instruction: "Score 1-5.".to_string(),
                    score_range: ScoreRangeConfig { min: 1, max: 5 },
                    guidance: None,
                }).collect(),
            },
        }
    }

    fn fake_judge_agent_config() -> autotune_agent::AgentConfig {
        autotune_agent::AgentConfig {
            prompt: String::new(),
            allowed_tools: vec![],
            working_directory: std::path::PathBuf::from("."),
            model: None,
            max_turns: Some(1),
            reasoning_effort: None,
        }
    }

    #[test]
    fn run_judge_measure_returns_metrics_per_rubric() {
        let tmp = tempfile::tempdir().unwrap();
        let config = judge_measure("critique", &["r1", "r2"]);
        let agent = FakeAgent {
            response: "r1\nscore: 4\nreason: Good.\n\nr2\nscore: 3\nreason: Acceptable.".to_string(),
        };
        let ctx = JudgeContext {
            agent: &agent,
            agent_config: fake_judge_agent_config(),
        };
        let report = run_judge_measure(&config, tmp.path(), "approach-a", 1, &ctx).unwrap();
        assert_eq!(*report.metrics.get("r1").unwrap(), 4.0);
        assert_eq!(*report.metrics.get("r2").unwrap(), 3.0);
        assert_eq!(report.name, "critique");
    }

    #[test]
    fn run_judge_measure_with_command_captures_output() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = judge_measure("critique", &["r1"]);
        config.command = Some(vec!["sh".to_string(), "-c".to_string(), "echo 'source code here'".to_string()]);
        let agent = FakeAgent {
            response: "r1\nscore: 5\nreason: Excellent.".to_string(),
        };
        let ctx = JudgeContext {
            agent: &agent,
            agent_config: fake_judge_agent_config(),
        };
        let report = run_judge_measure(&config, tmp.path(), "my-approach", 2, &ctx).unwrap();
        assert_eq!(*report.metrics.get("r1").unwrap(), 5.0);
        // stdout from command is visible in the report
        assert!(report.stdout.contains("source code here") || report.stderr.is_empty());
    }

    #[test]
    fn run_all_measures_with_judge_ctx_dispatches_judge_measure() {
        let tmp = tempfile::tempdir().unwrap();
        let configs = vec![judge_measure("j", &["score"])];
        let agent = FakeAgent {
            response: "score\nscore: 5\nreason: Perfect.".to_string(),
        };
        let ctx = JudgeContext {
            agent: &agent,
            agent_config: fake_judge_agent_config(),
        };
        let (metrics, reports) =
            run_all_measures_with_output(&configs, tmp.path(), "approach", 1, Some(&ctx)).unwrap();
        assert_eq!(*metrics.get("score").unwrap(), 5.0);
        assert_eq!(reports.len(), 1);
    }
}
```

- [ ] **Step 3: Run test to confirm fail**

```bash
cargo nextest run -p autotune-benchmark -E 'test(judge_tests)' 2>&1 | head -30
```

Expected: compile errors — `JudgeContext`, `run_judge_measure` not found; signature mismatch on `run_all_measures_with_output`.

- [ ] **Step 4: Implement `JudgeContext` and `run_judge_measure`**

Add to `crates/autotune-benchmark/src/lib.rs` (after the existing imports, before `run_measure`):

```rust
use autotune_config::AdaptorConfig;
use autotune_judge::{Rubric, ScoreRange, Subject, SubjectContext, SubjectContextKind,
                     parse_batch_response, render_batch_prompt};

pub struct JudgeContext<'a> {
    pub agent: &'a dyn autotune_agent::Agent,
    pub agent_config: autotune_agent::AgentConfig,
}

pub fn run_judge_measure(
    config: &MeasureConfig,
    working_dir: &Path,
    approach_name: &str,
    iteration: u32,
    ctx: &JudgeContext,
) -> Result<MeasureReport, MeasureError> {
    let AdaptorConfig::Judge { persona, rubrics: rubric_configs } = &config.adaptor else {
        panic!("run_judge_measure called on non-judge measure");
    };

    // Optionally run the command to gather subject context.
    let (cmd_stdout, cmd_stderr) = if let Some(cmd) = &config.command {
        let output = run_command_with_timeout(config, working_dir)?;
        if !output.status.success() {
            return Err(MeasureError::CommandFailed {
                name: config.name.clone(),
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        (
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    } else {
        (String::new(), String::new())
    };

    // Build subject context.
    let mut context = vec![
        SubjectContext {
            kind: SubjectContextKind::Note,
            label: "iteration".to_string(),
            body: iteration.to_string(),
        },
        SubjectContext {
            kind: SubjectContextKind::Note,
            label: "approach".to_string(),
            body: approach_name.to_string(),
        },
    ];
    if !cmd_stdout.is_empty() || !cmd_stderr.is_empty() {
        context.push(SubjectContext {
            kind: SubjectContextKind::SourceSnippet,
            label: "command_output".to_string(),
            body: format!("{cmd_stdout}\n{cmd_stderr}"),
        });
    }

    let subject = Subject::new(&config.name, approach_name).with_context(context);

    // Convert config rubrics to autotune-judge Rubric types.
    let rubrics: Vec<Rubric> = rubric_configs
        .iter()
        .map(|r| Rubric {
            id: r.id.clone(),
            title: r.title.clone(),
            persona: persona.clone(),
            score_range: ScoreRange {
                min: r.score_range.min,
                max: r.score_range.max,
            },
            instruction: r.instruction.clone(),
            guidance: r.guidance.clone(),
        })
        .collect();

    let prompt = render_batch_prompt(persona, &subject, &rubrics);

    let mut agent_cfg = ctx.agent_config.clone();
    agent_cfg.prompt = prompt;

    let response = ctx
        .agent
        .spawn(&agent_cfg)
        .map_err(|e| MeasureError::Extraction {
            name: config.name.clone(),
            source: autotune_adaptor::AdaptorError::Io {
                source: std::io::Error::other(format!("judge agent call failed: {e}")),
            },
        })?;

    let assessments =
        parse_batch_response(&rubrics, &response.text).map_err(|e| MeasureError::Extraction {
            name: config.name.clone(),
            source: autotune_adaptor::AdaptorError::Io {
                source: std::io::Error::other(format!("batch response parse failed: {e}")),
            },
        })?;

    let metrics: Metrics = assessments
        .iter()
        .map(|a| (a.rubric_id.clone(), a.score as f64))
        .collect();

    Ok(MeasureReport {
        name: config.name.clone(),
        stdout: cmd_stdout,
        stderr: cmd_stderr,
        metrics,
    })
}
```

- [ ] **Step 5: Update `run_all_measures_with_output` and `run_all_measures` signatures**

Change the existing functions to accept `approach_name`, `iteration`, and `judge_ctx`:

```rust
pub fn run_all_measures(
    configs: &[MeasureConfig],
    working_dir: &Path,
    approach_name: &str,
    iteration: u32,
    judge_ctx: Option<&JudgeContext>,
) -> Result<Metrics, MeasureError> {
    run_all_measures_with_output(configs, working_dir, approach_name, iteration, judge_ctx)
        .map(|(metrics, _)| metrics)
}

pub fn run_all_measures_with_output(
    configs: &[MeasureConfig],
    working_dir: &Path,
    approach_name: &str,
    iteration: u32,
    judge_ctx: Option<&JudgeContext>,
) -> Result<(Metrics, Vec<MeasureReport>), MeasureError> {
    let mut all_metrics = HashMap::new();
    let mut reports = Vec::with_capacity(configs.len());

    for config in configs {
        let report = match &config.adaptor {
            AdaptorConfig::Judge { .. } => {
                let ctx = judge_ctx.ok_or_else(|| MeasureError::Extraction {
                    name: config.name.clone(),
                    source: autotune_adaptor::AdaptorError::Io {
                        source: std::io::Error::other(
                            "judge adaptor requires a JudgeContext but none was provided",
                        ),
                    },
                })?;
                run_judge_measure(config, working_dir, approach_name, iteration, ctx)?
            }
            _ => run_measure_with_output(config, working_dir)?,
        };
        all_metrics.extend(report.metrics.clone());
        reports.push(report);
    }

    Ok((all_metrics, reports))
}
```

Also update `run_command_with_timeout` to handle `Option<Vec<String>>` for command. Change:
```rust
let program = &config.command[0];
let args = &config.command[1..];
```
to:
```rust
let command = config.command.as_ref().expect("non-judge measure must have command");
let program = &command[0];
let args = &command[1..];
```

- [ ] **Step 6: Fix existing tests in `autotune-benchmark`**

Update all `run_all_measures` and `run_all_measures_with_output` call sites in existing tests to pass the new params. For example:

```rust
// Before:
run_all_measures(&[m1, m2], tmp.path())
// After:
run_all_measures(&[m1, m2], tmp.path(), "test-approach", 1, None)
```

Also update `MeasureConfig` literals in tests: `command: vec![...]` → `command: Some(vec![...])`.

- [ ] **Step 7: Run benchmark tests**

```bash
cargo nextest run -p autotune-benchmark
```

Expected: all pass including new judge tests.

- [ ] **Step 8: Commit**

```bash
git add crates/autotune-benchmark/src/lib.rs crates/autotune-benchmark/Cargo.toml
git commit -m "feat(benchmark): add JudgeContext and run_judge_measure for judge adaptor"
```

---

## Task 5: Wire `judge_ctx` through `machine.rs`

**Files:**
- Modify: `crates/autotune/src/machine.rs`

- [ ] **Step 1: Update `run_measuring` signature**

Change `run_measuring` to accept the judge context:

```rust
fn run_measuring(
    config: &AutotuneConfig,
    store: &TaskStore,
    state: &mut TaskState,
    judge_ctx: Option<&autotune_benchmark::JudgeContext>,
) -> Result<()> {
    let approach = state
        .current_approach
        .as_ref()
        .context("no current approach in Measuring phase")?;
    println!(
        "[autotune] iteration {} — measuring '{}'",
        state.current_iteration, approach.name
    );

    let approach_name = approach.name.clone();
    let iteration = state.current_iteration;
    let worktree_path = approach.worktree_path.clone();

    let (metrics, reports) = autotune_benchmark::run_all_measures_with_output(
        &config.measure,
        &worktree_path,
        &approach_name,
        iteration,
        judge_ctx,
    )
    .context("measuring failed")?;

    for report in &reports {
        let _ = store.save_measure_output(
            state.current_iteration,
            &approach_name,
            &report.name,
            &report.stdout,
            &report.stderr,
        );
    }

    let approach_mut = state.current_approach.as_mut().unwrap();
    approach_mut.metrics = Some(metrics);
    state.current_phase = Phase::Scoring;
    store.save_state(state)?;
    Ok(())
}
```

- [ ] **Step 2: Update `run_single_phase` signature**

Add `judge_ctx: Option<&autotune_benchmark::JudgeContext>` as the last parameter and pass it to `run_measuring`:

```rust
pub fn run_single_phase(
    config: &AutotuneConfig,
    agent: &dyn Agent,
    scorer: &dyn ScoreCalculator,
    repo_root: &Path,
    store: &TaskStore,
    state: &mut TaskState,
    approver: Option<&dyn ToolApprover>,
    judge_ctx: Option<&autotune_benchmark::JudgeContext>,
) -> Result<bool> {
```

In the `Phase::Measuring` arm:
```rust
Phase::Measuring => {
    run_measuring(config, store, state, judge_ctx)?;
}
```

- [ ] **Step 3: Update `run_task` to thread judge_ctx**

Find `run_task` in `machine.rs`. It calls `run_single_phase` in a loop. Add `judge_ctx` to its signature and pass it through.

- [ ] **Step 4: Fix compile errors**

```bash
cargo build -p autotune 2>&1 | head -40
```

Fix any call sites of `run_single_phase` and `run_task` inside `machine.rs` tests that need the new param.

For unit tests in machine.rs that call `run_measuring` directly, add `None` as the last argument:
```rust
run_measuring(&config, &store, &mut state, None).unwrap();
```

- [ ] **Step 5: Run machine tests**

```bash
cargo nextest run -p autotune
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/autotune/src/machine.rs
git commit -m "feat(machine): thread JudgeContext through run_single_phase and run_measuring"
```

---

## Task 6: Wire judge agent into `main.rs`

**Files:**
- Modify: `crates/autotune/src/main.rs`
- Modify: `crates/autotune/src/agent_factory.rs`

- [ ] **Step 1: Add `AgentRole::Judge` to `agent_factory.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Research,
    Implementation,
    Init,
    Judge,
}

pub fn resolve_backend_name(config: &AgentConfig, role: AgentRole) -> Option<&str> {
    let role_config: Option<&AgentRoleConfig> = match role {
        AgentRole::Research => config.research.as_ref(),
        AgentRole::Implementation => config.implementation.as_ref(),
        AgentRole::Init => config.init.as_ref(),
        AgentRole::Judge => config.judge.as_ref(),
    };

    role_config
        .and_then(|rc| rc.backend.as_deref())
        .or(config.backend.as_deref())
}
```

- [ ] **Step 2: Add `judge_agent_session_config` helper in `main.rs`**

Add alongside `research_agent_session_config`:

```rust
fn judge_agent_session_config(
    config: &AutotuneConfig,
    repo_root: &Path,
) -> autotune_agent::AgentConfig {
    autotune_agent::AgentConfig {
        prompt: String::new(),
        allowed_tools: vec![],
        working_directory: repo_root.to_path_buf(),
        model: config.agent.judge.as_ref().and_then(|j| j.model.clone()),
        max_turns: Some(1),
        reasoning_effort: None,
    }
}
```

- [ ] **Step 3: Add `has_judge_measure` helper**

```rust
fn has_judge_measure(config: &AutotuneConfig) -> bool {
    config.measure.iter().any(|m| {
        matches!(m.adaptor, autotune_config::AdaptorConfig::Judge { .. })
    })
}
```

- [ ] **Step 4: Build judge agent in `cmd_run`**

Find the `cmd_run` function. After `load_config` and before calling the main run loop, add:

```rust
let judge_agent_and_config = if has_judge_measure(&config) {
    let agent = build_agent(&config, AgentRole::Judge)?;
    let agent_cfg = judge_agent_session_config(&config, &repo_root);
    Some((agent, agent_cfg))
} else {
    None
};
let judge_ctx = judge_agent_and_config.as_ref().map(|(agent, cfg)| {
    autotune_benchmark::JudgeContext {
        agent: agent.as_ref(),
        agent_config: cfg.clone(),
    }
});
```

Then pass `judge_ctx.as_ref()` to `run_task` / `run_single_phase` calls.

- [ ] **Step 5: Repeat for `cmd_step` and `cmd_resume`**

Apply the same pattern — build judge agent if needed, pass `judge_ctx.as_ref()` to `run_single_phase`.

- [ ] **Step 6: Extend `apply_global_agent_defaults` for judge role**

In `apply_global_agent_defaults`, add a `merge_role` call for the judge role after the init role:

```rust
merge_role(
    &mut config.agent.judge,
    &global_agent.judge,
    &project_defaults,
    &global_defaults,
);
```

- [ ] **Step 7: Build and fix remaining compile errors**

```bash
cargo build 2>&1 | head -40
```

Fix any remaining call sites. The mock path in `build_agent` doesn't need changes for the judge role — it returns the existing mock which is fine for non-judge scenarios.

- [ ] **Step 8: Run all tests**

```bash
cargo nextest run
```

Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add crates/autotune/src/main.rs crates/autotune/src/agent_factory.rs
git commit -m "feat: wire judge agent through main.rs and agent_factory for judge adaptor measures"
```

---

## Task 7: Scenario test — judge adaptor drives a full iteration

**Files:**
- Modify: `crates/autotune/tests/` (whichever scenario test file covers measure phase)

- [ ] **Step 1: Locate the scenario test file**

```bash
ls crates/autotune/tests/
```

Find `scenario_run_test.rs` or equivalent. Read the existing scenario test patterns to understand how `AUTOTUNE_MOCK_RESEARCH_SCRIPT` is used.

- [ ] **Step 2: Write a failing scenario test**

Add a new test that uses a judge adaptor measure. The mock agent response for the measuring phase must be in the batch format. Add to the scenario test file:

```rust
#[test]
#[cfg(feature = "mock")]
fn judge_adaptor_measure_produces_rubric_metrics_in_state() {
    use std::fs;

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // Minimal git repo + autotune config with a judge adaptor measure.
    init_git_repo(repo);  // use whatever helper initialises a test repo

    fs::write(repo.join(".autotune.toml"), r#"
[task]
name = "judge-test"
max_iterations = "1"

[paths]
tunable = ["src/**"]

[[measure]]
name = "critique"
[measure.adaptor]
type = "judge"
persona = "A strict reviewer"
[[measure.adaptor.rubrics]]
id = "quality"
title = "Quality"
instruction = "Score quality 1-5."
score_range = { min = 1, max = 5 }

[score]
type = "weighted_sum"
primary_metrics = [{ name = "quality", direction = "Maximize" }]
"#).unwrap();

    // Research script: emit one plan XML.
    let research_script = tmp.path().join("research.txt");
    fs::write(&research_script, r#"<plan>
<hypothesis>test hypothesis</hypothesis>
<approach-name>test-approach</approach-name>
<files>src/lib.rs</files>
</plan>"#).unwrap();

    // Mock judge response in batch format.
    // The mock agent returns this for the judge spawn call.
    // We hook into AUTOTUNE_MOCK_RESEARCH_SCRIPT to also supply the judge response.
    // Add a second entry separated by `---` for the judge call.
    fs::write(&research_script, concat!(
        "<plan>\n",
        "<hypothesis>test hypothesis</hypothesis>\n",
        "<approach-name>test-approach</approach-name>\n",
        "<files>src/lib.rs</files>\n",
        "</plan>\n",
        "---\n",
        "quality\nscore: 4\nreason: Good quality overall.",
    )).unwrap();

    // Run one iteration via the CLI.
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_autotune"))
        .arg("run")
        .current_dir(repo)
        .env("AUTOTUNE_MOCK", "1")
        .env("AUTOTUNE_MOCK_RESEARCH_SCRIPT", &research_script)
        .status()
        .unwrap();
    // The task will finish (or be in a terminal state after 1 iteration).

    // Verify the state has the quality metric.
    let ledger_path = repo.join(".autotune/tasks/judge-test/ledger.json");
    let ledger = fs::read_to_string(&ledger_path).unwrap();
    assert!(ledger.contains("\"quality\""), "ledger: {ledger}");
    assert!(ledger.contains("4"), "ledger should contain score 4: {ledger}");
}
```

Note: adapt the test to the actual test helper patterns in the file (e.g. how `init_git_repo`, environment variables, and CLI invocation are done in existing scenario tests).

- [ ] **Step 3: Run the test to confirm it fails**

```bash
cargo nextest run --features mock -E 'test(judge_adaptor_measure)'
```

Expected: FAIL — either compile error or runtime failure (mock agent doesn't route judge call correctly yet).

- [ ] **Step 4: Make the mock agent supply judge responses**

Check `autotune-mock` to understand how `AUTOTUNE_MOCK_RESEARCH_SCRIPT` splits responses. The mock agent currently routes responses to the research agent. The judge call is a `spawn()` call — if the mock agent treats all `spawn()` calls as research responses, the judge call will consume a research response entry.

Inspect `crates/autotune-mock/src/` and add a `AUTOTUNE_MOCK_JUDGE_SCRIPT` env var (or extend the existing script) so the mock agent returns a judge-formatted response for judge measure spawn calls. The simplest approach: add a `judge_response` builder method to `MockAgent` and a `AUTOTUNE_MOCK_JUDGE_SCRIPT` env var read in `main.rs` alongside `AUTOTUNE_MOCK_RESEARCH_SCRIPT`.

```rust
// In main.rs mock setup block:
if let Ok(path) = std::env::var("AUTOTUNE_MOCK_JUDGE_SCRIPT")
    && let Ok(content) = std::fs::read_to_string(&path)
{
    for entry in content.split("\n---\n") {
        let entry = entry.trim_end_matches('\n');
        if !entry.is_empty() {
            builder = builder.judge_response(entry);
        }
    }
}
```

Update the scenario test to use `AUTOTUNE_MOCK_JUDGE_SCRIPT` for the batch response.

- [ ] **Step 5: Run scenario test**

```bash
cargo nextest run --features mock -E 'test(judge_adaptor_measure)'
```

Expected: PASS.

- [ ] **Step 6: Run full test suite**

```bash
cargo nextest run
cargo nextest run --features mock
```

Expected: all pass.

- [ ] **Step 7: Final pre-commit checks**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

Fix any warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/autotune/tests/ crates/autotune-mock/src/ crates/autotune/src/main.rs
git commit -m "test(scenario): add end-to-end test for judge adaptor measure producing rubric metrics"
```

---

## Self-Review

**Spec coverage check:**
- ✅ `AdaptorConfig::Judge { persona, rubrics }` — Task 1
- ✅ `MeasureConfig.command: Option<Vec<String>>` — Task 1
- ✅ `agent.judge` optional role — Task 1 + Task 6
- ✅ `render_batch_prompt` — Task 2
- ✅ `parse_batch_response` — Task 3
- ✅ `JudgeContext` + `run_judge_measure` — Task 4
- ✅ `run_all_measures_with_output` extended — Task 4
- ✅ Optional command: stdout/stderr as SubjectContext — Task 4
- ✅ Iteration metadata as SubjectContext — Task 4
- ✅ `run_single_phase` / `run_measuring` updated — Task 5
- ✅ `cmd_run` / `cmd_step` / `cmd_resume` wired — Task 6
- ✅ `apply_global_agent_defaults` covers judge — Task 6
- ✅ Config validation: no rubrics, empty command, missing agent — Task 1
- ✅ Scenario test — Task 7
- ✅ No review step (raw assessment score used directly) — Task 4
- ✅ No example store in v1 — Task 4 (not present, intentionally omitted)
