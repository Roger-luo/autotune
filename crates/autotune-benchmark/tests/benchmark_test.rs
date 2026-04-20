use autotune_benchmark::{run_all_measures, run_measure};
use autotune_config::{AdaptorConfig, MeasureConfig, RegexPattern};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Instant;

fn make_regex_measure(name: &str, command_output: &str, metric_name: &str) -> MeasureConfig {
    MeasureConfig {
        name: name.to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("echo '{}'", command_output),
        ]),
        timeout: 30,
        adaptor: AdaptorConfig::Regex {
            patterns: vec![RegexPattern {
                name: metric_name.to_string(),
                pattern: r"([0-9.]+)".to_string(),
            }],
        },
    }
}

#[test]
fn single_task_extracts_metric() {
    let config = make_regex_measure("bench1", "149.83", "time_us");

    let metrics = run_measure(&config, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["time_us"], 149.83);
}

#[test]
fn multiple_tasks_merge_metrics() {
    let configs = vec![
        make_regex_measure("bench1", "100.5", "time"),
        make_regex_measure("bench2", "256.0", "mem"),
    ];

    let metrics = run_all_measures(&configs, std::path::Path::new("."), "test", 1, None).unwrap();
    assert_eq!(metrics["time"], 100.5);
    assert_eq!(metrics["mem"], 256.0);
}

#[test]
fn task_command_failure() {
    let config = MeasureConfig {
        name: "bad".to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "exit 1".to_string(),
        ]),
        timeout: 30,
        adaptor: AdaptorConfig::Regex { patterns: vec![] },
    };

    let err = run_measure(&config, std::path::Path::new(".")).unwrap_err();
    assert!(err.to_string().contains("command failed"));
}

#[test]
fn script_adaptor_task_extraction() {
    let config = MeasureConfig {
        name: "scripted".to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo raw output".to_string(),
        ]),
        timeout: 30,
        adaptor: AdaptorConfig::Script {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                r#"echo '{"fidelity": 0.97}'"#.to_string(),
            ],
        },
    };

    let metrics = run_measure(&config, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["fidelity"], 0.97);
}

#[test]
fn task_command_times_out() {
    let config = MeasureConfig {
        name: "slow".to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 2".to_string(),
        ]),
        timeout: 1,
        adaptor: AdaptorConfig::Regex { patterns: vec![] },
    };

    let started = Instant::now();
    let err = run_measure(&config, std::path::Path::new(".")).unwrap_err();
    assert!(started.elapsed().as_secs_f32() < 2.0);
    assert!(err.to_string().contains("timed out"));
}

#[test]
fn script_adaptor_runs_in_working_dir() {
    let tempdir = tempfile::tempdir().unwrap();
    let workdir = tempdir.path();
    fs::write(workdir.join("marker.txt"), "present").unwrap();

    let script = workdir.join("extract.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
test -f marker.txt || exit 1
echo '{"cwd_metric": 7.0}'
"#,
    )
    .unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();

    let config = MeasureConfig {
        name: "scripted".to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo raw output".to_string(),
        ]),
        timeout: 30,
        adaptor: AdaptorConfig::Script {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "./extract.sh".to_string(),
            ],
        },
    };

    let metrics = run_measure(&config, workdir).unwrap();
    assert_eq!(metrics["cwd_metric"], 7.0);
}

#[test]
fn task_does_not_false_timeout_when_stdout_is_verbose() {
    let config = MeasureConfig {
        name: "verbose".to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "i=0; while [ \"$i\" -lt 20000 ]; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n'; i=$((i + 1)); done; echo 42.5".to_string(),
        ]),
        timeout: 1,
        adaptor: AdaptorConfig::Regex {
            patterns: vec![RegexPattern {
                name: "score".to_string(),
                pattern: r"(42\.5)".to_string(),
            }],
        },
    };

    let metrics = run_measure(&config, Path::new(".")).unwrap();
    assert_eq!(metrics["score"], 42.5);
}

#[test]
fn task_timeout_kills_background_descendants() {
    let tempdir = tempfile::tempdir().unwrap();
    let pid_file = tempdir.path().join("bg.pid");
    let config = MeasureConfig {
        name: "timeout-tree".to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("sleep 30 & echo $! > {}; wait", shell_quote_path(&pid_file)),
        ]),
        timeout: 1,
        adaptor: AdaptorConfig::Regex { patterns: vec![] },
    };

    let err = run_measure(&config, Path::new(".")).unwrap_err();
    assert!(err.to_string().contains("timed out"));

    let pid: i32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let alive = process_exists(pid);
    if alive {
        kill_process(pid);
    }
    assert!(!alive, "background process {pid} survived timeout cleanup");
}

fn shell_quote_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn process_exists(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn kill_process(pid: i32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
