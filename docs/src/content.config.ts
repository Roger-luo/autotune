import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';

// A single `docs` collection backed by Markdown files in src/content/docs.
// Each entry's `id` is its filename without extension (e.g. "getting-started").
// `order` controls sidebar position; lower numbers come first.
const docs = defineCollection({
  loader: glob({ pattern: '**/*.md', base: './src/content/docs' }),
  schema: z.object({
    title: z.string(),
    description: z.string().optional(),
    order: z.number().default(100),
    // Sidebar grouping; rendered in the order defined by Sidebar.astro.
    section: z.string().default('Guides'),
    // Optional shorter label for the sidebar (defaults to `title`).
    sidebarLabel: z.string().optional(),
  }),
});

export const collections = { docs };
