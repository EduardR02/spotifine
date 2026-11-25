import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { realBridge } from "./dev/real-bridge.js";
import { stagedBuildOutput } from "./dev/staged-build-output.js";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// Matches "frontendDist": "../build" in src-tauri/tauri.conf.json
const outDir = "build";

// https://vite.dev/config/
export default defineConfig({
  base: './',
  // realBridge is dev-server only (apply: "serve"): the UI harness reads the
  // owner's running app through it. See dev/real-bridge.js.
  plugins: [svelte(), realBridge(), stagedBuildOutput(outDir)],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`, and the harness bridge's
      //    answer cache, which it rewrites on every live read
      ignored: ["**/src-tauri/**", "**/dev/.real-cache/**"],
    },
  },
  build: {
    outDir,
  },
});
