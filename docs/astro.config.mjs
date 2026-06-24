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

export default defineConfig({
  site,
  base,
  trailingSlash: 'ignore',
  markdown: {
    shikiConfig: {
      themes: {
        light: 'github-light',
        dark: 'github-dark',
      },
    },
  },
});
