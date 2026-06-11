---
title: API Reference
description: Type and function reference for every crate, generated from rustdoc JSON.
section: API Reference
sidebarLabel: Overview
order: 0
---

The pages in this section are **generated from rustdoc** — the same doc
comments that `cargo doc` renders, extracted via rustdoc's experimental JSON
output and consumed directly by Astro so they share this site's theme, search,
and navigation.

## How it works

1. `pnpm api:rustdoc` runs `cargo +nightly rustdoc -p <crate> --lib -- -Z unstable-options --output-format json`
   for every workspace crate, writing JSON into `docs/.rustdoc/`.
2. A custom Astro content loader (`src/content.config.ts`) reads that JSON at
   build/dev time, converts each crate's public API to Markdown
   (`scripts/rustdoc-parse.mjs`), and renders it through Astro's own Markdown
   pipeline — no intermediate files.

Regenerate the JSON after changing a crate's public API or doc comments;
generating it requires a Rust **nightly** toolchain. The JSON is committed, so
the site builds without a toolchain.

## Pages

One page per crate, listing its public structs, enums, traits, functions,
macros, and type aliases with signatures and doc comments. Browse them from the
sidebar, or start with the prose [crate guides](/crates/) for usage-oriented
documentation.
