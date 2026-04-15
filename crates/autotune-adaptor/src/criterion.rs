use crate::{AdaptorError, MeasureOutput, MetricAdaptor, Metrics};
use std::path::{Path, PathBuf};

/// Reads Criterion's estimates.json for a named measure.
pub struct CriterionAdaptor {
    criterion_dir: PathBuf,
    measure_name: String,
}

impl CriterionAdaptor {
    pub fn new(criterion_dir: &Path, measure_name: &str) -> Self {
        Self {
            criterion_dir: criterion_dir.to_path_buf(),
            measure_name: measure_name.to_string(),
        }
    }

    fn estimates_path(&self) -> PathBuf {
        self.criterion_dir
            .join(&self.measure_name)
            .join("new")
            .join("estimates.json")
    }
}

#[derive(serde::Deserialize)]
struct CriterionEstimates {
    mean: CriterionStat,
    median: CriterionStat,
    std_dev: CriterionStat,
}

#[derive(serde::Deserialize)]
struct CriterionStat {
    point_estimate: f64,
}

impl MetricAdaptor for CriterionAdaptor {
    fn extract(&self, _output: &MeasureOutput) -> Result<Metrics, AdaptorError> {
        let path = self.estimates_path();
        let content =
            std::fs::read_to_string(&path).map_err(|_| AdaptorError::CriterionNotFound {
                path: path.display().to_string(),
            })?;

        let estimates: CriterionEstimates = serde_json::from_str(&content)
            .map_err(|source| AdaptorError::CriterionParse { source })?;

        let mut metrics = Metrics::new();
        metrics.insert("mean".to_string(), estimates.mean.point_estimate);
        metrics.insert("median".to_string(), estimates.median.point_estimate);
        metrics.insert("std_dev".to_string(), estimates.std_dev.point_estimate);

        Ok(metrics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MeasureOutput;

    #[test]
    fn criterion_not_found_error() {
        let adaptor = CriterionAdaptor::new(std::path::Path::new("/nonexistent"), "bench");
        let output = MeasureOutput { stdout: String::new(), stderr: String::new() };
        let err = adaptor.extract(&output).unwrap_err();
        assert!(matches!(err, crate::AdaptorError::CriterionNotFound { .. }));
    }

    #[test]
    fn criterion_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let bench_dir = dir.path().join("bench").join("new");
        std::fs::create_dir_all(&bench_dir).unwrap();
        std::fs::write(bench_dir.join("estimates.json"), b"not valid json").unwrap();
        let adaptor = CriterionAdaptor::new(dir.path(), "bench");
        let output = MeasureOutput { stdout: String::new(), stderr: String::new() };
        let err = adaptor.extract(&output).unwrap_err();
        assert!(matches!(err, crate::AdaptorError::CriterionParse { .. }));
    }
}
