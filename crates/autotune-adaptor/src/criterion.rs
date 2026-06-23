use crate::{AdaptorError, MeasureOutput, MetricAdaptor, MetricVariance, Metrics, Variances};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum CriterionStat {
    Mean,
    Median,
    StdDev,
}

pub struct CriterionBenchmarkEntry {
    pub name: String,
    pub group: String,
    pub stat: CriterionStat,
}

pub struct CriterionAdaptor {
    criterion_dir: PathBuf,
    benchmarks: Vec<CriterionBenchmarkEntry>,
}

impl CriterionAdaptor {
    pub fn new(criterion_dir: &Path, benchmarks: Vec<CriterionBenchmarkEntry>) -> Self {
        Self {
            criterion_dir: criterion_dir.to_path_buf(),
            benchmarks,
        }
    }

    fn estimates_path(&self, group: &str) -> PathBuf {
        self.criterion_dir
            .join(group)
            .join("new")
            .join("estimates.json")
    }

    /// Resolve the on-disk `estimates.json` path for a benchmark `group`,
    /// trying the literal path first and falling back to the `full_id` index
    /// (see [`Self::build_full_id_index`]). `full_id_index` is the lazily-built
    /// cache shared across benchmarks in a single extraction pass.
    fn resolve_estimates_path(
        &self,
        group: &str,
        full_id_index: &mut Option<HashMap<String, PathBuf>>,
    ) -> Result<PathBuf, AdaptorError> {
        let literal = self.estimates_path(group);
        if literal.is_file() {
            return Ok(literal);
        }
        // The literal `group` path doesn't exist — Criterion likely sanitized
        // a '/' inside the group/function id. Resolve via the logical full_id
        // recorded in benchmark.json.
        let index = full_id_index.get_or_insert_with(|| self.build_full_id_index());
        match index.get(group) {
            Some(new_dir) => Ok(new_dir.join("estimates.json")),
            None => Err(AdaptorError::CriterionNotFound {
                path: literal.display().to_string(),
            }),
        }
    }

    /// Resolve, for each configured benchmark, the on-disk `estimates.json`
    /// path it reads from. Returned as `(metric_name, path)` pairs so a caller
    /// can copy the file for post-hoc analysis (the iteration worktree is
    /// removed after integration). Unresolvable benchmarks are skipped — this
    /// is a best-effort lookup, not the measurement path.
    pub fn estimates_files(&self) -> Vec<(String, PathBuf)> {
        let mut index: Option<HashMap<String, PathBuf>> = None;
        let mut out = Vec::new();
        for entry in &self.benchmarks {
            if let Ok(path) = self.resolve_estimates_path(&entry.group, &mut index) {
                out.push((entry.name.clone(), path));
            }
        }
        out
    }

    /// Index every benchmark's logical `full_id` (as printed by `cargo bench`,
    /// e.g. `gates/two-qubit/cnot`) to the `new/` directory holding its
    /// `estimates.json`.
    ///
    /// Criterion sanitizes the path-separator `/` *within* a group or function
    /// id into `_` when forming the on-disk directory (so `gates/two-qubit/cnot`
    /// lives under `gates_two-qubit/cnot`, and `sparse-vec/u128/add_or_insert/...`
    /// under `sparse-vec/u128_add_or_insert_...`). That means the literal
    /// `full_id` is not a usable path and can't be reconstructed from a single
    /// string. We recover the mapping from the `benchmark.json` Criterion writes
    /// next to each `estimates.json`, which records the original `full_id`.
    fn build_full_id_index(&self) -> HashMap<String, PathBuf> {
        let mut index = HashMap::new();
        let mut stack = vec![self.criterion_dir.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.file_name() != Some(OsStr::new("benchmark.json")) {
                    continue;
                }
                // Only index the fresh run: a re-run leaves a stale `base/`
                // benchmark.json carrying the same full_id.
                if path.parent().and_then(Path::file_name) != Some(OsStr::new("new")) {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(meta) = serde_json::from_str::<CriterionBenchmarkMeta>(&content) else {
                    continue;
                };
                if let Some(parent) = path.parent() {
                    index.insert(meta.full_id, parent.to_path_buf());
                }
            }
        }
        index
    }
}

#[derive(serde::Deserialize)]
struct CriterionEstimates {
    mean: CriterionStatValue,
    median: CriterionStatValue,
    std_dev: CriterionStatValue,
}

