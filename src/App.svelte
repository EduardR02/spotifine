<script>
  import { tick, untrack } from "svelte";
  import {
    initEvents,
    route,
    loadDetail,
    detail,
    libraryState,
    playback,
    credits,
    togglePlay,
    focusSearch,
    isLoggedOut,
    goBack,
    goForward,
    ui,
    api,
    maybeBackfillLazyQueue,
    setPlayheadVisible,
  } from "./lib/state.svelte.js";
  import IconSprite from "./components/IconSprite.svelte";
  import Icon from "./components/Icon.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import PlayerBar from "./components/PlayerBar.svelte";
  import TopBar from "./components/TopBar.svelte";
  import CreditsDialog from "./components/CreditsDialog.svelte";
  import NowPlayingPanel from "./components/NowPlayingPanel.svelte";
  import Ambient from "./components/Ambient.svelte";
  import { scrollbar } from "./lib/scrollbar.js";
  import { frost, shiftHaze } from "./lib/ambient.svelte.js";
  import { panelLayout, snapPanel } from "./lib/layout.js";
  import { panel, setPanelWidth, nowPlayingShown } from "./lib/panel.svelte.js";
  import { startPanelDrag } from "./lib/panel-drag.js";
  import { spaceControlsPlayback } from "./lib/playback-shortcuts.js";
  import { devicesEnabled } from "./lib/personal.svelte.js";
  import { setMeterActivity } from "./lib/vu-meter.js";
  import { os, frame, watchFrame } from "./lib/window-frame.svelte.js";
  import LibraryView from "./views/LibraryView.svelte";
  import MadeForYouView from "./views/MadeForYouView.svelte";
  import LikedSongsView from "./views/LikedSongsView.svelte";
  import PlaylistView from "./views/PlaylistView.svelte";
  import RadioView from "./views/RadioView.svelte";
  import AlbumView from "./views/AlbumView.svelte";
  import ArtistView from "./views/ArtistView.svelte";
  import DiscographyView from "./views/DiscographyView.svelte";
  import FansAlsoLikeView from "./views/FansAlsoLikeView.svelte";
  import AppearsOnView from "./views/AppearsOnView.svelte";
  import ArtistPlaylistCollectionView from "./views/ArtistPlaylistCollectionView.svelte";
  import SearchView from "./views/SearchView.svelte";
  import SearchSongsView from "./views/SearchSongsView.svelte";
  import QueueView from "./views/QueueView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import HistoryView from "./views/HistoryView.svelte";
  import LoginView from "./views/LoginView.svelte";
  import PodcastView from "./views/PodcastView.svelte";
  import ProfileView from "./views/ProfileView.svelte";

  $effect(() => {
    initEvents();
  });

  // The stylesheet's hook for the window frame (see window-frame.svelte.js).
  document.documentElement.dataset.os = os;

  $effect(() => watchFrame());

  let trackEditor = null;
  const loadTrackEditor = () => (trackEditor ??= import("./views/TrackEditorView.svelte"));

  /* The content pane's own width, published for the track table.
     A ResizeObserver rather than a window resize listener, because the pane
     also changes width when the inspector opens and the window does not; and
     rather than a media query, because the arithmetic that turns a window
     width into a pane width was being written out by hand and got it wrong.
     This is the only layout read outside a scroll handler and it fires only
     when the pane actually changes size. */
  let paneEl = $state(null);
  $effect(() => {
    const node = paneEl;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) => {
      const width = Math.round(entry.contentRect.width);
      if (width !== ui.paneWidth) ui.paneWidth = width;
      relayoutPanel();
    });
    observer.observe(node);
    ui.paneWidth = Math.round(node.getBoundingClientRect().width);
    return () => observer.disconnect();
  });

  /* ---- The now-playing panel's width (layout.js) -----------------------
     Worked out from the window and from where the pane starts — the rail's
     width is the rail's business, so it is measured, not assumed — on a
     resize and when the pane moves; never per frame at rest. Until the pane
     has been measured the rail is taken at its token, so the first paint
     already has the right shape. */
  const GUTTER = 8; // app.css --gutter
  let paneStart =
    2 * GUTTER + (parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--sidebar-w")) || 240);
  const measurePanel = () =>
    panelLayout({
      width: window.innerWidth,
      height: window.innerHeight,
      start: paneStart,
      gutter: GUTTER,
      dpr: window.devicePixelRatio || 1,
      user: panel.width,
    });
  const first = measurePanel();
  let box = $state(first);
  panel.fits = first.fits;

  /* The pane's start is read fresh each time: a resize can carry the rail
     across its breakpoint, and a stale start would hide the panel for a
     frame and bring it straight back. */
  function relayoutPanel() {
    if (paneEl) paneStart = paneEl.getBoundingClientRect().left;
    const next = measurePanel();
    if (next.fits !== panel.fits) panel.fits = next.fits;
    if (next.width !== box.width || next.min !== box.min || next.max !== box.max) box = next;
  }

  $effect(() => {
    panel.width;
    untrack(relayoutPanel);
  });
  $effect(() => {
    window.addEventListener("resize", relayoutPanel);
    return () => window.removeEventListener("resize", relayoutPanel);
  });

  /* ---- The handle -------------------------------------------------------
     In the gap left of the panel, like the official client's. A drag sets
     the width on the grid each frame (dragWidth) and nothing else: the
     haze slides with the edge instead of being redrawn (shiftHaze), and the
     panel holds its anchor back until it is let go (panel.resizing). The
     width it ends on is kept; a double-click goes back to automatic. */
  let dragWidth = $state(null);
  const shownWidth = $derived(dragWidth ?? box.width);
  let cancelPanelDrag = null;

  $effect(() => {
    if (!nowPlayingShown()) cancelPanelDrag?.();
    return () => cancelPanelDrag?.();
  });

  /** Keep `width` (null: automatic), in one step: the grid never sees the
      old width again on the way, and the haze is slid to the new edge until
      the picture drawn for it fades in. */
  function keepPanelWidth(width) {
    setPanelWidth(width);
    relayoutPanel();
    dragWidth = null;
    shiftHaze(window.innerWidth - box.width);
    shiftHaze(null);
  }

  function resizePanel(event) {
    if (event.button !== 0 || event.isPrimary === false || cancelPanelDrag) return;
    event.preventDefault();
    const from = box.width;
    panel.resizing = true;
    cancelPanelDrag = startPanelDrag(event, {
      width: from,
      min: box.min,
      max: box.max,
      dpr: window.devicePixelRatio || 1,
      onChange(width) {
        if (width === shownWidth) return;
        dragWidth = width;
        shiftHaze(window.innerWidth - width);
      },
      onEnd(commit) {
        cancelPanelDrag = null;
        if (commit && dragWidth != null && dragWidth !== from) keepPanelWidth(dragWidth);
        dragWidth = null;
        shiftHaze(null, !commit);
        panel.resizing = false;
      },
    });
  }

  /** The keyboard's way: a step per arrow, Home for automatic. */
  function nudgePanel(event) {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const step = { ArrowLeft: 16, ArrowRight: -16 }[event.key];
    if (event.key === "Home") {
      event.preventDefault();
      keepPanelWidth(null);
    } else if (step) {
      event.preventDefault();
      keepPanelWidth(snapPanel(Math.min(box.max, Math.max(box.min, box.width + step)), window.devicePixelRatio || 1));
    }
  }

  /* Every route reuses this one scroll container, so the pane keeps a small
     ledger of where each route identity (name + id + param) was last left,
     written by a passive scroll listener for as long as a view is live.
     Revisiting a route restores its entry; a first visit opens at the top.
     Recording from the listener rather than at navigation time matters:
     leaving a detail route drops its data before the route flips, the
     outgoing view collapses, and by the time navigation could read the
     pane the browser has already clamped scrollTop — the last event that
     fired under the outgoing identity is the honest position. Zeroing in
     $effect.pre keeps the incoming view from inheriting those pixels for
     even one frame.

     Detail pages fetch their body after mounting, so a remembered offset
     routinely exceeds what is laid out yet. Restoration follows layout growth
     while content is loading, then settles at the attainable end if the final
     range is shorter. Intentional scrolling cancels the wait. No polling or
     extra fetch: child resizes and DOM replacements wake the check. */
  let scrollEl = $state(null);
  const SCROLL_MEMORY_MAX = 50; // matches the navigation history scale
  const scrollMemory = new Map(); // route identity -> last pane offset
  let lastRouteKey = null;
  let pendingRestoreKey = null;

  function routeKey() {
    return `${route.name}\u0000${route.id ?? ""}\u0000${route.param ?? ""}`;
  }

  function remember(key, offset) {
    scrollMemory.delete(key);
    scrollMemory.set(key, offset);
    if (scrollMemory.size > SCROLL_MEMORY_MAX) {
      scrollMemory.delete(scrollMemory.keys().next().value);
    }
  }

  $effect.pre(() => {
    const key = routeKey();
    const node = scrollEl;
    const previous = lastRouteKey;
    lastRouteKey = key;
    if (!node || previous === null || previous === key) return;
    pendingRestoreKey = key;
    node.scrollTop = 0;
  });

  $effect(() => {
    const node = scrollEl;
    if (!node) return;
    const onScroll = () => {
      if (lastRouteKey === null || lastRouteKey === pendingRestoreKey) return;
      remember(lastRouteKey, node.scrollTop);
    };
    node.addEventListener("scroll", onScroll, { passive: true });
    return () => node.removeEventListener("scroll", onScroll);
  });

  $effect(() => {
    const key = routeKey();
    const node = scrollEl;
    if (!node) return;
    const target = scrollMemory.get(key) ?? 0;
    let disposed = false;
    let observer = null;
    let mutations = null;
    const watched = new Set();

    function stopWaiting() {
      observer?.disconnect();
      mutations?.disconnect();
      observer = null;
      mutations = null;
      if (pendingRestoreKey === key) pendingRestoreKey = null;
    }
    function contentPending() {
      const name = route.name;
      const id = route.id;
      if (["playlist", "radio", "album", "artist", "discography",
        "fans-also-like", "appears-on", "artist-playlists", "discovered-on"].includes(name)) {
        const payload = name === "playlist" ? detail.playlist
          : name === "radio" ? detail.radio
          : name === "album" ? detail.album : detail.artist;
        return !detail.error && payload?.id !== id;
      }
      if (name === "library" && !libraryState.loaded) return true;
      if (name === "show" || name === "episode") {
        return !!node.querySelector('.podcast-page[aria-busy="true"]');
      }
      // History and Liked Songs own their first fetch inside their view.
      // Their placeholder rows disappear when the first answer is rendered.
      return (name === "history" || name === "liked") && !!node.querySelector(".sk-row");
    }
    function apply(top) {
      node.scrollTo({ top, behavior: "instant" });
    }
    function check() {
      if (disposed || pendingRestoreKey !== key) return;
      const range = Math.max(0, node.scrollHeight - node.clientHeight);
      if (range >= target || !contentPending()) {
        stopWaiting();
        apply(Math.min(target, range));
      }
    }
    function watchChildren() {
      if (!observer) return;
      for (const child of watched) {
        if (child.parentNode !== node) {
          observer.unobserve(child);
          watched.delete(child);
        }
      }
      for (const child of node.children) {
        if (watched.has(child)) continue;
        watched.add(child);
        observer.observe(child);
      }
    }
    function cancelForUser() {
      if (pendingRestoreKey !== key) return;
      stopWaiting();
      remember(key, node.scrollTop);
    }
    function onKey(event) {
      if (["ArrowDown", "ArrowUp", "PageDown", "PageUp", "Home", "End", " "].includes(event.key) &&
        (node.contains(document.activeElement) || document.activeElement === document.body)) {
        cancelForUser();
      }
    }
    function onPointer(event) {
      if (event.target.closest?.(".sb")?.previousElementSibling === node) cancelForUser();
    }
    function onWheel(event) {
      if (event.target.closest?.(".sb")?.previousElementSibling === node) cancelForUser();
    }
    node.addEventListener("wheel", cancelForUser, { passive: true });
    node.addEventListener("touchstart", cancelForUser, { passive: true });
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("pointerdown", onPointer, true);

    window.addEventListener("wheel", onWheel, { passive: true });
    // Wait for the incoming view before testing its scroll range. Only a
    // still-short loading view needs observers; normal restores are one write.
    tick().then(() => {
      if (disposed || pendingRestoreKey !== key) return;
      check();
      if (pendingRestoreKey !== key) return;
      observer = new ResizeObserver(() => {
        watchChildren();
        check();
      });
      observer.observe(node);
      watchChildren();
      mutations = new MutationObserver(() => {
        watchChildren();
        check();
      });
      // Child resizes cover virtualized tables; mutations cover skeleton
      // replacement even if the replacement is shorter.
      mutations.observe(node, { childList: true, subtree: true });
    });

    return () => {
      disposed = true;
      stopWaiting();
      node.removeEventListener("wheel", cancelForUser);
      node.removeEventListener("touchstart", cancelForUser);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("wheel", onWheel);
    };
  });

  // Dynamic album/catalogue queues stay small. The next bounded page is
  // requested only when playback approaches the loaded tail.
  $effect(() => {
    playback.current_index;
    playback.queue.length;
    untrack(() => maybeBackfillLazyQueue().catch(() => {}));
  });

  /* The decorative meter's one shared clock follows playback and actual
     window visibility, not the WebView's sometimes-stale focus alone. */
  let pageVisible = $state(!document.hidden);
  $effect(() => {
    ui.windowFocused = document.hasFocus();
    const on = () => (ui.windowFocused = true);
    const off = () => (ui.windowFocused = false);
    const visibility = () => (pageVisible = !document.hidden);
    window.addEventListener("focus", on);
    window.addEventListener("blur", off);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      window.removeEventListener("focus", on);
      window.removeEventListener("blur", off);
      document.removeEventListener("visibilitychange", visibility);
    };
  });
  $effect(() => {
    setMeterActivity(playback.playing && ui.windowFocused && pageVisible && !frame.minimized);
    return () => setMeterActivity(false);
  });

  /* The playhead's 4 Hz tick follows whether the window can be seen at all —
     not focus: a visible window behind another one still shows its progress
     bar. Untracked, because a restore re-anchors from the projection, and this
     effect must not come to depend on the playhead it is gating. */
  $effect(() => {
    const seen = pageVisible && !frame.minimized;
    untrack(() => setPlayheadVisible(seen));
  });

  /* The same signal paces a Spotify device: the shell reads one regularly only
     while the window can be seen, and focus reads it at once. The first call
     covers a window that starts minimized, and brings the shell up to date
     when devices are turned back on. With devices off nothing listens. */
  $effect(() => {
    if (!devicesEnabled()) return;
    const minimized = frame.minimized;
    const visibility = () => api.setWindowVisible(!document.hidden && !minimized).catch(() => {});
    const focus = () => api.setWindowVisible(!document.hidden && !minimized, true).catch(() => {});
    visibility();
    window.addEventListener("focus", focus);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      window.removeEventListener("focus", focus);
      document.removeEventListener("visibilitychange", visibility);
    };
  });

  /* The one banner, fed by two owners that must not share a field.
     `playback.error` is the engine's: it arrives on the state payload and is
     overwritten by every event, so it clears itself when the engine recovers.
     `ui.error` is a frontend action that failed and has nowhere local to
     report — nothing but a person dismissing it will clear that one. The
     frontend message wins a tie because it is the one the user just caused.
     Dismiss clears both, so the button always empties the banner it is in. */
  const bannerError = $derived(ui.error ?? playback.error);

  function dismissBanner() {
    ui.error = null;
    playback.error = null;
  }

  /* Fetch detail data when a detail route becomes active. The fetch itself
     lives in the state module so that a failed page's "Try again" is literally
     the same call. `untrack` because loadDetail reads `detail` to decide
     whether the artist payload is already the right one, and this effect must
     depend on the ROUTE and nothing else. */
  $effect(() => {
    const name = route.name;
    const id = route.id;
    untrack(() => loadDetail(name, id));
  });

  // Global shortcuts. Ignored while typing in inputs.
  $effect(() => {
    function onKey(e) {
      const t = e.target;
      const typing =
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.tagName === "SELECT" ||
          t.isContentEditable);

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        focusSearch();
        return;
      }
      if (e.altKey && e.key === "ArrowLeft") {
        e.preventDefault();
        goBack();
        return;
      }
      if (e.altKey && e.key === "ArrowRight") {
        e.preventDefault();
        goForward();
        return;
      }
      // Hardware media keys work even while typing — that is their point.
      // (Unfocused delivery is the SMTC integration's job.)
      if (e.key === "MediaPlayPause") {
        e.preventDefault();
        togglePlay();
        return;
      }
      if (e.key === "MediaTrackNext") {
        e.preventDefault();
        api.next();
        return;
      }
      if (e.key === "MediaTrackPrevious") {
        e.preventDefault();
        api.previous();
        return;
      }
      if (typing) return;
      if ((e.ctrlKey || e.metaKey) && e.key === "ArrowRight") {
        e.preventDefault();
        api.next();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && e.key === "ArrowLeft") {
        e.preventDefault();
        api.previous();
        return;
      }
      if (spaceControlsPlayback(e)) {
        e.preventDefault();
        togglePlay();
      }
    }
    // Mouse thumb buttons: desktop users expect these to navigate.
    function onMouseUp(e) {
      if (e.button === 3) {
        e.preventDefault();
        goBack();
      } else if (e.button === 4) {
        e.preventDefault();
        goForward();
      }
    }
    window.addEventListener("keydown", onKey);
    window.addEventListener("mouseup", onMouseUp);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mouseup", onMouseUp);
    };
  });
