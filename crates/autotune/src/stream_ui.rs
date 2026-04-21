//! Shared helpers for rendering streaming agent events to stderr.
//!
//! Used by `cmd_run` for the research agent's initial spawn, by the planning
//! phase for each in-loop hypothesis request, and by the implementation phase
//! for sandboxed agent runs.
//!
//! Output model:
//! - An ephemeral status line (dimmed) is shown until the first text/tool event.
//! - Tool use events (`Read`, `Glob`, `Grep`, etc.) render as a single dimmed
//!   line that is overwritten by the next event.
//! - Streaming text is **buffered** and rendered as markdown (via `termimad`)
//!   whenever a natural block boundary is reached — a blank line between
//!   paragraphs, or the closing of a fenced code block. Any unflushed text is
//!   rendered on `Stream::finish()`.
//! - For the research agent, once a line begins with `<`, the rest of the
//!   response is treated as the XML protocol payload and suppressed so it
//!   doesn't leak into the terminal.

use autotune_agent::{AgentEvent, EventHandler};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Which kind of protocol payload (if any) to suppress once it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuppressMode {
    /// Research-agent mode — suppress from the first line that starts with `<`.
    Xml,
    /// Implementation-agent mode — render everything.
    None,
}

/// A streaming UI session that forwards agent events to stderr with buffered
/// markdown rendering.
///
/// Create one per agent invocation. Call `handler()` to get an `EventHandler`
/// (you can call it multiple times; all handlers share the same state), pass
/// it to the agent, then call `finish()` after the agent returns to flush any
/// trailing buffered markdown and clear the ephemeral status line.
pub struct Stream {
    state: Arc<Mutex<StreamState>>,
}

impl Stream {
    /// Stream for the research agent. Buffers markdown and suppresses the
    /// `<plan>` / `<request-tool>` XML payload once it begins.
    pub fn research(status: &str) -> Self {
        Self::new(status, SuppressMode::Xml)
    }

    /// Stream for the implementation agent. Buffers markdown; there is no
    /// protocol payload to suppress.
    pub fn implementation(status: &str) -> Self {
        Self::new(status, SuppressMode::None)
    }

    /// Stream for the judge agent. Buffers markdown; no protocol payload to suppress.
    pub fn judge(status: &str) -> Self {
        Self::new(status, SuppressMode::None)
    }

    fn new(status: &str, suppress: SuppressMode) -> Self {
        // Show the dim status line as a permanent header; tool-tail lines
        // will appear below it and be erased/redrawn as events arrive.
        let mut stderr = std::io::stderr();
        let _ = writeln!(stderr, "  \x1b[2m{status}\x1b[0m");
        let _ = stderr.flush();

        Self {
            state: Arc::new(Mutex::new(StreamState::new(suppress))),
        }
    }

    /// Build an `EventHandler` for the underlying agent. Safe to call multiple
    /// times — each returned handler shares the same buffering state.
    pub fn handler(&self) -> EventHandler {
        let state = self.state.clone();
        Box::new(move |event| {
            if let Ok(mut s) = state.lock() {
                s.on_event(event);
            }
        })
    }

    /// Flush any buffered markdown, render a final newline, and clear the
    /// ephemeral status line. Call after the agent returns.
    pub fn finish(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.finish();
        }
    }
}

struct StreamState {
    /// Complete lines waiting to be rendered.
    pending: String,
    /// Partial line currently being accumulated (no trailing newline yet).
    current_line: String,
    /// Whether we are currently inside a ``` fenced code block.
    in_code_fence: bool,
    /// Rolling buffer of the last 3 tool-use descriptions currently shown.
    tool_tail: VecDeque<String>,
    /// How many dim lines we last rendered to stderr (so we can erase them).
    rendered_tail_count: usize,
    /// True once the protocol payload has begun — further text is dropped.
    suppressed: bool,
    /// Protocol-payload detection mode.
    suppress_mode: SuppressMode,
    /// Reusable markdown skin for rendering.
    skin: termimad::MadSkin,
}

