import { expect, test } from "bun:test";
import { applyCacheMarks } from "./cache-marks.js";

// Match the existing playback-state tests: these assertions concern row
// identity and metadata transitions, not Svelte rendering.
globalThis.$state = Object.assign((value) => value, { raw: (value) => value });
const { applyPlayback, playback, detail, search, api, clearCache, watchCacheMarks, loadCataloguePage } = await import("./state.svelte.js");

const track = (id, cached = false) => ({ id, uri: `spotify:track:${id}`, cached });

function reset() {
  applyPlayback({ queue: [], queue_revision: 0, upcoming: [], order_revision: 0,
    current_index: -1, current_uri: null, playing: false, context: "", output_device_id: "" });
  for (const key of ["playlist", "album", "artist", "radio"]) detail[key] = null;
}

test("download IDs update duplicate queue rows and detail-only songs without replacing queue or order", () => {
  reset();
  applyPlayback({ queue: [track("shared"), track("shared"), track("other")],
    queue_revision: 9001, upcoming: [2, 1], order_revision: 9001, current_index: 1 });
  const queue = playback.queue;
  const order = playback.upcoming;
  const rows = [...queue];
  detail.album = { tracks: [track("shared"), track("detail-only"), track("other")] };
  detail.artist = { top_tracks: [track("detail-only")] };
  const album = detail.album.tracks;

  applyPlayback({ queue_revision: 9001, order_revision: 9001, cached_ids: ["shared", "detail-only"] });

  expect(playback.queue).toBe(queue);
  expect(playback.upcoming).toBe(order);
  expect(playback.current_index).toBe(1);
  expect(queue.every((row, index) => row === rows[index])).toBe(true);
  expect(queue.map((row) => row.cached)).toEqual([true, true, false]);
  expect(detail.album.tracks).toBe(album);
  expect(album.map((row) => row.cached)).toEqual([true, true, false]);
  expect(detail.artist.top_tracks[0].cached).toBe(true);
});

test("cache additions survive omitted, null and empty ID fields, including held incoming queue arrays", () => {
  reset();
  applyPlayback({ queue: [track("a"), track("b", true)], queue_revision: 9002,
    cached_ids: ["a"] });
  const held = playback.queue;
  for (const payload of [{}, { cached_ids: null }, { cached_ids: [] }, {
    queue: [track("a"), track("b")], queue_revision: 9002,
  }]) applyPlayback(payload);
  expect(playback.queue).toBe(held);
  expect(held.map((row) => row.cached)).toEqual([true, true]);
});

test("a cache-only update reaches an independently owned visible list even if its ID is absent from the queue", () => {
  reset();
  applyPlayback({ queue: [track("queue-only")], queue_revision: 9003 });
  const visible = [track("episode"), track("episode"), track("untouched")];
  const off = watchCacheMarks((ids) => applyCacheMarks(visible, ids));
  try {
    applyPlayback({ cached_ids: ["episode"] });
    expect(visible.map((row) => row.cached)).toEqual([true, true, false]);
    expect(playback.queue[0].cached).toBe(false);
  } finally {
    off();
  }
});

test("a delta accompanying a new queue applies to adopted rows rather than the replaced generation", () => {
  reset();
  applyPlayback({ queue: [track("old")], queue_revision: 9010 });
  const old = playback.queue[0];
  detail.album = { tracks: [track("new"), track("outside")] };
  applyPlayback({ queue: [track("new")], queue_revision: 9011,
    cached_ids: ["new", "outside"] });
  expect(playback.queue[0].cached).toBe(true);
  expect(old.cached).toBe(false);
  expect(detail.album.tracks.map((row) => row.cached)).toEqual([true, true]);
});

test("only a successful audio-cache wipe unmarks all open surfaces in place", async () => {
  reset();
  applyPlayback({ queue: [track("a", true)], queue_revision: 9004 });
  detail.playlist = { tracks: [track("a", true)] };
  detail.radio = { tracks: [track("radio", true)] };
  const visible = [track("episode", true)];
  const held = playback.queue;
  const playlist = detail.playlist.tracks[0];
  const off = watchCacheMarks((ids) => applyCacheMarks(visible, ids));
  const original = api.clearCache;
  try {
    api.clearCache = async () => { throw new Error("disk busy"); };
    await expect(clearCache("audio")).rejects.toThrow("disk busy");
    expect([held[0].cached, playlist.cached, visible[0].cached]).toEqual([true, true, true]);

    api.clearCache = async () => ({ audio: { files: 0, bytes: 0 }, covers: null });
    await clearCache("covers");
    expect([held[0].cached, playlist.cached, visible[0].cached]).toEqual([true, true, true]);

    await clearCache("audio");
    expect(playback.queue).toBe(held);
    expect(detail.playlist.tracks[0]).toBe(playlist);
    expect([held[0].cached, playlist.cached, detail.radio.tracks[0].cached, visible[0].cached])
      .toEqual([false, false, false, false]);
  } finally {
    api.clearCache = original;
    off();
  }
});

test("search songs and kept catalogue pages take live marks, and a wipe clears them without a refetch", async () => {
  reset();
  const browse = api.browseArtistCatalogue;
  const wipe = api.clearCache;
  let fetches = 0;
  try {
    api.browseArtistCatalogue = async () => {
      fetches += 1;
      return { releases: [{ id: "album", tracks: [track("song"), track("other")] }], total: 1, next_offset: null };
    };
    api.clearCache = async () => ({ audio: { files: 0, bytes: 0 }, covers: null });
    search.results = { tracks: [track("song"), track("hit")] };
    const page = await loadCataloguePage("artist-marks", ["albums"], 0, 4);

    applyPlayback({ cached_ids: ["song", "hit"] });
    expect(search.results.tracks.map((row) => row.cached)).toEqual([true, true]);
    expect(page.releases[0].tracks.map((row) => row.cached)).toEqual([true, false]);

    await clearCache("audio");
    expect(search.results.tracks.map((row) => row.cached)).toEqual([false, false]);
    expect(await loadCataloguePage("artist-marks", ["albums"], 0, 4)).toBe(page);
    expect(page.releases[0].tracks[0].cached).toBe(false);
    expect(fetches).toBe(1);
  } finally {
    api.browseArtistCatalogue = browse;
    api.clearCache = wipe;
    search.results = null;
  }
});
