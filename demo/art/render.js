/* Renders the demo's covers (covers.html) to 600px WebP in
   demo/public/media/covers/, one headless Edge screenshot per cover.
   Needs the network (the photos and fonts load from their sources) and
   ImageMagick. `bun demo/art/render.js [key ...]`; no keys renders all. */
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const EDGE = process.env.EDGE ?? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
const page = fileURLToPath(new URL("covers.html", import.meta.url));
const out = fileURLToPath(new URL("../public/media/covers/", import.meta.url));
const keys = process.argv.slice(2).length
  ? process.argv.slice(2)
  : [...readFileSync(page, "utf8").matchAll(/class="cover" id="([a-z]+)"/g)].map((m) => m[1]);

const scratch = mkdtempSync(join(tmpdir(), "spotifine-covers-"));
try {
  for (const key of keys) {
    const png = join(scratch, `${key}.png`);
    spawnSync(EDGE, [
      "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
      "--window-size=600,600", "--virtual-time-budget=8000", `--user-data-dir=${join(scratch, "profile")}`,
      `--screenshot=${png}`, `${pathToFileURL(page).href}?k=${key}`,
    ], { stdio: "ignore" });
    const made = spawnSync("magick", [png, "-crop", "600x600+0+0", "+repage", "-quality", "76", "-define", "webp:method=6", join(out, `${key}.webp`)]);
    console.log(key, made.status === 0 ? "ok" : String(made.stderr));
  }
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
