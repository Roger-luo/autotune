use crate::{AdaptorError, MeasureOutput, MetricAdaptor, Metrics};
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
        self.criterion_dir.join(group).join("new").join("estimates.json")
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

impl MetricAdaptor for CriterionAdaptor {
    fn extract(&self, _output: &MeasureOutput) -> Result<Metrics, AdaptorError> {
        let mut metrics = Metrics::new();
        for entry in &self.benchmarks {
            let path = self.estimates_path(&entry.group);
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
        MeasureOutput { stdout: String::new(), stderr: String::new() }
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
}
