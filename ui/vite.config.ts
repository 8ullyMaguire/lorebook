import { sveltekit } from '@sveltejs/kit/vite';
import adapter from '@sveltejs/adapter-static';
import { defineConfig } from 'vite';
import type { KitConfig } from '@sveltejs/kit';

/**
 * The SvelteKit config lives here, inside vite.config.ts, and there is no
 * svelte.config.js. That works ONLY because `sveltekit()` is called WITH the
 * config object below.
 *
 * With a bare `plugins: [sveltekit()]`, SvelteKit calls `load_svelte_config()`,
 * finds no svelte.config.js, and silently uses its defaults — which means NO
 * ADAPTER, plus a build warning that scrolls past. The build still succeeds and
 * still produces output, so the failure mode is a missing static export rather
 * than a build error.
 *
 * The UI talks to Rust over Tauri's IPC (`invoke`), not over HTTP, so there is
 * no dev proxy and no backend URL to configure: the same `invoke` call works in
 * dev and in the packaged app.
 */
const kit: KitConfig = {
  prerender: { entries: ['*'] },
  adapter: adapter({
    pages: 'build',
    assets: 'build',
    // A SPA fallback. The library view is entirely client-side, so the deep-link
    // case is a selected book, and the shell's own URL handling must serve the
    // app rather than 404. `strict: true` makes a missing file an error instead
    // of a silent fallback, which is what makes the fallback trustworthy.
    fallback: 'index.html',
    precompress: false,
    strict: true
  }),
  alias: { $lib: 'src/lib' },
  typescript: {
    config(cfg: Record<string, unknown>) {
      cfg.checkJs = false;
      cfg.noUnusedLocals = true;
      return cfg;
    }
  }
};

export default defineConfig({
  plugins: [sveltekit(kit)],
  server: { port: 1420, strictPort: true }
});
