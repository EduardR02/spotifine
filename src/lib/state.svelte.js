/* ------------------------------------------------------------------ */
/* App state — updated ONLY by Tauri events + command responses.       */
/* All command wrappers use the exact Tauri contract names.            */
/* ------------------------------------------------------------------ */
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { normalizeCanonicalPlaylistDescription } from "./artist.js";
import { parseSpotifyLink } from "./spotify-link.js";
import { applyCacheMarks } from "./cache-marks.js";
import { findFolder, insertFirst, moveItem, removeFolder, removePlaylist, renameFolder } from "./library-tree.js";
import { createCanvasLookups } from "./canvas.js";

/* ---------------- Navigation ---------------- */

export const route = $state({ name: "library", id: null, param: null });

/**
 * Back/forward history as a plain stack plus a cursor. Deliberately not URL
 * or hash routing: nothing here is addressable or shareable, so a stack is
 * the entire feature and costs no dependency.
 */
const HISTORY_MAX = 50;
const history = $state({ entries: [{ name: "library", id: null, param: null }], cursor: 0 });

let navigationGuard = null;

/**
 * Installs the single synchronous route-leave guard used by transient editors.
 * The returned cleanup only removes the guard it installed.
 */
export function setNavigationGuard(guard) {
  navigationGuard = typeof guard === "function" ? guard : null;
  const installed = navigationGuard;
  return () => {
    if (navigationGuard === installed) navigationGuard = null;
  };
}

function navigationAllowed(entry, action) {
  return !navigationGuard || navigationGuard(entry, action) !== false;
}

export function canGoBack() {
  return history.cursor > 0;
}

export function canGoForward() {
  return history.cursor < history.entries.length - 1;
}

/** The artist routes that read one and the same `detail.artist` payload. */
const ARTIST_ROUTES = new Set([
  "artist",
  "discography",
  "fans-also-like",
  "appears-on",
  "artist-playlists",
  "discovered-on",
]);

/**
 * Clears stale detail so the target view shows its loading state.
 *
 * Artist detail and every auxiliary artist route are presentations of one
 * payload. Moving among them for the same artist must keep `detail.artist`;
 * blanking it would turn a data-free route change into another page load.
 * Called before the route is applied, so `route` still describes where we are
 * coming FROM.
 */
function clearStaleDetail(entry) {
  if (entry.name === "playlist") detail.playlist = null;
  if (entry.name === "radio") detail.radio = null;
  if (entry.name === "album") detail.album = null;
  if (ARTIST_ROUTES.has(entry.name)) {
    const sameArtist = ARTIST_ROUTES.has(route.name) && route.id === entry.id;
    if (!sameArtist) detail.artist = null;
  }
  // A failure belongs to the page that produced it, never to the next one.
  detail.error = "";
}

function applyEntry(entry) {
  clearStaleDetail(entry);
  route.name = entry.name;
  route.id = entry.id;
  route.param = entry.param ?? null;
}

/**
 * `param` is a second, non-identifying coordinate — which discography segment
 * is selected, for instance. It rides in the history entry so back and forward
 * restore the view you actually left, but it is deliberately NOT part of the
 * "are we already here" test: re-selecting the same page with a different
 * segment is a real navigation.
 */
export function navigate(name, id = null, param = null) {
  const current = history.entries[history.cursor];
  if (current && current.name === name && current.id === id && current.param === param) return;
  if (!navigationAllowed({ name, id, param }, "navigate")) return;
  // Navigating from the middle of the stack starts a new branch, so anything
  // ahead of the cursor is dropped.
  history.entries.splice(history.cursor + 1);
  history.entries.push({ name, id, param });
  if (history.entries.length > HISTORY_MAX) history.entries.shift();
  history.cursor = history.entries.length - 1;
  applyEntry({ name, id, param });
}

export function goBack() {
  if (!canGoBack()) return;
  if (!navigationAllowed(history.entries[history.cursor - 1], "back")) return;
  history.cursor -= 1;
  applyEntry(history.entries[history.cursor]);
}

export function goForward() {
  if (!canGoForward()) return;
  if (!navigationAllowed(history.entries[history.cursor + 1], "forward")) return;
  history.cursor += 1;
  applyEntry(history.entries[history.cursor]);
}

/**
 * The display names a navigation already knew, keyed by artist id.
 *
 * Every production entry into an artist page carries the Spotify-provided
 * artist name at the click site, while the route itself records only the id —
 * so anything needing the name has waited for the whole artist payload.
 * Recording the name here lets ArtistView start songwriter discovery from the
 * route alone. Deliberately not `$state`: entries are written synchronously
 * before the navigation commits and read on mount, so reactivity would only
 * be global noise. Bounded at HISTORY_MAX because its lifetime is one
 * history stack's — a hint older than the oldest place back can never be
 * read again.
 */
const artistNameHints = new Map();

export function navigateArtist(id, name) {
  const artistId = typeof id === "string" ? id.trim() : "";
  if (!artistId) return;
  const hint = typeof name === "string" ? name.trim() : "";
  if (hint) {
    /* Re-insert so iteration order stays most-recent-first and eviction takes
       the stalest entry, not an arbitrarily old one. */
    artistNameHints.delete(artistId);
    artistNameHints.set(artistId, hint);
    while (artistNameHints.size > HISTORY_MAX) {
      artistNameHints.delete(artistNameHints.keys().next().value);
    }
  }
  navigate("artist", artistId);
}

/** The recorded name for `id`, or "" when navigation knew none. Read-only. */
export function artistNameHint(id) {
  const key = typeof id === "string" ? id.trim() : "";
  return (key && artistNameHints.get(key)) || "";
}

/* Whether the inspector was open last time, in localStorage. Read here at
   module init rather than from a mount effect so the very first paint already
   has the restored layout — restoring later shows the panel visibly swinging
   open after the window appears. Reading is wrapped because a webview with
   storage disabled throws on access rather than returning null. */
const INSPECTOR_KEY = "sr.now-playing-open";

function readNowPlayingOpen() {
  try {
    return localStorage.getItem(INSPECTOR_KEY) === "1";
  } catch {
    return false;
  }
}

/**
 * `paneWidth` is the measured inner width of the content pane, published by
 * App.svelte from a ResizeObserver.
 *
 * It exists because the track table has to choose its columns from the space
 * it actually has, and that space is not the window: it is the window minus
 * the rail, minus the inspector when it is open, minus three gutters. Media
 * queries were doing that arithmetic by hand in two duplicated blocks and got
 * it wrong (see TrackList). A container query is not an option either —
 * `container-type: inline-size` implies `contain: layout`, which would make
 * the pane a containing block for `position: fixed` and reposition every row
 * menu. One observed number, read on resize and never on scroll.
 */
export const ui = $state({
  searchFocusTick: 0,
  nowPlayingOpen: readNowPlayingOpen(),
  /* The panel's Canvas is the picture, covering the panel edge to edge, so
     the haze under it is never seen. Owned by NowPlayingPanel; the haze
     reads it. The layout does not: an open panel has one shape. */
  immersive: false,
  paneWidth: 0,
  /* Whether the app window has focus. Owned by App.svelte, published here
     because two unrelated surfaces gate decorative work on it: the VU meter's
     shared meter clock and the Canvas video's decoder. */
  windowFocused: true,
  /* An action the FRONTEND started and that failed, for the one banner in
     App.svelte. Separate from `playback.error` on purpose, in both
     directions. `playback.error` is an engine field — it rides in on the
     state payload and `applyPlayback` overwrites it from every event — so a
     message written there is a message the next state tick may erase before
     it has been read. And it is read as a meaning, not just a string:
     TrackEditorView tests it bare to decide its preview failed, so parking an
     unrelated failure in it makes some other surface report itself broken.
     Two owners, two fields; the banner shows whichever is set. */
  error: null,
});

/** The one write point for the inspector, so every toggle persists. */
export function setNowPlayingOpen(open) {
  ui.nowPlayingOpen = !!open;
  try {
    localStorage.setItem(INSPECTOR_KEY, ui.nowPlayingOpen ? "1" : "0");
  } catch {
    /* private mode / storage disabled: the panel stays session-local */
  }
}

/** The single settings snapshot, hydrated by bootstrap and mutation replies. */
export const appSettings = $state({ animated_canvas: true, devices_enabled: true });
export const settingsState = $state({ loaded: false });

/**
 * The name the sticky top bar shows for a page that loads its own record —
 * a show, an episode, a profile — rather than going through `detail`. The
 * view writes it against its route; the bar shows it only while that route
 * is the one on screen, so a stale name can never label another page.
 */
export const pageTitle = $state({ route: "", text: "" });
export function setPageTitle(text) {
  pageTitle.route = `${route.name}\u0000${route.id ?? ""}`;
  pageTitle.text = text ?? "";
}

const trackEditor = $state({ tracks: {} });
const trackEditorKeys = [];


function trackEditorKey(trackId, playlistId) {
  return JSON.stringify([String(trackId ?? ""), playlistId || null]);
}

export function getTrackEditorTrack(trackId, playlistId = null) {
  return trackEditor.tracks[trackEditorKey(trackId, playlistId)] ?? null;
}

export function openTrackEditor(track, playlistId = null) {
  if (!track?.id) return;
  const sourcePlaylist = playlistId || null;
  const key = trackEditorKey(track.id, sourcePlaylist);
  trackEditor.tracks[key] = { ...track };
  const oldIndex = trackEditorKeys.indexOf(key);
  if (oldIndex >= 0) trackEditorKeys.splice(oldIndex, 1);
  trackEditorKeys.push(key);
  while (trackEditorKeys.length > HISTORY_MAX) {
    delete trackEditor.tracks[trackEditorKeys.shift()];
  }
  navigate("track-editor", track.id, sourcePlaylist);
}

function applyAppSettings(value) {
  if (!value || typeof value !== "object") return;
  Object.assign(appSettings, value);
  settingsState.loaded = true;
}

let appSettingsRevision = 0;

function mutateAppSettings(command, args) {
  const revision = ++appSettingsRevision;
  return invoke(command, args).then((value) => {
    if (revision === appSettingsRevision) applyAppSettings(value);
    return value;
  });
}

export function focusSearch() {
  if (route.name !== "search") navigate("search");
  ui.searchFocusTick += 1;
}

/* ---------------- Playback ---------------- */

/**
 * The engine's playback state as the webview last heard it.
 *
 * `queue` and `upcoming` are raw, like the detail payloads (`detail`): a
 * payload that moves them hands over whole new arrays, which are adopted as
 * they arrive and never edited in place (`applyPlayback`). A deep proxy would
 * wrap every row of a 10,000-song queue, and every reader would walk those
 * proxies: the download marks on each delta, the queue view's keys on each
 * change. Marks are the one write into a row, and they announce themselves
 * through `isCached`.
 */
class PlaybackState {
  ready = $state(false);
  auth_state = $state(null);
  auth_url = $state(null);
  playing = $state(false);
  buffering = $state(false);
  output_device_id = $state("");
  output_device_name = $state("");
  username = $state(null);
  position_ms = $state(0);
  duration_ms = $state(0);
  volume = $state(100);
  shuffle = $state(false);
  repeat = $state("off");
  playback_speed = $state(1);
  audible_playback_speed = $state(1);
  current_index = $state(-1);
  current_uri = $state(null);
  context = $state("");
  queue = $state.raw([]);
  queue_revision = $state(0);
  order_revision = $state(0);
  /* Queue indexes in the order automatic playback will actually reach them,
     current row excluded. Only the engine knows this: it holds the live
     shuffle bag and the per-playlist exclusions, so index order is a guess. */
  upcoming = $state.raw([]);
  /* The row the engine is fetching ahead for a gapless start, from about
     thirty seconds before the end until the next load; "" otherwise. The
     only time the app does anything for a track before it plays: the panel
     warms its Canvas (NowPlayingPanel). */
  preload_uri = $state("");
  error = $state(null);
}
export const playback = new PlaybackState();

/** The fields a payload's value is copied into as it arrives: all of them but
    `queue` and `upcoming`, which are adopted by revision (`applyPlayback`). A
    compiled class keeps its state behind accessors, so the names are listed
    rather than read off the object. */
const PLAYBACK_FIELDS = new Set([
  "ready", "auth_state", "auth_url", "playing", "buffering", "output_device_id",
  "output_device_name", "username", "position_ms", "duration_ms", "volume", "shuffle",
  "repeat", "playback_speed", "audible_playback_speed", "current_index", "current_uri",
  "context", "queue_revision", "order_revision", "preload_uri", "error",
]);

/** Source identity comes from the engine's active row, never queue contents. */
export function isPlayingSource(context) {
  return Boolean(context && playback.current_uri && playback.context === context);
}
let playingRequestGeneration = 0;
let playingAuthorityGeneration = 0;
let seekRequestGeneration = 0;
let seekAuthorityGeneration = 0;
let volumeRequestGeneration = 0;
let volumeAuthorityGeneration = 0;
let volumePendingGeneration = null;
let confirmedVolume = playback.volume;


// Logical playback target: transport ticks and queue-tail edits are not new plays.
let playbackIdentityGeneration = $state(0);

export function playbackIdentity() {
  return playbackIdentityGeneration;
}

/* A catalogue queue's tail: the source it pages from and where the next page
   starts. Plain, not reactive: it is read only from commands and from the
   App's low-water effect, which reads it untracked. `generation` counts
   choices of what plays; a catalogue start still on its way yields to any
   newer one. */
const lazyQueue = { generation: 0, source: null, cursor: null, retryAfter: 0, suspendedLease: null };
const QUEUE_BACKFILL_LOW_WATER = 8;
let lazyBackfillPromise = null;
const cataloguePageCache = new Map();
const CATALOGUE_PAGE_CACHE_MAX = 64;
const cataloguePagePending = new Map();
/* Counts the choices a skip still waiting to be sent yields to: every choice
   of what plays except another skip, and an account change (`skip`). */
let skipGeneration = 0;

function clearLazyQueue(keepSource = false) {
  lazyQueue.generation += 1;
  // Cursor choices cancel pending starts but keep the installed source's
  // single-flight backfill, so an already-submitted page cannot be duplicated.
  if (keepSource) return;
  lazyQueue.source = null;
  lazyQueue.cursor = null;
  lazyQueue.retryAfter = 0;
  lazyQueue.suspendedLease = null;
  lazyBackfillPromise = null;
}
/** A new choice of what plays. It supersedes a catalogue start still on its
    way and, unless it is itself a skip, the skips still waiting to be sent:
    those were aimed at the queue as it stood. */
function beginPlayback(keepLazySource = false, { skip = false } = {}) {
  clearLazyQueue(keepLazySource);
  if (!skip) skipGeneration += 1;
  playbackIdentityGeneration += 1;
  return lazyQueue.generation;
}


/**
 * Labels a row with the context it is about to play in.
 *
 * The engine adds this label to every row that arrives without one: `play_queue`,
 * `add_queue`, `add_queue_batch` and `restore_queue` all fill an empty context
 * from the command's own `context` argument (`fill_queue_context` in the
 * engine), so a row the caller never labelled does not need a copy carrying the
 * label. Building those copies was the whole cost of a click on a long list —
 * one fresh object per row. Only a row that already claims a different
 * context is rewritten here, because that is the one thing the engine will not
 * decide for it.
 */
function contextTrack(track, context) {
  const source = String(context ?? "").trim();
  if (!source || !track) return track;
  if (!track.context || track.context === source) return track;
  return { ...track, context: source };
}

/** The same rule as [`contextTrack`], for a list: an unlabelled list is handed
    over as it stands, and only a list holding a row that claims another
    context is rewritten row by row. */
function contextTracks(tracks, context) {
  const source = String(context ?? "").trim();
  if (!source) return tracks;
  const rows = tracks ?? [];
  if (!rows.some((track) => track?.context && track.context !== source)) return rows;
  return rows.map((track) => contextTrack(track, source));
}

