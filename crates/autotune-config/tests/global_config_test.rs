use autotune_config::global::GlobalConfig;
use std::io::Write;

#[test]
fn load_from_explicit_path() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
[agent]
backend = "claude"

[agent.init]
model = "opus"
"#,
    )
    .unwrap();

    let config = GlobalConfig::load_from(f.path()).unwrap();
    let agent = config.agent.unwrap();
    assert_eq!(agent.backend.as_deref(), Some("claude"));
    let init = agent.init.unwrap();
    assert_eq!(init.model.as_deref(), Some("opus"));
}

#[test]
fn load_from_missing_file_returns_empty() {
    let config = GlobalConfig::load_from(std::path::Path::new("/nonexistent/config.toml")).unwrap();
    assert!(config.agent.is_none());
}

#[test]
fn merge_user_overrides_system() {
    let mut sys = tempfile::NamedTempFile::new().unwrap();
    sys.write_all(
        br#"
[agent]
backend = "claude"

[agent.init]
model = "sonnet"
"#,
    )
    .unwrap();

    let mut user = tempfile::NamedTempFile::new().unwrap();
    user.write_all(
        br#"
[agent]
backend = "claude"

[agent.init]
model = "opus"
"#,
    )
    .unwrap();

    let config = GlobalConfig::load_layered(&[sys.path(), user.path()]).unwrap();
    let agent = config.agent.unwrap();
    let init = agent.init.unwrap();
    assert_eq!(init.model.as_deref(), Some("opus"));
}

#[test]
fn load_codex_backend_defaults() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
[agent]
backend = "codex"
reasoning_effort = "medium"

[agent.research]
backend = "codex"
model = "gpt-5"
reasoning_effort = "high"

[agent.init]
backend = "codex"
model = "gpt-5-mini"
reasoning_effort = "low"
"#,
    )
    .unwrap();

    let config = GlobalConfig::load_from(f.path()).unwrap();
    let agent = config.agent.unwrap();
    assert_eq!(agent.backend.as_deref(), Some("codex"));
    assert_eq!(
        agent.reasoning_effort,
        Some(autotune_config::ReasoningEffort::Medium)
    );
    let research = agent.research.unwrap();
    assert_eq!(research.backend.as_deref(), Some("codex"));
    assert_eq!(research.model.as_deref(), Some("gpt-5"));
    assert_eq!(
        research.reasoning_effort,
        Some(autotune_config::ReasoningEffort::High)
    );
    let init = agent.init.unwrap();
    assert_eq!(init.backend.as_deref(), Some("codex"));
    assert_eq!(init.model.as_deref(), Some("gpt-5-mini"));
    assert_eq!(
        init.reasoning_effort,
        Some(autotune_config::ReasoningEffort::Low)
    );
}
