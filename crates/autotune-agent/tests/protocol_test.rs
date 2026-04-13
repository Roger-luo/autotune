use autotune_agent::protocol::{AgentRequest, ConfigSection, parse_agent_request};

#[test]
fn parse_message_request() {
    let json = r#"{"type":"message","text":"Hello, I found some benchmarks."}"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Message { text } => {
            assert_eq!(text, "Hello, I found some benchmarks.");
        }
        _ => panic!("expected Message"),
    }
}

#[test]
fn parse_question_request() {
    let json = r#"{
        "type": "question",
        "text": "What type of project is this?",
        "options": [
            {"key": "a", "label": "Rust library"},
            {"key": "b", "label": "Python package", "description": "uses pyproject.toml"}
        ],
        "allow_free_response": true
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Question {
            text,
            options,
            allow_free_response,
        } => {
            assert_eq!(text, "What type of project is this?");
            assert_eq!(options.len(), 2);
            assert_eq!(options[0].key, "a");
            assert!(allow_free_response);
        }
        _ => panic!("expected Question"),
    }
}

#[test]
fn parse_config_task_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "task",
            "name": "my-task",
            "max_iterations": "10",
            "canonical_branch": "main"
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => match section {
            ConfigSection::Task(task) => {
                assert_eq!(task.name, "my-task");
            }
            _ => panic!("expected Task section"),
        },
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_paths_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "paths",
            "tunable": ["src/**/*.rs"],
            "denied": []
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => match section {
            ConfigSection::Paths(paths) => {
                assert_eq!(paths.tunable, vec!["src/**/*.rs"]);
            }
            _ => panic!("expected Paths section"),
        },
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_test_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "test",
            "name": "rust",
            "command": ["cargo", "test"]
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            assert!(matches!(section, ConfigSection::Test(_)));
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_measure_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "measure",
            "name": "perf",
            "command": ["cargo", "bench"],
            "adaptor": {
                "type": "regex",
                "patterns": [{"name": "time_us", "pattern": "time:\\s+([0-9.]+)"}]
            }
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            assert!(matches!(section, ConfigSection::Measure(_)));
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_config_score_section() {
    let json = r#"{
        "type": "config",
        "section": {
            "type": "score",
            "value": {
                "type": "weighted_sum",
                "primary_metrics": [{"name": "time_us", "direction": "Minimize"}]
            }
        }
    }"#;
    let req = parse_agent_request(json).unwrap();
    match req {
        AgentRequest::Config { section } => {
            assert!(matches!(section, ConfigSection::Score { .. }));
        }
        _ => panic!("expected Config"),
    }
}

#[test]
fn parse_request_with_surrounding_prose() {
    let response = r#"
I've analyzed your project. Here's my suggestion:

{"type":"message","text":"This looks like a Rust project with Criterion benchmarks."}

Let me know if you'd like to proceed.
"#;
    let req = parse_agent_request(response).unwrap();
    assert!(matches!(req, AgentRequest::Message { .. }));
}

#[test]
fn parse_request_no_json_errors() {
    let response = "I couldn't figure out what to do.";
    let err = parse_agent_request(response).unwrap_err();
    assert!(err.to_string().contains("no valid JSON"));
}
