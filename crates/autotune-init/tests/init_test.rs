use autotune_config::global::GlobalConfig;
use autotune_init::{MockInput, run_init};
use autotune_mock::MockAgent;
use std::path::PathBuf;

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
