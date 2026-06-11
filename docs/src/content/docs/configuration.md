---
title: Configuration
description: The .autotune.toml reference.
order: 2
---

Autotune reads its configuration from `.autotune.toml` in your project root.
Global defaults can be set once and merged per project.

## Task commands

Define how to build, test, and measure the code under tuning. A test failure
discards a candidate before it is ever scored.

## Metric adaptors

An adaptor turns raw task output into a `name → number` map. Built-in adaptors:

- **Regex** — capture a number from stdout with a pattern.
- **Criterion** — read Rust Criterion benchmark results.
- **Script** — shell out to a custom extractor.

## Scoring

A score calculator compares baseline, candidate, and best metrics and returns a
rank plus a keep/discard decision. Built-in calculators include weighted-sum,
threshold, and script-based scorers.

> This page is a scaffold. Expand each section with the full option reference as
> the configuration surface stabilizes.
