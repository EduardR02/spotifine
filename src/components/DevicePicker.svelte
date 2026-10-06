<script>
  import { personalApi, personalDevicesActive, watchPersonal } from "../lib/personal.svelte.js";
  import { api, playback } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Menu from "./Menu.svelte";

  // Discovery is demand-only; selected-output sync belongs to the native
  // playback router and remains active when this menu closes. With devices
  // turned off in Settings there is no button and nothing here runs.
  let open = $state(false);
  let button = $state(null);
  let devices = $state([]);
  let loading = $state(false);
  let error = $state("");
  let transferring = $state("");
  let generation = 0;

  $effect(() => watchPersonal());
  const active = $derived(personalDevicesActive());
  const remote = $derived(playback.output_device_id);
  const remoteName = $derived(playback.output_device_name);
  $effect(() => {
    if (active) return;
    generation++;
    open = false;
    devices = [];
  });

  async function refresh() {
    if (!open || loading || !personalDevicesActive()) return;
    const current = generation;
    loading = true;
    error = "";
    try {
      const list = await personalApi.devices();
      if (current !== generation) return;
      devices = Array.isArray(list) ? list : [];
    } catch (reason) {
      if (current === generation) error = String(reason);
    } finally {
      if (current === generation) loading = false;
    }
  }

  function toggle() {
    open = !open;
    if (open) refresh();
  }

  function dismiss() {
    open = false;
    generation++;
    loading = false;
    transferring = "";
  }

  async function select(device = null) {
    if ((device && (!device.id || device.is_restricted)) || transferring) return;
    const current = generation;
    transferring = device?.id ?? "local";
    error = "";
    try {
      await api.selectOutput(device?.id ?? null);
      if (current === generation && device) await refresh();
    } catch (reason) {
      if (current === generation) error = String(reason);
    } finally {
      if (current === generation) transferring = "";
    }
  }

  /* Spotify names a device's kind in CamelCase ("CastVideo", "GameConsole");
     folded to letters, each maps to a glyph and a word a person would use. */
  const KINDS = {
    computer: ["computer", "Computer"],
    smartphone: ["phone", "Phone"],
    tablet: ["tablet", "Tablet"],
    speaker: ["speaker", "Speaker"],
    avr: ["speaker", "Receiver"],
    audiodongle: ["cast", "Audio dongle"],
    castaudio: ["cast", "Cast"],
    castvideo: ["cast", "Cast"],
    tv: ["tv", "TV"],
    stb: ["tv", "Set-top box"],
    gameconsole: ["tv", "Console"],
    automobile: ["speaker", "Car"],
  };
  const kind = (type) => KINDS[String(type ?? "").toLowerCase().replace(/[^a-z]/g, "")] ?? ["speaker", ""];
</script>

