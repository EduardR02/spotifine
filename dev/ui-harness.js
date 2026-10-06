/**
 * Copy-only browser harness for UI work without the Tauri shell.
 *
 * The bridge is installed before the real app modules load. Calls are retained
 * in window.__calls so playlist drags can be inspected as add-only actions.
 */

const calls = [];

// Every play_queue start, resolved: whether the caller asked for an automatic
// (skip-aware) start, which row was requested, and which row actually began.
const playQueueLog = [];
window.__calls = calls;
window.__clearCalls = () => {
  calls.length = 0;
};

let callbackId = 1;
let listenerId = 1;
const callbacks = new Map();
const callbackOnce = new Set();
const listeners = new Map();

function transformCallback(callback, once = false) {
  const id = callbackId++;
  callbacks.set(id, callback);
  if (once) callbackOnce.add(id);
  return id;
}

function unregisterCallback(id) {
  callbacks.delete(id);
  callbackOnce.delete(id);
  for (const [eventId, listener] of listeners) {
    if (listener.callbackId === id) listeners.delete(eventId);
  }
}

function runCallback(id, payload) {
  const callback = callbacks.get(id);
  if (typeof callback !== "function") return false;
  callback(payload);
  if (callbackOnce.has(id)) unregisterCallback(id);
  return true;
}

function registerListener(event, handlerId) {
  const id = listenerId++;
  listeners.set(id, { event, callbackId: handlerId });
  return id;
}

function unregisterListener(event, eventId, handlerId) {
  const listener = listeners.get(eventId);
  if (listener && (!event || listener.event === event)) listeners.delete(eventId);
  if (handlerId != null) unregisterCallback(handlerId);
  return null;
}

function emit(event, payload) {
  let delivered = 0;
  for (const [id, listener] of listeners) {
    if (listener.event === event && runCallback(listener.callbackId, { event, id, payload })) {
      delivered += 1;
    }
  }
  return delivered;
}

window.__emit = emit;

function makeTracks(n, offset = 0) {
  return Array.from({ length: n }, (_, i) => {
    const n2 = i + offset;
    return {
      id: `t${n2}`,
      uri: `spotify:track:t${n2}`,
      name: `Song ${n2}`,
      artist_names: [`Artist ${n2 % 4}`],
      artist_ids: [`a${n2 % 4}`],
      artist_id: `a${n2 % 4}`,
      album_name: `Album ${n2 % 5}`,
      album_id: `al${n2 % 5}`,
      duration_ms: 180000 + n2 * 7000,
      added_at: Date.now() - n2 * 86400000,
      unavailable: false,
    };
  });
}

const fixtures = {
  playlists: [
    { id: "p1", name: "Road Trip", owner_id: "eduard", tracks_total: 12 },
    { id: "p2", name: "Deep Focus", owner_id: "eduard", tracks_total: 3 },
    { id: "p3", name: "Night Drive", owner_id: "eduard", tracks_total: 0 },
  ],
};
fixtures.episode = {
  show_id: "show1", show_name: "Listening Notes", description: "A short audio-only journey through city sounds.",
  published_at: Date.UTC(2026, 8, 20),
  track: {
    id: "ep1", uri: "spotify:episode:ep1", name: "A Walk Through the Night",
    artist_names: ["Listening Notes"], artist_ids: [], album_id: "show1",
    album_name: "Listening Notes", cover_url: "", duration_ms: 1820000,
    unavailable: false, unavailable_reason: null,
  },
};
fixtures.show = {
  id: "show1", uri: "spotify:show:show1", name: "Listening Notes",
  publisher: "Spotifine Radio", description: "Stories told entirely in sound.",
  cover_url: "", episodes: [
    fixtures.episode,
    { ...fixtures.episode, published_at: Date.UTC(2026, 8, 21),
      track: { ...fixtures.episode.track, id: "ep2", uri: "spotify:episode:ep2", name: "A Quiet Morning" } },
  ],
};
fixtures.profile = {
  username: "eduard", name: "Eduard", image_url: "", playlists: fixtures.playlists,
};
fixtures.playlistTree = [
  { kind: "folder", id: "f1", name: "Trips", children: [
    { kind: "playlist", id: fixtures.playlists[0].id },
    { kind: "folder", id: "f2", name: "Late drives", children: [
      { kind: "playlist", id: fixtures.playlists[2].id },
    ] },
  ] },
  { kind: "playlist", id: fixtures.playlists[1].id },
];
fixtures.playlistDetail = {
  id: "p1",
  name: "Road Trip",
  owner_id: "eduard",
  snapshot_id: "snap-1",
  tracks: makeTracks(12),
  excluded_track_ids: [],
};
fixtures.sharedTrack = {
  ...makeTracks(1)[0],
  id: "3TxKtkCNR1yQARsvHxvNnP",
  uri: "spotify:track:3TxKtkCNR1yQARsvHxvNnP",
  name: "Shared Song",
  artist_names: ["Shared Artist"],
};
fixtures.longTrack = {
  ...makeTracks(1, 99)[0],
  name: "A Deliberately Long Track Title for Truncation and Overflow Checks",
  artist_names: ["An Artist Name Long Enough to Exercise Every Narrow Layout"],
  album_name: "An Equally Long Album Name for Dense Player Surfaces",
  duration_ms: 900000,
};

/**
 * The artist-page fixture. Quiet on purpose — a couple of releases and a
 * short About are all the context the Written-by section needs. Songwriter
 * discovery is a separate request so the artist overview can render first;
 * the verified playlist below mirrors that later engine response: titled
 * "Written by <artist>", owned by user id `spotify`, TEN tracks in source
 * order, while `tracks_total` reports the full 37.
 */
const songwriterPlaylist = {
  playlist: {
    id: "sw-written",
    uri: "spotify:playlist:sw-written",
    name: "Written by Artist 1",
    description: "The official songwriting playlist, verified by ownership.",
    owner: "Spotify",
    owner_id: "spotify",
    cover_url: "",
    cover_urls: [],
    collaborative: false,
    tracks_total: 37,
    snapshot_id: "snap-sw-written",
    last_played: null,
    last_activity: null,
  },
  tracks: makeTracks(10, 100),
};
fixtures.artist = {
  id: "ar1",
  uri: "spotify:artist:ar1",
  name: "Artist 1",
  cover_url: "",
  top_tracks: makeTracks(10),
  releases: {
    albums: [
      { id: "al0", uri: "spotify:album:al0", name: "Album 0", artist_names: ["Artist 1"], year: 2021, cover_url: "" },
      { id: "al1", uri: "spotify:album:al1", name: "Album 1", artist_names: ["Artist 1"], year: 2018, cover_url: "" },
    ],
    singles: [],
    compilations: [],
    appears_on: [],
  },
  release_counts: { albums: 3, singles: 2, compilations: 1, appears_on: 7 },
  releases_next_offset: null,
  overview: {
    biography: "A fixture artist: enough prose to draw the About card, nothing more.",
    header_image_url: null,
    biography_images: [
      { url: "/dev/gallery-landscape.svg", width: 640, height: 429 },
      { url: "/dev/gallery-portrait.svg", width: 800, height: 1000 },
      { url: "/dev/gallery-panorama.svg", width: 1200, height: 500 },
    ],
    popularity: 72,
    followers: 1234567,
    monthly_listeners: 9876543,
    world_rank: 421,
    top_cities: [],
    popular_releases: [
      { id: "al0", uri: "spotify:album:al0", name: "Album 0", artist_names: ["Artist 1"], year: 2021, cover_url: "" },
      { id: "al1", uri: "spotify:album:al1", name: "Album 1", artist_names: ["Artist 1"], year: 2018, cover_url: "" },
    ],
    related_artists: [],
    discovered_on: [],
    artist_playlists: [],
    artist_pick: null,
  },
};
fixtures.songwriterPlaylist = clone(songwriterPlaylist);

