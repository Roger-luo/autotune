use autotune_adaptor::criterion::CriterionAdaptor;
use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};
use autotune_adaptor::script::ScriptAdaptor;
use autotune_adaptor::{BenchmarkOutput, MetricAdaptor, Metrics};
use autotune_config::{AdaptorConfig, BenchmarkConfig};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

/// Errors returned by benchmark execution and metric extraction.
#[derive(Debug, Error)]
pub enum BenchmarkError {
    #[error("benchmark '{name}' command failed (exit code {code}): {stderr}")]
    CommandFailed {
        name: String,
        code: i32,
        stderr: String,
    },

    #[error("benchmark '{name}' IO error: {source}")]
    Io {
        name: String,
        source: std::io::Error,
    },

    #[error("metric extraction failed for benchmark '{name}': {source}")]
    Extraction {
        name: String,
        source: autotune_adaptor::AdaptorError,
    },
}

/// Run a single benchmark command and extract metrics.
pub fn run_benchmark(
    config: &BenchmarkConfig,
    working_dir: &Path,
) -> Result<Metrics, BenchmarkError> {
    let program = &config.command[0];
    let args = &config.command[1..];

    let output = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .output()
        .map_err(|source| BenchmarkError::Io {
            name: config.name.clone(),
            source,
        })?;

    if !output.status.success() {
        return Err(BenchmarkError::CommandFailed {
            name: config.name.clone(),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let bench_output = BenchmarkOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };

    let adaptor = build_adaptor(&config.adaptor, working_dir);
    adaptor
        .extract(&bench_output)
        .map_err(|source| BenchmarkError::Extraction {
            name: config.name.clone(),
            source,
        })
}

/// Run all configured benchmarks and merge their metrics.
pub fn run_all_benchmarks(
    configs: &[BenchmarkConfig],
    working_dir: &Path,
) -> Result<Metrics, BenchmarkError> {
    let mut all_metrics = HashMap::new();

    for config in configs {
        let metrics = run_benchmark(config, working_dir)?;
        all_metrics.extend(metrics);
    }

    Ok(all_metrics)
}

/// Build a MetricAdaptor from config.
fn build_adaptor(config: &AdaptorConfig, working_dir: &Path) -> Box<dyn MetricAdaptor> {
    match config {
        AdaptorConfig::Regex { patterns } => {
            let configs: Vec<RegexPatternConfig> = patterns
                .iter()
                .map(|pattern| RegexPatternConfig {
                    name: pattern.name.clone(),
                    pattern: pattern.pattern.clone(),
                })
                .collect();
            Box::new(RegexAdaptor::new(configs))
        }
        AdaptorConfig::Criterion { benchmark_name } => {
            let criterion_dir = working_dir.join("target").join("criterion");
            Box::new(CriterionAdaptor::new(&criterion_dir, benchmark_name))
        }
        AdaptorConfig::Script { command } => Box::new(ScriptAdaptor::new(command.clone())),
    }
}
