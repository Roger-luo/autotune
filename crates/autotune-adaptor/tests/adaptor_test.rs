use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};
use autotune_adaptor::script::ScriptAdaptor;
use autotune_adaptor::{BenchmarkOutput, MetricAdaptor};
use std::fs;

#[test]
fn criterion_extracts_estimates() {
    let tempdir = tempfile::tempdir().unwrap();
    let criterion_dir = tempdir.path().join("criterion");
    let benchmark_dir = criterion_dir.join("my-benchmark").join("new");
    fs::create_dir_all(&benchmark_dir).unwrap();
    fs::write(
        benchmark_dir.join("estimates.json"),
        r#"{
            "mean": {"point_estimate": 1.25},
            "median": {"point_estimate": 1.0},
            "std_dev": {"point_estimate": 0.25}
        }"#,
    )
    .unwrap();

    let adaptor =
        autotune_adaptor::criterion::CriterionAdaptor::new(&criterion_dir, "my-benchmark");
    let output = BenchmarkOutput {
        stdout: String::new(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["mean"], 1.25);
    assert_eq!(metrics["median"], 1.0);
    assert_eq!(metrics["std_dev"], 0.25);
}

#[test]
fn regex_extracts_single_metric() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "time_us".to_string(),
        pattern: r"time:\s+([0-9.]+)\s+µs".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: "benchmark result\ntime: 149.83 µs\nother stuff".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["time_us"], 149.83);
}

#[test]
fn regex_extracts_named_group() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "throughput".to_string(),
        pattern: r"throughput=(?P<value>[0-9.]+)".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: "throughput=1234.5".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["throughput"], 1234.5);
}

#[test]
fn regex_extracts_multiple_metrics() {
    let adaptor = RegexAdaptor::new(vec![
        RegexPatternConfig {
            name: "time".to_string(),
            pattern: r"time:\s+([0-9.]+)".to_string(),
        },
        RegexPatternConfig {
            name: "mem".to_string(),
            pattern: r"memory:\s+([0-9.]+)".to_string(),
        },
    ]);

    let output = BenchmarkOutput {
        stdout: "time: 100.5\nmemory: 256.0".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["time"], 100.5);
    assert_eq!(metrics["mem"], 256.0);
}

#[test]
fn regex_no_match_returns_error() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "missing".to_string(),
        pattern: r"nonexistent:\s+([0-9.]+)".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: "no match here".to_string(),
        stderr: String::new(),
    };

    assert!(adaptor.extract(&output).is_err());
}

#[test]
fn regex_searches_stderr_too() {
    let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
        name: "val".to_string(),
        pattern: r"result=([0-9.]+)".to_string(),
    }]);

    let output = BenchmarkOutput {
        stdout: String::new(),
        stderr: "result=42.0".to_string(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["val"], 42.0);
}

#[test]
fn script_adaptor_echo_json() {
    let adaptor = ScriptAdaptor::new(vec![
        "sh".to_string(),
        "-c".to_string(),
        r#"echo '{"metric1": 42.0, "metric2": 2.5}'"#.to_string(),
    ]);

    let output = BenchmarkOutput {
        stdout: "ignored input".to_string(),
        stderr: String::new(),
    };

    let metrics = adaptor.extract(&output).unwrap();
    assert_eq!(metrics["metric1"], 42.0);
    assert_eq!(metrics["metric2"], 2.5);
}

#[test]
fn script_adaptor_nonzero_exit_returns_error() {
    let adaptor = ScriptAdaptor::new(vec![
        "sh".to_string(),
        "-c".to_string(),
        "exit 1".to_string(),
    ]);

    let output = BenchmarkOutput {
        stdout: String::new(),
        stderr: String::new(),
    };

    assert!(adaptor.extract(&output).is_err());
}