/**
 * Who the fixture account follows.
 *
 * Collection reads mirror the playback-session engine; optional writes use
 * the personal API fixture handlers below. The rail fixture reproduces a
 * mix of artists with portraits and without, and names long enough to reach
 * the ellipsis in a 212px rail.
 *
 * Nine entries, which is what the owner's real account carries: enough to
 * fill the rail's visible run without paging, which the real read does not do
 * either.
 */
let followedArtists = [
  { id: "ar1", uri: "spotify:artist:ar1", name: "Artist 1", cover_url: "/dev/gallery-portrait.svg" },
  { id: "ar2", uri: "spotify:artist:ar2", name: "Artist 2", cover_url: "" },
  { id: "fa1", uri: "spotify:artist:fa1", name: "Conrad.", cover_url: "/dev/gallery-landscape.svg" },
  { id: "fa2", uri: "spotify:artist:fa2", name: "The Sundown Committee", cover_url: "" },
  { id: "fa3", uri: "spotify:artist:fa3", name: "Marguerite Fontaine-Delacroix", cover_url: "/dev/gallery-panorama.svg" },
  { id: "fa4", uri: "spotify:artist:fa4", name: "Halogen", cover_url: "" },
  { id: "fa5", uri: "spotify:artist:fa5", name: "Nightjar", cover_url: "" },
  { id: "fa6", uri: "spotify:artist:fa6", name: "Beacon & Ash", cover_url: "" },
  { id: "fa7", uri: "spotify:artist:fa7", name: "Yuki Watanabe Ensemble", cover_url: "" },
];
let followedFailure = null;
/** How long the read pretends the network takes; see `setFollowedDelay`,
    which is what makes the rail's loading frame observable at all. */
let followedDelayMs = 0;

async function followedRoundTrip() {
  if (followedDelayMs > 0) await new Promise((done) => setTimeout(done, followedDelayMs));
  if (followedFailure) {
    // Rejected with the bare string, the way a Tauri command's
    // `Result<_, String>` reaches the frontend — an Error here would put an
    // "Error:" prefix on screen that production never shows.
    return Promise.reject(followedFailure);
  }
}

/** How the fake engine answers `login`: null accepts, a string refuses with
    that message. See `setLoginFailure`. */
let loginFailure = null;

/** Track ids for catalogue releases start past every library fixture. */
let catalogueTrackOffset = 200;

/**
 * One catalogue release for the paged artist surfaces: a summary plus its
 * COMPLETE track list, exactly like an engine page — the discography reader
 * renders every row under the record. Tracks credit Artist 1 (compilations
 * and appears-on summaries spread the bill) and point back at this release.
 */
function catalogueRelease(kind, id, name, year, trackCount, artists = ["Artist 1"]) {
  const offset = catalogueTrackOffset;
  catalogueTrackOffset += trackCount + 4;
  return {
    id,
    uri: `spotify:album:${id}`,
    name,
    artist_names: artists,
    artist_ids: [],
    cover_url: "",
    year,
    tracks: makeTracks(trackCount, offset).map((track) => ({
      ...track,
      album_name: name,
      album_id: id,
      artist_names: [artists[0]],
      artist_ids: ["ar1"],
      artist_id: "ar1",
    })),
  };
}

/**
 * The virtual catalogue `browse_artist_catalogue` walks, pooled by release
 * type: three albums, two singles, a compilation and seven appears-on
 * summaries. Sized so a four-release page (six-item mixed walk) and a
 * six-release page both land mid-pool and leave a genuine remainder behind.
 */
const cataloguePools = {
  albums: [
    catalogueRelease("albums", "al0", "Album 0", 2021, 8),
    catalogueRelease("albums", "al1", "Album 1", 2018, 10),
    catalogueRelease("albums", "al2", "Album 2", 2015, 9),
  ],
  singles: [
    catalogueRelease("singles", "sg0", "Single 0", 2023, 2),
    catalogueRelease("singles", "sg1", "Single 1", 2020, 1),
  ],
  compilations: [
    catalogueRelease("compilations", "cp0", "Compilation 0", 2019, 12, ["Artist 1", "Artist 2"]),
  ],
  appears_on: [
    catalogueRelease("appears_on", "ao0", "Appears On 0", 2022, 5),
    catalogueRelease("appears_on", "ao1", "Appears On 1", 2021, 7),
    catalogueRelease("appears_on", "ao2", "Appears On 2", 2020, 6),
    catalogueRelease("appears_on", "ao3", "Appears On 3", 2019, 4),
    catalogueRelease("appears_on", "ao4", "Appears On 4", 2017, 8, ["Artist 1", "Artist 3"]),
    catalogueRelease("appears_on", "ao5", "Appears On 5", 2016, 6),
    catalogueRelease("appears_on", "ao6", "Appears On 6", 2014, 5, ["Artist 1", "Artist 4"]),
  ],
};

/**
 * The album route's payload (AlbumDetail): the record plus its track list.
 * It answers as Album 0 for whatever id is asked — enough for layout review,
 * and consistent with the artist shelf whose first release shares the id.
 */
fixtures.album = catalogueRelease("albums", "al0", "Album 0", 2021, 8);

/** The routes that read one cached artist payload — mirrors state.svelte.js. */
const ARTIST_ROUTE_NAMES = [
  "artist",
  "discography",
  "fans-also-like",
  "appears-on",
  "artist-playlists",
  "discovered-on",
];

const now = Date.now();
const H = 3600_000;

/**
 * An archive the size of the real one, because the history view's problems are
 * only visible at scale: fourteen rows page in one request and group into one
 * day, so a fourteen-row fixture proves nothing about either. This builds ~1200
 * plays across three weeks with days of wildly different length — including two
 * silent days, which is the case that a naive day-grouping gets wrong.
 *
 * Deterministic: a small LCG rather than Math.random, so a screenshot taken now
 * and one taken after a change differ only where the code differs.
 */
const HISTORY_DAYS = 21;
const DAY = 86_400_000;
function historyArchive() {
  const catalogue = makeTracks(60);
  const contexts = ["playlist:p1", "album:al1", "search", "liked", "radio:r1", "playlist:p2", ""];
  const rows = [];
  let seed = 20_260_908;
  const rand = () => ((seed = (seed * 1_103_515_245 + 12_345) & 0x7fffffff) / 0x7fffffff);
  const midnight = new Date();
  midnight.setHours(0, 0, 0, 0);
  for (let day = 0; day < HISTORY_DAYS; day += 1) {
    // Two deliberately silent days, so the grouping has gaps to survive.
    if (day === 4 || day === 11) continue;
    const plays = day === 0 ? 9 : 20 + Math.floor(rand() * 70);
    // Listening starts somewhere in the morning and runs forward from there.
    let at = midnight.getTime() - day * DAY + (8 + rand() * 3) * H;
    for (let play = 0; play < plays && at < now; play += 1) {
      const track = catalogue[Math.floor(rand() * catalogue.length)];
      const completed = rand() > 0.28;
      rows.push({
        track_id: track.id,
        started_at: Math.round(at),
        ms_played: completed
          ? track.duration_ms
          : Math.floor(track.duration_ms * (0.12 + rand() * 0.6)),
        completed,
        context: contexts[Math.floor(rand() * contexts.length)],
        track,
      });
      at += track.duration_ms + rand() * 4 * 60_000;
    }
  }
  rows.sort((a, b) => b.started_at - a.started_at);
  return rows;
}
fixtures.history = historyArchive();

