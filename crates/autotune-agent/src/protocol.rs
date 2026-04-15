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

// ---------------------------------------------------------------------------
// Lenient tag extraction — used by all top-level parsers
// ---------------------------------------------------------------------------

/// A substring match found by [`lenient_find_all`].
#[derive(Debug, Clone, Copy)]
pub struct TagMatch<'a> {
    /// Byte offset of the opening `<tag>` in the source string.
    pub start: usize,
    /// Content between `<tag>` and `</tag>`, not including the tags themselves.
    pub inner: &'a str,
    /// Full `<tag>…</tag>` substring including both tags.
    pub outer: &'a str,
}

/// Find all `<tag>…</tag>` occurrences by literal substring matching.
///
/// Returns one [`TagMatch`] per pair found, in document order. Does not attempt
/// XML parsing — `inner` may contain any characters, including unescaped `<`,
/// `>`, and `&`. Unterminated opens (no matching `</tag>`) are silently
/// skipped rather than treated as errors.
///
/// Only matches literal `<tag>` opens; attribute-bearing opens like
/// `<tag foo="bar">` are not recognized (none of our protocol tags use
/// attributes).
pub fn lenient_find_all<'a>(text: &'a str, tag: &str) -> Vec<TagMatch<'a>> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut matches = Vec::new();
    let mut cursor = 0;
    while let Some(rel_open) = text[cursor..].find(&open) {
        let open_start = cursor + rel_open;
        let content_start = open_start + open.len();
        if let Some(rel_close) = text[content_start..].find(&close) {
            let inner_end = content_start + rel_close;
            let outer_end = inner_end + close.len();
            matches.push(TagMatch {
                start: open_start,
                inner: &text[content_start..inner_end],
                outer: &text[open_start..outer_end],
            });
            cursor = outer_end;
        } else {
            // Unterminated — skip this occurrence and keep scanning.
            cursor = content_start;
        }
    }
    matches
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

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

/// Top-level fragment tags that may contain free-form prose. A
/// `<request-tool>` occurrence nested inside any of these spans is assumed to
/// be illustrative example text (e.g. the research agent writing
/// "`<request-tool>…</request-tool>`" inside a `<hypothesis>` while describing
/// a test case) — not a real top-level request — and is skipped by
/// [`parse_tool_requests`].
const WRAPPER_TAGS: &[&str] = &[
    "plan", "message", "question", "task", "paths", "test", "measure", "score", "agent",
];

