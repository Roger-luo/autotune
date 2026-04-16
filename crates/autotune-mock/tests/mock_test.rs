use std::path::Path;
use std::process::Command;

use autotune_agent::{Agent, AgentConfig, AgentSession, ToolPermission};
use autotune_mock::{ImplBehavior, MockAgent};

fn init_temp_repo(dir: &Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init failed");

    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();

    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    std::fs::write(dir.join("README.md"), "# test\n").unwrap();

    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", "initial commit"])
        .current_dir(dir)
        .output()
        .unwrap();
}

fn latest_sha(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn dummy_session() -> AgentSession {
    AgentSession {
        session_id: "mock-session-001".to_string(),
        backend: "mock".to_string(),
    }
}

fn dummy_config(dir: &Path) -> AgentConfig {
    AgentConfig {
        prompt: "do something".to_string(),
        allowed_tools: vec![ToolPermission::Allow("Read".to_string())],
        working_directory: dir.to_path_buf(),
        model: Some("test-model".to_string()),
        max_turns: Some(10),
        reasoning_effort: None,
    }
}

// -----------------------------------------------------------------------
// Test 1: Builder creates agent with hypotheses, cycling works
// -----------------------------------------------------------------------

#[test]
fn test_hypothesis_cycling() {
    let agent = MockAgent::builder()
        .hypothesis("opt-1", "reduce allocations", &["src/alloc.rs"])
        .hypothesis("opt-2", "use SIMD", &["src/simd.rs"])
        .build();

    let session = dummy_session();

    let r1 = agent.send(&session, "plan 1").unwrap();
    assert!(r1.text.contains("opt-1"));
    assert!(r1.text.contains("reduce allocations"));

    let r2 = agent.send(&session, "plan 2").unwrap();
    assert!(r2.text.contains("opt-2"));
    assert!(r2.text.contains("use SIMD"));

    // Cycling: should wrap back to opt-1
    let r3 = agent.send(&session, "plan 3").unwrap();
    assert!(r3.text.contains("opt-1"));
}

// -----------------------------------------------------------------------
// Test 2: spawn() first call returns ready, subsequent calls create commits
// -----------------------------------------------------------------------

#[test]
fn test_spawn_commit_dummy() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_temp_repo(dir);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "test", &[])
        .implementation_behavior(ImplBehavior::CommitDummy)
        .build();

    let config = dummy_config(dir);

    // First spawn: research agent init, returns "ready"
    let r1 = agent.spawn(&config).unwrap();
    assert_eq!(r1.text, "ready");
    assert_eq!(r1.session_id, "mock-session-001");
    let sha_after_first = latest_sha(dir);

    // Second spawn: implementation, creates a commit
    let sha_before = latest_sha(dir);
    let r2 = agent.spawn(&config).unwrap();
    assert!(
        r2.text.starts_with("implementation done"),
        "response text: {}",
        r2.text
    );
    let sha_after = latest_sha(dir);
    assert_ne!(sha_before, sha_after, "commit should have been created");
    // First spawn didn't create a commit (research init)
    assert_eq!(sha_after_first, sha_before);
}

// -----------------------------------------------------------------------
// Test 3: NoCommit behavior returns response but doesn't create commits
// -----------------------------------------------------------------------

#[test]
fn test_spawn_no_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_temp_repo(dir);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "test", &[])
        .implementation_behavior(ImplBehavior::NoCommit)
        .build();

    let config = dummy_config(dir);

    // First spawn: research init
    let _ = agent.spawn(&config).unwrap();

    // Second spawn: should NOT create a commit
    let sha_before = latest_sha(dir);
    let r = agent.spawn(&config).unwrap();
    assert!(
        r.text.starts_with("implementation done"),
        "response text: {}",
        r.text
    );
    let sha_after = latest_sha(dir);
    assert_eq!(sha_before, sha_after, "no commit should have been created");
}

// -----------------------------------------------------------------------
// Test 4: Tracking counters increment correctly
// -----------------------------------------------------------------------

#[test]
fn test_tracking_counters() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_temp_repo(dir);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "test", &[])
        .build();

    assert_eq!(agent.spawn_count(), 0);
    assert_eq!(agent.send_count(), 0);

    let config = dummy_config(dir);
    let session = dummy_session();

    agent.spawn(&config).unwrap();
    assert_eq!(agent.spawn_count(), 1);

    agent.spawn(&config).unwrap();
    assert_eq!(agent.spawn_count(), 2);

    agent.send(&session, "hello").unwrap();
    assert_eq!(agent.send_count(), 1);

    agent.send(&session, "world").unwrap();
    assert_eq!(agent.send_count(), 2);
}

// -----------------------------------------------------------------------
// Test 5: last_spawn_config captures the config
// -----------------------------------------------------------------------

#[test]
fn test_last_spawn_config() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_temp_repo(dir);

    let agent = MockAgent::builder()
        .hypothesis("opt-1", "test", &[])
        .build();

    assert!(agent.last_spawn_config().is_none());

    let config = dummy_config(dir);
    agent.spawn(&config).unwrap();

    let captured = agent.last_spawn_config().unwrap();
    assert_eq!(captured.prompt, "do something");
    assert_eq!(captured.model, Some("test-model".to_string()));
    assert_eq!(captured.max_turns, Some(10));
}

// -----------------------------------------------------------------------
// Test 6: last_send_message captures the message
// -----------------------------------------------------------------------

#[test]
fn test_last_send_message() {
    let agent = MockAgent::builder()
        .hypothesis("opt-1", "test", &[])
        .build();

    assert!(agent.last_send_message().is_none());

    let session = dummy_session();
    agent.send(&session, "first message").unwrap();
    assert_eq!(agent.last_send_message().unwrap(), "first message");

    agent.send(&session, "second message").unwrap();
    assert_eq!(agent.last_send_message().unwrap(), "second message");
}