/** Every history page this harness has served, so paging can be measured. */
const historyPages = [];
window.__historyPages = historyPages;

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function findTrack(value) {
  const id = String(value ?? "").replace("spotify:track:", "");
  for (const tracks of playlistTracks.values()) {
    const found = tracks.find((track) => track.id === id || track.uri === value);
    if (found) return found;
  }
  if (fixtures.longTrack.id === id || fixtures.longTrack.uri === value) return fixtures.longTrack;
  return null;
}

function contextTracks(tracks, context) {
  return tracks.map((track) => ({ ...track, context: track.context || context }));
}

const playlistTracks = new Map([
  ["p1", fixtures.playlistDetail.tracks],
  ["p2", makeTracks(3, 12)],
  ["p3", []],
]);
const playlistNames = new Map(fixtures.playlists.map((playlist) => [playlist.id, playlist.name]));
const memberships = new Map();
for (const track of fixtures.playlistDetail.tracks) memberships.set(track.id, new Set(["p1"]));
memberships.get("t0").add("liked");

/**
 * The playlist-local skip preference, mirrored onto every detail payload as
 * `excluded_track_ids`, exactly like the engine's flattened browse answer.
 * p1's list lives directly on the shared fixture so tests can read either.
 */
const playlistExclusions = new Map();

function exclusionsFor(id) {
  return id === "p1" ? fixtures.playlistDetail.excluded_track_ids : playlistExclusions.get(id);
}

function exclusionList(id) {
  let list = exclusionsFor(id);
  if (!list) {
    list = [];
    playlistExclusions.set(id, list);
  }
  return list;
}

function applyTrackExclusion(id, trackId, excluded) {
  const wanted = String(trackId ?? "");
  if (!wanted) return false;
  const list = exclusionList(id);
  const at = list.findIndex((value) => String(value) === wanted);
  if (excluded && at === -1) {
    list.push(wanted);
    return true;
  }
  if (!excluded && at !== -1) {
    list.splice(at, 1);
    return true;
  }
  return false;
}

function editKey(trackId, playlistId = "p1") {
  return `${trackId}:${playlistId || "p1"}`;
}

function initialEdit(track) {
  return {
    definition: {
      track_id: track.id,
      duration_ms: track.duration_ms,
      cuts: [{ start_ms: 12000, end_ms: 18000 }],
      loop_range: { start_ms: 36000, end_ms: 54000, play_count: 3 },
    },
    enabled: true,
  };
}

const edits = new Map();
edits.set(editKey("t0"), initialEdit(fixtures.playlistDetail.tracks[0]));
edits.set(editKey(fixtures.longTrack.id), initialEdit(fixtures.longTrack));

function effectiveEdit(track, playlistId = "p1") {
  const saved = edits.get(editKey(track.id, playlistId));
  return saved && saved.enabled ? saved.definition : null;
}

function queueWithEdits(tracks, context = "playlist:p1") {
  return contextTracks(tracks, context).map((entry) => {
    const edit = effectiveEdit(entry, entry.context?.startsWith("playlist:") ? entry.context.slice("playlist:".length) : "");
    return edit ? { ...entry, effective_edit: clone(edit) } : entry;
  });
}

function detailFor(id) {
  const tracks = playlistTracks.get(id) || [];
  if (id === "p1") return fixtures.playlistDetail;
  return {
    id,
    name: playlistNames.get(id) || id,
    owner_id: "eduard",
    snapshot_id: `snap-${id}`,
    tracks,
    excluded_track_ids: exclusionsFor(id) ?? [],
  };
}

function syncPlaylistTotal(id) {
  const playlist = fixtures.playlists.find((item) => item.id === id);
  if (playlist) playlist.tracks_total = (playlistTracks.get(id) || []).length;
}

/** The refreshed library summary a real backend emits with `playlist_summary`. */
function playlistSummaryFor(id) {
  const playlist = fixtures.playlists.find((item) => item.id === id);
  return playlist
    ? { ...playlist, tracks_total: (playlistTracks.get(id) || []).length }
    : null;
}

function emitPlaylistSummary(id) {
  const summary = playlistSummaryFor(id);
  if (summary) emit("playlist_summary", clone(summary));
}

/* Independent array revisions mirror the engine wire contract. Comparing
   fixture values here keeps mutations, including browser-driven ones, honest. */
let queueRevision = 1;
let orderRevision = 1;
let emittedQueueRevision = 0;
let emittedOrderRevision = 0;
let queueIdentity = "";
let heldOrder = [];

function refreshQueueEdits() {
  playback.queue = playback.queue.map((entry) => {
    const track = findTrack(entry.uri);
    const edit = track && effectiveEdit(track);
    if (edit) return { ...entry, effective_edit: clone(edit) };
    const { effective_edit: unused, ...withoutEdit } = entry;
    return withoutEdit;
  });
}

/**
 * Automatic shuffle starts consume the first draw from the same filtered bag
 * as the upcoming rows. Ordered starts walk to the first eligible row;
 * direct plays keep the row they were handed.
 */
function automaticStartIndex(from) {
  const length = playback.queue.length;
  if (!length) return null;
  if (playback.shuffle) {
    playback.current_index = null;
    redrawShuffleBag();
    return shuffleBag.pop() ?? null;
  }
  for (let step = 0; step < length; step += 1) {
    const at = (((from + step) % length) + length) % length;
    if (queueRowEligible(at)) return at;
  }
  return null;
}

function waveformFor(trackId, durationMs) {
  const track = findTrack(trackId) || fixtures.longTrack;
  const requested = durationMs ?? track.duration_ms;
  const duration = Math.max(0, Math.ceil(Number(requested) || 0));
  const pairs = new Int16Array(duration * 2);
  for (let bin = 0; bin < duration; bin += 1) {
    const t = duration ? bin / duration : 0;
    const raw = (Math.sin(t * 61) * 0.3 + Math.sin(t * 17.3) * 0.45 + 0.55) * 30000;
    const high = Math.min(32767, Math.max(0, Math.round(raw)));
    const low = -Math.round(high * (0.55 + 0.4 * Math.abs(Math.sin(t * 7))));
    pairs[bin * 2] = low;
    pairs[bin * 2 + 1] = high;
  }
  const bytes = new Uint8Array(pairs.buffer);
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return {
    track_id: trackId,
    duration_ms: duration,
    interval_ms: 1,
    bin_count: duration,
    peaks_base64: btoa(binary),
  };
}

function trackFromQueueItem(item) {
  if (typeof item === "object" && item) return item;
  const known = findTrack(item);
  if (known) return known;
  const id = String(item || "queued").replace("spotify:track:", "");
  return { ...makeTracks(1)[0], id, uri: `spotify:track:${id}`, name: `Queued ${id}` };
}