</script>

<IconSprite />
<Ambient />

<!-- The gutters between the panes are the window's to move it by: only a
     press on the grid itself, never on a pane, counts. -->
<div
  class="app"
  class:has-panel={nowPlayingShown()}
  style:--np-w="{shownWidth}px"
  data-tauri-drag-region
>
  <Sidebar />

  <main class="pane glass-pane" bind:this={paneEl} use:frost>
    <!-- The overlay bar starts under the sticky topbar (52px, --topbar-h). -->
    <div class="scroll" bind:this={scrollEl} use:scrollbar={{ top: 52 }}>
      <TopBar />

      {#if bannerError}
        <div class="error-banner glass-card" role="alert">
          <span class="error-text">{bannerError}</span>
          <button class="btn-round" title="Dismiss" onclick={dismissBanner}>
            <Icon name="x" size={14} />
          </button>
        </div>
      {/if}

      {#if isLoggedOut()}
        <LoginView />
      {:else if route.name === "library"}
        <LibraryView />
      {:else if route.name === "made-for-you"}
        <MadeForYouView />
      {:else if route.name === "search"}
        <SearchView />
      {:else if route.name === "search-songs"}
        <SearchSongsView />
      {:else if route.name === "liked"}
        <LikedSongsView />
      {:else if route.name === "playlist"}
        <PlaylistView />
      {:else if route.name === "radio"}
        <RadioView />
      {:else if route.name === "album"}
        <AlbumView />
      {:else if route.name === "artist"}
        <ArtistView />
      {:else if route.name === "discography"}
        <DiscographyView />
      {:else if route.name === "fans-also-like"}
        <FansAlsoLikeView />
      {:else if route.name === "appears-on"}
        <AppearsOnView />
      {:else if route.name === "artist-playlists" || route.name === "discovered-on"}
        <ArtistPlaylistCollectionView />
      {:else if route.name === "show" || route.name === "episode"}
        <PodcastView />
      {:else if route.name === "profile"}
        <ProfileView />
      {:else if route.name === "queue"}
        <QueueView />
      {:else if route.name === "history"}
        <HistoryView />
      {:else if route.name === "track-editor"}
        <!-- The editor and its waveform are the heaviest page in the app and
             the least visited: loaded the first time they are opened. -->
        {#await loadTrackEditor() then { default: TrackEditorView }}
          <TrackEditorView />
        {/await}
      {:else if route.name === "settings"}
        <SettingsView />
      {/if}
    </div>
  </main>

  <NowPlayingPanel open={nowPlayingShown()} />
  {#if nowPlayingShown()}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions (a focusable separator is the window-splitter pattern) -->
    <div
      class="np-resize"
      class:dragging={dragWidth !== null}
      role="separator"
      aria-orientation="vertical"
      aria-label="Now playing width"
      aria-valuemin={Math.round(box.min)}
      aria-valuemax={Math.round(box.max)}
      aria-valuenow={Math.round(shownWidth)}
      tabindex="0"
      onpointerdown={resizePanel}
      ondblclick={() => keepPanelWidth(null)}
      onkeydown={nudgePanel}
    ></div>
  {/if}

  <PlayerBar />
</div>

{#if credits.open}
  <CreditsDialog />
{/if}

<style>
  /* A card on the pane (.glass-card, in the markup), lit with the danger
     wash rather than the card's plain light. */
  .error-banner {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    margin: 0 var(--s6);
    padding: var(--s2) var(--s3);
    border-radius: var(--r2);
    background: var(--glass-sheen), var(--danger-wash);
    color: var(--danger);
    font-size: var(--t-12);
  }
  .error-text {
    min-width: 0; /* a long engine error must ellipsis, not widen the pane */
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
