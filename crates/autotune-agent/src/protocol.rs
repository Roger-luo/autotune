use serde::{Deserialize, Serialize};

use autotune_config::{
    AgentConfig as AgentSectionConfig, BenchmarkConfig, ExperimentConfig, PathsConfig, ScoreConfig,
    TestConfig,
};

use crate::AgentError;

/// A structured request from the agent to the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    /// Free-form text to the user. User responds naturally.
    Message { text: String },

    /// Structured question with specific options.
    Question {
        text: String,
        options: Vec<QuestionOption>,
        #[serde(default)]
        allow_free_response: bool,
    },

    /// Propose a config section for validation.
    Config { section: ConfigSection },
}

/// An option in a structured question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub key: String,
    pub description: String,
}

/// A section of the autotune config, proposed incrementally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigSection {
    Experiment(ExperimentConfig),
    Paths(PathsConfig),
    Test(TestConfig),
    Benchmark(BenchmarkConfig),
    Score { value: ScoreConfig },
    Agent(AgentSectionConfig),
}

/// Parse an `AgentRequest` from an agent response that may contain surrounding prose.
/// Uses the same brace-depth scanning pattern as `parse_hypothesis` in `autotune-plan`.
pub fn parse_agent_request(response: &str) -> Result<AgentRequest, AgentError> {
    let mut depth = 0i32;
    let mut start = None;

    for (i, ch) in response.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        let candidate = &response[s..=i];
                        if let Ok(request) = serde_json::from_str::<AgentRequest>(candidate) {
                            return Ok(request);
                        }
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }

    Err(AgentError::ParseFailed {
        message: "no valid JSON agent request found in response".to_string(),
    })
}