impl StreamState {
    fn new(suppress_mode: SuppressMode) -> Self {
        Self {
            pending: String::new(),
            current_line: String::new(),
            in_code_fence: false,
            tool_tail: VecDeque::new(),
            rendered_tail_count: 0,
            suppressed: false,
            suppress_mode,
            skin: termimad::MadSkin::default_dark(),
        }
    }

    fn on_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Text(text) => self.push_text(&text),
            AgentEvent::ToolUse {
                tool,
                input_summary,
            } => self.push_tool_use(&tool, &input_summary),
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.suppressed {
            return;
        }

        for ch in text.chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut self.current_line);
                self.process_line(line);
                if self.suppressed {
                    return;
                }
            } else {
                self.current_line.push(ch);
            }
        }
    }

    /// Handle one complete input line: update fence state, check XML
    /// suppression, append to pending, and flush on paragraph boundaries.
    fn process_line(&mut self, line: String) {
        let trimmed = line.trim();

        // Research-mode protocol payload detection: once we see a line whose
        // first non-whitespace char is `<`, everything from here on is XML we
        // shouldn't render. Only triggers at a block boundary so we don't
        // accidentally swallow an inline `<` inside prose.
        if self.suppress_mode == SuppressMode::Xml
            && !self.suppressed
            && !self.in_code_fence
            && trimmed.starts_with('<')
        {
            self.flush_pending();
            self.suppressed = true;
            return;
        }

        // Track fenced code blocks — we flush *after* the closing fence so the
        // whole block goes through the renderer together.
        let is_fence = trimmed.starts_with("```");
        if is_fence {
            self.in_code_fence = !self.in_code_fence;
        }

        self.pending.push_str(&line);
        self.pending.push('\n');

        let paragraph_break = trimmed.is_empty() && !self.in_code_fence;
        let fence_just_closed = is_fence && !self.in_code_fence;
        if paragraph_break || fence_just_closed {
            self.flush_pending();
        }
    }

    /// Erase the currently rendered tail lines from stderr.
    fn erase_tail(&mut self, stderr: &mut impl Write) {
        if self.rendered_tail_count > 0 {
            let _ = write!(stderr, "\x1b[{}A\x1b[J", self.rendered_tail_count);
            self.rendered_tail_count = 0;
        }
    }

    /// Re-render the current tool tail (up to 3 dim lines).
    fn draw_tail(&mut self, stderr: &mut impl Write) {
        for line in &self.tool_tail {
            let trimmed = if line.len() > 120 { &line[..120] } else { line };
            let _ = writeln!(stderr, "  \x1b[2m{trimmed}\x1b[0m");
        }
        self.rendered_tail_count = self.tool_tail.len();
    }

    fn flush_pending(&mut self) {
        if self.pending.trim().is_empty() {
            self.pending.clear();
            return;
        }
        let md = std::mem::take(&mut self.pending);
        let mut stderr = std::io::stderr();
        self.erase_tail(&mut stderr);
        let _ = self
            .skin
            .write_text_on(&mut stderr, md.trim_end_matches('\n'));
        // Ensure a blank line follows each rendered block so subsequent
        // tool-use lines or blocks don't visually run together.
        let _ = writeln!(stderr);
        self.draw_tail(&mut stderr);
        let _ = stderr.flush();
    }

    fn push_tool_use(&mut self, tool: &str, input_summary: &str) {
        if !matches!(tool, "Read" | "Glob" | "Grep" | "Bash" | "Edit" | "Write") {
            return;
        }
        let detail = describe_tool_use(tool, input_summary);
        self.tool_tail.push_back(detail);
        if self.tool_tail.len() > 3 {
            self.tool_tail.pop_front();
        }
        let mut stderr = std::io::stderr();
        self.erase_tail(&mut stderr);
        self.draw_tail(&mut stderr);
        let _ = stderr.flush();
    }

    fn finish(&mut self) {
        // Flush any partial trailing line.
        if !self.current_line.is_empty() {
            let line = std::mem::take(&mut self.current_line);
            // Don't let XML suppression swallow a legitimate trailing line on
            // finish — process_line applies suppression rules though, which is
            // fine: a trailing `<plan>` fragment without a newline would be
            // suppressed just as it would mid-stream.
            self.process_line(line);
        }
        self.flush_pending();

        // Clear the tool tail if still showing.
        let mut stderr = std::io::stderr();
        self.erase_tail(&mut stderr);
        let _ = stderr.flush();
    }
}

