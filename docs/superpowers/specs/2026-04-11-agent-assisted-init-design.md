# Agent-Assisted Init Design Spec

## Overview

When `autotune init` is run without an existing `.autotune.toml`, the CLI spawns an init agent that explores the codebase, converses with the user, and incrementally proposes config sections. The CLI validates each section as it arrives, assembles the final config, then continues seamlessly into sanity tests and baseline benchmarking.

This fills the gap in `cmd_init` which currently prints "agent-assisted init not yet supported" when no config exists.

## Architecture

### New Crate: `autotune-init`

```
crates/autotune-init/
├── Cargo.toml
└── src/
    └── lib.rs    # run_init, prompt building, conversation loop
```

**Dependencies:** `autotune-agent`, `autotune-config`

**Entry point:**
```rust
pub fn run_init(
    agent: &dyn Agent,
    global_config: &GlobalConfig,
    repo_root: &Path,
) -> Result<AutotuneConfig>
```

### Protocol Types (in `autotune-agent`)

The agent communicates via structured JSON responses. Each response contains exactly one request.

```rust
enum AgentRequest {
    /// Free-form text to the user. User responds naturally.
    Message { text: String },

    /// Structured question with specific options.
    Question {
        text: String,
        options: Vec<QuestionOption>,
        allow_free_response: bool,
    },

    /// Propose a validated config section.
    Config { section: ConfigSection },

    // TODO: future iterations
    // Profile { command: Vec<String> },
    // Bench { command: Vec<String> },
    // RunTests { command: Vec<String> },
}

struct QuestionOption {
    key: String,         // "a", "b", "c" or short label
    description: String,
}

enum ConfigSection {
    Experiment(ExperimentConfig),
    Paths(PathsConfig),
    Test(TestConfig),           // one test entry at a time
    Benchmark(BenchmarkConfig), // one benchmark entry at a time
    Score(ScoreConfig),
    Agent(AgentConfig),
}
```

The agent's text response may contain prose around the JSON (same pattern as `parse_hypothesis` in `autotune-plan`). The parser extracts the JSON object and deserializes it.

**Error handling:** If the agent returns unparseable output, retry once with a corrective prompt ("respond with valid JSON matching the schema"). If it fails again, abort with a clear error.

### CLI → Agent Responses

After handling each `AgentRequest`, the CLI sends the result back via `agent.send()`:

- **`Message`**: sends the user's typed text
- **`Question`**: sends the user's selected option (or free-form text if allowed)
- **`Config`**: sends validation result — either confirmation of acceptance or the specific validation error for the agent to correct

## Global Config

Solves the chicken-and-egg problem: when `.autotune.toml` doesn't exist, the init agent needs backend/model settings from somewhere.

### File Locations

- **System:** `/etc/autotune/config.toml`
- **User:** `~/.config/autotune/config.toml`

Resolution order: system → user → project (each layer overrides the previous). For init, only system and user apply.

### Shape

```toml
[agent]
backend = "claude"

[agent.research]
model = "opus"

[agent.implementation]
model = "sonnet"

[agent.init]
model = "opus"
```

Only agent defaults. Project-specific settings (experiment, paths, tests, benchmarks, score) do not belong here.

### Implementation

A `GlobalConfig` struct in `autotune-config`:

```rust
pub struct GlobalConfig {
    pub agent: Option<AgentConfig>,
}

impl GlobalConfig {
    /// Load from system → user config, merging layers.
    pub fn load() -> Result<GlobalConfig>;
}
```

Reuses the existing `AgentConfig` / `AgentRoleConfig` types.

## Init Conversation Loop

### Flow

1. Load global config (system → user) for agent settings
2. Build agent from global config (or hardcoded defaults if no global config exists)
3. Build the init system prompt (schema, repo root, instructions)
4. `agent.spawn(config)` — first turn
5. Parse response as `AgentRequest`
6. Handle based on type:
   - `Message` → print text, read user input, send to agent
   - `Question` → render options, read user selection, send to agent
   - `Config { section }` → validate section immediately:
     - Valid: accumulate, confirm to agent
     - Invalid: send validation error back to agent for correction
