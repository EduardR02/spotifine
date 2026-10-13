<script>
  import { tick, untrack } from "svelte";
  import {
    api,
    navigate,
    navigateArtist,
    playback,
    isPlayingSource,
    togglePlay,
    library,
    promotePlaylist,
    openCredits,
    ui,
    openTrackEditor,
    isCached,
    session,
    sessionEpoch,
  } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import Menu from "./Menu.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import { personalConnected, watchPersonal, setLiked } from "../lib/personal.svelte.js";
  import { formatTime } from "../lib/time.js";
  import { spotifyLink, writeClipboard } from "../lib/spotify-link.js";
  import { observeStuck } from "../lib/sticky.js";
  import { rowWindow } from "../lib/virtual.js";
  import { scrollbar } from "../lib/scrollbar.js";
  import { vuMeter } from "../lib/vu-meter.js";
  import { pressTrack, justDragged, registerReorderZone, trackDrag } from "../lib/dnd.svelte.js";
  import { occurrenceKeys, occurrences } from "../lib/occurrence-keys.js";

  let {
    tracks = [],
    /** `tracks`' occurrence keys, when the page holds a proof of them that a
        fresh count cannot give (a local reorder carries its rows' keys). */
    trackKeys = null,
    playFrom,
    showAlbum = true,
    showArt = true,
    /** Off for short embedded lists (search), where column heads are noise. */
    showHead = true,
    playlistId = null,
    /** The page-owned live skip projection and mutation callback. */
    excludedTrackIds = null,
    onSetExcluded = null,
    /** Compact source context copied onto tracks added to the queue. */
    queueContext = null,
    /** Playlist tables may give the added date its own column. */
    showAdded = false,
    /** Official desktop surfaces expose lifetime plays on albums/Popular. */
    showPlays = false,
    /** Recommendation rows may forbid adding when the source playlist is read-only. */
    allowAddToPlaylist = true,
    /** Render every row and install no shared-pane windowing machinery. */
    disableWindowing = false,
    /** Re-measure when sibling content moves this table without changing its tracks. */
    layoutKey = null,
    sortKey = null,
    sortDirection = "asc",
    onSort = null,
    /** Optional always-visible trailing action used by compact recommendation lists. */
    rowActionLabel = null,
    onRowAction = null,
    /** Optional per-row guard for an always-visible trailing action. */
    rowActionDisabled = null,
    /** Optimistic in-place reorder for owned playlists; null disables drops into this table. */
    onReorder = null,
  } = $props();

  const queueSource = $derived(
    String(queueContext || (playlistId ? `playlist:${playlistId}` : "")).trim(),
  );

  /* =====================================================================
     PLAYLIST SKIPS

     A per-playlist listening preference stored beside the track-edit data.
     Skipped rows keep their colour, click and play glyph.
     The page owns pending intents and overlays them on its latest snapshot.
     Every row and the header read that same live projection, so a background
     refresh cannot erase an unacknowledged menu edit.
     ===================================================================== */
  const skipsTracked = $derived(!!playlistId && Array.isArray(excludedTrackIds));
  const skippedIds = $derived.by(
    () => new Set(skipsTracked ? excludedTrackIds.map(String) : []),
  );

  /** One sentence everywhere the preference is explained. */
  const SKIP_HELP =
    "Skipped when this playlist advances. You can still play it directly.";

  function rowSkipped(track) {
    return !!track?.id && skippedIds.has(String(track.id));
  }

  /* =====================================================================
     DRAG SOURCE / REORDER TARGET

     A row press becomes a drag only after 6px of travel; `pressTrack` keeps
     a plain press-and-release byte-identical to the old click behaviour.
     Two independent capabilities decide what a drag can do:

       draggable out   allowAddToPlaylist — every surface whose row menu
                       offers "Add to playlist…" honours the same guard here,
                       including the recommendation footer that turns it off.
      reorder inside  onReorder — passed only by PlaylistView, and only
                      while the table shows the playlist's own order
                      (forwards or reversed); any other sorted projection
                      cannot accept manual positions.

     While a drag from THIS table is live, the rows part around the pointer:
     neighbours translate one slot and the source row fades, so the bar sits
     visibly in the hole the row will fill. Pure transforms — no measurement
     beyond the one rect the drag loop already reads per frame.
     ===================================================================== */
  const canReorder = $derived(!!onReorder);
  const dragEnabled = $derived(allowAddToPlaylist || !!onReorder);
  /* This table's body element — the reorder zone registered below. */
  const zoneEl = $derived(disableWindowing ? flatBody : bodyEl);
  const engaged = $derived(trackDrag.active && !!zoneEl && trackDrag.listEl === zoneEl);

  /**
   * Neighbour shift for row `i`. `slot` is the FINAL landing index: rows
   * between the source and it slide one slot toward the source, which is
   * what opens the visible hole the insertion bar marks.
   */
  function rowDragTransform(i) {
    if (!engaged || !canReorder || trackDrag.slot < 0) return null;
    const from = trackDrag.sourceIndex;
    if (i === from) return null;
    if (i > from && i <= trackDrag.slot) return "translateY(calc(var(--row-h) * -1))";
    if (i < from && i >= trackDrag.slot) return "translateY(var(--row-h))";
    return null;
  }

  function rowIsSource(i) {
    return engaged && canReorder && trackDrag.sourceIndex === i;
  }

  /** Where the insertion bar renders inside this body, in px: the top edge
      of the hole at the FINAL landing slot. No-op slots show nothing. */
  const insertion = $derived.by(() => {
    if (!engaged || !canReorder || trackDrag.slot < 0) return null;
    if (trackDrag.slot === trackDrag.sourceIndex) return null;
    return trackDrag.slot * ROW_H;
  });

  $effect(() => {
    if (!zoneEl) return;
    // A table/list revision replaces gesture ownership. Window recycling does
    // not: individual row DOM can disappear while its owner remains valid.
    queueSource;
    playlistId;
    session.username;
    sortKey;
    sortDirection;
    tracks;
    trackKeys;
    return registerReorderZone(zoneEl, () => ({
      length: tracks.length,
      source: `${queueSource}\u0000${playlistId ?? ""}\u0000${sessionEpoch()}`,
      canReorder: !!onReorder,
      indexOf: occurrenceIndex,
      run: onReorder,
    }));
  });

  /** Row presses start here, delegated so windowing can recycle rows freely. */
  function onBodyPointerDown(event) {
    if (!dragEnabled) return;
    if (event.target.closest("button, a, input, textarea")) return;
    const row = event.target.closest(".tl-row");
    if (!row) return;
    const index = Number(row.dataset.i);
    if (row.dataset.rowKey !== rowKeys[index]) return;
    if (!tracks[index]?.uri?.startsWith("spotify:track:")) return;
    pressTrack({
      event,
      rowEl: row,
      ownerEl: zoneEl,
      source: `${queueSource}\u0000${playlistId ?? ""}\u0000${sessionEpoch()}`,
      occurrence: rowKeys[index],
      multiplicity: keyMultiplicity(rowKeys[index]),
      index,
      track: tracks[index],
      playlistId: playlistId ?? null,
    });
  }

  /* =====================================================================
     COLUMNS — one source of truth, in the component that renders the cells.

     This used to be nine `--cols` declarations in app.css, duplicated across
     two media-query blocks, and it was broken: inside those blocks
     `.has-inspector .tl:not(.no-album):not(.album-page)` scores 0-4-0 and
     `.has-inspector .tl.playlist-sort` only 0-3-0, so a playlist table got the
     SIX-column template while it was still rendering nine cells. Grid does
     exactly what it is told with that — cells seven, eight and nine wrap onto
     an implicit second row inside a 48px-high box, which is why the kebab
     landed on top of the next row's play icon and flickered under the pointer.
     That was reachable at any window narrower than 1276px with the now-playing
     rail open, which is most of the sizes this app is used at.

     A template that can disagree with the cells is the bug, so the two are now
     computed from the same booleans, here, and the CSS just reads `--cols`.

     THE ARTIST IS NOT A COLUMN. It used to be one on playlist tables, and it
     was costing the table a whole elastic track to repeat information that the
     title cell can carry for free on its second line — which is what every
     other list in this app, and the official client, already do. The width it
     was taking is why titles and album names in a playlist ran out of room and
     had to be dropped early. It now always renders under the title, at every
     width, so the row shape is the same everywhere instead of changing under
     you at 560px.

     Sorting by artist survives the column: the title head is two controls
     rather than one, because the cell it names genuinely holds two things.

     The DROP ORDER takes space from optional metadata before identity:

       1. Album      — the first optional column to give way
       2. Added      — when you saved it
       3. Plays      — how popular it is
       4. Artwork    — last, and only in the extreme

     Nothing ever wraps and nothing is ever squeezed to zero: every elastic
     column is a minmax(0, …) and every cell truncates.
     ===================================================================== */
  const COL = {
    idx: "28px",
    art: "36px",
    plays: "108px",
    time: "52px",
    more: "28px",
  };

  /* Pane width, in CSS px, below which each column stops earning its keep.
     Measured against the pane rather than the window — see `ui.paneWidth`.
     These are independent of the panel's minimum reading-pane width:
     album drops first, then added; title/artist and duration always stay. */
  const NEEDS = { album: 640, added: 544, plays: 540, art: 430 };
  /* Below this the 16px column gap becomes 12px: at a narrow pane the gaps are
     a bigger share of the row than any single column. */
  const DENSE_BELOW = 780;

  const pane = $derived(ui.paneWidth || 1200);
  const colArt = $derived(showArt && pane >= NEEDS.art);
  const colAlbum = $derived(showAlbum && pane >= NEEDS.album);
  const colAdded = $derived(showAdded && pane >= NEEDS.added);
  const colPlays = $derived(showPlays && pane >= NEEDS.plays);
  const dense = $derived(pane < DENSE_BELOW);

  const cols = $derived.by(() => {
    const list = [COL.idx];
    if (colArt) list.push(COL.art);
    /* The title carries the row. It gets the largest share when it shares the
       elastic space, and all of it when it does not. */
    const elastic = (colAlbum ? 1 : 0) + (colAdded ? 1 : 0);
    list.push(elastic ? "minmax(0, 2.2fr)" : "minmax(0, 1fr)");
    if (colAlbum) list.push("minmax(0, 1.6fr)");
    /* A date has a natural minimum — "12 Aug 2026" — and truncating it to
       "12 Au…" tells you nothing, so this column has a floor rather than a
       share it can be squeezed out of. */
    if (colAdded) list.push("minmax(84px, 0.8fr)");
    if (colPlays) list.push(COL.plays);
    list.push(COL.time, rowActionLabel ? "56px" : COL.more);
    return list.join(" ");
  });

  const addedDateFormatter = new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
  const playCountFormatter = new Intl.NumberFormat();

  function formatAddedAt(value) {
    const timestamp = Number(value);
    if (!Number.isFinite(timestamp) || timestamp <= 0) return "—";
    const date = new Date(timestamp);
    if (!Number.isFinite(date.getTime())) return "—";
    return addedDateFormatter.format(date);
  }

  function formatPlayCount(value) {
    const count = Number(value);
    return Number.isFinite(count) && count > 0 ? playCountFormatter.format(count) : "";
  }

  function activateSort(key) {
    onSort?.(key);
  }

  function sortAriaLabel(key, label) {
    if (sortKey !== key) return `Sort by ${label}`;
    return `Sort by ${label}, currently ${sortDirection === "asc" ? "ascending" : "descending"}`;
  }


  /**
   * Closes every menu surface. The kebab gets its focus back only when the
   * menu was holding it — the copy confirmation's timer fires while the pointer
   * may be anywhere by then, and a close behind the user's back must not pull
   * the caret out of whatever they moved to.
   */
  function closeMenus(returnFocus = false) {
    const menuHadFocus = !!menuEl?.contains(document.activeElement);
    menu.generation += 1;
    menu.open = false;
    picker.open = false;
    artistPicker.open = false;
    if (returnFocus && menuHadFocus) {
      /* The row the menu hung off may have been recycled by the windowing while
         the confirmation was on screen; a detached kebab cannot take focus. */
      const anchor = menu.anchor;
      queueMicrotask(() => {
        if (anchor?.isConnected) anchor.focus();
      });
    }
  }

  /* One shared menu instance for the whole table rather than one per row.
     `maxH` is part of the position, not decoration: see openRowMenu. */
  const menu = $state({
    open: false, x: 0, top: null, bottom: null, maxH: 0,
    track: null, index: -1, copyState: "idle", generation: 0, epoch: -1,
    editDefined: false, editEnabled: false, editLoading: false, editError: "",
    skipPending: false, skipError: "", anchor: null, liked: null, likeBusy: false,
  });
  /* The row the menu was opened on, as the row object itself. `menu.track` is
     that row seen through the menu's proxy, which suits the markup but is
     never `===` a plain row of `tracks` (the long lists are `$state.raw`), so
     the identity checks behind every async answer compare against this. */
  let menuRow = null;
  let menuEl = $state(null);
  const picker = $state({ open: false, x: 0, top: null, bottom: null, maxH: 0, track: null, trigger: null });
  /* Same shape and same placement rules as the playlist picker — a second

     surface hanging off one row of the menu, rather than N rows inside it. */
  const artistPicker = $state({ open: false, x: 0, top: null, bottom: null, maxH: 0, artists: [], trigger: null });
  let menuTracks;
  let menuPlaylistId;
  let menuSession;
  $effect(() => {
    const nextTracks = tracks;
    const nextPlaylistId = playlistId;
    const nextSession = session.username;
    if (nextTracks === menuTracks && nextPlaylistId === menuPlaylistId && nextSession === menuSession) return;
    menuTracks = nextTracks;
    menuPlaylistId = nextPlaylistId;
    menuSession = nextSession;
    if (menu.open || picker.open || artistPicker.open) untrack(closeMenus);
  });

  /** Highlight and toggle only the active source. Its displayed order may be
      sorted or windowed, so resolve the row by URI rather than queue index. */
  const currentRow = $derived.by(() => {
    const uri = playback.current_uri;
    if (!uri || !isPlayingSource(queueSource)) return -1;
    // When this list IS the playing context the index still agrees, and taking
    // it keeps duplicates resolving to the instance actually playing.
    const i = playback.current_index;
    if (i >= 0 && i < tracks.length && tracks[i]?.uri === uri) return i;
    return tracks.findIndex((t) => t.uri === uri);
  });

  function onRowDblClick(i) {
    if (justDragged()) return; // the release click of a drag is not play intent
    onPlayIconClick(i);
  }

  function onPlayIconClick(i) {
    if (tracks[i]?.unavailable) return;
    if (i === currentRow) togglePlay();
    else playFrom(i);
  }

  /**
   * Roughly how tall the menu can get: seven items at 30px, separator, and
   * the 4px padding. An estimate is enough — it only decides which SIDE of the
   * button the menu opens on, and being a few pixels out changes nothing.
   * Measuring the real height would mean rendering it offscreen first, which
   * is a layout read and a frame of flicker for no gain.
   */
  const MENU_MAX_H = 232;
  /* No menu is ever laid out shorter than this. Below about four rows a menu
     stops being a list and becomes a peephole, and scrolling one item at a
     time is worse than covering a little of the row you opened it from. */
  const MENU_MIN_H = 132;
  /* The menu's width, as `.menu.track-menu` sets it; a submenu hangs its own
     220px plus a gap to the menu's left. */
  const MENU_W = 264;
  const SUBMENU_OFFSET = 228;

  /**
   * The band a fixed-position popover may occupy, in WINDOW coordinates.
   *
   * The top of that band is not the top of the window: the topbar is sticky at
   * `z-index: 30` and carries a `backdrop-filter`, so anything drawn under it
   * is both blurred and unclickable. The old clamp was `Math.max(8, …)`, i.e.
   * 8px from the WINDOW — and the bar ends 60px down (an 8px app gutter plus
   * its own 52px), so that clamp parked the menu 52px inside the glass. That is
   * exactly the reported bug: open the menu on the first row of a playlist
   * scrolled to the top, it flips upward, hits the clamp, and its first two
   * items are drawn behind the bar where nothing can click them.
   *
   * Measured rather than derived from `--topbar-h`, because the bar's offset
   * from the window depends on the shell's gutter and padding as well as its
   * own height, and a rect is the honest answer to "where does it actually
   * end". One layout read, in a click handler — never in a scroll handler.
   */
  function popoverBand() {
    const bar = document.querySelector(".topbar");
    const top = (bar ? bar.getBoundingClientRect().bottom : 0) + 8;
    return { top, bottom: window.innerHeight - 8 };
  }

  /**
   * Places a popover against an anchor rect, inside the band.
   *
   * Returns the edge it is PINNED BY rather than a `y`, and that is the whole
   * trick. Opening downward pins the top to the anchor's bottom; opening
   * upward pins the BOTTOM to the anchor's top, so the menu grows away from
   * the row it belongs to and always touches it, whatever its content height
   * turns out to be. Computing a `y` for the upward case instead means
   * subtracting a GUESSED height, which is what put the old menu 8px from the
   * top of the window — under the topbar — whenever the guess was too big.
   *
   * `maxH` is then simply the room on the chosen side. It is a ceiling, not a
   * size: a menu that fits renders at its natural height, and one that does
   * not scrolls inside the space it has, so no item is ever unreachable at any
   * window height.
   *
   * `desiredH` only decides which side to prefer.
   */
  function placePopover(anchor, desiredH) {
    const band = popoverBand();
    const roomBelow = band.bottom - (anchor.bottom + 4);
    const roomAbove = anchor.top - 4 - band.top;
    if (roomBelow >= desiredH || roomBelow >= roomAbove) {
      /* The clamp matters even opening DOWNWARD: a row can be partly under the
         sticky bar (keyboard focus, a programmatic scroll), and its anchor rect
         is then above the band's top edge. */
      return {
        top: Math.max(band.top, anchor.bottom + 4),
        bottom: null,
        maxH: Math.max(MENU_MIN_H, roomBelow),
      };
    }
    return {
      top: null,
      bottom: window.innerHeight - (anchor.top - 4),
      maxH: Math.max(MENU_MIN_H, roomAbove),
    };
  }

  function menuTargetCurrent(generation, sourcePlaylist, track, index) {
    return (
      menu.open &&
      menu.generation === generation &&
      menu.epoch === sessionEpoch() &&
      playlistId === sourcePlaylist &&
      menuRow === track &&
      menu.index === index &&
      tracks[index] === track
    );
  }

  function openRowMenu(e, track, i) {
    e.stopPropagation();
    if (menu.open && menu.index === i) {
      closeMenus();
      return;
    }
    const generation = ++menu.generation;
    const sourcePlaylist = playlistId;
    const r = e.currentTarget.getBoundingClientRect();
    const placed = placePopover(r, MENU_MAX_H);
    /* A copy still counting down must not close the menu that replaces it. */
    clearTimeout(copyResetTimer);
    menu.open = true;
    menu.anchor = e.currentTarget;
    menu.x = Math.max(8, Math.min(r.right - MENU_W, window.innerWidth - MENU_W - 8));
    menu.top = placed.top;
    menu.bottom = placed.bottom;
    menu.maxH = placed.maxH;
    menu.track = track;
    menu.epoch = sessionEpoch();
    menuRow = track;
    menu.index = i;
    menu.copyState = "idle";
    menu.skipPending = false;
    menu.skipError = "";
    menu.editError = "";
    menu.liked = null;
    menu.likeBusy = false;
    picker.open = false;
    artistPicker.open = false;
    menu.editDefined = false;
    menu.editEnabled = false;
    /* Liked Songs is answered by the shell's membership index — in memory,
       one IPC round trip, no network — so the item is drawn in its final
       place at once and its heart fills a frame later at most. */
    if (personalConnected() && track?.uri?.startsWith("spotify:track:")) {
      api.getTrackPlaylists(track.uri)
        .then((refs) => {
          if (menuTargetCurrent(generation, sourcePlaylist, track, i)) {
            menu.liked = (refs ?? []).some((ref) => ref.id === "liked");
          }
        })
        .catch(() => {});
    }
    menu.editLoading = !!sourcePlaylist && track?.uri?.startsWith("spotify:track:");
    if (menu.editLoading && track?.id) {
      api.getTrackEdit(track.id, sourcePlaylist)
        .then((status) => {
          if (!menuTargetCurrent(generation, sourcePlaylist, track, i)) return;
          menu.editDefined = !!status?.definition;
          menu.editEnabled = !!status?.enabled;
        })
        .catch((reason) => {
          if (!menuTargetCurrent(generation, sourcePlaylist, track, i)) return;
          menu.editError = String(reason || "Could not load edit status.");
        })
        .finally(() => {
          if (menuTargetCurrent(generation, sourcePlaylist, track, i)) {
            menu.editLoading = false;
          }
        });
    }
  }

  /** Likes or unlikes the menu's track; the menu stays open on the answer,
      so the heart it just changed is the confirmation. */
  async function toggleLiked() {
    const track = menu.track;
    const liked = menu.liked;
    if (!track?.uri || liked === null || menu.likeBusy) return;
    const generation = menu.generation;
    menu.likeBusy = true;
    try {
      await setLiked([track.uri], !liked);
      if (menu.open && menu.generation === generation) menu.liked = !liked;
    } catch (reason) {
      ui.error = `Could not ${liked ? "remove from" : "save to"} Liked Songs. ${reason instanceof Error ? reason.message : String(reason ?? "")}`.trim();
    } finally {
      if (menu.generation === generation) menu.likeBusy = false;
    }
  }

  async function toggleEditedVersion() {
    if (!playlistId || !menu.track?.id || !menu.editDefined || menu.editLoading) return;
    const generation = menu.generation;
    const sourcePlaylist = playlistId;
    const track = menuRow;
    const index = menu.index;
    const enabled = !menu.editEnabled;
    menu.editLoading = true;
    menu.editError = "";
    try {
      await api.setPlaylistTrackEditEnabled(sourcePlaylist, track.id, enabled);
      if (menuTargetCurrent(generation, sourcePlaylist, track, index)) {
        menu.editEnabled = enabled;
      }
    } catch (reason) {
      if (menuTargetCurrent(generation, sourcePlaylist, track, index)) {
        menu.editError = String(reason || "Could not update this playlist.");
      }
    } finally {
      if (menuTargetCurrent(generation, sourcePlaylist, track, index)) {
        menu.editLoading = false;
      }
    }
  }

  /** Mutation ownership lives in PlaylistView, not a captured exclusions array. */
  async function toggleRowSkip() {
    const track = menuRow;
    if (!skipsTracked || !onSetExcluded || !track?.id || !playlistId || menu.skipPending) return;
    const id = String(track.id);
    const generation = menu.generation;
    const sourcePlaylist = playlistId;
    const index = menu.index;
    const exclude = !skippedIds.has(id);
    menu.skipPending = true;
    menu.skipError = "";
    try {
      await onSetExcluded(id, exclude);
    } catch (reason) {
      if (menuTargetCurrent(generation, sourcePlaylist, track, index)) {
        menu.skipError = String(reason || "Could not update this playlist.");
      }
      return;
    } finally {
      if (menuTargetCurrent(generation, sourcePlaylist, track, index)) menu.skipPending = false;
    }
    if (menuTargetCurrent(generation, sourcePlaylist, track, index)) closeMenus();
  }

  function openPicker(e) {
    if (!menu.track) return;
    /* The picker is as tall as the library, so it is placed against its own
       trigger and given the same band as the menu rather than inheriting a
       position that was computed for a much shorter list. */
    const placed = placePopover(e.currentTarget.getBoundingClientRect(), 60 * 6);
    picker.open = true;
    picker.trigger = e.currentTarget;
    picker.track = menu.track;
    picker.x = Math.max(8, menu.x - SUBMENU_OFFSET);
    picker.top = placed.top;
    picker.bottom = placed.bottom;
    picker.maxH = placed.maxH;
  }

  /** One artist: go. Several: hang a submenu off this row. */
  function goToArtist(e) {
    if (menuArtists.length === 1) {
      menu.open = false;
      navigateArtist(menuArtists[0].id, menuArtists[0].name);
      return;
    }
    const placed = placePopover(e.currentTarget.getBoundingClientRect(), 40 * menuArtists.length);
    artistPicker.open = true;
    artistPicker.trigger = e.currentTarget;
    artistPicker.artists = menuArtists;
    artistPicker.x = Math.max(8, menu.x - SUBMENU_OFFSET);
    artistPicker.top = placed.top;
    artistPicker.bottom = placed.bottom;
    artistPicker.maxH = placed.maxH;
  }

  /* ---------------- Copy link ----------------
     The URL is built from the id already on the row — `id`, or the trailing
     segment of `spotify:track:<id>` when only the URI is present — and never
     from anything else, so a row with neither draws no item. Both the string
     and the clipboard write are the shared ones every share surface uses; see
     lib/spotify-link.js. */
  const menuLink = $derived(spotifyLink(menu.track?.uri?.startsWith("spotify:episode:") ? "episode" : "track", menu.track?.id || menu.track?.uri));

  let copyResetTimer = 0;
  /* The confirmation outlives the menu by design, so the timer has to die with
     the list: a navigation inside its window would otherwise leave it writing
     to a component that is gone. */
  $effect(() => () => {
    menu.generation += 1;
    clearTimeout(copyResetTimer);
  });
  /* The menu STAYS OPEN on copy, because the confirmation is on the item
     itself and dismissing it would take the only feedback with it. A copy that
     landed closes it shortly after, through the close every other dismissal
     uses, which also drops any status the open menu had asked for. A refused
     write stays up and says so: the label that never changes is the dead
     control the confirmation exists to prevent, and the item is also the
     retry. */
  async function copyTrackLink() {
    const link = menuLink;
    if (!link) return;
    const generation = menu.generation;
    const ok = await writeClipboard(link);
    if (!menu.open || menu.generation !== generation) return;
    menu.copyState = ok ? "copied" : "failed";
    clearTimeout(copyResetTimer);
    if (ok) copyResetTimer = setTimeout(() => {
      if (menu.generation === generation) closeMenus(true);
    }, 900);
  }

  /**
   * Which artists this row's menu can send you to.
   *
   * "Go to artist" read `track.artist_id`, the PRIMARY artist, so a collab
   * silently offered exactly one of its artists and no way to reach the rest.
   * The row already carries the full parallel `artist_ids`/`artist_names`
   * pair — it is what the artist line under the title links with — so the
   * fix is to read the list that is already there.
   *
   * Naming every artist as its own row was the first attempt and it was wrong:
   * a three-way collaboration pushed three near-identical lines into a menu
   * that already has seven, and the artists are reachable from the line under
   * the title anyway. So the menu keeps ONE row whatever the track. With a
   * single artist it navigates; with several it opens the same kind of submenu
   * the playlist picker uses, which costs one row instead of N.
   */
  const menuArtists = $derived.by(() => {
    const track = menu.track;
    if (!track) return [];
    const names = track.artist_names ?? [];
    const ids = track.artist_ids ?? [];
    const linkable = names
      .map((name, i) => ({ name, id: ids[i] ?? "" }))
      .filter((artist) => artist.id && artist.name);
    if (linkable.length) return linkable;
    return track.artist_id ? [{ name: names[0] ?? "artist", id: track.artist_id }] : [];
  });

  async function addToPlaylist(playlist) {
    if (!picker.track) return;
    const uri = picker.track.uri;
    /* Close both menus before waiting on IPC so the picker never holds the
       pointer hostage while the add is in flight. */
    picker.open = false;
    artistPicker.open = false;
    menu.open = false;
    try {
      await api.addPlaylistTracks(playlist.id, [uri]);
      // An add is library activity, not playback. Failed adds never reach
      // either promotion or the activity command.
      promotePlaylist(playlist.id);
      api.touchPlaylistActivity(playlist.id).catch(() => {});
    } catch {
      // A failed add must update neither timestamp.
    }
  }

  /* ---------------- Windowed rendering ----------------
     Long page-level lists keep a fixed-height body and occurrence-keyed rows
     positioned at their absolute indexes. Overlapping rows retain their Cover subtree
     when the window slides. A focused row stays mounted outside the window,
     so a scroll never discards the keyboard's anchor. The shared pane owns
     the scroll offset, not this component. */
  const ROW_H = 48; // must track --row-h
  /* Sliding the window one row at a time measured better than batching it into
     blocks of 8 (92 vs 86 fps, worst frame 18ms vs 27ms): the batched version
     does the same total work in rarer, bigger bursts, and it is the burst that
     misses the frame. Keep the updates small and frequent. */
  const OVERSCAN = 6;

  let rootEl = $state(null);
  let bodyEl = $state(null);
  let flatBody = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(0);
  /* Plain mirrors of the two above: measure() must not *read* reactive state,
     or the wiring effect below would re-subscribe on every scroll frame. */
  let curFirst = 0;
  let curLast = 0;

  /* A list from another source needs a new window, even at the same length: a
     source change resets the retained absolute-index range before the new
     rows are patched. The lists themselves are immutable values ($state.raw),
     so the same source handing over a new array — a sort, a reorder, a
     refresh, another page — is an edit of the list on screen: it keeps its
     window, and its keyed rows update in place instead of being rebuilt. */
  let seenSource = null;
  let seenPlaylistId = null;
  let seenLength = -1;

  // Keys are computed on list changes, never on scroll (occurrence-keys.js).
  const rowKeys = $derived(trackKeys ?? occurrenceKeys(tracks));
  const rowIdentity = $derived(occurrences(rowKeys));
  function keyMultiplicity(key) {
    return rowIdentity.multiplicity(key);
  }
  function occurrenceIndex(key, multiplicity) {
    if (multiplicity !== undefined && keyMultiplicity(key) !== multiplicity) return -1;
    return rowIdentity.indexOf(key);
  }
  let focusedKey = $state(null);
  let focusedMultiplicity = $state(0);
  let focusedTrack = null;
  // Exact duplicates on a fresh snapshot have no provable correspondence.
  // Known local MOVs keep both this row object and its carried occurrence key.
  const focusedRow = $derived(focusedKey === null ? -1 : occurrenceIndex(focusedKey, focusedMultiplicity));
  const renderedIndices = $derived.by(() => {
    const indices = [];
    if (focusedRow >= 0 && focusedRow < firstRow && focusedRow < tracks.length) indices.push(focusedRow);
    for (let i = firstRow; i < lastRow; i++) indices.push(i);
    if (focusedRow >= lastRow && focusedRow < tracks.length) indices.push(focusedRow);
    if (menu.open && tracks[menu.index] === menuRow && !indices.includes(menu.index)) {
      indices.push(menu.index);
    }
    return indices;
  });
  const visible = $derived(disableWindowing ? tracks : renderedIndices.map((i) => tracks[i]));

  function retainRowFocus(event) {
    const row = event.target.closest(".tl-row");
    if (row) {
      focusedKey = row.dataset.rowKey;
      focusedMultiplicity = keyMultiplicity(focusedKey);
      focusedTrack = tracks[Number(row.dataset.i)];
    }
  }

  let focusOwner = null;
  $effect.pre(() => {
    const owner = `${queueSource}\u0000${playlistId ?? ""}\u0000${session.username ?? ""}`;
    if (owner === focusOwner) return;
    focusOwner = owner;
    untrack(() => {
      if (zoneEl?.contains(document.activeElement)) document.activeElement.blur();
      focusedKey = null;
    });
  });
  $effect.pre(() => {
    rowKeys;
    const owner = focusOwner;
    const body = zoneEl;
    const active = untrack(() => document.activeElement);
    if (!body?.contains(active)) return;
    if (untrack(() => focusedKey !== null && (
      focusedRow < 0 || (focusedMultiplicity > 1 && tracks[focusedRow] !== focusedTrack)
    ))) {
      active.blur();
      focusedKey = null;
      return;
    }
    // Moving an existing keyed node can temporarily drop browser focus.
    tick().then(() => {
      if (owner === focusOwner && active.isConnected && document.activeElement === document.body) {
        active.focus({ preventScroll: true });
      }
    });
  });
  function releaseRowFocus(event) {
    if (event.relatedTarget && !bodyEl?.contains(event.relatedTarget)) focusedKey = null;
  }

  $effect(() => watchPersonal());


  function measure(scroller) {
    if (!bodyEl || !scroller) return;
    /* The window arithmetic is shared with the history list; the clamping it
       does also keeps this window valid when the shared scroller continues
       below the table because another section follows it. */
    const { first: f, last: l } = rowWindow(bodyEl, scroller, ROW_H, OVERSCAN, tracks.length);
    if (f === curFirst && l === curLast) return;
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  function resetWindow(length, scroller) {
    const initialLast = scroller
      ? Math.min(length, Math.ceil(scroller.clientHeight / ROW_H) + OVERSCAN)
      : length;
    curFirst = 0;
    curLast = initialLast;
    firstRow = 0;
    lastRow = initialLast;
  }

  function clampWindow(length) {
    const maxFirst = Math.max(0, length - 1);
    const f = Math.min(curFirst, maxFirst);
    const l = Math.min(Math.max(curLast, f), length);
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  /*
   * Reset the retained row range before a new list is patched. The shared pane
   * owns its scroll position in App.svelte; a child table must not erase a
   * route restoration when async playlist rows arrive. The post-effect below
   * measures the live offset immediately and selects the matching row window.
   * Length-only changes preserve and clamp the current window.
   */
  $effect.pre(() => {
    if (disableWindowing) return;
    const list = tracks;
    const length = list.length;
    const body = bodyEl;
    if (!body) return;

    const sourceChanged = queueSource !== seenSource || playlistId !== seenPlaylistId;
    const lengthChanged = length !== seenLength;
    if (!sourceChanged && !lengthChanged) return;

    seenSource = queueSource;
    seenPlaylistId = playlistId;
    seenLength = length;
    const scroller = body.closest(".scroll");
    if (sourceChanged) resetWindow(length, scroller);
    else clampWindow(length);
  });

  // Re-measure when the list changes without rebuilding the scroll listener
  // and ResizeObserver on every playlist reorder, sort or track edit.
  $effect(() => {
    if (disableWindowing) return;
    const list = tracks;
    list.length;
    playlistId;
    layoutKey;
    if (!bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) {
      // No scroll ancestor (embedded use): render everything, as before.
      curFirst = 0;
      curLast = list.length;
      firstRow = 0;
      lastRow = list.length;
      return;
    }
    untrack(() => measure(scroller));
  });

  $effect(() => {
    if (disableWindowing || !bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) return;
    let scrollFrame = 0;
    const onScroll = () => {
      if (scrollFrame) return;
      scrollFrame = requestAnimationFrame(() => {
        scrollFrame = 0;
        measure(scroller);
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(() => measure(scroller));
    ro.observe(scroller);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      ro.disconnect();
      if (scrollFrame) cancelAnimationFrame(scrollFrame);
    };
  });

  /* The column heads are type on the page until rows start passing under
     them; see observeStuck for why this is an observer and not a scroll
     timeline or a scroll handler. */
  let headSentinel = $state(null);
  let headStuck = $state(false);
  $effect(() => observeStuck(headSentinel, (stuck) => (headStuck = stuck)));

</script>

{#snippet trackRows(items, start, indices = null)}
  {#each items as track, k (rowKeys[indices ? indices[k] : start + k])}
    {@const i = indices ? indices[k] : start + k}
    <div
      class="tl-row"
      class:current={i === currentRow}
      class:unavailable={track.unavailable}
      class:skipped={rowSkipped(track)}
      class:drag-source={rowIsSource(i)}
      data-i={i}
      data-row-key={rowKeys[i]}
      style:transform={rowDragTransform(i)}
      style:position={indices ? "absolute" : null}
      style:top={indices ? `${i * ROW_H}px` : null}
      style:left={indices ? "0" : null}
      style:right={indices ? "0" : null}
      role="button"
      aria-disabled={track.unavailable ? "true" : undefined}
      title={track.unavailable ? (track.unavailable_reason || "Unavailable") : undefined}
      tabindex="-1"
      ondblclick={() => onRowDblClick(i)}
    >
      <span class="c-idx">
        <span class="n">{i + 1}</span>
        {#if i === currentRow}
          <canvas class="eq" use:vuMeter></canvas>
        {/if}
        <button
          class="go"
          title={track.unavailable ? (track.unavailable_reason || "Unavailable") : "Play"}
          disabled={track.unavailable}
          onclick={() => onPlayIconClick(i)}
        >
          <Icon name={i === currentRow && playback.playing ? "pause" : "play"} size={12} />
        </button>
      </span>

      {#if colArt}
        <Cover
          src={track.cover_url}
          id={track.album_id || track.uri}
          name={track.album_name || track.name}
          size={36}
          class="c-art"
        />
      {/if}

      <!-- Two lines, always: the name, then who made it. The artist is not a
           column any more (see the block at the top of this file). -->
      <span class="c-title">
        <span class="t-name">{track.name}</span>
        <span class="t-sub">
          {#if isCached(track)}
            <!-- Status, not action. It sits before the names because that is
                 where the official client puts it and because it qualifies the
                 whole line rather than any one artist. -->
            <!-- 13px, not 10. At 10 the punched arrow inside the disc was too
                 small to resolve as an arrow at all — it read as a dot, which
                 is a mark that says "something" rather than "downloaded". -->
            <span class="t-cached" title="Downloaded — this plays from the local cache">
              <Icon name="cached" size={13} />
              <span class="sr-only">Downloaded</span>
            </span>
          {/if}
          {#if rowSkipped(track)}
            <!-- Same slot and manners as the cached mark, but neutral: this
                 is not a fact about the file, it is a preference about
                 ordering, so it stays grey and says so in words for readers
                 who cannot see the ring. The tooltip repeats the menu's one
                 sentence rather than inventing a second phrasing. -->
            <span class="t-skip" title={SKIP_HELP}>
              <Icon name="skipped" size={13} />
              <span class="sr-only">Skipped during playlist playback</span>
            </span>
          {/if}
          {#if track.uri?.startsWith("spotify:episode:")}
            <button class="t-artists" disabled={!track.album_id} onclick={() => navigate("show", track.album_id)}>{track.album_name || track.artist_names?.[0]}</button>
          {:else}
            <ArtistLinks
              class="t-artists"
              names={track.artist_names}
              ids={track.artist_ids ?? []}
              id={track.artist_id}
            />
          {/if}
        </span>
      </span>

      {#if colAlbum}
        <!-- Always rendered, even when it repeats the title (single-track
             releases). Blanking those cells leaves holes that read as data
             that failed to load, which is worse than mild redundancy. -->
        <button class="c-album" onclick={() => track.album_id && navigate(track.uri?.startsWith("spotify:episode:") ? "show" : "album", track.album_id)}>
          {track.album_name}
        </button>
      {/if}

      {#if colAdded}
        <span class="c-added">{formatAddedAt(track.added_at)}</span>
      {/if}

      {#if colPlays}
        <span class="c-plays">{formatPlayCount(track.play_count)}</span>
      {/if}


      <span class="c-time">{formatTime(track.duration_ms)}</span>

      <span class="c-more">
        {#if rowActionLabel && onRowAction}
          <button
            style="width:56px;opacity:1"
            title={rowActionDisabled?.(track, i) ? "Adding…" : rowActionLabel}
            disabled={!!rowActionDisabled?.(track, i)}
            onclick={() => onRowAction(track, i)}
          >{rowActionDisabled?.(track, i) ? "Adding…" : rowActionLabel}</button>
        {:else}
          <button title="More options" onclick={(e) => openRowMenu(e, track, i)}>
            <Icon name="more" size={15} />
          </button>
        {/if}
      </span>
    </div>
  {/each}
{/snippet}

<div
  class="tl"
  class:dense
  class:reordering={engaged}
  bind:this={rootEl}
  style="overflow-anchor: none"
  style:--cols={cols}
>
  {#if showHead}
    <!-- Parked where the head starts sticking; see the observer above. -->
    <div class="tl-head-sentinel" bind:this={headSentinel} aria-hidden="true"></div>
    <div class="tl-head" class:stuck={headStuck}>
      <button
        class="tl-sort-btn"
        class:active={sortKey === "order"}
        aria-label={sortAriaLabel("order", "original/custom order")}
        aria-pressed={sortKey === "order"}
        onclick={() => activateSort("order")}
      ># {#if sortKey === "order"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {#if colArt}<span></span>{/if}
      <!-- ONE grid cell holding TWO controls, because the cell it names holds
           two things now. Splitting the head this way is what keeps sorting by
           artist available after the artist column was folded under the title,
           without adding a separate sort control to every page that has a
           table. Still exactly one cell — the `--cols` invariant above depends
           on the head and the row agreeing on their count. -->
      <span class="tl-title-head">
        <button
          class="tl-sort-btn"
          class:active={sortKey === "title"}
          aria-label={sortAriaLabel("title", "title")}
          aria-pressed={sortKey === "title"}
          onclick={() => activateSort("title")}
        >Title {#if sortKey === "title"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
        <span class="tl-head-sep" aria-hidden="true"></span>
        <button
          class="tl-sort-btn tl-artist-sort"
          class:active={sortKey === "artist"}
          aria-label={sortAriaLabel("artist", "artist")}
          aria-pressed={sortKey === "artist"}
          onclick={() => activateSort("artist")}
        >Artist {#if sortKey === "artist"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      </span>
      {#if colAlbum}
        <button
          class="tl-sort-btn"
          class:active={sortKey === "album"}
          aria-label={sortAriaLabel("album", "album")}
          aria-pressed={sortKey === "album"}
          onclick={() => activateSort("album")}
        >Album {#if sortKey === "album"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {/if}
      {#if colAdded}
        <button
          class="tl-sort-btn"
          class:active={sortKey === "added"}
          aria-label={sortAriaLabel("added", "date added")}
          aria-pressed={sortKey === "added"}
          onclick={() => activateSort("added")}
        >Added {#if sortKey === "added"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {/if}
      {#if colPlays}<span class="tl-plays-head">Plays</span>{/if}
      <button
        class="tl-sort-btn tl-sort-duration"
        class:active={sortKey === "duration"}
        aria-label={sortAriaLabel("duration", "duration")}
        aria-pressed={sortKey === "duration"}
        onclick={() => activateSort("duration")}
      ><span class="tl-sort-duration-label">Duration</span><Icon name="clock" size={13} />{#if sortKey === "duration"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      <span></span>
    </div>
  {/if}

  {#if disableWindowing}
    <div
      bind:this={flatBody}
      role="presentation"
      style="overflow-anchor: none; position: relative"
      onpointerdown={onBodyPointerDown}
    >
      {@render trackRows(visible, 0)}
      {#if insertion !== null}<div class="tl-insert" style:top="{insertion}px"></div>{/if}
    </div>
  {:else}
    <div
      bind:this={bodyEl}
      role="presentation"
      style="overflow-anchor: none; position: relative"
      style:height="{tracks.length * ROW_H}px"
      onpointerdown={onBodyPointerDown}
      onfocusin={retainRowFocus}
      onfocusout={releaseRowFocus}
    >
      <div
        style="position: absolute; inset: 0 0 auto"
      >
        {@render trackRows(visible, 0, renderedIndices)}
      </div>
      {#if insertion !== null}<div class="tl-insert" style:top="{insertion}px"></div>{/if}
    </div>
  {/if}
</div>

{#if menu.open}
  <!-- The popover is the glass and its child is the scroller, so that the
       scroller's overlay bar is laid inside the popover, in the top layer. -->
  <Menu class="track-menu" placement={menu} anchor={menu.anchor} group bind:element={menuEl} label="Track actions" onclose={closeMenus}>
    <div class="menu-scroll" use:scrollbar style:max-height="{menu.maxH}px">
    <button class="menu-item" onclick={() => { menu.open = false; api.addQueue(menu.track, queueSource).catch(() => {}); }}>
      <Icon name="queue-add" size={16} /><span class="menu-label">Add to queue</span>
    </button>
    {#if allowAddToPlaylist && menu.track?.uri?.startsWith("spotify:track:")}
      <button class="menu-item" aria-haspopup="menu" aria-expanded={picker.open} onclick={openPicker}>
        <Icon name="playlist-add" size={16} /><span class="menu-label">Add to playlist</span>
        <span class="chev"><Icon name="forward" size={14} /></span>
      </button>
    {/if}
    {#if personalConnected() && menu.track?.uri?.startsWith("spotify:track:")}
      <!-- The label says what the click will do, and the heart what is true
           now. The state is the shell's membership index, answered in one IPC
           round trip; until it has answered the item is drawn in place and
           inert (busy, never disabled, so a focused item keeps its focus). -->
      <button
        class="menu-item"
        class:liked={menu.liked === true}
        role="menuitemcheckbox"
        aria-checked={menu.liked === true}
        aria-busy={menu.liked === null || menu.likeBusy}
        onclick={toggleLiked}
      >
        <Icon name={menu.liked ? "heart-f" : "heart"} size={16} />
        <span class="menu-label">{menu.liked ? "Remove from Liked Songs" : "Add to Liked Songs"}</span>
      </button>
    {/if}
    {#if menuLink}
      <!-- The confirmation lives on the item, which is why the menu does not
           close on click: a copy with no feedback is indistinguishable from a
           dead control, and a toast system for one line of text is not worth
           having. The glyph becomes a foam check — this is a thing you just
           did. -->
      <button
        class="menu-item"
        class:done={menu.copyState === "copied"}
        class:failed={menu.copyState === "failed"}
        onclick={copyTrackLink}
      >
        <Icon name={menu.copyState === "copied" ? "check" : "copy"} size={16} />
        <span class="menu-label">{menu.copyState === "copied" ? "Link copied" : menu.copyState === "failed" ? "Copy failed" : "Copy link"}</span>
      </button>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:track:")}
      <button class="menu-item" onclick={() => { menu.open = false; openCredits(menu.track); }}>
        <Icon name="credits" size={16} /><span class="menu-label">View credits</span>
      </button>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:track:")}
      <button
        class="menu-item"
        onclick={() => {
          menu.open = false;
          openTrackEditor(menu.track, playlistId);
        }}
      >
        <Icon name="scissors" size={16} /><span class="menu-label">Edit playback…</span>
      </button>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:track:") && playlistId && menu.editDefined}
      <button
        class="menu-item"
        role="menuitemcheckbox"
        aria-checked={menu.editEnabled}
        disabled={menu.editLoading}
        onclick={toggleEditedVersion}
      >
        <Icon name="waveform" size={16} /><span class="menu-label">Use edited version</span>
        {#if menu.editEnabled}<span class="mark edit-check"><Icon name="check" size={14} /></span>{/if}
      </button>
    {/if}
    {#if menu.editError}
      <p class="menu-error" role="alert">{menu.editError}</p>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:track:") && skipsTracked && menu.track?.id}
      <!-- The one playlist-local listening preference. It sits with the edit
           cluster because both are "how this row behaves inside THIS list".
           Unlike Copy link, the row itself acknowledges the click — the skip
           mark moves — so a confirmed toggle closes the menu, and only a
           refused command keeps it up, long enough to say why. -->
      <button
        class="menu-item"
        disabled={menu.skipPending}
        title={SKIP_HELP}
        onclick={toggleRowSkip}
      >
        <Icon name={rowSkipped(menu.track) ? "restore" : "skipped"} size={16} />
        <span class="menu-label">{rowSkipped(menu.track)
          ? "Include in playlist playback"
          : "Skip during playlist playback"}</span>
      </button>
    {/if}
    {#if menu.skipError}
      <p class="menu-error" role="alert">{menu.skipError}</p>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:track:")}
      <button class="menu-item" onclick={() => { menu.open = false; navigate("radio", menu.track.id); }}>
        <Icon name="radio" size={16} /><span class="menu-label">Go to song radio</span>
      </button>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:episode:") && menu.track?.album_id}
      <button class="menu-item" onclick={() => { menu.open = false; navigate("show", menu.track.album_id); }}>
        <Icon name="podcast" size={16} /><span class="menu-label">Go to podcast</span>
      </button>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:track:") && menu.track?.album_id}
      <button class="menu-item" onclick={() => { menu.open = false; navigate("album", menu.track.album_id); }}>
        <Icon name="album" size={16} /><span class="menu-label">Go to album</span>
      </button>
    {/if}
    <!-- One row whatever the track. A collaboration opens a submenu rather than
         printing an extra near-identical line per artist. -->
    {#if menu.track?.uri?.startsWith("spotify:track:") && menuArtists.length}
      <button
        class="menu-item"
        aria-haspopup={menuArtists.length > 1 ? "menu" : undefined}
        aria-expanded={menuArtists.length > 1 ? artistPicker.open : undefined}
        onclick={goToArtist}
      >
        <Icon name="artist" size={16} /><span class="menu-label">Go to artist</span>
        {#if menuArtists.length > 1}<span class="chev"><Icon name="forward" size={14} /></span>{/if}
      </button>
    {/if}
    {#if menu.track?.uri?.startsWith("spotify:track:") && playlistId}
      <div class="menu-sep"></div>
      <button
        class="menu-item danger"
        onclick={() => { menu.open = false; api.removePlaylistTracks(playlistId, [menu.track.uri]).catch(() => {}); }}
      >
        <Icon name="trash" size={16} /><span class="menu-label">Remove from this playlist</span>
      </button>
    {/if}
    </div>
  </Menu>
{/if}

{#if picker.open}
  <Menu class="track-submenu submenu" placement={picker} returnTo={picker.trigger} group label="Add to playlist" onclose={() => (picker.open = false)}>
    <div class="menu-scroll" use:scrollbar style:max-height="{picker.maxH}px">
      {#each library as pl (pl.id)}
        <button class="menu-item" title={pl.name} onclick={() => addToPlaylist(pl)}><span class="menu-label">{pl.name}</span></button>
      {/each}
    </div>
  </Menu>
{/if}

{#if artistPicker.open}
  <Menu class="track-submenu submenu" placement={artistPicker} returnTo={artistPicker.trigger} group label="Go to artist" onclose={() => (artistPicker.open = false)}>
    <div class="menu-scroll" use:scrollbar style:max-height="{artistPicker.maxH}px">
    {#each artistPicker.artists as artist (artist.id)}
      <button
        class="menu-item"
        onclick={() => {
          menu.open = false;
          artistPicker.open = false;
          navigateArtist(artist.id, artist.name);
        }}
        title={artist.name}
      ><span class="menu-label">{artist.name}</span></button>
    {/each}
    </div>
  </Menu>
{/if}

<style>
  /* `max-height` is set INLINE, from the room actually available above or
     below the anchor — see placePopover. It used to be `60vh` here, which is a
     number that knows nothing about where the menu was put, so a menu that
     flipped upward near the top of the list was clamped to the window and slid
     under the topbar's glass with its first items unreachable. */
  /* `inset` and `margin` are overrides of the UA's own `[popover]` rule, which
     sets `inset: 0; margin: auto` to centre a popover in the viewport. The
     three declarations that follow it are the position; the UA's `right: 0`
     would otherwise stretch every menu to the right edge of the window. */
  :global(.menu.track-menu), :global(.menu.track-submenu) { padding: 0; }
  /* MENU_W, in the script, places it: keep the two equal. */
  :global(.menu.track-menu) { width: 264px; min-width: 0; max-width: calc(100vw - 16px); }
  .menu-scroll {
    padding: var(--s1);
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .menu-error {
    max-width: 240px;
    margin: 4px 10px 7px;
    color: var(--rose-ink);
    font-size: var(--t-11);
    line-height: 1.35;
  }
  .menu-item .edit-check { color: var(--accent); }
</style>
