import { afterAll, beforeAll, expect, test } from "bun:test";
import { compileModule } from "svelte/compiler";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { types } from "node:util";

/* Every other state test runs the hub UNcompiled, with `$state` stubbed to
   return its argument — right for identity, blind to reactivity. These
   questions are about reactivity: whether a raw list hands IPC plain rows,
   whether a mark written into a plain row still reaches the screen, whether
   the playhead clock stops and resumes. So this file compiles the real module
   with Svelte's compiler and runs it on Svelte's real client runtime. It is a
   second instance of the hub, private to this file, so the shared uncompiled
   one the other tests use is untouched. */
const here = dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"));
const runtimeUrl = pathToFileURL(Bun.resolveSync("svelte/internal/client", here)).href;
let outDir;
let state;
let $;

function compileHub() {
  const file = resolve(here, "state.svelte.js");
  const { js } = compileModule(readFileSync(file, "utf8"), { filename: file, generate: "client" });
  // Absolute imports: the compiled copy lives outside the tree, and must share
  // the runtime and every plain module with what this test imports.
  const code = js.code.replace(/(from\s+|import\s*\(\s*)(["'])([^"']+)\2/g, (_, lead, quote, spec) => {
    const target = spec.startsWith(".") ? resolve(here, spec) : Bun.resolveSync(spec, here);
    if (target.endsWith(".svelte.js")) throw new Error(`compile ${spec} too before relying on it here`);
    return `${lead}${quote}${pathToFileURL(target).href}${quote}`;
  });
  outDir = mkdtempSync(join(tmpdir(), "spotifine-hub-"));
  const out = join(outDir, "state.compiled.js");
  writeFileSync(out, code);
  return pathToFileURL(out).href;
}

/* The Tauri bridge: commands are recorded, event subscriptions captured so a
   test can deliver `playlist` and `position` events itself. */
const commands = [];
const handlers = new Map();
const callbacks = new Map();
let callbackId = 0;
let previousWindow;

beforeAll(async () => {
  previousWindow = globalThis.window;
  globalThis.window = {
    __TAURI_INTERNALS__: {
      transformCallback(callback) {
        callbacks.set(++callbackId, callback);
        return callbackId;
      },
      invoke(command, args) {
        if (command === "plugin:event|listen") {
          handlers.set(args.event, callbacks.get(args.handler));
          return Promise.resolve(args.handler);
        }
        if (command === "get_state") return new Promise(() => {});
        commands.push({ command, args });
        return Promise.resolve(null);
      },
    },
  };
  $ = await import(runtimeUrl);
  state = await import(compileHub());
  await state.initEvents();
});

afterAll(() => {
  if (previousWindow === undefined) delete globalThis.window;
  else globalThis.window = previousWindow;
  if (outDir) rmSync(outDir, { recursive: true, force: true });
});

const row = (id, extra = {}) => ({
  id, uri: `spotify:track:${id}`, name: `Track ${id}`, artist_names: ["Artist"], artist_ids: ["artist"],
  duration_ms: 180_000, cached: false, context: "", ...extra,
});

/** Runs `read` as a render effect and returns every value it produced. */
function watch(read) {
  const seen = [];
  const stop = $.effect_root(() => {
    $.render_effect(() => {
      seen.push(read());
    });
  });
  return { seen, stop };
}

function lastCommand(name) {
  return commands.filter((entry) => entry.command === name).at(-1)?.args;
}

test("a playlist page is a raw snapshot: Play hands IPC the plain rows themselves", async () => {
  const rows = [row("a"), row("b"), row("c")];
  state.detail.playlist = { id: "p-plain", tracks: rows };
  const open = state.detail.playlist.tracks;
  expect(open).toBe(rows);
  expect(types.isProxy(open[0])).toBe(false);

  await state.api.playQueue(open, 1, "playlist:p-plain");
  const play = lastCommand("play_queue");
  expect(types.isProxy(play.queue)).toBe(false);
  expect(play.queue).toBe(rows);
  expect(play.queue.some((track) => types.isProxy(track))).toBe(false);

  // The sorted projection PlaylistView plays is a new array of the same rows.
  await state.api.playQueue(open.slice().reverse(), 0, "playlist:p-plain");
  expect(lastCommand("play_queue").queue.some((track) => types.isProxy(track))).toBe(false);

  // A row that claims another context is the one rewritten copy; still plain.
  await state.api.addQueueBatch([row("d"), row("e", { context: "album:x" })], "playlist:p-plain");
  const batch = lastCommand("add_queue_batch");
  expect(types.isProxy(batch.tracks)).toBe(false);
  expect(batch.tracks.some((track) => types.isProxy(track))).toBe(false);
  expect(batch.tracks[1].context).toBe("playlist:p-plain");
});

test("a download mark written into a plain playlist row re-renders that row's mark", () => {
  state.detail.playlist = { id: "p-marks", tracks: [row("m1"), row("m2"), row("m3")] };
  const rows = state.detail.playlist.tracks;
  const view = watch(() => state.detail.playlist.tracks.map((track) => state.isCached(track)));
  try {
    expect(view.seen.at(-1)).toEqual([false, false, false]);
    state.applyPlayback({ cached_ids: ["m2"] });
    $.flush();
    expect(view.seen.at(-1)).toEqual([false, true, false]);
    // In place: the same list, the same row objects, still plain.
    expect(state.detail.playlist.tracks).toBe(rows);
    expect(types.isProxy(rows[1])).toBe(false);

    state.applyPlayback({ uncached_ids: ["m2"] });
    $.flush();
    expect(view.seen.at(-1)).toEqual([false, false, false]);
  } finally {
    view.stop();
  }
});

test("a mark reaches a view's own raw list through its cache-mark listener", () => {
  // LikedSongsView's shape: a raw list, patched by a watchCacheMarks listener.
  const list = $.state([row("l1"), row("l2")]);
  const off = state.watchCacheMarks((ids, cached) => {
    for (const track of $.get(list)) if (ids === null || ids.has(track.id)) track.cached = ids === null ? false : cached;
  });
  const view = watch(() => $.get(list).map((track) => state.isCached(track)));
  try {
    state.applyPlayback({ cached_ids: ["l2"] });
    $.flush();
    expect(view.seen.at(-1)).toEqual([false, true]);
  } finally {
    view.stop();
    off();
  }
});

test("a background refresh keeps the open list when nothing moved and replaces only the row that did", () => {
  state.navigate("playlist", "p-refresh");
  const rows = [row("r1"), row("r2", { play_count: 5 }), row("r3")];
  state.detail.playlist = { id: "p-refresh", name: "Before", tracks: rows };
  const deliver = (payload) => handlers.get("playlist")({ payload });
  const counts = watch(() => state.detail.playlist?.tracks.map((track) => track.play_count ?? null));
  try {
    deliver({ id: "p-refresh", name: "After", tracks: rows.map((track) => ({ ...track })) });
    $.flush();
    expect(state.detail.playlist.name).toBe("After");
    expect(state.detail.playlist.tracks).toBe(rows);

    deliver({ id: "p-refresh", name: "After", tracks: rows.map((track, i) => ({ ...track, play_count: i === 1 ? 6 : track.play_count })) });
    $.flush();
    const refreshed = state.detail.playlist.tracks;
    expect(refreshed).not.toBe(rows);
    expect(refreshed[0]).toBe(rows[0]);
    expect(refreshed[2]).toBe(rows[2]);
    expect(refreshed[1]).not.toBe(rows[1]);
    expect(refreshed[1].play_count).toBe(6);
    expect(rows[1].play_count).toBe(5);
    expect(counts.seen.at(-1)).toEqual([null, 6, null]);

    const other = [row("r9")];
    deliver({ id: "p-refresh", tracks: other });
    $.flush();
    expect(state.detail.playlist.tracks).toBe(other);
  } finally {
    counts.stop();
    state.detail.playlist = null;
  }
});

test("the queue is held as the plain rows the engine sent, and what shows it still follows it", () => {
  const rows = [row("raw-q1", { name: "First" }), row("raw-q2", { name: "Second" }), row("raw-q3", { name: "Third" })];
  state.applyPlayback({ queue: rows, queue_revision: 900, upcoming: [1, 2], order_revision: 900, current_index: 0 });
  expect(state.playback.queue).toBe(rows);
  expect(types.isProxy(state.playback.queue)).toBe(false);
  expect(types.isProxy(state.playback.queue[0])).toBe(false);
  expect(types.isProxy(state.playback.upcoming)).toBe(false);

  // The player bar's track, the panel's "next" and the marks on queue rows.
  const nowPlaying = watch(() => state.playback.queue[state.playback.current_index]?.name);
  const upNext = watch(() => state.playback.queue[state.playback.upcoming[0]]?.name ?? null);
  const marks = watch(() => state.playback.queue.map((track) => state.isCached(track)));
  try {
    expect(nowPlaying.seen.at(-1)).toBe("First");
    expect(upNext.seen.at(-1)).toBe("Second");
    expect(marks.seen.at(-1)).toEqual([false, false, false]);

    // A download mark is written into the held row and still re-renders it.
    state.applyPlayback({ cached_ids: ["raw-q2"] });
    $.flush();
    expect(marks.seen.at(-1)).toEqual([false, true, false]);
    expect(state.playback.queue).toBe(rows);

    // The same generation again keeps the rows; a new one replaces them whole.
    state.applyPlayback({ queue: rows.map((track) => ({ ...track })), queue_revision: 900, position_ms: 1_000 });
    expect(state.playback.queue).toBe(rows);
    const fresh = [row("raw-q4", { name: "Fourth" }), row("raw-q2", { name: "Second", cached: true })];
    state.applyPlayback({ queue: fresh, queue_revision: 901, upcoming: [0], order_revision: 901, current_index: 1 });
    $.flush();
    expect(state.playback.queue).toBe(fresh);
    expect(nowPlaying.seen.at(-1)).toBe("Second");
    expect(upNext.seen.at(-1)).toBe("Fourth");
    expect(marks.seen.at(-1)).toEqual([false, true]);

    // An order-only change moves "next" and leaves the rows alone.
    state.applyPlayback({ upcoming: [], order_revision: 902 });
    $.flush();
    expect(upNext.seen.at(-1)).toBeNull();
    expect(state.playback.queue).toBe(fresh);
  } finally {
    nowPlaying.stop();
    upNext.stop();
    marks.stop();
  }
});

/* The playhead clock: a fake interval and a fake monotonic clock, so the test
   can see how many tickers run and what the progress bar would show. */
function withFakeClock(run) {
  const realSetInterval = globalThis.setInterval;
  const realClearInterval = globalThis.clearInterval;
  const realNow = performance.now;
  const timers = new Map();
  let nextTimer = 0;
  const clock = { now: 1_000_000 };
  globalThis.setInterval = (callback) => {
    timers.set(++nextTimer, callback);
    return nextTimer;
  };
  globalThis.clearInterval = (id) => timers.delete(id);
  performance.now = () => clock.now;
  const tick = () => { for (const callback of timers.values()) callback(); };
  try {
    run({ timers, clock, tick });
  } finally {
    state.applyPlayback({ playing: false });
    state.setPlayheadVisible(true);
    globalThis.setInterval = realSetInterval;
    globalThis.clearInterval = realClearInterval;
    performance.now = realNow;
  }
}

test("the playhead ticker stops while the window cannot be seen and resumes at the time it is now", () => {
  withFakeClock(({ timers, clock, tick }) => {
    state.setPlayheadVisible(true);
    state.applyPlayback({ playing: true, buffering: false, audible_playback_speed: 1, duration_ms: 600_000, position_ms: 10_000 });
    expect(timers.size).toBe(1);
    const bar = watch(() => state.positionMs());
    try {
      clock.now += 1_000;
      tick();
      $.flush();
      expect(bar.seen.at(-1)).toBe(11_000);

      // Minimized: no ticker, and nothing that arrives while hidden starts one.
      state.setPlayheadVisible(false);
      expect(timers.size).toBe(0);
      state.applyPlayback({ playing: true, buffering: false });
      expect(timers.size).toBe(0);
      const rendersWhileHidden = bar.seen.length;
      clock.now += 30_000;
      $.flush();
      expect(bar.seen.length).toBe(rendersWhileHidden);

      // Restored: the very next render shows now, not the time it was hidden at,
      // and exactly one ticker runs again.
      state.setPlayheadVisible(true);
      $.flush();
      expect(bar.seen.at(-1)).toBe(41_000);
      expect(timers.size).toBe(1);

      // The shell's resync on restore lands on that anchor and corrects it.
      clock.now += 200;
      handlers.get("position")({ payload: 41_150 });
      $.flush();
      expect(bar.seen.at(-1)).toBe(41_150);
      expect(timers.size).toBe(1);
      clock.now += 250;
      tick();
      $.flush();
      expect(bar.seen.at(-1)).toBe(41_400);
    } finally {
      bar.stop();
    }
  });
});

test("a resync that beats the restore, and a pause while hidden, both come back right", () => {
  withFakeClock(({ timers, clock }) => {
    state.setPlayheadVisible(true);
    state.applyPlayback({ playing: true, buffering: false, audible_playback_speed: 1, duration_ms: 600_000, position_ms: 50_000 });
    const bar = watch(() => state.positionMs());
    try {
      state.setPlayheadVisible(false);
      clock.now += 5_000;
      handlers.get("position")({ payload: 54_900 });
      expect(timers.size).toBe(0);
      clock.now += 1_000;
      state.setPlayheadVisible(true);
      $.flush();
      expect(bar.seen.at(-1)).toBe(55_900);
      expect(timers.size).toBe(1);

      state.setPlayheadVisible(false);
      clock.now += 2_000;
      state.applyPlayback({ playing: false, position_ms: 57_000 });
      clock.now += 60_000;
      state.setPlayheadVisible(true);
      $.flush();
      expect(timers.size).toBe(0);
      expect(bar.seen.at(-1)).toBe(57_000);
    } finally {
      bar.stop();
    }
  });
});

const settle = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
let operationAccount = 0;

/** Isolated deferred IPC against the compiled production hub, not a store stub. */
async function withDeferredCommands(run) {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const pending = [];
  state.navigate("library");
  state.applyPlayback({ auth_state: "ready", username: `compiled-operations-${++operationAccount}`,
    playing: false, shuffle: false, current_index: -1, current_uri: null, context: "", queue: [] });
  window.__TAURI_INTERNALS__.invoke = (command, args) =>
    new Promise((resolve, reject) => pending.push({ command, args, resolve, reject }));
  try {
    await run(pending);
  } finally {
    state.applySession({ auth_state: "logged_out", username: null });
    state.applyPlayback({ playing: false });
    window.__TAURI_INTERNALS__.invoke = invoke;
  }
}

test("a delayed catalogue shuffle reply cannot issue a play over a newer playlist", async () => {
  await withDeferredCommands(async (pending) => {
    state.playback.shuffle = true;
    const obsolete = state.playCatalogueContext([{ tracks: [row("old-catalogue")] }], "old-artist", ["albums"], 4, 0);
    expect(pending.map((request) => request.command)).toEqual(["set_shuffle"]);
    const newer = state.api.playQueue([row("new-playlist")], 0, "playlist:new");
    pending[1].resolve(null);
    await newer;
    pending[0].resolve(null);
    await obsolete;
    await settle();
    expect(pending.map((request) => request.command)).toEqual(["set_shuffle", "play_queue"]);
    expect(pending[1].args.queue[0].id).toBe("new-playlist");
    expect(pending[1].args.context).toBe("playlist:new");
  });
});

test("a newer catalogue operation owns both its play command and its lazy tail", async () => {
  await withDeferredCommands(async (pending) => {
    state.playback.shuffle = true;
    const old = state.playCatalogueContext([{ tracks: [row("old")] }], "old-artist", ["albums"], 4, 0);
    const current = state.playCatalogueContext([{ tracks: [row("current")] }], "current-artist", ["singles"], 8, 0);
    pending[1].resolve(null);
    await settle();
    expect(pending[2].command).toBe("play_queue");
    state.applyPlayback({ current_index: 0, current_uri: row("current").uri, context: "artist:current-artist",
      queue: [row("current")], queue_revision: 0 });
    pending[2].resolve(null);
    await current;
    expect(pending[3].command).toBe("browse_artist_catalogue");
    expect(pending[3].args.id).toBe("current-artist");
    pending[0].resolve(null);
    await old;
    pending[3].resolve({ releases: [{ tracks: [row("tail")] }], next_offset: null });
    await settle();
    expect(pending[4].command).toBe("add_queue_batch");
    expect(pending[4].args.context).toBe("artist:current-artist");
    pending[4].resolve(null);
    await settle();
    expect(pending.filter((request) => request.command === "play_queue")).toHaveLength(1);
  });
});

test("append operations do not supersede a catalogue play awaiting shuffle", async () => {
  await withDeferredCommands(async (pending) => {
    state.playback.shuffle = true;
    const play = state.playCatalogueContext([{ tracks: [row("catalogue")] }], "artist", ["albums"], null, 0);
    const addition = state.api.addQueueBatch([row("queued")], "playlist:extra");
    pending[1].resolve(null);
    await addition;
    pending[0].resolve(null);
    await settle();
    expect(pending[2].command).toBe("play_queue");
    expect(pending[2].args.queue[0].id).toBe("catalogue");
    pending[2].resolve(null);
    await play;
  });
});

test("queue-index and transport choices supersede an awaiting catalogue without losing existing lazy tails", async () => {
  await withDeferredCommands(async (pending) => {
    const tracks = Array.from({ length: 12 }, (_, i) => row(`existing-${i}`));
    state.applyPlayback({ queue: tracks, current_index: 0, current_uri: tracks[0].uri, queue_revision: 0 });
    const setup = state.playCatalogueContext([{ tracks }], "existing-artist", ["albums"], 4, 0);
    pending[0].resolve(null);
    await setup;
    const choice = state.api.playQueueIndex(11);
    pending[1].resolve(null);
    await choice;
    state.applyPlayback({ current_index: 11, current_uri: tracks[11].uri });
    const backfill = state.maybeBackfillLazyQueue();
    expect(pending[2].command).toBe("browse_artist_catalogue");
    const identity = state.playbackIdentity();
    pending[2].resolve({ releases: [{ tracks: [row("tail")] }], next_offset: null });
    await settle();
    pending[3].resolve(null);
    await backfill;
    expect(state.playbackIdentity()).toBe(identity);
    state.playback.shuffle = true;
    const obsolete = state.playCatalogueContext([{ tracks: [row("obsolete")] }], "obsolete", ["albums"], null, 0);
    const next = state.api.next();
    pending[5].resolve(null);
    await next;
    pending[4].resolve(null);
    await obsolete;
    expect(pending.map((request) => request.command)).toEqual([
      "play_queue", "play_queue_index", "browse_artist_catalogue", "add_queue_batch", "set_shuffle", "next",
    ]);
  });
});

test("failed library removal cannot resurrect a row or tree excluded by a later rootlist", async () => {
  await withDeferredCommands(async (pending) => {
    const child = { kind: "playlist", id: "removed" };
    state.setLibrary([{ id: "removed", name: "Removed" }, { id: "kept", name: "Kept" }],
      { fresh: true, tree: [child, { kind: "playlist", id: "kept" }] });
    const view = watch(() => state.library.map((playlist) => playlist.id));
    try {
      const removal = state.removeFromLibrary("removed");
      $.flush();
      expect(view.seen.at(-1)).toEqual(["kept"]);
      // An authoritative list need not carry a tree to supersede the removal.
      handlers.get("library")({ payload: { playlists: [{ id: "kept", name: "Fresh kept" }] } });
      pending[0].reject(new Error("late failure"));
      expect(await removal).toBe(false);
      $.flush();
      expect(view.seen.at(-1)).toEqual(["kept"]);
      expect(state.libraryState.tree).toEqual([{ kind: "playlist", id: "kept" }]);
      expect(state.library[0].name).toBe("Fresh kept");
    } finally {
      view.stop();
    }
  });
});

test("failed removal restores its owned row and folder baseline before the next serialized edit", async () => {
  await withDeferredCommands(async (pending) => {
    const tree = [{ kind: "folder", id: "folder", name: "Before", children: [{ kind: "playlist", id: "restore" }] }];
    state.setLibrary([{ id: "restore", name: "Restore" }], { fresh: true, tree });
    const removal = state.removeFromLibrary("restore");
    const rename = state.renameLibraryFolder("folder", "After");
    expect(pending).toHaveLength(1);
    pending[0].reject(new Error("removal failed"));
    expect(await removal).toBe(false);
    await settle();
    expect(state.library.map((playlist) => playlist.id)).toEqual(["restore"]);
    expect(state.libraryState.tree[0].children).toEqual(tree[0].children);
    expect(state.libraryState.tree[0].name).toBe("After");
    pending[1].reject(new Error("rename failed"));
    expect(await rename).toBe(false);
    expect(state.libraryState.tree).toEqual(tree);
  });
});

test("an authoritative reappearance keeps the fresh row and placement on removal failure", async () => {
  await withDeferredCommands(async (pending) => {
    state.setLibrary([{ id: "same", name: "Before" }], { tree: [{ kind: "playlist", id: "same" }] });
    const removal = state.removeFromLibrary("same");
    const freshTree = [{ kind: "folder", id: "fresh-folder", name: "Fresh", children: [{ kind: "playlist", id: "same" }] }];
    handlers.get("library")({ payload: { playlists: [{ id: "same", name: "Fresh row" }], playlist_tree: freshTree } });
    pending[0].reject(new Error("late failure"));
    expect(await removal).toBe(false);
    expect(state.library.map((playlist) => playlist.name)).toEqual(["Fresh row"]);
    expect(state.libraryState.tree).toEqual(freshTree);
  });
});

const cachePayload = (audio, covers) => ({ audio: { files: audio, bytes: audio * 100 }, covers: { files: covers, bytes: covers * 100 } });

test("a pre-clear cache scan cannot overwrite the clear result or release a newer scan's loading owner", async () => {
  await withDeferredCommands(async (pending) => {
    const old = state.refreshCacheStats();
    const clear = state.clearCache("audio");
    pending[1].resolve(cachePayload(0, 2));
    await clear;
    const current = state.refreshCacheStats();
    const shared = state.refreshCacheStats();
    expect(pending.filter((request) => request.command === "get_cache_stats")).toHaveLength(2);
    pending[0].resolve(cachePayload(9, 9));
    await old;
    expect(state.cacheStats.audio.files).toBe(0);
    expect(state.cacheStats.covers.files).toBe(2);
    expect(state.cacheStats.loading).toBe(true);
    const sharedAfterOld = state.refreshCacheStats();
    expect(pending.filter((request) => request.command === "get_cache_stats")).toHaveLength(2);
    pending[2].resolve(cachePayload(1, 2));
    await current;
    await Promise.all([shared, sharedAfterOld]);
    expect(state.cacheStats.audio.files).toBe(1);
    expect(state.cacheStats.loading).toBe(false);
  });
});

test("obsolete cache errors and finally cannot replace a fresh error or clear its pending request", async () => {
  await withDeferredCommands(async (pending) => {
    const old = state.refreshCacheStats().catch((error) => error);
    const clear = state.clearCache("covers");
    pending[1].resolve(cachePayload(3, 0));
    await clear;
    const current = state.refreshCacheStats();
    const outcome = current.catch((error) => error);
    pending[0].reject(new Error("obsolete scan"));
    expect((await old).message).toBe("obsolete scan");
    expect(state.cacheStats.error).toBeNull();
    expect(state.cacheStats.loading).toBe(true);
    const shared = state.refreshCacheStats().catch((error) => error);
    expect(pending.filter((request) => request.command === "get_cache_stats")).toHaveLength(2);
    pending[2].reject(new Error("fresh scan"));
    await outcome;
    await shared;
    expect(state.cacheStats.error).toContain("fresh scan");
    expect(state.cacheStats.loading).toBe(false);
  });
});

test("a failed clear preserves a newer successful read and does not steal a newer read error", async () => {
  await withDeferredCommands(async (pending) => {
    const firstClear = state.clearCache("audio").catch((error) => error);
    const fresh = state.refreshCacheStats();
    pending[1].resolve(cachePayload(5, 6));
    await fresh;
    pending[0].reject(new Error("clear failed"));
    await firstClear;
    expect(state.cacheStats.audio.files).toBe(5);
    expect(state.cacheStats.error).toBeNull();
    const secondClear = state.clearCache("covers").catch((error) => error);
    const failedRead = state.refreshCacheStats().catch((error) => error);
    pending[3].reject(new Error("new read failed"));
    await failedRead;
    pending[2].reject(new Error("older clear failed"));
    await secondClear;
    expect(state.cacheStats.error).toContain("new read failed");
    expect(state.cacheStats.audio.files).toBe(5);
    expect(state.cacheStats.loading).toBe(false);
  });
});

test("concurrent cache kinds are serialized and only the final complete snapshot is retained", async () => {
  await withDeferredCommands(async (pending) => {
    const audio = state.clearCache("audio");
    const covers = state.clearCache("covers");
    expect(pending.map((request) => request.args.kind)).toEqual(["audio"]);
    pending[0].resolve(cachePayload(0, 7));
    await audio;
    await settle();
    expect(pending.map((request) => request.args.kind)).toEqual(["audio", "covers"]);
    const scan = state.refreshCacheStats();
    pending[1].resolve(cachePayload(0, 0));
    await covers;
    pending[2].resolve(cachePayload(0, 7));
    await scan;
    expect(state.cacheStats.audio.files).toBe(0);
    expect(state.cacheStats.covers.files).toBe(0);
    expect(state.cacheStats.loading).toBe(false);
  });
});

test("logical playback identity is reactive across duplicate rows and stable through transport and appended rows", async () => {
  await withDeferredCommands(async (pending) => {
    state.applyPlayback({ queue: [row("duplicate"), row("duplicate")], current_index: 0,
      current_uri: row("duplicate").uri, context: "playlist:duplicates", output_device_id: "local", queue_revision: 10 });
    const view = watch(() => state.playbackIdentity());
    try {
      const held = state.playbackIdentity();
      state.applyPlayback({ position_ms: 500, volume: 40, queue_revision: 10 });
      state.applyPlayback({ queue: [row("duplicate"), row("duplicate"), row("tail")], queue_revision: 11 });
      $.flush();
      expect(state.playbackIdentity()).toBe(held);
      expect(view.seen).toHaveLength(1);
      state.applyPlayback({ current_index: 1, current_uri: row("duplicate").uri });
      $.flush();
      expect(view.seen.at(-1)).not.toBe(held);
      const nextRow = state.playbackIdentity();
      const replay = state.api.playQueueIndex(1);
      expect(state.playbackIdentity()).not.toBe(nextRow);
      pending[0].resolve(null);
      await replay;
      const local = state.playbackIdentity();
      state.applyPlayback({ output_device_id: "phone" });
      expect(state.playbackIdentity()).not.toBe(local);
    } finally {
      view.stop();
    }
  });
});

test("account reset clears library and detail and fences pending catalogue and serialized collection writes", async () => {
  await withDeferredCommands(async (pending) => {
    const tree = [{ kind: "folder", id: "folder", name: "Private", children: [{ kind: "playlist", id: "private" }] }];
    state.setLibrary([{ id: "private", name: "Private" }], { fresh: true, tree });
    state.detail.playlist = { id: "private", tracks: [row("private")] };
    state.playback.shuffle = true;
    const play = state.playCatalogueContext([{ tracks: [row("private")] }], "private-artist", ["albums"], 4, 0);
    const removal = state.removeFromLibrary("private");
    const rename = state.renameLibraryFolder("folder", "Queued private");
    const identity = state.playbackIdentity();
    const epoch = state.sessionEpoch();
    state.applySession({ auth_state: "logged_out", username: null });
    expect(state.sessionEpoch()).not.toBe(epoch);
    expect(state.playbackIdentity()).not.toBe(identity);
    expect(state.library).toEqual([]);
    expect(state.libraryState.tree).toEqual([]);
    expect(state.libraryState.loaded).toBe(false);
    expect(state.libraryState.fresh).toBe(false);
    expect(state.detail.playlist).toBeNull();
    state.applyPlayback({ auth_state: "ready", username: "compiled-new-account", playing: false });
    state.setLibrary([{ id: "new-account", name: "New account" }], { fresh: true, tree: [] });
    pending[0].resolve(null);
    pending[1].reject(new Error("private write failed"));
    await play;
    expect(await removal).toBe(false);
    expect(await rename).toBe(false);
    await settle();
    expect(pending.map((request) => request.command)).toEqual(["set_shuffle", "delete_playlist"]);
    expect(state.library.map((playlist) => playlist.id)).toEqual(["new-account"]);
    expect(state.libraryState.tree).toEqual([]);
    expect(state.detail.playlist).toBeNull();
  });
});

test("a backfill already in flight remains single-flight across an explicit queue-index choice", async () => {
  await withDeferredCommands(async (pending) => {
    const tracks = [row("first"), row("second")];
    state.applyPlayback({ queue: tracks, current_index: 0, current_uri: tracks[0].uri, queue_revision: 0 });
    const play = state.playCatalogueContext([{ tracks }], "in-flight-tail", ["albums"], 4, 0);
    pending[0].resolve(null);
    await play;
    expect(pending[1].command).toBe("browse_artist_catalogue");
    const firstBackfill = state.maybeBackfillLazyQueue();
    const choice = state.api.playQueueIndex(1);
    pending[2].resolve(null);
    await choice;
    const secondBackfill = state.maybeBackfillLazyQueue();
    pending[1].resolve({ releases: [{ tracks: [row("third")] }], next_offset: null });
    await settle();
    expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(1);
    expect(pending.filter((request) => request.command === "add_queue_batch")).toHaveLength(1);
    pending[3].resolve(null);
    await firstBackfill;
    await secondBackfill;
    await state.maybeBackfillLazyQueue();
    expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(1);
    expect(pending.filter((request) => request.command === "add_queue_batch")).toHaveLength(1);
  });
});

test("a next waiting for catalogue backfill cannot skip the replacement playlist", async () => {
  await withDeferredCommands(async (pending) => {
    const first = row("last");
    state.applyPlayback({ queue: [first], current_index: 0, current_uri: first.uri, queue_revision: 0 });
    const setup = state.playCatalogueContext([{ tracks: [first] }], "next-tail", ["albums"], 4, 0);
    pending[0].resolve(null);
    await setup;
    const next = state.api.next();
    const replacement = state.api.playQueue([row("replacement")], 0, "playlist:replacement");
    pending[2].resolve(null);
    await replacement;
    pending[1].resolve({ releases: [{ tracks: [row("old-tail")] }], next_offset: null });
    await next;
    expect(pending.map((request) => request.command)).toEqual(["play_queue", "browse_artist_catalogue", "play_queue"]);
  });
});

for (const wasPlaying of [true, false]) {
  test(`${wasPlaying ? "Pause" : "Play"} pressed while an artist play-all is starting applies to the playback that starts`, async () => {
    await withDeferredCommands(async (pending) => {
      const current = row(`press-current-${wasPlaying}`);
      state.applyPlayback({ queue: [current], queue_revision: 0, current_index: 0, current_uri: current.uri,
        playing: wasPlaying, shuffle: true });
      const start = state.playCatalogueContext([{ tracks: [row("press-start")] }], "press-artist", ["albums"], null, 0);
      const identity = state.playbackIdentity();
      state.togglePlay();
      // Shown at once and sent with the start's queue, so it neither cancels
      // the start nor lands on the playback being replaced.
      expect(state.playback.playing).toBe(!wasPlaying);
      expect(state.playbackIdentity()).toBe(identity);
      expect(pending.map((request) => request.command)).toEqual(["set_shuffle"]);
      pending[0].resolve(null);
      await settle();
      expect(pending[1].command).toBe("play_queue");
      expect(pending[1].args.queue[0].id).toBe("press-start");
      expect(pending[1].args.startPaused).toBe(wasPlaying);
      pending[1].resolve(null);
      await start;
      await settle();
      // One command: no play-then-pause, nothing after the queue.
      expect(pending.map((request) => request.command)).toEqual(["set_shuffle", "play_queue"]);
      expect(state.playback.playing).toBe(!wasPlaying);
    });
  });
}

test("a Pause pressed once the start's queue has gone out follows it", async () => {
  await withDeferredCommands(async (pending) => {
    const current = row("press-late-current");
    state.applyPlayback({ queue: [current], queue_revision: 0, current_index: 0, current_uri: current.uri,
      playing: true, shuffle: false });
    // Shuffle already off: the queue goes out at once, before the press.
    const start = state.playCatalogueContext([{ tracks: [row("press-late")] }], "press-late", ["albums"], null, 0);
    expect(pending[0].command).toBe("play_queue");
    expect(pending[0].args.startPaused).toBe(false);
    state.togglePlay();
    expect(pending).toHaveLength(1);
    pending[0].resolve(null);
    await start;
    await settle();
    expect(pending.map((request) => request.command)).toEqual(["play_queue", "pause"]);
    pending[1].resolve(null);
    await settle();
    expect(state.playback.playing).toBe(false);
  });
});

test("a Pause carried by a start that fails pauses the playback already there", async () => {
  await withDeferredCommands(async (pending) => {
    const current = row("press-failed-current");
    state.applyPlayback({ queue: [current], queue_revision: 0, current_index: 0, current_uri: current.uri,
      playing: true, shuffle: true });
    const start = state.playCatalogueContext([{ tracks: [row("press-failed")] }], "press-failed", ["albums"], null, 0)
      .catch((error) => error);
    state.togglePlay();
    pending[0].resolve(null);
    await settle();
    expect(pending[1].args.startPaused).toBe(true);
    pending[1].reject(new Error("The device went away."));
    expect(await start).toBeInstanceOf(Error);
    await settle();
    expect(pending.map((request) => request.command)).toEqual(["set_shuffle", "play_queue", "pause"]);
    pending[2].resolve(null);
    await settle();
    expect(state.playback.playing).toBe(false);
  });
});

test("a newer choice of what plays takes a waiting Play/Pause with the start it was for", async () => {
  await withDeferredCommands(async (pending) => {
    const current = row("press-superseded-current");
    state.applyPlayback({ queue: [current], queue_revision: 0, current_index: 0, current_uri: current.uri,
      playing: true, shuffle: true });
    const start = state.playCatalogueContext([{ tracks: [row("press-superseded")] }], "press-superseded", ["albums"], null, 0);
    state.togglePlay();
    const replacement = state.api.playQueue([row("press-replacement")], 0, "playlist:press-replacement");
    pending[1].resolve(null);
    await replacement;
    pending[0].resolve(null);
    await start;
    await settle();
    // The replacement plays; nothing pauses it on behalf of a start that never happened.
    expect(pending.map((request) => request.command)).toEqual(["set_shuffle", "play_queue"]);
    expect(pending[1].args.context).toBe("playlist:press-replacement");
    expect(pending[1].args.startPaused).toBeUndefined();
    expect(state.playback.playing).toBe(true);
  });
});

for (const outcome of ["arrives", "fails"]) {
  test(`a Next from the last loaded row is sent after its page ${outcome}, whatever is pressed meanwhile`, async () => {
    await withDeferredCommands(async (pending) => {
      const id = `next-page-${outcome}`;
      const last = row(`${id}-last`);
      state.applyPlayback({ queue: [last], queue_revision: 0, current_index: 0, current_uri: last.uri, playing: true });
      const setup = state.playCatalogueContext([{ tracks: [last] }], id, ["albums"], 4, 0);
      pending[0].resolve(null);
      await setup;
      expect(pending[1].command).toBe("browse_artist_catalogue");
      state.ui.error = null;
      try {
        const next = state.api.next();
        // Space while the Next waits: paused at once, and the Next still stands.
        const pause = state.api.pause();
        expect(pending.map((request) => request.command)).toEqual(["play_queue", "browse_artist_catalogue", "pause"]);
        pending[2].resolve(null);
        await pause;
        if (outcome === "arrives") {
          pending[1].resolve({ releases: [{ tracks: [row(`${id}-page`)] }], next_offset: null });
          await settle();
          expect(pending[3].command).toBe("add_queue_batch");
          pending[3].resolve(null);
        } else {
          pending[1].reject(new Error("The network is unreachable."));
        }
        await settle();
        expect(pending.at(-1).command).toBe("next");
        pending.at(-1).resolve(null);
        await next;
        if (outcome === "fails") expect(state.ui.error).toContain("The network is unreachable.");
        else expect(state.ui.error).toBeNull();
      } finally {
        state.ui.error = null;
      }
    });
  });
}

test("skips pressed while a Next waits for its page are each sent, in the order pressed", async () => {
  await withDeferredCommands(async (pending) => {
    const last = row("skip-order-last");
    state.applyPlayback({ queue: [last], queue_revision: 0, current_index: 0, current_uri: last.uri });
    const setup = state.playCatalogueContext([{ tracks: [last] }], "skip-order", ["albums"], 4, 0);
    pending[0].resolve(null);
    await setup;
    const presses = [state.api.next(), state.api.next(), state.api.previous()];
    expect(pending.map((request) => request.command)).toEqual(["play_queue", "browse_artist_catalogue"]);
    pending[1].resolve({ releases: [{ tracks: [row("skip-order-a"), row("skip-order-b")] }], next_offset: null });
    await settle();
    pending[2].resolve(null);
    await settle();
    expect(pending.map((request) => request.command)).toEqual([
      "play_queue", "browse_artist_catalogue", "add_queue_batch", "next", "next", "previous",
    ]);
    for (const request of pending.slice(3)) request.resolve(null);
    await Promise.all(presses);
    // Once the backlog has drained, a skip goes straight out again.
    const later = state.api.previous();
    expect(pending.at(-1).command).toBe("previous");
    expect(pending).toHaveLength(7);
    pending.at(-1).resolve(null);
    await later;
  });
});

test("removing or moving a queued song keeps an artist play-all loading the rest", async () => {
  await withDeferredCommands(async (pending) => {
    const tracks = Array.from({ length: 12 }, (_, i) => row(`edit-tail-${i}`));
    state.applyPlayback({ queue: tracks, queue_revision: 0, current_index: 0, current_uri: tracks[0].uri,
      context: "artist:edit-tail" });
    const play = state.playCatalogueContext([{ tracks }], "edit-tail", ["albums"], 6, 0);
    pending[0].resolve(null);
    await play;
    const removal = state.api.removeQueue(3);
    pending[1].resolve(null);
    await removal;
    const kept = tracks.filter((_, i) => i !== 3);
    state.applyPlayback({ queue: kept, queue_revision: 0 });
    const move = state.api.moveQueue(5, 2);
    pending[2].resolve(null);
    await move;
    // Playback reaches the last rows loaded: the next releases still come.
    state.applyPlayback({ current_index: 10, current_uri: kept[10].uri });
    const tail = state.maybeBackfillLazyQueue();
    expect(pending[3].command).toBe("browse_artist_catalogue");
    expect(pending[3].args.id).toBe("edit-tail");
    expect(pending[3].args.offset).toBe(6);
    pending[3].resolve({ releases: [{ tracks: [row("edit-tail-next")] }], next_offset: null });
    await settle();
    expect(pending[4].command).toBe("add_queue_batch");
    expect(pending[4].args.tracks.map((track) => track.id)).toEqual(["edit-tail-next"]);
    expect(pending[4].args.context).toBe("artist:edit-tail");
    pending[4].resolve(null);
    await tail;
  });
});

test("a failed clear retains measured usage and fences an older scan while reporting its own error", async () => {
  await withDeferredCommands(async (pending) => {
    const measured = state.refreshCacheStats();
    pending[0].resolve(cachePayload(4, 5));
    await measured;
    const oldScan = state.refreshCacheStats();
    const clear = state.clearCache("audio").catch((error) => error);
    pending[2].reject(new Error("disk busy"));
    await clear;
    pending[1].resolve(cachePayload(99, 99));
    await oldScan;
    expect(state.cacheStats.audio.files).toBe(4);
    expect(state.cacheStats.covers.files).toBe(5);
    expect(state.cacheStats.error).toContain("disk busy");
    expect(state.cacheStats.loading).toBe(false);
  });
});

test("account switch fences pending collection pages and late completions cannot replace the new cached first page", async () => {
  await withDeferredCommands(async (pending) => {
    const old = state.api.browseLikedSongs().catch((error) => error);
    const oldCatalogue = state.loadCataloguePage("shared-artist", ["albums"], 0, 4).catch((error) => error);
    state.applyPlayback({ auth_state: "ready", username: "collection-new-account", playing: false });
    const current = state.api.browseLikedSongs();
    const catalogue = state.loadCataloguePage("shared-artist", ["albums"], 0, 4);
    pending[2].resolve({ tracks: [row("new-saved")], next_cursor: null });
    pending[3].resolve({ releases: [{ tracks: [row("new-release")] }], next_offset: null });
    const newPage = await current;
    const newCatalogue = await catalogue;
    pending[0].resolve({ tracks: [row("old-saved")], next_cursor: null });
    pending[1].resolve({ releases: [{ tracks: [row("old-release")] }], next_offset: null });
    expect(await old).toBeInstanceOf(Error);
    expect(await oldCatalogue).toBeInstanceOf(Error);
    expect(await state.api.browseLikedSongs()).toBe(newPage);
    expect(await state.loadCataloguePage("shared-artist", ["albums"], 0, 4)).toBe(newCatalogue);
    expect(newPage.tracks[0].id).toBe("new-saved");
    expect(newCatalogue.releases[0].tracks[0].id).toBe("new-release");
    expect(pending).toHaveLength(4);
  });
});

for (const selection of ["rejected device", "same device", "owned handoff"]) {
  test(`${selection} preserves the installed catalogue cursor and bounded tail read`, async () => {
    await withDeferredCommands(async (pending) => {
      const tracks = Array.from({ length: 12 }, (_, i) => row(`device-tail-${i}`));
      state.applyPlayback({ queue: tracks, current_index: 0, current_uri: tracks[0].uri,
        output_device_id: "local", context: "artist:device-tail", queue_revision: 0 });
      const setup = state.playCatalogueContext([{ tracks }], "device-tail", ["albums"], 7, 0);
      pending[0].resolve(null);
      await setup;
      const identity = state.playbackIdentity();
      const choose = state.api.selectOutput(selection === "same device" ? "local" : "phone").catch((error) => error);
      expect(state.playbackIdentity()).not.toBe(identity);
      if (selection === "rejected device") {
        pending[1].reject(new Error("device unavailable"));
        expect(await choose).toBeInstanceOf(Error);
      } else {
        if (selection === "owned handoff") {
          state.applyPlayback({ output_device_id: "phone", queue: tracks.map((track) => ({ ...track })), queue_revision: 0 });
        }
        pending[1].resolve(null);
        await choose;
      }
      state.applyPlayback({ current_index: 11, current_uri: tracks[11].uri });
      const first = state.maybeBackfillLazyQueue();
      const shared = state.maybeBackfillLazyQueue();
      expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(1);
      expect(pending[2].args.id).toBe("device-tail");
      expect(pending[2].args.offset).toBe(7);
      pending[2].resolve({ releases: [{ tracks: [row("device-tail-next")] }], next_offset: null });
      await settle();
      expect(pending.filter((request) => request.command === "add_queue_batch")).toHaveLength(1);
      expect(pending[3].args.tracks.map((track) => track.id)).toEqual(["device-tail-next"]);
      expect(pending[3].args.context).toBe("artist:device-tail");
      pending[3].resolve(null);
      await Promise.all([first, shared]);
    });
  });
}

async function installPreviewCatalogue(pending, atTail = false) {
  const id = `preview-catalogue-${operationAccount}`;
  const tracks = Array.from({ length: 12 }, (_, i) => row(`${id}-${i}`));
  state.applyPlayback({ queue: tracks, queue_revision: 0, current_index: atTail ? 11 : 0,
    current_uri: tracks[atTail ? 11 : 0].uri, context: `artist:${id}` });
  const setup = state.playCatalogueContext([{ tracks }], id, ["albums"], 9, 0);
  pending[0].resolve(null);
  await setup;
  return { id, tracks };
}

for (const restore of ["none", "success", "refusal"]) {
  test(`a refused preview retains the catalogue tail after ${restore} restore`, async () => {
    await withDeferredCommands(async (pending) => {
      const { id, tracks } = await installPreviewCatalogue(pending);
      const lease = state.allocatePreviewLeaseId();
      const preview = state.api.previewTrackEdit(row("preview"), [], null, 0, lease).catch((error) => error);
      pending[1].reject(new Error("remote preview unavailable"));
      expect(await preview).toBeInstanceOf(Error);
      if (restore !== "none") {
        const restored = state.api.restorePreview(lease).catch((error) => error);
        if (restore === "refusal") pending[2].reject(new Error("no preview to restore"));
        else pending[2].resolve(null);
        await restored;
      }
      state.applyPlayback({ current_index: 11, current_uri: tracks[11].uri });
      const tail = state.maybeBackfillLazyQueue();
      const page = pending.at(-1);
      expect(page.command).toBe("browse_artist_catalogue");
      expect(page.args.id).toBe(id);
      expect(page.args.offset).toBe(9);
      page.resolve({ releases: [{ tracks: [row("retained-preview-tail")] }], next_offset: null });
      await settle();
      const append = pending.at(-1);
      expect(append.command).toBe("add_queue_batch");
      expect(append.args.tracks[0].id).toBe("retained-preview-tail");
      append.resolve(null);
      await tail;
      expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(1);
    });
  });
}

for (const failedUpdate of [false, true]) {
  test(`an accepted preview suppresses an in-flight tail until matching restore${failedUpdate ? " even after a refused update" : ""}`, async () => {
    await withDeferredCommands(async (pending) => {
      const { tracks } = await installPreviewCatalogue(pending, true);
      const lease = state.allocatePreviewLeaseId();
      const preview = state.api.previewTrackEdit(row("preview"), [], null, 0, lease);
      pending[2].resolve(null);
      await preview;
      if (failedUpdate) {
        const update = state.api.previewTrackEdit(row("preview"), [], null, 100, lease).catch((error) => error);
        pending[3].reject(new Error("update refused"));
        expect(await update).toBeInstanceOf(Error);
      }
      pending[1].resolve({ releases: [{ tracks: [row("suspended-tail")] }], next_offset: null });
      await settle();
      await state.maybeBackfillLazyQueue();
      expect(pending.filter((request) => request.command === "add_queue_batch")).toHaveLength(0);
      const refusedRestore = state.api.restorePreview(lease).catch((error) => error);
      pending.at(-1).reject(new Error("restore refused"));
      expect(await refusedRestore).toBeInstanceOf(Error);
      await state.maybeBackfillLazyQueue();
      expect(pending.filter((request) => request.command === "add_queue_batch")).toHaveLength(0);
      state.applyPlayback({ queue: tracks, queue_revision: 0, current_index: 11, current_uri: tracks[11].uri });
      const restored = state.api.restorePreview(lease);
      pending.at(-1).resolve(null);
      await restored;
      await settle();
      expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(1);
      const appends = pending.filter((request) => request.command === "add_queue_batch");
      expect(appends).toHaveLength(1);
      expect(appends[0].args.tracks[0].id).toBe("suspended-tail");
      appends[0].resolve(null);
      await settle();
    });
  });
}

test("an old preview restore cannot cancel a newer catalogue start or resurrect its former tail", async () => {
  await withDeferredCommands(async (pending) => {
    await installPreviewCatalogue(pending);
    const lease = state.allocatePreviewLeaseId();
    const preview = state.api.previewTrackEdit(row("preview"), [], null, 0, lease);
    pending[1].resolve(null);
    await preview;
    state.playback.shuffle = true;
    const tracks = Array.from({ length: 12 }, (_, i) => row(`replacement-preview-${i}`));
    const newer = state.playCatalogueContext([{ tracks }], "replacement-preview", ["albums"], 5, 0);
    const staleRestore = state.api.restorePreview(lease);
    pending[3].resolve(null);
    await staleRestore;
    pending[2].resolve(null);
    await settle();
    expect(pending[4].command).toBe("play_queue");
    state.applyPlayback({ queue: tracks, queue_revision: 0, current_index: 11, current_uri: tracks[11].uri });
    pending[4].resolve(null);
    await newer;
    expect(pending[5].command).toBe("browse_artist_catalogue");
    expect(pending[5].args.id).toBe("replacement-preview");
    expect(pending[5].args.offset).toBe(5);
    pending[5].resolve({ releases: [{ tracks: [row("replacement-tail")] }], next_offset: null });
    await settle();
    expect(pending[6].command).toBe("add_queue_batch");
    expect(pending[6].args.context).toBe("artist:replacement-preview");
    pending[6].resolve(null);
    await settle();
    expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(1);
  });
});

test("a stale preview lease restore cannot release the current preview's suspended tail", async () => {
  await withDeferredCommands(async (pending) => {
    const { tracks } = await installPreviewCatalogue(pending);
    const oldLease = state.allocatePreviewLeaseId(), currentLease = state.allocatePreviewLeaseId();
    const first = state.api.previewTrackEdit(row("first-preview"), [], null, 0, oldLease);
    pending[1].resolve(null);
    await first;
    const current = state.api.previewTrackEdit(row("current-preview"), [], null, 0, currentLease);
    pending[2].resolve(null);
    await current;
    state.applyPlayback({ current_index: 11, current_uri: tracks[11].uri });
    const stale = state.api.restorePreview(oldLease);
    pending[3].resolve(null);
    await stale;
    await state.maybeBackfillLazyQueue();
    expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(0);
    const restored = state.api.restorePreview(currentLease);
    pending[4].resolve(null);
    await restored;
    await settle();
    expect(pending[5].command).toBe("browse_artist_catalogue");
    pending[5].resolve({ releases: [{ tracks: [row("current-lease-tail")] }], next_offset: null });
    await settle();
    expect(pending[6].command).toBe("add_queue_batch");
    expect(pending[6].args.tracks[0].id).toBe("current-lease-tail");
    pending[6].resolve(null);
    await settle();
  });
});

test("a first preview refusal lets its already-running catalogue read drain exactly once", async () => {
  await withDeferredCommands(async (pending) => {
    await installPreviewCatalogue(pending, true);
    const lease = state.allocatePreviewLeaseId();
    const preview = state.api.previewTrackEdit(row("preview"), [], null, 0, lease).catch((error) => error);
    pending[2].reject(new Error("preview refused"));
    expect(await preview).toBeInstanceOf(Error);
    pending[1].resolve({ releases: [{ tracks: [row("resumed-in-flight-tail")] }], next_offset: null });
    await settle();
    const appends = pending.filter((request) => request.command === "add_queue_batch");
    expect(appends).toHaveLength(1);
    expect(appends[0].args.tracks[0].id).toBe("resumed-in-flight-tail");
    appends[0].resolve(null);
    await settle();
    await state.maybeBackfillLazyQueue();
    expect(pending.filter((request) => request.command === "browse_artist_catalogue")).toHaveLength(1);
    expect(pending.filter((request) => request.command === "add_queue_batch")).toHaveLength(1);
  });
});
