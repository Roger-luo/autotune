use autotune_agent::protocol::{parse_agent_request, AgentRequest};
use autotune_agent::{Agent, AgentConfig, ToolPermission};
use autotune_mock::MockAgent;
use std::path::PathBuf;

#[test]
fn mock_agent_init_conversation() {
    let agent = MockAgent::builder()
        .init_response(r#"{"type":"message","text":"I see a Rust project."}"#)
        .init_response(r#"{"type":"question","text":"Pick a name","options":[{"key":"a","description":"my-exp"}],"allow_free_response":false}"#)
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
    let req = parse_agent_request(&resp.text).unwrap();
    assert!(matches!(req, AgentRequest::Message { .. }));

    // send returns second init_response
    let session = autotune_agent::AgentSession {
        session_id: resp.session_id.clone(),
        backend: "mock".to_string(),
    };
    let resp2 = agent.send(&session, "sounds good").unwrap();
    let req2 = parse_agent_request(&resp2.text).unwrap();
    assert!(matches!(req2, AgentRequest::Question { .. }));
}
