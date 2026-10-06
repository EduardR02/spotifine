<script module>
  /**
   * An action asked for from outside the playlist's own page — the rail's
   * right-click menu — that needs the page to carry it out: renaming happens
   * in place of the title, the cleanup and the delete confirmation are
   * sheets over it. The rail navigates there and leaves the request here;
   * PlaylistView takes it once that playlist has loaded.
   */
  export const playlistRequest = $state({ id: null, action: null });
</script>

<script>
  import { addToLibrary, library, navigate, removeFromLibrary, session } from "../lib/state.svelte.js";
  import { isPinned, togglePin } from "../lib/pins.svelte.js";
  import { spotifyLink } from "../lib/spotify-link.js";
  import CopyLinkItem from "./CopyLinkItem.svelte";
  import Icon from "./Icon.svelte";
  import MoveToFolder from "./MoveToFolder.svelte";

  /**
   * THE playlist menu: the items of the playlist header's "…" and of the
   * rail's right-click menu, which are one menu with one wording.
   *
   * `playlist` is a library summary or the loaded playlist (id, name,
   * owner_id). On the playlist's page the host passes `onRename`,
   * `onCleanup` and `onDelete`; anywhere else those go through the page.
   * Liked Songs is a collection rather than a playlist, so its menu is its
   * pin and nothing else.
   *
   * Library membership follows Spotify: a playlist you made is deleted, one
   * someone else made is removed from your library — at once, no question
   * asked — and offered back here while its page is open. Both are the same
   * rootlist edit. In the library it can also move between folders; the host
   * menu is a `group` menu for that submenu.
   */
  let { playlist, trackCount = null, close, onRename = null, onCleanup = null, onDelete = null } = $props();

  const id = $derived(playlist?.id ?? "");
  const count = $derived(trackCount ?? playlist?.tracks_total ?? 0);
  const liked = $derived(id === "liked");
  const editable = $derived(!liked && !!playlist?.owner_id && playlist.owner_id === session.username);
  const inLibrary = $derived(!liked && library.some((entry) => entry.id === id));
  const pinnable = $derived(liked || inLibrary);

  /* Every item reads what it acts on before it closes the menu: in the rail
     the playlist comes from the menu's own state, which closing clears. */
  function pin() {
    togglePin(id);
    close?.(true);
  }

  function run(action, local) {
    const target = id;
    close?.();
    if (local) {
      local();
      return;
    }
    playlistRequest.id = target;
    playlistRequest.action = action;
    navigate("playlist", target);
  }

  function remove() {
    const target = id;
    close?.();
    removeFromLibrary(target);
  }

  function add() {
    const row = playlist;
    close?.(true);
    addToLibrary(row);
  }
</script>

{#if !liked && !inLibrary && id}
  <button class="menu-item" role="menuitem" onclick={add}><Icon name="library-add" size={16} />Add to Your Library</button>
{/if}
{#if pinnable}
  <button class="menu-item" role="menuitem" onclick={pin}><Icon name="pin-line" size={16} />{isPinned(id) ? "Unpin" : "Pin to top"}</button>
{/if}
{#if inLibrary}
  <MoveToFolder item={{ kind: "playlist", id }} {close} />
{/if}
{#if editable}
  <button class="menu-item" role="menuitem" onclick={() => run("rename", onRename)}><Icon name="pencil" size={16} />Rename…</button>
  <button class="menu-item" role="menuitem" disabled={!count} onclick={() => run("cleanup", onCleanup)}><Icon name="filter" size={16} />Remove songs by rules…</button>
{/if}
{#if !liked}
  <CopyLinkItem link={spotifyLink("playlist", id)} {close} />
{/if}
{#if editable}
  <div class="menu-sep" role="separator"></div>
  <button class="menu-item danger" role="menuitem" onclick={() => run("delete", onDelete)}><Icon name="trash" size={16} />Delete playlist…</button>
{:else if inLibrary}
  <div class="menu-sep" role="separator"></div>
  <button class="menu-item" role="menuitem" onclick={remove}><Icon name="library-remove" size={16} />Remove from Your Library</button>
{/if}
