//! XML-based wire protocol between the init agent and the CLI.
//!
//! The agent emits XML tags in its response. Each turn can produce multiple
//! top-level fragments in any order. The parser walks top-level elements and
//! returns an `AgentFragment` per element, ignoring any surrounding prose.
//!
//! # Schema
//!
//! See `autotune-init/src/prompt.rs` for the full schema sent to the agent.
//! Top-level fragments: `<message>`, `<question>`, `<task>`, `<paths>`,
//! `<test>`, `<measure>`, `<score>`, `<agent>`.
//!
//! Inside a tag, child elements are read; text/CDATA is joined verbatim.
//! Quoting scalars is neither required nor allowed — the tag's position
//! in the schema determines its type.

use autotune_config::{
    AdaptorConfig, AgentConfig as AgentSectionConfig, AgentRoleConfig, Direction, GuardrailMetric,
    MeasureConfig, PathsConfig, PrimaryMetric, RegexPattern, ScoreConfig, StopValue, TargetMetric,
    TaskConfig, TestConfig, ThresholdCondition,
};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::AgentError;

/// A question option rendered as a selection menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub key: String,
    pub label: String,
    pub description: Option<String>,
}

/// A request from an agent to be granted additional tool access for its session.
/// Emitted as a `<request-tool>` XML fragment in the agent response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    /// Tool name, e.g. "Bash", "WebFetch".
    pub tool: String,
    /// Optional scope (e.g. "cargo tree:*" for Bash). When None, unscoped Allow.
    pub scope: Option<String>,
    /// Why the agent wants this tool — shown verbatim to the user.
    pub reason: String,
}

/// Parse any `<request-tool>` top-level fragments in an agent response.
/// Ignores all other content. Safe to call on responses that may or may not
/// contain tool requests (returns an empty Vec if none found).
pub fn parse_tool_requests(response: &str) -> Result<Vec<ToolRequest>, AgentError> {
    let mut reader = Reader::from_str(response);
    reader.config_mut().trim_text(false);
    let mut requests = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = tag_name(&e)?;
                if name == "request-tool" {
                    requests.push(parse_tool_request(&mut reader)?);
                } else {
                    skip_element(&mut reader, &name)?;
                }
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(AgentError::ParseFailed {
                    message: format!("XML parse error: {e}"),
                });
            }
        }
        buf.clear();
    }
    Ok(requests)
}