#[derive(serde::Deserialize)]
struct CriterionStatValue {
    point_estimate: f64,
    #[serde(default)]
    confidence_interval: Option<CriterionConfidenceInterval>,
}

#[derive(serde::Deserialize)]
struct CriterionConfidenceInterval {
    lower_bound: f64,
    upper_bound: f64,
}

impl CriterionEstimates {
    fn stat(&self, stat: &CriterionStat) -> &CriterionStatValue {
        match stat {
            CriterionStat::Mean => &self.mean,
            CriterionStat::Median => &self.median,
            CriterionStat::StdDev => &self.std_dev,
        }
    }
}

/// The subset of Criterion's `benchmark.json` we need: the logical benchmark id
/// (e.g. `gates/two-qubit/cnot`) used to recover the sanitized on-disk path.
#[derive(serde::Deserialize)]
struct CriterionBenchmarkMeta {
    full_id: String,
}

impl CriterionAdaptor {
    /// Read and parse the `estimates.json` for a benchmark `group`.
    fn read_estimates(
        &self,
        group: &str,
        full_id_index: &mut Option<HashMap<String, PathBuf>>,
    ) -> Result<CriterionEstimates, AdaptorError> {
        let path = self.resolve_estimates_path(group, full_id_index)?;
        let content =
            std::fs::read_to_string(&path).map_err(|_| AdaptorError::CriterionNotFound {
                path: path.display().to_string(),
            })?;
        serde_json::from_str(&content).map_err(|source| AdaptorError::CriterionParse { source })
    }
}

impl MetricAdaptor for CriterionAdaptor {
    fn extract(&self, _output: &MeasureOutput) -> Result<Metrics, AdaptorError> {
        let mut metrics = Metrics::new();
        // Built lazily on the first miss: walking the criterion tree is only
        // needed when a `group` doesn't resolve as a literal path.
        let mut full_id_index: Option<HashMap<String, PathBuf>> = None;
        for entry in &self.benchmarks {
            let estimates = self.read_estimates(&entry.group, &mut full_id_index)?;
            metrics.insert(
                entry.name.clone(),
                estimates.stat(&entry.stat).point_estimate,
            );
        }
        Ok(metrics)
    }

