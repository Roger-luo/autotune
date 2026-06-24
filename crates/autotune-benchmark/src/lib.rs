use autotune_adaptor::criterion::CriterionAdaptor;
use autotune_adaptor::regex::{RegexAdaptor, RegexPatternConfig};
use autotune_adaptor::{MetricAdaptor, Metrics};
use autotune_agent::aprintln;

// Re-export for consumers that need to work with build_adaptor
pub use autotune_adaptor::MeasureOutput;
// Re-export the variance types so consumers (the CLI) can map noise estimates
// without taking a direct dependency on autotune-adaptor.
pub use autotune_adaptor::{MetricVariance, Variances};
use autotune_config::{AdaptorConfig, MeasureConfig};
use autotune_judge::{
    Rubric, ScoreRange, Subject, SubjectContext, SubjectContextKind, parse_batch_response,
    render_batch_prompt,
};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Errors returned by measure execution and metric extraction.
#[derive(Debug, Error)]
pub enum MeasureError {
    #[error("measure '{name}' command failed (exit code {code}): {stderr}")]
    CommandFailed {
        name: String,
        code: i32,
        stderr: String,
    },

    #[error("measure '{name}' IO error: {source}")]
    Io {
        name: String,
        source: std::io::Error,
    },

    #[error("measure '{name}' timed out after {timeout} seconds")]
    TimedOut { name: String, timeout: u64 },

    #[error("metric extraction failed for measure '{name}': {source}")]
    Extraction {
        name: String,
        source: autotune_adaptor::AdaptorError,
    },
}

/// Result of running a single measure: the raw stdout/stderr plus the
/// extracted metrics. Raw output is retained so callers can persist it for
/// later inspection (e.g. by a research agent looking for context beyond the
/// summary metrics).
#[derive(Debug, Clone)]
pub struct MeasureReport {
    pub name: String,
    pub stdout: String,
    pub stderr: String,
    pub metrics: Metrics,
    /// Per-metric noise estimates extracted alongside `metrics`, when the
    /// adaptor supplies them (criterion CI/stddev). Empty for regex/script.
    pub variances: Variances,
}

/// Factory that creates a streaming handler for one judge invocation.
/// Receives a status string; returns an event handler and a finish closure.
pub type JudgeStreamFactory =
    dyn Fn(&str) -> (autotune_agent::EventHandler, Box<dyn FnOnce()>) + Send + Sync;

/// Carries the judge agent and its config into the measuring phase.
pub struct JudgeContext<'a> {
    pub agent: &'a dyn autotune_agent::Agent,
    pub agent_config: autotune_agent::AgentConfig,
    /// Optional factory called once per judge invocation. Receives a status
    /// string and returns an event handler plus a finish closure to call after
    /// the agent returns (clears ephemeral status lines, flushes buffered text).
    pub make_stream: Option<Box<JudgeStreamFactory>>,
}

