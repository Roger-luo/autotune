use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use autotune_agent::{Agent, AgentConfig, AgentError, AgentResponse, AgentSession, ToolPermission};

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
    /// Raw verbatim responses for the research agent, consumed in order
    /// across `spawn()` + `send()`. When non-empty, these take precedence
    /// over `hypotheses` and let tests inject arbitrary XML (e.g. a
    /// `<request-tool>` fragment, malformed XML, or a `<plan>` with
    /// surrounding prose).
    research_responses: Vec<String>,
    // Interior-mutable tracking state
    spawn_count: Mutex<usize>,
    send_count: Mutex<usize>,
    /// Next index into `research_responses` to return.
    research_turn: Mutex<usize>,
    last_spawn_config: Mutex<Option<AgentConfig>>,
    last_send_message: Mutex<Option<String>>,
    /// History of all spawn configs (prompt + permissions + model).
    spawn_configs: Mutex<Vec<AgentConfig>>,
    /// History of all send messages.
    send_messages: Mutex<Vec<String>>,
    /// Permissions granted via `grant_session_permission`.
    granted_permissions: Mutex<Vec<ToolPermission>>,
}

/// Builder for [`MockAgent`].
pub struct MockAgentBuilder {
    hypotheses: Vec<HypothesisEntry>,
    impl_behavior: ImplBehavior,
    init_responses: Vec<String>,
    research_responses: Vec<String>,
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

    /// Queue a raw verbatim research-agent response (e.g. a `<plan>` or
    /// `<request-tool>` XML fragment, or arbitrary text). Responses are
    /// consumed in order across the research agent's `spawn()` + `send()`
    /// calls. When any are set, they take precedence over `hypothesis()`.
    pub fn research_response(mut self, raw: &str) -> Self {
        self.research_responses.push(raw.to_string());
        self
    }

    /// Build the [`MockAgent`].
    pub fn build(self) -> MockAgent {
        MockAgent {
            hypotheses: self.hypotheses,
            impl_behavior: self.impl_behavior,
            init_responses: self.init_responses,
            research_responses: self.research_responses,
            spawn_count: Mutex::new(0),
            send_count: Mutex::new(0),
            research_turn: Mutex::new(0),
            last_spawn_config: Mutex::new(None),
            last_send_message: Mutex::new(None),
            spawn_configs: Mutex::new(Vec::new()),
            send_messages: Mutex::new(Vec::new()),
            granted_permissions: Mutex::new(Vec::new()),
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
            research_responses: Vec::new(),
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

    /// All configs passed to `spawn()`, in order.
    pub fn spawn_configs(&self) -> Vec<AgentConfig> {
        self.spawn_configs.lock().unwrap().clone()
    }

    /// All messages passed to `send()`, in order.
    pub fn send_messages(&self) -> Vec<String> {
        self.send_messages.lock().unwrap().clone()
    }

    /// All permissions granted via `grant_session_permission()`.
    pub fn granted_permissions(&self) -> Vec<ToolPermission> {
        self.granted_permissions.lock().unwrap().clone()
    }
}

impl Agent for MockAgent {
    fn spawn(&self, config: &AgentConfig) -> Result<AgentResponse, AgentError> {
        let mut count = self.spawn_count.lock().unwrap();
        let idx = *count;
        *count += 1;
        drop(count);

        *self.last_spawn_config.lock().unwrap() = Some(config.clone());
        self.spawn_configs.lock().unwrap().push(config.clone());

        // First spawn call may be the research agent initialization (if the
        // caller uses spawn for that). We detect implementation spawns by
        // checking whether the working directory is a git worktree (`.git` is a
        // file, not a directory) — the same heuristic the original inline mock
        // used.
        let wd = &config.working_directory;
        let is_worktree = wd.join(".git").is_file();

        if idx == 0 && !is_worktree {
            // Priority order for the research-agent's initial spawn response:
            // 1. Programmable `research_response()` queue (lets tests inject
            //    `<request-tool>` fragments, malformed XML, etc. verbatim).
            // 2. Legacy `init_response()` queue (used by the init flow).
            // 3. Plain "ready".
            let text = if !self.research_responses.is_empty() {
                let mut turn = self.research_turn.lock().unwrap();
                let t = *turn;
                *turn += 1;
                self.research_responses[t.min(self.research_responses.len() - 1)].clone()
            } else if !self.init_responses.is_empty() {
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
        self.send_messages.lock().unwrap().push(message.to_string());

        let mut count = self.send_count.lock().unwrap();
        let idx = *count;
        *count += 1;
        drop(count);

        // Programmable research script takes priority — consume the next
        // entry in the research_responses queue (repeating the last one if
        // the queue is drained so long runs don't explode).
        if !self.research_responses.is_empty() {
            let mut turn = self.research_turn.lock().unwrap();
            let t = *turn;
            *turn += 1;
            let pick = t.min(self.research_responses.len() - 1);
            return Ok(AgentResponse {
                text: self.research_responses[pick].clone(),
                session_id: "mock-session-001".to_string(),
            });
        }

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
                text: "<plan><approach>default</approach>\
                       <hypothesis>no hypothesis configured</hypothesis>\
                       <files-to-modify></files-to-modify></plan>"
                    .to_string(),
                session_id: "mock-session-001".to_string(),
            });
        }

        let entry = &self.hypotheses[hyp_idx];
        let mut xml = String::new();
        xml.push_str("<plan>");
        xml.push_str("<approach>");
        xml.push_str(&xml_escape(&entry.approach));
        xml.push_str("</approach>");
        xml.push_str("<hypothesis>");
        xml.push_str(&xml_escape(&entry.hypothesis));
        xml.push_str("</hypothesis>");
        xml.push_str("<files-to-modify>");
        for f in &entry.files_to_modify {
            xml.push_str("<file>");
            xml.push_str(&xml_escape(f));
            xml.push_str("</file>");
        }
        xml.push_str("</files-to-modify>");
        xml.push_str("</plan>");

        Ok(AgentResponse {
            text: xml,
            session_id: "mock-session-001".to_string(),
        })
    }

    fn backend_name(&self) -> &str {
        "mock"
    }

    fn handover_command(&self, _session: &AgentSession) -> String {
        "mock-handover".to_string()
    }

    fn grant_session_permission(
        &self,
        _session: &AgentSession,
        permission: ToolPermission,
    ) -> Result<(), AgentError> {
        self.granted_permissions.lock().unwrap().push(permission);
        Ok(())
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
