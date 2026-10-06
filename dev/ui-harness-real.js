/**
 * Real mode for the UI harness: /dev/ui-harness.html?real
 *
 * Reads come from the owner's account through the dev server's bridge
 * (dev/real-bridge.js): live from the running app when `bun dev/real-app.js`
 * opened its DevTools port, otherwise from the bridge's cache or the app's
 * saved state. Playback stays simulated here: the harness boots on a local
 * copy of the real queue, and play/next/seek/queue changes move that copy and
 * emit the harness's usual `state` events. Nothing this page does can reach
 * the real player — the bridge refuses every command that is not a read.
 *
 * Covers stay remote https URLs (i.scdn.co and friends send
 * `Access-Control-Allow-Origin: *`, so covertone can read their pixels);
 * Canvas videos come from canvaz.scdn.co, which sends the same header.
 *
 * Helpers on `window.__harness.real` — see `helpers` at the bottom.
 */

import { READ_COMMANDS } from "./real-commands.js";

/** Handled by the fixture backend, because they only move local state that
    real data survives: events, the simulated transport, the canvas and
    devices settings, and the session screens. */
const LOCAL_MOCK = new Set([
  "plugin:event|listen",
  "plugin:event|unlisten",
  "plugin:event|remove_listener",
  "play",
  "pause",
  "previous",
  "seek",
  "set_volume",
  "set_shuffle",
  "set_repeat",
  "set_playback_speed",
  "play_queue_index",
  "remove_queue",
  "move_queue",
  "clear_queue",
  "set_animated_canvas",
  "set_devices_enabled",
  "enable_account_canvas",
  "login",
  "logout",
]);

/** How many queue rows a boot warms canvases and credits for. */
const WARM_QUEUE_LIMIT = 60;
const WARM_CONCURRENCY = 3;
/** A warmed answer younger than this is not asked for again on reload. */
const WARM_MAX_AGE_MS = 6 * 3600_000;

async function bridge(cmd, args = {}, extra = {}) {
  try {
    const response = await fetch("/__real/invoke", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ cmd, args, ...extra }),
    });
    return await response.json();
  } catch (error) {
    return { ok: false, source: "none", error: `bridge unreachable: ${error?.message ?? error}` };
  }
}

async function pool(items, limit, work) {
  let next = 0;
  const runners = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const item = items[next++];
      await work(item).catch(() => {});
    }
  });
  await Promise.all(runners);
}

