---
title: autotune-benchmark
description: Runs configured measure commands, captures their output, and extracts metrics via pluggable adaptors.
section: Crates
order: 4
---

`autotune-benchmark` executes the measurement phase of the tune loop. It runs each configured measure command (with per-measure timeouts and process-group cleanup), captures raw stdout/stderr, and turns that output into a `HashMap<String, f64>` of metrics by dispatching to the right adaptor (regex, criterion, script, or an LLM judge).

## When to use it

- You need to run task/measurement commands and reduce their output to named numeric metrics.
- You want raw stdout/stderr retained alongside the metrics for later inspection.
- You need to combine several measures into one merged metric map, optionally including LLM-judge rubric scores.

The state machine's Measuring phase is the primary caller; use the crate directly when you want measurement without the full loop.

## Public API

- `run_measure(config, working_dir) -> Result<Metrics, MeasureError>` — Run one measure command and return only its extracted metrics.
- `run_measure_with_output(config, working_dir) -> Result<MeasureReport, MeasureError>` — Same, but returns the report including captured stdout/stderr.
- `run_all_measures(configs, working_dir, approach_name, iteration, judge_ctx) -> Result<Metrics, MeasureError>` — Run all measures and merge their metrics into one map.
- `run_all_measures_with_output(...) -> Result<(Metrics, Vec<MeasureReport>), MeasureError>` — Merged metrics plus per-measure reports, in config order.
- `run_judge_measure(config, working_dir, approach_name, iteration, ctx) -> Result<MeasureReport, MeasureError>` — Run a judge measure: optionally run a command, build a subject, call the batch judge, and return one metric per rubric ID.
- `build_adaptor(config, working_dir) -> Box<dyn MetricAdaptor>` — Construct a metric adaptor from an `AdaptorConfig` (panics for `Judge`, which uses the judge pipeline instead).
- `MeasureReport` — Result of one measure: `name`, `stdout`, `stderr`, `metrics`.
- `MeasureError` — Error enum: `CommandFailed`, `Io`, `TimedOut`, `Extraction`.
- `JudgeContext<'a>` — Carries the judge `agent`, `agent_config`, and an optional `make_stream` factory into the measuring phase.
- `JudgeStreamFactory` — Type alias for the per-invocation streaming-handler factory.
- `MeasureOutput` — Re-exported from `autotune-adaptor` for working with `build_adaptor`.

## Usage

```rust
use autotune_benchmark::{run_measure, MeasureError};
use autotune_config::{AdaptorConfig, MeasureConfig, RegexPattern};
use std::path::Path;

fn main() -> Result<(), MeasureError> {
    let config = MeasureConfig {
        name: "bench".to_string(),
        command: Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo 'score: 99.5'".to_string(),
        ]),
        timeout: 30,
        adaptor: AdaptorConfig::Regex {
            patterns: vec![RegexPattern {
                name: "score".to_string(),
                pattern: r"score: ([0-9.]+)".to_string(),
            }],
        },
    };

    let metrics = run_measure(&config, Path::new("."))?;
    assert_eq!(metrics["score"], 99.5);
    Ok(())
}
```

## Internal dependencies

- `autotune-config` — provides `MeasureConfig`, `AdaptorConfig`, and related config types.
- `autotune-adaptor` — provides the `MetricAdaptor` trait, `Metrics`, `MeasureOutput`, and the regex/criterion adaptors.
- `autotune-judge` — provides rubric/subject types and the batch judge prompt rendering and response parsing.
- `autotune-agent` — provides the `Agent` trait, streaming config, `aprintln!`, and terminal live-tail used while running commands.

## Notes

- `timeout` is in whole seconds. On timeout the measure's entire process group is killed (`SIGKILL` on Unix), so background descendants spawned by the command are cleaned up too — a plain `child.kill()` would leak them.
- The `Script` adaptor runs its command in `working_dir` with the measure's combined `stdout\nstderr` piped to stdin, and expects a JSON object of `{name: number}` on stdout.
- A `Judge` measure in `run_all_measures*` requires a `JudgeContext`; passing `None` yields a `MeasureError::Extraction`. Calling `build_adaptor` on a `Judge` config panics by design.