    /// Read the confidence interval (and the std_dev point estimate) criterion
    /// records next to each benchmark's mean, so the scorer can size a noise
    /// envelope. The CI is the selected stat's own interval; the stddev is the
    /// `std_dev` point estimate (a single number criterion always reports).
    fn extract_variances(&self, _output: &MeasureOutput) -> Result<Variances, AdaptorError> {
        let mut variances = Variances::new();
        let mut full_id_index: Option<HashMap<String, PathBuf>> = None;
        for entry in &self.benchmarks {
            let estimates = self.read_estimates(&entry.group, &mut full_id_index)?;
            let stat = estimates.stat(&entry.stat);
            let (ci_lower, ci_upper) = match &stat.confidence_interval {
                Some(ci) => (Some(ci.lower_bound), Some(ci.upper_bound)),
                None => (None, None),
            };
            let variance = MetricVariance {
                // The std_dev point estimate is the dispersion of the samples;
                // it stands in for k·sigma noise sizing when no CI is present.
                stddev: Some(estimates.std_dev.point_estimate),
                ci_lower,
                ci_upper,
            };
            if !variance.is_empty() {
                variances.insert(entry.name.clone(), variance);
            }
        }
        Ok(variances)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MeasureOutput;

    fn dummy_output() -> MeasureOutput {
        MeasureOutput {
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn write_estimates(dir: &std::path::Path, group: &str, mean: f64, median: f64, std_dev: f64) {
        let group_dir = dir.join(group).join("new");
        std::fs::create_dir_all(&group_dir).unwrap();
        let json = format!(
            r#"{{"mean":{{"point_estimate":{mean}}},"median":{{"point_estimate":{median}}},"std_dev":{{"point_estimate":{std_dev}}}}}"#
        );
        std::fs::write(group_dir.join("estimates.json"), json).unwrap();
    }

    /// Write an estimates.json shaped the way real criterion writes it: each
    /// stat carries a `confidence_interval` with lower/upper bounds.
    fn write_estimates_with_ci(
        dir: &std::path::Path,
        group: &str,
        mean: f64,
        mean_ci: (f64, f64),
        std_dev: f64,
    ) {
        let group_dir = dir.join(group).join("new");
        std::fs::create_dir_all(&group_dir).unwrap();
        let (lo, hi) = mean_ci;
        let json = format!(
            r#"{{"mean":{{"confidence_interval":{{"confidence_level":0.95,"lower_bound":{lo},"upper_bound":{hi}}},"point_estimate":{mean}}},"median":{{"point_estimate":{mean}}},"std_dev":{{"point_estimate":{std_dev}}}}}"#
        );
        std::fs::write(group_dir.join("estimates.json"), json).unwrap();
    }

    #[test]
    fn criterion_not_found_error() {
        let adaptor = CriterionAdaptor::new(
            std::path::Path::new("/nonexistent"),
            vec![CriterionBenchmarkEntry {
                name: "m".to_string(),
                group: "bench".to_string(),
                stat: CriterionStat::Mean,
            }],
        );
        let err = adaptor.extract(&dummy_output()).unwrap_err();
        assert!(matches!(err, crate::AdaptorError::CriterionNotFound { .. }));
    }

    #[test]
    fn criterion_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let bench_dir = dir.path().join("bench").join("new");
        std::fs::create_dir_all(&bench_dir).unwrap();
        std::fs::write(bench_dir.join("estimates.json"), b"not valid json").unwrap();
        let adaptor = CriterionAdaptor::new(
            dir.path(),
            vec![CriterionBenchmarkEntry {
                name: "m".to_string(),
                group: "bench".to_string(),
                stat: CriterionStat::Mean,
            }],
        );
        let err = adaptor.extract(&dummy_output()).unwrap_err();
        assert!(matches!(err, crate::AdaptorError::CriterionParse { .. }));
    }

    #[test]
    fn criterion_extracts_named_metrics_with_stat_selection() {
        let dir = tempfile::tempdir().unwrap();
        write_estimates(dir.path(), "sort/random", 100.0, 95.0, 5.0);
        write_estimates(dir.path(), "search/linear", 200.0, 190.0, 10.0);

        let adaptor = CriterionAdaptor::new(
            dir.path(),
            vec![
                CriterionBenchmarkEntry {
                    name: "sort_mean_ns".to_string(),
                    group: "sort/random".to_string(),
                    stat: CriterionStat::Mean,
                },
                CriterionBenchmarkEntry {
                    name: "search_median_ns".to_string(),
                    group: "search/linear".to_string(),
                    stat: CriterionStat::Median,
                },
            ],
        );

        let metrics = adaptor.extract(&dummy_output()).unwrap();
        assert_eq!(metrics["sort_mean_ns"], 100.0);
        assert_eq!(metrics["search_median_ns"], 190.0);
        assert_eq!(metrics.len(), 2);
    }

    /// Write a benchmark the way Criterion actually lays it out: estimates +
    /// a `benchmark.json` recording the logical `full_id`, stored under the
    /// (possibly sanitized) `directory_name`.
    fn write_bench(
        dir: &std::path::Path,
        directory_name: &str,
        full_id: &str,
        new_dir_name: &str,
        mean: f64,
        median: f64,
        std_dev: f64,
    ) {
        let new_dir = dir.join(directory_name).join(new_dir_name);
        std::fs::create_dir_all(&new_dir).unwrap();
        let estimates = format!(
            r#"{{"mean":{{"point_estimate":{mean}}},"median":{{"point_estimate":{median}}},"std_dev":{{"point_estimate":{std_dev}}}}}"#
        );
        std::fs::write(new_dir.join("estimates.json"), estimates).unwrap();
        let bench = format!(r#"{{"full_id":"{full_id}","directory_name":"{directory_name}"}}"#);
        std::fs::write(new_dir.join("benchmark.json"), bench).unwrap();
    }

    #[test]
    fn criterion_resolves_via_full_id_when_group_slashes_are_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        // Criterion sanitizes the '/' *within* a group/function id into '_',
        // keeping only the single '/' between group and function. So the
        // benchmark whose logical id is "gates/two-qubit/cnot" is stored under
        // "gates_two-qubit/cnot", and "sparse-vec/u128/add_or_insert/existing"
        // under "sparse-vec/u128_add_or_insert_existing". A config that uses the
        // benchmark id verbatim (as printed by `cargo bench`) must still resolve.
        write_bench(
            dir.path(),
            "gates_two-qubit/cnot",
            "gates/two-qubit/cnot",
            "new",
            42.0,
            40.0,
            1.0,
        );
        write_bench(
            dir.path(),
            "sparse-vec/u128_add_or_insert_existing",
            "sparse-vec/u128/add_or_insert/existing",
            "new",
            7.0,
            6.0,
            0.5,
        );

        let adaptor = CriterionAdaptor::new(
            dir.path(),
            vec![
                CriterionBenchmarkEntry {
                    name: "cnot_ns".to_string(),
                    group: "gates/two-qubit/cnot".to_string(),
                    stat: CriterionStat::Mean,
                },
                CriterionBenchmarkEntry {
                    name: "add_ns".to_string(),
                    group: "sparse-vec/u128/add_or_insert/existing".to_string(),
                    stat: CriterionStat::Mean,
                },
            ],
        );

        let metrics = adaptor.extract(&dummy_output()).unwrap();
        assert_eq!(metrics["cnot_ns"], 42.0);
        assert_eq!(metrics["add_ns"], 7.0);
        assert_eq!(metrics.len(), 2);
    }

    #[test]
    fn criterion_full_id_lookup_prefers_new_over_base() {
        let dir = tempfile::tempdir().unwrap();
        // A re-run leaves a stale `base/` alongside `new/`; both carry the same
        // full_id. The fallback must read the fresh `new/` estimates.
        write_bench(
            dir.path(),
            "grp_sub/fn",
            "grp/sub/fn",
            "base",
            999.0,
            999.0,
            9.0,
        );
        write_bench(
            dir.path(),
            "grp_sub/fn",
            "grp/sub/fn",
            "new",
            11.0,
            10.0,
            0.1,
        );

        let adaptor = CriterionAdaptor::new(
            dir.path(),
            vec![CriterionBenchmarkEntry {
                name: "m".to_string(),
                group: "grp/sub/fn".to_string(),
                stat: CriterionStat::Mean,
            }],
        );

        let metrics = adaptor.extract(&dummy_output()).unwrap();
        assert_eq!(metrics["m"], 11.0);
    }

    /// The adaptor reads the confidence interval and std_dev criterion writes
    /// next to the mean, populating a `MetricVariance` per benchmark. The CI
    /// half-width is `(upper - lower) / 2`.
    #[test]
    fn criterion_extracts_variance_from_estimates_json() {
        let dir = tempfile::tempdir().unwrap();
        write_estimates_with_ci(dir.path(), "bench", 100.0, (90.0, 110.0), 7.5);

        let adaptor = CriterionAdaptor::new(
            dir.path(),
            vec![CriterionBenchmarkEntry {
                name: "m".to_string(),
                group: "bench".to_string(),
                stat: CriterionStat::Mean,
            }],
        );

        let variances = adaptor.extract_variances(&dummy_output()).unwrap();
        let v = &variances["m"];
        assert_eq!(v.stddev, Some(7.5));
        assert_eq!(v.ci_lower, Some(90.0));
        assert_eq!(v.ci_upper, Some(110.0));
        assert_eq!(v.ci_half_width(), Some(10.0));
    }

    /// An estimates.json without a `confidence_interval` (older criterion, or a
    /// hand-written fixture) still yields a variance carrying just the stddev —
    /// the CI fields stay `None` and `ci_half_width` is `None`.
    #[test]
    fn criterion_variance_without_ci_carries_only_stddev() {
        let dir = tempfile::tempdir().unwrap();
        write_estimates(dir.path(), "bench", 100.0, 98.0, 5.0);

        let adaptor = CriterionAdaptor::new(
            dir.path(),
            vec![CriterionBenchmarkEntry {
                name: "m".to_string(),
                group: "bench".to_string(),
                stat: CriterionStat::Mean,
            }],
        );

        let variances = adaptor.extract_variances(&dummy_output()).unwrap();
        let v = &variances["m"];
        assert_eq!(v.stddev, Some(5.0));
        assert_eq!(v.ci_lower, None);
        assert_eq!(v.ci_half_width(), None);
    }

    /// `estimates_files` resolves the on-disk path for each benchmark so a
    /// caller can copy it for post-hoc analysis (after the worktree is gone).
    #[test]
    fn criterion_estimates_files_resolves_paths() {
        let dir = tempfile::tempdir().unwrap();
        write_estimates_with_ci(dir.path(), "bench", 100.0, (90.0, 110.0), 7.5);

        let adaptor = CriterionAdaptor::new(
            dir.path(),
            vec![CriterionBenchmarkEntry {
                name: "m".to_string(),
                group: "bench".to_string(),
                stat: CriterionStat::Mean,
            }],
        );

        let files = adaptor.estimates_files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "m");
        assert!(files[0].1.ends_with("bench/new/estimates.json"));
        assert!(files[0].1.is_file());
    }
}
