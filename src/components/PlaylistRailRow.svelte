<script>
  import { navigate } from "../lib/state.svelte.js";
  import { railDrag, trackDrag } from "../lib/dnd.svelte.js";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";
  /* `parent` is the folder the row sits in (null at the top level): a row
     dropped onto this one joins it there. `onpress` starts the rail's own
     drag; a row that cannot move (a filtered list) passes none. */
  let { playlist, active = false, playing = false, pinned = false, depth = 0, parent = null, onmenu, onpress = null, observeCover } = $props();
</script>

<button class="lib-row" class:active class:playing
  class:no-drop={trackDrag.active && trackDrag.sourcePlaylistId === playlist.id}
  class:drag-source={railDrag.item?.kind === "playlist" && railDrag.item.id === playlist.id}
  data-pid={playlist.id} data-parent={parent ?? ""}
  use:observeCover={playlist.id}
  style:padding-left={`${8 + depth * 16}px`}
  aria-haspopup="menu"
  onclick={() => navigate("playlist", playlist.id)}
  onpointerdown={(event) => onpress?.(event, playlist)}
  oncontextmenu={(event) => onmenu?.(event, playlist)}
  onkeydown={(event) => onmenu?.(event, playlist)}>
  <Cover src={playlist.cover_url} srcs={playlist.cover_urls ?? []} id={playlist.id} name={playlist.name} size={32} />
  <span class="lib-name">{playlist.name}</span>
  {#if trackDrag.active}
    <span class="lib-drop-hint" aria-hidden="true"><Icon name="plus" size={13} /></span>
  {:else}
    <span class="lib-tail">
      <!-- A small mark, not a heading: the row is where it is because it was
           pinned, and this says so without a section around it. -->
      {#if pinned}<span class="lib-pin" title="Pinned to top"><Icon name="pin" size={11} /></span>{/if}
      {#if playlist.tracks_total}<span class="lib-count">{playlist.tracks_total}</span>{/if}
    </span>
  {/if}
</button>

<style>
  .lib-tail { display: flex; align-items: center; gap: var(--s2); }
  .lib-pin { display: grid; place-items: center; color: var(--fg-3); }
  .lib-row.drag-source { opacity: 0.4; }
</style>