{#snippet row(device, glyph, label, selected, busy, unusable, title, onclick)}
  <button
    class="menu-item device-row"
    class:selected
    role="menuitemradio"
    aria-checked={selected}
    aria-busy={busy}
    disabled={unusable || !!transferring}
    {title}
    {onclick}
  >
    <Icon name={glyph} size={16} />
    <span class="device-name">{device}</span>
    <span class="device-kind">{busy ? "Connecting…" : label}</span>
    <span class="device-state">{#if selected}<Icon name="check" size={14} />{/if}</span>
  </button>
{/snippet}

{#if active || remote}
  <button
    class="btn-round device-btn"
    class:on={!!remote}
    bind:this={button}
    title={remote ? `Playing on ${remoteName}` : "Playing on this computer"}
    aria-label="Spotify Connect devices"
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={toggle}
  >
    <Icon name="devices" size={18} />
  </button>
  {#if open}
    <Menu anchor={button} side="above" align="end" label="Spotify Connect devices" class="device-menu" onclose={dismiss}>
      {#snippet children()}
        <div class="device-head">
          <span class="device-title" title="Spotify devices play the original audio: speed and song edits apply only on this computer.">Devices</span>
          <button
            class="btn-round device-refresh"
            class:spinning={loading}
            title="Refresh devices"
            aria-label="Refresh devices"
            disabled={loading || !active}
            onclick={refresh}
          >
            <Icon name="refresh" size={15} />
          </button>
        </div>
        {@render row(
          "This computer", "computer", "", !remote, transferring === "local", false,
          remote ? "Bring playback back here, paused" : "Spotifine is playing here",
          () => select(),
        )}
        {#if devices.length}
          <div class="menu-sep" role="separator"></div>
        {/if}
        {#each devices as device (device.id ?? device.name)}
          {@const [glyph, label] = kind(device.type)}
          {@render row(
            device.name, glyph, label, remote === device.id, transferring === device.id,
            !active || !device.id || device.is_restricted,
            device.is_restricted ? `${device.name} can't be controlled` : remote === device.id ? `Spotifine is playing on ${device.name}` : `Play on ${device.name}`,
            () => select(device),
          )}
        {/each}
        {#if !devices.length && !error}
          <p class="device-note">{loading ? "Looking for devices…" : "No other devices found"}</p>
        {/if}
        {#if error || (remote && playback.error)}
          <p class="device-error" role="alert" title={error || playback.error}>{error || playback.error}</p>
        {/if}
      {/snippet}
    </Menu>
  {/if}
{/if}

<style>
  /* Playing elsewhere is a state the bar keeps showing: the glyph in foam and
     the dot every other "on" control in the bar wears. */
  .device-btn { position: relative; }
  .device-btn.on::after {
    content: ""; position: absolute; bottom: 1px; left: 50%; margin-left: -1.5px;
    width: 3px; height: 3px; border-radius: 50%; background: var(--accent);
  }

  /* A device list, not a form: a caps label with its one action, then the
     rows — this computer first, the account's other devices under a
     hairline. A row is the menu's item with a device glyph in the leading
     column, the name, and the kind in quiet small type at the end. */
  :global(.menu.device-menu) { width: 280px; }
  .device-head {
    display: flex; align-items: center; justify-content: space-between;
    height: 32px; padding: 0 0 0 var(--s3); margin-bottom: 2px;
  }
  .device-title {
    font-family: var(--font-small); font-size: var(--t-caps); font-weight: var(--w-semi);
    letter-spacing: var(--track-caps); text-transform: uppercase; color: var(--label);
    cursor: default;
  }
  .device-refresh { width: 28px; height: 28px; }
  .device-refresh:disabled { opacity: 1; color: var(--fg-3); }
  /* Turns only while a lookup is in flight, so it costs nothing at rest. */
  .device-refresh.spinning :global(.icon) { animation: device-spin 0.9s linear infinite; }
  @keyframes device-spin { to { transform: rotate(360deg); } }

  .device-row { height: 36px; }
  .device-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .device-kind {
    flex: none; margin-left: auto; color: var(--fg-3);
    font-family: var(--font-small); font-size: var(--t-11);
  }
  .device-state { display: grid; place-items: center; width: 14px; flex: none; margin-left: calc(var(--s2) - var(--s3)); color: var(--accent); }
  /* Where Spotifine plays now is foam, glyph and name, with a check at the
     end: the one row that says "here". */
  .device-row.selected { color: var(--accent); }
  .device-row.selected > :global(.icon:first-child) { color: var(--accent); }
  .device-row:disabled:not([aria-busy="true"]) { opacity: 0.45; }
  .device-row[aria-busy="true"] { opacity: 1; }
  .device-row[aria-busy="true"] .device-kind { color: var(--fg-2); }

  .device-note {
    margin: 0; padding: var(--s2) var(--s3) var(--s1) calc(var(--s3) + 16px + var(--s3));
    color: var(--fg-3); font-family: var(--font-small); font-size: var(--t-11);
  }
  /* One line; the whole text is on the tooltip. */
  .device-error {
    margin: var(--s1) 0 var(--s1); padding: 0 var(--s3);
    color: var(--love); font-family: var(--font-small); font-size: var(--t-11); line-height: 1.4;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
</style>