7. Repeat until all required sections are accumulated
8. Assemble full `AutotuneConfig`, display to user for final approval
9. Return the config

### Section Validation

Each `ConfigSection` is validated against the same rules as `AutotuneConfig::load()`:

| Section | Validation |
|---|---|
| `Experiment` | Name non-empty, at least one stop condition |
| `Paths` | Tunable globs are syntactically valid |
| `Test` | Command non-empty |
| `Benchmark` | Command non-empty, adaptor valid, metric names unique across all accumulated benchmarks |
| `Score` | Referenced metric names exist in accumulated benchmarks |

Order matters: `Score` can only be fully validated after benchmarks are defined. The agent's prompt guides it to propose sections in natural order (experiment → paths → tests → benchmarks → score).

### Completion Detection

The CLI tracks which required sections have been provided:

- **Required:** `Experiment`, `Paths`, at least one `Benchmark`, `Score`
- **Optional:** `Test`, `Agent`

Once all required sections are present, the CLI signals the agent to wrap up.

### Final Approval

After all required sections are accumulated, the CLI assembles and displays the full config. The user can:
- **Approve** → CLI writes `.autotune.toml` and continues to baseline
- **Reject** → CLI sends the user's feedback back to the agent, which can propose revised sections. The loop continues.

### User Abort

Ctrl+C at any prompt exits gracefully, no config written.

## Init Agent Configuration

### Permissions

- `Read`, `Glob`, `Grep` — full codebase read access
- No `Edit`, `Write`, `Bash`, `Agent`, `WebFetch`

The agent is read-only. All side effects go through the CLI via the action protocol (when action types are implemented in future iterations).

### System Prompt

The prompt is built dynamically in `autotune-init` (not a static file). It includes:

1. **Role:** help the user configure autotune for their project
2. **Protocol:** respond with JSON matching the `AgentRequest` schema, one request per response
3. **Config schema:** what each `ConfigSection` contains — required fields, valid values, with examples
4. **Order guidance:** explore codebase first, then propose sections in order (experiment → paths → tests → benchmarks → score)
5. **Repo root path** so the agent knows where to look

### First Turn Behavior

The agent should use its read tools to explore the project (look for existing benchmarks, test commands, `Cargo.toml`/`package.json`, CI config, etc.) and then start the conversation — either a `Message` summarizing what it found or a `Question` to disambiguate.

### Agent Settings

Read from global config `[agent.init]` section. Falls back to:
- backend: `"claude"`
- model: none (use backend default)
- max_turns: none (unlimited)

## Integration in Binary Crate

### `cmd_init` Updated Flow

**When `.autotune.toml` is missing:**

1. Load global config (system → user)
2. Build agent from global config defaults
3. Call `autotune_init::run_init(agent, global_config, repo_root)` → `AutotuneConfig`
4. Serialize to TOML, write `.autotune.toml`
5. Continue into existing init flow: create experiment dir, snapshot config, sanity tests, baseline benchmarks, record to ledger

**When `.autotune.toml` exists:** unchanged.

### Dependency Additions

```
autotune (binary)
├── autotune-init → autotune-agent, autotune-config
└── ... (existing deps)
```

### Updated Dependency Graph

```
autotune (binary+lib)
├── autotune-init       → autotune-agent, autotune-config  (NEW)
├── autotune-plan       → autotune-agent, autotune-state
├── autotune-implement  → autotune-agent, autotune-git
├── autotune-test       → autotune-config
├── autotune-benchmark  → autotune-config, autotune-adaptor
├── autotune-config     (leaf)
├── autotune-state      (leaf)
├── autotune-agent      (leaf)
├── autotune-adaptor    (leaf)
├── autotune-score      (leaf)
├── autotune-git        (leaf)
└── autotune-mock       (dev-only)
```

## Future Work (TODO)

The following action types are deferred to a future iteration. They are not needed for the initial implementation — the agent can be effective by reading the codebase and conversing with the user.

- `Profile { command: Vec<String> }` — run a profiler, return output to agent
- `Bench { command: Vec<String> }` — run a benchmark, return output to agent
- `RunTests { command: Vec<String> }` — run tests, return pass/fail + output to agent

These will be designed alongside the research agent's adoption of the same protocol.