fn describe_tool_use(tool: &str, input: &str) -> String {
    if input.is_empty() {
        format!("{tool}()")
    } else {
        let summary = if input.len() > 60 {
            format!("{}...", &input[..57])
        } else {
            input.to_string()
        };
        format!("{tool}({summary})")
    }
}

/// Build the markdown snippet presented to the user after the research agent
/// proposes a hypothesis. Factored out of `render_hypothesis` so the formatting
/// can be unit-tested without touching stderr.
fn build_hypothesis_markdown(iteration: usize, hypothesis: &autotune_plan::Hypothesis) -> String {
    let mut md = String::new();
    md.push_str(&format!(
        "## Iteration {iteration} — Proposed Hypothesis\n\n"
    ));
    md.push_str(&format!("**Approach:** `{}`\n\n", hypothesis.approach));
    md.push_str(hypothesis.hypothesis.trim());
    md.push('\n');
    if !hypothesis.files_to_modify.is_empty() {
        md.push_str("\n**Files to modify:**\n");
        for f in &hypothesis.files_to_modify {
            md.push_str(&format!("- `{f}`\n"));
        }
    }
    md
}

/// Render the proposed hypothesis to stderr so the user can see what the
/// research agent chose before the implementation phase runs.
pub fn render_hypothesis(iteration: usize, hypothesis: &autotune_plan::Hypothesis) {
    let md = build_hypothesis_markdown(iteration, hypothesis);
    let skin = termimad::MadSkin::default_dark();
    let mut stderr = std::io::stderr();
    let _ = skin.write_text_on(&mut stderr, md.trim_end_matches('\n'));
    let _ = writeln!(stderr);
    let _ = stderr.flush();
}

/// Clear the current terminal line. Used by the tool-approval prompt to wipe
/// any leftover ephemeral status before showing an interactive question.
pub fn clear_status() {
    eprint!("\r\x1b[2K");
    let _ = std::io::stderr().flush();
}

/// Interactive approval for runtime tool requests from the research agent.
///
/// Each request shows the tool + scope + reason and asks the user to allow
/// (for the rest of the task run) or deny. Hard-denied tools are rejected
/// upstream before reaching this type.
pub struct TerminalToolApprover;

