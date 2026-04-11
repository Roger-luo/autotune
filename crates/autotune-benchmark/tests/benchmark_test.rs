use autotune_benchmark::{run_all_benchmarks, run_benchmark};
use autotune_config::{AdaptorConfig, BenchmarkConfig, RegexPattern};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

fn make_regex_benchmark(name: &str, command_output: &str, metric_name: &str) -> BenchmarkConfig {
    BenchmarkConfig {
        name: name.to_string(),
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("echo '{}'", command_output),
        ],
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
fn single_benchmark_extracts_metric() {
    let config = make_regex_benchmark("bench1", "149.83", "time_us");

    let metrics = run_benchmark(&config, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["time_us"], 149.83);
}

#[test]
fn multiple_benchmarks_merge_metrics() {
    let configs = vec![
        make_regex_benchmark("bench1", "100.5", "time"),
        make_regex_benchmark("bench2", "256.0", "mem"),
    ];

    let metrics = run_all_benchmarks(&configs, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["time"], 100.5);
    assert_eq!(metrics["mem"], 256.0);
}

#[test]
fn benchmark_command_failure() {
    let config = BenchmarkConfig {
        name: "bad".to_string(),
        command: vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()],
        timeout: 30,
        adaptor: AdaptorConfig::Regex { patterns: vec![] },
    };

    let err = run_benchmark(&config, std::path::Path::new(".")).unwrap_err();
    assert!(err.to_string().contains("command failed"));
}

#[test]
fn script_adaptor_benchmark_extraction() {
    let config = BenchmarkConfig {
        name: "scripted".to_string(),
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo raw output".to_string(),
        ],
        timeout: 30,
        adaptor: AdaptorConfig::Script {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                r#"echo '{"fidelity": 0.97}'"#.to_string(),
            ],
        },
    };

    let metrics = run_benchmark(&config, std::path::Path::new(".")).unwrap();
    assert_eq!(metrics["fidelity"], 0.97);
}

#[test]
fn benchmark_command_times_out() {
    let config = BenchmarkConfig {
        name: "slow".to_string(),
        command: vec!["sh".to_string(), "-c".to_string(), "sleep 2".to_string()],
        timeout: 1,
        adaptor: AdaptorConfig::Regex { patterns: vec![] },
    };

    let started = Instant::now();
    let err = run_benchmark(&config, std::path::Path::new(".")).unwrap_err();
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

    let config = BenchmarkConfig {
        name: "scripted".to_string(),
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo raw output".to_string(),
        ],
        timeout: 30,
        adaptor: AdaptorConfig::Script {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "./extract.sh".to_string(),
            ],
        },
    };

    let metrics = run_benchmark(&config, workdir).unwrap();
    assert_eq!(metrics["cwd_metric"], 7.0);
}