function contextForQueueSource(source) {
  if (source?.kind === "catalogue" && source.id) return `artist:${source.id}`;
  return "";
}

async function startLazyQueue(tracks, source, cursor, index = 0, generation) {
  if (!tracks?.length) throw new Error("No playable tracks were returned.");
  const context = contextForQueueSource(source);
  await invoke("play_queue", {
    queue: contextTracks(tracks, context),
    index,
    context,
  });
  if (generation !== lazyQueue.generation) return;
  lazyQueue.source = cursor == null ? null : source;
  lazyQueue.cursor = cursor;
  // The queue event may have arrived before play_queue resolved, while the
  // App's low-water effect still saw no source. Installing it is a trigger too.
  backfillLazyQueue(false).catch(() => {});
}


function catalogueTracks(releases) {
  return (releases ?? []).flatMap((release) => release?.tracks ?? []);
}

export function loadCataloguePage(id, releaseTypes = ["albums", "singles"], offset = 0, limit = 4) {
  const epoch = searchSessionEpoch;
  const key = `${id}:${releaseTypes.join(",")}:${offset}:${limit}`;
  const cached = cataloguePageCache.get(key);
  if (cached) {
    // Pages contain complete track lists, so keep this in-memory cache bounded
    // while retaining recently visited catalogue and Appears On pages.
    cataloguePageCache.delete(key);
    cataloguePageCache.set(key, cached);
    return Promise.resolve(cached);
  }
  if (cataloguePagePending.has(key)) return cataloguePagePending.get(key);
  const request = api.browseArtistCatalogue(id, releaseTypes, offset, limit)
    .then((page) => {
      if (epoch !== searchSessionEpoch) throw new Error("The Spotify session changed.");
      cataloguePageCache.set(key, page);
      while (cataloguePageCache.size > CATALOGUE_PAGE_CACHE_MAX) {
        cataloguePageCache.delete(cataloguePageCache.keys().next().value);
      }
      return page;
    })
    .finally(() => {
      if (cataloguePagePending.get(key) === request) cataloguePagePending.delete(key);
    });
  cataloguePagePending.set(key, request);
  return request;
}

/** The catalogue start still on its way to the engine, or null: the choice
    it was made under and a promise that settles once its queue has been sent
    or it has given way. A Play/Pause pressed meanwhile is for the playback
    that starts, so it is sent after this (`requestPlaying`). */
let pendingStart = null;

export function playCatalogueContext(releases, id, releaseTypes, nextOffset, index) {
  const generation = beginPlayback();
  const pending = { generation, settled: null };
  pendingStart = pending;
  const start = (async () => {
    try {
      // Shuffling would mean enumerating the very catalogue this path
      // deliberately does not fetch, so this context is sequential.
      if (playback.shuffle) await invoke("set_shuffle", { enabled: false });
      if (generation !== lazyQueue.generation) return;
      await startLazyQueue(
        catalogueTracks(releases),
        { kind: "catalogue", id, releaseTypes: [...releaseTypes] },
        nextOffset,
        index,
        generation,
      );
    } finally {
      if (pendingStart === pending) pendingStart = null;
    }
  })();
  pending.settled = start.then(() => {}, () => {});
  return start;
}

async function backfillLazyQueue(force = false) {
  const source = lazyQueue.source;
  // Tail ownership follows the installed source, not subsequent cursor plays.
  const cursor = lazyQueue.cursor;
  if (!source || cursor == null || lazyQueue.suspendedLease !== null) return;
  if (!force && (performance.now() < lazyQueue.retryAfter || playback.current_index < 0 || playback.queue.length - playback.current_index > QUEUE_BACKFILL_LOW_WATER)) return;
  if (lazyBackfillPromise) return lazyBackfillPromise;
  const request = (async () => {
    try {
      const page = await loadCataloguePage(source.id, source.releaseTypes, cursor);
      if (source !== lazyQueue.source || lazyQueue.suspendedLease !== null) return;
      const tracks = catalogueTracks(page?.releases);
      if (tracks.length) {
        const context = contextForQueueSource(source);
        await invoke("add_queue_batch", {
          tracks: contextTracks(tracks, context),
          context,
        });
      }
      if (source !== lazyQueue.source) return;
      lazyQueue.retryAfter = 0;
      lazyQueue.cursor = page?.next_offset ?? null;
      if (lazyQueue.cursor == null) lazyQueue.source = null;
    } catch (error) {
      if (source === lazyQueue.source) lazyQueue.retryAfter = performance.now() + 5000;
      throw error;
    } finally {
      if (lazyBackfillPromise === request) lazyBackfillPromise = null;
    }
  })();
  lazyBackfillPromise = request;
  return request;
}

function resumeLazyQueue(source, previewLeaseId) {
  if (source !== lazyQueue.source || lazyQueue.suspendedLease?.id !== previewLeaseId) return;
  lazyQueue.suspendedLease = null;
  // A suspended read must finish releasing its slot before we resume its page.
  (lazyBackfillPromise ?? Promise.resolve()).then(() => {
    if (source === lazyQueue.source) return backfillLazyQueue(false);
  }).catch(() => {});
}

export function maybeBackfillLazyQueue() {
  return backfillLazyQueue(false);
}

/** `authPending` covers the invoke that binds the OAuth callback port; the
    Log in buttons disable on it so one click cannot become two flows. */
export const session = $state({ username: null, error: null, authPending: false });

/* ---------------- Playhead projection ---------------- */
/* The Rust side emits a full `state` only when something other than the
   playhead changed; a plain heartbeat arrives as a `position` number. Between
   syncs the playhead is projected here off a monotonic clock, so advancing the
   progress bar costs one number assignment instead of re-parsing the whole
   queue and rebuilding its Svelte proxies once a second. */

const playhead = $state({ base_ms: 0, at: 0, now: 0 });
let playheadTimer = null;
/* Whether anyone can see the playhead: false while the window is minimized or
   its document hidden. App.svelte owns the signal, because WebView2 can keep a
   natively minimized document "visible" and only the shell's frame state says
   otherwise (window-frame.svelte.js). */
let playheadSeen = true;

function startPlayheadTicker() {
  if (playheadTimer !== null) return;
  playhead.now = performance.now();
  playheadTimer = setInterval(() => {
    playhead.now = performance.now();
  }, 250);
}

function stopPlayheadTicker() {
  if (playheadTimer === null) return;
  clearInterval(playheadTimer);
  playheadTimer = null;
}

/** Reads the store rather than a caller's requested target: the ticker follows
    playback as a whole, so a play request that arrives while the engine is
    still loading cannot leave a 4 Hz invalidation running over a playhead that
    has nothing to advance from. Nor over one nobody can see. */
function syncPlayheadTicker() {
  if (playheadSeen && playback.playing && !playback.buffering) startPlayheadTicker();
  else stopPlayheadTicker();
}

/**
 * Stops the 4 Hz invalidation while the playhead cannot be seen and starts it
 * again when it can.
 *
 * Nothing is lost in between: the projection runs off the monotonic clock, not
 * off the tick, so `positionMs()` is exact at any moment. Coming back, the
 * playhead is re-anchored at that projected position before anything renders,
 * so the first frame shows the time it is now rather than the one the window
 * was hidden at — the tick alone would leave that stale time up for a quarter
 * of a second. A resync the shell sends on restore, a `position` or a full
 * `state`, then lands on a fresh anchor whichever order the two arrive in.
 */
export function setPlayheadVisible(visible) {
  const seen = !!visible;
  if (seen === playheadSeen) return;
  playheadSeen = seen;
  if (seen) anchorPlayhead(positionMs());
  syncPlayheadTicker();
}

/** Pins the playhead to `ms` as of right now; projection restarts from here. */
function anchorPlayhead(ms) {
  const t = performance.now();
  playhead.base_ms = ms;
  playhead.at = t;
  playhead.now = t;
}

/** The playhead in ms, projected forward from the last engine sync. */
export function positionMs() {
  // Keep the 250 ms ticker as a reactive invalidation source for ordinary
  // progress UI, but use the monotonic clock directly for callers that sample
  // the projection at display refresh cadence (the editor preview marker).
  const now = Math.max(playhead.now, performance.now());
  const speed = Number(playback.audible_playback_speed);
  const playbackSpeed = Number.isFinite(speed) && speed > 0 ? speed : 1;
  const elapsed = playback.playing && !playback.buffering ? Math.max(0, now - playhead.at) * playbackSpeed : 0;
  const projected = playhead.base_ms + elapsed;
  return playback.duration_ms > 0
    ? Math.min(projected, playback.duration_ms)
    : projected;
}

/**
 * The queue generation `playback.queue` holds, or null while it holds rows
 * whose generation nothing has named. The engine sends the rows with the
 * revision that identifies them and omits both when the revision has not
 * moved, which is how a state event can be read as "these rows did not
 * change" instead of "the queue is empty".
 */
let queueRevision = null;
let orderRevision = null;

/**
 * The generation a payload names, or null when it names none.
 *
 * A payload that leaves `queue_revision` out parses as `NaN` here, and the
 * field's own default is `0` — the value a pre-revision engine and every
 * snapshot written before this field existed carry (see
 * `PlaybackState::queue_revision` in the shell, where `0` means "assume
 * changed"). Neither can be a generation to compare against: two payloads that
 * both arrive that way describe unrelated queues, so both read as "nothing
 * named" and the rows the payload carries are adopted instead of being merged
 * against the rows already held under a number that names no rows in
 * particular.
 */
function namedQueueRevision(value) {
  const revision = Number(value);
  return Number.isSafeInteger(revision) && revision > 0 ? revision : null;
}

export function applyPlayback(payload) {
  if (!payload) return;
  if (("output_device_id" in payload && payload.output_device_id !== playback.output_device_id)
    || ("current_index" in payload && payload.current_index !== playback.current_index)
    || ("current_uri" in payload && payload.current_uri !== playback.current_uri)
    || ("context" in payload && payload.context !== playback.context)) playbackIdentityGeneration += 1;
  if ("position_ms" in payload || "playing" in payload || "buffering" in payload
    || "current_uri" in payload || "current_index" in payload
    || "output_device_id" in payload || "audible_playback_speed" in payload) {
    seekAuthorityGeneration += 1;
  }
  // The requested control value can change before queued audio reaches that
  // rate. Only an audible boundary changes the clock; a partial rate update
  // without a position still preserves the old-rate projection at the seam.
  const rateBoundaryPosition =
    !("position_ms" in payload) &&
    "audible_playback_speed" in payload &&
    payload.audible_playback_speed !== playback.audible_playback_speed
      ? positionMs()
      : null;
  if ("playing" in payload) playingAuthorityGeneration += 1;
  if ("volume" in payload) {
    volumeAuthorityGeneration += 1;
    confirmedVolume = payload.volume;
  }
  const loggedOut = observeSearchSession(payload);
  /* The queue is adopted below rather than copied with the scalars: rows that
     the revision says have not moved must keep their identity, because every
     consumer of a queue row — the player bar's container lookup, the plan
     derived from it, the queue view's rows — is keyed on the row object, and a
     fresh array of equal rows is a fresh set of objects to all of them. */
  if ("output_device_id" in payload && payload.output_device_id !== playback.output_device_id) {
    queueRevision = null;
    orderRevision = null;
  }
  const incomingQueue = "queue" in payload ? payload.queue : null;
  const incomingRevision = namedQueueRevision(payload.queue_revision);
  const queueHeld =
    incomingQueue === null ||
    (queueRevision !== null && incomingRevision === queueRevision);
  const incomingOrder = "upcoming" in payload ? payload.upcoming : null;
  const incomingOrderRevision = namedQueueRevision(payload.order_revision);
  const orderHeld = incomingOrder === null ||
    (orderRevision !== null && incomingOrderRevision === orderRevision);
  for (const key of Object.keys(payload)) {
    if (!PLAYBACK_FIELDS.has(key)) continue;
    // A full state for an older drag position must not undo the live intent.
    if (key === "volume" && volumePendingGeneration !== null) continue;
    playback[key] = payload[key];
  }
  if (loggedOut) {
    playback.username = null;
    session.username = null;
  } else if (payload.username != null) {
    // A ready startup snapshot may be the only identity message after reload.
    // Event wiring already protects it from overwriting a newer session.
    session.username = payload.username;
  }
  if ("position_ms" in payload) anchorPlayhead(payload.position_ms);
  else if (rateBoundaryPosition !== null) anchorPlayhead(rateBoundaryPosition);
  /* Unconditional, not "only when the advancing flag changed": a payload that
     reports no change then repairs any ticker that drifted, at the price of a
     null check, and no caller has to remember the `buffering` term. */
  syncPlayheadTicker();
  if (!queueHeld) {
    // Queue rows remain authoritative when a new generation is adopted.
    playback.queue = incomingQueue;
    /* Only a generation the payload actually named is remembered. One that
       named none is a payload from before this contract (or from a harness):
       its rows are adopted as they arrive, and the next payload that does name
       a generation starts the comparison afresh rather than matching against a
       number that stands for nothing. */
    queueRevision = incomingRevision;
  }
  if (!orderHeld) {
    playback.upcoming = incomingOrder;
    orderRevision = incomingOrderRevision;
  }
  if (payload.cache_marks_reset === true) propagateCacheMarks(null);
  if (Array.isArray(payload.uncached_ids) && payload.uncached_ids.length) {
    propagateCacheMarks(new Set(payload.uncached_ids), false);
  }
  let cachedIds = Array.isArray(payload.cached_ids) && payload.cached_ids.length
    ? new Set(payload.cached_ids) : null;
  if (!queueHeld) {
    for (const track of incomingQueue) {
      if (track.cached && track.id) (cachedIds ??= new Set()).add(track.id);
    }
  }
  if (cachedIds) propagateCacheMarks(cachedIds);
  maybeStartDeferredSearch();
}

/** Cache deltas update rows and late reads without replacing their identity. */
const cacheMarkListeners = new Set();
let cacheMarksVersion = 0;

/**
 * The one notification for download marks.
 *
 * A mark is written into the row object itself (`applyCacheMarks`), in place,
 * so a row keeps its identity and a mark that lands mid-scroll does not hand
 * the table a new list. Most rows are plain objects, though: the long lists
 * live in `$state.raw` so IPC and iteration see plain rows, and a write to a
 * plain `row.cached` notifies nobody. So every reader of a mark goes through
 * `isCached`, and each propagation moves this one signal: the marks on screen
 * re-read their row, and nothing else in the app is woken. It mirrors
 * `cacheMarksVersion` but is a separate value on purpose: reads compare
 * against that plain counter, and an effect that starts a read must never
 * come to depend on marks.
 */
let cacheMarkTick = $state(0);

export function isCached(track) {
  cacheMarkTick;
  return !!track?.cached;
}
const pendingCacheReads = new Set();
const CACHE_PAYLOAD_KEYS = ["tracks", "top_tracks", "queue", "track", "releases", "episodes", "entries", "playback"];

function markPayload(value, pending) {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const row of value) markPayload(row, pending);
    return;
  }
  if (value.uri?.startsWith("spotify:track:") || value.uri?.startsWith("spotify:episode:")) {
    if (pending.reset || pending.marks.has(value.id)) value.cached = pending.marks.get(value.id) ?? false;
  }
  if (Array.isArray(value.cached_ids)) {
    value.cached_ids = value.cached_ids.filter((id) =>
      pending.marks.get(id) !== false && (!pending.reset || pending.marks.get(id) === true));
  }
  for (const key of CACHE_PAYLOAD_KEYS) {
    if (value[key]) markPayload(value[key], pending);
  }
}

function sessionInvoke(command, args) {
  const epoch = searchSessionEpoch;
  return invoke(command, args).then((value) => {
    if (epoch !== searchSessionEpoch) throw new Error("The Spotify session changed.");
    return value;
  });
}

