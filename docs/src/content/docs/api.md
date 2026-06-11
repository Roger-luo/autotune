---
title: API Reference
description: Type and function reference for every crate, generated from rustdoc JSON.
section: API Reference
sidebarLabel: Overview
order: 0
---

The pages in this section are **generated from rustdoc** — the same doc
comments that `cargo doc` renders, extracted via rustdoc's experimental JSON
output and converted to Markdown so they share this site's theme, search, and
navigation.

## How it works

1. `pnpm api:rustdoc` runs `cargo +nightly rustdoc -p <crate> --lib -- -Z unstable-options --output-format json`
   for every workspace crate, writing JSON into `docs/.rustdoc/`.
2. `pnpm api:build` parses that JSON (`scripts/gen-api.mjs`) and emits one
   Markdown page per crate under `src/content/docs/api/`.
3. Astro renders those pages like any other doc.

Run both steps at once with `pnpm api:gen`. Regenerate after changing a crate's
public API or doc comments. Generating the JSON requires a Rust **nightly**
toolchain.

## Pages

One page per crate, listing its public structs, enums, traits, functions,
macros, and type aliases with signatures and doc comments. Browse them from the
sidebar, or start with the prose [crate guides](/crates/) for usage-oriented
documentation.
