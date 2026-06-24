// `import.meta.env.BASE_URL` is Astro's configured `base`, always with a
// trailing slash ('/' locally, '/autotune/' in production — see astro.config.mjs).
// Astro prepends the base to assets it controls (CSS/JS imports, `<img src>`
// from `astro:assets`), but NOT to links we hand-write in `.astro` components.
// Use this helper for those so they resolve under the project-page base path.
// (Hand-written links inside Markdown content are handled separately by the
// rehypeBasePaths plugin in astro.config.mjs.)
const BASE = import.meta.env.BASE_URL;

/** Prefix a root-absolute internal path (e.g. "/api/") with the site base. */
export function withBase(path: string): string {
  const prefix = BASE.replace(/\/$/, ''); // '' when base is '/', else e.g. '/autotune'
  return prefix + (path.startsWith('/') ? path : `/${path}`);
}
