# Agent protocol — XML fragments

All agent-to-CLI communication (research, implementation, init) uses an XML
fragment protocol. The parser is `autotune_agent::protocol::parse_agent_response`,
which scans the response text for top-level tags and returns a `Vec<AgentFragment>`.

## Fragment types

| Tag | Produced by | Purpose |
|---|---|---|
| `<message>` | any agent | Free-form text passed through to UI. |
| `<question>` | init agent | Prompts the user with options. |
| `<task>` | init agent | Proposes a `[task]` config section. |
| `<paths>` | init agent | Proposes `[paths]` (tunable, denied). |
| `<test>` | init agent | Proposes a `[[test]]` entry. |
| `<measure>` | init agent | Proposes a `[[measure]]` entry. |
| `<score>` | init agent | Proposes `[score]`. |
| `<agent>` | init agent | Proposes `[agent]` config. |
| `<plan>` | research agent | Proposes the next iteration (approach/hypothesis/files). |
| `<request-tool>` | research agent | Asks the CLI to grant a tool at runtime. |

Content rules:
- Use `<![CDATA[...]]>` for free-text fields that may contain `<` or `&`.
- Multiple fragments are allowed in one response; they're all collected.
- Free prose around fragments is ignored, not parsed.

See `crates/autotune-agent/tests/protocol_test.rs` for concrete XML examples
of each fragment.

## MockAgent responses must be XML

A recurring pitfall: the `MockAgent` in `autotune-mock` is used by scenario
tests and the `AUTOTUNE_MOCK=1` path. **Its responses (including init)
must be valid XML fragments.** Emitting JSON (the old protocol) causes
`parse_agent_response` to return zero fragments, and the init loop burns all
its turns waiting for a valid `<task>` / `<paths>` / etc.

When writing or updating mock responses:
- Use the XML format documented in `protocol.rs` and the tests.
- Wrap user-provided strings in `<![CDATA[...]]>` if they might contain `<`.
- For init, you can emit multiple config fragments in a single response —
  the accumulator collects them all in one turn.

The mock init agent (`mock_init_agent()` in `main.rs` behind `#[cfg(feature = "mock")]`)
is the reference example.

## Planning response format (research agent)

```xml
<plan>
  <approach>short-kebab-name-or-any-string</approach>
  <hypothesis><![CDATA[concrete instructions for the implementation agent]]></hypothesis>
  <files-to-modify>
    <file>path/one.rs</file>
    <file>path/two.rs</file>
  </files-to-modify>
</plan>
```

The `<approach>` string is slugified before use as a git branch component
(`crates/autotune-implement/src/lib.rs::slugify`), so it can contain spaces,
commas, and other special characters — the CLI will normalize.

## Tool requests (research agent)

```xml
<request-tool>
  <tool>Bash</tool>
  <scope>cargo tree:*</scope>
  <reason>need the dependency graph to identify heavy crates</reason>
</request-tool>
```

The CLI prompts the user for approval. `Edit`, `Write`, and `Agent` are
hard-denied for the research role regardless of user response.
