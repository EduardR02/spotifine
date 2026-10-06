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
  } from "../lib/state.svelte.js";
  import { trackDrag } from "../lib/dnd.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import LikedMark from "./LikedMark.svelte";
  import Menu from "./Menu.svelte";
  import PlaylistActions from "./PlaylistActions.svelte";
  import { scrollbar } from "../lib/scrollbar.js";
  import PlaylistRailRow from "./PlaylistRailRow.svelte";
  import { pins, loadPins, isPinned, isCollapsed, toggleFolder } from "../lib/pins.svelte.js";
  import { avatar, loadAvatar } from "../lib/avatar.svelte.js";
  import { personalApi, personalConnected, watchPersonal } from "../lib/personal.svelte.js";
  import { frost } from "../lib/ambient.svelte.js";

  let creating = $state(false);
  let newName = $state("");
  let field = $state(null);
  let libList = $state(null);
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
   * Saved podcasts are the third, and only there when the personal app is
   * connected: it is the only thing that can read them.
   */
  let mode = $state("playlists");
  const connected = $derived(personalConnected());
  const modes = $derived([
    ["playlists", "Playlists"],
    ["artists", "Artists"],
    ...(connected ? [["podcasts", "Podcasts"]] : []),
  ]);
  $effect(() => {
    if (mode === "podcasts" && !connected) untrack(() => (mode = "playlists"));
  });

  $effect(() => { loadPins(session.username); });
  $effect(() => { loadAvatar(session.username); });
  $effect(() => watchPersonal());
  const filteredLibrary = $derived(libraryRailEntries(library, libraryState.tree, pins.ids, filterQuery, pins.folders));
  const showLiked = $derived(!filterQuery.trim() || "liked songs".includes(filterQuery.trim().toLocaleLowerCase()));

  /* ---------------- Saved podcasts ----------------
     Read a page at a time, and only once the tab is opened: like Following,
     the collection costs nothing while you are not looking at it. The next
     page is asked for when the end of the list scrolls into view. */
  const SHOWS_PAGE = 50;
  const podcasts = $state({ shows: [], total: null, loading: false, error: "" });
  let podcastGeneration = 0;
  let podcastEnd = $state(null);
  $effect(() => {
    session.username;
    connected;
    untrack(() => {
      podcastGeneration++;
      podcasts.shows = [];
      podcasts.total = null;
      podcasts.loading = false;
      podcasts.error = "";
    });
  });
  async function loadShows() {
    if (podcasts.loading || !personalConnected()) return;
    if (podcasts.total !== null && podcasts.shows.length >= podcasts.total) return;
    const generation = podcastGeneration;
    podcasts.loading = true;
    podcasts.error = "";
    try {
      const page = await personalApi.savedShows(podcasts.shows.length, SHOWS_PAGE);
      if (generation !== podcastGeneration) return;
      const seen = new Set(podcasts.shows.map((show) => show.id));
      const next = (page?.items ?? []).map((item) => item?.show).filter((show) => show?.id && !seen.has(show.id));
      podcasts.shows = [...podcasts.shows, ...next];
      podcasts.total = next.length ? (page?.total ?? podcasts.shows.length) : podcasts.shows.length;
    } catch (reason) {
      if (generation === podcastGeneration) podcasts.error = String(reason || "Could not load your podcasts.");
    } finally {
      if (generation === podcastGeneration) podcasts.loading = false;
    }
  }
  $effect(() => {
    if (mode === "podcasts" && podcasts.total === null && !podcasts.error) untrack(loadShows);
  });
  $effect(() => {
    const sentinel = podcastEnd;
    const root = libList;
    if (!sentinel || !root) return;
    const observer = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting && !podcasts.error) untrack(loadShows);
    }, { root, rootMargin: "0px 0px 240px 0px" });
    observer.observe(sentinel);
    return () => observer.disconnect();
  });
  /** The smallest picture that is still sharp in a 32px tile. */
  function showArt(show) {
    const images = [...(show?.images ?? [])].sort((a, b) => (a.width ?? 0) - (b.width ?? 0));
    return (images.find((image) => (image.width ?? 0) >= 64) ?? images.at(-1))?.url ?? "";
  }
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

  $effect(() => {
    session.username;
    route.name;
    route.id;
    mode;
    filterQuery;
    untrack(() => (railMenu = null));
  });

  const filteredArtists = $derived.by(() => {
    const query = filterQuery.trim().toLocaleLowerCase();
    if (!query) return followed.artists;
    return followed.artists.filter((artist) =>
      artist.name?.toLocaleLowerCase().includes(query),
    );
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
    if (current === "artists") filteredArtists.length;
    else if (current === "podcasts") filteredShows.length;
    else filteredLibrary.length;
    const list = libList;
    if (!list) return;
    /* A new result set begins at the top; updating the current result set
       must not kick someone back to the top of a filtered list. */
    if (query !== scrolledQuery || current !== scrolledMode) list.scrollTop = 0;
    scrolledQuery = query;
    scrolledMode = current;
    scheduleFades();
  });

  $effect(() => {
    if (creating) field?.focus();
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

  function startCreate() {
    /* A previous filter activation may have marked the editor cancelled. */
    filtering = false;
    filterQuery = "";
    creating = true;
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
    creating = false;
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
    creating = true;
    commitCreate();
  }

  async function commitCreate() {
    /* Pointer-down on Filter cancels before moving focus. Its ensuing blur
       must not resurrect the discarded partial name as a new playlist. */
    if (!creating) return;
    const name = newName.trim();
    creating = false;
    newName = "";
    if (!name) return;
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
    <div class="lib-head" class:crowded={modes.length > 2}>
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
               same header geometry without advertising an unrelated action. -->
          {#if mode === "playlists"}
            <button class="btn-round lib-head-btn lib-new" title="New playlist" onclick={startCreate}>
              <Icon name="plus" size={14} />
            </button>
          {/if}
        </div>
      {/if}
    </div>

    {#if creating}
      <form
        class="lib-create"
        onsubmit={(e) => {
          e.preventDefault();
          commitCreate();
        }}
      >
        <span class="field sm">
          <input
            bind:this={field}
            bind:value={newName}
            aria-label="New playlist name"
            placeholder="Playlist name"
            spellcheck="false"
            onblur={commitCreate}
            onkeydown={(e) => e.key === "Escape" && cancelCreate()}
          />
        </span>
      </form>
    {/if}

    <div class="lib-list" class:fade-top={fadeTop} class:fade-bottom={fadeBottom} class:droppable={trackDrag.active} bind:this={libList} use:scrollbar>
      {#if mode === "artists"}
        <!-- Portraits, in circles, because a face is most of how an artist is
             recognised and the rail already draws every artist that way. -->
        {#each filteredArtists as artist (artist.id)}
          <button
            class="lib-row"
            class:active={route.name === "artist" && route.id === artist.id}
            onclick={() => navigateArtist(artist.id, artist.name)}
          >
            <Cover src={artist.cover_url} id={artist.id} name={artist.name} size={32} circle />
            <span class="lib-name">{artist.name}</span>
          </button>
        {/each}
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
      {:else if mode === "podcasts"}
        <!-- Square art and two lines — the show and who makes it — because a
             podcast is recognised by its cover and its publisher together. -->
        {#each filteredShows as show (show.id)}
          <button
            class="lib-row lib-show"
            class:active={route.name === "show" && route.id === show.id}
            onclick={() => navigate("show", show.id)}
          >
            <Cover src={showArt(show)} id={show.id} name={show.name} size={32} />
            <span class="lib-two">
              <span class="lib-name">{show.name}</span>
              <span class="lib-sub">{show.publisher || (show.total_episodes ? `${show.total_episodes} episodes` : "Podcast")}</span>
            </span>
          </button>
        {/each}
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
        {:else if podcasts.total === 0}
          <p class="lib-filter-empty">No saved podcasts yet</p>
        {/if}
        {#if podcasts.total !== null && podcasts.shows.length < podcasts.total && !filterQuery.trim()}
          <div class="lib-end" bind:this={podcastEnd} aria-hidden="true"></div>
        {/if}
      {:else}
        {#if showLiked && isPinned("liked")}{@render likedRow()}{/if}
        {#each filteredLibrary as entry (entry.kind === "folder" ? `folder:${entry.id}` : `playlist:${entry.playlist.id}`)}
          {#if entry.kind === "folder"}
            <!-- A folder heading is its own disclosure: the chevron turns and
                 the rows under it go, and the rail remembers, per account. -->
            <button
              class="lib-folder"
              class:shut={entry.collapsed}
              style:padding-left={`${8 + entry.depth * 16}px`}
              aria-expanded={!entry.collapsed}
              title={entry.collapsed ? `Show ${entry.name}` : `Hide ${entry.name}`}
              onclick={() => toggleFolder(entry.id)}
            >
              <span class="lib-chevron"><Icon name="chevron-down" size={12} /></span>
              <span class="lib-folder-name">{entry.name}</span>
              {#if entry.collapsed && entry.count}<span class="lib-count">{entry.count}</span>{/if}
            </button>
          {:else}
            <PlaylistRailRow playlist={entry.playlist} depth={entry.depth} {observeCover}
              pinned={isPinned(entry.playlist.id)}
              active={route.name === "playlist" && route.id === entry.playlist.id} playing={playingId === entry.playlist.id}
              onmenu={openRailMenu} />
          {/if}
        {/each}
        {#if showLiked && !isPinned("liked")}{@render likedRow()}{/if}
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
      {/if}
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

{#if railMenu}
  <Menu at={railMenu.at} returnTo={railMenu.trigger} label={`${railMenu.playlist.name} actions`} onclose={() => (railMenu = null)}>
    {#snippet children(close)}
      <PlaylistActions playlist={railMenu.playlist} {close} />
    {/snippet}
  </Menu>
{/if}

<style>
  .lib-row.liked-row { grid-template-columns: 32px minmax(0, 1fr) auto; }
  .lib-head-btn { width: 24px; height: 24px; }
  /* Three collections do not fit the narrow rail beside two buttons. New
     playlist gives way (the filter's "Create" still makes one); the words
     close up a step. */
  @media (max-width: 1180px) {
    .lib-head.crowded .lib-new { display: none; }
    .lib-head.crowded .lib-mode { font-size: 11px; }
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

  .lib-pin { display: grid; place-items: center; color: var(--fg-3); }

  /* A saved podcast: the name, then its publisher, in the rail's own type. */
  .lib-show { grid-template-columns: 32px minmax(0, 1fr); }
  .lib-two { display: flex; flex-direction: column; min-width: 0; gap: 1px; }
  .lib-sub {
    font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-3);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .lib-end { height: 1px; }

  /* The account row takes the Settings row's place and its 34px. */
  .account { display: flex; align-items: center; gap: var(--s1); }
  .account-me { flex: 1; min-width: 0; }
  .account-me:disabled { opacity: 1; cursor: default; }
  .account-me :global(.art) { flex: none; }
  .account-gear { color: var(--fg-2); }
</style>
