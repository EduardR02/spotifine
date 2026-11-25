<script>
  import { updates, checkUpdates, installUpdate, restartUpdate } from "../lib/updates.svelte.js";
</script>

<div class="set-row">
  <div>
    <div class="k">Spotifine {updates.currentVersion}</div>
    {#if updates.status}<div class="d" role="status">{updates.status}</div>{/if}
    {#if updates.error}<div class="inline-error" role="alert">{updates.error}</div>{/if}
  </div>
  <div class="set-ctl">
    {#if updates.phase === "available"}
      <button class="pill accent" onclick={installUpdate}>Install &amp; restart</button>
    {:else if updates.phase === "installed"}
      <button class="pill accent" onclick={restartUpdate}>Restart now</button>
    {:else}
      <button class="pill" onclick={() => checkUpdates()} disabled={updates.busy}>{updates.phase === "checking" ? "Checking…" : "Check"}</button>
    {/if}
  </div>
  {#if updates.phase === "downloading" || updates.phase === "installing"}
    <div class="set-meter" style:--p={updates.phase === "installing" ? 1 : updates.progress}>
      <span class="set-meter-rail"><span class="set-meter-fill"></span></span>
    </div>
  {/if}
</div>
