use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use autotune_agent::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession};

/// Configures what the mock implementation agent does when `spawn()` is called.
pub enum ImplBehavior {
    /// Create a dummy file and commit it (makes SHA-before != SHA-after).
    CommitDummy,
    /// Do nothing (the pipeline will record this as a crash).
    NoCommit,
    /// Run a user-provided closure with the working directory.
    Custom(Box<dyn Fn(&Path) + Send + Sync>),
}

struct HypothesisEntry {
    approach: String,
    hypothesis: String,
    files_to_modify: Vec<String>,
}

/// A configurable mock LLM agent for testing the autotune pipeline
/// end-to-end without real LLM calls.
pub struct MockAgent {
    hypotheses: Vec<HypothesisEntry>,
    impl_behavior: ImplBehavior,
    init_responses: Vec<String>,
    // Interior-mutable tracking state
    spawn_count: Mutex<usize>,
    send_count: Mutex<usize>,
    last_spawn_config: Mutex<Option<AgentConfig>>,
    last_send_message: Mutex<Option<String>>,
}

/// Builder for [`MockAgent`].
pub struct MockAgentBuilder {
    hypotheses: Vec<HypothesisEntry>,
    impl_behavior: ImplBehavior,
    init_responses: Vec<String>,
}

impl MockAgentBuilder {
    /// Queue a hypothesis that the research agent will return via `send()`.
    pub fn hypothesis(
        mut self,
        approach: &str,
        hypothesis: &str,
        files_to_modify: &[&str],
    ) -> Self {
        self.hypotheses.push(HypothesisEntry {
            approach: approach.to_string(),
            hypothesis: hypothesis.to_string(),
            files_to_modify: files_to_modify.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// Set the behavior when `spawn()` is called for implementation agents.
    pub fn implementation_behavior(mut self, behavior: ImplBehavior) -> Self {
        self.impl_behavior = behavior;
        self
    }

    /// Queue a JSON response string for the init conversation.
    /// The first call to `spawn()` (non-worktree) returns `init_responses[0]`;
    /// subsequent `send()` calls cycle through the remaining responses.
    pub fn init_response(mut self, json: &str) -> Self {
        self.init_responses.push(json.to_string());
        self
    }

    /// Build the [`MockAgent`].
    pub fn build(self) -> MockAgent {
        MockAgent {
            hypotheses: self.hypotheses,
            impl_behavior: self.impl_behavior,
            init_responses: self.init_responses,
            spawn_count: Mutex::new(0),
            send_count: Mutex::new(0),
            last_spawn_config: Mutex::new(None),
            last_send_message: Mutex::new(None),
        }
    }
}

impl MockAgent {
    /// Create a new builder for configuring a `MockAgent`.
    pub fn builder() -> MockAgentBuilder {
        MockAgentBuilder {
            hypotheses: Vec::new(),
            impl_behavior: ImplBehavior::CommitDummy,
            init_responses: Vec::new(),
        }
    }

    /// Number of times `spawn()` has been called.
    pub fn spawn_count(&self) -> usize {
        *self.spawn_count.lock().unwrap()
    }

    /// Number of times `send()` has been called.
    pub fn send_count(&self) -> usize {
        *self.send_count.lock().unwrap()
    }

    /// Clone of the last `AgentConfig` passed to `spawn()`.
    pub fn last_spawn_config(&self) -> Option<AgentConfig> {
        self.last_spawn_config.lock().unwrap().clone()
    }

    /// Last message passed to `send()`.
    pub fn last_send_message(&self) -> Option<String> {
        self.last_send_message.lock().unwrap().clone()
    }
}

impl Agent for MockAgent {
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        let mut count = self.spawn_count.lock().unwrap();
        let idx = *count;
        *count += 1;
        drop(count);

        *self.last_spawn_config.lock().unwrap() = Some(config.clone());

        // First spawn call may be the research agent initialization (if the
        // caller uses spawn for that). We detect implementation spawns by
        // checking whether the working directory is a git worktree (`.git` is a
        // file, not a directory) — the same heuristic the original inline mock
        // used.
        let wd = &config.working_directory;
        let is_worktree = wd.join(".git").is_file();

        if idx == 0 && !is_worktree {
            let text = if !self.init_responses.is_empty() {
                self.init_responses[0].clone()
            } else {
                "ready".to_string()
            };
            return Ok(AgentResponse {
                text,
                session_id: "mock-session-001".to_string(),
            });
        }

        // This is an implementation spawn.
        match &self.impl_behavior {
            ImplBehavior::CommitDummy => {
                create_dummy_commit(wd, idx);
            }
            ImplBehavior::NoCommit => {
                // Do nothing.
            }
            ImplBehavior::Custom(f) => {
                f(wd);
            }
        }

        Ok(AgentResponse {
            text: "implementation done".to_string(),
            session_id: "mock-session-001".to_string(),
        })
    }

    fn send(&self, _session: &AgentSession, message: &str) -> Result<AgentResponse, AgentError> {
        *self.last_send_message.lock().unwrap() = Some(message.to_string());

        let mut count = self.send_count.lock().unwrap();
        let idx = *count;
        *count += 1;
        drop(count);

        // In init mode, cycle through init_responses. The +1 offset accounts for
        // spawn() having consumed index 0.
        if !self.init_responses.is_empty() {
            let response_idx = (idx + 1) % self.init_responses.len();
            return Ok(AgentResponse {
                text: self.init_responses[response_idx].clone(),
                session_id: "mock-session-001".to_string(),
            });
        }

        // Hypothesis (research) mode.
        let hyp_idx = idx % self.hypotheses.len().max(1);

        if self.hypotheses.is_empty() {
            return Ok(AgentResponse {
                text: r#"{"approach":"default","hypothesis":"no hypothesis configured","files_to_modify":[]}"#.to_string(),
                session_id: "mock-session-001".to_string(),
            });
        }

        let entry = &self.hypotheses[hyp_idx];
        let json = serde_json::json!({
            "approach": entry.approach,
            "hypothesis": entry.hypothesis,
            "files_to_modify": entry.files_to_modify,
        });

        Ok(AgentResponse {
            text: json.to_string(),
            session_id: "mock-session-001".to_string(),
        })
    }

    fn backend_name(&self) -> &str {
        "mock"
    }

    fn handover_command(&self, _session: &AgentSession) -> String {
        "mock-handover".to_string()
    }
}

fn create_dummy_commit(dir: &Path, idx: usize) {
    let dummy = dir.join("mock_change.txt");
    std::fs::write(&dummy, format!("mock change #{idx}")).unwrap();

    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", &format!("mock: implementation #{idx}")])
        .current_dir(dir)
        .output()
        .unwrap();
}