function setCurrent(index, resetPosition = true) {
  if (!playback.queue.length) {
    playback.current_index = 0;
    playback.current_uri = null;
    playback.duration_ms = 0;
    if (resetPosition) playback.position_ms = 0;
    return;
  }
  const length = playback.queue.length;
  playback.current_index = ((index % length) + length) % length;
  const current = playback.queue[playback.current_index];
  playback.current_uri = current.uri;
  const source = findTrack(current.uri);
  playback.duration_ms = source?.duration_ms ?? current.duration_ms ?? 0;
  if (resetPosition) playback.position_ms = 0;
  applyKindSpeed();
}

/* As the engine does: songs and episodes keep their own speed, and the
   current item's kind decides which one plays and shows. */
const isEpisode = (uri) => String(uri ?? "").startsWith("spotify:episode:");
function applyKindSpeed() {
  if (playback.output_device_id) return;
  playback.playback_speed = isEpisode(playback.current_uri) ? playback.episode_speed : playback.track_speed;
  playback.audible_playback_speed = playback.playback_speed;
}

const playback = {
  ready: true,
  auth_state: "ready",
  auth_url: "",
  playing: true,
  preview: false,
  output_device_id: "",
  output_device_name: "",
  username: "eduard",
  position_ms: 45000,
  duration_ms: fixtures.playlistDetail.tracks[0].duration_ms,
  volume: 70,
  shuffle: false,
  repeat: "off",
  playback_speed: 1,
  track_speed: 1,
  episode_speed: 1,
  audible_playback_speed: 1,
  current_index: 0,
  current_uri: fixtures.playlistDetail.tracks[0].uri,
  queue: queueWithEdits(fixtures.playlistDetail.tracks),
  error: "",
};

/**
 * The queue's real play order, mirroring `Engine::upcoming_indices`.
 *
 * The engine publishes this because index order is a guess: only it holds the
 * shuffle bag and the per-playlist skips. A harness that shipped the state
 * event without it would render a Queue view permanently showing one row, so
 * the surface under test would not be the surface.
 *
 * The bag is modelled the same way too — drawn once, popped as tracks play,
 * repaired rather than redrawn — because a harness that reshuffled on every
 * mutation would hide exactly the defect the engine's repair exists to fix.
 * The draw is seeded and deterministic so a harness run is reproducible.
 */
let shuffleBag = [];
let bagSeed = 0x9e3779b9;

function bagRandom() {
  bagSeed ^= bagSeed << 13;
  bagSeed ^= bagSeed >>> 7;
  bagSeed ^= bagSeed << 17;
  return bagSeed >>> 0;
}

function queueRowEligible(index) {
  const track = playback.queue[index];
  if (!track || track.unavailable) return false;
  const context = String(track.context ?? "");
  if (!context.startsWith("playlist:")) return true;
  const excluded = exclusionsFor(context.slice("playlist:".length)) ?? [];
  return !excluded.some((value) => String(value) === String(track.id));
}

function redrawShuffleBag() {
  shuffleBag = [];
  if (!playback.shuffle) return;
  for (let index = 0; index < playback.queue.length; index++) {
    if (index !== playback.current_index && queueRowEligible(index)) shuffleBag.push(index);
  }
  for (let i = shuffleBag.length - 1; i > 0; i--) {
    const j = bagRandom() % (i + 1);
    [shuffleBag[i], shuffleBag[j]] = [shuffleBag[j], shuffleBag[i]];
  }
}

/** A new row joins the bag at a random slot, never at either end: the bag
    pops from the back, so an append would pin it to "always next" or
    "always last". Mirrors `Engine::splice_into_shuffle_pool`. */
function spliceIntoShuffleBag(index) {
  if (!playback.shuffle || index === playback.current_index) return;
  if (!queueRowEligible(index) || shuffleBag.includes(index)) return;
  shuffleBag.splice(bagRandom() % (shuffleBag.length + 1), 0, index);
}

/** Drops a removed row and shifts the rows above it down, keeping the drawn
    order of every survivor. Mirrors `Engine::repair_shuffle_pool`. */
function repairShuffleBagAfterRemoval(index) {
  shuffleBag = shuffleBag
    .filter((pooled) => pooled !== index)
    .map((pooled) => (pooled > index ? pooled - 1 : pooled))
    .filter((pooled) => pooled !== playback.current_index && pooled < playback.queue.length);
}

function upcomingIndices() {
  const current = playback.current_index;
  if (!(current >= 0)) {
    return playback.queue.map((_, i) => i).filter(queueRowEligible);
  }
  if (playback.shuffle) {
    return shuffleBag.filter(queueRowEligible).reverse();
  }
  const out = [];
  for (let i = current + 1; i < playback.queue.length; i++) {
    if (queueRowEligible(i)) out.push(i);
  }
  if (playback.repeat === "context") {
    for (let i = 0; i <= current; i++) if (queueRowEligible(i)) out.push(i);
  }
  return out;
}

function snapshot() {
  const identity = JSON.stringify(playback.queue);
  if (identity !== queueIdentity) {
    queueIdentity = identity;
    queueRevision += 1;
  }
  const upcoming = upcomingIndices();
  if (upcoming.length !== heldOrder.length || upcoming.some((row, index) => row !== heldOrder[index])) {
    heldOrder = upcoming;
    orderRevision += 1;
  }
  return { ...clone(playback), context: playback.queue[playback.current_index]?.context ?? "",
    queue_revision: queueRevision, order_revision: orderRevision, upcoming };
}

function emitState() {
  const state = snapshot();
  if (queueRevision === emittedQueueRevision) delete state.queue;
  if (orderRevision === emittedOrderRevision) delete state.upcoming;
  emittedQueueRevision = queueRevision;
  emittedOrderRevision = orderRevision;
  emit("state", state);
}

function playlistIdFrom(args) {
  return args.playlistId ?? args.playlist_id ?? args.id ?? "p1";
}

function addPlaylistTracks(id, uris) {
  const target = playlistTracks.get(id) || [];
  if (!playlistTracks.has(id)) playlistTracks.set(id, target);
  let changed = false;
  for (const uri of uris) {
    const track = findTrack(uri);
    if (!track || target.some((item) => item.uri === track.uri)) continue;
    target.push(track);
    const set = memberships.get(track.id) || new Set();
    set.add(id);
    memberships.set(track.id, set);
    changed = true;
  }
  syncPlaylistTotal(id);
  if (changed) {
    emit("memberships_changed", null);
    emit("playlist", clone(detailFor(id)));
  }
  emitPlaylistSummary(id);
}

function removePlaylistTracks(id, uris) {
  const target = playlistTracks.get(id) || [];
  const wanted = new Set(uris);
  const kept = target.filter((track) => !wanted.has(track.uri) && !wanted.has(track.id));
  if (kept.length === target.length) return;
  playlistTracks.set(id, kept);
  for (const uri of uris) {
    const track = findTrack(uri);
    const set = track && memberships.get(track.id);
    if (set) set.delete(id);
  }
  if (id === "p1") fixtures.playlistDetail.tracks = kept;
  emit("memberships_changed", null);
  emit("playlist", clone(detailFor(id)));
  emitPlaylistSummary(id);
}

function updateMemberships(args, add) {
  const id = playlistIdFrom(args);
  const values = args.uris ?? args.trackUris ?? args.track_uris ?? [];
  const uris = Array.isArray(values) ? values : [values];
  if (add) addPlaylistTracks(id, uris);
  else removePlaylistTracks(id, uris);
}