impl autotune_plan::ToolApprover for TerminalToolApprover {
    fn approve(
        &self,
        req: &autotune_agent::protocol::ToolRequest,
    ) -> std::io::Result<autotune_plan::ApprovalDecision> {
        clear_status();
        println!();
        println!("[autotune] research agent requests a tool:");
        let scope_str = match &req.scope {
            Some(s) if !s.is_empty() => format!("{}({s})", req.tool),
            _ => req.tool.clone(),
        };
        println!("  tool:   {scope_str}");
        println!("  reason: {}", req.reason);
        autotune_agent::trace::record(
            "approval.prompt",
            serde_json::json!({
                "tool": req.tool,
                "scope": req.scope,
                "reason": req.reason,
            }),
        );

        // Layer 1: dialoguer puts the terminal in raw mode. If interrupted,
        // Drop on this guard restores.
        let _terminal_guard = autotune_agent::terminal::Guard::new();
        let confirmed = dialoguer::Confirm::new()
            .with_prompt("Allow this tool for the rest of the task run?")
            .default(false)
            .interact()
            .map_err(std::io::Error::other)?;

        let decision = if confirmed {
            autotune_plan::ApprovalDecision::Approve
        } else {
            autotune_plan::ApprovalDecision::Deny
        };
        autotune_agent::trace::record(
            "approval.answer",
            serde_json::json!({
                "tool": req.tool,
                "decision": if confirmed { "approve" } else { "deny" },
            }),
        );
        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hypothesis_markdown_includes_approach_and_files() {
        let h = autotune_plan::Hypothesis {
            approach: "inline-cache".to_string(),
            hypothesis: "Inlining the cache reduces overhead.".to_string(),
            files_to_modify: vec!["src/cache.rs".into(), "src/main.rs".into()],
        };
        let md = build_hypothesis_markdown(7, &h);
        assert!(md.contains("Iteration 7"));
        assert!(md.contains("`inline-cache`"));
        assert!(md.contains("Inlining the cache reduces overhead."));
        assert!(md.contains("- `src/cache.rs`"));
        assert!(md.contains("- `src/main.rs`"));
    }

    #[test]
    fn hypothesis_markdown_omits_files_section_when_empty() {
        let h = autotune_plan::Hypothesis {
            approach: "noop".to_string(),
            hypothesis: "Nothing to change.".to_string(),
            files_to_modify: vec![],
        };
        let md = build_hypothesis_markdown(1, &h);
        assert!(!md.contains("Files to modify"));
    }

    #[test]
    fn describe_tool_use_no_input() {
        assert_eq!(describe_tool_use("Read", ""), "Read()");
    }

    #[test]
    fn describe_tool_use_short_input() {
        assert_eq!(
            describe_tool_use("Glob", "src/**/*.rs"),
            "Glob(src/**/*.rs)"
        );
    }

    #[test]
    fn describe_tool_use_long_input_truncated() {
        let long_input = "a".repeat(70);
        let result = describe_tool_use("Grep", &long_input);
        // The result is "{tool}({summary})" where summary ends with "..."
        // so result ends with "...)" not "..."
        assert!(
            result.contains("..."),
            "expected truncation marker in: {result}"
        );
        assert!(
            result.starts_with("Grep("),
            "expected Grep( prefix: {result}"
        );
    }

    #[test]
    fn stream_research_handler_processes_text_events() {
        let stream = Stream::research("loading...");
        let handler = stream.handler();
        // Text with a newline flushes a line through process_line
        handler(AgentEvent::Text("hello\n".to_string()));
        // Blank line triggers flush_pending
        handler(AgentEvent::Text("\n".to_string()));
        // Trailing text without newline
        handler(AgentEvent::Text("trailing".to_string()));
        stream.finish();
    }

    #[test]
    fn stream_implementation_handler_processes_tool_use() {
        let stream = Stream::implementation("running...");
        let handler = stream.handler();
        // Known tool — exercises push_tool_use main path
        handler(AgentEvent::ToolUse {
            tool: "Read".to_string(),
            input_summary: "src/main.rs".to_string(),
        });
        // Unknown tool — exercises early return in push_tool_use
        handler(AgentEvent::ToolUse {
            tool: "UnknownTool".to_string(),
            input_summary: "ignored".to_string(),
        });
        stream.finish();
    }

    #[test]
    fn stream_research_suppresses_xml_payload() {
        let stream = Stream::research("thinking...");
        let handler = stream.handler();
        // Normal prose first
        handler(AgentEvent::Text("Some reasoning.\n".to_string()));
        handler(AgentEvent::Text("\n".to_string()));
        // XML line triggers suppression in research mode
        handler(AgentEvent::Text("<plan>\n".to_string()));
        handler(AgentEvent::Text("  <approach>x</approach>\n".to_string()));
        stream.finish();
    }

    #[test]
    fn stream_research_suppresses_xml_after_buffered_prose() {
        let mut state = StreamState::new(SuppressMode::Xml);
        state.process_line("Some reasoning.".to_string());
        state.process_line("<plan>".to_string());

        assert!(state.suppressed, "XML should suppress even after prose");
        assert!(
            !state.pending.contains("<plan>"),
            "protocol XML must not leak into pending rendered content"
        );
    }

    #[test]
    fn stream_processes_fenced_code_block() {
        let stream = Stream::implementation("building...");
        let handler = stream.handler();
        handler(AgentEvent::Text("```rust\n".to_string()));
        handler(AgentEvent::Text("fn main() {}\n".to_string()));
        handler(AgentEvent::Text("```\n".to_string()));
        stream.finish();
    }

    #[test]
    fn stream_finish_flushes_partial_line() {
        let stream = Stream::implementation("done");
        let handler = stream.handler();
        // No trailing newline — finish should flush the partial line
        handler(AgentEvent::Text("no newline at end".to_string()));
        stream.finish();
    }
}