function cacheMarkedRead(command, args) {
  const generation = dataReadGeneration;
  const pending = { version: cacheMarksVersion, reset: false, marks: new Map() };
  pendingCacheReads.add(pending);
  return sessionInvoke(command, args).then((value) => {
    if (generation !== dataReadGeneration) throw new Error("Spotify disconnected while reading.");
    if (pending.version !== cacheMarksVersion) markPayload(value, pending);
    return value;
  }).finally(() => pendingCacheReads.delete(pending));
}

export function watchCacheMarks(listener) {
  cacheMarkListeners.add(listener);
  return () => cacheMarkListeners.delete(listener);
}

function propagateCacheMarks(ids, cached = true) {
  cacheMarksVersion++;
  for (const pending of pendingCacheReads) {
    if (ids === null) {
      pending.reset = true;
      pending.marks.clear();
    } else {
      for (const id of ids) pending.marks.set(id, cached);
    }
  }
  applyCacheMarks(playback.queue, ids, cached);
  for (const view of [detail.playlist, detail.album, detail.artist, detail.radio]) {
    applyCacheMarks(view?.tracks, ids, cached);
    applyCacheMarks(view?.top_tracks, ids, cached);
  }
  applyCacheMarks(search.results?.tracks, ids, cached);
  // Kept pages are shown again as they are, so they carry the marks too.
  for (const page of cataloguePageCache.values()) {
    for (const release of page?.releases ?? []) applyCacheMarks(release?.tracks, ids, cached);
  }
  applyCacheMarks(likedFirstPage?.page?.tracks, ids, cached);
  for (const page of playlistRecommendationsCache.values()) applyCacheMarks(page.tracks, ids, cached);
  for (const listener of cacheMarkListeners) listener(ids, cached);
  // After the listeners: they patch their own rows in place, and this is
  // what tells the screen any of those rows moved. Written from the plain
  // counter rather than incremented, so a propagation that happens to run
  // inside an effect never reads, and so never subscribes to, its own signal.
  cacheMarkTick = cacheMarksVersion;
}

export function applySession(payload) {
  if (!payload) return;
  const loggedOut = observeSearchSession(payload);
  if (payload.auth_state != null) {
    // Auth identity lives once, on `playback`: every reader needs the same
    // value and two copies could disagree mid-transition.
    playback.auth_state = payload.auth_state;
  }
  if (loggedOut) {
    session.username = null;
    playback.username = null;
  } else if (payload.username != null) {
    session.username = payload.username;
    playback.username = payload.username;
  }
  if (Object.prototype.hasOwnProperty.call(payload, "error")) session.error = payload.error;
  maybeStartDeferredSearch();
}


export function isLoggedOut() {
  // The engine emits `needs_login` (with a fresh auth_url) when no session
  // exists or the last connect attempt failed; `logged_out` is kept for
  // compatibility with older engines. Both must show the LoginView.
  return ["logged_out", "needs_login"].includes(playback.auth_state);
}

/* ---------------- Browse data ---------------- */

export const library = $state([]);
/**
 * Whether the library has been ANSWERED, as distinct from being empty.
 *
 * An empty array means both "still on its way" and "you have no playlists",
 * and those want opposite treatments: a frame of the rows that are coming, or
 * a sentence explaining that there are none. Without this flag every cold
 * start showed "No playlists yet" for the length of the round trip, which is
 * an error message for a state that is not an error.
 *
 * `fresh` is the narrower claim: the current rows are the authoritative
 * rootlist answer, not the cached disk snapshot that hydrates the first
 * paint. Only the backend `library` event promotes it — and so does a
 * completed play, whose recency this client just made authoritative itself.
 * The cached `get_state` pull hydrates without promoting, and an actual
 * logout or account switch demotes it until the next account's own event.
 * Home's provisional shelves wait on `fresh`, so a stale snapshot can never
 * mount listening-history rows that the fresh answer immediately reshuffles.
 */
export const libraryState = $state({ loaded: false, fresh: false, tree: [] });
let libraryAuthorityGeneration = 0;

// Only missing, visible rows enter this queue. Headers reuse the engine's
// account-scoped profile metadata; the library remains the sole row store.
const libraryCoverRequests = new Map();
let libraryCoverQueue = [];
let libraryCoverRunning = false;

export function sessionEpoch() {
  return searchSessionEpoch;
}

export function hydrateLibraryCovers(ids) {
  if (!isSearchReady()) return Promise.resolve([]);
  const epoch = searchSessionEpoch;
  const wanted = new Set(ids);
  const requests = [];
  for (const row of library) {
    if (!wanted.has(row.id) || row.cover_url || row.cover_urls?.length) continue;
    let request = libraryCoverRequests.get(row.id);
    if (request?.epoch !== epoch || performance.now() - request.answeredAt >= 300_000) {
      let resolve, reject;
      const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
      request = { id: row.id, epoch, promise, resolve, reject, answeredAt: Infinity };
      libraryCoverRequests.set(row.id, request);
      libraryCoverQueue.push(request);
    }
    requests.push(request.promise);
  }
  if (!libraryCoverRunning && libraryCoverQueue.length) {
    libraryCoverRunning = true;
    queueMicrotask(drainLibraryCovers);
  }
  return Promise.all(requests);
}

async function drainLibraryCovers() {
  try {
    while (libraryCoverQueue.length) {
      const batch = libraryCoverQueue.splice(0, 32);
      const epoch = batch[0].epoch;
      if (epoch !== searchSessionEpoch) {
        for (const request of batch) request.resolve(null);
        continue;
      }
      try {
        const summaries = await invoke("hydrate_library_covers", { ids: batch.map((request) => request.id) });
        if (epoch === searchSessionEpoch) {
          const byId = new Map((summaries ?? []).map((summary) => [summary.id, summary]));
          for (const row of library) {
            const summary = byId.get(row.id);
            if (!summary) continue;
            if (!row.cover_url && summary.cover_url) row.cover_url = summary.cover_url;
            if (!row.cover_urls?.length && summary.cover_urls?.length) row.cover_urls = summary.cover_urls;
          }
        }
        for (const request of batch) {
          request.answeredAt = performance.now();
          request.resolve(null);
        }
      } catch (error) {
        for (const request of batch) {
          if (libraryCoverRequests.get(request.id) === request) libraryCoverRequests.delete(request.id);
          request.reject(error);
        }
      }
    }
  } finally {
    libraryCoverRunning = false;
  }
}
/**
 * The payload behind the current detail route, plus why there isn't one.
 *
 * `error` is not decoration. Every detail view renders a skeleton while its
 * payload is null, and the browse commands used to swallow their rejections —
 * so an engine that was down left a playlist page showing placeholder rows
 * for ever, with nothing on screen to say the request had failed or any way
 * to try it again. A missing payload has two causes and they need two
 * different screens.
 *
 * The payloads are raw: each is an immutable snapshot that a writer REPLACES
 * (a refresh, a reorder) and never edits in place. A deep proxy would wrap
 * every row of a 10,000-song playlist, and Play hands that list to IPC:
 * serializing through the proxies took well over 100 ms for 10k rows, against
 * under 10 ms for the same plain rows. Download marks are the one exception to
 * "never edited in place", and they announce themselves through `isCached`.
 */
class DetailPayloads {
  playlist = $state.raw(null);
  radio = $state.raw(null);
  album = $state.raw(null);
  artist = $state.raw(null);
  error = $state("");
}
export const detail = new DetailPayloads();

let browseSeq = 0;

/**
 * Fetches the payload for a detail route. Lives here rather than in App so
 * that a view's "Try again" is the same call the navigation made, rather than
 * a second implementation of it.
 *
 * The sequence guard is what stops a slow answer for a page you have already
 * left from painting over the page you are on.
 */
export function loadDetail(name = route.name, id = route.id) {
  const seq = ++browseSeq;
  const epoch = searchSessionEpoch;
  if (!id) return;
  // The artist page and its auxiliary/discography views share one payload;
  // arriving at any of them with it already loaded must not refetch.
  if (ARTIST_ROUTES.has(name) && detail.artist?.id === id) return;
  detail.error = "";
  const settle = (key) => (payload) => {
    if (seq !== browseSeq || epoch !== searchSessionEpoch) return;
    // A cached open starts its engine refresh before this command response is
    // delivered. If that fresher event won the race, never replace it with the
    // stale disk snapshot that arrived later.
    if (key === "playlist" && detail.playlist?.id === id) return;
    detail[key] = payload ?? null;
  };
  const fail = (reason) => {
    if (seq === browseSeq && epoch === searchSessionEpoch) detail.error = String(reason || "Nothing came back from the engine.");
  };
  if (name === "playlist") api.browsePlaylist(id).then(settle("playlist")).catch(fail);
  else if (name === "radio") api.browseRadio(id).then(settle("radio")).catch(fail);
  else if (name === "album") api.browseAlbum(id).then(settle("album")).catch(fail);
  else if (ARTIST_ROUTES.has(name)) {
    api.browseArtist(id).then(settle("artist")).catch(fail);
  }
}

/** The same request the route made, for a failed page's one action. */
export function retryDetail() {
  loadDetail(route.name, route.id);
}

/* ---------------- Followed artists ---------------- */

/**
 * The artists this account follows, as the library rail's other list.
 *
 * Collection reads use the playback session. Optional artist follow writes
 * use the user-owned personal Web API grant; without one this stays read-only.
 *
 * Nothing is fetched until the rail is actually switched to artists. That is
 * the whole point of the rail's design — Following costs nothing at rest —
 * and it costs nothing here either. `loaded` rather than `artists.length` is
 * the "has this been asked" flag, because an account following nobody is a
 * real answer and must not re-request on every switch.
 */
export const followed = $state({ artists: [], loaded: false, loading: false, error: "" });

let followedGeneration = 0;

/**
 * Loads the followed artists, once per session unless forced.
 *
 * Not cached to disk anywhere: following is mutable from every other Spotify
 * client, so a list restored from last week is a list of people you may no
 * longer follow, and the round trip is one request nobody pays for unless
 * they open the switch.
 */
export function loadFollowedArtists({ force = false } = {}) {
  if (followed.loading) return;
  /* A failure is an answer too, and it stops the asking. The rail calls this
     from an effect that watches these very flags, so without the `error` term
     a refused read would set `error`, wake the effect, and be asked again
     immediately — a request loop against a service that had just said no.
     Only Try again, or a new account, asks a second time. */
  if (!force && (followed.loaded || followed.error)) return;
  const generation = ++followedGeneration;
  followed.loading = true;
  followed.error = "";
  api
    .browseFollowedArtists()
    .then((artists) => {
      if (generation !== followedGeneration) return;
      followed.artists = Array.isArray(artists) ? artists.filter((entry) => entry?.id) : [];
      followed.loaded = true;
    })
    .catch((reason) => {
      if (generation !== followedGeneration) return;
      followed.error = String(reason || "Could not load your followed artists.");
    })
    .finally(() => {
      if (generation === followedGeneration) followed.loading = false;
    });
}


export const search = $state({ query: "", results: null, submitted: false, busy: false, error: null, link: null });

/**
 * Which of the user's own containers hold the playing track — Liked Songs
 * included, as the id "liked". One in-memory IPC per track change: the index
 * lives in the Rust side and is kept fresh there by playlist fetches and a
 * background reconciliation, so nothing here polls or waits on the network.
 * The player bar drives the lookup (it is always mounted); the now-playing
 * panel reads the same answer instead of asking again.
 */
export const nowSaved = $state({ refs: [] });
let savedSeq = 0;

/** Hide the saved mark and fence replies already in flight, without an IPC. */
export function invalidateSavedIn() {
  savedSeq += 1;
  nowSaved.refs = [];
}

export async function lookupSavedIn(uri) {
  const seq = ++savedSeq;
  if (!uri?.startsWith("spotify:track:")) {
    nowSaved.refs = [];
    return;
  }
  try {
    const refs = await api.getTrackPlaylists(uri);
    if (seq === savedSeq) nowSaved.refs = refs ?? [];
  } catch {
    // Backend not ready or gone: no mark is safer than a wrong mark.
    if (seq === savedSeq) nowSaved.refs = [];
  }
}

/**
 * On-demand track credits surface state. The payload is kept as the backend
 * returns it (`TrackCreditsDetail`): groups retain their source-provided
 * headings, contributors retain every returned name and subrole, and a
 * contributor URLs are opened verbatim and never constructed from artist ids.
 */
export const credits = $state({
  open: false,
  loading: false,
  track: null,
  data: null,
  error: null,
});
let creditsSeq = 0;

/**
 * Credits shown *inside* the now-playing panel, as opposed to the modal.
 *
 * Separate state, one shared cache. The panel is an opt-in inspector, so this
 * only ever fetches while it is open, and each track is fetched at most once
 * per session; opening the full dialog for a track the panel already loaded
 * then paints from the cache with no second request.
 */
export const trackCredits = $state({ id: null, loading: false, data: null, error: null });
const creditsCache = new Map();
const creditsPending = new Map();
const CREDITS_CACHE_MAX = 128;

function keepCredits(id, data) {
  creditsCache.delete(id);
  creditsCache.set(id, data);
  if (creditsCache.size > CREDITS_CACHE_MAX) {
    creditsCache.delete(creditsCache.keys().next().value);
  }
}
let panelCreditsSeq = 0;

function readTrackCredits(id) {
  const existing = creditsPending.get(id);
  if (existing) return existing;
  const epoch = searchSessionEpoch;
  const request = api.browseTrackCredits(id).then((data) => {
    if (epoch !== searchSessionEpoch) throw new Error("The Spotify session changed.");
    keepCredits(id, data ?? null);
    return data ?? null;
  }).finally(() => {
    if (creditsPending.get(id) === request) creditsPending.delete(id);
  });
  creditsPending.set(id, request);
  return request;
}

export function loadTrackCredits(track) {
  const id = track?.id?.trim?.() ?? "";
  if (!id) {
    panelCreditsSeq += 1;
    trackCredits.id = null;
    trackCredits.loading = false;
    trackCredits.data = null;
    trackCredits.error = null;
    return;
  }
  if (trackCredits.id === id && (trackCredits.data || trackCredits.loading)) return;
  panelCreditsSeq += 1;
  const seq = panelCreditsSeq;
  trackCredits.id = id;
  trackCredits.error = null;
  if (creditsCache.has(id)) {
    trackCredits.data = creditsCache.get(id);
    keepCredits(id, trackCredits.data);
    trackCredits.loading = false;
    return;
  }
  trackCredits.data = null;
  trackCredits.loading = true;
  readTrackCredits(id)
    .then((data) => {
      if (seq !== panelCreditsSeq) return;
      trackCredits.data = data ?? null;
      trackCredits.loading = false;
    })
    .catch((error) => {
      if (seq !== panelCreditsSeq) return;
      trackCredits.loading = false;
      trackCredits.error = String(error || "Could not load credits.");
    });
}


/** See the note on `api.search`; this is the single knob that moves latency. */
export const SEARCH_LIMIT = 10;

/**
 * Debounce before a keystroke becomes a request. Every search costs at least
 * the ~580ms server floor no matter how small the limit, so firing per
 * keystroke would queue requests faster than they return. Long enough to skip
 * the middle of a word, short enough that results feel like they are keeping up.
 */
const SEARCH_DEBOUNCE_MS = 220;

const SEARCH_WAITING_MESSAGE = "Search is waiting for Spotify to connect.";
const SEARCH_FAILURE_MESSAGE = "Search could not load. Try again.";

let searchTimer = null;
let searchSeq = 0;
let searchSessionEpoch = 0;
let dataReadGeneration = 0;
let currentSearch = null;
let deferredSearch = null;
const searchRequests = new Map();
const LOGGED_OUT_AUTH_STATES = new Set(["logged_out", "needs_login"]);
let observedSearchSession = {
  known: false,
  authState: null,
  username: "",
  loggedOut: false,
};

