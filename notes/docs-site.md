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

## Deployment (GitHub Pages via the `gh-pages` branch)

`.github/workflows/docs.yml` builds the site and publishes it to the
`gh-pages` branch. The owner's user site has a custom apex domain
(`rogerluo.dev`), so this project repo's Pages serve at
`https://rogerluo.dev/autotune/` — note the `/autotune` base path (a *project*
page lives under `/<repo>`; the `roger-luo.github.io/autotune/` URL just
redirects to the custom domain).

- **`site` + `base` are env-driven** (`astro.config.mjs` reads `AUTOTUNE_SITE`
  / `AUTOTUNE_BASE`) so one config powers local dev (defaults: site =
  `rogerluo.dev`, base `/`), the main deploy (base `/autotune`), and PR
  previews (base `/autotune/pr-preview/pr-<N>`). A plain local `pnpm build`
  uses base `/`; CI always passes the real base, so production is correct
  regardless of the default. **If you ever build for production by hand, set
  `AUTOTUNE_BASE=/autotune`** or every asset URL 404s under the project path.
- **No Rust toolchain in CI.** The API JSON under `docs/.rustdoc/*.json` is
  committed, so the workflow only needs Node 22 + pnpm 11 (pinned to match the
  `allowBuilds` workspace key). Regenerate the JSON locally with
  `pnpm api:rustdoc` when crate APIs change — CI will not do it for you.
- **Push to `main`** → build + publish to the gh-pages root via
  `peaceiris/actions-gh-pages` with `keep_files: true` (so live PR previews
  under `/pr-preview/` survive a main deploy).
- **PR previews** → `rossjrw/pr-preview-action` publishes each PR build to
  `gh-pages/pr-preview/pr-<N>/` and comments the URL; a `pull_request_target:
  closed` job removes it. Previews only deploy for PRs from branches **in this
  repo** — fork PRs get a read-only token and the deploy job is skipped (the
  build job still runs). The build job is deliberately read-only; only the
  deploy jobs can push to gh-pages.
- **Pages is already enabled** on the `gh-pages` branch (root). If it ever
  needs re-enabling after the branch is recreated:
  `gh api -X POST repos/Roger-luo/autotune/pages -f 'source[branch]=gh-pages' -f 'source[path]=/'`.