fn parse_tool_request(reader: &mut Reader<&[u8]>) -> Result<ToolRequest, AgentError> {
    let mut tool = String::new();
    let mut scope: Option<String> = None;
    let mut reason = String::new();

    walk_children(reader, "request-tool", |tag, reader| {
        match tag {
            "tool" => tool = read_text(reader, "tool")?,
            "scope" => scope = Some(read_text(reader, "scope")?),
            "reason" => reason = read_text(reader, "reason")?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    if tool.is_empty() {
        return Err(AgentError::ParseFailed {
            message: "<request-tool> missing <tool>".to_string(),
        });
    }
    if reason.is_empty() {
        return Err(AgentError::ParseFailed {
            message: format!("<request-tool> for '{tool}' is missing <reason>"),
        });
    }
    Ok(ToolRequest {
        tool,
        scope,
        reason,
    })
}

/// One top-level fragment emitted by the agent during a single turn.
/// A turn may produce zero or more of these.
#[derive(Debug, Clone)]
pub enum AgentFragment {
    /// Free-form prose to show the user; user types a reply.
    Message(String),
    /// Structured question with selectable options.
    Question {
        text: String,
        options: Vec<QuestionOption>,
        allow_free_response: bool,
    },
    /// A proposed `[task]` section.
    Task(TaskConfig),
    /// A proposed `[paths]` section.
    Paths(PathsConfig),
    /// A proposed `[[test]]` entry.
    Test(TestConfig),
    /// A proposed `[[measure]]` entry.
    Measure(MeasureConfig),
    /// A proposed `[score]` section.
    Score(ScoreConfig),
    /// A proposed `[agent]` section.
    Agent(AgentSectionConfig),
}

/// Parse an agent response into zero or more fragments.
///
/// Prose outside recognised top-level tags is ignored. Unknown top-level tags
/// are skipped. Malformed XML or bad fragment contents return `ParseFailed`.
pub fn parse_agent_response(response: &str) -> Result<Vec<AgentFragment>, AgentError> {
    let mut reader = Reader::from_str(response);
    reader.config_mut().trim_text(false);
    let mut fragments = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = tag_name(&e)?;
                let frag = match name.as_str() {
                    "message" => AgentFragment::Message(read_text(&mut reader, "message")?),
                    "question" => parse_question(&mut reader)?,
                    "task" => AgentFragment::Task(parse_task(&mut reader)?),
                    "paths" => AgentFragment::Paths(parse_paths(&mut reader)?),
                    "test" => AgentFragment::Test(parse_test(&mut reader)?),
                    "measure" => AgentFragment::Measure(parse_measure(&mut reader)?),
                    "score" => AgentFragment::Score(parse_score(&mut reader)?),
                    "agent" => AgentFragment::Agent(parse_agent(&mut reader)?),
                    other => {
                        skip_element(&mut reader, other)?;
                        continue;
                    }
                };
                fragments.push(frag);
            }
            Ok(Event::Empty(e)) => {
                // Self-closing top-level tag — accept only scalar-style ones as empty strings.
                let name = tag_name(&e)?;
                if let "message" = name.as_str() {
                    fragments.push(AgentFragment::Message(String::new()));
                }
                // Other empty top-level tags are ignored (they'd be invalid sections anyway).
            }
            Ok(Event::Eof) => break,
            Ok(_) => {} // text/comment/decl outside any tag — skip
            Err(e) => {
                return Err(AgentError::ParseFailed {
                    message: format!("XML parse error: {e}"),
                });
            }
        }
        buf.clear();
    }

    Ok(fragments)
}

// ---------------------------------------------------------------------------
// Tag parsers
// ---------------------------------------------------------------------------

