import { randomUUID } from "node:crypto";
import { copyFile, mkdir, readdir, readFile, rm } from "node:fs/promises";
import { join, resolve } from "node:path";

async function syncOutput(staging, output) {
  const entries = await readdir(staging, { withFileTypes: true });
  const next = new Map(entries.map((entry) => [entry.name, entry]));
  await mkdir(output, { recursive: true });

  const existingFiles = new Set();
  // Remove obsolete paths before writing, including case-only renames on Windows.
  for (const entry of await readdir(output, { withFileTypes: true })) {
    const replacement = next.get(entry.name);
    if (entry.isDirectory() && replacement?.isDirectory()) continue;
    if (entry.isFile() && replacement?.isFile()) {
      existingFiles.add(entry.name);
    } else {
      await rm(join(output, entry.name), { recursive: true });
    }
  }

  for (const entry of entries) {
    const source = join(staging, entry.name);
    const destination = join(output, entry.name);
    if (entry.isDirectory()) {
      await syncOutput(source, destination);
    } else {
      if (existingFiles.has(entry.name)) {
        const [before, after] = await Promise.all([readFile(destination), readFile(source)]);
        if (before.equals(after)) continue;
      }
      await copyFile(source, destination);
    }
  }
}

export function stagedBuildOutput(desktopOutDir) {
  let output;
  let staging;
  return {
    name: "spotifine:staged-build-output",
    apply(config, { command }) {
      const options = config.build;
      // Leave alternate destinations, watch mode and special output modes to Vite.
      return command === "build" && options?.outDir === desktopOutDir &&
        !options.watch && !options.ssr && !options.lib && !options.sourcemap &&
        options.write !== false && options.emptyOutDir !== false && !options.rollupOptions?.output;
    },
    config(config) {
      const root = resolve(config.root ?? ".");
      output = join(root, desktopOutDir);
      staging = join(root, "node_modules", `.vite-spotifine-build-${randomUUID()}`);
      return { build: { outDir: staging } };
    },
    writeBundle: {
      order: "post",
      sequential: true,
      async handler() {
        // Vite has already copied public assets and Rollup has written the bundle.
        // Tauri embeds these files; rewriting identical bytes invalidates Cargo.
        await syncOutput(staging, output);
      },
    },
    async closeBundle() {
      await rm(staging, { recursive: true, force: true });
    },
  };
}
