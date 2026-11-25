import { getVersion } from "@tauri-apps/api/app";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { Update } from "@tauri-apps/plugin-updater";

const DAY = 24 * 60 * 60 * 1000;
const RETRY_BACKOFF = 15 * 60 * 1000;
const CACHE_KEY = "sr.update-check";
let timer;
let running = false;
let update = null;

export const updates = new class {
  currentVersion = $state("");
  version = $state("");
  phase = $state("idle");
  error = $state("");
  downloaded = $state(0);
  total = $state(0);

  get busy() {
    const phase = this.phase;
    return phase === "checking" || phase === "downloading" || phase === "installing" || phase === "restarting";
  }
  get progress() {
    return this.total ? Math.min(1, this.downloaded / this.total) : 0;
  }
  get status() {
    switch (this.phase) {
      case "checking": return "Checking for updates…";
      case "current": return "Up to date";
      case "available": return `Version ${this.version} is available`;
      case "downloading": return `Downloading ${this.version}${this.total ? ` · ${Math.floor(this.progress * 100)}%` : "…"}`;
      case "installing": return "Installing…";
      case "installed": return "Installed · restart to finish";
      case "restarting": return "Restarting…";
      default: return "";
    }
  }
}();

function schedule(delay) {
  clearTimeout(timer);
  if (!running) return;
  timer = setTimeout(() => {
    if (updates.busy) schedule(DAY);
    else void checkUpdates(true);
  }, delay);
}

export function startUpdateChecks() {
  if (!isTauri()) return;
  running = true;
  void getVersion().catch(() => "").then((version) => {
    if (!running) return;
    updates.currentVersion = version;
    // The cached version keeps the affordance visible across restarts. The
    // native resource is window-local and is always obtained before install.
    let cached;
    try { cached = JSON.parse(localStorage.getItem(CACHE_KEY)); } catch { /* optional */ }
    if (version && cached?.installedVersion === version && !updates.busy && updates.phase === "idle") {
      updates.version = cached.availableVersion || "";
      updates.phase = updates.version ? "available" : "idle";
      const age = Date.now() - cached.checkedAt;
      schedule(age >= 0 && age < DAY ? DAY - age : 0);
    } else if (!updates.busy && updates.phase === "idle") {
      schedule(0);
    }
  });
  return () => {
    running = false;
    clearTimeout(timer);
  };
}

export async function checkUpdates(automatic = false) {
  if (updates.busy || updates.phase === "installed") return;
  const previousPhase = updates.phase;
  updates.phase = "checking";
  if (!automatic) updates.error = "";
  try {
    // The shell checkpoints playback before Windows hands off to the installer.
    const metadata = await invoke("check_for_update");
    const previous = update;
    update = metadata ? new Update(metadata) : null;
    updates.version = update?.version ?? "";
    updates.phase = update ? "available" : "current";
    updates.error = "";
    if (previous) void previous.close().catch(() => {});
    if (updates.currentVersion) {
      try {
        localStorage.setItem(CACHE_KEY, JSON.stringify({
          installedVersion: updates.currentVersion, availableVersion: updates.version, checkedAt: Date.now(),
        }));
      } catch { /* optional */ }
    }
    schedule(DAY);
  } catch (error) {
    updates.phase = previousPhase;
    if (!automatic) updates.error = `Could not check for updates: ${String(error)}`;
    schedule(RETRY_BACKOFF);
  }
}

export async function restartUpdate() {
  if (updates.phase !== "installed") return;
  updates.error = "";
  updates.phase = "restarting";
  try {
    await invoke("relaunch_app");
  } catch (error) {
    updates.phase = "installed";
    updates.error = `The update is installed, but restart failed: ${String(error)}`;
  }
}

export async function installUpdate() {
  if (updates.phase !== "available") return;
  if (!update) await checkUpdates();
  if (!update || updates.phase !== "available") return;
  updates.phase = "downloading";
  updates.error = "";
  updates.downloaded = 0;
  updates.total = 0;
  const selected = update;
  try {
    await selected.downloadAndInstall((event) => {
      if (event.event === "Started") updates.total = event.data.contentLength ?? 0;
      else if (event.event === "Progress") updates.downloaded += event.data.chunkLength;
      else if (event.event === "Finished") updates.phase = "installing";
    });
  } catch (error) {
    update = null;
    void selected.close().catch(() => {});
    updates.phase = "available";
    updates.error = `Could not install the update: ${String(error)}`;
    return;
  }
  update = null;
  void selected.close().catch(() => {});
  updates.phase = "installed";
  await restartUpdate();
}