function searchRequestKey(epoch, seq) {
  return `${epoch}:${seq}`;
}

/**
 * A link's identity, independent of how its text is spelled: Spotify's own
 * share text carries a `?si=` tail, and typing or pasting that tail re-parses
 * to the same target. Null for anything that is not an openable link.
 */
function linkKey(link) {
  return link?.kind ? `${link.kind}:${link.id}` : null;
}

function resetSearchForSession() {
  clearTimeout(searchTimer);
  searchTimer = null;
  searchSeq += 1;
  currentSearch = null;
  deferredSearch = null;
  searchRequests.clear();
  search.query = "";
  search.results = null;
  search.submitted = false;
  search.busy = false;
  search.error = null;
  search.link = null;
}

/**
 * Session events arrive separately from playback state events. Observe both,
 * but only invalidate once for a real logout or an identity change. The first
 * username after startup is not a transition: a query typed while the engine
 * was still authenticating must survive until that first session becomes ready.
 */
function observeSearchSession(payload) {
  if (!payload) return false;
  const hasAuthState = Object.prototype.hasOwnProperty.call(payload, "auth_state");
  const hasUsername = Object.prototype.hasOwnProperty.call(payload, "username");
  const authState = hasAuthState ? payload.auth_state ?? null : observedSearchSession.authState;
  const rawUsername = hasUsername
    ? String(payload.username ?? "").trim()
    : observedSearchSession.username;
  // A partial session event carrying the first username is itself evidence
  // that authentication completed; do not let a prior needs_login snapshot
  // erase the deferred query before the matching ready event arrives.
  const loggedOut = hasAuthState
    ? LOGGED_OUT_AUTH_STATES.has(authState)
    : hasUsername
      ? !rawUsername && !!observedSearchSession.username
      : observedSearchSession.loggedOut;
  // Temporary disconnect snapshots can omit identity; remember it until a
  // real logout so reconnecting as another account is still a transition.
  const username = loggedOut ? "" : rawUsername || observedSearchSession.username;
  const previous = observedSearchSession;

  if (previous.known) {
    if (previous.authState === "ready" && hasAuthState && authState !== "ready") {
      dataReadGeneration += 1;
      browseSeq += 1;
      likedFirstPagePending = null;
      cataloguePagePending.clear();
      for (const [key, entry] of playlistRecommendationsCache) {
        if (entry.promise) playlistRecommendationsCache.delete(key);
      }
    }
    const actualLogout =
      loggedOut &&
      !previous.loggedOut &&
      (!!previous.username || previous.authState === "ready");
    const accountTransition =
      !!username &&
      !!previous.username &&
      username !== previous.username;
    if (actualLogout || accountTransition) {
      searchSessionEpoch += 1;
      playbackIdentityGeneration += 1;
      resetSearchForSession();
      resetPersonalizedDiscoveryForSession();
      invalidateLikedFirstPage();
      canvasLookups.forget();
      libraryCoverRequests.clear();
      for (const request of libraryCoverQueue) request.resolve(null);
      libraryCoverQueue = [];
      followedGeneration++;
      followed.artists = [];
      followed.loaded = false;
      followed.loading = false;
      followed.error = "";
      invalidateSavedIn();
      clearLazyQueue();
      skipGeneration += 1;
      cataloguePageCache.clear();
      cataloguePagePending.clear();
      clearPlaylistRecommendationsCache();
      creditsCache.clear();
      creditsPending.clear();
      panelCreditsSeq += 1;
      Object.assign(trackCredits, { id: null, loading: false, data: null, error: null });
      closeCredits();
      browseSeq += 1;
      Object.assign(detail, { playlist: null, radio: null, album: null, artist: null, error: "" });
      library.length = 0;
      libraryState.loaded = false;
      libraryState.fresh = false;
      libraryState.tree = [];
      treeEditTail = null;
      history.entries = [{ name: "library", id: null, param: null }];
      history.cursor = 0;
      applyEntry(history.entries[0]);
    }
  }

  observedSearchSession = { known: true, authState, username, loggedOut };
  if (authState === "ready" && previous.authState !== "ready" && route.id) {
    const key = ARTIST_ROUTES.has(route.name) ? "artist" : route.name;
    if (key in detail && !detail[key]) {
      const epoch = searchSessionEpoch;
      queueMicrotask(() => {
        if (epoch === searchSessionEpoch && playback.auth_state === "ready") loadDetail();
      });
    }
  }
  return loggedOut;
}

function isSearchReady() {
  return playback.ready === true && playback.auth_state === "ready";
}

function searchErrorMessage(reason) {
  if (typeof reason === "string" && reason.trim()) return reason;
  if (reason?.message) return reason.message;
  return SEARCH_FAILURE_MESSAGE;
}

function searchIsCurrent(query, seq, epoch) {
  return (
    currentSearch?.q === query &&
    currentSearch?.seq === seq &&
    currentSearch?.epoch === epoch &&
    epoch === searchSessionEpoch
  );
}

function requestIsCurrent(request) {
  return (
    request.epoch === searchSessionEpoch &&
    searchIsCurrent(request.q, request.seq, request.epoch) &&
    searchRequests.get(request.key) === request
  );
}

function runSearch(query, seq, epoch) {
  if (!searchIsCurrent(query, seq, epoch)) return;
  if (!isSearchReady()) {
    deferredSearch = { q: query, seq, epoch };
    search.busy = false;
    search.error = SEARCH_WAITING_MESSAGE;
    return;
  }

  const key = searchRequestKey(epoch, seq);
  const existing = searchRequests.get(key);
  if (existing) {
    // A readiness event or an explicit submit can arrive more than once. Keep
    // the one request and leave any deferred token intact until it settles.
    search.busy = true;
    search.error = null;
    return;
  }

  const request = { q: query, seq, epoch, key };
  searchRequests.set(key, request);
  search.busy = true;
  search.error = null;

  let promise;
  try {
    const link = parseSpotifyLink(query);
    promise = link?.kind === "track"
      ? api.browseTrack(link.id).then((track) => {
        if (track?.id !== link.id || track.uri !== `spotify:track:${link.id}`) {
          throw new Error("Spotify couldn't resolve this song. Check the link and try again.");
        }
        return { tracks: [track], albums: [], artists: [], playlists: [], top: { ...track, kind: "track" } };
      })
      : api.search(query);
  } catch (reason) {
    promise = Promise.reject(reason);
  }

  Promise.resolve(promise)
    .then((result) => {
      if (!requestIsCurrent(request)) return;
      if (result == null) {
        search.error = SEARCH_FAILURE_MESSAGE;
        return;
      }
      search.results = result;
      search.error = null;
    })
    .catch((reason) => {
      if (!requestIsCurrent(request)) return;
      if (!isSearchReady()) {
        deferredSearch = { q: query, seq, epoch };
        search.error = SEARCH_WAITING_MESSAGE;
      } else {
        search.error = searchErrorMessage(reason);
      }
    })
    .finally(() => {
      if (searchRequests.get(request.key) === request) searchRequests.delete(request.key);
      if (searchIsCurrent(request.q, request.seq, request.epoch)) {
        search.busy = false;
      }
      // If readiness returned while this request was still cleaning up,
      // retry only after the old entry has been removed. This also keeps a
      // deferred q1 alive when q1→q2→q1 creates a fresh request token.
      if (isSearchReady()) maybeStartDeferredSearch();
    });
}

function maybeStartDeferredSearch() {
  const pending = deferredSearch;
  if (!pending) return;
  if (!searchIsCurrent(pending.q, pending.seq, pending.epoch)) {
    deferredSearch = null;
    return;
  }
  if (!isSearchReady()) return;
  // Keep the deferred token until the prior request's finally has removed its
  // epoch+sequence entry. Clearing it here would lose the readiness retry.
  if (searchRequests.has(searchRequestKey(pending.epoch, pending.seq))) return;
  deferredSearch = null;
  runSearch(pending.q, pending.seq, pending.epoch);
}

function enqueueSearch(query, force = false) {
  const q = String(query ?? "").trim();
  const link = parseSpotifyLink(q);
  search.link = link;
  const current = currentSearch;

  // Input events may call this for the same value more than once. A retry for
  // an errored query is the only same-value call that should start over.
  if (q && current?.q === q && current.epoch === searchSessionEpoch && !force) {
    const key = searchRequestKey(current.epoch, current.seq);
    if (
      searchTimer !== null ||
      (deferredSearch?.q === q && deferredSearch?.seq === current.seq) ||
      searchRequests.has(key)
    ) {
      return;
    }
    if (!search.error) return;
    force = true;
  }
  if (q && current?.q === q && current.epoch === searchSessionEpoch && force) {
    const key = searchRequestKey(current.epoch, current.seq);
    if (
      searchTimer !== null ||
      (deferredSearch?.q === q && deferredSearch?.seq === current.seq) ||
      searchRequests.has(key)
    ) {
      return;
    }
  }

  clearTimeout(searchTimer);
  searchTimer = null;
  if (!q) {
    searchSeq += 1;
    currentSearch = null;
    deferredSearch = null;
    search.submitted = false;
    search.results = null;
    search.busy = false;
    search.error = null;
    return;
  }

  const seq = ++searchSeq;
  const epoch = searchSessionEpoch;
  currentSearch = { q, seq, epoch, linkKey: linkKey(link) };
  deferredSearch = null;
  search.submitted = true;
  search.busy = true;
  search.error = null;
  /* `submitted` is set HERE rather than in the response handler. Otherwise
     the view checks `!submitted` first and the loading state is unreachable.
     Existing results deliberately stay visible while a refinement is pending. */
  searchTimer = setTimeout(() => {
    searchTimer = null;
    runSearch(q, seq, epoch);
  }, SEARCH_DEBOUNCE_MS);
}

/**
 * Starts `q` now instead of waiting out the debounce. Enter and a pasted link
 * are decisions, not keystrokes: both can arrive while the timer is still
 * counting, and neither may duplicate the call that timer was about to make.
 */
function runSearchNow(query) {
  const q = String(query ?? "").trim();
  if (
    !currentSearch ||
    currentSearch.q !== q ||
    currentSearch.epoch !== searchSessionEpoch
  ) {
    enqueueSearch(q);
  }
  clearTimeout(searchTimer);
  searchTimer = null;
  const current = currentSearch;
  if (!current) return;
  const key = searchRequestKey(current.epoch, current.seq);
  if (searchRequests.has(key)) {
    // A readiness event or an explicit submit can arrive more than once. Keep
    // the one request and leave any deferred token intact until it settles.
    search.busy = true;
    search.error = null;
    return;
  }
  runSearch(current.q, current.seq, current.epoch);
}

/**
 * Whether `key` is already this surface's answer: its request is in flight,
 * waiting for Spotify to connect, or settled on screen. A failed attempt is
 * deliberately not an answer, so Try again and a fresh paste run it once more.
 */
function linkAlreadyResolving(key) {
  const current = currentSearch;
  if (!current || current.linkKey !== key || current.epoch !== searchSessionEpoch) return false;
  if (searchRequests.has(searchRequestKey(current.epoch, current.seq))) return true;
  if (deferredSearch?.q === current.q && deferredSearch.seq === current.seq) return true;
  return !search.error;
}

/**
 * Opens a Spotify link, from a paste (`queueSearch`) or Enter (`submitSearch`).
 * A song resolves through `browse_track` and its one result replaces the page;
 * an album, artist or playlist link leaves the search surface for its own
 * route. Neither opens a stream: a link names a page, and the Play there is
 * still the only thing that starts playback.
 *
 * Idempotent by target, because an input event is not a user decision: the same
 * link re-detected — extended with a share tail, or echoed by a second input
 * event — must not resolve twice or push its route twice.
 */
function openLink(link, text) {
  if (link.error) {
    enqueueSearch("");
    search.link = link;
    search.error = link.error;
    search.submitted = true;
    return;
  }

  if (link.kind !== "track") {
    /* A route is its own record of being open: re-pasting the album that is
       already on screen has nothing left to do, while the same link pasted
       after visiting anywhere else still goes where it points. */
    if (route.name === link.kind && route.id === link.id) return;
    /* The surface is emptied rather than left holding the link: the route owns
       what that link shows now, and a heading still claiming a link is open
       above a page that has searched nothing is a worse record than none. The
       box is emptied with it — reaching for the box is what pushed `search`
       onto the history in the first place — so Back lands on an empty search
       page rather than on this link again. */
    enqueueSearch("");
    search.query = "";
    navigate(link.kind, link.id);
    return;
  }

  if (linkAlreadyResolving(linkKey(link))) return;
  /* The previous query's results answer a different question; the view shows
     its loading frame until `browse_track` lands. */
  search.results = null;
  runSearchNow(text);
}

export function queueSearch(query) {
  const link = parseSpotifyLink(query);
  if (link) {
    /* A paste opens the link itself — the same thing Enter does — because the
       state in between ("here is a link, open it?") asked the user to confirm
       an action they had already taken. */
    openLink(link, query);
    return;
  }
  if (search.link) search.results = null;
  enqueueSearch(query);
}

/** Submit skips the remaining debounce without duplicating an in-flight call. */
export function submitSearch(query) {
  const raw = String(query ?? "");
  search.query = raw;
  const q = raw.trim();
  const link = parseSpotifyLink(q);
  if (link) {
    openLink(link, q);
    return;
  }
  if (!q) {
    enqueueSearch("");
    return;
  }
  runSearchNow(q);
}

export function retrySearch() {
  const q = search.query.trim();
  if (search.link) submitSearch(q);
  else if (q) enqueueSearch(q, true);
}

/**
 * Opens credits only after an explicit row-menu action. A sequence token
 * prevents a slow response for a closed/replaced dialog from painting stale
 * contributors into the next track's surface.
 */
export function openCredits(track) {
  const id = track?.id?.trim?.() ?? "";
  creditsSeq += 1;
  const seq = creditsSeq;
  credits.open = true;
  credits.loading = true;
  credits.track = track ?? null;
  credits.data = null;
  credits.error = null;
  if (!id) {
    credits.loading = false;
    credits.error = "Credits are unavailable for this track.";
    return;
  }
  // Opened from the now-playing panel, the payload is usually already here.
  if (creditsCache.has(id)) {
    credits.data = creditsCache.get(id);
    keepCredits(id, credits.data);
    credits.loading = false;
    return;
  }
  readTrackCredits(id)
    .then((data) => {
      if (seq !== creditsSeq) return;
      credits.data = data ?? null;
      credits.loading = false;
    })
    .catch((error) => {
      if (seq !== creditsSeq) return;
      credits.loading = false;
      credits.error = String(error || "Could not load credits.");
    });
}

export function closeCredits() {
  creditsSeq += 1;
  credits.open = false;
  credits.loading = false;
  credits.track = null;
  credits.data = null;
  credits.error = null;
}


/* ------------------------------------------------------------------ */
/* Library activity                                                   */
/* ------------------------------------------------------------------ */

/**
 * Names Spotify uses for its algorithmic/personalized playlists. This is
 * deliberately local and explicit: Home must classify the already-loaded
 * rootlist without making a recommendation request or treating every
 * playlist as personalized by accident.
 */
const MADE_FOR_YOU_RULES = Object.freeze([
  ["discover-weekly", /^discover weekly$/],
  ["release-radar", /^release radar$/],
  ["daily-mix", /^daily mix \d+$/],
  ["on-repeat", /^on repeat$/],
  ["repeat-rewind", /^repeat rewind$/],
  ["time-capsule", /^time capsule$/],
  ["daylist", /\bdaylist\b/],
  ["your-top-songs", /^your top songs \d{4}$/],
]);

/**
 * Returns the canonical personalized-playlist kind, or `null` for an ordinary
 * library playlist. Callers pass either a Playlist object or a name string.
 */
