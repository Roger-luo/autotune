import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CRATE_ORDER, crateToMarkdown } from '../scripts/rustdoc-parse.mjs';

// Hand-written docs: guides + the section overview pages.
// Each entry's `id` is its path under src/content/docs (e.g. "crates/autotune").
// `order` controls sidebar position; `section` groups it; `sidebarLabel`
// optionally overrides the displayed nav label.
const docs = defineCollection({
  loader: glob({ pattern: '**/*.md', base: './src/content/docs' }),
  schema: z.object({
    title: z.string(),
    description: z.string().optional(),
    order: z.number().default(100),
    section: z.string().default('Guides'),
    sidebarLabel: z.string().optional(),
  }),
});

// Auto-generated API reference. A custom loader reads the rustdoc JSON
// (docs/.rustdoc/*.json) directly and renders each crate's public API to HTML
// at build/dev time via Astro's own Markdown pipeline — no intermediate files.
// Regenerate the JSON with `pnpm api:rustdoc`.
const RUSTDOC_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', '.rustdoc');

const api = defineCollection({
  loader: {
    name: 'rustdoc-json',
    async load({ store, logger, parseData, renderMarkdown, watcher }) {
      store.clear();
      if (!existsSync(RUSTDOC_DIR)) {
        logger.warn(
          `No rustdoc JSON at ${RUSTDOC_DIR}. Run \`pnpm api:rustdoc\`; the API reference will be empty.`,
        );
        return;
      }
      if (watcher) watcher.add(RUSTDOC_DIR);

      const files = readdirSync(RUSTDOC_DIR).filter((f: string) => f.endsWith('.json')).sort();
      for (const file of files) {
        const crate = file.replace(/\.json$/, '');
        const json = JSON.parse(readFileSync(join(RUSTDOC_DIR, file), 'utf8'));
        if (json.format_version !== 57) {
          logger.warn(`${crate}: rustdoc format_version ${json.format_version} (expected 57); output may be off`);
        }

        const { markdown } = crateToMarkdown(json, crate);
        const data = await parseData({
          id: crate,
          data: {
            title: crate,
            description: `API reference for the ${crate} crate, generated from rustdoc.`,
            section: 'API Reference',
            sidebarLabel: crate,
            order: (CRATE_ORDER as Record<string, number>)[crate] ?? 100,
          },
        });
        const rendered = await renderMarkdown(markdown);
        store.set({ id: crate, data, rendered });
      }
      logger.info(`Loaded ${files.length} crate API pages from rustdoc JSON`);
    },
  },
  schema: z.object({
    title: z.string(),
    description: z.string().optional(),
    order: z.number().default(100),
    section: z.string().default('API Reference'),
    sidebarLabel: z.string().optional(),
  }),
});

export const collections = { docs, api };
