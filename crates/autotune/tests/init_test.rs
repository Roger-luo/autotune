use autotune_config::global::GlobalConfig;
use autotune_init::{MockInput, run_init};
use autotune_mock::MockAgent;
use std::path::PathBuf;

#[test]
fn agent_assisted_init_produces_valid_config() {
    // Agent can bundle all required sections plus an optional test suite in one turn.
    let agent = MockAgent::builder()
        .init_response(
            r#"
<message>I see a Rust project.</message>

<task>
  <name>perf-opt</name>
  <description><![CDATA[Optimize performance]]></description>
  <canonical-branch>main</canonical-branch>
  <max-iterations>20</max-iterations>
</task>

<paths>
  <tunable>src/**/*.rs</tunable>
</paths>

<test>
  <name>rust</name>
  <command>
    <segment>cargo</segment>
    <segment>test</segment>
  </command>
</test>

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
        .build();

    let global = GlobalConfig::default();
    // MockInput returns "yes" to everything, including the <message> prompt.
    let input = MockInput::new("yes");
    let result = run_init(&agent, &global, &PathBuf::from("/tmp/fake"), &input, None).unwrap();

    assert_eq!(result.config.task.name, "perf-opt");
    assert_eq!(
        result.config.task.description.as_deref(),
        Some("Optimize performance")
    );
    assert_eq!(result.config.paths.tunable, vec!["src/**/*.rs"]);
    assert_eq!(result.config.test.len(), 1);
    assert_eq!(result.config.test[0].name, "rust");
    assert_eq!(result.config.measure.len(), 1);
    assert_eq!(result.config.measure[0].name, "bench1");
    assert!(result.baseline_metrics.is_none());

    // Verify the config serializes to valid TOML that roundtrips
    let toml_str = toml::to_string_pretty(&result.config).unwrap();
    let reparsed: autotune_config::AutotuneConfig = toml::from_str(&toml_str).unwrap();
    reparsed.validate().unwrap();
    assert_eq!(reparsed.task.name, "perf-opt");
}

#[test]
fn agent_assisted_init_validates_sections_incrementally() {
    // Agent proposes an invalid task (no stop condition), then a valid one,
    // then the rest of the config across turns.
    let agent = MockAgent::builder()
        .init_response(r#"<task><name>test-exp</name></task>"#)
        .init_response(r#"<task><name>test-exp</name><max-iterations>5</max-iterations></task>"#)
        .init_response(r#"<paths><tunable>src/**</tunable></paths>"#)
        .init_response(
            r#"
<measure>
  <name>b</name>
  <command><segment>echo</segment></command>
  <adaptor>
    <type>regex</type>
    <pattern><name>m</name><regex>x</regex></pattern>
  </adaptor>
</measure>
"#,
        )
        .init_response(
            r#"
<score>
  <type>weighted_sum</type>
  <primary-metric><name>m</name><direction>Maximize</direction></primary-metric>
</score>
"#,
        )
        .build();

    let global = GlobalConfig::default();
    let input = MockInput::new("yes");
    let result = run_init(&agent, &global, &PathBuf::from("/tmp/fake"), &input, None).unwrap();

    assert_eq!(result.config.task.name, "test-exp");
}
