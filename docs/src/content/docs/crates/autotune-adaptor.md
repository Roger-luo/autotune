---
title: autotune-adaptor
description: Trait and built-in adaptors that extract numeric metrics from a measure command's output.
section: Crates
order: 2
---

`autotune-adaptor` defines the `MetricAdaptor` trait and its built-in implementations. Each adaptor turns a measure command's raw output (or its on-disk artifacts) into a `Metrics` map — `HashMap<String, f64>` — which the rest of Autotune feeds into scoring.

## When to use it

- You're adding or modifying how Autotune pulls metrics out of task output (a new output format, a new statistic).
- You're writing a new adaptor backend: implement the `MetricAdaptor` trait.
- You're debugging extraction failures surfaced as `AdaptorError` (regex didn't match, Criterion file missing, script returned non-JSON, etc.).

## Public API

- `MetricAdaptor` — the core trait: `extract(&self, output: &MeasureOutput) -> Result<Metrics, AdaptorError>`.
- `Metrics` — type alias for `HashMap<String, f64>`, the common output of every adaptor.
- `MeasureOutput` — struct holding the `stdout` and `stderr` (both `String`) of a measure command.
- `AdaptorError` — `thiserror` enum covering regex compile/no-match, float parse, missing/invalid Criterion JSON, empty/failed script commands, script output parse, and IO errors.
- `regex::RegexAdaptor` + `regex::RegexPatternConfig` — extract metrics via regex capture groups, one named pattern per metric.
- `criterion::CriterionAdaptor`, `criterion::CriterionBenchmarkEntry`, `criterion::CriterionStat` — read point estimates from Criterion `estimates.json` files; `CriterionStat` selects `Mean`, `Median`, or `StdDev`.
- `script::ScriptAdaptor` — run a user script that reads measure output on stdin and writes JSON metrics to stdout.

## Usage

```rust
use autotune_adaptor::{MeasureOutput, MetricAdaptor};
use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};

let adaptor = RegexAdaptor::new(vec![RegexPatternConfig {
    name: "throughput".to_string(),
    // captured via the named group `value`, or falling back to group 1
    pattern: r"throughput:\s*(?P<value>[0-9.]+)".to_string(),
}]);

let output = MeasureOutput {
    stdout: "throughput: 1234.5 ops/s".to_string(),
    stderr: String::new(),
};

let metrics = adaptor.extract(&output)?;
assert_eq!(metrics["throughput"], 1234.5);
# Ok::<(), autotune_adaptor::AdaptorError>(())
```

## Internal dependencies

None — this is a leaf crate (depends only on `thiserror`, `serde`, `serde_json`, and `regex`).

## Notes

- `RegexAdaptor` and `ScriptAdaptor` concatenate `stdout` and `stderr` (joined by a newline) before processing, so a pattern can match either stream.
- `RegexAdaptor` reads the capture group named `value` first and falls back to the first positional group; a pattern with no value/group-1 capture is treated as a no-match.
- `CriterionAdaptor` ignores `MeasureOutput` entirely — it reads `<criterion_dir>/<group>/new/estimates.json` from disk, so the benchmark must have already run and written its artifacts.
- `ScriptAdaptor`'s script must exit zero and print valid JSON deserializable into `HashMap<String, f64>`; anything else becomes `ScriptFailed` or `ScriptOutputParse`.
