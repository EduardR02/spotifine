/* =====================================================================
   THE DEMO'S SHELL

   What the Tauri shell and the playback engine are to the app, played in
   the page: the commands the app sends answered from demo/library.js, and
   the events it listens for sent back the way the shell sends them. The
   app itself is unchanged; demo/tauri.js hands it this instead of IPC.

   Playback is a clock, not audio: a track "plays" by its position moving,
   ends, and the next one starts. Edits to a playlist's rows (order, skips,
   adding and removing songs) are kept until the page reloads. What needs a
   Spotify account (likes, search, renaming and deleting, devices) answers
   that it is not part of the demo.
   ===================================================================== */
import { ME, OPENING, playlists, tree, liked, followedArtists, canvasFor, creditsFor } from "./library.js";

/** The answer to a write the demo does not make. */
const NOT_IN_DEMO = "That needs a Spotify account, so it's not in this demo.";

/* ---- Events --------------------------------------------------------- */

const listeners = new Map();

/** Subscribe to a shell event; the answer unsubscribes. */
export function listen(event, handler) {
  let set = listeners.get(event);
  if (!set) listeners.set(event, (set = new Set()));
  set.add(handler);
  return () => set.delete(handler);
}

/* Payloads are copied as IPC copies them: the app may write into what it
   is given, and must never be writing into the demo's own data. */
const copy = (value) => structuredClone(value);

function emit(event, payload) {
  for (const handler of [...(listeners.get(event) ?? [])]) handler({ event, id: 0, payload: copy(payload) });
}

/* ---- The library ---------------------------------------------------- */

const lists = new Map(playlists.map((list) => [list.summary.id, {
  summary: { ...list.summary },
  rows: list.rows.map((row) => ({ ...row })),
  excluded: new Set(),
  revision: 0,
}]));

function detailOf(id) {
  const list = lists.get(id);
  if (!list) return null;
  return {
    ...list.summary,
    snapshot_id: `${list.summary.snapshot_id}-${list.revision}`,
    tracks: list.rows,
    tracks_total: list.rows.length,
    rows_complete: true,
    excluded_track_ids: [...list.excluded],
  };
}

/** A playlist's rows changed: the shell refreshes the detail and the summary. */
function changed(id) {
  const list = lists.get(id);
  list.revision += 1;
  list.summary.tracks_total = list.rows.length;
  list.summary.cover_urls = [...new Set(list.rows.map((row) => row.cover_url))].slice(0, 4);
  emit("playlist", detailOf(id));
  emit("playlist_summary", list.summary);
}

/** Every track the demo knows, by uri. */
const known = new Map();
for (const list of lists.values()) for (const row of list.rows) known.set(row.uri, row);
const likedUris = new Set(liked.map((track) => track.uri));

/* ---- Playback ------------------------------------------------------- */

const engine = {
  queue: OPENING.rows.map((row) => ({ ...row, context: `playlist:${OPENING.summary.id}` })),
  index: 0,
  playing: true,
  /** Position at `at` (performance.now()); it moves while playing. */
  base: 64_000,
  at: performance.now(),
  volume: 72,
  shuffle: false,
  repeat: "off",
  speed: 1,
  preload: "",
  /** The shuffled play order of the rows after the current one. */
  bag: [],
  queueRevision: 1,
  orderRevision: 1,
  sentQueue: 0,
  sentOrder: 0,
  lastOrder: "",
};

const position = () =>
  Math.min(duration(), engine.base + (engine.playing ? (performance.now() - engine.at) * engine.speed : 0));
const current = () => engine.queue[engine.index] ?? null;
const duration = () => current()?.duration_ms ?? 0;

function setPosition(ms) {
  engine.base = Math.max(0, ms);
  engine.at = performance.now();
}

/** Whether automatic playback reaches a row: a playlist's skips are passed over. */
function eligible(index) {
  const row = engine.queue[index];
  if (!row || row.unavailable) return false;
  const context = row.context ?? "";
  if (!context.startsWith("playlist:")) return true;
  return !lists.get(context.slice("playlist:".length))?.excluded.has(row.id);
}

