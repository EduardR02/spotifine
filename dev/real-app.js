/**
 * Opens the native app's DevTools bridge for the read-only real-account harness.
 *
 *   bun dev/real-app.js                 unchanged release app, non-admin shell
 *   bun dev/real-app.js --debug --build isolated debug build, then launch
 *   bun dev/real-app.js --debug         launch that debug build again
 *
 * REAL_APP_CDP_PORT selects the port (default 9341); give the dev server the
 * same variable. A debug build embeds it through a command-local Tauri config
 * override, preserving all Windows window properties. It never edits release
 * config, writes registry policy, or closes an existing app.
 *
 * WebView2 Runtime 150+ ignores environment browser flags in elevated hosts:
 * https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/webview-features-flags
 * The release path therefore requires a non-admin shell. Explicit debug builds
 * pass the port via the supported AdditionalBrowserArguments API instead.
 * Debug builds share the real account/state and use the current release engine.
 * Anyone on this computer can drive the port while this app is running.
 */

import { spawn, execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { CDP_PORT } from "./real-bridge.js";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const harness = "http://127.0.0.1:1420/dev/ui-harness.html?real";
const TAURI_PAGE = /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/;

async function tauriPage() {
  try {
    const response = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`, {
      signal: AbortSignal.timeout(1_000),
    });
    const targets = await response.json();
    return { answered: true, page: targets.find((t) => t.type === "page" && TAURI_PAGE.test(t.url)) ?? null };
  } catch {
    return { answered: false, page: null };
  }
}

function runningApps() {
  const csv = execFileSync("tasklist", ["/FI", "IMAGENAME eq Spotifine.exe", "/FO", "CSV", "/NH"], {
    encoding: "utf8",
  });
  return csv.split(/\r?\n/).map((line) => line.match(/^"Spotifine\.exe","(\d+)"/i)?.[1]).filter(Boolean);
}

function elevated() {
  const result = execFileSync("powershell.exe", [
    "-NoProfile", "-NonInteractive", "-Command",
    "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
  ], { encoding: "utf8", windowsHide: true }).trim();
  if (result !== "True" && result !== "False") throw new Error("Could not determine whether this launch is elevated.");
  return result === "True";
}

function browserArguments(existing = "") {
  // The requested port must win over an inherited or previously configured port.
  return `${existing.replace(/(?:^|\s)--remote-debugging-(?:port|address)=\S+/g, "").trim()} --remote-debugging-address=127.0.0.1 --remote-debugging-port=${CDP_PORT}`.trim();
}

function debugConfig() {
  const base = JSON.parse(fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"));
  const windows = JSON.parse(fs.readFileSync(path.join(root, "src-tauri", "tauri.windows.conf.json"), "utf8"));
  // Tauri merge-patch replaces arrays, so pass each full effective window,
  // including existing browser flags, rather than a port-only window entry.
  return {
    app: {
      windows: (windows.app?.windows ?? base.app.windows).map((window) => ({
        ...window,
        additionalBrowserArgs: browserArguments(window.additionalBrowserArgs),
      })),
    },
  };
}

function done(code, message) {
  console.log(message);
  process.exit(code);
}

async function main() {
  const args = new Set(process.argv.slice(2));
  if (args.has("--help")) {
    done(0, [
      "Usage: bun dev/real-app.js [--debug [--build]]",
      "Default: launch target/release/Spotifine.exe from a non-admin shell with a per-process WebView2 port.",
      "--debug --build: build target/debug/Spotifine.exe with a temporary full-window Tauri override, then launch it.",
      "--debug: launch an already-built debug app; rebuild if REAL_APP_CDP_PORT changed.",
      "The debug app uses target/release/PlaybackEngine.exe and your real account/state. No production configuration is changed.",
      `Port: ${CDP_PORT}. Give the harness dev server the same REAL_APP_CDP_PORT.`,
    ].join("\n"));
  }
  for (const arg of args) {
    if (arg !== "--debug" && arg !== "--build") throw new Error(`Unknown option: ${arg}. Use --help.`);
  }
  const debug = args.has("--debug");
  const build = args.has("--build");
  if (build && !debug) throw new Error("--build requires --debug; this helper never rebuilds production with a debugging port.");
  if (!Number.isInteger(CDP_PORT) || CDP_PORT < 1 || CDP_PORT > 65535) throw new Error("REAL_APP_CDP_PORT must be an integer from 1 to 65535.");

  const before = await tauriPage();
  if (before.page && !build) {
    done(0, `Real app is reachable on DevTools port ${CDP_PORT} (${before.page.url}).\nHarness: ${harness}`);
  }
  if (before.answered && !before.page) {
    throw new Error(`Something answers on port ${CDP_PORT}, but it is not the app. Pick another port with REAL_APP_CDP_PORT.`);
  }
  const pids = runningApps();
  if (pids.length) {
    done(2, [
      `Spotifine.exe is already running (pid ${pids.join(", ")}).`,
      "Quit the native app before launching or rebuilding it; this helper never closes an existing app.",
      "For an elevated shell use `bun dev/real-app.js --debug --build`; otherwise use a non-admin shell for the release app.",
      `The real harness may read genuine cached data while offline, but unavailable reads fail rather than using fixtures: ${harness}`,
    ].join("\n"));
  }
  if (!debug && elevated()) {
    throw new Error("This shell is elevated. WebView2 Runtime 150+ ignores environment browser flags for elevated hosts. Use a non-admin shell for the unchanged release app, or explicitly build the isolated debug app with `bun dev/real-app.js --debug --build`. No global policy or registry bypass is used.");
  }

  if (build) {
    const config = JSON.stringify(debugConfig());
    const command = ["tauri", "build", "--debug", "--no-bundle", "--config", config];
    console.log(`Building the debug-only native bridge with arguments: ${JSON.stringify(command)}`);
    const builder = spawn(process.execPath, command, { cwd: root, stdio: "inherit" });
    const code = await new Promise((resolve, reject) => {
      builder.once("error", reject);
      builder.once("exit", resolve);
    });
    if (code !== 0) throw new Error(`Debug build failed (${code ?? "terminated"}); the app was not launched.`);
  }

  const exe = path.join(root, "target", debug ? "debug" : "release", "Spotifine.exe");
  const engine = path.join(root, "target", "release", "PlaybackEngine.exe");
  if (!fs.existsSync(exe) || !fs.existsSync(engine)) {
    throw new Error(debug
      ? "Missing debug app or release engine. Run `bun dev/real-app.js --debug --build`."
      : `No release build at ${exe} with ${engine}. Build it with \`bun tauri build\`.`);
  }
  const child = spawn(exe, [], {
    cwd: path.dirname(exe),
    detached: true,
    stdio: "ignore",
    env: {
      ...process.env,
      ...(debug ? { SPOTIFY_ENGINE_PATH: engine } : {}),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: browserArguments(process.env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS),
    },
  });
  await new Promise((resolve, reject) => {
    child.once("spawn", resolve);
    child.once("error", reject);
  });
  child.unref();
  console.log(`Started ${exe} (pid ${child.pid}); waiting for its Tauri page on localhost:${CDP_PORT}...`);

  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) throw new Error(`The native app exited before its page appeared (exit ${child.exitCode}, signal ${child.signalCode}).`);
    const now = await tauriPage();
    if (now.page) done(0, `Real app is reachable (${now.page.url}).\nHarness: ${harness}`);
    await new Promise((resolve) => setTimeout(resolve, 400));
  }
  throw new Error(`No Tauri page appeared on port ${CDP_PORT} within 30 s. Quit the app and run \`bun dev/real-app.js --debug --build\` to embed this port explicitly. An ordinary debug build without the override does not fix elevated-host environment suppression.`);
}

await main().catch((error) => done(1, String(error?.message ?? error)));