/// Parse any `<request-tool>` top-level fragments in an agent response.
///
/// Only `<request-tool>` blocks matter here — everything else is prose, plans,
/// or fragments handled by a different parser. In practice the rest of the
/// response is full of things that aren't well-formed XML: agents routinely
/// embed Rust type signatures (`&Value`, `Vec<T>`), code snippets with `<` and
/// `&`, and markdown tables. Walking the whole document with strict quick-xml
/// blows up on the first such occurrence and takes down the tune loop.
///
/// So we do a lenient string scan: find each literal `<request-tool>` …
/// `</request-tool>` pair and run the strict parser only on that substring.
/// Whatever sits between pairs is ignored verbatim, regardless of whether it
/// parses as XML.
///
/// Matches that sit inside another known wrapper fragment (see
/// [`WRAPPER_TAGS`]) are skipped: the research agent legitimately writes
/// literal `<request-tool>` examples inside a `<plan>`'s `<hypothesis>` when
/// describing tests, and those must not be parsed as real requests.
///
/// Attribute-bearing opens (`<request-tool foo="bar">`) are not supported —
/// the schema doesn't use attributes on this tag, and accepting them here
/// would force us back onto the strict walker.
pub fn parse_tool_requests(response: &str) -> Result<Vec<ToolRequest>, AgentError> {
    // Gather byte-ranges of all wrapper fragments so we can filter out
    // `<request-tool>` matches nested inside them.
    let mut wrapper_spans: Vec<(usize, usize)> = Vec::new();
    for tag in WRAPPER_TAGS {
        for m in lenient_find_all(response, tag) {
            let end = m.start + m.outer.len();
            wrapper_spans.push((m.start, end));
        }
    }

    let mut requests = Vec::new();
    for m in lenient_find_all(response, "request-tool") {
        let nested = wrapper_spans
            .iter()
            .any(|&(s, e)| m.start >= s && m.start < e);
        if nested {
            continue;
        }
        requests.push(parse_fragment_strict(
            m.outer,
            "request-tool",
            parse_tool_request,
        )?);
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
/// Uses lenient substring extraction at the top level: each known tag
/// (`<message>`, `<question>`, `<task>`, `<paths>`, `<test>`, `<measure>`,
/// `<score>`, `<agent>`) is found by literal string matching. The strict XML
/// parser runs only on each matched fragment's content, so garbage prose
/// between fragments (unescaped `<`, `&`, Rust generics, markdown tables)
/// is harmlessly ignored.
///
/// Fragments are returned in document order (sorted by byte offset).
/// Unknown tags and unmatched text are silently skipped.
pub fn parse_agent_response(response: &str) -> Result<Vec<AgentFragment>, AgentError> {
    const KNOWN: &[&str] = &[
        "message", "question", "task", "paths", "test", "measure", "score", "agent",
    ];

    // Collect all matches for every known tag, then sort by document position.
    let mut all: Vec<(usize, &str, &str)> = Vec::new(); // (start, tag, outer)
    for tag in KNOWN {
        for m in lenient_find_all(response, tag) {
            all.push((m.start, tag, m.outer));
        }
    }
    all.sort_by_key(|&(start, _, _)| start);

    let mut fragments = Vec::new();
    for (_, tag, outer) in all {
        let frag = match tag {
            "message" => parse_fragment_strict(outer, "message", |r| {
                Ok(AgentFragment::Message(read_text(r, "message")?))
            })?,
            "question" => parse_fragment_strict(outer, "question", parse_question)?,
            "task" => {
                parse_fragment_strict(outer, "task", |r| Ok(AgentFragment::Task(parse_task(r)?)))?
            }
            "paths" => parse_fragment_strict(outer, "paths", |r| {
                Ok(AgentFragment::Paths(parse_paths(r)?))
            })?,
            "test" => {
                parse_fragment_strict(outer, "test", |r| Ok(AgentFragment::Test(parse_test(r)?)))?
            }
            "measure" => parse_fragment_strict(outer, "measure", |r| {
                Ok(AgentFragment::Measure(parse_measure(r)?))
            })?,
            "score" => parse_fragment_strict(outer, "score", |r| {
                Ok(AgentFragment::Score(parse_score(r)?))
            })?,
            "agent" => parse_fragment_strict(outer, "agent", |r| {
                Ok(AgentFragment::Agent(parse_agent(r)?))
            })?,
            _ => unreachable!(),
        };
        fragments.push(frag);
    }

    Ok(fragments)
}

// ---------------------------------------------------------------------------
// Fragment dispatch
// ---------------------------------------------------------------------------

/// Parse a single fragment whose full text (including outer tags) is in
/// `fragment`. Creates a fresh strict Reader, skips to the opening `<tag>`
/// event, then delegates to `parser` which consumes everything up to and
/// including the closing `</tag>`.
fn parse_fragment_strict<T, F>(fragment: &str, tag: &str, parser: F) -> Result<T, AgentError>
where
    F: FnOnce(&mut Reader<&[u8]>) -> Result<T, AgentError>,
{
    let mut reader = Reader::from_str(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = tag_name(&e)?;
                if name == tag {
                    return parser(&mut reader);
                }
            }
            Ok(Event::Eof) => {
                return Err(AgentError::ParseFailed {
                    message: format!("no <{tag}> found in fragment"),
                });
            }
            Ok(_) => {}
            Err(e) => {
                return Err(AgentError::ParseFailed {
                    message: format!("XML parse error in <{tag}>: {e}"),
                });
            }
        }
        buf.clear();
    }
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
    let mut max_fix_attempts: Option<u32> = None;
    let mut max_fresh_spawns: Option<u32> = None;

    walk_children(reader, tag, |child, reader| {
        match child {
            "backend" => backend = Some(read_text(reader, "backend")?),
            "model" => model = Some(read_text(reader, "model")?),
            "max-turns" => max_turns = Some(parse_u64(&read_text(reader, "max-turns")?)?),
            "max-fix-attempts" => {
                max_fix_attempts = Some(parse_u64(&read_text(reader, "max-fix-attempts")?)? as u32);
            }
            "max-fresh-spawns" => {
                max_fresh_spawns = Some(parse_u64(&read_text(reader, "max-fresh-spawns")?)? as u32);
            }
            other => skip_element(reader, other)?,
        }
        Ok(())
    })?;

    Ok(AgentRoleConfig {
        backend,
        model,
        max_turns,
        max_fix_attempts,
        max_fresh_spawns,
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

#[cfg(test)]
mod tests {
    use super::*;
    use autotune_config::{AdaptorConfig, Direction, ScoreConfig, StopValue};

    #[test]
    fn parse_measure_with_criterion_adaptor() {
        let xml = r#"<measure><name>bench</name><command><segment>cargo</segment><segment>bench</segment></command><adaptor><type>criterion</type><measure-name>bench/sort</measure-name></adaptor></measure>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Measure(m) => {
                assert_eq!(m.name, "bench");
                assert!(
                    matches!(&m.adaptor, AdaptorConfig::Criterion { measure_name } if measure_name == "bench/sort")
                );
            }
            _ => panic!("expected Measure"),
        }
    }

    #[test]
    fn parse_measure_with_script_adaptor() {
        let xml = r#"<measure><name>custom</name><command><segment>sh</segment></command><adaptor><type>script</type><command><segment>sh</segment><segment>-c</segment><segment>cat</segment></command></adaptor></measure>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Measure(m) => {
                assert!(
                    matches!(&m.adaptor, AdaptorConfig::Script { command } if command == &["sh", "-c", "cat"])
                );
            }
            _ => panic!("expected Measure"),
        }
    }

    #[test]
    fn parse_measure_missing_adaptor_errors() {
        let xml = r#"<measure><name>x</name><command><segment>sh</segment></command></measure>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("adaptor"), "error was: {err}");
    }

    #[test]
    fn parse_adaptor_unknown_type_errors() {
        let xml = r#"<measure><name>x</name><command><segment>sh</segment></command><adaptor><type>unknown_xyz</type></adaptor></measure>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("unknown_xyz"), "error was: {err}");
    }

    #[test]
    fn parse_score_threshold() {
        let xml = r#"<score><type>threshold</type><condition><metric>latency_ms</metric><direction>Minimize</direction><threshold>5.0</threshold></condition></score>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Score(ScoreConfig::Threshold { conditions }) => {
                assert_eq!(conditions.len(), 1);
                assert_eq!(conditions[0].metric, "latency_ms");
                assert!(matches!(conditions[0].direction, Direction::Minimize));
                assert_eq!(conditions[0].threshold, 5.0);
            }
            _ => panic!("expected Threshold score"),
        }
    }

    #[test]
    fn parse_score_script() {
        let xml = r#"<score><type>script</type><command><segment>sh</segment><segment>-c</segment><segment>echo</segment></command></score>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Score(ScoreConfig::Script { command }) => {
                assert_eq!(command, &["sh", "-c", "echo"]);
            }
            _ => panic!("expected Script score"),
        }
    }

    #[test]
    fn parse_score_command() {
        let xml = r#"<score><type>command</type><command><segment>./score.sh</segment></command></score>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Score(ScoreConfig::Command { command }) => {
                assert_eq!(command, &["./score.sh"]);
            }
            _ => panic!("expected Command score"),
        }
    }

    #[test]
    fn parse_score_missing_type_errors() {
        let xml = r#"<score><primary-metric><name>m</name><direction>Maximize</direction><weight>1.0</weight></primary-metric></score>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("type"), "error was: {err}");
    }

    #[test]
    fn parse_score_unknown_type_errors() {
        let xml = r#"<score><type>neural_net</type></score>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("neural_net"), "error was: {err}");
    }

    #[test]
    fn parse_score_weighted_sum_with_guardrail_and_minimize() {
        let xml = r#"<score><type>weighted_sum</type><primary-metric><name>latency_ms</name><direction>Minimize</direction><weight>2.0</weight></primary-metric><guardrail-metric><name>accuracy</name><direction>Maximize</direction><max-regression>0.05</max-regression></guardrail-metric></score>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Score(ScoreConfig::WeightedSum {
                primary_metrics,
                guardrail_metrics,
            }) => {
                assert_eq!(primary_metrics[0].direction, Direction::Minimize);
                assert_eq!(primary_metrics[0].weight, 2.0);
                assert_eq!(guardrail_metrics[0].name, "accuracy");
                assert_eq!(guardrail_metrics[0].max_regression, 0.05);
            }
            _ => panic!("expected WeightedSum score"),
        }
    }

    #[test]
    fn parse_agent_fragment() {
        let xml = r#"<agent><backend>claude</backend><research><model>claude-opus</model><max-turns>20</max-turns></research><implementation><backend>claude</backend></implementation></agent>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Agent(agent) => {
                assert_eq!(agent.backend, "claude");
                let research = agent.research.as_ref().unwrap();
                assert_eq!(research.model.as_deref(), Some("claude-opus"));
                assert_eq!(research.max_turns, Some(20));
                assert_eq!(
                    agent.implementation.as_ref().unwrap().backend.as_deref(),
                    Some("claude")
                );
            }
            _ => panic!("expected Agent"),
        }
    }

    #[test]
    fn parse_test_fragment() {
        let xml = r#"<test><name>unit</name><command><segment>cargo</segment><segment>nextest</segment><segment>run</segment></command><timeout>120</timeout></test>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Test(t) => {
                assert_eq!(t.name, "unit");
                assert_eq!(t.command, vec!["cargo", "nextest", "run"]);
                assert_eq!(t.timeout, 120);
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn parse_bool_false_values() {
        for val in &["false", "no", "0"] {
            let xml = format!(
                r#"<question><text>q</text><allow-free-response>{val}</allow-free-response></question>"#
            );
            let frags = parse_agent_response(&xml).unwrap();
            match &frags[0] {
                AgentFragment::Question {
                    allow_free_response,
                    ..
                } => {
                    assert!(!allow_free_response, "expected false for '{val}'");
                }
                _ => panic!("expected Question"),
            }
        }
    }

    #[test]
    fn parse_task_invalid_stop_value_errors() {
        let xml = r#"<task><name>t</name><max-iterations>not_a_number</max-iterations></task>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("not_a_number"), "error was: {err}");
    }

    #[test]
    fn lenient_find_all_unterminated_is_skipped() {
        let matches = lenient_find_all("<foo>no close", "foo");
        assert!(matches.is_empty());
    }

    #[test]
    fn lenient_find_all_finds_multiple() {
        let text = "<foo>first</foo> prose <foo>second</foo>";
        let matches = lenient_find_all(text, "foo");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].inner, "first");
        assert_eq!(matches[1].inner, "second");
    }

    #[test]
    fn parse_bool_yes_and_one_values() {
        for val in &["yes", "1"] {
            let xml = format!(
                r#"<question><text>q</text><allow-free-response>{val}</allow-free-response></question>"#
            );
            let frags = parse_agent_response(&xml).unwrap();
            match &frags[0] {
                AgentFragment::Question { allow_free_response, .. } => {
                    assert!(*allow_free_response, "expected true for '{val}'");
                }
                _ => panic!("expected Question"),
            }
        }
    }

    #[test]
    fn parse_bool_invalid_errors() {
        let xml = r#"<question><text>q</text><allow-free-response>maybe</allow-free-response></question>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("invalid boolean"), "error was: {err}");
    }

    #[test]
    fn parse_u64_invalid_errors() {
        let xml = r#"<test><name>t</name><command><segment>sh</segment></command><timeout>not_a_number</timeout></test>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("invalid integer"), "error was: {err}");
    }

    #[test]
    fn parse_f64_invalid_errors() {
        let xml = r#"<score><type>weighted_sum</type><primary-metric><name>m</name><direction>Maximize</direction><weight>not_a_float</weight></primary-metric></score>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("invalid number"), "error was: {err}");
    }

    #[test]
    fn parse_direction_invalid_errors() {
        let xml = r#"<score><type>threshold</type><condition><metric>m</metric><direction>Sideways</direction><threshold>5.0</threshold></condition></score>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("Sideways"), "error was: {err}");
    }

    #[test]
    fn parse_task_with_description_and_extra_fields() {
        let xml = r#"<task><name>my-task</name><description>desc text</description><canonical-branch>main</canonical-branch><max-iterations>10</max-iterations><target-improvement>0.05</target-improvement><max-duration>1h</max-duration></task>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Task(task) => {
                assert_eq!(task.description.as_deref(), Some("desc text"));
                assert_eq!(task.target_improvement, Some(0.05));
                assert_eq!(task.max_duration.as_deref(), Some("1h"));
            }
            _ => panic!("expected Task"),
        }
    }

    #[test]
    fn parse_criterion_adaptor_missing_measure_name_errors() {
        let xml = r#"<measure><name>x</name><command><segment>sh</segment></command><adaptor><type>criterion</type></adaptor></measure>"#;
        let err = parse_agent_response(xml).unwrap_err();
        assert!(err.to_string().contains("criterion"), "error was: {err}");
    }

    #[test]
    fn parse_agent_with_fix_and_spawn_budgets() {
        let xml = r#"<agent><backend>claude</backend><implementation><max-fix-attempts>3</max-fix-attempts><max-fresh-spawns>2</max-fresh-spawns></implementation></agent>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Agent(agent) => {
                let impl_cfg = agent.implementation.as_ref().unwrap();
                assert_eq!(impl_cfg.max_fix_attempts, Some(3));
                assert_eq!(impl_cfg.max_fresh_spawns, Some(2));
            }
            _ => panic!("expected Agent"),
        }
    }

    #[test]
    fn parse_agent_with_init_section() {
        let xml = r#"<agent><backend>claude</backend><init><model>claude-haiku</model></init></agent>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Agent(agent) => {
                let init_cfg = agent.init.as_ref().unwrap();
                assert_eq!(init_cfg.model.as_deref(), Some("claude-haiku"));
            }
            _ => panic!("expected Agent"),
        }
    }

    #[test]
    fn parse_stop_value_finite_number() {
        let xml = r#"<task><name>t</name><max-iterations>5</max-iterations></task>"#;
        let frags = parse_agent_response(xml).unwrap();
        match &frags[0] {
            AgentFragment::Task(task) => {
                assert!(matches!(task.max_iterations, Some(StopValue::Finite(5))));
            }
            _ => panic!("expected Task"),
        }
    }
}
