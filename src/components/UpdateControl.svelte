<script>
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { invoke } from "@tauri-apps/api/core";
  import { Update } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";

  /**
   * Updates, as one row: the version and where it stands, and the one thing
   * worth doing about it — check, or install when there is something to
   * install. A download shows as a thin bar in the row's own material.
   * Nothing here runs until asked.
   */
  let currentVersion = $state("");
  let update = $state(null);
  let phase = $state("idle");
  let errorMessage = $state("");
  let downloaded = $state(0);
  let total = $state(0);
  let active = true;

  onMount(() => {
    getVersion()
      .then((version) => {
        if (active) currentVersion = version;
      })
      .catch((error) => {
        if (active) errorMessage = `Could not read app version: ${String(error)}`;
      });
    return () => {
      active = false;
      if (update && phase !== "downloading" && phase !== "installing") {
        void update.close();
      }
    };
  });

  const busy = $derived(phase === "checking" || phase === "downloading" || phase === "installing");
  const progress = $derived(total ? Math.min(1, downloaded / total) : 0);
  const status = $derived(
    phase === "checking" ? "Checking for updates…"
      : phase === "current" ? "Up to date"
      : phase === "available" && update ? `Version ${update.version} is available`
      : phase === "downloading" ? `Downloading ${update?.version ?? "the update"}${total ? ` · ${Math.floor(progress * 100)}%` : "…"}`
      : phase === "installing" ? "Installing…"
      : phase === "installed" ? "Installed · restart to finish"
      : "",
  );

  async function checkUpdates() {
    if (busy) return;
    phase = "checking";
    errorMessage = "";
    if (update) {
      const previous = update;
      update = null;
      try {
        await previous.close();
      } catch (error) {
        phase = "error";
        errorMessage = `Could not release previous update: ${String(error)}`;
        return;
      }
    }
    try {
      // The shell's Update resource includes the final engine flush before
      // Windows launches its installer and exits; keep official install APIs.
      const metadata = await invoke("check_for_update");
      const result = metadata ? new Update(metadata) : null;
      if (!active) {
        if (result) await result.close();
        return;
      }
      update = result;
      phase = result ? "available" : "current";
    } catch (error) {
      if (active) {
        phase = "error";
        errorMessage = `Could not check for updates: ${String(error)}`;
      }
    }
  }

  async function restart() {
    try {
      await relaunch();
    } catch (error) {
      if (active) errorMessage = `The update is installed, but restart failed: ${String(error)}`;
    }
  }

  async function install() {
    if (!update || phase !== "available") return;
    phase = "downloading";
    errorMessage = "";
    downloaded = 0;
    total = 0;
    const selected = update;
    try {
      await selected.downloadAndInstall((event) => {
        if (!active) return;
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
        } else if (event.event === "Finished") {
          phase = "installing";
        }
      });
      if (active) phase = "installed";
      await restart();
    } catch (error) {
      if (active) {
        phase = "error";
        errorMessage = `Could not install the update: ${String(error)}`;
      }
    } finally {
      update = null;
      await selected.close().catch(() => {});
    }
  }
</script>

<div class="set-row">
  <div>
    <div class="k">Spotifine {currentVersion || ""}</div>
    {#if status}<div class="d" role="status">{status}</div>{/if}
    {#if errorMessage}<div class="inline-error" role="alert">{errorMessage}</div>{/if}
  </div>
  <div class="set-ctl">
    {#if phase === "available" && update}
      <button class="pill accent" onclick={install}>Install &amp; restart</button>
    {:else if phase === "installed"}
      <button class="pill accent" onclick={restart}>Restart now</button>
    {:else}
      <button class="pill" onclick={checkUpdates} disabled={busy}>{phase === "checking" ? "Checking…" : "Check"}</button>
    {/if}
  </div>
  {#if phase === "downloading" || phase === "installing"}
    <div class="set-meter" style:--p={phase === "installing" ? 1 : progress}>
      <span class="set-meter-rail"><span class="set-meter-fill"></span></span>
    </div>
  {/if}
</div>
