use autotune_agent::protocol::{AgentFragment, parse_agent_response};
use autotune_config::{AdaptorConfig, Direction, ScoreConfig};

#[test]
fn parse_empty_response_yields_no_fragments() {
    let frags = parse_agent_response("Just some prose with no tags.").unwrap();
    assert!(frags.is_empty());
}

#[test]
fn parse_message_fragment() {
    let frags = parse_agent_response("<message>hello there</message>").unwrap();
    assert_eq!(frags.len(), 1);
    match &frags[0] {
        AgentFragment::Message(text) => assert_eq!(text, "hello there"),
        other => panic!("expected Message, got {other:?}"),
    }
}

#[test]
fn parse_message_with_cdata_containing_reserved_chars() {
    let xml = "<message><![CDATA[reduce latency < 10ms & keep accuracy]]></message>";
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Message(text) => {
            assert_eq!(text, "reduce latency < 10ms & keep accuracy");
        }
        _ => panic!("expected Message"),
    }
}

#[test]
fn parse_question_fragment() {
    let xml = r#"
<question>
  <text>Which threshold?</text>
  <option><key>95</key><label>95%</label></option>
  <option><key>100</key><label>Full</label><description>100% coverage</description></option>
  <allow-free-response>true</allow-free-response>
</question>
"#;
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Question {
            text,
            options,
            allow_free_response,
        } => {
            assert_eq!(text, "Which threshold?");
            assert_eq!(options.len(), 2);
            assert_eq!(options[0].key, "95");
            assert_eq!(options[1].description.as_deref(), Some("100% coverage"));
            assert!(*allow_free_response);
        }
        _ => panic!("expected Question"),
    }
}

#[test]
fn parse_task_fragment() {
    let xml = r#"
<task>
  <name>test-coverage</name>
  <description><![CDATA[increase line coverage]]></description>
  <canonical-branch>main</canonical-branch>
  <max-iterations>20</max-iterations>
  <target-metric>
    <name>line_coverage</name>
    <value>95</value>
    <direction>Maximize</direction>
  </target-metric>
</task>
"#;
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Task(task) => {
            assert_eq!(task.name, "test-coverage");
            assert_eq!(task.canonical_branch, "main");
            assert_eq!(task.target_metric.len(), 1);
            assert_eq!(task.target_metric[0].name, "line_coverage");
            assert_eq!(task.target_metric[0].value, 95.0);
            assert!(matches!(
                task.target_metric[0].direction,
                Direction::Maximize
            ));
        }
        _ => panic!("expected Task"),
    }
}

#[test]
fn parse_task_with_inf_iterations() {
    let xml = "<task><name>t</name><max-iterations>inf</max-iterations></task>";
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Task(task) => {
            assert!(matches!(
                task.max_iterations,
                Some(autotune_config::StopValue::Infinite)
            ));
        }
        _ => panic!("expected Task"),
    }
}

#[test]
fn parse_paths_fragment() {
    let xml = r#"
<paths>
  <tunable>crates/**/*.rs</tunable>
  <tunable>src/lib.rs</tunable>
  <denied>target/**</denied>
</paths>
"#;
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Paths(p) => {
            assert_eq!(p.tunable, vec!["crates/**/*.rs", "src/lib.rs"]);
            assert_eq!(p.denied, vec!["target/**"]);
        }
        _ => panic!("expected Paths"),
    }
}

#[test]
fn parse_measure_with_regex_adaptor() {
    let xml = r#"
<measure>
  <name>coverage</name>
  <command>
    <segment>cargo</segment>
    <segment>llvm-cov</segment>
  </command>
  <timeout>600</timeout>
  <adaptor>
    <type>regex</type>
    <pattern>
      <name>line_coverage</name>
      <regex><![CDATA[TOTAL\s+\d+\s+([\d.]+)%]]></regex>
    </pattern>
  </adaptor>
</measure>
"#;
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Measure(m) => {
            assert_eq!(m.name, "coverage");
            assert_eq!(m.command, vec!["cargo", "llvm-cov"]);
            assert_eq!(m.timeout, 600);
            match &m.adaptor {
                AdaptorConfig::Regex { patterns } => {
                    assert_eq!(patterns.len(), 1);
                    assert_eq!(patterns[0].name, "line_coverage");
                    assert_eq!(patterns[0].pattern, r"TOTAL\s+\d+\s+([\d.]+)%");
                }
                _ => panic!("expected regex adaptor"),
            }
        }
        _ => panic!("expected Measure"),
    }
}