export function classifyLibraryPlaylist(playlist) {
  const raw = typeof playlist === "string" ? playlist : playlist?.name;
  const name = String(raw ?? "").trim().replace(/\s+/g, " ").toLowerCase();
  if (!name) return null;
  return MADE_FOR_YOU_RULES.find(([, pattern]) => pattern.test(name))?.[0] ?? null;
}

export function isMadeForYouPlaylist(playlist) {
  return classifyLibraryPlaylist(playlist) !== null;
}
/**
 * Stable Home ordering for the personalized playlists that are already in the
 * authenticated rootlist. Spotify can add more named algorithmic playlists
 * over time, so known-but-unprioritized kinds remain visible after the three
 * shelves the Home surface promises.
 *
 * The return value is a sort key, not a display label. `null` means that the
 * playlist is not a known personalized item and should stay in the library
 * grid. Daily Mixes 1-6 get their own ordered band; later mixes are treated as
 * other personalized items rather than displacing the requested six.
 */
export function personalizedPlaylistRank(playlist) {
  const kind = classifyLibraryPlaylist(playlist);
  if (!kind) return null;
  if (kind === "release-radar") return 0;
  if (kind === "daily-mix") {
    const name = String(typeof playlist === "string" ? playlist : playlist?.name ?? "")
      .trim()
      .replace(/\s+/g, " ")
      .toLowerCase();
    const number = Number(name.match(/^daily mix (\d+)$/)?.[1]);
    if (Number.isInteger(number) && number >= 1 && number <= 6) return number;
    return 200 + (Number.isInteger(number) ? number : 99);
  }
  if (kind === "discover-weekly") return 100;
  return 300;
}

/** The DJ shelf is not a user playlist recommendation and never belongs on Home. */
export function isDjPlaylist(playlist) {
  const raw = typeof playlist === "string" ? playlist : playlist?.name;
  const name = String(raw ?? "").trim().replace(/\s+/g, " ").toLowerCase();
  return /^dj(?:\b|$)/.test(name);
}

/* ------------------------------------------------------------------ */
/* Personalized discovery                                             */
/* ------------------------------------------------------------------ */

/**
 * Home's recommendation band is deliberately assembled from the same bounded
 * search surface users can invoke themselves. The cache belongs to the
 * authenticated session, not to a component instance: leaving and returning
 * to Home must never fan out another set of searches.
 */
const PERSONALIZED_DISCOVERY_BACKOFF_MS = 5_000;
const PERSONALIZED_DISCOVERY_QUERIES = Object.freeze(["Daily Mix", "Release Radar"]);
const PERSONALIZED_SEARCH_LIMIT = 20;
const PERSONALIZED_FAILURE_MESSAGE = "Personalized playlists could not load.";

export const personalizedDiscovery = $state({
  playlists: [],
  status: "idle",
  error: "",
  retryAfter: 0,
  sessionEpoch: searchSessionEpoch,
});

let personalizedDiscoveryPromise = null;

function resetPersonalizedDiscoveryForSession() {
  personalizedDiscoveryPromise = null;
  personalizedDiscovery.playlists = [];
  personalizedDiscovery.status = "idle";
  personalizedDiscovery.error = "";
  personalizedDiscovery.retryAfter = 0;
  personalizedDiscovery.sessionEpoch = searchSessionEpoch;
}

function normalizedPlaylistName(value) {
  return String(value ?? "").trim().replace(/\s+/g, " ").toLowerCase();
}

function personalizedSearchName(name) {
  return name.includes("daily") ||
    name === "release radar" ||
    name === "discover weekly";
}

function playlistId(value) {
  const direct = String(value?.id ?? "").trim();
  if (direct) return direct;
  const uri = String(value?.uri ?? "").trim();
  return uri.match(/^spotify:playlist:([^:]+)$/)?.[1] ?? "";
}

function ownerValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function isSpotifyOwner(playlist) {
  const identities = [
    ["id", ownerValue(playlist?.owner_id)],
    ["name", ownerValue(playlist?.owner_name)],
    ["uri", ownerValue(playlist?.owner_uri)],
    ["display", ownerValue(playlist?.owner)],
  ].filter(([, value]) => value);
  if (!identities.length) return false;
  for (const [kind, raw] of identities) {
    const value = raw.toLowerCase().replace(/\s+/g, " ").trim();
    if (kind === "uri") {
      if (!/(?:^|:)spotify$/.test(value)) return false;
    } else if (!/^spotify(?:$|[\s_-])/.test(value)) {
      return false;
    }
  }
  return true;
}

function normalizeSearchPlaylist(value) {
  if (!value || typeof value !== "object") return null;
  const id = playlistId(value);
  const name = String(value.name ?? "").trim().replace(/\s+/g, " ");
  if (!id || !name || !personalizedSearchName(normalizedPlaylistName(name))) return null;
  if (!isSpotifyOwner(value)) return null;
  const ownerId = ownerValue(value.owner_id);
  const ownerName = ownerValue(value.owner_name);
  const owner = ownerValue(value.owner) || ownerName || ownerId;
  const coverUrl = ownerValue(value.cover_url);
  const coverUrls = Array.isArray(value.cover_urls)
    ? value.cover_urls.filter((url) => typeof url === "string" && url.trim())
    : [];
  const count = Number(value.tracks_total ?? value.track_count);
  return {
    ...value,
    id,
    uri: ownerValue(value.uri) || `spotify:playlist:${id}`,
    name,
    description: normalizeCanonicalPlaylistDescription(value.description),
    owner,
    owner_id: ownerId,
    cover_url: coverUrl,
    cover_urls: coverUrls,
    tracks_total: Number.isFinite(count) && count > 0 ? Math.round(count) : 0,
    last_played: value.last_played ?? null,
    last_activity: value.last_activity ?? null,
  };
}

function extractPersonalizedSearchPlaylists(result) {
  const values = Array.isArray(result?.playlists) ? result.playlists : [];
  return values.map(normalizeSearchPlaylist).filter(Boolean);
}

function dedupePlaylists(playlists) {
  const seen = new Set();
  const result = [];
  for (const playlist of playlists) {
    if (!playlist?.id || seen.has(playlist.id)) continue;
    seen.add(playlist.id);
    result.push(playlist);
  }
  return result;
}

function mergePlaylistMetadata(primary, supplement) {
  const primaryDescription = normalizeCanonicalPlaylistDescription(primary?.description);
  const supplementDescription = normalizeCanonicalPlaylistDescription(supplement?.description);
  const primaryOwner = ownerValue(primary?.owner);
  const primaryOwnerId = ownerValue(primary?.owner_id);
  const supplementOwner = ownerValue(supplement?.owner) || ownerValue(supplement?.owner_name);
  const primaryOwnerSparse = !primaryOwner ||
    (primaryOwnerId && primaryOwner.toLowerCase() === primaryOwnerId.toLowerCase());
  const owner = primaryOwnerSparse
    ? supplementOwner || ownerValue(primary?.owner_name) || primaryOwner
    : primaryOwner;
  return {
    ...supplement,
    ...primary,
    uri: primary.uri || supplement.uri,
    description: primaryDescription || supplementDescription,
    owner,
    owner_id: primary.owner_id || supplement.owner_id,
    cover_url: primary.cover_url || supplement.cover_url,
    cover_urls: primary.cover_urls?.length ? primary.cover_urls : supplement.cover_urls ?? [],
    tracks_total: Number(primary.tracks_total) > 0
      ? primary.tracks_total
      : supplement.tracks_total ?? 0,
  };
}

/**
 * Merges the authenticated rootlist with the session's search answer. Rootlist
 * metadata wins when it exists (it carries local activity), while search
 * artwork/owner/count fields fill the gaps in a rootlist reference.
 */
export function mergePersonalizedPlaylists(rootlist = library) {
  const byId = new Map();
  for (const playlist of rootlist ?? []) {
    if (
      !playlist?.id ||
      isDjPlaylist(playlist) ||
      !isMadeForYouPlaylist(playlist)
    ) {
      continue;
    }
    byId.set(playlist.id, playlist);
  }
  for (const playlist of personalizedDiscovery.playlists) {
    if (!playlist?.id || isDjPlaylist(playlist)) continue;
    const existing = byId.get(playlist.id);
    byId.set(
      playlist.id,
      existing ? mergePlaylistMetadata(existing, playlist) : playlist,
    );
  }
  return [...byId.values()].sort((left, right) => {
    const leftRank = personalizedPlaylistRank(left) ?? Number.MAX_SAFE_INTEGER;
    const rightRank = personalizedPlaylistRank(right) ?? Number.MAX_SAFE_INTEGER;
    if (leftRank !== rightRank) return leftRank - rightRank;
    return String(left.name ?? "").localeCompare(String(right.name ?? ""));
  });
}

/**
 * Starts the one Home discovery pass for the current account. Calls made
 * while this pass is running receive the same promise; a successful answer is
 * retained until the auth epoch changes. A failed pass keeps the rootlist
 * visible and can only be retried by a later Home visit after the backoff.
 */
export function ensurePersonalizedDiscovery(rootlist = library) {
  if (!isSearchReady()) return Promise.resolve(personalizedDiscovery.playlists);
  if (personalizedDiscovery.sessionEpoch !== searchSessionEpoch) {
    resetPersonalizedDiscoveryForSession();
  }
  if (personalizedDiscoveryPromise) return personalizedDiscoveryPromise;
  if (personalizedDiscovery.status === "ready") {
    return Promise.resolve(personalizedDiscovery.playlists);
  }
  if (
    personalizedDiscovery.status === "error" &&
    Date.now() < personalizedDiscovery.retryAfter
  ) {
    return Promise.resolve(personalizedDiscovery.playlists);
  }

  const epoch = searchSessionEpoch;
  const root = Array.isArray(rootlist) ? rootlist : [];
  const hasDiscoverWeekly = root.some(
    (playlist) => normalizedPlaylistName(playlist?.name) === "discover weekly",
  );
  const queries = hasDiscoverWeekly
    ? PERSONALIZED_DISCOVERY_QUERIES
    : [...PERSONALIZED_DISCOVERY_QUERIES, "Discover Weekly"];
  personalizedDiscovery.sessionEpoch = epoch;
  personalizedDiscovery.status = "loading";
  personalizedDiscovery.error = "";
  personalizedDiscovery.retryAfter = 0;

  const pass = Promise.allSettled(
    queries.map((query) =>
      Promise.resolve().then(() => api.search(query, PERSONALIZED_SEARCH_LIMIT)),
    ),
  ).then((settled) => {
    if (epoch !== searchSessionEpoch) return personalizedDiscovery.playlists;
    const found = [];
    let failures = 0;
    for (const result of settled) {
      if (result.status === "fulfilled") found.push(...extractPersonalizedSearchPlaylists(result.value));
      else failures += 1;
    }
    if (found.length) {
      personalizedDiscovery.playlists = dedupePlaylists(found);
    }
    if (failures) {
      personalizedDiscovery.status = "error";
      personalizedDiscovery.error = PERSONALIZED_FAILURE_MESSAGE;
      personalizedDiscovery.retryAfter = Date.now() + PERSONALIZED_DISCOVERY_BACKOFF_MS;
    } else {
      personalizedDiscovery.status = "ready";
      personalizedDiscovery.error = "";
      personalizedDiscovery.retryAfter = 0;
    }
    return personalizedDiscovery.playlists;
  }).finally(() => {
    if (personalizedDiscoveryPromise === pass) personalizedDiscoveryPromise = null;
  });
  personalizedDiscoveryPromise = pass;
  return pass;
}

function activityNow() {
  return Math.floor(Date.now() / 1000);
}

function activityValue(playlist) {
  const value = Number(playlist?.last_activity);
  return Number.isFinite(value) ? value : Number.NEGATIVE_INFINITY;
}

export function setLibrary(playlists, { fresh = false, tree } = {}) {
  libraryAuthorityGeneration += 1;
  if (tree !== undefined) libraryState.tree = tree;
  const ordered = [...(playlists ?? [])];
  ordered.sort((left, right) => {
    const a = activityValue(left);
    const b = activityValue(right);
    return a === b ? 0 : b - a;
  });
  library.length = 0;
  library.push(...ordered);
  const present = new Set(ordered.map((playlist) => playlist.id));
  for (const id of libraryCoverRequests.keys()) {
    if (!present.has(id)) libraryCoverRequests.delete(id);
  }
  // Any answer at all, including an empty one, ends the loading state.
  // Sidebar and all library-derived views now share activity order even when
  // the first payload came from an older on-disk snapshot.
  libraryState.loaded = true;
  // Freshness is opt-in: the cached get_state pull hydrates the grid through
  // this same door without promoting itself to authoritative rootlist data.
  if (fresh) libraryState.fresh = true;
}

/**
 * Keep rootlist folders, but let the activity-ordered library decide where
 * their contents sit. A folder moves with its most recent remaining child,
 * or with its own stamp when this app made it (a new folder comes first the
 * way a new playlist does, empty or not); explicitly pinned playlists are
 * lifted out once, never duplicated inside. Filtering is a flat,
 * activity-ordered result set rather than orphaned folder headings or
 * indentation with no parent. A collapsed folder keeps its heading (with the
 * number of playlists it holds) and hides what is under it; a filter looks
 * inside collapsed folders too. Each entry says which folder it sits in
 * (`parent`), which is what a drop onto it means.
 */
export function libraryRailEntries(playlists, tree, pinnedIds = [], filter = "", collapsedIds = []) {
  const byId = new Map(playlists.map((playlist) => [playlist.id, playlist]));
  const rank = new Map(playlists.map((playlist, index) => [playlist.id, index]));
  const pinned = new Set(pinnedIds);
  const collapsed = new Set(collapsedIds);
  const seen = new Set();
  const pinnedRows = [];
  const query = filter.trim().toLocaleLowerCase();
  const matches = (playlist) => !query || playlist.name?.toLocaleLowerCase().includes(query);
  /* A row's place: its activity, then its library position — the library is
     already activity order with the rootlist's behind it, so for playlists
     the position alone decides; the activity is what lets a folder's own
     stamp stand among them. */
  const before = (left, right) => {
    if (left.at !== right.at) return right.at > left.at ? 1 : -1;
    return left.rank === right.rank ? 0 : left.rank < right.rank ? -1 : 1;
  };

  function playlistRow(playlist) {
    const id = playlist?.id;
    if (!id || seen.has(id)) return null;
    seen.add(id);
    if (!matches(playlist)) return null;
    const row = { kind: "playlist", playlist, at: activityValue(playlist), rank: rank.get(id) ?? Infinity };
    if (pinned.has(id)) {
      pinnedRows.push(row);
      return null;
    }
    return row;
  }

  function collect(nodes) {
    const rows = [];
    for (const node of nodes) {
      if (node.kind === "folder") {
        const children = collect(node.children ?? []);
        // Do not leave empty headings behind a lifted pin. Truly empty folders
        // still belong to the rootlist and remain visible outside a filter.
        if (!children.length && (query || node.children?.length)) continue;
        const stamp = Number(node.last_activity);
        let folder = { kind: "folder", id: node.id, name: node.name, children, at: Number.isFinite(stamp) ? stamp : Number.NEGATIVE_INFINITY, rank: Infinity };
        const lead = children[0];
        if (lead && before(lead, folder) < 0) folder = { ...folder, at: lead.at, rank: lead.rank };
        rows.push(folder);
      } else if (node.kind === "playlist") {
        // The tree names a playlist; its row lives once, in the library.
        const row = playlistRow(byId.get(node.id));
        if (row) rows.push(row);
      }
    }
    rows.sort(before);
    return rows;
  }

  const roots = collect(Array.isArray(tree) ? tree : []);
  for (const playlist of playlists) {
    const row = playlistRow(playlist);
    if (row) roots.push(row);
  }
  roots.sort(before);
  pinnedRows.sort(before);
  const entries = pinnedRows.map(({ playlist }) => ({ kind: "playlist", playlist, depth: 0, parent: null }));
  const countOf = (rows) => rows.reduce((sum, row) => sum + (row.kind === "folder" ? countOf(row.children) : 1), 0);
  function flatten(rows, depth, parent) {
    for (const row of rows) {
      if (row.kind === "folder") {
        if (query) {
          flatten(row.children, 0, row.id);
          continue;
        }
        const shut = collapsed.has(row.id);
        entries.push({ kind: "folder", id: row.id, name: row.name, depth, parent, collapsed: shut, count: countOf(row.children) });
        if (!shut) flatten(row.children, depth + 1, row.id);
      } else {
        entries.push({ kind: "playlist", playlist: row.playlist, depth, parent });
      }
    }
  }
  flatten(roots, 0, null);
  if (query) {
    entries.sort((left, right) => {
      const pinOrder = Number(pinned.has(right.playlist.id)) - Number(pinned.has(left.playlist.id));
      return pinOrder || (rank.get(left.playlist.id) ?? Infinity) - (rank.get(right.playlist.id) ?? Infinity);
    });
  }
  return entries;
}

