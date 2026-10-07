<script>
  import { untrack } from "svelte";
  import {
    route,
    navigate,
    navigateArtist,
    library,
    libraryState,
    libraryRailEntries,
    hydrateLibraryCovers,
    followed,
    loadFollowedArtists,
    insertPlaylist,
    api,
    playback,
    session,
    ui,
    createLibraryFolder,
    renameLibraryFolder,
    deleteLibraryFolder,
    moveLibraryItem,
  } from "../lib/state.svelte.js";
  import { pressLibraryItem, railDrag, trackDrag } from "../lib/dnd.svelte.js";
  import { findFolder, folderContents, moveItem, playlistIds } from "../lib/library-tree.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import LikedMark from "./LikedMark.svelte";
  import Menu from "./Menu.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import MoveToFolder from "./MoveToFolder.svelte";
  import PlaylistActions from "./PlaylistActions.svelte";
  import { scrollbar } from "../lib/scrollbar.js";
  import PlaylistRailRow from "./PlaylistRailRow.svelte";
  import { pins, loadPins, isPinned, openFolder, toggleFolder } from "../lib/pins.svelte.js";
  import { avatar, loadAvatar } from "../lib/avatar.svelte.js";
  import { frost } from "../lib/ambient.svelte.js";

  /** What the naming field is making: `{ kind: "playlist" | "folder",
      parent }`, `parent` being the folder a new folder goes in (the field
      then sits under that folder's heading), or null. */
  let creating = $state(null);
  let newName = $state("");
  let field = $state(null);
  let libList = $state(null);
  let railBody = $state(null);
  let rowProbe = $state(null);
  let folderProbe = $state(null);
  let createProbe = $state(null);
  const railHeights = $state({ row: 0, folder: 0, create: 0 });
  let railFirst = $state(0);
  let railLast = $state(20);
  let focusedKey = $state("");
  let fadeTop = $state(false);
  let fadeBottom = $state(false);
  let filtering = $state(false);
  let filterQuery = $state("");
  let filterInput = $state(null);

  const coverRows = new Map();
  let coverObserver = null;
  function observeCover(node, id) {
    coverRows.set(node, id);
    coverObserver?.observe(node);
    return {
      update(nextId) {
        coverRows.set(node, nextId);
        coverObserver?.unobserve(node);
        coverObserver?.observe(node);
      },
      destroy() {
        coverObserver?.unobserve(node);
        coverRows.delete(node);
      },
    };
  }
  $effect(() => {
    const root = libList;
    const account = session.username;
    if (!root || !account) return;
    const observer = new IntersectionObserver((entries) => {
      const ids = entries.filter((entry) => entry.isIntersecting)
        .map((entry) => coverRows.get(entry.target)).filter(Boolean);
      if (ids.length) hydrateLibraryCovers(ids).catch(() => {});
    }, { root });
    coverObserver = observer;
    for (const node of coverRows.keys()) observer.observe(node);
    return () => { observer.disconnect(); coverObserver = null; };
  });

  /**
   * Which collection the one library list is showing.
   *
   * Following used to be a pinned row here, and the owner's objection to that
   * was exact: a permanent row costs a playlist row for ever, for nine
   * artists you rarely open. So each collection is a MODE of the list that
   * already exists rather than anything new in the column — the switch lives
   * in the head beside Filter and New playlist, a row that was already there.
   * Saved podcasts are the third, read by the engine from Spotify's own
   * collection service, so they are there for everyone.
   */
  let mode = $state("playlists");
  const modes = [
    ["playlists", "Playlists"],
    ["artists", "Artists"],
    ["podcasts", "Podcasts"],
  ];

  $effect(() => { loadPins(session.username); });
  $effect(() => { loadAvatar(session.username); });
  const filteredLibrary = $derived(libraryRailEntries(library, libraryState.tree, pins.ids, filterQuery, pins.folders));
  const showLiked = $derived(!filterQuery.trim() || "liked songs".includes(filterQuery.trim().toLocaleLowerCase()));

  /* ---------------- Saved podcasts ----------------
     Read once the tab is first opened — like Following, the collection costs
     nothing while you are not looking at it — and in one answer, newest
     first, with rail-sized artwork. A sign-in to another account starts
     over. */
  const podcasts = $state({ shows: [], loaded: false, loading: false, error: "" });
  let podcastGeneration = 0;
  $effect(() => {
    session.username;
    untrack(() => {
      podcastGeneration++;
      podcasts.shows = [];
      podcasts.loaded = false;
      podcasts.loading = false;
      podcasts.error = "";
    });
  });
  async function loadShows() {
    if (podcasts.loading || !session.username) return;
    const generation = podcastGeneration;
    podcasts.loading = true;
    podcasts.error = "";
    try {
      const shows = await api.browseSavedShows();
      if (generation !== podcastGeneration) return;
      podcasts.shows = Array.isArray(shows) ? shows.filter((show) => show?.id) : [];
      podcasts.loaded = true;
    } catch (reason) {
      if (generation === podcastGeneration) podcasts.error = String(reason || "Could not load your podcasts.");
    } finally {
      if (generation === podcastGeneration) podcasts.loading = false;
    }
  }
  $effect(() => {
    if (mode === "podcasts" && !podcasts.loaded && !podcasts.error) untrack(loadShows);
  });
  const filteredShows = $derived.by(() => {
    const query = filterQuery.trim().toLocaleLowerCase();
    if (!query) return podcasts.shows;
    return podcasts.shows.filter((show) =>
      show.name?.toLocaleLowerCase().includes(query) || show.publisher?.toLocaleLowerCase().includes(query));
  });

  /* ---------------- The right-click menu ----------------
     THE playlist menu (PlaylistActions), hung at the pointer — the same
     items, in the same words, as the playlist page's "…". */
  let railMenu = $state(null);

  function openRailMenu(event, playlist) {
    const keyboard = event.type === "keydown";
    if (keyboard && event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
    event.preventDefault();
    if (trackDrag.active || !session.username) return;
    const trigger = event.currentTarget;
    const bounds = trigger.getBoundingClientRect();
    railMenu = {
      playlist, trigger,
      at: { x: keyboard ? bounds.left + 8 : event.clientX, y: keyboard ? bounds.bottom : event.clientY },
    };
  }

  /* ---------------- Folders ----------------
     A folder heading has its own menu (rename, a folder inside it, move it,
     delete it), renames in place, and is a drop target for the rail's drag.
     Every edit is the rootlist's (state.svelte.js) and shows at once. */
  let folderMenu = $state(null);
  let renaming = $state(null);
  let renameDraft = $state("");
  let renameField = $state(null);
  let deleting = $state(null);

  function openFolderMenu(event, entry) {
    const keyboard = event.type === "keydown";
    if (keyboard && event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
    event.preventDefault();
    if (trackDrag.active || railDrag.active || !session.username) return;
    const trigger = event.currentTarget;
    const bounds = trigger.getBoundingClientRect();
    folderMenu = {
      entry, trigger,
      at: { x: keyboard ? bounds.left + 8 : event.clientX, y: keyboard ? bounds.bottom : event.clientY },
    };
  }

  function folderAction(close, action) {
    const entry = folderMenu?.entry;
    close();
    if (entry) action(entry);
  }

  function startRename(entry) {
    cancelCreate();
    renaming = entry.id;
    renameDraft = entry.name;
  }

  function cancelRename() {
    renaming = null;
    renameDraft = "";
  }

  function commitRename() {
    if (!renaming) return;
    const id = renaming;
    const name = renameDraft;
    cancelRename();
    renameLibraryFolder(id, name);
  }

  $effect(() => {
    if (!renaming || !renameField) return;
    renameField.focus();
    renameField.select();
  });

  function startFolderInside(entry) {
    cancelRename();
    filtering = false;
    filterQuery = "";
    openFolder(entry.id);
    creating = { kind: "folder", parent: entry.id };
  }

  /** What deleting a folder takes with it, said plainly before it happens. */
  function deleteMessage(folder) {
    const { playlists, folders } = folderContents(folder);
    const name = `“${folder.name}”`;
    if (!playlists && !folders) return `${name} is removed from your library.`;
    const parts = [
      playlists ? `${playlists} ${playlists === 1 ? "playlist" : "playlists"}` : "",
      folders ? `${folders} ${folders === 1 ? "folder" : "folders"}` : "",
    ].filter(Boolean).join(" and ");
    const ids = new Set(playlistIds(folder));
    const mine = library.filter((row) => ids.has(row.id) && row.owner_id && row.owner_id === session.username).length;
    const yours = !mine ? ""
      : mine === playlists ? (playlists === 1 ? " You made the playlist, so it is deleted." : " You made all of them, so they are deleted.")
      : ` The ${mine} you made ${mine === 1 ? "is" : "are"} deleted.`;
    return `${name} and everything in it are removed from your library: ${parts}.${yours}`;
  }

  function requestDelete(entry) {
    const folder = findFolder(libraryState.tree, entry.id);
    if (!folder) return;
    deleting = { id: folder.id, message: deleteMessage(folder), busy: false, error: "" };
  }

  async function confirmDelete() {
    const request = deleting;
    if (!request || request.busy) return;
    const inside = new Set(playlistIds(findFolder(libraryState.tree, request.id)));
    request.busy = true;
    request.error = "";
    try {
      await deleteLibraryFolder(request.id);
      openFolder(request.id);
      if (deleting === request) deleting = null;
      if (route.name === "playlist" && inside.has(route.id)) navigate("library");
    } catch (error) {
      if (deleting === request) {
        request.busy = false;
        request.error = error instanceof Error ? error.message : String(error || "Could not delete this folder.");
      }
    }
  }

  /* The rail's drag: a playlist or folder row, pressed and moved, carried to
     a folder heading (into it) or a row (beside it, in its folder). Not while
     filtering — a filtered list shows no folders to aim at. */
  function pressRow(event, item) {
    if (filterQuery.trim() || renaming || event.target.closest?.("input")) return;
    pressLibraryItem({
      event,
      rowEl: event.currentTarget,
      item,
      accepts: (target) => !!moveItem(libraryState.tree, item, target),
      drop: (target) => moveLibraryItem(item, target),
    });
  }

  const dropRoot = $derived(railDrag.active && railDrag.target === null);

  $effect(() => {
    session.username;
    route.name;
    route.id;
    mode;
    filterQuery;
    untrack(() => {
      railMenu = null;
      folderMenu = null;
      createMenu = null;
    });
  });

  const filteredArtists = $derived.by(() => {
    const query = filterQuery.trim().toLocaleLowerCase();
    if (!query) return followed.artists;
    return followed.artists.filter((artist) =>
      artist.name?.toLocaleLowerCase().includes(query),
    );
  });

  const railEntries = $derived.by(() => {
    if (mode === "artists") return filteredArtists.map((artist) => ({ key: `artist:${artist.id}`, artist }));
    if (mode === "podcasts") return filteredShows.map((show) => ({ key: `show:${show.id}`, show }));
    const entries = filteredLibrary.map((entry) => ({
      ...entry, key: entry.kind === "folder" ? `folder:${entry.id}` : `playlist:${entry.playlist.id}`,
    }));
    if (showLiked) {
      const liked = { kind: "liked", key: "liked" };
      if (isPinned("liked")) entries.unshift(liked);
      else entries.push(liked);
    }
    return entries;
  });
  const railIndex = $derived(new Map(railEntries.map((entry, index) => [entry.key, index])));
  const railOffsets = $derived.by(() => {
    const offsets = [0];
    for (const entry of railEntries) {
      const height = entry.kind === "folder" ? railHeights.folder : railHeights.row;
      const editor = entry.kind === "folder" && creating?.parent === entry.id && !entry.collapsed;
      offsets.push(offsets[offsets.length - 1] + height + (editor ? railHeights.create : 0));
    }
    return offsets;
  });
  const renderedRail = $derived.by(() => {
    const indices = new Set();
    for (let i = railFirst; i < Math.min(railLast, railEntries.length); i++) indices.add(i);
    const keep = (key, neighbors = false) => {
      const index = railIndex.get(key);
      if (index == null) return;
      indices.add(index);
      if (neighbors && index > 0) indices.add(index - 1);
      if (neighbors && index + 1 < railEntries.length) indices.add(index + 1);
    };
    keep(focusedKey, true);
    keep(railMenu?.playlist.id === "liked" ? "liked" : `playlist:${railMenu?.playlist.id ?? ""}`);
    keep(`folder:${folderMenu?.entry.id ?? ""}`);
    keep(`folder:${renaming ?? ""}`, true);
    keep(`folder:${creating?.parent ?? ""}`, true);
    keep(railDrag.item ? `${railDrag.item.kind}:${railDrag.item.id}` : "");
    return [...indices].sort((a, b) => a - b).map((index) => ({ entry: railEntries[index], index }));
  });

  function measureRail() {
    if (!libList || !railBody || !railHeights.row) return;
    const above = libList.scrollTop;
    const below = above + libList.clientHeight;
    const at = (offset) => {
      let low = 0, high = railEntries.length;
      while (low < high) {
        const mid = (low + high) >>> 1;
        if (railOffsets[mid + 1] <= offset) low = mid + 1;
        else high = mid;
      }
      return low;
    };
    railFirst = Math.max(0, at(above) - 6);
    railLast = Math.min(railEntries.length, at(below) + 7);
  }

  function retainRailFocus(event) {
    const row = event.target.closest("[data-rail-key]");
    if (row) focusedKey = row.dataset.railKey;
  }

  function releaseRailFocus(event) {
    if (!railBody?.contains(event.relatedTarget)) focusedKey = "";
  }

  $effect(() => {
    if (!rowProbe || !folderProbe || !createProbe) return;
    const measure = () => {
      railHeights.row = rowProbe.getBoundingClientRect().height;
      railHeights.folder = folderProbe.getBoundingClientRect().height;
      railHeights.create = createProbe.getBoundingClientRect().height;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(rowProbe);
    observer.observe(folderProbe);
    observer.observe(createProbe);
    return () => observer.disconnect();
  });

  /* The collection is fetched the first time it is actually asked for, never
     at startup: the whole argument for this switch is that Following costs
     nothing while you are not looking at it.

     Deliberately reactive on `loaded` rather than firing once on the switch.
     Signing into another account empties the collection and clears the flag,
     and the rail may well be sitting on artists when that happens — a list
     that went blank and stayed blank until you toggled twice would be the
     alternative. The loader's own guards make a repeat call free. */
  $effect(() => {
    if (mode === "artists" && !followed.loaded) loadFollowedArtists();
  });

  /* A song in flight has no destination among artists or podcasts — the drop
     targets are playlists — so a drag that starts over either would present
     a rail that silently refuses everything. Switch back and let the gesture
     find its targets. */
  $effect(() => {
    if (trackDrag.active) mode = "playlists";
  });

  /* The observer and scroll listener belong to the element, not to the
     changing filter/results. Keep them mounted while queries are typed or
     playlists arrive; only the fade measurement needs scheduling then. */
  let scrolledMode = "playlists";
  let scrolledQuery = "";
  let scheduleFades = () => {};

  $effect(() => {
    const list = libList;
    if (!list) return;
    let frame = 0;
    const updateFades = () => {
      frame = 0;
      const maxScroll = Math.max(0, list.scrollHeight - list.clientHeight);
      fadeTop = list.scrollTop > 1;
      fadeBottom = list.scrollTop < maxScroll - 1;
      measureRail();
    };
    scheduleFades = () => {
      if (!frame) frame = requestAnimationFrame(updateFades);
    };
    scheduleFades();
    list.addEventListener("scroll", scheduleFades, { passive: true });
    const resizeObserver = new ResizeObserver(scheduleFades);
    resizeObserver.observe(list);
    return () => {
      list.removeEventListener("scroll", scheduleFades);
      resizeObserver.disconnect();
      if (frame) cancelAnimationFrame(frame);
      scheduleFades = () => {};
    };
  });

  $effect(() => {
    const query = filterQuery;
    const current = mode;
    railOffsets;
    const list = libList;
    if (!list) return;
    /* A new result set begins at the top; updating the current result set
       must not kick someone back to the top of a filtered list. */
    if (query !== scrolledQuery || current !== scrolledMode) list.scrollTop = 0;
    scrolledQuery = query;
    scrolledMode = current;
    scheduleFades();
    untrack(measureRail);
  });

  $effect(() => {
    if (creating && field) field.focus();
  });

  $effect(() => {
    if (!filtering) return;
    queueMicrotask(() => filterInput?.focus());

    /* Clicking away dismisses an *empty* filter, the same as pressing Escape.
       A filter with text in it stays: the list on screen is the result of that
       text, so silently discarding it would leave the sidebar showing a subset
       with nothing to explain why. Those are only dismissed deliberately. */
    function onPointerDown(event) {
      if (filterQuery.trim()) return;
      if (filterInput?.closest(".field")?.contains(event.target)) return;
      closeFilter();
    }

    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });

  /* The head's "+" opens Create playlist / Create folder, as Spotify's does;
     either names its new row in place. */
  let createMenu = $state(null);

  /* A right-click on the list itself, not on a row, is where Spotify offers
     the same two: it is also the way in when the narrow rail hides "+". */
  function openListMenu(event) {
    if (mode !== "playlists" || event.target.closest(".lib-row, .lib-folder, form, button, input")) return;
    event.preventDefault();
    if (trackDrag.active || railDrag.active || !session.username) return;
    createMenu = { anchor: null, at: { x: event.clientX, y: event.clientY } };
  }

  function startCreate(kind) {
    /* A previous filter activation may have marked the editor cancelled. */
    filtering = false;
    filterQuery = "";
    cancelRename();
    creating = { kind, parent: null };
  }

  /* A half-typed playlist name is not a name for another collection, and the
     create field sits under a list that is about to become something that
     cannot hold a playlist. Switching abandons it, exactly as pointing at
     Filter does. (The filter needs no such call: it takes over the whole head
     row, so this switch is not on screen while one is open.) */
  function showMode(next) {
    if (mode === next) return;
    cancelCreate();
    mode = next;
  }

  function cancelCreate() {
    creating = null;
    newName = "";
  }

  function startFilter() {
    cancelCreate();
    filtering = true;
  }

  function closeFilter() {
    filtering = false;
    filterQuery = "";
  }

  function createFromFilter() {
    newName = filterQuery.trim();
    closeFilter();
    creating = { kind: "playlist", parent: null };
    commitCreate();
  }

  async function commitCreate() {
    /* Pointer-down on Filter cancels before moving focus. Its ensuing blur
       must not resurrect the discarded partial name as a new playlist. */
    if (!creating) return;
    const { kind, parent } = creating;
    const name = newName.trim();
    creating = null;
    newName = "";
    if (!name) return;
    if (kind === "folder") {
      try {
        await createLibraryFolder(name, parent);
      } catch (error) {
        ui.error = `Could not create the folder "${name}". ${error instanceof Error ? error.message : String(error ?? "")}`.trim();
      }
      return;
    }
    try {
      /* The command answers with the finished row, name included, and that
         row is what goes into the list. Waiting for the rootlist refetch to
         supply it instead is how the name went missing: the refetch reads an
         eventually-consistent rootlist that lists the playlist before its
         attributes are readable, so the row appeared untitled. */
      insertPlaylist(await api.createPlaylist(name));
    } catch (error) {
      /* A creation that fails is not a no-op the user can be left to infer
         from an absent row — the field has already closed by now, so there
         is nowhere local to say it, and the app's one banner (App.svelte) is
         where an action failure with no surviving form belongs. */
      ui.error = `Could not create "${name}". ${
        error instanceof Error ? error.message : String(error ?? "")
      }`.trim();
    }
  }

  /** The badge is what will actually PLAY, not how many rows the queue holds:
      `queue.length` counts the rows the engine walks around too — excluded
      from automatic playback, unavailable — so it read 77 against a Queue view
      headed "68 songs up next". Same arithmetic as that header (QueueView's
      `upNext`), because two counts for one list is one count too many. */
  const upNextCount = $derived.by(() => {
    const planned = playback.upcoming?.length ?? 0;
    /* With nothing playing the view heads its list with the first planned row
       instead of a current track, so that one is not "up next" either. */
    return playback.current_index >= 0 ? planned : Math.max(0, planned - 1);
  });

  /** The playlist context of the current queue row, when it is a playlist. */
  const playingId = $derived.by(() => {
    const context = playback.queue[playback.current_index]?.context;
    if (typeof context !== "string") return null;
    const match = /^playlist:([^:]+)$/.exec(context);
    return match?.[1] ?? null;
  });

  const me = $derived(playback.username || session.username || "");
  const LIKED = { id: "liked", name: "Liked Songs" };
</script>

{#snippet likedRow()}
  <button class="lib-row liked-row" class:active={route.name === "liked"} aria-haspopup="menu"
    onclick={() => navigate("liked")}
    oncontextmenu={(event) => openRailMenu(event, LIKED)}
    onkeydown={(event) => openRailMenu(event, LIKED)}>
    <LikedMark size={32} /><span class="lib-name">Liked Songs</span>
    {#if isPinned("liked")}<span class="lib-pin" title="Pinned to top"><Icon name="pin" size={11} /></span>{/if}
  </button>
{/snippet}

<aside class="sidebar glass-chrome" use:frost>
  <!-- The rail's top edge moves the window; on macOS the traffic lights sit
       in it. -->
  <div class="sidebar-top" data-tauri-drag-region></div>
  <nav class="nav">
    <button class="nav-item" class:active={route.name === "library"} onclick={() => navigate("library")}>
      <!-- Spotifine's mark, where the outline house used to be. It came off
           an inert branding block above this nav; the rail is short enough that
           48px of it was worth a whole library row, and Home is the one
           destination that is also "the app", so the mark still says what it
           said before without costing a line. -->
      <span class="nav-mark"><Icon name="note" size={12} /></span><span>Home</span>
    </button>
    <button class="nav-item" class:active={route.name === "queue"} onclick={() => navigate("queue")}>
      <Icon name="queue" size={17} /><span>Queue</span>
      {#if upNextCount}<span class="nav-count">{upNextCount}</span>{/if}
    </button>
    <button class="nav-item" class:active={route.name === "history"} onclick={() => navigate("history")}>
      <Icon name="clock" size={17} /><span>History</span>
    </button>
  </nav>

  <div class="lib">
    <div class="lib-head">
      {#if filtering}
        <span class="field sm lib-filter">
          <Icon name="search" size={13} />
          <input
            bind:this={filterInput}
            bind:value={filterQuery}
            aria-label={mode === "artists" ? "Filter followed artists" : mode === "podcasts" ? "Filter saved podcasts" : "Filter library"}
            placeholder={mode === "artists" ? "Filter artists" : mode === "podcasts" ? "Filter podcasts" : "Filter library"}
            spellcheck="false"
            onkeydown={(event) => event.key === "Escape" && closeFilter()}
          />
          <button class="field-btn" title="Clear filter" aria-label="Clear filter" onclick={closeFilter}>
            <Icon name="x" size={11} />
          </button>
        </span>
      {:else}
        <!-- The switch IS the section label, so the head row costs exactly
             what it cost before and the list below never moves. The selected
             word is the heading; the others are quiet words you can press. -->
        <div class="lib-modes" role="tablist" aria-label="Library collection">
          {#each modes as [id, label] (id)}
            <button class="lib-mode" class:on={mode === id} role="tab" aria-selected={mode === id} onclick={() => showMode(id)}>
              {label}
            </button>
          {/each}
        </div>
        <div class="lib-head-actions">
          <button
            class="btn-round lib-head-btn"
            title={mode === "artists" ? "Filter followed artists" : mode === "podcasts" ? "Filter saved podcasts" : "Filter library"}
            onpointerdown={cancelCreate}
            onclick={startFilter}
          >
            <Icon name="search" size={13} />
          </button>
          <!-- Creation belongs to playlists; the other collections keep the
               same header geometry without advertising an unrelated action.
               One "+" for both things a library makes, as Spotify's is; a
               right-click on the list opens the same menu. -->
          {#if mode === "playlists"}
            <button
              class="btn-round lib-head-btn lib-new"
              title="Create"
              aria-label="Create playlist or folder"
              aria-haspopup="menu"
              aria-expanded={!!createMenu}
              onpointerdown={cancelCreate}
              onclick={(event) => (createMenu = createMenu ? null : { anchor: event.currentTarget, at: null })}
            >
              <Icon name="plus" size={14} />
            </button>
          {/if}
        </div>
      {/if}
    </div>

    {#if creating && !creating.parent}{@render createField(0)}{/if}

    <div class="lib-list" role="group" aria-label="Your library collection" class:fade-top={fadeTop} class:fade-bottom={fadeBottom} class:droppable={trackDrag.active} class:drop-root={dropRoot} bind:this={libList} use:scrollbar oncontextmenu={openListMenu}>
      <div class="rail-probes" aria-hidden="true">
        <span class="lib-row" bind:this={rowProbe}></span>
        <span class="lib-folder" bind:this={folderProbe}></span>
        <div class="lib-create" bind:this={createProbe}><span class="field sm"><input tabindex="-1" /></span></div>
      </div>
      <div class="rail-body" bind:this={railBody}
        style:height={railHeights.row ? `${railOffsets[railOffsets.length - 1]}px` : null}
        onfocusin={retainRailFocus} onfocusout={releaseRailFocus}>
      {#if mode === "artists"}
        <!-- Portraits, in circles, because a face is most of how an artist is
             recognised and the rail already draws every artist that way. -->
        {#each renderedRail as row (row.entry.key)}
          {@const artist = row.entry.artist}
          <div class="rail-item" data-rail-key={row.entry.key}
            style:position={railHeights.row ? "absolute" : null} style:top={`${railOffsets[row.index]}px`}>
          <button
            class="lib-row"
            class:active={route.name === "artist" && route.id === artist.id}
            onclick={() => navigateArtist(artist.id, artist.name)}
          >
            <Cover src={artist.cover_url} id={artist.id} name={artist.name} size={32} circle />
            <span class="lib-name">{artist.name}</span>
          </button>
          </div>
        {/each}
        <div style:padding-top={`${railOffsets[railOffsets.length - 1]}px`}>
        {#if followed.loading && !followed.artists.length}
          <!-- The same loading frame the playlists get, at the same row
               geometry with a round tile, so the list does not jump. -->
          {#each Array.from({ length: 6 }) as _, i (i)}
            <div class="lib-row" aria-hidden="true">
              <span class="skeleton" style="width:32px;height:32px;border-radius:50%"></span>
              <span class="skeleton line" style="width:{72 - ((i * 13) % 30)}%;height:11px;margin:0"></span>
            </div>
          {/each}
        {:else if followed.error}
          <p class="lib-filter-empty">
            Your artists couldn't load.
            <button class="link-more" onclick={() => loadFollowedArtists({ force: true })}>Try again</button>
          </p>
        {:else if filterQuery.trim() && !filteredArtists.length}
          <p class="lib-filter-empty">No matching artists</p>
        {:else if followed.loaded && !followed.artists.length}
          <p class="lib-filter-empty">You are not following any artists yet</p>
        {/if}
        </div>
      {:else if mode === "podcasts"}
        <!-- Square art and two lines — the show and who makes it — because a
             podcast is recognised by its cover and its publisher together. -->
        {#each renderedRail as row (row.entry.key)}
          {@const show = row.entry.show}
          <div class="rail-item" data-rail-key={row.entry.key}
            style:position={railHeights.row ? "absolute" : null} style:top={`${railOffsets[row.index]}px`}>
          <button
            class="lib-row lib-show"
            class:active={route.name === "show" && route.id === show.id}
            onclick={() => navigate("show", show.id)}
          >
            <Cover src={show.cover_url} id={show.id} name={show.name} size={32} />
            <span class="lib-two">
              <span class="lib-name">{show.name}</span>
              <span class="lib-sub">{show.publisher || "Podcast"}</span>
            </span>
          </button>
          </div>
        {/each}
        <div style:padding-top={`${railOffsets[railOffsets.length - 1]}px`}>
        {#if podcasts.loading && !podcasts.shows.length}
          {#each Array.from({ length: 6 }) as _, i (i)}
            <div class="lib-row" aria-hidden="true">
              <span class="skeleton" style="width:32px;height:32px;border-radius:var(--r1)"></span>
              <span class="skeleton line" style="width:{70 - ((i * 13) % 30)}%;height:11px;margin:0"></span>
            </div>
          {/each}
        {:else if podcasts.error}
          <p class="lib-filter-empty">
            Your podcasts couldn't load.
            <button class="link-more" onclick={loadShows}>Try again</button>
          </p>
        {:else if filterQuery.trim() && !filteredShows.length}
          <p class="lib-filter-empty">No matching podcasts</p>
        {:else if podcasts.loaded && !podcasts.shows.length}
          <p class="lib-filter-empty">No saved podcasts yet</p>
        {/if}
        </div>
      {:else}
        {#each renderedRail as row (row.entry.key)}
          {@const entry = row.entry}
          <div class="rail-item" data-rail-key={entry.key}
            style:position={railHeights.row ? "absolute" : null} style:top={`${railOffsets[row.index]}px`}>
          {#if entry.kind === "liked"}
            {@render likedRow()}
          {:else if entry.kind === "folder"}
            {#if renaming === entry.id}
              <form
                class="lib-folder-rename"
                style:padding-left={`${entry.depth * 16}px`}
                onsubmit={(event) => {
                  event.preventDefault();
                  commitRename();
                }}
              >
                <span class="field sm">
                  <input
                    bind:this={renameField}
                    bind:value={renameDraft}
                    aria-label="Folder name"
                    spellcheck="false"
                    onblur={commitRename}
                    onkeydown={(event) => event.key === "Escape" && cancelRename()}
                  />
                </span>
              </form>
            {:else}
              <!-- A folder heading is its own disclosure: the chevron turns and
                   the rows under it go, and the rail remembers, per account.
                   It is also where a dragged row goes in, and it has a menu. -->
              <button
                class="lib-folder"
                class:shut={entry.collapsed}
                class:drop-into={railDrag.active && railDrag.target === entry.id}
                class:drag-source={railDrag.item?.kind === "folder" && railDrag.item.id === entry.id}
                data-folder={entry.id}
                style:padding-left={`${8 + entry.depth * 16}px`}
                aria-expanded={!entry.collapsed}
                aria-haspopup="menu"
                title={entry.collapsed ? `Show ${entry.name}` : `Hide ${entry.name}`}
                onclick={() => toggleFolder(entry.id)}
                onpointerdown={(event) => pressRow(event, { kind: "folder", id: entry.id })}
                oncontextmenu={(event) => openFolderMenu(event, entry)}
                onkeydown={(event) => openFolderMenu(event, entry)}
              >
                <span class="lib-chevron"><Icon name="chevron-down" size={12} /></span>
                <span class="lib-folder-name">{entry.name}</span>
                {#if entry.collapsed && entry.count}<span class="lib-count">{entry.count}</span>{/if}
              </button>
            {/if}
            {#if creating?.parent === entry.id && !entry.collapsed}{@render createField(entry.depth + 1)}{/if}
          {:else}
            <PlaylistRailRow playlist={entry.playlist} depth={entry.depth} parent={entry.parent} {observeCover}
              pinned={isPinned(entry.playlist.id)}
              active={route.name === "playlist" && route.id === entry.playlist.id} playing={playingId === entry.playlist.id}
              onmenu={openRailMenu}
              onpress={(event, playlist) => pressRow(event, { kind: "playlist", id: playlist.id })} />
          {/if}
          </div>
        {/each}
        <div style:padding-top={`${railOffsets[railOffsets.length - 1]}px`}>
        {#if !libraryState.loaded && !library.length}
          <!-- The rail's own loading frame. Rows at the real height with the
               real tile and name geometry, so the list does not jump when the
               library lands. -->
          {#each Array.from({ length: 8 }) as _, i (i)}
            <div class="lib-row" aria-hidden="true">
              <span class="skeleton" style="width:32px;height:32px;border-radius:var(--r1)"></span>
              <span class="skeleton line" style="width:{78 - ((i * 13) % 34)}%;height:11px;margin:0"></span>
            </div>
          {/each}
        {:else if filterQuery.trim() && !filteredLibrary.length && !showLiked}
          <!-- The filter is also a way to make the playlist you did not find. -->
          <p class="lib-filter-empty">
            No matching playlists
            <button class="link-more" onclick={createFromFilter}>Create “{filterQuery.trim()}”</button>
          </p>
        {:else if libraryState.loaded && !library.length}
          <p class="lib-filter-empty">No playlists in your library yet</p>
        {/if}
        </div>
      {/if}
      </div>
    </div>
  </div>

  <!-- The account, where the Settings row was and at its height: you, opening
       your profile, and the gear for Settings at the end of the same line. -->
  <div class="account">
    <button
      class="nav-item account-me"
      class:active={route.name === "profile" && (!route.id || route.id === me)}
      title="Your profile"
      disabled={!me}
      onclick={() => navigate("profile", me)}
    >
      <Cover src={avatar.url} id={me || "account"} name={me || "?"} size={20} circle />
      <span>{me || "Not signed in"}</span>
    </button>
    <button class="btn-round account-gear" class:on={route.name === "settings"} title="Settings" aria-label="Settings" onclick={() => navigate("settings")}>
      <Icon name="settings" size={17} />
    </button>
  </div>
</aside>

{#snippet createField(depth)}
  <form
    class="lib-create"
    style:padding-left={depth ? `${depth * 16}px` : null}
    onsubmit={(e) => {
      e.preventDefault();
      commitCreate();
    }}
  >
    <span class="field sm">
      <input
        bind:this={field}
        bind:value={newName}
        aria-label={creating?.kind === "folder" ? "New folder name" : "New playlist name"}
        placeholder={creating?.kind === "folder" ? "Folder name" : "Playlist name"}
        spellcheck="false"
        onblur={commitCreate}
        onkeydown={(e) => e.key === "Escape" && cancelCreate()}
      />
    </span>
  </form>
{/snippet}

{#if railMenu}
  <Menu at={railMenu.at} returnTo={railMenu.trigger} group label={`${railMenu.playlist.name} actions`} onclose={() => (railMenu = null)}>
    {#snippet children(close)}
      <PlaylistActions playlist={railMenu.playlist} {close} />
    {/snippet}
  </Menu>
{/if}

{#if createMenu}
  <Menu anchor={createMenu.anchor} at={createMenu.at} label="Create" onclose={() => (createMenu = null)}>
    {#snippet children(close)}
      <button class="menu-item" role="menuitem" onclick={() => { close(); startCreate("playlist"); }}><Icon name="playlist-add" size={16} />Create playlist</button>
      <button class="menu-item" role="menuitem" onclick={() => { close(); startCreate("folder"); }}><Icon name="folder-plus" size={16} />Create folder</button>
    {/snippet}
  </Menu>
{/if}

{#if folderMenu}
  <Menu at={folderMenu.at} returnTo={folderMenu.trigger} group label={`${folderMenu.entry.name} actions`} onclose={() => (folderMenu = null)}>
    {#snippet children(close)}
      <!-- Each item reads its folder before closing: closing drops the menu
           state the items are rendered from. -->
      <button class="menu-item" role="menuitem" onclick={() => folderAction(close, startRename)}><Icon name="pencil" size={16} />Rename</button>
      <button class="menu-item" role="menuitem" onclick={() => folderAction(close, startFolderInside)}><Icon name="folder-plus" size={16} />Create folder</button>
      <MoveToFolder item={{ kind: "folder", id: folderMenu.entry.id }} {close} />
      <div class="menu-sep" role="separator"></div>
      <button class="menu-item danger" role="menuitem" onclick={() => folderAction(close, requestDelete)}><Icon name="trash" size={16} />Delete folder…</button>
    {/snippet}
  </Menu>
{/if}

{#if deleting}
  <ConfirmDialog
    open
    title="Delete this folder?"
    message={deleting.message}
    confirmLabel="Delete folder"
    busyLabel="Deleting…"
    busy={deleting.busy}
    error={deleting.error}
    onConfirm={confirmDelete}
    onCancel={() => (deleting = null)}
  />
{/if}

<style>
  .rail-body { position: relative; overflow-anchor: none; }
  .rail-item { left: 0; right: 0; }
  .rail-probes { position: absolute; visibility: hidden; pointer-events: none; width: 1px; overflow: hidden; }
  .lib-row.liked-row { grid-template-columns: 32px minmax(0, 1fr) auto; }
  .lib-head-btn { width: 24px; height: 24px; }
  /* Three collections do not fit the narrow rail beside two buttons. The
     "+" gives way there; the same Create playlist / Create folder menu opens
     from a right-click on the list, and the filter's "Create" still makes a
     playlist. */
  @media (max-width: 1180px) {
    .lib-new { display: none; }
    .lib-head :global(.lib-mode) { font-size: 11px; }
  }
  .lib-filter { width: 100%; }

  /* A folder heading: a quiet disclosure row, its chevron turned a quarter
     back when shut. */
  .lib-folder {
    display: flex; align-items: center; gap: var(--s2);
    width: 100%; height: 30px; min-width: 0; padding-right: var(--s2);
    border-radius: var(--r2); text-align: left;
    color: var(--fg-2); font-size: var(--t-12);
    transition: color var(--d1) var(--ease), background-color var(--d1) var(--ease);
  }
  .lib-folder:hover { color: var(--fg); background: var(--hover); }
  .lib-chevron { display: grid; place-items: center; width: 16px; flex: none; color: var(--fg-3); transition: transform var(--d2) var(--ease); }
  .lib-folder:hover .lib-chevron { color: var(--fg-2); }
  .lib-folder.shut .lib-chevron { transform: rotate(-90deg); }
  .lib-folder-name { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .lib-folder .lib-count { flex: none; }
  /* Where a dragged row would go: the heading it would go into lifts under a
     foam hairline — arranging is position, not membership — and the row in
     hand dims. A drop to the top level rings the list itself. */
  .lib-folder.drop-into {
    color: var(--fg);
    background: var(--raise-2);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 50%, transparent);
  }
  .lib-folder.drop-into .lib-chevron { color: var(--accent); }
  .lib-folder.drag-source { opacity: 0.4; }
  .lib-list.drop-root { box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 30%, transparent); border-radius: var(--r2); }
  .lib-folder-rename { display: flex; align-items: center; height: 30px; }
  .lib-folder-rename .field { width: 100%; }

  .lib-pin { display: grid; place-items: center; color: var(--fg-3); }

  /* A saved podcast: the name, then its publisher, in the rail's own type. */
  .lib-show { grid-template-columns: 32px minmax(0, 1fr); }
  .lib-two { display: flex; flex-direction: column; min-width: 0; gap: 1px; }
  .lib-sub {
    font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-3);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }

  /* The account row takes the Settings row's place and its 34px. */
  .account { display: flex; align-items: center; gap: var(--s1); }
  .account-me { flex: 1; min-width: 0; }
  .account-me:disabled { opacity: 1; cursor: default; }
  .account-me :global(.art) { flex: none; }
  .account-gear { color: var(--fg-2); }
</style>
