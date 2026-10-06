<script>
  import Icon from "./Icon.svelte";
  import {
    frame,
    watchFrame,
    minimizeWindow,
    toggleMaximizeWindow,
    closeWindow,
  } from "../lib/window-frame.svelte.js";

  $effect(() => watchFrame());
</script>

<!-- Minimize, maximize and close for the frameless Windows window: the
     cover/Canvas toggle's small round glass, three of them in a row. The
     topbar holds them while the now-playing panel is closed, the panel's
     head while it is open. -->
<div class="win-controls">
  <button class="win-btn glass-plate" type="button" title="Minimize" aria-label="Minimize" onclick={minimizeWindow}>
    <Icon name="win-min" size={10} />
  </button>
  <button
    class="win-btn glass-plate"
    type="button"
    title={frame.maximized ? "Restore" : "Maximize"}
    aria-label={frame.maximized ? "Restore" : "Maximize"}
    onclick={toggleMaximizeWindow}
  >
    <Icon name={frame.maximized ? "win-restore" : "win-max"} size={10} />
  </button>
  <button class="win-btn win-close glass-plate" type="button" title="Close" aria-label="Close" onclick={closeWindow}>
    <Icon name="win-close" size={10} />
  </button>
</div>
