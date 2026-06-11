# Documentation site (`docs/`)

The public docs site is a framework-free **Astro 5** project under `docs/`
(no React/Vue; the only client JS is the theme toggle). It coexists with
`docs/superpowers/` (brainstorming specs/plans) — Astro only serves
`src/content/`, so those planning files are never published.

Run it: `cd docs && pnpm install && pnpm dev`.

## API reference is generated from rustdoc JSON — pinned to `format_version: 57`

The "API Reference" pages are **not** hand-written. They are produced from
rustdoc's experimental JSON output and consumed **directly by Astro** (no
intermediate Markdown files):

1. `pnpm api:rustdoc` (`scripts/rustdoc-to-json.mjs`) runs
   `cargo +nightly rustdoc -p <crate> --lib -- -Z unstable-options --output-format json`
   for every workspace crate and copies the result into `docs/.rustdoc/<crate>.json`.
   **That JSON is committed** (it is the loader's input), so the site builds
   without a Rust toolchain.
2. A custom Astro content loader — the `api` collection in
   `docs/src/content.config.ts` — reads each JSON at build/dev time, converts a
   crate to a Markdown string via the pure `docs/scripts/rustdoc-parse.mjs`, and
   renders it with Astro's own pipeline (`renderMarkdown`) so it gets Shiki
   highlighting + heading slugs + TOC for free.

**The footgun:** `rustdoc-parse.mjs` is written against rustdoc JSON
**`format_version: 57`** (nightly ~2026-04). This format is unstable and bumps
between nightly releases — field names and the `Type`/`Item` enum shapes change.
When the version moves:

- `content.config.ts` logs a warning (`format_version N (expected 57)`); the
  pages may render with `_` placeholders or miss items rather than crash.
- To fix: run `pnpm api:rustdoc`, inspect a sample JSON for the changed shape
  (e.g. `node -e 'const j=require("./docs/.rustdoc/autotune-score.json"); …'`),
  update the relevant `*ToString`/`render*` helpers and the `57` checks in
  both `rustdoc-parse.mjs`'s header comment and `content.config.ts`.

The loader only walks **public** module items (following `pub use`
re-exports), groups them by kind, and lists inherent methods + a flat
"Implements" line of trait names. It deliberately skips synthetic/blanket impls
and the `StructuralPartialEq`/`StructuralEq` marker traits.

## Other non-obvious bits

- **`--lib` is required.** `autotune` is a binary **and** library crate;
  `cargo rustdoc -p autotune` without `--lib` is ambiguous. The script passes
  `--lib` to every crate (harmless for pure libs).
- **pnpm build approval.** `astro build` runs a pnpm deps-status check that
  re-invokes `pnpm install`; an "ignored build scripts" warning there makes that
  nested install exit non-zero and fails the build. `docs/pnpm-workspace.yaml`
  pre-approves the offenders (`allowBuilds: { esbuild: true, sharp: true }`).
  pnpm 11 no longer reads the old `pnpm.onlyBuiltDependencies` key in
  `package.json`.
- **Theme modes.** auto/light/dark cycle stored in `localStorage`; a blocking
  inline script in `DocsLayout`'s `<head>` resolves it before first paint to
  avoid a flash, and a `matchMedia` listener keeps "auto" following the OS.
  Shiki dual themes (`github-light`/`github-dark`) switch via the
  `:root[data-theme='dark'] .astro-code` rule in `global.css`.
- **Sidebar sections.** Nav is grouped by a `section` content field
  (`Guides` / `Crates` / `API Reference`), ordered in `Sidebar.astro`, which
  merges the `docs` and `api` collections. `sidebarLabel` overrides the label.
