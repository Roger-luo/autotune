use autotune_config::global::GlobalConfig;
use std::io::Write;

#[test]
fn user_config_path_returns_some() {
    // On any system with a home dir, this should return Some
    let path = GlobalConfig::user_config_path();
    assert!(path.is_some());
    let p = path.unwrap();
    assert!(p.ends_with("autotune/config.toml"));
}

#[test]
fn roundtrip_through_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    // Write a config
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(
        br#"
[agent]
backend = "claude"

[agent.init]
model = "opus"
max_turns = 100
"#,
    )
    .unwrap();

    // Load and verify
    let config = GlobalConfig::load_from(&path).unwrap();
    let agent = config.agent.unwrap();
    assert_eq!(agent.backend.as_deref(), Some("claude"));
    let init = agent.init.unwrap();
    assert_eq!(init.model.as_deref(), Some("opus"));
    assert_eq!(init.max_turns, Some(100));
}
