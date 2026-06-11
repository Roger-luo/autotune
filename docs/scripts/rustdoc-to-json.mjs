#!/usr/bin/env node
// Generate rustdoc JSON for every workspace crate into docs/.rustdoc/.
//
// Requires a Rust *nightly* toolchain — it uses the unstable
// `--output-format json` rustdoc flag. Run via `pnpm api:rustdoc`
// (or `pnpm api:gen` to also rebuild the Markdown pages).
import { execFileSync } from 'node:child_process';
import { mkdirSync, copyFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const DOCS = join(SCRIPT_DIR, '..'); // docs/
const ROOT = join(DOCS, '..'); // workspace root (docs/ lives at the repo root)
const OUT = join(DOCS, '.rustdoc');

const CRATES = [
  'autotune', 'autotune-adaptor', 'autotune-agent', 'autotune-benchmark',
  'autotune-config', 'autotune-git', 'autotune-implement', 'autotune-init',
  'autotune-judge', 'autotune-mock', 'autotune-plan', 'autotune-score',
  'autotune-state', 'autotune-test',
];

mkdirSync(OUT, { recursive: true });

let failed = 0;
for (const crate of CRATES) {
  process.stdout.write(`rustdoc ${crate} … `);
  try {
    execFileSync(
      'cargo',
      ['+nightly', 'rustdoc', '-p', crate, '--lib', '--',
        '-Z', 'unstable-options', '--output-format', 'json'],
      { cwd: ROOT, stdio: ['ignore', 'ignore', 'pipe'] },
    );
    const src = join(ROOT, 'target', 'doc', crate.replace(/-/g, '_') + '.json');
    copyFileSync(src, join(OUT, crate + '.json'));
    console.log('ok');
  } catch (err) {
    failed++;
    console.log('FAILED');
    console.error(err.stderr?.toString?.() ?? err.message);
  }
}

if (failed) {
  console.error(`\n${failed} crate(s) failed to document.`);
  process.exit(1);
}
console.log(`\nWrote JSON for ${CRATES.length} crates to ${OUT}`);
