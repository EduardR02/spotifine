<script>
  import { libraryState, moveLibraryItem } from "../lib/state.svelte.js";
  import { folderTargets, parentOf } from "../lib/library-tree.js";
  import { scrollbar } from "../lib/scrollbar.js";
  import Icon from "./Icon.svelte";
  import Menu from "./Menu.svelte";

  /**
   * "Move to folder" for a playlist or a folder (`item`, `{ kind, id }`): an
   * item in the menu it sits in, and the submenu of the folders it can go
   * into, hung beside that menu. "Remove from folders" (Spotify's words)
   * takes it out of every folder. The item draws nothing while there is
   * nowhere to move it.
   *
   * The host menu must be a `group` menu, so a press in this submenu does
   * not dismiss it; choosing closes both through `close`.
   */
  let { item, close } = $props();

  let button = $state(null);
  let placement = $state(null);
  const SUBMENU_W = 220;
  const ROW_H = 32;

  const targets = $derived(folderTargets(libraryState.tree, item));
  const inside = $derived(parentOf(libraryState.tree, item) !== null);

  function show() {
    if (placement) {
      placement = null;
      return;
    }
    const host = button.closest(".menu")?.getBoundingClientRect() ?? button.getBoundingClientRect();
    const row = button.getBoundingClientRect();
    const right = host.right + 4;
    const x = right + SUBMENU_W <= window.innerWidth - 8 ? right : Math.max(8, host.left - 4 - SUBMENU_W);
    const wanted = (targets.length + (inside ? 1 : 0)) * ROW_H + 16;
    const below = window.innerHeight - 8 - (row.top - 4);
    const above = row.bottom + 4 - 8;
    placement = below >= wanted || below >= above
      ? { x, top: row.top - 4, bottom: null, maxH: below }
      : { x, top: null, bottom: window.innerHeight - (row.bottom + 4), maxH: above };
  }

  function choose(folder) {
    /* Read before closing: closing the host menu drops what `item` is
       derived from. */
    const moving = { kind: item.kind, id: item.id };
    placement = null;
    close?.();
    moveLibraryItem(moving, folder);
  }
</script>

{#if targets.length || inside}
  <button class="menu-item" bind:this={button} aria-haspopup="menu" aria-expanded={!!placement} onclick={show}>
    <Icon name="folder-move" size={16} /><span class="menu-label">Move to folder</span>
    <span class="chev"><Icon name="forward" size={14} /></span>
  </button>
{/if}
{#if placement}
  <Menu class="submenu folder-submenu" {placement} group returnTo={button} label="Move to folder" onclose={() => (placement = null)}>
    <div class="folder-scroll" use:scrollbar style:max-height="{placement.maxH}px">
      {#if inside}
        <button class="menu-item" onclick={() => choose(null)}><span class="menu-label">Remove from folders</span></button>
        {#if targets.length}<div class="menu-sep" role="separator"></div>{/if}
      {/if}
      {#each targets as target (target.id)}
        <button class="menu-item" title={target.name} style:padding-left="calc(var(--s3) + {target.depth * 14}px)" onclick={() => choose(target.id)}>
          <span class="menu-label">{target.name}</span>
        </button>
      {/each}
    </div>
  </Menu>
{/if}

<style>
  :global(.menu.folder-submenu) { padding: 0; }
  .folder-scroll { padding: var(--s1); overflow-y: auto; overscroll-behavior: contain; }
  .menu-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
</style>