/**
 * Track credits, shaped exactly like the engine's `TrackCreditsDetail`:
 * licensor-named groups, contributors with optional subroles and verbatim
 * urls. Rich on purpose — the wide sheet, the filter field and the compact
 * variant all need real data to be reviewed against.
 */
function trackCreditsPayload(id) {
  const n = Number(String(id).replace(/\D/g, "")) || 0;
  const performer = (name, subroles) => ({ id: `c${n}-${name}`, uri: `spotify:artist:${name}`, url: `https://artists.spotify.com/songwriter/${name}`, name, subroles });
  return {
    track_uri: `spotify:track:${id}`,
    track_name: `Song ${n}`,
    groups: [
      { title: "Performer", contributors: [performer("Main Artist", ["Main Vocalist"]), performer("Featured Artist", ["Featured Vocalist"]), performer("Choir", ["Background Vocalist"])] },
      { title: "Writer", contributors: [performer("Author One", ["Lyricist", "Composer"]), performer("Author Two", ["Composer"])] },
      { title: "Producer", contributors: [performer("Producer One", ["Producer"]), performer("Co Producer", ["Co-Producer", "Programmer"])] },
      { title: "Technical", contributors: [performer("Engineer One", ["Recording Engineer", "Mixing Engineer"]), performer("Mastering Engineer", ["Mastering Engineer"])] },
      { title: "Source", contributors: [performer("Sample Source", [])] },
    ],
    source: "Universal Music Publishing Group",
  };
}


const personalStatus = {
  client_id: "00000000000000000000000000000000", connected: true, account_id: "eduard",
  devices_authorized: true, authorization_pending: false, error: null,
};
const outputDevices = [
  { id: "remote-1", name: "Living room speaker", type: "Speaker", is_active: true,
    is_restricted: false, is_private_session: false, volume_percent: 65, supports_volume: true },
  { id: "remote-2", name: "Kitchen display", type: "Computer", is_active: false,
    is_restricted: false, is_private_session: false, volume_percent: 30, supports_volume: true },
];
let outputFailure = "";
/* Follows the fixture account holds. Likes are the membership index's
   "liked" container, as they are in the shell. */
const personalFollows = new Set(["spotify:artist:ar1"]);

window.__fixtures = fixtures;
/**
 * The fixture backend: every command answered from the fixtures above. In real
 * mode (`?real`, dev/ui-harness-real.js) the owner's app answers the reads and
 * this keeps playback simulated.
 */
