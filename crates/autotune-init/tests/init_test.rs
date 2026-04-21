use autotune_config::global::GlobalConfig;
use autotune_init::{MockInput, UserInput, run_init};
use autotune_mock::MockAgent;
use std::path::PathBuf;

/// A `UserInput` that pops responses from a queue, falling back to a default.
struct ScriptedInput {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    fallback: String,
}

impl ScriptedInput {
    fn new(responses: &[&str], fallback: &str) -> Self {
        ScriptedInput {
            responses: std::sync::Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
            fallback: fallback.to_string(),
        }
    }
}

impl UserInput for ScriptedInput {
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

fn complete_init_agent() -> MockAgent {
    // Agent emits everything needed in one bundled response (the common case).
    MockAgent::builder()
        .init_response(
            r#"
<task>
  <name>test-exp</name>
  <canonical-branch>main</canonical-branch>
  <max-iterations>10</max-iterations>
</task>

<paths>
  <tunable>src/**</tunable>
</paths>

<measure>
  <name>bench1</name>
  <command>
    <segment>cargo</segment>
    <segment>bench</segment>
  </command>
  <adaptor>
    <type>regex</type>
    <pattern>
      <name>time_us</name>
      <regex><![CDATA[time:\s+([0-9.]+)]]></regex>
    </pattern>
  </adaptor>
</measure>

<score>
  <type>weighted_sum</type>
  <primary-metric>
    <name>time_us</name>
    <direction>Minimize</direction>
  </primary-metric>
</score>
"#,
        )
        .build()
}

#[test]
fn run_init_complete_conversation() {
    let agent = complete_init_agent();
    let global = GlobalConfig::default();
    let input = MockInput::new("yes");
    let result = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        &input,
        None,
    )
    .unwrap();

    assert_eq!(result.config.task.name, "test-exp");
    assert_eq!(result.config.paths.tunable, vec!["src/**"]);
    assert_eq!(result.config.measure.len(), 1);
    assert_eq!(result.config.measure[0].name, "bench1");
    assert!(result.baseline_metrics.is_none());
}

#[test]
fn run_init_accepts_fragments_across_multiple_turns() {
    // Agent emits one section at a time — the CLI should accept and ask for more.
    let agent = MockAgent::builder()
        .init_response(r#"<task><name>t</name><max-iterations>10</max-iterations></task>"#)
        .init_response(r#"<paths><tunable>src/**</tunable></paths>"#)
        .init_response(
            r#"
<measure>
  <name>m</name>
  <command><segment>echo</segment></command>
  <adaptor>
    <type>regex</type>
    <pattern><name>x</name><regex>x</regex></pattern>
  </adaptor>
</measure>
"#,
        )
        .init_response(
            r#"
<score>
  <type>weighted_sum</type>
  <primary-metric><name>x</name><direction>Maximize</direction></primary-metric>
</score>
"#,
        )
        .build();

    let global = GlobalConfig::default();
    let input = MockInput::new("yes");
    let result = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        &input,
        None,
    )
    .unwrap();

    assert_eq!(result.config.task.name, "t");
    assert_eq!(result.config.measure[0].name, "m");
}

#[test]
fn run_init_missing_required_sections_keeps_going() {
    // Agent only provides task + paths, never gets to measure/score.
    let agent = MockAgent::builder()
        .init_response(r#"<task><name>t</name><max-iterations>10</max-iterations></task>"#)
        .init_response(r#"<paths><tunable>src/**</tunable></paths>"#)
        .build();

    let global = GlobalConfig::default();
    let input = MockInput::new("yes");
    let result = run_init(
        &agent,
        &global,
        &PathBuf::from("/tmp/fake-repo"),
        &input,
        None,
    );

    // Should error because we never get measure + score sections (MockAgent runs out of responses)
    assert!(result.is_err());
}

#[test]
fn run_init_full_judge_flow() {
    // Agent responses (via init_responses, consumed sequentially):
    //   spawn  → judge measure header
    //   send 0 → correctness rubric proposal
    //   send 1 → readability rubric proposal
    //   send 2 → rubrics-done + remaining config sections
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
        .init_response(
            r#"<rubric>
  <id>correctness</id>
  <title>Correctness</title>
  <instruction><![CDATA[Does the implementation produce correct results for all inputs?]]></instruction>
  <score-range><min>1</min><max>5</max></score-range>
</rubric>"#,
        )
        .init_response(
            r#"<rubric>
  <id>readability</id>
  <title>Readability</title>
  <instruction><![CDATA[Is the code idiomatic and easy to follow?]]></instruction>
  <score-range><min>1</min><max>5</max></score-range>
</rubric>"#,
        )
        .init_response(
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

    // ScriptedInput pops responses in order:
    //   "accept" → user goal prompt (prompt_text)
    //   "accept" → correctness rubric approval (prompt_select in show_rubric_proposal)
    //   "accept" → readability rubric approval (prompt_select in show_rubric_proposal)
    //   "yes"    → config approval (prompt_approve)
    let input = ScriptedInput::new(&["accept", "accept", "accept", "yes"], "yes");

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
    assert!(result.baseline_metrics.is_none());
}