/// Run a judge measure: optionally run a command, build a subject, call the
/// batch judge, and return one metric per rubric ID.
pub fn run_judge_measure(
    config: &MeasureConfig,
    working_dir: &Path,
    approach_name: &str,
    iteration: u32,
    ctx: &JudgeContext,
) -> Result<MeasureReport, MeasureError> {
    let AdaptorConfig::Judge {
        persona,
        rubrics: rubric_configs,
    } = &config.adaptor
    else {
        panic!("run_judge_measure called on non-judge measure");
    };

    let (cmd_stdout, cmd_stderr) = if config.command.is_some() {
        let output = run_command_with_timeout(config, working_dir, &[])?;
        if !output.status.success() {
            return Err(MeasureError::CommandFailed {
                name: config.name.clone(),
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        (
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    } else {
        (String::new(), String::new())
    };

    let mut context = vec![
        SubjectContext {
            kind: SubjectContextKind::Note,
            label: "iteration".to_string(),
            body: iteration.to_string(),
        },
        SubjectContext {
            kind: SubjectContextKind::Note,
            label: "approach".to_string(),
            body: approach_name.to_string(),
        },
    ];
    if !cmd_stdout.is_empty() || !cmd_stderr.is_empty() {
        context.push(SubjectContext {
            kind: SubjectContextKind::SourceSnippet,
            label: "command_output".to_string(),
            body: format!("{cmd_stdout}\n{cmd_stderr}"),
        });
    }

    let subject = Subject::new(&config.name, approach_name).with_context(context);

    let rubrics: Vec<Rubric> = rubric_configs
        .iter()
        .map(|r| Rubric {
            id: r.id.clone(),
            title: r.title.clone(),
            persona: persona.clone(),
            score_range: ScoreRange {
                min: r.score_range.min,
                max: r.score_range.max,
            },
            instruction: r.instruction.clone(),
            guidance: r.guidance.clone(),
        })
        .collect();

    let prompt = render_batch_prompt(persona, &subject, &rubrics);

    let mut agent_cfg = ctx.agent_config.clone();
    agent_cfg.prompt = prompt;

    let model = ctx.agent_config.model.as_deref().unwrap_or("default");
    aprintln!("[autotune] judge '{}': model={}", config.name, model);

    let (maybe_handler, maybe_finish) = if let Some(ref factory) = ctx.make_stream {
        let status = format!("judge '{}' evaluating...", config.name);
        let (h, f) = factory(&status);
        (Some(h), Some(f))
    } else {
        (None, None)
    };

    let config_with_events = autotune_agent::AgentConfigWithEvents::new(agent_cfg);
    let config_with_events = match maybe_handler {
        Some(handler) => config_with_events.with_event_handler(handler),
        None => config_with_events,
    };

    let response =
        ctx.agent
            .spawn_streaming(config_with_events)
            .map_err(|e| MeasureError::Extraction {
                name: config.name.clone(),
                source: autotune_adaptor::AdaptorError::Io {
                    source: std::io::Error::other(format!("judge agent call failed: {e}")),
                },
            })?;

    if let Some(finish) = maybe_finish {
        finish();
    }

    let assessments =
        parse_batch_response(&rubrics, &response.text).map_err(|e| MeasureError::Extraction {
            name: config.name.clone(),
            source: autotune_adaptor::AdaptorError::Io {
                source: std::io::Error::other(format!("batch response parse failed: {e}")),
            },
        })?;

    let metrics: Metrics = assessments
        .iter()
        .map(|a| (a.rubric_id.clone(), a.score as f64))
        .collect();

    Ok(MeasureReport {
        name: config.name.clone(),
        stdout: cmd_stdout,
        stderr: cmd_stderr,
        metrics,
        // Judge scores carry no dispersion estimate.
        variances: Variances::new(),
    })
}

/// Run a single measure command and extract metrics.
pub fn run_measure(config: &MeasureConfig, working_dir: &Path) -> Result<Metrics, MeasureError> {
    run_measure_with_output(config, working_dir).map(|r| r.metrics)
}

/// Run a single measure command, returning the extracted metrics along with
/// the raw stdout/stderr captured during the run.
pub fn run_measure_with_output(
    config: &MeasureConfig,
    working_dir: &Path,
) -> Result<MeasureReport, MeasureError> {
    run_measure_with_output_env(config, working_dir, &[])
}

/// Like [`run_measure_with_output`], but injects extra environment variables
/// into the measure command's process. Used by baseline replication (option 1)
/// to force a rebuild between replicates via a per-replicate `RUSTFLAGS` value
/// (which perturbs Cargo's build fingerprint so codegen/layout actually
/// changes). The empty-slice path is identical to the original behavior.
pub fn run_measure_with_output_env(
    config: &MeasureConfig,
    working_dir: &Path,
    extra_env: &[(String, String)],
) -> Result<MeasureReport, MeasureError> {
    let output = run_command_with_timeout(config, working_dir, extra_env)?;

    if !output.status.success() {
        return Err(MeasureError::CommandFailed {
            name: config.name.clone(),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let bench_output = MeasureOutput {
        stdout: stdout.clone(),
        stderr: stderr.clone(),
    };

    // Criterion writes its results under `<CARGO_TARGET_DIR>/criterion/...`.
    // When the task injects a shared `CARGO_TARGET_DIR` (PR #22), that is NOT
    // `<working_dir>/target`, so the adaptor must resolve estimates.json under
    // the EFFECTIVE target dir the bench actually built into.
    let target_dir = effective_target_dir(working_dir, extra_env);
    let adaptor = build_adaptor_with_target_dir(&config.adaptor, working_dir, &target_dir);
    let metrics = adaptor
        .extract(&bench_output)
        .map_err(|source| MeasureError::Extraction {
            name: config.name.clone(),
            source,
        })?;
    // Best-effort: an adaptor that can't compute dispersion returns an empty
    // map by default. A variance-extraction error must not fail a measure that
    // already produced valid metrics, so fall back to empty on error.
    let variances = adaptor.extract_variances(&bench_output).unwrap_or_default();

    Ok(MeasureReport {
        name: config.name.clone(),
        stdout,
        stderr,
        metrics,
        variances,
    })
}

/// Run all configured measures and merge their metrics.
pub fn run_all_measures(
    configs: &[MeasureConfig],
    working_dir: &Path,
    approach_name: &str,
    iteration: u32,
    judge_ctx: Option<&JudgeContext>,
) -> Result<Metrics, MeasureError> {
    run_all_measures_with_output(configs, working_dir, approach_name, iteration, judge_ctx)
        .map(|(metrics, _)| metrics)
}

/// Run all configured measures, returning the merged metrics and the per-measure
/// raw output reports (in the order the measures were configured).
pub fn run_all_measures_with_output(
    configs: &[MeasureConfig],
    working_dir: &Path,
    approach_name: &str,
    iteration: u32,
    judge_ctx: Option<&JudgeContext>,
) -> Result<(Metrics, Vec<MeasureReport>), MeasureError> {
    run_all_measures_with_output_env(
        configs,
        working_dir,
        approach_name,
        iteration,
        judge_ctx,
        &[],
    )
}

/// Like [`run_all_measures_with_output`], but injects extra environment
/// variables into each non-judge measure command's process. Used by baseline
/// replication (option 1) to force a rebuild between replicates. Judge measures
/// ignore the env (they don't shell out to a build). The empty-slice path is
/// identical to [`run_all_measures_with_output`].
pub fn run_all_measures_with_output_env(
    configs: &[MeasureConfig],
    working_dir: &Path,
    approach_name: &str,
    iteration: u32,
    judge_ctx: Option<&JudgeContext>,
    extra_env: &[(String, String)],
) -> Result<(Metrics, Vec<MeasureReport>), MeasureError> {
    let mut all_metrics = HashMap::new();
    let mut reports = Vec::with_capacity(configs.len());

    for config in configs {
        let report = match &config.adaptor {
            AdaptorConfig::Judge { .. } => {
                let ctx = judge_ctx.ok_or_else(|| MeasureError::Extraction {
                    name: config.name.clone(),
                    source: autotune_adaptor::AdaptorError::Io {
                        source: std::io::Error::other(
                            "judge adaptor requires a JudgeContext but none was provided",
                        ),
                    },
                })?;
                run_judge_measure(config, working_dir, approach_name, iteration, ctx)?
            }
            _ => run_measure_with_output_env(config, working_dir, extra_env)?,
        };
        all_metrics.extend(report.metrics.clone());
        reports.push(report);
    }

    Ok((all_metrics, reports))
}

/// The directory cargo actually builds into for this measure: the injected
/// `CARGO_TARGET_DIR` (PR #22 shares one per task) if `extra_env` carries it,
/// else the conventional `<working_dir>/target`. Criterion writes its results
/// under `<this>/criterion/...`, so result resolution must use the same base.
pub fn effective_target_dir(working_dir: &Path, extra_env: &[(String, String)]) -> PathBuf {
    extra_env
        .iter()
        .find(|(k, _)| k == "CARGO_TARGET_DIR")
        .map(|(_, v)| PathBuf::from(v))
        .unwrap_or_else(|| working_dir.join("target"))
}

/// Translate the config benchmark list into the adaptor's entry type, then
/// build a [`CriterionAdaptor`] rooted at `<target_dir>/criterion`.
fn criterion_adaptor_for(
    benchmarks: &[autotune_config::CriterionBenchmark],
    target_dir: &Path,
) -> CriterionAdaptor {
    use autotune_adaptor::criterion::{CriterionBenchmarkEntry, CriterionStat};
    let criterion_dir = target_dir.join("criterion");
    let entries = benchmarks
        .iter()
        .map(|b| CriterionBenchmarkEntry {
            name: b.name.clone(),
            group: b.group.clone(),
            stat: match b.stat {
                autotune_config::CriterionStat::Mean => CriterionStat::Mean,
                autotune_config::CriterionStat::Median => CriterionStat::Median,
                autotune_config::CriterionStat::StdDev => CriterionStat::StdDev,
            },
        })
        .collect();
    CriterionAdaptor::new(&criterion_dir, entries)
}

/// For a criterion measure, resolve the on-disk `estimates.json` files it
/// reads so a caller can copy them into the iteration dir for post-hoc
/// analysis (the iteration worktree is removed after integration). Returns
/// `(metric_name, source_path)` pairs. Non-criterion measures yield nothing.
///
/// Resolves under `<working_dir>/target/criterion`. Use
/// [`criterion_estimates_files_with_env`] when the bench ran with an injected
/// `CARGO_TARGET_DIR` (PR #22 shared target dir).
pub fn criterion_estimates_files(
    config: &MeasureConfig,
    working_dir: &Path,
) -> Vec<(String, PathBuf)> {
    criterion_estimates_files_with_env(config, working_dir, &[])
}

/// Like [`criterion_estimates_files`], but resolves under the EFFECTIVE target
/// dir given the `extra_env` the bench ran with (honoring `CARGO_TARGET_DIR`).
/// The empty-slice path is identical to [`criterion_estimates_files`].
pub fn criterion_estimates_files_with_env(
    config: &MeasureConfig,
    working_dir: &Path,
    extra_env: &[(String, String)],
) -> Vec<(String, PathBuf)> {
    let AdaptorConfig::Criterion { benchmarks } = &config.adaptor else {
        return Vec::new();
    };
    let target_dir = effective_target_dir(working_dir, extra_env);
    criterion_adaptor_for(benchmarks, &target_dir).estimates_files()
}

/// Merge the per-measure `variances` maps from a slice of reports into one map
/// keyed by metric name (mirrors how `run_all_measures_with_output` merges
/// `metrics`). Later measures win on a name collision — but metric names are
/// validated unique across measures, so collisions don't happen in practice.
pub fn merge_variances(reports: &[MeasureReport]) -> Variances {
    let mut merged = Variances::new();
    for report in reports {
        merged.extend(report.variances.clone());
    }
    merged
}

/// Build a MetricAdaptor from config, resolving criterion results under
/// `<working_dir>/target/criterion`. For a measure that ran with an injected
/// `CARGO_TARGET_DIR`, use [`build_adaptor_with_target_dir`] so criterion finds
/// its estimates.json under the redirected target dir (PR #22 shared target).
pub fn build_adaptor(config: &AdaptorConfig, working_dir: &Path) -> Box<dyn MetricAdaptor> {
    build_adaptor_with_target_dir(config, working_dir, &working_dir.join("target"))
}

/// Build a MetricAdaptor from config, rooting criterion result resolution at
/// `<target_dir>/criterion`. `target_dir` is the EFFECTIVE Cargo target dir the
/// bench built into — `<working_dir>/target` by default, or the injected
/// `CARGO_TARGET_DIR` when the task shares one per-task target dir (PR #22).
/// `working_dir` is still used for the script adaptor's process cwd.
pub fn build_adaptor_with_target_dir(
    config: &AdaptorConfig,
    working_dir: &Path,
    target_dir: &Path,
) -> Box<dyn MetricAdaptor> {
    match config {
        AdaptorConfig::Regex { patterns } => {
            let configs: Vec<RegexPatternConfig> = patterns
                .iter()
                .map(|pattern| RegexPatternConfig {
                    name: pattern.name.clone(),
                    pattern: pattern.pattern.clone(),
                })
                .collect();
            Box::new(RegexAdaptor::new(configs))
        }
        AdaptorConfig::Criterion { benchmarks } => {
            Box::new(criterion_adaptor_for(benchmarks, target_dir))
        }
        AdaptorConfig::Script { command } => Box::new(ScriptAdaptorWithWorkingDir::new(
            command.clone(),
            working_dir.to_path_buf(),
        )),
        AdaptorConfig::Judge { .. } => {
            // Judge adaptor is handled at a higher level; this path should not be reached.
            panic!("build_adaptor called for Judge adaptor — use the judge pipeline instead");
        }
    }
}

fn run_command_with_timeout(
    config: &MeasureConfig,
    working_dir: &Path,
    extra_env: &[(String, String)],
) -> Result<Output, MeasureError> {
    let command = config.command.as_deref().ok_or_else(|| MeasureError::Io {
        name: config.name.clone(),
        source: std::io::Error::other("measure command is required but not set"),
    })?;
    let program = &command[0];
    let args = &command[1..];

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|source| MeasureError::Io {
        name: config.name.clone(),
        source,
    })?;

    let tail = autotune_agent::terminal::LiveTail::stderr();

    let stdout_tail = tail.clone();
    let stdout_handle = spawn_line_reader(child.stdout.take(), move |line| {
        stdout_tail.push_line(line);
    });

    let stderr_tail = tail.clone();
    let stderr_handle = spawn_line_reader(child.stderr.take(), move |line| {
        stderr_tail.push_line(line);
    });

    let result = match wait_for_child(config, &mut child) {
        Ok(status) => collect_output(config, status, stdout_handle, stderr_handle),
        Err(err) => {
            let _ = join_reader(config, stdout_handle);
            let _ = join_reader(config, stderr_handle);
            Err(err)
        }
    };

    tail.finish();

    result
}

fn wait_for_child(config: &MeasureConfig, child: &mut Child) -> Result<ExitStatus, MeasureError> {
    let deadline = Duration::from_secs(config.timeout);
    let started_at = Instant::now();

    loop {
        if let Some(status) = child.try_wait().map_err(|source| MeasureError::Io {
            name: config.name.clone(),
            source,
        })? {
            return Ok(status);
        }

        if started_at.elapsed() >= deadline {
            terminate_child(child);
            let _ = child.wait();
            return Err(MeasureError::TimedOut {
                name: config.name.clone(),
                timeout: config.timeout,
            });
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn collect_output(
    config: &MeasureConfig,
    status: ExitStatus,
    stdout_handle: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr_handle: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<Output, MeasureError> {
    let stdout = join_reader(config, stdout_handle)?;
    let stderr = join_reader(config, stderr_handle)?;

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_line_reader<R, F>(
    reader: Option<R>,
    on_line: F,
) -> Option<JoinHandle<std::io::Result<Vec<u8>>>>
where
    R: Read + Send + 'static,
    F: Fn(&str) + Send + 'static,
{
    use std::io::BufRead;
    reader.map(|r| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let br = std::io::BufReader::new(r);
            for line in br.lines().map_while(Result::ok) {
                buf.extend_from_slice(line.as_bytes());
                buf.push(b'\n');
                on_line(&line);
            }
            Ok(buf)
        })
    })
}

fn join_reader(
    config: &MeasureConfig,
    handle: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<Vec<u8>, MeasureError> {
    let Some(handle) = handle else {
        return Ok(Vec::new());
    };

    handle
        .join()
        .map_err(|_| MeasureError::Io {
            name: config.name.clone(),
            source: std::io::Error::other("measure output reader thread panicked"),
        })?
        .map_err(|source| MeasureError::Io {
            name: config.name.clone(),
            source,
        })
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    let pgid = Pid::from_raw(child.id() as i32);
    // Measures run in their own process group so timeout cleanup can reach descendants.
    if signal::killpg(pgid, Signal::SIGKILL).is_err() {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) {
    let _ = child.kill();
}

struct ScriptAdaptorWithWorkingDir {
    command: Vec<String>,
    working_dir: PathBuf,
}

impl ScriptAdaptorWithWorkingDir {
    fn new(command: Vec<String>, working_dir: PathBuf) -> Self {
        Self {
            command,
            working_dir,
        }
    }
}

impl MetricAdaptor for ScriptAdaptorWithWorkingDir {
    fn extract(&self, output: &MeasureOutput) -> Result<Metrics, autotune_adaptor::AdaptorError> {
        let Some((program, args)) = self.command.split_first() else {
            return Err(autotune_adaptor::AdaptorError::ScriptEmptyCommand);
        };

        let mut child = Command::new(program)
            .args(args)
            .current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| autotune_adaptor::AdaptorError::Io { source })?;

        if let Some(mut stdin) = child.stdin.take() {
            let combined = format!("{}\n{}", output.stdout, output.stderr);
            stdin
                .write_all(combined.as_bytes())
                .map_err(|source| autotune_adaptor::AdaptorError::Io { source })?;
        }

        let result = child
            .wait_with_output()
            .map_err(|source| autotune_adaptor::AdaptorError::Io { source })?;

        if !result.status.success() {
            return Err(autotune_adaptor::AdaptorError::ScriptFailed {
                code: result.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&result.stdout);
        let metrics: Metrics = serde_json::from_str(&stdout)
            .map_err(|source| autotune_adaptor::AdaptorError::ScriptOutputParse { source })?;

        Ok(metrics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autotune_adaptor::AdaptorError;
    use autotune_config::{AdaptorConfig, MeasureConfig, RegexPattern};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn script_adaptor(command: Vec<String>, working_dir: &Path) -> ScriptAdaptorWithWorkingDir {
        ScriptAdaptorWithWorkingDir::new(command, working_dir.to_path_buf())
    }

    #[test]
    fn build_adaptor_regex_produces_regex_adaptor() {
        let config = AdaptorConfig::Regex {
            patterns: vec![RegexPattern {
                name: "m".to_string(),
                pattern: "([0-9]+)".to_string(),
                sources: vec![],
            }],
        };
        let adaptor = build_adaptor(&config, Path::new("."));
        let output = MeasureOutput {
            stdout: "value: 42\n".to_string(),
            stderr: String::new(),
        };
        let metrics = adaptor.extract(&output).unwrap();
        assert_eq!(*metrics.get("m").unwrap(), 42.0);
    }

    #[test]
    fn build_adaptor_script_returns_adaptor() {
        let config = AdaptorConfig::Script {
            command: vec!["echo".to_string()],
        };
        let _adaptor = build_adaptor(&config, Path::new("."));
        // Just verify no panic on construction.
    }

    #[test]
    fn script_adaptor_extract_rejects_empty_command() {
        let tmp = tempfile::tempdir().unwrap();
        let adaptor = script_adaptor(Vec::new(), tmp.path());
        let output = MeasureOutput {
            stdout: "ignored".to_string(),
            stderr: "ignored".to_string(),
        };

        let err = adaptor.extract(&output).unwrap_err();

        assert!(matches!(err, AdaptorError::ScriptEmptyCommand));
    }

    #[test]
    fn script_adaptor_extract_passes_combined_output_and_working_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("marker.txt"), "present").unwrap();
        let script = tmp.path().join("extract.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
test -f marker.txt || exit 1
bytes=$(cat | wc -c | tr -d ' ')
echo "{\"stdin_bytes\": $bytes, \"pwd_ok\": 1}"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
        let adaptor = script_adaptor(vec![script.display().to_string()], tmp.path());
        let output = MeasureOutput {
            stdout: "alpha".to_string(),
            stderr: "beta".to_string(),
        };

        let metrics = adaptor.extract(&output).unwrap();

        assert_eq!(metrics.get("stdin_bytes"), Some(&10.0));
        assert_eq!(metrics.get("pwd_ok"), Some(&1.0));
    }

    #[test]
    fn script_adaptor_extract_surfaces_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let adaptor = script_adaptor(
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'script blew up' >&2\nexit 7".to_string(),
            ],
            tmp.path(),
        );
        let output = MeasureOutput {
            stdout: String::new(),
            stderr: String::new(),
        };

        let err = adaptor.extract(&output).unwrap_err();

        assert!(
            matches!(err, AdaptorError::ScriptFailed { code, ref stderr } if code == 7 && stderr.contains("script blew up"))
        );
    }

    #[test]
    fn run_measure_with_output_env_injects_env_var() {
        // The measure emits a marker only when CARGO_TARGET_DIR matches the
        // injected path, proving the env reached the child process.
        let config = MeasureConfig {
            name: "env-check".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "if [ \"$CARGO_TARGET_DIR\" = /shared/target ]; then echo 'ok: 1'; else echo 'ok: 0'; fi".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "ok".to_string(),
                    pattern: r"ok: ([0-9]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();
        let env = vec![("CARGO_TARGET_DIR".to_string(), "/shared/target".to_string())];
        let report = run_measure_with_output_env(&config, tmp.path(), &env).unwrap();
        assert_eq!(
            *report.metrics.get("ok").unwrap(),
            1.0,
            "measure command should observe the injected CARGO_TARGET_DIR"
        );
    }

    /// Write a criterion `estimates.json` for `group` under
    /// `<target_dir>/criterion/<group>/new/estimates.json`, the way criterion
    /// lays it out below `CARGO_TARGET_DIR`.
    fn write_criterion_estimates(target_dir: &Path, group: &str, mean: f64) {
        let new_dir = target_dir.join("criterion").join(group).join("new");
        fs::create_dir_all(&new_dir).unwrap();
        let json = format!(
            r#"{{"mean":{{"confidence_interval":{{"confidence_level":0.95,"lower_bound":{lo},"upper_bound":{hi}}},"point_estimate":{mean}}},"median":{{"point_estimate":{mean}}},"std_dev":{{"point_estimate":3.0}}}}"#,
            lo = mean - 5.0,
            hi = mean + 5.0,
        );
        fs::write(new_dir.join("estimates.json"), json).unwrap();
    }

    fn criterion_measure(group: &str, metric: &str) -> MeasureConfig {
        use autotune_config::CriterionBenchmark;
        MeasureConfig {
            name: "bench".to_string(),
            // A no-op command stands in for `cargo bench`; the estimates.json is
            // pre-seeded by the test under the redirected target dir.
            command: Some(vec!["true".to_string()]),
            timeout: 30,
            adaptor: AdaptorConfig::Criterion {
                benchmarks: vec![CriterionBenchmark {
                    name: metric.to_string(),
                    group: group.to_string(),
                    stat: autotune_config::CriterionStat::Mean,
                    sources: vec![],
                }],
            },
            sources: vec![],
        }
    }

    /// Regression for the PR #22 shared-target interaction: when
    /// `CARGO_TARGET_DIR` is injected via `extra_env`, criterion writes its
    /// results under `<CARGO_TARGET_DIR>/criterion/...`, NOT
    /// `<working_dir>/target/criterion/...`. The adaptor must resolve the
    /// estimates.json under the effective (redirected) target dir.
    #[test]
    fn run_measure_resolves_criterion_under_redirected_cargo_target_dir() {
        let working = tempfile::tempdir().unwrap();
        let shared_target = tempfile::tempdir().unwrap();
        // Results live under the REDIRECTED target, not <working>/target.
        write_criterion_estimates(shared_target.path(), "stim-circuits/cultivation_d5", 100.0);

        let config = criterion_measure("stim-circuits/cultivation_d5", "cultivation_ns");
        let env = vec![(
            "CARGO_TARGET_DIR".to_string(),
            shared_target.path().to_string_lossy().into_owned(),
        )];

        let report = run_measure_with_output_env(&config, working.path(), &env).unwrap();
        assert_eq!(*report.metrics.get("cultivation_ns").unwrap(), 100.0);
        // Variance (#20 envelope path) must resolve under the same target dir.
        let v = report.variances.get("cultivation_ns").unwrap();
        assert_eq!(v.stddev, Some(3.0));
        assert_eq!(v.ci_lower, Some(95.0));
        assert_eq!(v.ci_upper, Some(105.0));
    }

    /// With no `CARGO_TARGET_DIR` injected, criterion results are resolved under
    /// `<working_dir>/target/criterion/...` exactly as before (#22 regression
    /// must not change the default-path behavior).
    #[test]
    fn run_measure_resolves_criterion_under_working_dir_target_by_default() {
        let working = tempfile::tempdir().unwrap();
        write_criterion_estimates(&working.path().join("target"), "grp/fn", 50.0);

        let config = criterion_measure("grp/fn", "grp_ns");
        let report = run_measure_with_output_env(&config, working.path(), &[]).unwrap();
        assert_eq!(*report.metrics.get("grp_ns").unwrap(), 50.0);
    }

    /// `criterion_estimates_files` (the post-hoc copy for the #20 envelope) must
    /// also resolve under the redirected target dir.
    #[test]
    fn criterion_estimates_files_resolves_under_redirected_target() {
        let working = tempfile::tempdir().unwrap();
        let shared_target = tempfile::tempdir().unwrap();
        write_criterion_estimates(shared_target.path(), "grp/fn", 7.0);

        let config = criterion_measure("grp/fn", "grp_ns");
        let env = vec![(
            "CARGO_TARGET_DIR".to_string(),
            shared_target.path().to_string_lossy().into_owned(),
        )];
        let files = criterion_estimates_files_with_env(&config, working.path(), &env);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "grp_ns");
        assert!(
            files[0].1.is_file(),
            "resolved path: {}",
            files[0].1.display()
        );
        assert!(files[0].1.starts_with(shared_target.path()));
    }

    /// `effective_target_dir` honors an injected `CARGO_TARGET_DIR` and falls
    /// back to `<working_dir>/target` otherwise.
    #[test]
    fn effective_target_dir_prefers_injected_cargo_target_dir() {
        let working = Path::new("/wd");
        assert_eq!(effective_target_dir(working, &[]), Path::new("/wd/target"));
        let env = vec![("CARGO_TARGET_DIR".to_string(), "/shared/t".to_string())];
        assert_eq!(effective_target_dir(working, &env), Path::new("/shared/t"));
    }

    #[test]
    fn run_measure_returns_error_on_command_failure() {
        let config = MeasureConfig {
            name: "fail-test".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex { patterns: vec![] },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();
        let result = run_measure(&config, tmp.path());
        assert!(
            matches!(result, Err(MeasureError::CommandFailed { ref name, .. }) if name == "fail-test"),
            "expected CommandFailed, got: {result:?}"
        );
    }

    #[test]
    fn run_measure_extracts_metrics_on_success() {
        let config = MeasureConfig {
            name: "score-test".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'score: 99.5'".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "metric-name".to_string(),
                    pattern: r"score: ([0-9.]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();
        let metrics = run_measure(&config, tmp.path()).unwrap();
        assert_eq!(*metrics.get("metric-name").unwrap(), 99.5);
    }

    #[test]
    fn run_measure_with_output_returns_timeout_error() {
        let config = MeasureConfig {
            name: "timeout-test".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "sleep 1".to_string(),
            ]),
            timeout: 0,
            adaptor: AdaptorConfig::Regex { patterns: vec![] },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();

        let result = run_measure_with_output(&config, tmp.path());

        assert!(
            matches!(result, Err(MeasureError::TimedOut { ref name, timeout }) if name == "timeout-test" && timeout == 0),
            "expected TimedOut, got: {result:?}"
        );
    }

    #[test]
    fn run_measure_with_output_maps_extraction_failures() {
        let config = MeasureConfig {
            name: "extract-fail-test".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'not a matching metric'".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "score".to_string(),
                    pattern: r"score: ([0-9.]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();

        let result = run_measure_with_output(&config, tmp.path());

        assert!(
            matches!(result, Err(MeasureError::Extraction { ref name, .. }) if name == "extract-fail-test"),
            "expected Extraction, got: {result:?}"
        );
    }

    #[test]
    fn run_measure_with_output_returns_report_with_stdout() {
        let config = MeasureConfig {
            name: "output-test".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'val: 7'".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "val".to_string(),
                    pattern: r"val: ([0-9]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();
        let report = run_measure_with_output(&config, tmp.path()).unwrap();
        assert_eq!(report.name, "output-test");
        assert!(
            report.stdout.contains("val: 7"),
            "stdout: {:?}",
            report.stdout
        );
        assert_eq!(*report.metrics.get("val").unwrap(), 7.0);
    }

    #[test]
    fn run_all_measures_with_output_returns_per_measure_reports() {
        let m1 = MeasureConfig {
            name: "alpha".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'x: 10'".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "x".to_string(),
                    pattern: r"x: ([0-9]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let m2 = MeasureConfig {
            name: "beta".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'y: 20'".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "y".to_string(),
                    pattern: r"y: ([0-9]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_metrics, reports) =
            run_all_measures_with_output(&[m1, m2], tmp.path(), "test", 1, None).unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].name, "alpha");
        assert!(reports[0].stdout.contains("x: 10"));
        assert_eq!(reports[1].name, "beta");
        assert!(reports[1].stdout.contains("y: 20"));
    }

    #[test]
    fn run_all_measures_merges_metrics() {
        let m1 = MeasureConfig {
            name: "first".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'a: 1'".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "metric-a".to_string(),
                    pattern: r"a: ([0-9]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let m2 = MeasureConfig {
            name: "second".to_string(),
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'b: 2'".to_string(),
            ]),
            timeout: 30,
            adaptor: AdaptorConfig::Regex {
                patterns: vec![RegexPattern {
                    name: "metric-b".to_string(),
                    pattern: r"b: ([0-9]+)".to_string(),
                    sources: vec![],
                }],
            },
            sources: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();
        let metrics = run_all_measures(&[m1, m2], tmp.path(), "test", 1, None).unwrap();
        assert_eq!(*metrics.get("metric-a").unwrap(), 1.0);
        assert_eq!(*metrics.get("metric-b").unwrap(), 2.0);
    }

    // ── judge tests ──────────────────────────────────────────────────────────

    struct FakeAgent {
        response: String,
    }

    impl autotune_agent::Agent for FakeAgent {
        fn backend_name(&self) -> &str {
            "fake"
        }

        fn spawn(
            &self,
            _config: &autotune_agent::AgentConfig,
        ) -> Result<autotune_agent::AgentResponse, autotune_agent::AgentError> {
            Ok(autotune_agent::AgentResponse {
                text: self.response.clone(),
                session_id: "fake-session".to_string(),
            })
        }

        fn send(
            &self,
            _session: &autotune_agent::AgentSession,
            _message: &str,
        ) -> Result<autotune_agent::AgentResponse, autotune_agent::AgentError> {
            unimplemented!()
        }

        fn handover_command(&self, _session: &autotune_agent::AgentSession) -> String {
            unimplemented!()
        }
    }

    fn judge_measure_config(name: &str, rubric_ids: &[&str]) -> MeasureConfig {
        use autotune_config::{RubricConfig, ScoreRangeConfig};
        MeasureConfig {
            name: name.to_string(),
            command: None,
            timeout: 30,
            adaptor: AdaptorConfig::Judge {
                persona: "A reviewer".to_string(),
                rubrics: rubric_ids
                    .iter()
                    .map(|id| RubricConfig {
                        id: id.to_string(),
                        title: id.to_string(),
                        instruction: "Score 1-5.".to_string(),
                        score_range: ScoreRangeConfig { min: 1, max: 5 },
                        guidance: None,
                        sources: vec![],
                    })
                    .collect(),
            },
            sources: vec![],
        }
    }

    fn fake_agent_config() -> autotune_agent::AgentConfig {
        autotune_agent::AgentConfig {
            prompt: String::new(),
            allowed_tools: vec![],
            working_directory: std::path::PathBuf::from("."),
            model: None,
            max_turns: Some(1),
            reasoning_effort: None,
        }
    }

    #[test]
    fn run_judge_measure_returns_metrics_per_rubric() {
        let tmp = tempfile::tempdir().unwrap();
        let config = judge_measure_config("critique", &["r1", "r2"]);
        let agent = FakeAgent {
            response: "r1\nscore: 4\nreason: Good.\n\nr2\nscore: 3\nreason: Acceptable."
                .to_string(),
        };
        let ctx = JudgeContext {
            agent: &agent,
            agent_config: fake_agent_config(),
            make_stream: None,
        };
        let report = run_judge_measure(&config, tmp.path(), "approach-a", 1, &ctx).unwrap();
        assert_eq!(*report.metrics.get("r1").unwrap(), 4.0);
        assert_eq!(*report.metrics.get("r2").unwrap(), 3.0);
        assert_eq!(report.name, "critique");
    }

    #[test]
    fn run_judge_measure_with_command_captures_output() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = judge_measure_config("critique", &["r1"]);
        config.command = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo 'source code here'".to_string(),
        ]);
        let agent = FakeAgent {
            response: "r1\nscore: 5\nreason: Excellent.".to_string(),
        };
        let ctx = JudgeContext {
            agent: &agent,
            agent_config: fake_agent_config(),
            make_stream: None,
        };
        let report = run_judge_measure(&config, tmp.path(), "my-approach", 2, &ctx).unwrap();
        assert_eq!(*report.metrics.get("r1").unwrap(), 5.0);
        assert!(report.stdout.contains("source code here"));
    }

    #[test]
    fn run_all_measures_with_judge_ctx_dispatches_judge_measure() {
        let tmp = tempfile::tempdir().unwrap();
        let configs = vec![judge_measure_config("j", &["score"])];
        let agent = FakeAgent {
            response: "score\nscore: 5\nreason: Perfect.".to_string(),
        };
        let ctx = JudgeContext {
            agent: &agent,
            agent_config: fake_agent_config(),
            make_stream: None,
        };
        let (metrics, reports) =
            run_all_measures_with_output(&configs, tmp.path(), "approach", 1, Some(&ctx)).unwrap();
        assert_eq!(*metrics.get("score").unwrap(), 5.0);
        assert_eq!(reports.len(), 1);
    }
}