function reshuffle() {
  const rest = engine.queue.map((_, i) => i).filter((i) => i !== engine.index);
  for (let i = rest.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [rest[i], rest[j]] = [rest[j], rest[i]];
  }
  engine.bag = rest;
}

/** The rows automatic playback will reach, in order, the current one excluded. */
function upcoming() {
  if (engine.shuffle) return engine.bag.filter(eligible);
  const out = [];
  for (let i = engine.index + 1; i < engine.queue.length; i++) if (eligible(i)) out.push(i);
  if (engine.repeat === "context") for (let i = 0; i < engine.index; i++) if (eligible(i)) out.push(i);
  return out;
}

function snapshot() {
  const order = upcoming();
  const orderKey = order.join(",");
  if (orderKey !== engine.lastOrder) {
    engine.lastOrder = orderKey;
    engine.orderRevision += 1;
  }
  const row = current();
  return {
    ready: true,
    auth_state: "ready",
    auth_url: "",
    username: ME,
    playing: engine.playing,
    buffering: false,
    preview: false,
    output_device_id: "",
    output_device_name: "",
    position_ms: Math.round(position()),
    duration_ms: duration(),
    volume: engine.volume,
    shuffle: engine.shuffle,
    repeat: engine.repeat,
    playback_speed: engine.speed,
    audible_playback_speed: engine.speed,
    current_index: row ? engine.index : -1,
    current_uri: row?.uri ?? null,
    context: row?.context ?? "",
    preload_uri: engine.preload,
    error: "",
    queue: engine.queue,
    queue_revision: engine.queueRevision,
    upcoming: order,
    order_revision: engine.orderRevision,
  };
}

/** The full state, with the queue and the order only when they moved. */
function emitState() {
  const state = snapshot();
  if (engine.sentQueue === engine.queueRevision) delete state.queue;
  if (engine.sentOrder === engine.orderRevision) delete state.upcoming;
  engine.sentQueue = engine.queueRevision;
  engine.sentOrder = engine.orderRevision;
  emit("state", state);
  schedule();
}

let playlistInstall = null;
function queueChanged() {
  engine.queueRevision += 1;
  playlistInstall = null;
}

/* The track's end, and the gapless fetch-ahead thirty seconds before it,
   as timers rather than a tick: nothing runs between them. */
let endTimer = 0;
let preloadTimer = 0;
function schedule() {
  clearTimeout(endTimer);
  clearTimeout(preloadTimer);
  if (!engine.playing || !current()) return;
  const left = (duration() - position()) / engine.speed;
  endTimer = setTimeout(trackEnded, Math.max(0, left));
  const next = upcoming()[0];
  if (next != null && !engine.preload) {
    preloadTimer = setTimeout(() => {
      engine.preload = engine.queue[next]?.uri ?? "";
      emitState();
    }, Math.max(0, left - 30_000 / engine.speed));
  }
}

function load(index) {
  engine.index = index;
  engine.preload = "";
  setPosition(0);
  if (engine.shuffle) engine.bag = engine.bag.filter((i) => i !== index);
}

function trackEnded() {
  if (engine.repeat === "track") {
    setPosition(0);
  } else {
    const next = upcoming()[0];
    if (next == null) {
      engine.playing = false;
      setPosition(0);
    } else load(next);
  }
  emitState();
}

function skip(step) {
  if (!engine.queue.length) return;
  if (step < 0) {
    // As players do: Previous restarts a track that has been playing a while.
    if (position() > 3000) setPosition(0);
    else load(Math.max(0, engine.index - 1));
  } else {
    const next = upcoming()[0];
    if (next != null) load(next);
    else load((engine.index + 1) % engine.queue.length);
  }
  engine.playing = true;
}

function startQueue(index, automaticStart) {
  let start = Number(index) || 0;
  if (engine.shuffle) {
    engine.index = start;
    reshuffle();
  }
  if (automaticStart) {
    if (engine.shuffle) start = engine.bag.find(eligible) ?? start;
    else start = engine.queue.findIndex((_, i) => i >= start && eligible(i));
  }
  engine.playing = !!engine.queue[start];
  if (engine.playing) load(start);
  emitState();
}