const mock = {
  invoke: async (cmd, args = {}) => {
    if (playback.output_device_id && ["set_playback_speed", "preview_track_edit", "restore_preview"].includes(cmd)) {
      const error = "Playback speed and track-editor previews require This computer; Spotify devices play original audio";
      emit("playback-action-error", error);
      throw new Error(error);
    }
    switch (cmd) {
      case "plugin:event|listen": {
        const handlerId = args.handler ?? args.handlerId;
        return registerListener(args.event, handlerId);
      }
      case "plugin:event|unlisten":
      case "plugin:event|remove_listener":
        return unregisterListener(args.event, args.eventId, args.handler ?? args.handlerId);
      case "get_state":
        return { playback: snapshot(), playlists: clone(fixtures.playlists), playlist_tree: clone(fixtures.playlistTree), settings: clone(settings), me_id: "eduard" };
      case "get_track_playlists": {
        const track = findTrack(args.uri ?? args.trackId);
        const ids = track ? [...(memberships.get(track.id) || [])] : [];
        return ids.map((id) => ({ id, name: id === "liked" ? "Liked Songs" : playlistNames.get(id) || id }));
      }
      case "search":
        if (!/podcast|episode|listening|night/i.test(String(args.query ?? ""))) {
          return { top: null, tracks: [], albums: [], artists: [], playlists: [], shows: [], episodes: [] };
        }
        return {
          top: { kind: "show", ...clone(fixtures.show), episodes: undefined },
          tracks: [], albums: [], artists: [], playlists: [],
          shows: [{ ...clone(fixtures.show), episodes: undefined }],
          episodes: [clone(fixtures.episode)],
        };
      case "browse_show":
        return clone(fixtures.show);
      case "browse_episode":
        return clone(fixtures.show.episodes.find((episode) => episode.track.id === args.id) ?? fixtures.episode);
      case "browse_profile":
        return clone(args.username === "eduard" ? fixtures.profile : {
          username: args.username, name: "Another Listener", image_url: "",
          playlists: [fixtures.playlists[1]],
        });
      case "personal_api_status":
        return clone(personalStatus);
      case "personal_api_configure":
        personalStatus.client_id = args.clientId;
        personalStatus.connected = false;
        emit("personal-api-changed", clone(personalStatus));
        return clone(personalStatus);
      case "personal_api_authorize":
        personalStatus.authorization_pending = true;
        emit("personal-api-changed", clone(personalStatus));
        return { url: "https://accounts.spotify.com/authorize?fixture=1" };
      case "personal_api_disconnect":
        playback.output_device_id = "";
        playback.output_device_name = "";
        playback.playing = false;
        emitState();
        personalStatus.connected = false;
        personalStatus.authorization_pending = false;
        emit("personal-api-changed", clone(personalStatus));
        return clone(personalStatus);
      case "personal_api_contains":
        return args.uris.map((uri) => personalFollows.has(uri));
      case "personal_api_set_saved": {
        let liked = false;
        for (const uri of args.uris) {
          if (uri.startsWith("spotify:track:")) {
            // As the shell does: a landed like goes straight into the index.
            const id = uri.slice("spotify:track:".length);
            const set = memberships.get(id) ?? new Set();
            if (args.saved) set.add("liked");
            else set.delete("liked");
            memberships.set(id, set);
            liked = true;
          } else if (args.saved) personalFollows.add(uri);
          else personalFollows.delete(uri);
        }
        if (liked) emit("memberships_changed", { saved_tracks: true, uris: args.uris, saved: args.saved });
        return null;
      }
      case "personal_api_devices":
        if (!settings.devices_enabled) throw new Error("Spotify devices are turned off in Settings");
        return clone(outputDevices);
      case "select_output": {
        if (args.deviceId && !settings.devices_enabled) throw new Error("Spotify devices are turned off in Settings");
        const device = args.deviceId ? outputDevices.find((device) => device.id === args.deviceId) : null;
        if (args.deviceId && (!device || device.is_restricted || !personalStatus.devices_authorized)) {
          throw new Error("Spotify device is unavailable or unauthorized");
        }
        if (outputFailure) {
          emit("playback-action-error", outputFailure);
          throw new Error(outputFailure);
        }
        playback.output_device_id = device?.id ?? "";
        playback.output_device_name = device?.name ?? "";
        if (!device) playback.playing = false;
        playback.playback_speed = 1;
        playback.audible_playback_speed = 1;
        emitState();
        return null;
      }
      case "personal_api_saved_shows": {
        const offset = Number(args.offset ?? 0);
        const items = offset === 0 ? [{ added_at: "2026-09-20", show: {
          id: fixtures.show.id, uri: fixtures.show.uri, name: fixtures.show.name,
          description: fixtures.show.description, publisher: fixtures.show.publisher,
          images: [], total_episodes: fixtures.show.episodes.length,
        } }] : [];
        return { items, offset, limit: 50, total: 1, next: null, previous: null, href: "" };
      }
      case "browse_artist":
        return clone(fixtures.artist);
      case "browse_artist_songwriter":
        return clone(fixtures.songwriterPlaylist);
      case "browse_track":
        if (args.id !== fixtures.sharedTrack.id) throw new Error("This song is no longer available on Spotify.");
        return clone(fixtures.sharedTrack);
      case "browse_album":
        return clone(fixtures.album);
      case "browse_artist_catalogue": {
        // The engine pages a stable round-robin walk across the selected
        // release types; mirror that so page boundaries mix records the way
        // production pages do instead of draining one pool at a time.
        const wanted = Array.isArray(args.releaseTypes) && args.releaseTypes.length
          ? args.releaseTypes
          : ["albums", "singles", "compilations"];
        const pools = wanted.map((type) => cataloguePools[type]).filter(Array.isArray);
        const walk = [];
        for (let round = 0; ; round += 1) {
          let added = false;
          for (const pool of pools) {
            if (round < pool.length) {
              walk.push(pool[round]);
              added = true;
            }
          }
          if (!added) break;
        }
        const offset = Math.max(0, Number(args.offset ?? 0));
        const limit = Math.max(1, Number(args.limit ?? 4));
        return {
          releases: clone(walk.slice(offset, offset + limit)),
          total: walk.length,
          next_offset: offset + limit < walk.length ? offset + limit : null,
        };
      }
      case "browse_playlist":
        return clone(detailFor(args.id ?? args.playlistId));
      case "browse_canvas":
        return {
          url: "https://res.cloudinary.com/demo/video/upload/ar_9:16,c_fill,w_720/samples/sea-turtle.mp4",
        };
      case "set_app_settings":
      case "set_settings":
        Object.assign(settings, args.settings ?? args);
        emit("settings", clone(settings));
        return null;
      case "set_animated_canvas":
        settings.animated_canvas = Boolean(args.enabled ?? args.value);
        emit("settings", clone(settings));
        return null;
      /* As the shell does: off brings playback on a device back here, paused. */
      case "set_devices_enabled":
        settings.devices_enabled = Boolean(args.enabled);
        if (!settings.devices_enabled && playback.output_device_id) {
          playback.output_device_id = "";
          playback.output_device_name = "";
          playback.playing = false;
          emitState();
        }
        return clone(settings);
      /* The fixture account already has Canvas on: nothing to write. */
      case "enable_account_canvas":
        return false;
      /* Pages exactly as the engine does — filter and sort here, return a
         window plus the two counts — so the view is exercised against the
         real contract instead of a fixture that hands it everything. Every
         page served is recorded, which is how the paging can be measured. */
      case "get_history": {
        const needle = String(args.query ?? "").trim().toLowerCase();
        const sort = String(args.sort ?? "recent");
        const artistsOf = (row) => (row.track.artist_names ?? []).join(", ");
        let rows = needle
          ? fixtures.history.filter((row) =>
              row.track.name.toLowerCase().includes(needle) ||
              artistsOf(row).toLowerCase().includes(needle))
          : fixtures.history.slice();
        if (sort === "oldest") rows.reverse();
        else if (sort === "title") rows.sort((a, b) => a.track.name.localeCompare(b.track.name) || b.started_at - a.started_at);
        else if (sort === "artist") rows.sort((a, b) => artistsOf(a).localeCompare(artistsOf(b)) || a.track.name.localeCompare(b.track.name) || b.started_at - a.started_at);
        const offset = Math.max(0, Number(args.offset ?? 0));
        const limit = Math.max(1, Number(args.limit ?? 100));
        const entries = rows.slice(offset, offset + limit);
        historyPages.push({ offset, limit, query: needle, sort, served: entries.length });
        return {
          entries: clone(entries),
          total: rows.length,
          recorded: fixtures.history.length,
          offset,
          next_offset: offset + entries.length < rows.length ? offset + entries.length : null,
        };
      }
      case "get_track_waveform":
        return waveformFor(args.trackId ?? args.track_id, args.durationMs ?? args.duration_ms);
      case "get_track_edit": {
        const saved = edits.get(editKey(args.trackId ?? args.track_id, args.playlistId ?? args.playlist_id));
        return saved ? clone(saved) : { definition: null, enabled: false };
      }
      case "save_track_edit": {
        const trackId = args.trackId ?? args.track_id;
        const playlistId = args.playlistId ?? args.playlist_id ?? "p1";
        const track = findTrack(trackId);
        const definition = {
          track_id: trackId,
          duration_ms: args.durationMs ?? args.duration_ms ?? track?.duration_ms ?? 0,
          cuts: args.cuts ?? [],
          loop_range: args.loopRange ?? args.loop_range ?? null,
        };
        const current = edits.get(editKey(trackId, playlistId));
        edits.set(editKey(trackId, playlistId), { definition, enabled: current?.enabled ?? true });
        refreshQueueEdits();
        return clone(definition);
      }
      case "delete_track_edit":
        edits.delete(editKey(args.trackId ?? args.track_id, args.playlistId ?? args.playlist_id));
        refreshQueueEdits();
        return null;
      case "set_playlist_track_edit_enabled":
      case "toggle_track_edit": {
        const trackId = args.trackId ?? args.track_id;
        const key = editKey(trackId, args.playlistId ?? args.playlist_id);
        const current = edits.get(key) || { definition: null, enabled: false };
        current.enabled = args.enabled == null ? !current.enabled : Boolean(args.enabled);
        edits.set(key, current);
        refreshQueueEdits();
        return null;
      }
      case "preview_track_edit": {
        const track = findTrack(args.trackId ?? args.track_id);
        if (track) {
          playback.preview = true;
          playback.playing = true;
          playback.current_uri = track.uri;
          playback.duration_ms = track.duration_ms;
          playback.position_ms = 0;
        }
        emitState();
        return null;
      }
      case "add_playlist_tracks":
        updateMemberships(args, true);
        return null;
      case "remove_playlist_tracks":
        if (args.expectedSnapshotId != null && args.expectedSnapshotId !== detailFor(playlistIdFrom(args)).snapshot_id) {
          throw new Error("This playlist changed since your preview. Reload it and review the matches before removing songs.");
        }
        updateMemberships(args, false);
        return null;
      case "set_playlist_track_excluded": {
        const id = playlistIdFrom(args);
        applyTrackExclusion(id, args.trackId ?? args.track_id, Boolean(args.excluded));
        // Refresh the open detail so row marks and the header note agree.
        emit("playlist", clone(detailFor(id)));
        emitState();
        return null;
      }
      case "touch_playlist_activity":
        emit("playlist", clone(detailFor(args.id ?? args.playlistId)));
        return null;
      case "reorder_playlist_tracks":
      case "reorder_playlist": {
        const id = playlistIdFrom(args);
        const tracks = playlistTracks.get(id) || [];
        const from = Number(args.from ?? args.fromIndex);
        const to = Number(args.to ?? args.toIndex);
        if (Number.isInteger(from) && Number.isInteger(to) && tracks[from] && to >= 0 && to < tracks.length) {
          const [track] = tracks.splice(from, 1);
          tracks.splice(to, 0, track);
          emit("playlist", clone(detailFor(id)));
          emitPlaylistSummary(id);
        }
        return null;
      }
      case "play":
        playback.playing = true;
        playback.preview = false;
        emitState();
        return null;
      case "pause":
        playback.playing = false;
        emitState();
        return null;
      case "next":
      case "play_next":
        setCurrent(playback.current_index + 1);
        playback.playing = true;
        emitState();
        return null;
      case "previous":
      case "play_previous":
        setCurrent(playback.current_index - 1);
        playback.playing = true;
        emitState();
        return null;
      case "seek":
      case "set_position":
        playback.position_ms = Number(args.positionMs ?? args.position_ms ?? args.position ?? 0);
        emitState();
        return null;
      case "set_volume":
        playback.volume = Math.max(0, Math.min(100, Number(args.percent ?? args.volume ?? 0)));
        emitState();
        return null;
      case "set_shuffle":
        playback.shuffle = Boolean(args.enabled ?? args.shuffle);
        redrawShuffleBag();
        emitState();
        return null;
      case "set_repeat":
        playback.repeat = args.mode ?? args.repeat ?? "off";
        emitState();
        return null;
      case "set_playback_speed":
        playback[isEpisode(playback.current_uri) ? "episode_speed" : "track_speed"] = Number(args.speed ?? 1);
        applyKindSpeed();
        emitState();
        return null;
      case "play_queue": {
        const input = args.queue ?? args.tracks ?? args.uris;
        if (Array.isArray(input)) playback.queue = queueWithEdits(input.map(trackFromQueueItem), args.context ?? "");
        const automaticStart = Boolean(args.automaticStart ?? args.automatic_start);
        const requestedIndex = Number(args.index ?? args.startIndex ?? 0);
        const startedIndex = automaticStart
          ? automaticStartIndex(requestedIndex)
          : requestedIndex;
        playQueueLog.push({ automaticStart, requestedIndex, startedIndex });
        if (startedIndex === null) {
          playback.current_index = null;
          playback.current_uri = null;
          playback.duration_ms = 0;
          playback.position_ms = 0;
          playback.playing = false;
        } else {
          setCurrent(startedIndex);
          playback.playing = true;
        }
        // An automatic shuffle start has already drawn and consumed its first
        // row; redrawing here would replace the remaining published order.
        if (!(automaticStart && playback.shuffle)) redrawShuffleBag();
        playback.preview = false;
        emitState();
        return null;
      }
      case "play_queue_index":
        setCurrent(Number(args.index ?? args.queueIndex ?? 0));
        playback.playing = true;
        emitState();
        return null;
      case "add_to_queue":
      case "add_queue":
      case "add_queue_item": {
        const item = args.uri ?? args.trackUri ?? args.track_uri ?? args.track;
        playback.queue.push(...queueWithEdits([trackFromQueueItem(item)], args.context ?? ""));
        spliceIntoShuffleBag(playback.queue.length - 1);
        emitState();
        return null;
      }
      case "remove_queue": {
        const index = args.index ?? args.queueIndex;
        const uri = args.uri ?? args.trackUri;
        const at = index == null ? playback.queue.findIndex((item) => item.uri === uri) : Number(index);
        if (at >= 0 && at < playback.queue.length) {
          playback.queue.splice(at, 1);
          if (at < playback.current_index) playback.current_index -= 1;
          if (at === playback.current_index) setCurrent(Math.min(at, playback.queue.length - 1), false);
          repairShuffleBagAfterRemoval(at);
        }
        emitState();
        return null;
      }
      case "move_queue":
      case "reorder_queue": {
        const from = Number(args.from ?? args.fromIndex);
        const to = Number(args.to ?? args.toIndex);
        if (Number.isInteger(from) && Number.isInteger(to) && playback.queue[from] && to >= 0 && to < playback.queue.length) {
          const [entry] = playback.queue.splice(from, 1);
          playback.queue.splice(to, 0, entry);
          setCurrent(to, false);
        }
        emitState();
        return null;
      }
      case "clear_queue":
        playback.queue = [];
        setCurrent(0);
        emitState();
        return null;
      case "login":
      case "start_auth":
        // The real command binds the OAuth callback port before it answers,
        // so its rejection is the one thing standing between the user and a
        // browser tab that could never come back. Rejected with the bare
        // string, the way a Tauri `Result<_, String>` reaches the frontend.
        if (loginFailure) return Promise.reject(loginFailure);
        playback.auth_state = "ready";
        playback.username = "eduard";
        emit("session", { auth_state: "ready", username: "eduard", error: "" });
        emitState();
        return null;
      case "logout":
      case "sign_out":
        playback.auth_state = "logged_out";
        playback.username = "";
        playback.playing = false;
        emit("session", { auth_state: "logged_out", username: "", error: "" });
        emitState();
        return null;
      case "browse_followed_artists":
        // The whole collection in one answer, no cursor — the real endpoint
        // returns no cursor field of any kind, so neither does this.
        await followedRoundTrip();
        return clone(followedArtists);
      case "browse_track_credits":
        return clone(trackCreditsPayload(args.id ?? args.trackId ?? "t0"));
      case "get_cache_stats":
        return { entries: 0, bytes: 0 };
      default:
        return null;
    }
  },
};

