//! Shared helpers for rendering streaming agent events to stderr.
//!
//! Used by `cmd_run` for the research agent's initial spawn and by the
//! planning phase for each in-loop hypothesis request. The init flow has
//! its own variant (XML-aware) — this one is JSON-aware for the research
//! agent's hypothesis payload.
//!
//! Output model:
//! - An ephemeral status line (dimmed) is shown until the first text/tool event.
//! - Tool use events (`Read`, `Glob`, `Grep`, etc.) render as a single dimmed
//!   line that is overwritten by the next event.
//! - Streaming text is printed as it arrives, typewriter-style.
//! - Once text begins the protocol payload (a leading `{`), the rest of the
//!   response is suppressed so the raw JSON doesn't leak into the terminal.

use autotune_agent::{AgentEvent, EventHandler};
use std::sync::{Arc, Mutex};

/// Build an event handler that forwards streaming agent events to stderr.
///
/// `status` is shown as a dimmed placeholder line until the first real event.
pub fn make_research_event_handler(status: &str) -> EventHandler {
    let has_tool_line = Arc::new(Mutex::new(false));
    let protocol_started = Arc::new(Mutex::new(false));

    {
        use std::io::Write;
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "\r\x1b[2K  \x1b[2m{status}\x1b[0m");
        let _ = stderr.flush();
    }
    *has_tool_line.lock().unwrap() = true;

    let htl = has_tool_line.clone();
    let ps = protocol_started.clone();
    Box::new(move |event| {
        use std::io::Write;
        let mut stderr = std::io::stderr();
        let mut has_tl = htl.lock().unwrap();
        match event {
            AgentEvent::Text(text) => {
                let mut flag = ps.lock().unwrap();
                if *flag {
                    return;
                }
                // Research agent's protocol payload is a JSON hypothesis — once
                // we see a `{` or a fenced block, suppress the rest.
                let trimmed = text.trim_start();
                if trimmed.starts_with('{') || trimmed.starts_with("```") {
                    *flag = true;
                    return;
                }
                if *has_tl {
                    let _ = write!(stderr, "\r\x1b[2K");
                    *has_tl = false;
                }
                let _ = write!(stderr, "{text}");
                let _ = stderr.flush();
            }
            AgentEvent::ToolUse {
                tool,
                input_summary,
            } => {
                if !matches!(
                    tool.as_str(),
                    "Read" | "Glob" | "Grep" | "Bash" | "Edit" | "Write"
                ) {
                    return;
                }
                if *has_tl {
                    let _ = write!(stderr, "\r\x1b[2K");
                } else {
                    let _ = writeln!(stderr);
                }
                let detail = describe_tool_use(&tool, &input_summary);
                let _ = write!(stderr, "  \x1b[2m{detail}\x1b[0m");
                let _ = stderr.flush();
                *has_tl = true;
            }
        }
    })
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

/// Clear the ephemeral status/tool line so subsequent output starts clean.
pub fn clear_status() {
    eprint!("\r\x1b[2K");
    let _ = std::io::Write::flush(&mut std::io::stderr());
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

        // Layer 1: dialoguer puts the terminal in raw mode. If interrupted,
        // Drop on this guard restores.
        let _terminal_guard = autotune_agent::terminal::Guard::new();
        let confirmed = dialoguer::Confirm::new()
            .with_prompt("Allow this tool for the rest of the task run?")
            .default(false)
            .interact()
            .map_err(std::io::Error::other)?;

        Ok(if confirmed {
            autotune_plan::ApprovalDecision::Approve
        } else {
            autotune_plan::ApprovalDecision::Deny
        })
    }
}
