use autotune_agent::protocol::{AgentFragment, parse_agent_response};
use autotune_agent::{Agent, AgentConfig, ToolPermission};
use autotune_mock::MockAgent;
use std::path::PathBuf;

#[test]
fn mock_agent_init_conversation() {
    let agent = MockAgent::builder()
        .init_response("<message>I see a Rust project.</message>")
        .init_response(
            r#"<question><text>Pick a name</text><option><key>a</key><label>my-exp</label></option></question>"#,
        )
        .build();

    let config = AgentConfig {
        prompt: "init prompt".to_string(),
        allowed_tools: vec![ToolPermission::Allow("Read".to_string())],
        working_directory: PathBuf::from("/tmp"),
        model: None,
        max_turns: None,
    };

    // spawn returns first init_response
    let resp = agent.spawn(&config).unwrap();
    let frags = parse_agent_response(&resp.text).unwrap();
    assert_eq!(frags.len(), 1);
    assert!(matches!(frags[0], AgentFragment::Message(_)));

    // send returns second init_response
    let session = autotune_agent::AgentSession {
        session_id: resp.session_id.clone(),
        backend: "mock".to_string(),
    };
    let resp2 = agent.send(&session, "sounds good").unwrap();
    let frags2 = parse_agent_response(&resp2.text).unwrap();
    assert_eq!(frags2.len(), 1);
    assert!(matches!(frags2[0], AgentFragment::Question { .. }));
}
