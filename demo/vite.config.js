import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

const here = (path) => fileURLToPath(new URL(path, import.meta.url));

/* The download page's live demo: the app's own frontend, built with the
   demo's shell in place of Tauri, into docs/demo/ for GitHub Pages. Relative
   paths throughout, so it runs from any subpath. `bun run build:demo`;
   `bun run dev:demo` to work on it. The app's own build is untouched. */
export default defineConfig({
  root: here("."),
  base: "./",
  publicDir: here("public"),
  plugins: [svelte({ configFile: here("../svelte.config.js") })],
  resolve: {
    alias: [
      // Every Tauri API the app imports is answered in the page.
      { find: /^@tauri-apps\/(api\/(core|event|app)|plugin-(opener|updater|process))$/, replacement: here("tauri.js") },
      // The demo has no native window; its frame is the page's.
      { find: /^(\.\.?\/)+(lib\/)?window-frame\.svelte\.js$/, replacement: here("window-frame.svelte.js") },
    ],
  },
  build: {
    outDir: here("../docs/demo"),
    emptyOutDir: true,
  },
});