fn parse_question(reader: &mut Reader<&[u8]>) -> Result<AgentFragment, AgentError> {
    let mut text = String::new();
    let mut options: Vec<QuestionOption> = Vec::new();
    let mut allow_free_response = false;

    walk_children(reader, "question", |name, reader| {
        match name {
            "text" => text = read_text(reader, "text")?,
            "option" => options.push(parse_option(reader)?),
            "allow-free-response" => {
                allow_free_response = parse_bool(&read_text(reader, "allow-free-response")?)?
            }
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(AgentFragment::Question {
        text,
        options,
        allow_free_response,
    })
}

fn parse_option(reader: &mut Reader<&[u8]>) -> Result<QuestionOption, AgentError> {
    let mut key = String::new();
    let mut label = String::new();
    let mut description: Option<String> = None;

    walk_children(reader, "option", |name, reader| {
        match name {
            "key" => key = read_text(reader, "key")?,
            "label" => label = read_text(reader, "label")?,
            "description" => description = Some(read_text(reader, "description")?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(QuestionOption {
        key,
        label,
        description,
    })
}

fn parse_task(reader: &mut Reader<&[u8]>) -> Result<TaskConfig, AgentError> {
    let mut name = String::new();
    let mut description: Option<String> = None;
    let mut canonical_branch = "main".to_string();
    let mut max_iterations: Option<StopValue> = None;
    let mut target_improvement: Option<f64> = None;
    let mut max_duration: Option<String> = None;
    let mut target_metric: Vec<TargetMetric> = Vec::new();

    walk_children(reader, "task", |tag, reader| {
        match tag {
            "name" => name = read_text(reader, "name")?,
            "description" => description = Some(read_text(reader, "description")?),
            "canonical-branch" => canonical_branch = read_text(reader, "canonical-branch")?,
            "max-iterations" => {
                let s = read_text(reader, "max-iterations")?;
                max_iterations = Some(parse_stop_value(&s)?);
            }
            "target-improvement" => {
                target_improvement = Some(parse_f64(&read_text(reader, "target-improvement")?)?);
            }
            "max-duration" => max_duration = Some(read_text(reader, "max-duration")?),
            "target-metric" => target_metric.push(parse_target_metric(reader)?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(TaskConfig {
        name,
        description,
        canonical_branch,
        max_iterations,
        target_improvement,
        max_duration,
        target_metric,
    })
}

fn parse_target_metric(reader: &mut Reader<&[u8]>) -> Result<TargetMetric, AgentError> {
    let mut name = String::new();
    let mut value = 0.0;
    let mut direction = Direction::Maximize;

    walk_children(reader, "target-metric", |tag, reader| {
        match tag {
            "name" => name = read_text(reader, "name")?,
            "value" => value = parse_f64(&read_text(reader, "value")?)?,
            "direction" => direction = parse_direction(&read_text(reader, "direction")?)?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(TargetMetric {
        name,
        value,
        direction,
    })
}

fn parse_paths(reader: &mut Reader<&[u8]>) -> Result<PathsConfig, AgentError> {
    let mut tunable: Vec<String> = Vec::new();
    let mut denied: Vec<String> = Vec::new();

    walk_children(reader, "paths", |tag, reader| {
        match tag {
            "tunable" => tunable.push(read_text(reader, "tunable")?),
            "denied" => denied.push(read_text(reader, "denied")?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(PathsConfig { tunable, denied })
}

fn parse_test(reader: &mut Reader<&[u8]>) -> Result<TestConfig, AgentError> {
    let mut name = String::new();
    let mut command: Vec<String> = Vec::new();
    let mut timeout: Option<u64> = None;

    walk_children(reader, "test", |tag, reader| {
        match tag {
            "name" => name = read_text(reader, "name")?,
            "command" => command = parse_command(reader, "command")?,
            "timeout" => timeout = Some(parse_u64(&read_text(reader, "timeout")?)?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(TestConfig {
        name,
        command,
        timeout: timeout.unwrap_or(300),
    })
}

fn parse_measure(reader: &mut Reader<&[u8]>) -> Result<MeasureConfig, AgentError> {
    let mut name = String::new();
    let mut command: Vec<String> = Vec::new();
    let mut timeout: Option<u64> = None;
    let mut adaptor: Option<AdaptorConfig> = None;

    walk_children(reader, "measure", |tag, reader| {
        match tag {
            "name" => name = read_text(reader, "name")?,
            "command" => command = parse_command(reader, "command")?,
            "timeout" => timeout = Some(parse_u64(&read_text(reader, "timeout")?)?),
            "adaptor" => adaptor = Some(parse_adaptor(reader)?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    let adaptor = adaptor.ok_or_else(|| AgentError::ParseFailed {
        message: format!("measure '{name}' is missing <adaptor>"),
    })?;

    Ok(MeasureConfig {
        name,
        command,
        timeout: timeout.unwrap_or(600),
        adaptor,
    })
}

fn parse_adaptor(reader: &mut Reader<&[u8]>) -> Result<AdaptorConfig, AgentError> {
    let mut type_tag: Option<String> = None;
    let mut patterns: Vec<RegexPattern> = Vec::new();
    let mut measure_name: Option<String> = None;
    let mut script_command: Vec<String> = Vec::new();

    walk_children(reader, "adaptor", |tag, reader| {
        match tag {
            "type" => type_tag = Some(read_text(reader, "type")?),
            "pattern" => patterns.push(parse_pattern(reader)?),
            "measure-name" => measure_name = Some(read_text(reader, "measure-name")?),
            "command" => script_command = parse_command(reader, "command")?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    let type_tag = type_tag.ok_or_else(|| AgentError::ParseFailed {
        message: "adaptor is missing <type>".to_string(),
    })?;

    match type_tag.as_str() {
        "regex" => Ok(AdaptorConfig::Regex { patterns }),
        "criterion" => {
            let measure_name = measure_name.ok_or_else(|| AgentError::ParseFailed {
                message: "adaptor type=criterion requires <measure-name>".to_string(),
            })?;
            Ok(AdaptorConfig::Criterion { measure_name })
        }
        "script" => Ok(AdaptorConfig::Script {
            command: script_command,
        }),
        other => Err(AgentError::ParseFailed {
            message: format!("unknown adaptor type '{other}'"),
        }),
    }
}

fn parse_pattern(reader: &mut Reader<&[u8]>) -> Result<RegexPattern, AgentError> {
    let mut name = String::new();
    let mut regex = String::new();

    walk_children(reader, "pattern", |tag, reader| {
        match tag {
            "name" => name = read_text(reader, "name")?,
            "regex" => regex = read_text(reader, "regex")?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(RegexPattern {
        name,
        pattern: regex,
    })
}

fn parse_score(reader: &mut Reader<&[u8]>) -> Result<ScoreConfig, AgentError> {
    let mut type_tag: Option<String> = None;
    let mut primary_metrics: Vec<PrimaryMetric> = Vec::new();
    let mut guardrail_metrics: Vec<GuardrailMetric> = Vec::new();
    let mut conditions: Vec<ThresholdCondition> = Vec::new();
    let mut command: Vec<String> = Vec::new();

    walk_children(reader, "score", |tag, reader| {
        match tag {
            "type" => type_tag = Some(read_text(reader, "type")?),
            "primary-metric" => primary_metrics.push(parse_primary_metric(reader)?),
            "guardrail-metric" => guardrail_metrics.push(parse_guardrail_metric(reader)?),
            "condition" => conditions.push(parse_threshold_condition(reader)?),
            "command" => command = parse_command(reader, "command")?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    let type_tag = type_tag.ok_or_else(|| AgentError::ParseFailed {
        message: "score is missing <type>".to_string(),
    })?;

    match type_tag.as_str() {
        "weighted_sum" => Ok(ScoreConfig::WeightedSum {
            primary_metrics,
            guardrail_metrics,
        }),
        "threshold" => Ok(ScoreConfig::Threshold { conditions }),
        "script" => Ok(ScoreConfig::Script { command }),
        "command" => Ok(ScoreConfig::Command { command }),
        other => Err(AgentError::ParseFailed {
            message: format!("unknown score type '{other}'"),
        }),
    }
}

fn parse_primary_metric(reader: &mut Reader<&[u8]>) -> Result<PrimaryMetric, AgentError> {
    let mut name = String::new();
    let mut direction = Direction::Maximize;
    let mut weight = 1.0;

    walk_children(reader, "primary-metric", |tag, reader| {
        match tag {
            "name" => name = read_text(reader, "name")?,
            "direction" => direction = parse_direction(&read_text(reader, "direction")?)?,
            "weight" => weight = parse_f64(&read_text(reader, "weight")?)?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(PrimaryMetric {
        name,
        direction,
        weight,
    })
}

fn parse_guardrail_metric(reader: &mut Reader<&[u8]>) -> Result<GuardrailMetric, AgentError> {
    let mut name = String::new();
    let mut direction = Direction::Minimize;
    let mut max_regression = 0.0;

    walk_children(reader, "guardrail-metric", |tag, reader| {
        match tag {
            "name" => name = read_text(reader, "name")?,
            "direction" => direction = parse_direction(&read_text(reader, "direction")?)?,
            "max-regression" => max_regression = parse_f64(&read_text(reader, "max-regression")?)?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(GuardrailMetric {
        name,
        direction,
        max_regression,
    })
}

fn parse_threshold_condition(reader: &mut Reader<&[u8]>) -> Result<ThresholdCondition, AgentError> {
    let mut metric = String::new();
    let mut direction = Direction::Maximize;
    let mut threshold = 0.0;

    walk_children(reader, "condition", |tag, reader| {
        match tag {
            "metric" => metric = read_text(reader, "metric")?,
            "direction" => direction = parse_direction(&read_text(reader, "direction")?)?,
            "threshold" => threshold = parse_f64(&read_text(reader, "threshold")?)?,
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(ThresholdCondition {
        metric,
        direction,
        threshold,
    })
}

fn parse_agent(reader: &mut Reader<&[u8]>) -> Result<AgentSectionConfig, AgentError> {
    let mut backend: Option<String> = None;
    let mut research: Option<AgentRoleConfig> = None;
    let mut implementation: Option<AgentRoleConfig> = None;
    let mut init: Option<AgentRoleConfig> = None;

    walk_children(reader, "agent", |tag, reader| {
        match tag {
            "backend" => backend = Some(read_text(reader, "backend")?),
            "research" => research = Some(parse_agent_role(reader, "research")?),
            "implementation" => implementation = Some(parse_agent_role(reader, "implementation")?),
            "init" => init = Some(parse_agent_role(reader, "init")?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(AgentSectionConfig {
        backend: backend.unwrap_or_else(|| "claude".to_string()),
        research,
        implementation,
        init,
    })
}

fn parse_agent_role(reader: &mut Reader<&[u8]>, tag: &str) -> Result<AgentRoleConfig, AgentError> {
    let mut backend: Option<String> = None;
    let mut model: Option<String> = None;
    let mut max_turns: Option<u64> = None;

    walk_children(reader, tag, |child, reader| {
        match child {
            "backend" => backend = Some(read_text(reader, "backend")?),
            "model" => model = Some(read_text(reader, "model")?),
            "max-turns" => max_turns = Some(parse_u64(&read_text(reader, "max-turns")?)?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(AgentRoleConfig {
        backend,
        model,
        max_turns,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk the immediate child elements of the current open tag, calling `on_child`
/// with each child's name. Text and CDATA between child tags are ignored (the
/// schema doesn't use mixed content at the parent level).
fn walk_children<F>(
    reader: &mut Reader<&[u8]>,
    parent: &str,
    mut on_child: F,
) -> Result<(), AgentError>
where
    F: FnMut(&str, &mut Reader<&[u8]>) -> Result<(), AgentError>,
{
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = tag_name(&e)?;
                on_child(&name, reader)?;
            }
            Ok(Event::Empty(e)) => {
                // Self-closing child treated as empty-string leaf: ignore for
                // structural children; scalar leaves are handled via Start.
                let _ = tag_name(&e);
            }
            Ok(Event::End(e)) => {
                let name = tag_name_end(&e)?;
                if name == parent {
                    return Ok(());
                }
                // Closing tag for something we didn't open — treat as end of parent.
                return Err(AgentError::ParseFailed {
                    message: format!("unexpected closing tag </{name}> while in <{parent}>"),
                });
            }
            Ok(Event::Eof) => {
                return Err(AgentError::ParseFailed {
                    message: format!("unexpected EOF inside <{parent}>"),
                });
            }
            Ok(_) => {} // text/CDATA/comment between children — skip
            Err(e) => {
                return Err(AgentError::ParseFailed {
                    message: format!("XML parse error inside <{parent}>: {e}"),
                });
            }
        }
        buf.clear();
    }
}

/// Read all text/CDATA inside the current open tag until its closing tag.
/// Nested elements are flattened to their text content.
fn read_text(reader: &mut Reader<&[u8]>, tag: &str) -> Result<String, AgentError> {
    let mut out = String::new();
    let mut buf = Vec::new();
    let mut depth = 0i32;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(_)) => {
                depth += 1;
            }
            Ok(Event::End(e)) => {
                let name = tag_name_end(&e)?;
                if depth == 0 {
                    if name == tag {
                        return Ok(out.trim().to_string());
                    }
                    return Err(AgentError::ParseFailed {
                        message: format!("unexpected closing tag </{name}> while reading <{tag}>"),
                    });
                }
                depth -= 1;
            }
            Ok(Event::Empty(_)) => {
                // Self-closing inside a text field — ignore.
            }
            Ok(Event::Text(t)) => {
                let s = t.unescape().map_err(|e| AgentError::ParseFailed {
                    message: format!("text unescape failed in <{tag}>: {e}"),
                })?;
                out.push_str(&s);
            }
            Ok(Event::CData(c)) => {
                let s = std::str::from_utf8(c.as_ref()).map_err(|e| AgentError::ParseFailed {
                    message: format!("CDATA utf8 error in <{tag}>: {e}"),
                })?;
                out.push_str(s);
            }
            Ok(Event::Eof) => {
                return Err(AgentError::ParseFailed {
                    message: format!("unexpected EOF inside <{tag}>"),
                });
            }
            Ok(_) => {}
            Err(e) => {
                return Err(AgentError::ParseFailed {
                    message: format!("XML parse error inside <{tag}>: {e}"),
                });
            }
        }
        buf.clear();
    }
}

/// Parse a `<command>` element containing `<segment>` children.
fn parse_command(reader: &mut Reader<&[u8]>, tag: &str) -> Result<Vec<String>, AgentError> {
    let mut segments: Vec<String> = Vec::new();
    walk_children(reader, tag, |child, reader| {
        match child {
            "segment" => segments.push(read_text(reader, "segment")?),
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;
    Ok(segments)
}

/// Skip all events until the matching closing tag at the current depth.
fn skip_element(reader: &mut Reader<&[u8]>, tag: &str) -> Result<(), AgentError> {
    let mut depth = 0i32;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(e)) => {
                if depth == 0 {
                    let name = tag_name_end(&e)?;
                    if name == tag {
                        return Ok(());
                    }
                    return Err(AgentError::ParseFailed {
                        message: format!("unexpected closing tag </{name}> while skipping <{tag}>"),
                    });
                }
                depth -= 1;
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Eof) => {
                return Err(AgentError::ParseFailed {
                    message: format!("unexpected EOF while skipping <{tag}>"),
                });
            }
            Ok(_) => {}
            Err(e) => {
                return Err(AgentError::ParseFailed {
                    message: format!("XML parse error while skipping <{tag}>: {e}"),
                });
            }
        }
        buf.clear();
    }
}

fn tag_name(e: &BytesStart) -> Result<String, AgentError> {
    std::str::from_utf8(e.name().as_ref())
        .map(|s| s.to_string())
        .map_err(|err| AgentError::ParseFailed {
            message: format!("non-utf8 tag name: {err}"),
        })
}

fn tag_name_end(e: &quick_xml::events::BytesEnd) -> Result<String, AgentError> {
    std::str::from_utf8(e.name().as_ref())
        .map(|s| s.to_string())
        .map_err(|err| AgentError::ParseFailed {
            message: format!("non-utf8 tag name: {err}"),
        })
}

fn parse_bool(s: &str) -> Result<bool, AgentError> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" | "" => Ok(false),
        other => Err(AgentError::ParseFailed {
            message: format!("invalid boolean '{other}'"),
        }),
    }
}

fn parse_u64(s: &str) -> Result<u64, AgentError> {
    s.trim()
        .parse::<u64>()
        .map_err(|e| AgentError::ParseFailed {
            message: format!("invalid integer '{s}': {e}"),
        })
}

fn parse_f64(s: &str) -> Result<f64, AgentError> {
    s.trim()
        .parse::<f64>()
        .map_err(|e| AgentError::ParseFailed {
            message: format!("invalid number '{s}': {e}"),
        })
}

fn parse_direction(s: &str) -> Result<Direction, AgentError> {
    match s.trim() {
        "Maximize" => Ok(Direction::Maximize),
        "Minimize" => Ok(Direction::Minimize),
        other => Err(AgentError::ParseFailed {
            message: format!("invalid direction '{other}' (expected Maximize or Minimize)"),
        }),
    }
}

fn parse_stop_value(s: &str) -> Result<StopValue, AgentError> {
    let s = s.trim();
    if s == "inf" {
        Ok(StopValue::Infinite)
    } else {
        s.parse::<u64>()
            .map(StopValue::Finite)
            .map_err(|e| AgentError::ParseFailed {
                message: format!("invalid max-iterations '{s}': {e}"),
            })
    }
}