#[test]
fn parse_score_weighted_sum() {
    let xml = r#"
<score>
  <type>weighted_sum</type>
  <primary-metric>
    <name>line_coverage</name>
    <direction>Maximize</direction>
    <weight>1.0</weight>
  </primary-metric>
</score>
"#;
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Score(ScoreConfig::WeightedSum {
            primary_metrics, ..
        }) => {
            assert_eq!(primary_metrics.len(), 1);
            assert_eq!(primary_metrics[0].name, "line_coverage");
            assert_eq!(primary_metrics[0].weight, 1.0);
        }
        _ => panic!("expected WeightedSum score"),
    }
}

#[test]
fn parse_multiple_fragments_in_one_response() {
    let xml = r#"
Some preamble that should be ignored.

<message>Setting up the config now.</message>

<task>
  <name>t</name>
  <max-iterations>10</max-iterations>
</task>

<paths>
  <tunable>src/**</tunable>
</paths>
"#;
    let frags = parse_agent_response(xml).unwrap();
    assert_eq!(frags.len(), 3);
    assert!(matches!(frags[0], AgentFragment::Message(_)));
    assert!(matches!(frags[1], AgentFragment::Task(_)));
    assert!(matches!(frags[2], AgentFragment::Paths(_)));
}

#[test]
fn parse_unknown_top_level_tag_is_skipped() {
    let xml = r#"
<mystery>ignored</mystery>
<message>kept</message>
"#;
    let frags = parse_agent_response(xml).unwrap();
    assert_eq!(frags.len(), 1);
    assert!(matches!(frags[0], AgentFragment::Message(_)));
}

#[test]
fn parse_unknown_child_tag_is_skipped() {
    let xml = r#"
<task>
  <name>t</name>
  <max-iterations>10</max-iterations>
  <bogus>ignored</bogus>
</task>
"#;
    let frags = parse_agent_response(xml).unwrap();
    match &frags[0] {
        AgentFragment::Task(t) => assert_eq!(t.name, "t"),
        _ => panic!(),
    }
}

#[test]
fn parse_malformed_xml_errors() {
    let err = parse_agent_response("<message>unclosed").unwrap_err();
    assert!(err.to_string().to_lowercase().contains("eof"));
}

#[test]
fn parse_tool_request_single() {
    let xml = r#"
<request-tool>
  <tool>Bash</tool>
  <scope>cargo tree:*</scope>
  <reason>need to see the dependency graph</reason>
</request-tool>
"#;
    let reqs = autotune_agent::protocol::parse_tool_requests(xml).unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].tool, "Bash");
    assert_eq!(reqs[0].scope.as_deref(), Some("cargo tree:*"));
    assert_eq!(reqs[0].reason, "need to see the dependency graph");
}

#[test]
fn parse_tool_request_without_scope() {
    let xml =
        r#"<request-tool><tool>WebFetch</tool><reason>check crate docs</reason></request-tool>"#;
    let reqs = autotune_agent::protocol::parse_tool_requests(xml).unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].tool, "WebFetch");
    assert!(reqs[0].scope.is_none());
}

#[test]
fn parse_multiple_tool_requests() {
    let xml = r#"
<request-tool><tool>Bash</tool><scope>cargo tree:*</scope><reason>deps</reason></request-tool>
<request-tool><tool>Bash</tool><scope>git log:*</scope><reason>history</reason></request-tool>
"#;
    let reqs = autotune_agent::protocol::parse_tool_requests(xml).unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].scope.as_deref(), Some("git log:*"));
}

#[test]
fn parse_tool_request_rejects_missing_reason() {
    let xml = r#"<request-tool><tool>Bash</tool></request-tool>"#;
    let err = autotune_agent::protocol::parse_tool_requests(xml).unwrap_err();
    assert!(err.to_string().contains("reason"));
}

#[test]
fn parse_tool_request_ignores_other_tags() {
    let xml = r#"
Prose.
<request-tool><tool>Bash</tool><scope>ls:*</scope><reason>explore</reason></request-tool>
<message>hi</message>
<task><name>x</name></task>
"#;
    let reqs = autotune_agent::protocol::parse_tool_requests(xml).unwrap();
    assert_eq!(reqs.len(), 1);
}

#[test]
fn parse_invalid_direction_errors() {
    let xml = r#"
<task>
  <name>t</name>
  <max-iterations>10</max-iterations>
  <target-metric><name>m</name><value>1</value><direction>Up</direction></target-metric>
</task>
"#;
    let err = parse_agent_response(xml).unwrap_err();
    assert!(err.to_string().contains("Up"));
}
