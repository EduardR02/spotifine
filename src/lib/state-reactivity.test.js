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

test("the probe can tell a proxy from a plain row", () => {
  expect(types.isProxy($.proxy([{ id: "x" }]))).toBe(true);
  expect(types.isProxy(row("x"))).toBe(false);
});

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
