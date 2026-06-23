# Metric-aware research/planning guidance

AutoTune is a **general** metric-driven loop: it tunes performance benchmarks,
code coverage, binary size, model accuracy — anything you can declare as a
metric. The core must NOT be biased toward performance.

## The principle: specialize by declaration, not by a mode flag

Earlier work (PR #9) hardcoded performance-specific advice into the *general*
research and planning prompts: "profile first", "you MAY run a profiler",
"target the hot path with the smallest change". That fires even for a coverage
or binary-size task, where "profile the hot path" is meaningless and actively
misleads the agent.

The fix follows the same model the noise gate already uses (PR #15): **behavior
specializes by the data/declarations present, not by a mode switch.** The noise
math self-gates — no variance ⇒ envelope is 0.0 ⇒ no-op. Likewise, perf guidance
gates on whether the task's *declared measures* describe a runtime benchmark.

There is **no** task-kind / profile config flag, and adding one would violate the
maintainer's hard design constraint. If you find yourself reaching for "is this a
perf task?" state, derive it from the config instead.

## The classifier

`autotune_config::AutotuneConfig::optimizes_runtime_perf()` returns `true` when
**any measure uses the `criterion` adaptor**. Rationale:

- Criterion is a wall-clock micro-benchmark harness whose entire purpose is
  timing. Its presence is an unambiguous declaration that the task optimizes
  runtime performance.
- Criterion is the one built-in adaptor that emits per-metric variance (CI /
  stddev), which is exactly the signal the noise gate consumes — so "produces
  timing/variance" and "is a criterion measure" coincide today.
- Regex / script / judge measures could extract *anything* (a coverage %, a byte
  count, an accuracy, a rubric score), so they do **not** imply a perf task on
  their own.

If a future adaptor also produces genuine timing/variance, extend the classifier
there (one method, one place) rather than threading a flag through.

## What it gates

When `optimizes_runtime_perf()` is true:

- The research spawn prompt keeps the "Forming a high-value hypothesis" block
  with hot-path / profiling / smallest-surgical-change advice, and the "You MAY
  run a profiler … under Bash" offer.
- The planning prompt keeps the hot-path framing and suggests `Bash` to profile.

Otherwise (coverage / size / accuracy / anything non-criterion):

- The research spawn prompt emits a GENERIC, metric-agnostic block: study what
  *moves the declared metric*, form the smallest change that improves the
  objective, respect the configured weights/directions, learn from the ledger.
  No profiler offer, no "hot path".
- The planning prompt stays metric-agnostic and does not suggest a profiler.

The two shared bullets (respect scoring weights; learn from the ledger) appear in
both branches — they are objective-agnostic.

## Where the wiring lives

- Classifier: `crates/autotune-config/src/lib.rs` —
  `AutotuneConfig::optimizes_runtime_perf()`.
- Research spawn prompt: `crates/autotune/src/main.rs` —
  `hypothesis_guidance_block(is_perf)` (the block selector) called from
  `build_research_agent_prompt`, which also gates the "You MAY run a profiler"
  line on the same `is_perf`.
- Planning prompt: `crates/autotune-plan/src/lib.rs` —
  `build_planning_prompt(..., is_perf)` and `plan_next(..., is_perf, ...)`.
  `autotune-plan` does not depend on `autotune-config` (it is a leaf), so the
  CLI passes the boolean in; `machine.rs::run_planning` computes it via
  `config.optimizes_runtime_perf()`.

## Profiler tool access

Profiler tooling reaches the agent only through the runtime `<request-tool>`
flow (`Bash`), which the user approves per request — there is no eager grant.
The perf prompt is what *invites* the agent to request `Bash` for profiling; the
generic prompt simply doesn't, so a non-perf run never nudges the agent toward a
profiler it has no use for.
