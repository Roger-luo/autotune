use crate::{AdaptorError, MeasureOutput, MetricAdaptor, Metrics};

/// Configuration for a single regex pattern.
#[derive(Debug, Clone)]
pub struct RegexPatternConfig {
    pub name: String,
    pub pattern: String,
}

/// Extracts metrics from measure output using regex capture groups.
pub struct RegexAdaptor {
    patterns: Vec<RegexPatternConfig>,
}

impl RegexAdaptor {
    pub fn new(patterns: Vec<RegexPatternConfig>) -> Self {
        Self { patterns }
    }
}

impl MetricAdaptor for RegexAdaptor {
    fn extract(&self, output: &MeasureOutput) -> Result<Metrics, AdaptorError> {
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        let mut metrics = Metrics::new();

        for pat in &self.patterns {
            let re =
                ::regex::Regex::new(&pat.pattern).map_err(|source| AdaptorError::RegexCompile {
                    pattern: pat.pattern.clone(),
                    source,
                })?;

            let caps = re
                .captures(&combined)
                .ok_or_else(|| AdaptorError::RegexNoMatch {
                    name: pat.name.clone(),
                    pattern: pat.pattern.clone(),
                })?;

            let value_str = caps
                .name("value")
                .or_else(|| caps.get(1))
                .ok_or_else(|| AdaptorError::RegexNoMatch {
                    name: pat.name.clone(),
                    pattern: pat.pattern.clone(),
                })?
                .as_str();

            let value = value_str
                .parse::<f64>()
                .map_err(|_| AdaptorError::ParseFloat {
                    name: pat.name.clone(),
                    value: value_str.to_string(),
                })?;

            metrics.insert(pat.name.clone(), value);
        }

        Ok(metrics)
    }
}
