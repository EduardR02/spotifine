import { afterEach, beforeEach, expect, test } from "bun:test";
import { applyCacheMarks } from "./cache-marks.js";

globalThis.$state = (value) => value;
const { api, applyPlayback, playback, detail, search, watchCacheMarks, loadCataloguePage, loadPlaylistRecommendations } = await import("./state.svelte.js");
let requests;
let previousWindow;
const track = (id, cached = true) => ({ id, uri: `spotify:track:${id}`, cached });

beforeEach(() => {
  previousWindow = globalThis.window;
  requests = [];
  globalThis.window = { __TAURI_INTERNALS__: {
    invoke: (command, args) => new Promise((resolve, reject) => requests.push({ command, args, resolve, reject })),
  } };
});
afterEach(() => { globalThis.window = previousWindow; });

test("negative IDs update off-queue rows, duplicates and listeners without replacing arrays", () => {
  const rows = [track("a"), track("b"), track("a")];
  applyPlayback({ queue: rows, current_index: 0 });
  detail.album = { tracks: [track("a"), track("outside")] };
  search.results = { tracks: [track("a")] };
  const visible = [track("a"), track("outside")];
  const off = watchCacheMarks((ids, cached) => applyCacheMarks(visible, ids, cached));
  try {
    const held = playback.queue;
    applyPlayback({ uncached_ids: ["a", "outside"] });
    expect(playback.queue).toBe(held);
    expect(rows.map((row) => row.cached)).toEqual([false, true, false]);
    expect(detail.album.tracks.map((row) => row.cached)).toEqual([false, false]);
    expect(search.results.tracks[0].cached).toBe(false);
    expect(visible.map((row) => row.cached)).toEqual([false, false]);
  } finally { off(); }
});

test("reset and negative marks precede fresh explicit positives in the same payload", () => {
  applyPlayback({ queue: [track("a"), track("b"), track("c")], cache_marks_reset: true,
    uncached_ids: ["a", "b"], cached_ids: ["b"] });
  expect(playback.queue.map((row) => row.cached)).toEqual([false, true, false]);
});

test("a late playlist response cannot revive evicted IDs but keeps unaffected marks", async () => {
  const pending = api.browsePlaylist("late");
  applyPlayback({ uncached_ids: ["a"] });
  requests[0].resolve({ tracks: [track("a"), track("b")] });
  const page = await pending;
  expect(page.tracks.map((row) => row.cached)).toEqual([false, true]);
});

test("reset fences late initial-state cached_ids as well as queue row flags", async () => {
  const pending = api.getState();
  applyPlayback({ cache_marks_reset: true });
  requests[0].resolve({ playback: { queue: [track("a")], cached_ids: ["a"] } });
  const snapshot = await pending;
  applyPlayback(snapshot.playback);
  expect(playback.queue[0].cached).toBe(false);
  expect(snapshot.playback.cached_ids).toEqual([]);
});

test("a genuine later download wins over an earlier invalidation in an outstanding read", async () => {
  const pending = api.browseAlbum("later");
  applyPlayback({ uncached_ids: ["a"] });
  applyPlayback({ cached_ids: ["a"] });
  requests[0].resolve({ tracks: [track("a", false)] });
  expect((await pending).tracks[0].cached).toBe(true);
});

test("fresh reads after reset may report fresh presence without retaining old tombstones", async () => {
  applyPlayback({ cache_marks_reset: true });
  const pending = api.browseTrack("fresh");
  requests[0].resolve(track("fresh"));
  expect((await pending).cached).toBe(true);
});

test("negative marks reach already kept catalogue and recommendation pages", async () => {
  const catalogue = loadCataloguePage("negative-catalogue", ["albums"], 0, 1);
  requests[0].resolve({ releases: [{ tracks: [track("kept")] }], next_offset: null });
  const page = await catalogue;
  const recommendations = loadPlaylistRecommendations("negative-playlist", "revision");
  requests[1].resolve({ tracks: [track("kept")] });
  const tracks = await recommendations;
  applyPlayback({ uncached_ids: ["kept"] });
  expect(page.releases[0].tracks[0].cached).toBe(false);
  expect(tracks[0].cached).toBe(false);
  expect(await loadPlaylistRecommendations("negative-playlist", "revision")).toBe(tracks);
});

for (const [name, read, payload, row] of [
  ["catalogue", () => api.browseArtistCatalogue("late"), () => ({ releases: [{ tracks: [track("a")] }] }), (p) => p.releases[0].tracks[0]],
  ["podcast", () => api.browseShow("late"), () => ({ episodes: [{ track: track("a") }] }), (p) => p.episodes[0].track],
  ["history", () => api.getHistory(), () => ({ entries: [{ track: track("a") }] }), (p) => p.entries[0].track],
]) {
  test(`reset fences a late ${name} response`, async () => {
    const pending = read();
    applyPlayback({ cache_marks_reset: true });
    requests[0].resolve(payload());
    expect(row(await pending).cached).toBe(false);
  });
}