/* ---- Commands ------------------------------------------------------- */

const settings = { animated_canvas: true, devices_enabled: false, spotify_listening_enabled: true };

const refuse = () => Promise.reject(NOT_IN_DEMO);

const commands = {
  get_state: () => ({
    playback: snapshot(),
    playlists: [...lists.values()].map((list) => list.summary),
    playlist_tree: tree,
    library_fresh: true,
    settings,
    me_id: ME,
  }),

  /* Reads */
  browse_playlist: ({ id }) => detailOf(id) ?? Promise.reject("This playlist isn't in the demo."),
  browse_liked_songs: () => ({ tracks: liked, next_cursor: null }),
  browse_playlist_recommendations: () => ({ tracks: [] }),
  browse_canvas: ({ id }) => {
    const url = canvasFor(id);
    return url ? { url: new URL(url, document.baseURI).href, type: "video_looping" } : null;
  },
  browse_track_credits: ({ id }) => creditsFor(id) ?? Promise.reject("No credits for this track."),
  get_track_playlists: ({ uri }) => {
    const refs = likedUris.has(uri) ? [{ id: "liked", name: "Liked Songs" }] : [];
    for (const list of lists.values()) {
      if (list.rows.some((row) => row.uri === uri)) refs.push({ id: list.summary.id, name: list.summary.name });
    }
    return refs;
  },
  browse_followed_artists: () => followedArtists,
  browse_saved_shows: () => [],
  hydrate_library_covers: () => [],
  get_track_edit: () => null,
  personal_api_status: () => ({
    client_id: "", connected: false, account_id: null,
    devices_authorized: false, authorization_pending: false, error: null,
  }),
  get_cache_stats: () => ({ entries: 0, bytes: 0 }),
  search: () => ({ top: null, tracks: [], albums: [], artists: [], playlists: [], shows: [], episodes: [] }),

  /* Playback */
  play: () => {
    if (!current()) return null;
    setPosition(position());
    engine.playing = true;
    emitState();
    return null;
  },
  pause: () => {
    setPosition(position());
    engine.playing = false;
    emitState();
    return null;
  },
  next: () => (skip(1), emitState(), null),
  previous: () => (skip(-1), emitState(), null),
  seek: ({ positionMs }) => {
    setPosition(Math.min(duration(), Number(positionMs) || 0));
    emitState();
    return null;
  },
  set_volume: ({ percent }) => {
    engine.volume = Math.max(0, Math.min(100, Math.round(Number(percent) || 0)));
    emit("state", { volume: engine.volume });
    return null;
  },
  set_shuffle: ({ enabled }) => {
    engine.shuffle = Boolean(enabled);
    if (engine.shuffle) reshuffle();
    engine.preload = "";
    emitState();
    return null;
  },
  set_repeat: ({ mode }) => {
    engine.repeat = ["off", "context", "track"].includes(mode) ? mode : "off";
    emitState();
    return null;
  },
  set_playback_speed: ({ speed }) => {
    setPosition(position());
    engine.speed = Math.max(0.5, Math.min(4, Number(speed) || 1));
    emitState();
    return null;
  },
  play_queue: ({ queue, index, context, automaticStart }) => {
    engine.queue = (queue ?? []).map((row) => ({ ...row, context: row.context || context || "" }));
    queueChanged();
    startQueue(index, automaticStart);
    return null;
  },
  play_playlist_row: ({ selection, index, context }) => {
    if (selection.reuse) {
      if (!playlistInstall || playlistInstall.context !== context ||
        selection.reuse.install !== playlistInstall.receipt.install ||
        selection.reuse.revision !== engine.queueRevision) return null;
      startQueue(index, false);
    } else {
      commands.play_queue({ queue: selection.rows, index, context });
      playlistInstall = { context, receipt: { install: crypto.randomUUID(), revision: engine.queueRevision } };
    }
    return { receipt: playlistInstall.receipt, queue_revision: engine.queueRevision };
  },
  play_queue_index: ({ index }) => {
    if (!engine.queue[index]) return null;
    load(index);
    engine.playing = true;
    emitState();
    return null;
  },
  add_queue: ({ track, context }) => {
    engine.queue = [...engine.queue, { ...track, context: track.context || context || "" }];
    if (engine.shuffle) engine.bag.splice(Math.floor(Math.random() * (engine.bag.length + 1)), 0, engine.queue.length - 1);
    queueChanged();
    emitState();
    return null;
  },
  add_queue_batch: ({ tracks, context }) => {
    for (const track of tracks ?? []) commands.add_queue({ track, context });
    return null;
  },
  remove_queue: ({ index }) => {
    if (index === engine.index || !engine.queue[index]) return null;
    engine.queue = engine.queue.filter((_, i) => i !== index);
    if (index < engine.index) engine.index -= 1;
    engine.bag = engine.bag.filter((i) => i !== index).map((i) => (i > index ? i - 1 : i));
    queueChanged();
    emitState();
    return null;
  },
  move_queue: ({ from, to }) => {
    const rows = [...engine.queue];
    if (!rows[from] || to < 0 || to >= rows.length) return null;
    const playing = rows[engine.index];
    const [row] = rows.splice(from, 1);
    rows.splice(to, 0, row);
    engine.queue = rows;
    engine.index = rows.indexOf(playing);
    queueChanged();
    emitState();
    return null;
  },

  /* A playlist's rows, kept until the page reloads */
  set_playlist_track_excluded: ({ playlistId, trackId, excluded }) => {
    const list = lists.get(playlistId);
    if (!list) return null;
    if (excluded) list.excluded.add(trackId);
    else list.excluded.delete(trackId);
    changed(playlistId);
    emitState();
    return null;
  },
  reorder_playlist_tracks: ({ id, from, to, rowCount }) => {
    const list = lists.get(id);
    if (!list || rowCount !== list.rows.length || !list.rows[from] || to < 0 || to >= list.rows.length) {
      return Promise.reject("The playlist changed. Reload it and try again.");
    }
    const [row] = list.rows.splice(from, 1);
    list.rows.splice(to, 0, row);
    changed(id);
    return null;
  },
  add_playlist_tracks: ({ id, uris }) => {
    const list = lists.get(id);
    if (!list) return refuse();
    for (const uri of uris ?? []) {
      const track = known.get(uri);
      if (track && !list.rows.some((row) => row.uri === uri)) list.rows.push({ ...track, added_at: Date.now() });
    }
    changed(id);
    emit("memberships_changed", { saved_tracks: false });
    return null;
  },
  remove_playlist_tracks: ({ id, uris }) => {
    const list = lists.get(id);
    if (!list) return refuse();
    const gone = new Set(uris ?? []);
    list.rows = list.rows.filter((row) => !gone.has(row.uri));
    changed(id);
    emit("memberships_changed", { saved_tracks: false });
    return null;
  },
  touch_playlist: () => null,
  touch_playlist_activity: () => null,
  set_window_visible: () => null,

  /* The account's own: not in the demo */
  create_playlist: refuse,
  rename_playlist: refuse,
  delete_playlist: refuse,
  follow_playlist: refuse,
  create_folder: refuse,
  rename_folder: refuse,
  delete_folder: refuse,
  move_library_item: refuse,
  personal_api_set_saved: refuse,
  personal_api_configure: refuse,
  personal_api_authorize: refuse,
  select_output: refuse,
  login: refuse,
  logout: refuse,
};

// The opening track is already playing: its end is due.
schedule();

/** The app's `invoke`: an answer, or a rejection with a readable reason. */
export async function invoke(command, args = {}) {
  const handler = commands[command];
  if (!handler) {
    if (import.meta.env.DEV) console.warn("[demo] unanswered command", command, args);
    return null;
  }
  const answer = await handler(args);
  return answer == null ? answer : copy(answer);
}
