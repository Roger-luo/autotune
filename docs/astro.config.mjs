// @ts-check
import { defineConfig } from 'astro/config';

// Minimal, framework-free Astro docs site.
// Dual Shiki themes are emitted as CSS variables; src/styles/global.css
// switches between them based on the `data-theme` attribute set by the
// theme toggle (see src/components/ThemeToggle.astro).
//
// `site` + `base` are read from env so one config powers every deploy target
// (see .github/workflows/docs.yml). Pages serves the repo under the owner's
// custom apex domain at /autotune (https://rogerluo.dev/autotune):
//   · local dev / plain build:  AUTOTUNE_SITE=https://rogerluo.dev  AUTOTUNE_BASE=/      (defaults below)
//   · main → gh-pages root:     AUTOTUNE_SITE=https://rogerluo.dev  AUTOTUNE_BASE=/autotune
//   · PR preview:               AUTOTUNE_SITE=https://rogerluo.dev  AUTOTUNE_BASE=/autotune/pr-preview/pr-<N>
const site = process.env.AUTOTUNE_SITE ?? 'https://rogerluo.dev';
const base = process.env.AUTOTUNE_BASE ?? '/';

// Astro prepends `base` to asset URLs it controls, but NOT to links we write by
// hand in Markdown content (`[x](/configuration/)`). Under a non-root base
// (project pages live at /autotune) those root-absolute links would 404. This
// rehype plugin rewrites internal, root-absolute href/src in rendered Markdown
// to include the base. Component links (.astro) use `withBase()` from
// src/lib/base.ts instead. Kept dependency-free (no unist-util-visit) by
// walking the hast tree directly.
function rehypeBasePaths() {
  const prefix = base.replace(/\/+$/, ''); // '' when base is '/', else e.g. '/autotune'
  if (!prefix) return () => {}; // root base: nothing to prepend
  /** @type {Record<string, string>} */
  const ATTRS = { a: 'href', img: 'src' };
  /** @param {any} node */
  const walk = (node) => {
    if (node.type === 'element') {
      const attr = ATTRS[node.tagName];
      const val = attr && node.properties?.[attr];
      // Only internal, root-absolute URLs: "/x" — never "//host", protocols, or hashes.
      if (typeof val === 'string' && val.startsWith('/') && !val.startsWith('//')) {
        node.properties[attr] = prefix + val;
      }
    }
    if (node.children) for (const child of node.children) walk(child);
  };
  return (/** @type {any} */ tree) => walk(tree);
}

export default defineConfig({
  site,
  base,
  trailingSlash: 'ignore',
  markdown: {
    rehypePlugins: [rehypeBasePaths],
    shikiConfig: {
      themes: {
        light: 'github-light',
        dark: 'github-dark',
      },
    },
  },
});
