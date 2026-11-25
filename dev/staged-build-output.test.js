import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readdir, readFile, rm, stat, utimes, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build } from "vite";
import { stagedBuildOutput } from "./staged-build-output.js";

async function snapshot(directory, prefix = "") {
  const files = {};
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    const name = `${prefix}${entry.name}`;
    if (entry.isDirectory()) {
      Object.assign(files, await snapshot(path, `${name}/`));
    } else {
      files[name] = (await readFile(path)).toString("base64");
    }
  }
  return files;
}

test("desktop output matches Vite, preserves unchanged files, and survives build failure", async () => {
  const root = await mkdtemp(join(tmpdir(), "spotifine-build-test-"));
  const output = join(root, "build");
  const publicDir = join(root, "public");
  const script = join(root, "main.js");
  const viteBuild = (outDir = "build", extraPlugins = []) => build({
    root,
    base: "./",
    configFile: false,
    logLevel: "silent",
    plugins: [stagedBuildOutput("build"), ...extraPlugins],
    build: { outDir },
  });

  try {
    await mkdir(join(publicDir, "nested"), { recursive: true });
    await writeFile(join(root, "index.html"), '<link rel="icon" href="/nested/keep.bin"><script type="module" src="/main.js"></script>');
    await writeFile(script, 'console.log("first build");');
    await writeFile(join(publicDir, "nested", "keep.bin"), Buffer.from([0, 255]));
    await writeFile(join(publicDir, "rename.txt"), "old name");
    await writeFile(join(publicDir, "replace.txt"), "old contents");
    await writeFile(join(publicDir, "swap"), "will be a directory");

    await viteBuild("baseline");
    await viteBuild();
    const first = await snapshot(output);
    expect(first).toEqual(await snapshot(join(root, "baseline")));
    expect(first["nested/keep.bin"]).toBe("AP8=");
    const oldScript = Object.keys(first).find((name) => /^assets\/.+\.js$/.test(name));
    expect(oldScript).toBeDefined();

    const oldTime = new Date("2020-01-01T00:00:00Z");
    const tracked = ["", "assets", "nested", ...Object.keys(first)];
    for (const name of tracked) await utimes(join(output, name), oldTime, oldTime);
    const mtimes = () => Promise.all(tracked.map(async (name) => (await stat(join(output, name))).mtimeMs));
    const before = await mtimes();
    await viteBuild();
    expect(await mtimes()).toEqual(before);

    await writeFile(script, 'console.log("changed build");');
    await rm(join(publicDir, "rename.txt"));
    await writeFile(join(publicDir, "Rename.txt"), "new name");
    await writeFile(join(publicDir, "replace.txt"), "new contents");
    await rm(join(publicDir, "swap"));
    await mkdir(join(publicDir, "swap"));
    await writeFile(join(publicDir, "swap", "new.txt"), "now nested");
    await expect(viteBuild("build", [{
      name: "force-output-failure",
      generateBundle() { throw new Error("fixture output failure"); },
    }])).rejects.toThrow("fixture output failure");
    expect(await snapshot(output)).toEqual(first);
    expect(await mtimes()).toEqual(before);
    const stages = () => readdir(join(root, "node_modules"));
    expect((await stages()).some((name) => name.startsWith(".vite-spotifine-build-"))).toBe(false);

    await viteBuild();
    await viteBuild("baseline");
    const changed = await snapshot(output);
    expect(changed).toEqual(await snapshot(join(root, "baseline")));
    expect(changed[oldScript]).toBeUndefined();
    expect(changed["rename.txt"]).toBeUndefined();
    expect(changed["Rename.txt"]).toBeDefined();
    expect(changed["swap/new.txt"]).toBeDefined();
    expect(await readFile(join(output, "replace.txt"), "utf8")).toBe("new contents");
    expect((await stat(join(output, "replace.txt"))).mtimeMs).toBeGreaterThan(oldTime.getTime());
    expect((await stat(join(output, "nested/keep.bin"))).mtimeMs).toBe(oldTime.getTime());
    expect((await stages()).some((name) => name.startsWith(".vite-spotifine-build-"))).toBe(false);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
