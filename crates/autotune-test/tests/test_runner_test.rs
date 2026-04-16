use autotune_config::TestConfig;
use autotune_test::{all_passed, run_all_tests, run_test};
use std::sync::mpsc;
use std::time::Duration;

fn make_test_config(name: &str, command: &[&str]) -> TestConfig {
    TestConfig {
        name: name.to_string(),
        command: command.iter().map(|s| s.to_string()).collect(),
        timeout: 30,
        allow_test_edits: false,
    }
}

#[test]
fn passing_test() {
    let config = make_test_config("echo", &["sh", "-c", "echo hello"]);
    let result = run_test(&config, std::path::Path::new(".")).unwrap();
    assert!(result.passed);
    assert!(result.stdout.contains("hello"));
}

#[test]
fn failing_test() {
    let config = make_test_config("fail", &["sh", "-c", "echo oops >&2; exit 1"]);
    let result = run_test(&config, std::path::Path::new(".")).unwrap();
    assert!(!result.passed);
    assert!(result.stderr.contains("oops"));
}

#[test]
fn run_all_stops_on_first_failure() {
    let configs = vec![
        make_test_config("pass1", &["sh", "-c", "echo ok"]),
        make_test_config("fail", &["sh", "-c", "exit 1"]),
        make_test_config("pass2", &["sh", "-c", "echo ok"]),
    ];
    let results = run_all_tests(&configs, std::path::Path::new(".")).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].passed);
    assert!(!results[1].passed);
    assert!(!all_passed(&results));
}

#[test]
fn run_all_passes() {
    let configs = vec![
        make_test_config("p1", &["sh", "-c", "echo a"]),
        make_test_config("p2", &["sh", "-c", "echo b"]),
    ];
    let results = run_all_tests(&configs, std::path::Path::new(".")).unwrap();
    assert_eq!(results.len(), 2);
    assert!(all_passed(&results));
}

#[test]
fn empty_test_list() {
    let results = run_all_tests(&[], std::path::Path::new(".")).unwrap();
    assert!(results.is_empty());
    assert!(all_passed(&results));
}

#[test]
fn times_out_long_running_test() {
    let config = TestConfig {
        name: "timeout".to_string(),
        command: ["sh", "-c", "sleep 2"]
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        timeout: 1,
        allow_test_edits: false,
    };

    let error = run_test(&config, std::path::Path::new(".")).unwrap_err();
    match error {
        autotune_test::TestError::Timeout { name, timeout } => {
            assert_eq!(name, "timeout");
            assert_eq!(timeout, 1);
        }
        other => panic!("expected timeout error, got {other:?}"),
    }
}

#[test]
fn timeout_returns_even_if_descendant_keeps_pipes_open() {
    let config = TestConfig {
        name: "orphaned-pipes".to_string(),
        command: ["sh", "-c", "sleep 5 & wait"]
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        timeout: 1,
        allow_test_edits: false,
    };

    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = run_test(&config, std::path::Path::new("."));
        let _ = tx.send(result);
    });

    let error = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("run_test should return promptly after timeout");
    handle.join().unwrap();

    match error.unwrap_err() {
        autotune_test::TestError::Timeout { name, timeout } => {
            assert_eq!(name, "orphaned-pipes");
            assert_eq!(timeout, 1);
        }
        other => panic!("expected timeout error, got {other:?}"),
    }
}

#[test]
fn verbose_test_does_not_false_timeout() {
    let config = TestConfig {
        name: "verbose".to_string(),
        command: ["sh", "-c", "yes x | head -c 1048576"]
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        timeout: 1,
        allow_test_edits: false,
    };

    let result = run_test(&config, std::path::Path::new(".")).unwrap();
    assert!(result.passed);
    assert_eq!(result.stdout.len(), 1_048_576);
}