export async function createRealMode(h) {
  const { playback, settings, mock, state, emit, emitState, setCurrent, clone } = h;

  const boot = await bridge("get_state");
  const available = boot.ok && !!boot.value?.playback;
  if (!available) console.error(`[real] no real state (${boot.error ?? boot.source}); no fixture account will be substituted.`);
  const bootSource = available ? boot.source : "none";
  const real = available ? boot.value : { playback: {}, playlists: [], me_id: "" };
  const library = real.playlists ?? [];
  const tree = real.playlist_tree ?? [];
  const meId = real.me_id ?? "";
  /** Where each command's last answer came from: live, cache, disk or mock. */
  const sources = {};
  /** Reads that were unavailable, with the reason. */
  const misses = {};
  /** Mutations refused by the read-only bridge, by command. */
  const ignored = {};
  /** uri → canvas url or null, as far as this page has learned. */
  const canvases = new Map();

  /* ------------------------------------------------ the simulated player */

  const seeded = real.playback;
  Object.assign(playback, {
    ready: seeded.ready === true,
    auth_state: seeded.auth_state || "needs_login",
    auth_url: seeded.auth_url || "",
    // Simulated playback runs, so the Canvas and the meters move; press
    // pause in the harness to see the paused surfaces.
    playing: available && !!seeded.queue?.length,
    preview: false,
    username: seeded.username || meId,
    position_ms: seeded.position_ms ?? 0,
    volume: seeded.volume ?? 70,
    shuffle: Boolean(seeded.shuffle),
    repeat: seeded.repeat ?? "off",
    playback_speed: seeded.playback_speed ?? 1,
    // The webview is only told the current item's speed; the harness starts
    // both kinds there.
    track_speed: seeded.track_speed ?? seeded.playback_speed ?? 1,
    episode_speed: seeded.episode_speed ?? seeded.playback_speed ?? 1,
    audible_playback_speed: seeded.audible_playback_speed ?? 1,
    queue: clone(seeded.queue ?? []),
    error: available ? seeded.error || "" : `Real account state unavailable: ${boot.error || "start the native app with its DevTools bridge."}`,
  });
  setCurrent(seeded.current_index ?? 0, false);
  playback.duration_ms = seeded.duration_ms || playback.queue[playback.current_index]?.duration_ms || 0;
  // The engine's own plan, when the snapshot carries one, so Up next shows
  // what the real player would have played; the bag pops from the back.
  if (playback.shuffle && seeded.upcoming?.length) h.shuffleBag.set([...seeded.upcoming].reverse());
  else h.redrawShuffleBag();

  Object.assign(settings, real.settings ?? {});

  function current() {
    return playback.queue[playback.current_index] ?? null;
  }

  function playIndex(index) {
    const length = playback.queue.length;
    if (!length) return null;
    const at = ((Number(index) % length) + length) % length;
    h.shuffleBag.set(h.shuffleBag.get().filter((pooled) => pooled !== at));
    setCurrent(at);
    playback.playing = true;
    playback.preview = false;
    emitState();
    return describe(at);
  }

  function describe(index) {
    const track = playback.queue[index];
    return track && { index, name: track.name, artists: (track.artist_names ?? []).join(", "), uri: track.uri, context: track.context };
  }

  /** The engine's walk: the published plan when there is one. */
  function next() {
    const plan = h.upcomingIndices();
    if (plan.length) return playIndex(plan[0]);
    if (playback.repeat === "off" && playback.current_index >= playback.queue.length - 1) return null;
    return playIndex(playback.current_index + 1);
  }

  function append(tracks, context = "") {
    for (const track of tracks) {
      playback.queue.push({ ...clone(track), context: track.context || context });
      h.spliceIntoShuffleBag(playback.queue.length - 1);
    }
    emitState();
  }

  /* ------------------------------------------------------------ invoke */

  async function forward(cmd, args) {
    const answer = await bridge(cmd, args);
    if (answer.ok) {
      sources[cmd] = answer.source;
      return answer.value;
    }
    sources[cmd] = answer.source || "none";
    misses[cmd] = answer.error;
    throw new Error(answer.source === "live" ? answer.error : `${cmd} is unavailable from the live app or its cache: ${answer.error || "no real data"}. Start the native app; no fixture data is used in real mode.`);
  }

  async function invoke(cmd, args = {}) {
    switch (cmd) {
      case "get_state":
        return { playback: h.snapshot(), playlists: clone(library), playlist_tree: clone(tree), settings: clone(settings), me_id: meId };
      case "play_queue": {
        playback.queue = Array.isArray(args.queue)
          ? args.queue.map((track) => ({ ...clone(track), context: track.context || args.context || "" }))
          : [];
        let start = Number(args.index ?? 0);
        if (args.automaticStart) {
          for (let step = 0; step < playback.queue.length; step += 1) {
            const at = (start + step) % playback.queue.length;
            if (!playback.queue[at].unavailable) {
              start = at;
              break;
            }
          }
        }
        setCurrent(start);
        h.redrawShuffleBag();
        playback.playing = true;
        playback.preview = false;
        emitState();
        return null;
      }
      case "add_queue":
        append([args.track].filter(Boolean), args.context);
        return null;
      case "add_queue_batch":
        append(Array.isArray(args.tracks) ? args.tracks : [], args.context);
        return null;
      case "next":
        next();
        return null;
    }
    if (READ_COMMANDS.has(cmd)) return forward(cmd, args);
    if (LOCAL_MOCK.has(cmd)) return mock.invoke(cmd, args);
    ignored[cmd] = (ignored[cmd] ?? 0) + 1;
    throw new Error(`${cmd} cannot run in the read-only real-account harness. Use the native Spotifine app for authorization, library changes, settings, or Spotify output selection.`);
  }

  /* ------------------------------------------------------------- scene */

  /** The surface the current track was started from. */
  function openScene() {
    const context = String(current()?.context ?? "");
    const [kind, id] = context.split(":");
    if (kind === "playlist" && id) state.navigate("playlist", id);
    else if (kind === "album" && id) state.navigate("album", id);
    else if (kind === "radio" && id) state.navigate("radio", id);
    else if (kind === "show" && id) state.navigate("show", id);
    else if (kind === "artist" && id) state.navigateArtist(id, current()?.artist_names?.[0] ?? "");
    else if (kind === "liked") state.navigate("liked");
    else state.navigate("library");
    return context || "library";
  }

  /* ---------------------------------------------------------- canvases */

  async function canvasOf(track, mode = "fill") {
    if (!track?.uri?.startsWith("spotify:track:")) return null;
    if (canvases.has(track.uri)) return canvases.get(track.uri);
    const answer = await bridge("browse_canvas", { id: track.id || track.uri }, { mode, maxAgeMs: WARM_MAX_AGE_MS });
    // Only a real answer counts; an outage is "unknown", not "no Canvas".
    if (!answer.ok) return undefined;
    const url = answer.value?.url ?? null;
    canvases.set(track.uri, url);
    return url;
  }

  async function canvasTracks() {
    const found = [];
    await pool(playback.queue.map((track, index) => ({ track, index })), WARM_CONCURRENCY, async ({ track, index }) => {
      const url = await canvasOf(track);
      if (url) found.push({ ...describe(index), canvas: url });
    });
    return found.sort((a, b) => a.index - b.index);
  }

  async function nextWithCanvas() {
    const length = playback.queue.length;
    for (let step = 1; step <= length; step += 1) {
      const index = (playback.current_index + step) % length;
      const url = await canvasOf(playback.queue[index]);
      if (url) return { ...playIndex(index), canvas: url };
    }
    return null;
  }

  /* ------------------------------------------------------------ warming */

  /**
   * Fills the bridge cache so the redesign keeps working after the app is
   * closed: canvases, credits and saved marks for the queue, the context the
   * current track came from, its album and artist, and the first page of the
   * followed artists, Liked Songs and history.
   *
   * Library playlists are deliberately not walked: the app keeps only its 25
   * most recently opened playlists in its tracks cache, so browsing the whole
   * library through it would evict the owner's own. The bridge already serves
   * those 25 from the app's saved file.
   */
  async function warm() {
    const started = performance.now();
    const jobs = [];
    const fill = (cmd, args) => jobs.push([cmd, args]);
    for (const track of playback.queue.slice(0, WARM_QUEUE_LIMIT)) {
      if (!track.id || !track.uri?.startsWith("spotify:track:")) continue;
      fill("browse_canvas", { id: track.id });
      fill("browse_track_credits", { id: track.id });
      fill("get_track_playlists", { uri: track.uri });
    }
    const now = current();
    const [kind, id] = String(now?.context ?? "").split(":");
    if (kind === "playlist" && id) fill("browse_playlist", { id });
    if (kind === "radio" && id) fill("browse_radio", { id });
    if (now?.uri?.startsWith("spotify:track:") && now.album_id) fill("browse_album", { id: now.album_id });
    fill("browse_followed_artists", {});
    fill("browse_liked_songs", { cursor: null });
    fill("get_history", { offset: 0, limit: 100, query: "", sort: "recent" });

    const tally = {};
    const run = async ([cmd, args]) => {
      const answer = await bridge(cmd, args, { mode: "fill", maxAgeMs: WARM_MAX_AGE_MS });
      const key = answer.ok ? answer.source : "failed";
      tally[key] = (tally[key] ?? 0) + 1;
      if (cmd === "browse_canvas" && answer.ok) {
        const track = playback.queue.find((row) => row.id === args.id);
        if (track) canvases.set(track.uri, answer.value?.url ?? null);
      }
      return answer;
    };
    await pool(jobs, WARM_CONCURRENCY, run);
    // The artist needs its own name for the songwriter lookup, as ArtistView does.
    const artistId = now?.artist_id || now?.artist_ids?.[0];
    if (artistId) {
      const artist = await run(["browse_artist", { id: artistId }]);
      if (artist.ok && artist.value?.name) await run(["browse_artist_songwriter", { id: artistId, name: artist.value.name }]);
    }
    const summary = { requests: jobs.length + (artistId ? 2 : 0), ...tally, ms: Math.round(performance.now() - started) };
    console.info("[real] cache warmed", summary);
    return summary;
  }

  /* ------------------------------------------------------------ status */

  const status = await fetch("/__real/status").then((r) => r.json()).catch(() => null);
  document.body.dataset.harnessReal = bootSource;
  document.title = `UI harness — real (${bootSource})`;
  console.info(
    `[real] booted from ${bootSource}: ${library.length} playlists, ${playback.queue.length} queued, ` +
      `now "${current()?.name ?? "nothing"}" from ${current()?.context || "no context"}. ` +
      (status?.live ? "The app is live." : `The app is not live (${status?.error ?? "bridge unreachable"}).`) +
      " Helpers: window.__harness.real",
  );
  const warming = status?.live ? warm() : Promise.resolve({ skipped: "the app is not live" });

  const helpers = {
    /** The bridge's view: live?, port, page origin, cache size. */
    status: () => fetch("/__real/status").then((r) => r.json()),
    /** Where each command was last answered from. */
    sources: () => ({ boot: bootSource, ...sources }),
    misses: () => ({ ...misses }),
    ignored: () => ({ ...ignored }),
    library: () => clone(library),
    queue: () => playback.queue.map((_, index) => describe(index)),
    current: () => describe(playback.current_index),
    playIndex,
    next,
    /** Queue rows that have a Canvas (asks the bridge for unknown ones). */
    canvasTracks,
    /** Hop to the next queue row that has a Canvas. */
    nextWithCanvas,
    canvasOf: (index = playback.current_index) => canvasOf(playback.queue[index]),
    openNowPlaying: (open = true) => state.setNowPlayingOpen(open),
    /** Back to the surface the current track was started from. */
    openScene,
    /** Resolves with the boot's warm-up summary. */
    warming: () => warming,
    warm,
    /** Push a raw state event, e.g. after editing `playback` by hand. */
    emitState,
    emit,
  };

  return { invoke, library, tree, openScene, helpers };
}