/**
 * Optimistically promotes a playlist after a successful local action.
 *
 * `played` is true only for a completed play-queue command. Every other
 * promotion is library activity only, so adding/editing a playlist can never
 * create a fake Home listening-history entry.
 */
export function promotePlaylist(id, { played = false } = {}) {
  if (!id) return false;
  const index = library.findIndex((playlist) => playlist?.id === id);
  if (index < 0) return false;
  const playlist = library[index];
  const at = activityNow();
  playlist.last_activity = at;
  if (played) {
    playlist.last_played = at;
    // A completed play is itself recency authority: Home may show this
    // action's shelf row immediately instead of waiting for the engine to
    // echo a rootlist whose interesting part we already know.
    libraryState.fresh = true;
  }
  if (index === 0) return true;
  library.splice(index, 1);
  library.unshift(playlist);
  return true;
}

/**
 * Inserts a just-created playlist into the library, at the top.
 *
 * The create command answers with the whole row — the name is the one that
 * was just typed — so the sidebar has no reason to wait for a rootlist
 * refetch to learn it. It used to: `createPlaylist` was fire-and-forget and
 * the row only appeared when the refresh landed, which is a round trip later
 * and, worse, is a read of an eventually-consistent rootlist that lists the
 * new playlist before its name attribute is readable. The row arrived blank.
 *
 * Ordering is not decided here. The backend stamps `last_activity` when it
 * creates the row and the refetch carries that stamp forward, so unshifting
 * is not an override of the sort in `setLibrary` — it is the position that
 * sort already produces for the newest stamp. That is deliberately the only
 * arrangement that keeps the optimistic row from visibly jumping when the
 * authoritative answer arrives.
 */
export function insertPlaylist(playlist) {
  const id = typeof playlist?.id === "string" ? playlist.id : "";
  if (!id) return false;
  // The refresh may have beaten us to it; one playlist is one row.
  if (library.some((entry) => entry?.id === id)) return applyPlaylistSummary(playlist);
  library.unshift({ ...playlist, last_activity: playlist.last_activity ?? activityNow() });
  // `libraryState` is deliberately untouched. One created row is not the
  // library having answered, and the rail's loading frame already stands
  // down on `library.length` — claiming `loaded` here would tell Home and
  // LibraryView that a one-row library is the whole of it.
  return true;
}

/**
 * Patches the matching library row from an authoritative `playlist_summary`
 * event, which the backend emits with the refreshed playlist after an applied
 * fetch (add/remove/reorder land here). The row updates in place — keyed
 * sidebar rows keep their element and repaint only the count — and the list
 * is never reordered: an edit already promoted its row through its own
 * command, so a bare count change must not shuffle the rail.
 *
 * `last_activity`/`last_played` are local-only ordering state. They serialize
 * as `null` whenever the backend has no stamp of its own; `null` means "not
 * supplied" and preserves ours. A concrete timestamp outranks it.
 */
export function applyPlaylistSummary(summary) {
  const id = typeof summary?.id === "string" ? summary.id : "";
  if (!id) return false;
  const row = library.find((playlist) => playlist?.id === id);
  if (!row) return false;
  for (const key of Object.keys(summary)) {
    const value = summary[key];
    if ((key === "last_activity" || key === "last_played") && value == null) continue;
    if (row[key] !== value) row[key] = value;
  }
  return true;
}

/* ---------------- Library edits: folders, following ----------------
   Every edit goes through the rootlist (see the engine's `edits`) and edits
   `libraryState.tree` — the one tree — with the same edit the shell applies
   to its copy when the write lands (library-tree.js). A move or a rename
   shows the moment it is asked for and is put back if the write fails,
   unless a library answer has replaced the tree meanwhile: then the
   account's answer stands. A create or a delete waits for the write, as a
   playlist's does — the one needs the id the account mints, the other is
   confirmed in a dialog that shows it working. */

function editFailed(what, error) {
  ui.error = `${what} ${error instanceof Error ? error.message : String(error ?? "")}`.trim();
}

// Compute each optimistic edit only after the previous one has settled, so
// rollback snapshots never contain another uncommitted edit.
let treeEditTail = null;
function serializeTreeEdit(edit) {
  const epoch = searchSessionEpoch;
  const run = () => epoch === searchSessionEpoch ? edit() : false;
  const operation = treeEditTail ? treeEditTail.then(run, run) : Promise.resolve(run());
  const pending = operation.finally(() => {
    if (treeEditTail === pending) treeEditTail = null;
  });
  treeEditTail = pending;
  return pending;
}

async function editTree(next, write, failure) {
  const epoch = searchSessionEpoch;
  const previous = libraryState.tree;
  libraryState.tree = next;
  const shown = libraryState.tree;
  try {
    await write();
    return epoch === searchSessionEpoch;
  } catch (error) {
    if (epoch !== searchSessionEpoch) return false;
    if (libraryState.tree === shown) libraryState.tree = previous;
    editFailed(failure, error);
    return false;
  }
}

function libraryItemName(item) {
  return item.kind === "folder"
    ? findFolder(libraryState.tree, item.id)?.name ?? "this folder"
    : library.find((row) => row.id === item.id)?.name ?? "this playlist";
}

/** A new folder, first in the library or first in `parent`. Throws. */
export async function createLibraryFolder(name, parent = null) {
  const epoch = searchSessionEpoch;
  const folder = await api.createFolder(name.trim(), parent);
  if (epoch !== searchSessionEpoch) throw new Error("The Spotify session changed.");
  if (!findFolder(libraryState.tree, folder.id)) {
    libraryState.tree = insertFirst(libraryState.tree, parent, folder);
  }
  return folder;
}

export function renameLibraryFolder(id, name) {
  return serializeTreeEdit(() => {
    const folder = findFolder(libraryState.tree, id);
    const trimmed = name.trim();
    if (!folder || !trimmed || trimmed === folder.name) return false;
    return editTree(
      renameFolder(libraryState.tree, id, trimmed),
      () => api.renameFolder(id, trimmed),
      `Could not rename "${folder.name}".`,
    );
  });
}

/** The folder and everything in it, gone from the library. Throws. */
export async function deleteLibraryFolder(id) {
  const epoch = searchSessionEpoch;
  await api.deleteFolder(id);
  if (epoch !== searchSessionEpoch) throw new Error("The Spotify session changed.");
  const { tree, playlistIds } = removeFolder(libraryState.tree, id);
  libraryState.tree = tree;
  const gone = new Set(playlistIds);
  for (let index = library.length - 1; index >= 0; index--) {
    if (gone.has(library[index]?.id)) library.splice(index, 1);
  }
}

/**
 * A playlist or a folder, `{ kind, id }`, to the top of `folder` — or out of
 * every folder when it is null. False, with nothing written, when it is
 * already there or cannot go there.
 */
export function moveLibraryItem(item, folder = null) {
  return serializeTreeEdit(() => {
    const next = moveItem(libraryState.tree, item, folder);
    if (!next) return false;
    return editTree(next, () => api.moveLibraryItem(item, folder), `Could not move "${libraryItemName(item)}".`);
  });
}

/**
 * Someone else's playlist out of your library, at once and without asking —
 * Spotify does not ask either, and the playlist's page offers it back.
 */
export function removeFromLibrary(id) {
  return serializeTreeEdit(() => removeLibraryPlaylist(id));
}

async function removeLibraryPlaylist(id) {
  const epoch = searchSessionEpoch;
  const authority = libraryAuthorityGeneration;
  const index = library.findIndex((row) => row?.id === id);
  if (index < 0) return false;
  const row = library[index];
  const previous = libraryState.tree;
  library.splice(index, 1);
  libraryState.tree = removePlaylist(previous, id);
  const shown = libraryState.tree;
  try {
    await api.deletePlaylist(id);
    return epoch === searchSessionEpoch;
  } catch (error) {
    if (epoch !== searchSessionEpoch) return false;
    if (authority === libraryAuthorityGeneration) {
      if (!library.some((entry) => entry?.id === id)) library.splice(Math.min(index, library.length), 0, row);
      if (libraryState.tree === shown) libraryState.tree = previous;
    }
    editFailed(`Could not remove "${row.name}" from your library.`, error);
    return false;
  }
}

/** Puts a playlist back in your library, first, from the row its page holds
    (the page's detail without its tracks: only the library row crosses). */
export async function addToLibrary(playlist) {
  const epoch = searchSessionEpoch;
  const { tracks: _tracks, excluded_track_ids: _excluded, ...row } = playlist ?? {};
  try {
    const summary = await api.followPlaylist(row);
    if (epoch !== searchSessionEpoch) return false;
    insertPlaylist(summary);
    return true;
  } catch (error) {
    if (epoch !== searchSessionEpoch) return false;
    editFailed(`Could not add "${playlist?.name ?? "this playlist"}" to your library.`, error);
    return false;
  }
}

/* ---------------- Spotify saved tracks ---------------- */

/**
 * How long the first page of Saved Tracks is worth keeping.
 *
 * Short because nothing here can subscribe to the collection: the shell's
 * membership index is refreshed on its own schedule, so a like added from
 * another client is only visible to us when `memberships_changed` arrives. A
 * bound of half a minute keeps the cache from outliving that by much while
 * still covering the burst it exists for — opening the page and pressing Play
 * on the Library card, both of which used to walk the same first page over the
 * network every single time.
 */
const LIKED_FIRST_PAGE_TTL_MS = 30_000;

/** The cached first page, the moment it was answered, and the session it
    belongs to. */
let likedFirstPage = null;
/** The walk in flight, so two callers share one pass over the network. */
let likedFirstPagePending = null;

/**
 * One page of Saved Tracks, deduplicated and reused for [`LIKED_FIRST_PAGE_TTL_MS`].
 *
 * Only the first page is cached: the collection is walked by cursor, and the
 * pages behind the first are read once while scrolling, by a view that owns the
 * cursor. The first page is the one every entry point asks for.
 *
 * Both the page kept here and the walk in flight belong to one account, and the
 * account is [`searchSessionEpoch`] — the counter `observeSearchSession` moves
 * whenever the identity of the session changes. Neither can be checked against
 * the account once it is in the air: a walk shared across a transition hands
 * the new account the previous one's saved tracks, and the page it answers with
 * would then be cached as if it were the new account's. Keying on the epoch is
 * what makes a stale page unreachable for another account even mid-flight, and
 * it holds for a transition that never reached `invalidateLikedFirstPage`.
 */
function browseLikedFirstPage() {
  const epoch = searchSessionEpoch;
  if (
    likedFirstPage &&
    likedFirstPage.epoch === epoch &&
    performance.now() - likedFirstPage.answeredAt < LIKED_FIRST_PAGE_TTL_MS
  ) {
    return Promise.resolve(likedFirstPage.page);
  }
  if (likedFirstPagePending && likedFirstPagePending.epoch === epoch) {
    return likedFirstPagePending.promise;
  }
  const pending = cacheMarkedRead("browse_liked_songs", { cursor: null })
    .then((page) => {
      /* Only the walk the hub is still tracking may cache its answer. An
         invalidation — a change of account, or news about what is saved — and a
         newer walk both take the slot, so a page that predates either cannot
         land on top of a fresher one, and a page walked for another account is
         not this one's to keep. */
      if (epoch === searchSessionEpoch && likedFirstPagePending?.promise === pending) {
        likedFirstPage = { page, answeredAt: performance.now(), epoch };
      }
      return page;
    })
    .finally(() => {
      // Only this walk clears the slot it took; a walk started after a
      // transition owns it now.
      if (likedFirstPagePending?.promise === pending) likedFirstPagePending = null;
    });
  likedFirstPagePending = { promise: pending, epoch };
  return pending;
}

/**
 * Drops the cached page and the walk in flight, so the next reader pays for a
 * fresh one from the account that is current now.
 *
 * Membership and account changes invalidate the collection. The next browse
 * must answer from the current account, not a stale first page.
 */
function invalidateLikedFirstPage() {
  likedFirstPage = null;
  likedFirstPagePending = null;
}

const savedTracksListeners = new Set();
export function watchSavedTracks(listener) {
  savedTracksListeners.add(listener);
  return () => savedTracksListeners.delete(listener);
}

/** Invalidate future first-page reads and notify mounted collections in place. */
export function savedTracksChanged(change = null) {
  invalidateLikedFirstPage();
  for (const listener of savedTracksListeners) listener(change);
}

/**
 * A follow or unfollow that landed, applied to the followed list in place:
 * that list is the local answer to "do I follow this artist", for the rail
 * and the artist page alike, so re-reading all of it would only be a network
 * round trip for what we already know. A list never asked for stays unasked.
 */
export function artistFollowChanged(artist, following) {
  if (!followed.loaded || !artist?.id) return;
  const present = followed.artists.some((entry) => entry.id === artist.id);
  if (following && !present) {
    followed.artists = [{ id: artist.id, name: artist.name ?? "", cover_url: artist.cover_url ?? "" }, ...followed.artists];
  } else if (!following && present) {
    followed.artists = followed.artists.filter((entry) => entry.id !== artist.id);
  }
}

/* ---------------- Cover resolution ---------------- */


/**
 * Disk-backed cache usage returned by `get_cache_stats`. The backend memoises
 * its filesystem walk, so Settings can refresh on mount/reopen without
 * repeatedly enumerating the cache directories.
 */
export const cacheStats = $state({
  audio: null,
  covers: null,
  loading: false,
  error: null,
  updatedAt: 0,
});
let cacheStatsRequest = null;
let cacheStatsGeneration = 0;
let cacheClearTail = null;

function invalidateCacheStatsRead() {
  cacheStatsGeneration += 1;
  cacheStatsRequest = null;
  cacheStats.loading = false;
}

function applyCacheStats(payload) {
  cacheStats.audio = payload?.audio ?? null;
  cacheStats.covers = payload?.covers ?? null;
  cacheStats.updatedAt = Date.now();
  cacheStats.error = null;
}


export function refreshCacheStats() {
  if (cacheStatsRequest) return cacheStatsRequest;
  const generation = ++cacheStatsGeneration;
  cacheStats.loading = true;
  cacheStats.error = null;
  const request = api
    .getCacheStats()
    .then((payload) => {
      if (generation === cacheStatsGeneration && cacheStatsRequest === request) applyCacheStats(payload);
      return payload;
    })
    .catch((error) => {
      if (generation === cacheStatsGeneration && cacheStatsRequest === request) {
        cacheStats.error = String(error || "Could not measure caches.");
      }
      throw error;
    })
    .finally(() => {
      if (generation === cacheStatsGeneration && cacheStatsRequest === request) {
        cacheStats.loading = false;
        cacheStatsRequest = null;
      }
    });
  cacheStatsRequest = request;
  return request;
}