/** Set before mount when the page was opened with `?real`. */
let realMode = null;
window.__TAURI_INTERNALS__ = {
  callbacks,
  transformCallback,
  runCallback,
  unregisterCallback,
  convertFileSrc: (source) => new URL(source, location.origin).href,
  invoke: async (cmd, args = {}) => {
    calls.push({ cmd, args });
    return realMode ? realMode.invoke(cmd, args) : mock.invoke(cmd, args);
  },
};

const settings = { animated_canvas: true, devices_enabled: true };
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener: (event, eventId, handlerId) => unregisterListener(event, eventId, handlerId),
};
window.__harness = {
  calls,
  playQueueLog,
  fixtures,
  callbacks,
  listeners,
  transformCallback,
  runCallback,
  unregisterCallback,
  listen: (event, handler, once = false) => {
    const handlerId = transformCallback(handler, once);
    const eventId = registerListener(event, handlerId);
    return Promise.resolve(() => unregisterListener(event, eventId, handlerId));
  },
  unlisten: unregisterListener,
  emit,
  invoke: (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args),
  setOutputFailure: (error = "") => { outputFailure = String(error); },
  setPersonalStatus: (changes) => {
    Object.assign(personalStatus, changes);
    emit("personal-api-changed", clone(personalStatus));
    return clone(personalStatus);
  },
  getState: snapshot,
  /** Deterministic skip controls for browser checks: patch the store,
      refresh the open detail, and report the resulting ids. No timers. */
  setExcluded: (trackIds, excluded = true, playlistId = "p1") => {
    for (const trackId of Array.isArray(trackIds) ? trackIds : [trackIds]) {
      applyTrackExclusion(playlistId, trackId, excluded);
    }
    emit("playlist", clone(detailFor(playlistId)));
    return [...(exclusionsFor(playlistId) ?? [])];
  },
  getExcluded: (playlistId = "p1") => [...(exclusionsFor(playlistId) ?? [])],
};

