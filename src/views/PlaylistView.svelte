<script module>
  /**
   * ONE collator, not one per comparison.
   *
   * `String#localeCompare` builds a collator on every call when it is handed
   * an options object, and the text sort made tens of thousands of those calls
   * to order a 5,000-row playlist. Collation is pure and these options never
   * change, so a single instance serves every sort — module scope rather than
   * the component's own script, because opening the next playlist should not
   * build another one.
   */
  const SORT_COLLATOR = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
</script>

<script>
  import { untrack } from "svelte";
  import { SvelteMap } from "svelte/reactivity";
  import { listen } from "@tauri-apps/api/event";
  import {
    detail,
    library,
    api,
    playback,
    isPlayingSource,
    navigate,
    route,
    promotePlaylist,
    togglePlay,
    ui,
    session,
    sessionEpoch,
    retryDetail,
    loadPlaylistRecommendations,
    removePlaylistRecommendation,
  } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import PlaylistCleanup from "../components/PlaylistCleanup.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { formatTotal } from "../lib/time.js";
  import { detailArtSize } from "../lib/layout.js";
  import { occurrenceKeys, occurrenceIdentity } from "../lib/occurrence-keys.js";
  import HeaderMenu from "../components/HeaderMenu.svelte";
  import PlaylistActions, { playlistRequest } from "../components/PlaylistActions.svelte";

  const pl = $derived(detail.playlist);
  /* The cover gives way before the title does when the pane is narrow. */
  const artSize = $derived(detailArtSize(ui.paneWidth));
  const tracks = $derived(pl?.tracks ?? []);
  const editable = $derived(
    !!pl?.owner_id && !!session.username && pl.owner_id === session.username,
  );

  const sortState = $state({ key: "order", direction: "asc" });
  const SORT_STORAGE_PREFIX = "playlist-sort:";
  const SORT_KEYS = new Set(["order", "title", "artist", "album", "added", "duration"]);
  const SORT_DIRECTIONS = new Set(["asc", "desc"]);

  function defaultSort() {
    return { key: "order", direction: "asc" };
  }

  function readSort(id) {
    const fallback = defaultSort();
    if (!id) return fallback;
    try {
      const saved = JSON.parse(localStorage.getItem(`${SORT_STORAGE_PREFIX}${id}`) ?? "null");
      if (
        !saved ||
        typeof saved !== "object" ||
        (saved.key !== null && !SORT_KEYS.has(saved.key)) ||
        !SORT_DIRECTIONS.has(saved.direction)
      ) {
        return fallback;
      }
      return { key: saved.key, direction: saved.direction };
    } catch {
      return fallback;
    }
  }

  function writeSort(id, key, direction) {
    if (
      !id ||
      (key !== null && !SORT_KEYS.has(key)) ||
      !SORT_DIRECTIONS.has(direction)
    ) {
      return;
    }
    try {
      localStorage.setItem(
        `${SORT_STORAGE_PREFIX}${id}`,
        JSON.stringify({ key, direction }),
      );
    } catch {
      /* private mode / storage disabled: sorting remains session-local */
    }
  }

  $effect(() => {
    if (route.name !== "playlist" || !route.id) return;
    const saved = readSort(route.id);
    sortState.key = saved.key;
    sortState.direction = saved.direction;
  });

  function textSortValue(track, key) {
    if (key === "artist") {
      return (track?.artist_names ?? []).filter(Boolean).join(", ").trim();
    }
    if (key === "album") return String(track?.album_name ?? "").trim();
    return String(track?.name ?? "").trim();
  }

  function compareOptionalText(left, right, direction) {
    const leftMissing = !left;
    const rightMissing = !right;
    if (leftMissing || rightMissing) {
      if (leftMissing && rightMissing) return 0;
      // Missing metadata stays at the end in either direction.
      return leftMissing ? 1 : -1;
    }
    return SORT_COLLATOR.compare(left, right) * direction;
  }

  function compareOptionalNumber(left, right, direction) {
    const leftMissing = !Number.isFinite(left) || left <= 0;
    const rightMissing = !Number.isFinite(right) || right <= 0;
    if (leftMissing || rightMissing) {
      if (leftMissing && rightMissing) return 0;
      return leftMissing ? 1 : -1;
    }
    return (left - right) * direction;
  }

  /**
   * The sort key for one row, built ONCE per row instead of inside the
   * comparator.
   *
   * The comparator used to rebuild both operands' keys on every comparison —
   * tens of thousands of string builds to sort a 5,000-row playlist, measured
   * at ~80ms for an artist sort. A key is a function of the row alone, so it
   * belongs in the same pass that decorates the row with its original index:
   * the work is then proportional to the rows, and the comparator does
   * nothing but compare.
   */
  function sortKeyFor(track, key) {
    switch (key) {
      case "title":
      case "artist":
      case "album":
        return textSortValue(track, key);
      case "added":
        return Number(track?.added_at);
      case "duration":
        return Number(track?.duration_ms);
      default:
        return 0;
    }
  }

  const sortedTracks = $derived.by(() => {
    // No sort is the common case and the list can be thousands long; the
    // comparator below would run and decide nothing.
    if (sortState.key === null || (sortState.key === "order" && sortState.direction === "asc")) return tracks;
    // Reverse the stored sequence directly: sorting decorated rows here costs
    // O(n log n) comparisons and O(n) transient objects for a pure reversal.
    if (sortState.key === "order") return tracks.slice().reverse();
    const sortKey = sortState.key;
    const direction = sortState.direction === "asc" ? 1 : -1;
    return tracks
      .map((track, originalIndex) => ({
        track,
        originalIndex,
        sortValue: sortKeyFor(track, sortKey),
      }))
      .sort((left, right) => {
        let comparison = 0;
        switch (sortKey) {
          case "title":
          case "artist":
          case "album":
            comparison = compareOptionalText(left.sortValue, right.sortValue, direction);
            break;
          case "added":
          case "duration":
            comparison = compareOptionalNumber(left.sortValue, right.sortValue, direction);
            break;
          case "order":
          default:
            comparison = (left.originalIndex - right.originalIndex) * direction;
            break;
        }
        // Explicit original-index tiebreaking keeps equal/missing metadata
        // stable even on runtimes whose Array#sort implementation changes.
        return comparison || left.originalIndex - right.originalIndex;
      })
      .map(({ track }) => track);
  });
  const sortedTrackKeys = $derived.by(() => {
    // A known local MOV carries occurrence tokens with its rows. A new
    // authoritative array cannot inherit that proof merely by matching URIs.
    const tokens = reorderSession?.rows === tracks
      ? reorderSession.tokens : occurrenceKeys(tracks);
    const byTrack = new Map();
    for (let i = 0; i < tracks.length; i++) {
      const keys = byTrack.get(tracks[i]) ?? [];
      keys.push(tokens[i]);
      byTrack.set(tracks[i], keys);
    }
    const seen = new Map();
    return sortedTracks.map((track) => {
      const occurrence = seen.get(track) ?? 0;
      seen.set(track, occurrence + 1);
      return byTrack.get(track)[occurrence];
    });
  });

  /**
   * Ascending, then descending, then off.
   *
   * Without the third step a sort can be changed but never undone: the
   * playlist's own order is a real choice, and returning to it should not
   * require leaving the page.
   */
  function toggleSort(key) {
    if (!SORT_KEYS.has(key)) return;
    if (sortState.key !== key) {
      sortState.key = key;
      sortState.direction = "asc";
    } else if (sortState.direction === "asc") {
      sortState.direction = "desc";
    } else {
      sortState.key = null;
      sortState.direction = "asc";
    }
    writeSort(route.id, sortState.key, sortState.direction);
  }

  /**
   * Manual reorder is meaningful only against the playlist's own sequence,
   * and BOTH "#" directions show it: ascending is storage order itself,
   * descending the same sequence read backwards — reorderTracks translates
   * view positions back to storage coordinates for it. Unsorted (key null)
   * is storage order too. Any other projection is a view, not storage:
   * dragging inside it would move rows the server never placed there, so
   * the table keeps its drag-out (add/move elsewhere) but takes no drops.
   */
  const ownOrderShown = $derived(sortState.key === null || sortState.key === "order");

  /**
   * Drag feedback changes the displayed array immediately. The backend MOV
   * uses indices, so commands must follow that same order, one at a time.
   * Tokens distinguish repeated tracks while a failed command is reconciled
   * against the authoritative sequence before later drags are sent.
   */
  let reorderSession = null;
  let reorderBlocked = $state(false);

  function move(list, from, to) {
    const [item] = list.splice(from, 1);
    list.splice(to, 0, item);
  }

  /** Publishes a row order. The playlist is a raw snapshot (see `detail`), so
      a new order is a new array on a new snapshot, never an edit of the one
      on screen: an edit would reach no reader. */
  function showOrder(rows) {
    if (reorderSession) reorderSession.rows = rows;
    detail.playlist = { ...detail.playlist, tracks: rows };
  }

  function activeReorder(session) {
    return reorderSession === session && route.name === "playlist" &&
      route.id === session.id && detail.playlist?.id === session.id &&
      detail.playlist.rows_complete === true;
  }

  async function fetchReorderRefresh(session) {
    const generation = ++session.refreshGeneration;
    let settle;
    const refreshed = new Promise((resolve) => { settle = resolve; });
    const unlisten = await listen("playlist", ({ payload }) => {
      if (!activeReorder(session) || generation !== session.refreshGeneration ||
        payload?.id !== session.id) return;
      // A refresh started before our successful MOVs cannot describe them.
      if (session.successes && payload.snapshot_id === session.startSnapshot) return;
      settle(payload);
    });
    session.cancelRefresh = () => settle(null);
    let timeout;
    try {
      if (!activeReorder(session)) return null;
      const followed = library.some((entry) => entry.id === session.id);
      const response = await api.browsePlaylist(session.id);
      if (!activeReorder(session)) return null;
      // Unfollowed playlists have no shell track-cache hit: this command
      // performed the real fetch itself, without scheduling a later event.
      if (!followed) return response;
      return await Promise.race([
        refreshed,
        new Promise((_, reject) => {
          timeout = setTimeout(() => reject(new Error("The playlist refresh did not finish.")), 30000);
        }),
      ]);
    } finally {
      clearTimeout(timeout);
      session.cancelRefresh = null;
      unlisten();
    }
  }

  async function flushReorders(session) {
    if (session.running) return;
    session.running = true;
    try {
      while (activeReorder(session) && session.pending.length) {
        const job = session.pending[0];
        try {
          await api.reorderPlaylistTracks(session.id, job.from, job.to, session.tokens.length);
          if (!activeReorder(session)) return;
          session.pending.shift();
          session.successes++;
          move(session.confirmedTokens, job.from, job.to);
        } catch (error) {
          if (!activeReorder(session)) return;
          reorderBlocked = true;
          ui.error = `Could not reorder playlist: ${String(error?.message ?? error)}`;
          let fresh;
          try {
            fresh = await fetchReorderRefresh(session);
          } catch (refreshError) {
            if (activeReorder(session)) {
              ui.error = `Could not refresh playlist after reorder failed: ${String(refreshError?.message ?? refreshError)}`;
              session.pending.length = 0;
            }
            return;
          }
          if (!activeReorder(session)) return;
          session.pending.shift(); // the failed move never reached storage
          if (!fresh || fresh.id !== session.id || !Array.isArray(fresh.tracks)) {
            ui.error = "Could not refresh playlist after reorder failed.";
            session.pending.length = 0;
            return;
          }
          if (fresh.rows_complete !== true) {
            session.pending.length = 0;
            reorderSession = null;
            detail.playlist = fresh;
            reorderBlocked = false;
            return;
          }
          const list = fresh.tracks.slice();
          const freshTokens = occurrenceKeys(list);
          // Keep duplicate rows' tokens attached to their confirmed storage
          // positions when the server reports the order of successful MOVs.
          session.tokens = freshTokens.length === session.confirmedTokens.length &&
            freshTokens.every((token, index) =>
              occurrenceIdentity(token) === occurrenceIdentity(session.confirmedTokens[index]))
            ? session.confirmedTokens.slice()
            : freshTokens;
          session.confirmedTokens = session.tokens.slice();
          // A downward drag lands AFTER its preceding row; an upward drag
          // lands BEFORE its following row. Their anchors survive rollback.
          const remaining = session.pending.splice(0);
          for (const pending of remaining) {
            const from = session.tokens.indexOf(pending.token);
            const neighbor = session.tokens.indexOf(pending.neighbor);
            if (from < 0 || neighbor < 0) {
              ui.error = "Playlist changed while reordering; a pending move could not be applied.";
              continue;
            }
            const to = pending.after
              ? (neighbor < from ? neighbor + 1 : neighbor)
              : (neighbor < from ? neighbor : neighbor - 1);
            if (from === to) continue;
            move(session.tokens, from, to);
            move(list, from, to);
            session.pending.push({ ...pending, from, to });
          }
          session.rows = list;
          detail.playlist = { ...fresh, tracks: list };
          reorderBlocked = false;
        }
      }
    } finally {
      session.running = false;
    }
  }

  function reorderTracks(from, to) {
    if (reorderBlocked || !editable || !ownOrderShown || pl?.rows_complete !== true) return;
    const list = detail.playlist?.tracks;
    const id = pl?.id;
    if (!id || route.name !== "playlist" || route.id !== id || !Array.isArray(list) ||
      from === to || from < 0 || to < 0 || from >= list.length || to >= list.length) return;
    const mirrored = sortState.key === "order" && sortState.direction === "desc";
    const last = list.length - 1;
    const actualFrom = mirrored ? last - from : from;
    const actualTo = mirrored ? last - to : to;
    if (!reorderSession || reorderSession.id !== id ||
        (!reorderSession.pending.length && reorderSession.rows !== list)) {
      const tokens = occurrenceKeys(list);
      reorderSession = {
        id, tokens, confirmedTokens: tokens.slice(), pending: [], running: false,
        startSnapshot: pl.snapshot_id, successes: 0, refreshGeneration: 0, cancelRefresh: null,
      };
    }
    const session = reorderSession;
    const rows = list.slice();
    move(rows, actualFrom, actualTo);
    move(session.tokens, actualFrom, actualTo);
    showOrder(rows);
    session.pending.push({
      from: actualFrom,
      to: actualTo,
      token: session.tokens[actualTo],
      after: actualTo > actualFrom,
      neighbor: session.tokens[actualTo > actualFrom ? actualTo - 1 : actualTo + 1],
    });
    flushReorders(session);
  }
  // Edit refreshes can arrive between two queued MOV replies. A refresh of
  // the first move must not erase the second drag's already visible position.
  $effect(() => {
    const list = pl?.tracks;
    const rowsComplete = pl?.rows_complete === true;
    if (!list) return;
    untrack(() => {
      const session = reorderSession;
      if (session && !rowsComplete) {
        session.pending.length = 0;
        session.cancelRefresh?.();
        reorderSession = null;
        reorderBlocked = false;
        return;
      }
      if (!session?.pending.length || !activeReorder(session) || session.rows === list) return;
      const incoming = occurrenceKeys(list);
      if (incoming.length !== session.tokens.length ||
        incoming.every((token, index) => token === session.tokens[index])) return;
      const rows = new Map(incoming.map((token, index) => [token, list[index]]));
      if (session.tokens.some((token) => !rows.has(token))) return;
      showOrder(session.tokens.map((token) => rows.get(token)));
    });
  });

  /* Fallback for a playlist the backend has not swept yet: derive the mosaic
     candidates from the tracks we already have on screen. */
  const artPool = $derived.by(() => {
    if (pl?.cover_url) return [];
    const covers = [];
    const seen = new Set();
    for (const track of tracks) {
      const cover = track?.cover_url;
      if (!cover || seen.has(cover)) continue;
      seen.add(cover);
      covers.push(cover);
      if (covers.length === 4) break;
    }
    return covers;
  });

  /**
   * The page's colour, read out of the artwork.
   *
   * A playlist's own cover if it has one; otherwise the whole mosaic, the same
   * list and in the same order the header's `<Cover>` is about to draw from.
   * It used to be `artPool[0]`, one arbitrary cell of four, which is how this
   * page opened brown beside three tiles that were not — `coverTone` now reads
   * every cell and keeps the one that most strongly has a colour. With
   * neither, it falls back to the same id-hashed identity hue the generated
   * tile uses, so the header and the tile still agree.
   */
  const tone = $derived(
    coverTone(pl?.cover_url || (pl?.cover_urls?.length ? pl.cover_urls : artPool), pl?.id ?? ""),
  );

  let renaming = $state(false);
  let nameDraft = $state("");
  let renameInput = $state(null);
  let renameSaving = $state(false);
  let renameError = $state("");
  let headerActions = $state(null);
  let deleteOpen = $state(false);
  let deleting = $state(false);
  let deleteError = $state("");
  let cleanupId = $state(null);
  /* The component is reused when one playlist route replaces another. Reset
     page-owned interactions before the incoming playlist can render them. */
  let activeId = null;
  let identity = 0;
  let renameId = $state(null);
  let deleteId = $state(null);
  $effect.pre(() => {
    const id = route.name === "playlist" ? route.id : null;
    if (id === activeId) return;
    activeId = id;
    identity++;
    reorderSession?.cancelRefresh?.();
    reorderSession = null;
    reorderBlocked = false;
    renaming = false;
    nameDraft = "";
    renameId = null;
    renameSaving = false;
    renameError = "";
    deleteOpen = false;
    deleteId = null;
    deleting = false;
    deleteError = "";
    cleanupId = null;
  });
  $effect(() => () => reorderSession?.cancelRefresh?.());
  $effect(() => {
    if (cleanupId && (route.name !== "playlist" || route.id !== cleanupId || pl?.id !== cleanupId || !editable)) {
      cleanupId = null;
    }
  });
  let recommendations = $state([]);
  const recommendationAdds = $state({});
  const recommendationState = $state({
    id: null,
    revision: "",
    requested: false,
    loading: false,
    hidden: false,
  });
  let recommendationFooter = $state(null);

  $effect(() => {
    if (!renaming || !editable) return;
    queueMicrotask(() => {
      renameInput?.focus();
      renameInput?.select();
    });
  });

  /* An action the rail's menu asked for (PlaylistActions), carried out once
     this playlist is the one on screen. */
  $effect(() => {
    const request = playlistRequest.action;
    if (!request || !pl || pl.id !== playlistRequest.id || route.id !== pl.id) return;
    untrack(() => {
      playlistRequest.id = null;
      playlistRequest.action = null;
      if (request === "rename") startRename();
      else if (request === "cleanup" && editable && tracks.length) cleanupId = pl.id;
      else if (request === "delete") requestDelete();
    });
  });

  // Recommendation tracks live in the session cache. A different playlist or
  // snapshot starts a new lazy demand without carrying rows across routes.
  $effect(() => {
    const id = pl?.id ?? null;
    const revision = pl?.snapshot_id ?? "";
    if (recommendationState.id === id && recommendationState.revision === revision) return;
    recommendationState.id = id;
    recommendationState.revision = revision;
    recommendationState.requested = false;
    recommendationState.loading = false;
    recommendationState.hidden = false;
    recommendations = [];
  });

  $effect(() => {
    const node = recommendationFooter;
    const id = pl?.id;
    const revision = pl?.snapshot_id ?? "";
    if (!node || !id || recommendationState.hidden || recommendationState.requested) return;
    const root = node.closest(".scroll");
    if (!root) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) requestRecommendations(id, revision);
      },
      { root, rootMargin: "0px 0px 480px 0px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  });

  async function requestRecommendations(
    expectedId = pl?.id,
    expectedRevision = pl?.snapshot_id ?? "",
    { force = false } = {},
  ) {
    const revision = expectedRevision ?? "";
    if (
      !expectedId ||
      recommendationState.id !== expectedId ||
      recommendationState.revision !== revision ||
      recommendationState.loading ||
      (recommendationState.requested && !force)
    ) return;
    recommendationState.requested = true;
    recommendationState.loading = true;
    recommendationState.hidden = false;
    try {
      const tracks = await loadPlaylistRecommendations(expectedId, revision, { force });
      if (
        recommendationState.id !== expectedId ||
        recommendationState.revision !== revision ||
        route.name !== "playlist" ||
        route.id !== expectedId
      ) return;
      recommendations = tracks;
      recommendationState.hidden = recommendations.length === 0;
    } catch {
      // An optional footer failure never invalidates the playlist. Leave an
      // existing successful set visible during a failed forced refresh.
      if (recommendationState.id === expectedId && recommendationState.revision === revision) {
        recommendationState.hidden = recommendations.length === 0;
      }
    } finally {
      if (recommendationState.id === expectedId && recommendationState.revision === revision) {
        recommendationState.loading = false;
      }
    }
  }


  function playRecommendation(index) {
    if (!recommendations[index]) return;
    api.playQueue(recommendations, index, `playlist:${pl?.id ?? ""}`).catch(() => {});
  }

  function recommendationAddKey(id, uri) {
    return `${id}\u0000${uri}`;
  }

  function recommendationAddDisabled(track) {
    const id = recommendationState.id;
    const uri = String(track?.uri ?? "").trim();
    return !!id && !!uri && !!recommendationAdds[recommendationAddKey(id, uri)];
  }

  async function addRecommendation(track) {
    const id = pl?.id;
    const revision = pl?.snapshot_id ?? "";
    const uri = String(track?.uri ?? "").trim();
    if (!id || !editable || !uri) return;
    const key = recommendationAddKey(id, uri);
    if (recommendationAdds[key]) return;
    recommendationAdds[key] = true;
    try {
      await api.addPlaylistTracks(id, [uri]);
      removePlaylistRecommendation(id, revision, uri);
      // A recommendation add is library activity, never playback.
      promotePlaylist(id);
      api.touchPlaylistActivity(id).catch(() => {});
      if (recommendationState.id === id && recommendationState.revision === revision) {
        recommendations = recommendations.filter((candidate) => candidate.uri !== uri);
        recommendationState.hidden = recommendations.length === 0;
      }
    } catch {
      // Keep the recommendation in place so a failed direct Add can be retried.
    } finally {
      delete recommendationAdds[key];
    }
  }
  /**
   * A successful queue command is the playback event for this local history.
   * The view knows the source playlist; the engine only receives track URIs and
   * cannot infer which followed playlist they came from.
   */
  function playQueue(queue, index, options = undefined) {
    const sourceId = pl?.id;
    const epoch = sessionEpoch();
    // Navigation cannot retarget a successful play; an account change fences it.
    return api.playQueue(queue, index, `playlist:${sourceId ?? ""}`, options).then(() => {
      if (!sourceId || epoch !== sessionEpoch()) return;
      promotePlaylist(sourceId, { played: true });
      api.touchPlaylist(sourceId).catch(() => {});
    });
  }

  function playFrom(i) {
    if (!pl) return;
    const queue = sortedTracks;
    if (!queue[i]) return;
    // The displayed order is a local projection; enqueue that exact array so
    // a click after sorting still targets the row the user selected.
    playQueue(queue, i).catch(() => {});
  }

  const playingThis = $derived(isPlayingSource(`playlist:${pl?.id ?? ""}`));

  /* Snapshot replacement starts with authoritative skips and overlays only
     this playlist/account owner's unsettled intents. The post-command refresh
     is authoritative, including a later change made elsewhere. */
  const excludedSnapshot = $derived(pl?.excluded_track_ids ?? null);
  const pendingSkips = new SvelteMap();
  let skipOwner = null;
  $effect.pre(() => {
    const id = pl?.id;
    const epoch = sessionEpoch();
    if (skipOwner?.id === id && skipOwner?.epoch === epoch) return;
    skipOwner = { id, epoch };
    pendingSkips.clear();
  });
  const excludedTrackIds = $derived.by(() => {
    const ids = excludedSnapshot;
    if (!ids) return null;
    const live = new Set(ids.map(String));
    for (const [id, intent] of pendingSkips) {
      if (intent.acknowledged && ids !== intent.snapshot) continue;
      if (intent.exclude) live.add(id);
      else live.delete(id);
    }
    return [...live];
  });
  $effect(() => {
    const snapshot = excludedSnapshot;
    for (const [id, intent] of pendingSkips) {
      if (intent.acknowledged && snapshot !== intent.snapshot) pendingSkips.delete(id);
    }
  });
  function setTrackExcluded(id, exclude) {
    const owner = skipOwner;
    if (!owner?.id || owner.id !== pl?.id || owner.epoch !== sessionEpoch()) return Promise.resolve();
    const previous = pendingSkips.get(id)?.promise;
    const intent = { exclude, acknowledged: false, snapshot: null, promise: null };
    const currentOwner = () => skipOwner === owner && owner.id === pl?.id && owner.epoch === sessionEpoch();
    pendingSkips.set(id, intent);
    intent.promise = (async () => {
      // Preserve write order for repeated intents without retaining an old
      // optimistic array. Only the latest operation owns reconciliation.
      if (previous) await previous.catch(() => {});
      // Navigation does not revoke an already chosen source-playlist intent.
      // An account transition does, before a queued native write is dispatched.
      if (owner.epoch !== sessionEpoch()) return;
      try {
        await api.setPlaylistTrackExcluded(owner.id, id, exclude);
        if (!currentOwner() || pendingSkips.get(id) !== intent) return;
        const snapshot = excludedSnapshot;
        if ((snapshot ?? []).map(String).includes(id) === exclude) pendingSkips.delete(id);
        else pendingSkips.set(id, { ...intent, acknowledged: true, snapshot });
      } catch (reason) {
        if (currentOwner() && pendingSkips.get(id) === intent) pendingSkips.delete(id);
        throw reason;
      }
    })();
    return intent.promise;
  }

  const skippedSet = $derived.by(
    () => new Set((excludedTrackIds ?? []).map(String)),
  );

  /**
   * Where AUTOMATIC playback may begin: the first row that is neither skipped
   * nor unavailable. Direct plays never consult this — clicking a skipped row
   * still plays that row, which is what makes the preference a preference.
   */
  const automaticStartIndex = $derived(
    sortedTracks.findIndex(
      (track) => !track.unavailable && !skippedSet.has(String(track.id)),
    ),
  );

  /**
   * Said once, calmly, beside the buttons when automatic playback has nowhere
   * legal to start. Derived rather than set-and-cleared, so including a row
   * again takes the sentence away without anyone remembering to.
   */
  const automaticStartBlocked = $derived(
    pl && tracks.length > 0 && automaticStartIndex < 0
      ? "Every song in this playlist is skipped or unavailable."
      : "",
  );

  /**
   * Play, or pause/resume what this playlist already started.
   *
   * Automatic starts begin at the first row the skip preference allows; a
   * start with nowhere legal to begin does nothing here — the sentence beside
   * the buttons explains why — and never falls through to an excluded row.
   */
  function playOrToggle() {
    if (playingThis) {
      togglePlay();
      return;
    }
    const queue = sortedTracks;
    if (!queue.length || automaticStartIndex < 0) return;
    playQueue(queue, automaticStartIndex, { automaticStart: true }).catch(() => {});
  }

  async function shufflePlay() {
    const queue = sortedTracks;
    const index = automaticStartIndex;
    if (!queue.length || index < 0) return;
    try {
      await api.setShuffle(true);
      await playQueue(queue, index, { automaticStart: true });
    } catch {}
  }

  function startRename() {
    if (!editable || pl?.id !== route.id) return;
    renameId = pl.id;
    nameDraft = pl.name ?? "";
    renameError = "";
    renaming = true;
  }

  function cancelRename() {
    if (renameSaving) return;
    renaming = false;
    renameId = null;
    nameDraft = "";
    renameError = "";
  }

  async function commitRename() {
    const id = renameId;
    const started = identity;
    const n = nameDraft.trim();
    if (!renaming || !id || pl?.id !== id || route.id !== id || !editable || renameSaving) return;
    if (!n) {
      renameError = "Playlist name cannot be empty.";
      return;
    }
    if (n === pl.name) {
      cancelRename();
      return;
    }
    renameSaving = true;
    renameError = "";
    try {
      await api.renamePlaylist(id, n);
      if (started === identity && renameId === id && route.id === id) {
        renaming = false;
        renameId = null;
      }
    } catch (error) {
      if (started === identity && renameId === id && route.id === id) {
        renameError = error instanceof Error ? error.message : String(error || "Could not rename this playlist.");
      }
    } finally {
      if (started === identity && renameId === id && route.id === id) renameSaving = false;
    }
  }

  function requestDelete() {
    cleanupId = null;
    if (!editable || pl?.id !== route.id) return;
    deleteId = pl.id;
    deleteError = "";
    deleteOpen = true;
  }

  async function deletePlaylist() {
    const id = deleteId;
    const started = identity;
    if (!deleteOpen || !id || pl?.id !== id || route.id !== id || !editable || deleting) return;
    deleting = true;
    deleteError = "";
    try {
      await api.deletePlaylist(id);
      if (started === identity && deleteId === id && route.id === id) {
        deleteOpen = false;
        deleteId = null;
        navigate("library");
      }
    } catch (error) {
      if (started === identity && deleteId === id && route.id === id) {
        deleteError = error instanceof Error ? error.message : String(error || "Could not delete this playlist.");
      }
    } finally {
      if (started === identity && deleteId === id && route.id === id) deleting = false;
    }
  }
</script>

<section
  class="view page wash"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if detail.error && !pl}
    <!-- The request failed, so this page stays a frame with an explanation in
         it rather than a skeleton that never resolves. -->
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="tag">Playlist</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This playlist could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="pill" onclick={retryDetail}>Try again</button>
        <button class="link-more" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if !pl}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <!-- Tag, title, meta and the two controls, each at the size of the thing
           that replaces it, so nothing moves when the playlist lands. -->
      <div>
        <span class="skeleton line sm" style="width:62px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(440px,72%)"></span>
        <span class="skeleton line sm" style="width:200px"></span>
        <div class="actions">
          <span class="skeleton" style="width:48px;height:48px;border-radius:var(--rf)"></span>
          <span class="skeleton" style="width:40px;height:40px;border-radius:var(--rf)"></span>
        </div>
      </div>
    </header>
    <div
      class="tl"
      style="margin-top:var(--s6);--cols:28px 36px minmax(0,1fr) 52px"
      aria-hidden="true"
    >
      {#each Array.from({ length: 9 }) as _, i (i)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk art"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{64 - ((i * 7) % 24)}%"></span>
            <span class="sk b" style="width:{32 - ((i * 5) % 11)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
      {/each}
    </div>
  {:else}
    <header class="detail-head">
      <Cover
        src={pl.cover_url}
        srcs={pl.cover_urls?.length ? pl.cover_urls : artPool}
        id={pl.id}
        name={pl.name}
        size={artSize}
        lg
        raised
      />
      <div>
        <span class="tag">Playlist</span>
        {#if renaming && renameId === pl.id && route.id === renameId && editable}
          <form
            class="rename-form"
            aria-label="Rename playlist"
            onsubmit={(e) => {
              e.preventDefault();
              commitRename();
            }}
          >
            <span class="field lg">
            <input
              bind:this={renameInput}
              bind:value={nameDraft}
              aria-label="Playlist name"
              aria-invalid={!!renameError}
              disabled={renameSaving}
              onkeydown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  e.stopPropagation();
                  cancelRename();
                }
              }}
              spellcheck="false"
            />
            </span>
            <div class="rename-controls">
              <button class="pill accent" type="submit" disabled={renameSaving}>
                {renameSaving ? "Saving…" : "Save"}
              </button>
              <button class="pill" type="button" disabled={renameSaving} onclick={cancelRename}>Cancel</button>
            </div>
            {#if renameError}<p class="inline-error" role="alert">{renameError}</p>{/if}
          </form>
        {:else}
          <h1 class="detail-title">{pl.name}</h1>
        {/if}
        <p class="detail-meta">
          <button class="who" title="Open {pl.owner}'s profile" onclick={() => navigate("profile", pl.owner_id || pl.owner)}>{pl.owner}</button>
          <span class="sep">/</span><span class="num">{tracks.length} songs</span>
          {#if tracks.length}
            <span class="sep">/</span><span class="num">{formatTotal(tracks)}</span>
          {/if}
        </p>
        <div class="actions" bind:this={headerActions}>
          <button
            class="play-lg"
            title={playingThis ? (playback.playing ? "Pause" : "Resume") : "Play"}
            onclick={playOrToggle}
            disabled={!tracks.length}
          >
            <Icon name={playingThis && playback.playing ? "pause" : "play"} size={22} />
          </button>
          <button class="btn-round lg" title="Shuffle" aria-label="Shuffle" onclick={shufflePlay} disabled={!tracks.length}>
            <Icon name="shuffle" size={20} />
          </button>
          <HeaderMenu label="Playlist actions">
            {#snippet children(close)}
              <PlaylistActions
                playlist={pl}
                trackCount={tracks.length}
                {close}
                onRename={startRename}
                onCleanup={() => (cleanupId = pl.id)}
                onDelete={requestDelete}
              />
            {/snippet}
          </HeaderMenu>
        </div>
        {#if automaticStartBlocked}
          <!-- Calm by design: grey type under the controls, not a red banner.
               Nothing failed — the listener chose this, and including any row
               again takes it away. -->
          <p class="start-blocked" role="alert">{automaticStartBlocked}</p>
        {/if}
        {#if editable && tracks.length && pl.rows_complete !== true}
          <p class="start-blocked">Reordering is unavailable until every playlist item can be shown, including local files and songs that could not be loaded.</p>
        {/if}
      </div>
    </header>

    {#if tracks.length}
      <div style="margin-top:var(--s6)">
        <TrackList
          tracks={sortedTracks}
          trackKeys={sortedTrackKeys}
          {playFrom}
          playlistId={editable ? pl.id : null}
          queueContext={`playlist:${pl?.id ?? ""}`}
          showAdded
          sortKey={sortState.key}
          sortDirection={sortState.direction}
          onSort={toggleSort}
          excludedTrackIds={excludedTrackIds}
          onSetExcluded={setTrackExcluded}
          onReorder={editable && ownOrderShown && pl.rows_complete === true && !reorderBlocked ? reorderTracks : null}
        />
      </div>
    {:else}
      <div class="empty">
        <p>No songs here yet.</p>
        <p class="sub">Find something in Search and add it to this playlist.</p>
      </div>
    {/if}

    <div bind:this={recommendationFooter} aria-hidden={recommendationState.hidden ? "true" : undefined}>
      {#if !recommendationState.hidden}
        <section class="section" aria-labelledby="playlist-recommendations-title">
          <div class="section-head">
            <h2 class="section-title" id="playlist-recommendations-title">Recommended</h2>
            {#if recommendationState.requested}
              <button
                class="link-more"
                disabled={recommendationState.loading}
                onclick={() => requestRecommendations(pl.id, pl.snapshot_id, { force: true })}
              >
                {recommendationState.loading ? "Refreshing…" : "Refresh"}
              </button>
            {:else}
              <button class="link-more" onclick={() => requestRecommendations(pl.id, pl.snapshot_id)}>
                Load recommendations
              </button>
            {/if}
          </div>
          {#if recommendationState.loading && !recommendations.length}
            <div class="tl" style="--cols:28px 36px minmax(0,1fr) 52px" aria-label="Loading recommendations">
              {#each Array.from({ length: 4 }) as _, i (i)}
                <div class="sk-row">
                  <span class="sk" style="width:12px"></span>
                  <span class="sk art"></span>
                  <span class="sk-stack">
                    <span class="sk a" style="width:{64 - ((i * 7) % 24)}%"></span>
                    <span class="sk b" style="width:{32 - ((i * 5) % 11)}%"></span>
                  </span>
                  <span class="sk" style="width:28px;justify-self:end"></span>
                </div>
              {/each}
            </div>
          {:else if recommendations.length}
            <TrackList
              tracks={recommendations}
              playFrom={playRecommendation}
              showHead={false}
              queueContext={`playlist:${pl?.id ?? ""}`}
              allowAddToPlaylist={false}
              disableWindowing
              rowActionLabel={editable ? "Add" : null}
              rowActionDisabled={editable ? recommendationAddDisabled : null}
              onRowAction={editable ? addRecommendation : null}
            />
          {/if}
        </section>
      {/if}
    </div>
  {/if}
</section>
{#if deleteOpen && deleteId === pl?.id && route.name === "playlist" && route.id === deleteId && editable}
  <ConfirmDialog
    open={deleteOpen}
    title="Delete this playlist?"
    message={`"${pl?.name ?? "This playlist"}" and its ${tracks.length} songs are removed from your library.`}
    confirmLabel="Delete playlist"
    busyLabel="Deleting…"
    busy={deleting}
    error={deleteError}
    onConfirm={deletePlaylist}
    onCancel={() => {
      deleteOpen = false;
      deleteId = null;
      deleteError = "";
    }}
  />
{/if}
{#if cleanupId && cleanupId === pl?.id && route.name === "playlist" && route.id === cleanupId && editable}
  {#key cleanupId}
    <PlaylistCleanup playlist={pl} onClose={() => { cleanupId = null; queueMicrotask(() => headerActions?.querySelector('[aria-haspopup="menu"]')?.focus()); }} />
  {/key}
{/if}