export function clearCache(kind) {
  // Both kinds return a complete snapshot, so serialize clears rather than let
  // an older reply for one kind replace the other kind's newer result.
  const run = async () => {
    invalidateCacheStatsRead();
    const generation = cacheStatsGeneration;
    try {
      const payload = await api.clearCache(kind);
      // Fence scans started before or during the clear, including late errors.
      invalidateCacheStatsRead();
      if (kind === "audio") propagateCacheMarks(null);
      applyCacheStats(payload);
      return payload;
    } catch (error) {
      if (generation === cacheStatsGeneration) {
        cacheStats.error = String(error || "Could not clear the cache.");
      }
      throw error;
    }
  };
  const operation = cacheClearTail ? cacheClearTail.then(run, run) : run();
  const pending = operation.finally(() => {
    if (cacheClearTail === pending) cacheClearTail = null;
  });
  cacheClearTail = pending;
  return pending;
}

/** The image request itself fetches and caches missing artwork. */
export function coverUrl(url) {
  return url ? convertFileSrc(url, "cover") : "";
}

/* ---------------- Commands (exact contract names) ---------------- */

let previewLeaseIdSequence = 0;

export function allocatePreviewLeaseId() {
  if (previewLeaseIdSequence >= Number.MAX_SAFE_INTEGER) {
    throw new Error("Preview lease ID exhausted.");
  }
  previewLeaseIdSequence += 1;
  return previewLeaseIdSequence;
}

let playbackSpeedRequestGeneration = 0;

function decodeTrackWaveform(payload) {
  if (!payload || typeof payload !== "object") throw new Error("The waveform response was empty.");
  const binCount = Number(payload.bin_count);
  const interval = Number(payload.interval_ms);
  const duration = Number(payload.duration_ms);
  if (!Number.isInteger(binCount) || binCount < 0 || interval !== 1 ||
      !Number.isInteger(duration) || duration < 0 || typeof payload.peaks_base64 !== "string") {
    throw new Error("The waveform response was invalid.");
  }
  const binary = atob(payload.peaks_base64);
  if (binary.length !== binCount * 4) throw new Error("The waveform payload was truncated.");
  const peaks = new Int16Array(binCount * 2);
  for (let index = 0; index < peaks.length; index += 1) {
    const offset = index * 2;
    const unsigned = binary.charCodeAt(offset) | (binary.charCodeAt(offset + 1) << 8);
    peaks[index] = unsigned & 0x8000 ? unsigned - 0x10000 : unsigned;
  }
  return {
    track_id: payload.track_id,
    duration_ms: duration,
    interval_ms: interval,
    bin_count: binCount,
    peaks,
  };
}
/**
 * Play or pause, shown at once.
 *
 * Neither is a choice of what plays, so neither cancels anything: not a
 * catalogue start still on its way, nor a skip waiting for its page. A press
 * while a start is pending is for the playback that starts: it is sent once
 * the start's queue has been, so the start goes ahead and then pauses or
 * plays as pressed, and the shuffle it switched off belongs to a playback
 * that exists. A start that gives way to a newer choice takes its press with
 * it; that choice owns playback.
 */
function requestPlaying(target) {
  const previous = playback.playing;
  const at = positionMs();
  const generation = ++playingRequestGeneration;
  const authority = playingAuthorityGeneration;
  seekAuthorityGeneration += 1;
  playback.playing = target;
  anchorPlayhead(at);
  syncPlayheadTicker();
  // Only the newest press, and only while no engine state has answered it.
  const rollback = () => {
    if (generation !== playingRequestGeneration || authority !== playingAuthorityGeneration) return;
    const rollbackAt = positionMs();
    playback.playing = previous;
    anchorPlayhead(rollbackAt);
    syncPlayheadTicker();
  };
  const command = target ? "play" : "pause";
  // A start that has already given way to a newer choice is not pending.
  const start = pendingStart?.generation === lazyQueue.generation ? pendingStart : null;
  const sent = start
    ? start.settled.then(() => {
      if (generation !== playingRequestGeneration) return;
      if (start.generation === lazyQueue.generation) return invoke(command);
      rollback();
    })
    : invoke(command);
  return sent.catch((error) => {
    rollback();
    throw error;
  });
}

/** Settles when the last skip still waiting has been sent or withdrawn; null when none waits. */
let skipBacklog = null;

/** A Next from the last loaded row of a catalogue queue, which has more to load. */
function atLoadedTail() {
  return !!lazyQueue.source && playback.current_index >= playback.queue.length - 1;
}

/**
 * Next or Previous. Every press becomes its command, sent in the order the
 * presses came.
 *
 * The one press that waits is a Next from the last loaded row of a catalogue
 * queue: it asks for the next page first, so the skip lands on that page's
 * first track instead of off the end. Whatever that page does — arrives,
 * fails, gives way to a preview — the skip is sent after it; a failed page is
 * reported, and playback goes on as far as the loaded rows reach. A skip
 * pressed meanwhile waits behind it rather than overtaking it, until it has
 * been sent. Only a newer choice of what plays (a row, a list, an output; not
 * another skip, nor Play or Pause) or an account change withdraws a skip not
 * yet sent: it was aimed at the queue as it stood.
 */
function skip(command) {
  beginPlayback(true, { skip: true });
  const generation = skipGeneration;
  const ahead = skipBacklog;
  if (!ahead && !(command === "next" && atLoadedTail())) return invoke(command);
  let sent;
  const turn = new Promise((resolve) => { sent = resolve; });
  skipBacklog = turn;
  turn.then(() => {
    if (skipBacklog === turn) skipBacklog = null;
  });
  return (async () => {
    try {
      if (ahead) await ahead;
      if (command === "next" && atLoadedTail()) {
        try {
          await backfillLazyQueue(true);
        } catch (error) {
          if (generation === skipGeneration) {
            ui.error = `Could not load more of this artist. ${error instanceof Error ? error.message : String(error ?? "")}`.trim();
          }
        }
      }
      if (generation !== skipGeneration) return;
      return invoke(command);
    } finally {
      sent();
    }
  })();
}

function requestVolume(percent) {
  if (!Number.isFinite(percent)) return Promise.reject(new Error("Volume must be a number."));
  const target = Math.min(100, Math.max(0, Math.round(percent)));
  const generation = ++volumeRequestGeneration;
  const authority = volumeAuthorityGeneration;
  volumePendingGeneration = generation;
  playback.volume = target;
  return invoke("set_volume", { percent: target }).then(() => {
    if (generation !== volumeRequestGeneration) return;
    // A newer engine event wins, including an external volume adjustment.
    // A successful no-op need not emit state, so retain its confirmed target.
    if (authority === volumeAuthorityGeneration) confirmedVolume = target;
  }).finally(() => {
    if (volumePendingGeneration === generation) {
      volumePendingGeneration = null;
      playback.volume = confirmedVolume;
    }
  });
}

/** Every Canvas lookup made this session, by track id (canvas.js): asked
    for the playing track, or for the one the engine is fetching ahead, and
    never otherwise. Forgotten with the account it was asked as. */
export const canvasLookups = createCanvasLookups((id) => invoke("browse_canvas", { id }));

export const api = {
  selectOutput: (deviceId = null) => {
    beginPlayback(true);
    return invoke("select_output", { deviceId }).catch((error) => {
      ui.error = String(error);
      throw error;
    });
  },
  play: () => requestPlaying(true),
  pause: () => requestPlaying(false),
  setWindowVisible: (visible, refresh = false) => invoke("set_window_visible", { visible, refresh }),
  next: () => skip("next"),
  previous: () => skip("previous"),
  seek: (ms) => {
    const target = Math.max(0, Math.round(ms));
    const generation = ++seekRequestGeneration;
    const authority = seekAuthorityGeneration;
    const previousPosition = positionMs();
    // Anchor optimistically: without this the knob snaps back to the last
    // engine sync until the next heartbeat lands, then jumps forward again.
    // Seeking never changes `playing`; the engine preserves this intent on
    // both the paused and playing paths.
    anchorPlayhead(target);
    return invoke("seek", { positionMs: target }).catch((error) => {
      // A seek owns only its projection, not play/pause or a newer engine sync.
      if (generation === seekRequestGeneration && authority === seekAuthorityGeneration) {
        anchorPlayhead(previousPosition);
      }
      throw error;
    });
  },
  setVolume: (percent) => requestVolume(percent),
  setShuffle: (enabled) => invoke("set_shuffle", { enabled: !!enabled }),
  setRepeat: (mode) => invoke("set_repeat", { mode }),
  setPlaybackSpeed: (speed) => {
    const target = Number(speed);
    if (!Number.isFinite(target) || target < 0.5 || target > 4) {
      return Promise.reject(new Error("Playback speed must be between 0.5 and 4.0."));
    }
    const previous = playback.playback_speed;
    const generation = ++playbackSpeedRequestGeneration;
    // Only the newest command still represents the control's intent. An older
    // rejection must not roll back a newer optimistic value.
    playback.playback_speed = target;
    return invoke("set_playback_speed", { speed: target }).catch((error) => {
      if (generation === playbackSpeedRequestGeneration) {
        playback.playback_speed = previous;
      }
      throw error;
    });
  },
  /**
   * `automaticStart` respects playlist skips and unavailable rows. With shuffle
   * enabled the engine chooses a random eligible starting row; otherwise it
   * starts at the first eligible row at or after `index`, wrapping once.
   * Direct plays — a clicked row, a search hit — keep the default false and
   * play exactly what was asked for, even with shuffle enabled.
   */
  playQueue: (queue, index, context = "", { automaticStart = false } = {}) => {
    beginPlayback();
    const source = String(context ?? "").trim();
    return invoke("play_queue", {
      queue: contextTracks(queue, source),
      index,
      context: source,
      automaticStart,
    });
  },
  playQueueIndex: (index) => {
    beginPlayback(true);
    return invoke("play_queue_index", { index });
  },
  addQueue: (track, context = "") => {
    const source = String(context ?? "").trim();
    return invoke("add_queue", { track: contextTrack(track, source), context: source });
  },
  addQueueBatch: (tracks, context = "") => {
    const source = String(context ?? "").trim();
    return invoke("add_queue_batch", {
      tracks: contextTracks(tracks, source),
      context: source,
    });
  },
  /* Edits of the queue, not choices of what plays: a catalogue queue's tail
     only ever appends, so removing or moving a row leaves its source and
     cursor, and a start or a skip still on its way, as they were. */
  removeQueue: (index) => invoke("remove_queue", { index }),
  moveQueue: (from, to) => invoke("move_queue", { from, to }),
  /**
   * One window of the listening archive. The filter and the order travel with
   * the request because the engine holds the archive and the view holds only
   * the rows it is showing — answering either here would mean pulling the
   * whole thing back, which is what the paging exists to stop.
   */
  getHistory: (offset = 0, limit = 100, query = "", sort = "recent") =>
    cacheMarkedRead("get_history", { offset, limit, query, sort }),
  clearHistory: () => invoke("clear_history"),
  getTrackEdit: (trackId, playlistId = null) =>
    invoke("get_track_edit", { trackId, playlistId }),
  getTrackWaveform: (trackId) =>
    invoke("get_track_waveform", { trackId }).then(decodeTrackWaveform),
  cancelTrackWaveform: (trackId) => invoke("cancel_track_waveform", { trackId }),
  saveTrackEdit: (trackId, durationMs, cuts, loopRange = null) =>
    invoke("save_track_edit", { trackId, durationMs, cuts, loopRange }),
  deleteTrackEdit: (trackId) => invoke("delete_track_edit", { trackId }),
  previewTrackEdit: (track, cuts, loopRange = null, positionMs = 0, previewLeaseId) => {
    beginPlayback(true);
    const source = lazyQueue.source;
    const accepted = lazyQueue.suspendedLease?.id === previewLeaseId && lazyQueue.suspendedLease.accepted;
    lazyQueue.suspendedLease = { id: previewLeaseId, accepted };
    const lease = lazyQueue.suspendedLease;
    return invoke("preview_track_edit", {
      track,
      cuts,
      loopRange,
      positionMs: Math.max(0, Math.round(Number(positionMs) || 0)),
      previewLeaseId,
    }).then((value) => {
      if (source === lazyQueue.source && lazyQueue.suspendedLease?.id === previewLeaseId) {
        lazyQueue.suspendedLease.accepted = true;
      }
      return value;
    }).catch((error) => {
      if (lazyQueue.suspendedLease === lease && !lease.accepted) resumeLazyQueue(source, previewLeaseId);
      throw error;
    });
  },
  restorePreview: (previewLeaseId) => {
    if (lazyQueue.suspendedLease?.id === previewLeaseId) beginPlayback(true);
    const source = lazyQueue.source;
    return invoke("restore_preview", { previewLeaseId }).then((value) => {
      resumeLazyQueue(source, previewLeaseId);
      return value;
    });
  },
  setPlaylistTrackEditEnabled: (playlistId, trackId, enabled) =>
    invoke("set_playlist_track_edit_enabled", {
      playlistId,
      trackId,
      enabled: !!enabled,
    }),
  setPlaylistTrackExcluded: (playlistId, trackId, excluded) =>
    invoke("set_playlist_track_excluded", {
      playlistId,
      trackId,
      excluded: !!excluded,
    }),
  /**
   * `limit` applies to every section the server returns, not just the three we
   * parse, and it dominates search latency: measured medians are 743ms at 10
   * against 1080ms at 40, over a ~580ms irreducible server floor. It also sets
   * how many covers the results reference (tracks + albums + artists), so 10
   * asks for ~30 instead of ~120. Both halves of the win come from this number.
   */
  search: (query, limit = SEARCH_LIMIT) => cacheMarkedRead("search", { query, limit }),
  browseTrack: (id) => cacheMarkedRead("browse_track", { id }),
  browseArtistCatalogue: (id, releaseTypes = ["albums", "singles"], offset = 0, limit = 4) =>
    cacheMarkedRead("browse_artist_catalogue", { id, releaseTypes, offset, limit }),
  touchPlaylist: (id) => invoke("touch_playlist", { id }),
  touchPlaylistActivity: (id) => invoke("touch_playlist_activity", { id }),
  /** Containers of the user that hold `uri` — the saved mark's data. */
  getTrackPlaylists: (uri) => sessionInvoke("get_track_playlists", { uri }),
  browseTrackCredits: (id) => sessionInvoke("browse_track_credits", { id }),
  getCacheStats: () => invoke("get_cache_stats"),
  /** Explicit Settings-only cache wipe; returns fresh audio/covers stats. */
  clearCache: (kind) => invoke("clear_cache", { kind }),
  setAudioCacheLimit: (mb) => mutateAppSettings("set_audio_cache_limit", { mb }),
  setNormalisation: (enabled) =>
    mutateAppSettings("set_normalisation", { enabled: !!enabled }),
  setLaunchAtLogin: (enabled) => mutateAppSettings("set_launch_at_login", { enabled: !!enabled }),
  setStartMinimized: (enabled) => mutateAppSettings("set_start_minimized", { enabled: !!enabled }),
  setAnimatedCanvas: (enabled) =>
    mutateAppSettings("set_animated_canvas", { enabled: !!enabled }),
  /* Spotify devices on or off, locally; off brings playback on a device back
     here, paused, first. */
  setDevicesEnabled: (enabled) =>
    mutateAppSettings("set_devices_enabled", { enabled: !!enabled }),
  /* Canvas is an account preference on Spotify's side; this turns it on there
     when it is off. Resolves to whether anything was written — and a write
     changes the answers, so the ones kept here go with the engine's. */
  enableAccountCanvas: () => invoke("enable_account_canvas").then((wrote) => {
    if (wrote) canvasLookups.forget();
    return wrote;
  }),
  browseShow: (id) => cacheMarkedRead("browse_show", { id }),
  browseEpisode: (id) => cacheMarkedRead("browse_episode", { id }),
  browseProfile: (username) => sessionInvoke("browse_profile", { username }),
  /**
   * The collection is walked by cursor; only the first page is deduplicated and
   * kept for a moment (see [`browseLikedFirstPage`]). Later pages belong to the
   * view that is scrolling, which holds the cursor and the rows it has already
   * merged.
   */
  browseLikedSongs: (cursor = null) =>
    cursor == null ? browseLikedFirstPage() : cacheMarkedRead("browse_liked_songs", { cursor }),
  browsePlaylist: (id) => cacheMarkedRead("browse_playlist", { id }),
  browseRadio: (id) => cacheMarkedRead("browse_radio", { id }),
  browsePlaylistRecommendations: (id) => cacheMarkedRead("browse_playlist_recommendations", { id }),
  browseAlbum: (id) => cacheMarkedRead("browse_album", { id }),
  browseArtist: (id) => cacheMarkedRead("browse_artist", { id }),
  browseArtistSongwriter: (id, name) =>
    cacheMarkedRead("browse_artist_songwriter", { id, name }),
  browseFollowedArtists: () => sessionInvoke("browse_followed_artists"),
  browseSavedShows: () => sessionInvoke("browse_saved_shows"),
  createPlaylist: (name) => sessionInvoke("create_playlist", { name }),
  renamePlaylist: (id, name) => sessionInvoke("rename_playlist", { id, name }),
  /* One rootlist REM: deleting a playlist you made and removing someone
     else's from your library are the same edit. */
  deletePlaylist: (id) => sessionInvoke("delete_playlist", { id }),
  followPlaylist: (playlist) => sessionInvoke("follow_playlist", { playlist }),
  createFolder: (name, parent = null) => sessionInvoke("create_folder", { name, parent }),
  renameFolder: (id, name) => sessionInvoke("rename_folder", { id, name }),
  deleteFolder: (id) => sessionInvoke("delete_folder", { id }),
  moveLibraryItem: (item, folder = null) => sessionInvoke("move_library_item", { item, folder }),
  addPlaylistTracks: (id, uris) => sessionInvoke("add_playlist_tracks", { id, uris }),
  removePlaylistTracks: (id, uris, expectedSnapshotId = null) =>
    sessionInvoke("remove_playlist_tracks", { id, uris, expectedSnapshotId }),
  reorderPlaylistTracks: (id, from, to) =>
    sessionInvoke("reorder_playlist_tracks", { id, from, to }),
  login: () => invoke("login"),
  logout: () => invoke("logout"),
  getState: () => cacheMarkedRead("get_state"),
};
/**
 * Session-scoped recommendation cache.
 *
 * Recommendations are tied to the playlist revision returned by
 * `browse_playlist`; a revision change must never reuse tracks from the old
 * snapshot. Entries keep the resolved tracks and, while a request is pending,
 * its promise so separate consumers share one server call. Rejections remove
 * the entry instead of turning a transient failure into a permanent miss.
 */