await import("../src/styles/app.css");
const { mount } = await import("svelte");
const { default: App } = await import("../src/App.svelte");
const state = await import("../src/lib/state.svelte.js");
Object.assign(window.__harness, {
  navigate: (name, id = null, param = null) => state.navigate(name, id, param),
  back: () => state.goBack(),
  forward: () => state.goForward(),
  navigateArtist: (id, name = "") => state.navigateArtist(id, name),
  selectCanvasDemo: () => {
    const track = playback.queue[playback.current_index];
    if (track) {
      track.cover_url =
        "https://res.cloudinary.com/demo/video/upload/so_0,ar_1:1,c_fill,w_720/samples/sea-turtle.jpg";
    }
    settings.animated_canvas = true;
    emit("settings", clone(settings));
    emitState();
  },
  openEditor: (trackId = playback.current_uri, playlistId = "p1") => {
    const track = findTrack(trackId);
    state.openTrackEditor(track, playlistId);
  },
  selectLongContent: () => {
    addPlaylistTracks("p1", [fixtures.longTrack.uri]);
    const playlist = fixtures.playlists.find((item) => item.id === "p1");
    playlist.name = "Road Trip — A Deliberately Long Playlist Name for Layout Checks";
    fixtures.playlistDetail.name = playlist.name;
    emit("library", clone(fixtures.playlists));
    emit("playlist", clone(detailFor("p1")));
    playback.queue = queueWithEdits(playlistTracks.get("p1"));
    setCurrent(playback.queue.length - 1);
    emitState();
  },
  selectEdited: () => {
    playback.queue = queueWithEdits(playlistTracks.get("p1"));
    setCurrent(0, false);
    emitState();
  },
  /**
   * Swap the independent songwriter-discovery response, then leave and
   * re-enter the artist route so its keyed post-overview request runs again.
   * Presence and absence are both deterministic states.
   */
  setSongwriterPlaylist: (value = null) => {
    fixtures.songwriterPlaylist = value ? clone(songwriterPlaylist) : null;
    if (ARTIST_ROUTE_NAMES.includes(state.route?.name)) state.navigate("library");
    state.navigate("artist", fixtures.artist.id);
    return clone(fixtures.songwriterPlaylist);
  },
  selectHistory: () => {
    historyPages.length = 0;
    return state.navigate("history");
  },
  /** What the view actually asked for, versus what the archive holds. */
  historyPages: () => ({
    requests: historyPages.length,
    rowsServed: historyPages.reduce((sum, page) => sum + page.served, 0),
    archive: fixtures.history.length,
    pages: clone(historyPages),
  }),
  /** Shrink or regrow the archive to exercise the empty and one-day cases. */
  setHistorySize: (count) => {
    fixtures.history = historyArchive().slice(0, Math.max(0, count));
    return fixtures.history.length;
  },
  /**
   * Replace the followed collection, then make the rail forget it has asked —
   * the read happens once per session, so swapping the fixture alone would
   * leave the rail showing the list it already has.
   */
  setFollowedArtists: (artists = []) => {
    followedArtists = artists.map((artist, index) => ({
      id: artist.id ?? `fa${index}`,
      uri: artist.uri ?? `spotify:artist:${artist.id ?? `fa${index}`}`,
      name: artist.name ?? `Artist ${index}`,
      cover_url: artist.cover_url ?? "",
    }));
    state.followed.loaded = false;
    state.followed.error = "";
    state.followed.artists = [];
    return followedArtists.length;
  },
  /**
   * Make the followed-artists read answer the way a refused round trip does.
   * Pass null to allow it again. The rail must show the refusal and offer the
   * retry rather than reading as an account that follows nobody.
   */
  setFollowedFailure: (message = "followed-artists request failed: 503 Service Unavailable") => {
    followedFailure = message || null;
    /* A recorded failure is what stops the rail asking again, so clearing it
       here is what lets the next switch actually re-request. */
    state.followed.loaded = false;
    state.followed.error = "";
    return followedFailure;
  },
  /** Make the read take as long as a real one, so the rail's loading frame is
      observable at all; the same reason `setCoverDelay` exists. */
  setFollowedDelay: (ms = 0) => (followedDelayMs = Math.max(0, Number(ms) || 0)),
  followedArtists: () => followedArtists.map((artist) => artist.id),
  /**
   * Put the app on the sign-in screen, which is otherwise unreachable here:
   * the harness boots an authenticated fixture, so LoginView — the first
   * surface a new machine ever shows — was never rendered in a browser check.
   */
  setLoggedOut: (authUrl = "https://accounts.spotify.com/authorize?fixture") => {
    playback.auth_state = "needs_login";
    playback.auth_url = authUrl;
    playback.username = "";
    playback.ready = false;
    playback.playing = false;
    state.session.username = null;
    state.session.error = null;
    state.session.authPending = false;
    emit("session", { auth_state: "needs_login", username: "", error: "" });
    emitState();
    return playback.auth_url;
  },
  /**
   * Make `login` refuse, the way the engine does when another process already
   * holds the fixed callback port. Pass null to allow it again. The browser
   * must NOT be opened in that case, and the reason has to land on screen.
   */
  setLoginFailure: (
    message = "another program is already using port 5588 on this machine, and Spotify can " +
      "only send the sign-in back to that exact port. Close whatever is holding it (or restart " +
      "the computer) and try again.",
  ) => {
    loginFailure = message || null;
    return loginFailure;
  },
  bootCachedLibrary: () => {
    // Stage 1 of the cached-then-fresh boot: hydrate from a cached get_state
    // snapshot — including one row the fresh rootlist will drop — without
    // marking it authoritative. Home must remain one cover-free skeleton.
    const stamp = Math.floor(Date.now() / 1000);
    state.libraryState.loaded = true;
    state.libraryState.fresh = false;
    state.setLibrary([
      {
        id: "p-cached-recent",
        name: "Cached Yesterday",
        owner_id: "eduard",
        tracks_total: 6,
        last_played: stamp - 3600,
        last_activity: stamp - 1800,
      },
      ...clone(fixtures.playlists),
    ]);
  },
  emitFreshLibrary: () => {
    // Stage 2: the authoritative `library` event. Omits the provisional row
    // and carries the engine's own recency for p1; the event handler is the
    // only writer that flips freshness back on, so final shelves mount once.
    const stamp = Math.floor(Date.now() / 1000);
    const [roadTrip, ...rest] = clone(fixtures.playlists);
    emit("library", {
      playlists: [{ ...roadTrip, last_played: stamp - 60, last_activity: stamp }, ...rest],
      playlist_tree: clone(fixtures.playlistTree),
    });
  },
});


if (new URLSearchParams(location.search).has("real")) {
  const { createRealMode } = await import("./ui-harness-real.js");
  // Missing live/cache state stays unavailable; real mode never uses a
  // fixture account in its place.
  realMode = await createRealMode({
    playback,
    settings,
    mock,
    state,
    emit,
    emitState,
    setCurrent,
    clone,
    redrawShuffleBag,
    spliceIntoShuffleBag,
    upcomingIndices,
    shuffleBag: {
      get: () => shuffleBag,
      set: (indices) => {
        shuffleBag = indices;
      },
    },
    snapshot,
  });
  if (realMode) window.__harness.real = realMode.helpers;
}

state.libraryState.loaded = true;
state.session.auth_state = "ready";
state.session.username = playback.username;
state.setLibrary(realMode ? realMode.library : fixtures.playlists, { tree: realMode ? realMode.tree : fixtures.playlistTree });
// Default boot is final/fresh so ordinary UI tests exercise the settled Home,
// shelves included. bootCachedLibrary() below rewinds to the staged boot.
state.libraryState.fresh = true;

mount(App, { target: document.getElementById("app") });
if (realMode) realMode.openScene();
else state.navigate("playlist", "p1");
document.body.dataset.harnessReady = "true";

console.info("[harness] ready — commands log at window.__calls");
