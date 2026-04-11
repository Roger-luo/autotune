use autotune_config::TestConfig;
use autotune_test::{all_passed, run_all_tests, run_test};

fn make_test_config(name: &str, command: &[&str]) -> TestConfig {
    TestConfig {
        name: name.to_string(),
        command: command.iter().map(|s| s.to_string()).collect(),
        timeout: 30,
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
