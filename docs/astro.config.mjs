// @ts-check
import { defineConfig } from 'astro/config';

// Minimal, framework-free Astro docs site.
// Dual Shiki themes are emitted as CSS variables; src/styles/global.css
// switches between them based on the `data-theme` attribute set by the
// theme toggle (see src/components/ThemeToggle.astro).
export default defineConfig({
  // Update `site` to the deployed URL when publishing.
  site: 'https://autotune.example.com',
  markdown: {
    shikiConfig: {
      themes: {
        light: 'github-light',
        dark: 'github-dark',
      },
    },
  },
});
