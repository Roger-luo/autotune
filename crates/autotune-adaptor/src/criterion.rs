use crate::{AdaptorError, MeasureOutput, MetricAdaptor, Metrics};
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
}

/// The subset of Criterion's `benchmark.json` we need: the logical benchmark id
/// (e.g. `gates/two-qubit/cnot`) used to recover the sanitized on-disk path.
#[derive(serde::Deserialize)]
struct CriterionBenchmarkMeta {
    full_id: String,
}

impl MetricAdaptor for CriterionAdaptor {
    fn extract(&self, _output: &MeasureOutput) -> Result<Metrics, AdaptorError> {
        let mut metrics = Metrics::new();
        // Built lazily on the first miss: walking the criterion tree is only
        // needed when a `group` doesn't resolve as a literal path.
        let mut full_id_index: Option<HashMap<String, PathBuf>> = None;
        for entry in &self.benchmarks {
            let literal = self.estimates_path(&entry.group);
            let path = if literal.is_file() {
                literal
            } else {
                // The literal `group` path doesn't exist — Criterion likely
                // sanitized a '/' inside the group/function id. Resolve via the
                // logical full_id recorded in benchmark.json.
                let index = full_id_index.get_or_insert_with(|| self.build_full_id_index());
                match index.get(&entry.group) {
                    Some(new_dir) => new_dir.join("estimates.json"),
                    None => {
                        return Err(AdaptorError::CriterionNotFound {
                            path: literal.display().to_string(),
                        });
                    }
                }
            };
            let content =
                std::fs::read_to_string(&path).map_err(|_| AdaptorError::CriterionNotFound {
                    path: path.display().to_string(),
                })?;
            let estimates: CriterionEstimates = serde_json::from_str(&content)
                .map_err(|source| AdaptorError::CriterionParse { source })?;
            let value = match entry.stat {
                CriterionStat::Mean => estimates.mean.point_estimate,
                CriterionStat::Median => estimates.median.point_estimate,
                CriterionStat::StdDev => estimates.std_dev.point_estimate,
            };
            metrics.insert(entry.name.clone(), value);
        }
        Ok(metrics)
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
}