const RECOMMENDATIONS_CACHE_MAX = 64;
const playlistRecommendationsCache = new Map();

function playlistRecommendationsKey(id, revision) {
  return `${id}\u0000${revision ?? ""}`;
}

function trimRecommendationCache() {
  while (playlistRecommendationsCache.size > RECOMMENDATIONS_CACHE_MAX) {
    playlistRecommendationsCache.delete(playlistRecommendationsCache.keys().next().value);
  }
}

function storeRecommendationCacheEntry(key, playlistId, revision, entry) {
  // A playlist has one current snapshot in the cache. Removing older
  // revisions also drops their pending promises, so a slow old response
  // cannot repopulate data after a newer browse payload arrived.
  for (const [candidateKey, candidate] of playlistRecommendationsCache) {
    if (candidate.playlistId === playlistId && candidateKey !== key) {
      playlistRecommendationsCache.delete(candidateKey);
    }
  }
  playlistRecommendationsCache.delete(key);
  playlistRecommendationsCache.set(key, { playlistId, revision, ...entry });
  trimRecommendationCache();
}

function touchRecommendationCache(key, entry) {
  playlistRecommendationsCache.delete(key);
  playlistRecommendationsCache.set(key, entry);
}

function clearPlaylistRecommendationsCache() {
  playlistRecommendationsCache.clear();
}

export function loadPlaylistRecommendations(id, revision = "", { force = false } = {}) {
  const playlistId = String(id ?? "").trim();
  if (!playlistId) return Promise.resolve([]);
  const epoch = searchSessionEpoch;

  const snapshot = String(revision ?? "");
  const key = playlistRecommendationsKey(playlistId, snapshot);
  const cached = playlistRecommendationsCache.get(key);

  // A refresh invalidates resolved data, but never duplicates an already
  // running request. The latter is important when an explicit refresh lands
  // at the same time as the near-footer observer.
  if (cached?.promise) {
    touchRecommendationCache(key, cached);
    return cached.promise;
  }
  if (cached && !force) {
    touchRecommendationCache(key, cached);
    return Promise.resolve(cached.tracks);
  }
  if (force) playlistRecommendationsCache.delete(key);

  let response;
  try {
    response = api.browsePlaylistRecommendations(playlistId);
  } catch (error) {
    return Promise.reject(error);
  }

  const request = Promise.resolve(response)
    .then((payload) => {
      if (epoch !== searchSessionEpoch) throw new Error("The Spotify session changed.");
      const tracks = Array.isArray(payload?.tracks) ? payload.tracks : [];
      const current = playlistRecommendationsCache.get(key);
      if (current?.promise === request) {
        storeRecommendationCacheEntry(key, playlistId, snapshot, { tracks });
      }
      return tracks;
    })
    .catch((error) => {
      // Errors are deliberately not cached: a later near-footer demand or
      // explicit refresh gets a chance to recover from startup/transient
      // engine failures.
      if (playlistRecommendationsCache.get(key)?.promise === request) {
        playlistRecommendationsCache.delete(key);
      }
      throw error;
    });

  storeRecommendationCacheEntry(key, playlistId, snapshot, { promise: request });
  return request;
}

/** Remove an URI after a successful Add so revisiting the route cannot restore it. */
export function removePlaylistRecommendation(id, revision = "", uri) {
  const playlistId = String(id ?? "").trim();
  const recommendationUri = String(uri ?? "").trim();
  if (!playlistId || !recommendationUri) return false;
  const snapshot = String(revision ?? "");
  const key = playlistRecommendationsKey(playlistId, snapshot);
  const cached = playlistRecommendationsCache.get(key);
  if (!cached || cached.promise || !Array.isArray(cached.tracks)) return false;
  const tracks = cached.tracks.filter((track) => track?.uri !== recommendationUri);
  if (tracks.length === cached.tracks.length) return false;
  storeRecommendationCacheEntry(key, playlistId, snapshot, { tracks });
  return true;
}


/** Optimistic play/pause flip; reconciled by the next `state` event. */
export function togglePlay() {
  if (!playback.queue.length) return;
  if (playback.current_index < 0 || playback.queue[playback.current_index]?.unavailable) return;
  requestPlaying(!playback.playing).catch(() => {});
}

/**
 * Start a Spotify sign-in: arm the engine first, open the browser second.
 *
 * The order is the whole point. `login` is what binds the loopback listener on
 * 127.0.0.1:5588 that Spotify's redirect comes back to, and until this
 * function existed in this shape nothing ever invoked it — the button opened
 * the authorize URL and nothing else, so the callback arrived at a port no
 * process held and the sign-in died there. The engine binds synchronously
 * before it answers, so awaiting the invoke is proof the port is held; opening
 * the browser first, or without awaiting, would put the race back.
 *
 * A failure here means the flow cannot start at all (the port is taken by
 * something else), so the browser must not be opened: the user is left on the
 * login surface with the reason, and the engine is still in `needs_login` with
 * its prepared URL intact, so clicking again is a clean retry.
 */
export async function openAuthUrl() {
  // The engine no-ops a second concurrent flow, but the UI must not rely on
  // that to know its own state: two clicks would otherwise open two tabs.
  if (session.authPending) return;
  const url = playback.auth_url;
  if (!url) return;
  session.authPending = true;
  session.error = null;
  try {
    await api.login();
  } catch (error) {
    session.error = String(error?.message ?? error);
    return;
  } finally {
    session.authPending = false;
  }
  // The listener is up and will wait; a browser that refuses to open is the
  // one remaining way for the user to be left with nothing on screen.
  openUrl(url).catch((error) => {
    session.error = `Could not open your browser: ${String(error?.message ?? error)}`;
  });
}

/* ---------------- Event wiring ---------------- */

let bootstrapped = false;

/** One readable reason out of an unknown subscription rejection. */
function describeWiringFailure(error) {
  return String(error instanceof Error ? error.message : error);
}

/**
 * A playlist opened from cache is served instantly and refreshed behind it.
 * This is that refresh landing.
 *
 * It is applied only if the playlist is still the one on screen, because the
 * fetch outlives the navigation that started it. The payload is a raw
 * snapshot (see `detail`), so a refresh publishes a new snapshot rather than
 * editing the open one; what it must not do is hand over a new LIST when the
 * list did not change. The table's keyed rows, the open row menu and a
 * reorder still in flight all hold on to row objects, so the same rows in the
 * same order keep the open array and every row the refresh did not move. Only
 * a genuinely different list takes the incoming array.
 */
function handlePlaylistRefresh(e) {
  const fresh = e.payload;
  const id = fresh?.id;
  if (!id || route.name !== "playlist" || route.id !== id) return;
  const open = detail.playlist;
  // The refresh may beat the cached browse command response. Publishing it
  // now gives that response something authoritative to preserve.
  if (!open) {
    detail.playlist = fresh;
    return;
  }
  // `PlaylistDetail` is flattened by serde: metadata and `tracks` are
  // siblings, so the metadata comes across whole and the rows are merged.
  detail.playlist = { ...open, ...fresh, tracks: refreshedRows(open.tracks ?? [], fresh.tracks ?? []) };
}

/** The fields a refresh can legitimately move on a row it keeps. */
const REFRESHED_ROW_FIELDS = ["cached", "play_count", "unavailable", "unavailable_reason", "added_at"];

/**
 * The open rows with a refresh applied: `current` itself when nothing moved,
 * a copy whose moved rows are replaced when only fields moved, and `incoming`
 * when the rows or their order differ. A moved row becomes a new object,
 * never an edited one: its fields are plain, so an edit would notify nobody.
 */
function refreshedRows(current, incoming) {
  const sameRows =
    current.length === incoming.length &&
    current.every((track, i) => track.id === incoming[i]?.id);
  if (!sameRows) return incoming;
  let rows = current;
  for (let i = 0; i < current.length; i++) {
    const into = current[i];
    const from = incoming[i];
    if (REFRESHED_ROW_FIELDS.every((key) => into[key] === from[key])) continue;
    if (rows === current) rows = current.slice();
    const row = { ...into };
    for (const key of REFRESHED_ROW_FIELDS) row[key] = from[key];
    rows[i] = row;
  }
  return rows;
}

export async function initEvents() {
  if (bootstrapped) return;
  bootstrapped = true;
  // A startup snapshot may arrive after live events. The independent event
  // lanes must not overwrite one another while the initial pull is in flight.
  let stateEvents = 0;
  let volumeEvents = 0;
  let positionEvents = 0;
  let sessionEvents = 0;
  let libraryEvents = 0;

  // Subscriptions reject only when the event bridge itself is broken
  // (missing plugin, webview teardown). Swallowing that left a permanently
  // deaf UI, so the first failure surfaces as a playback error banner.
  const targets = [
    ["state", (e) => {
      if ("ready" in e.payload) stateEvents += 1;
      else if ("volume" in e.payload) volumeEvents += 1;
      applyPlayback(e.payload);
    }],
    ["playback-action-error", (e) => { ui.error = String(e.payload); }],
    // Heartbeats carry only the projected compiled position as a scalar; the
    // full state event remains authoritative for duration and queue metadata.
    [
      "position",
      (e) => {
        const position = Number(e.payload);
        if (!Number.isFinite(position)) return;
        positionEvents += 1;
        seekAuthorityGeneration += 1;
        playback.position_ms = Math.max(0, Math.round(position));
        anchorPlayhead(playback.position_ms);
      },
    ],
    ["playlist", handlePlaylistRefresh],
    ["session", (e) => {
      sessionEvents += 1;
      applySession(e.payload);
    }],
    // Owned-playlist index updates repaint the saved check but cannot change
    // personal Liked membership or invalidate its cached first page.
    ["memberships_changed", (e) => {
      if (e.payload?.saved_tracks) savedTracksChanged(e.payload);
    }],
    // Disk hydration is useful for the sidebar and detail routes, but must
    // never promote Home past its fresh-rootlist loading frame. If a cache
    // event is delivered late, keep the authoritative answer already shown.
    ["library_cached", (e) => {
      if (libraryState.fresh) return;
      libraryEvents += 1;
      setLibrary(e.payload.playlists, { tree: e.payload.playlist_tree });
    }],
    // The one authoritative rootlist answer; the only writer that promotes
    // `libraryState.fresh` (a completed play may also, via promotePlaylist).
    ["library", (e) => {
      libraryEvents += 1;
      setLibrary(e.payload.playlists, { fresh: true, tree: e.payload.playlist_tree });
    }],
    // A mutation's refreshed summary patches the one library row it names;
    // full rootlist answers still arrive as `library`.
    ["playlist_summary", (e) => {
      if (applyPlaylistSummary(e.payload)) libraryEvents += 1;
    }],
  ];

  const results = await Promise.allSettled(
    targets.map(([event, handler]) => listen(event, handler)),
  );
  const failures = results.flatMap((result, index) =>
    result.status === "rejected"
      ? [`"${targets[index][0]}" — ${describeWiringFailure(result.reason)}`]
      : [],
  );
  if (failures.length > 0) {
    playback.error = failures.join("; ");
  }

  // Pull initial state. The engine may not be ready yet (offline, never), so
  // the cached library of the last signed-in account — which the Rust side
  // selects at startup and presents as the account signing in — is applied
  // here for an instant paint. Once ready, the coalesced library event
  // supplies fresh rootlist data without issuing a duplicate browse request;
  // a different account at ready is an account switch, which resets it.
  const stateAtPull = stateEvents;
  const volumeAtPull = volumeEvents;
  const positionAtPull = positionEvents;
  const sessionAtPull = sessionEvents;
  const libraryAtPull = libraryEvents;
  const settingsAtPull = appSettingsRevision;
  api
    .getState()
    .then((payload) => {
      if (settingsAtPull === appSettingsRevision) applyAppSettings(payload.settings);
      if (stateEvents === stateAtPull) {
        let snapshot = payload?.playback ?? payload;
        if (snapshot && (
          volumeEvents !== volumeAtPull ||
          positionEvents !== positionAtPull ||
          sessionEvents !== sessionAtPull
        )) {
          snapshot = { ...snapshot };
          if (volumeEvents !== volumeAtPull) snapshot.volume = playback.volume;
          if (positionEvents !== positionAtPull) snapshot.position_ms = playback.position_ms;
          if (sessionEvents !== sessionAtPull) {
            snapshot.auth_state = playback.auth_state;
            snapshot.username = playback.username;
          }
        }
        applyPlayback(snapshot);
      }
      if (Array.isArray(payload?.playlists) && (
        libraryEvents === libraryAtPull ||
        (payload.library_fresh === true && !libraryState.fresh)
      )) {
        // A cached event may arrive during the pull even though the rootlist
        // already completed before subscriptions. Do not let that event keep
        // Home in the loading frame; a newer fresh event wins on its own.
        setLibrary(payload.playlists, { fresh: payload.library_fresh === true, tree: payload.playlist_tree });
      }
    })
    .catch(() => {});
}
