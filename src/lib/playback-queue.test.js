import { expect, test } from "bun:test";

/* The hub is a runes module. Outside Svelte `$state` is just the value it is
   handed, and what these tests are about is which array objects the store
   keeps — identity, not reactivity — so the stub has to exist before the
   module is evaluated. */
globalThis.$state = Object.assign((value) => value, { raw: (value) => value });

const { applyPlayback, detail, playback, isPlayingSource } = await import("./state.svelte.js");

function track(id, cached = false) {
  return { id, uri: `spotify:track:${id}`, name: `Track ${id}`, duration_ms: 180_000, cached };
}

/* That the store holds the very rows the engine sent, as plain objects, is a
   question for the real runtime: see state-reactivity.test.js. */
test("rows whose generation did not move keep their identity", () => {
  applyPlayback({ queue: [track("a"), track("b")], queue_revision: 5, current_index: 0 });
  const held = playback.queue;

  // The same generation again: a payload carrying an equal array must not
  // replace the rows every consumer of the queue is keyed on.
  applyPlayback({ queue: [track("a"), track("b")], queue_revision: 5, position_ms: 1_000 });
  expect(playback.queue).toBe(held);
  expect(playback.queue[0]).toBe(held[0]);
});

test("a new generation adopts the rows it names, an empty one included", () => {
  applyPlayback({ queue: [track("a")], queue_revision: 6, current_index: 0 });
  expect(playback.queue.map((row) => row.id)).toEqual(["a"]);

  applyPlayback({ queue: [track("a"), track("b")], queue_revision: 7, current_index: 0 });
  expect(playback.queue.map((row) => row.id)).toEqual(["a", "b"]);

  // A queue that became empty is a new generation with no rows: it has to
  // clear the store rather than be read as "unchanged".
  applyPlayback({ queue: [], queue_revision: 8 });
  expect(playback.queue).toEqual([]);
});

test("a payload with no revision at all is adopted as it arrives", () => {
  applyPlayback({ queue: [track("c")], current_index: 0 });
  expect(playback.queue.map((row) => row.id)).toEqual(["c"]);
});

test("a payload that keeps the queue still updates everything else", () => {
  applyPlayback({ queue: [track("a")], queue_revision: 11, current_index: 0, duration_ms: 1_000 });
  const held = playback.queue;
  applyPlayback({ queue_revision: 11, current_index: 0, position_ms: 4_000, duration_ms: 2_000 });
  expect(playback.position_ms).toBe(4_000);
  expect(playback.duration_ms).toBe(2_000);
  expect(playback.queue).toBe(held);
});

test("a download mark that moved reaches the list on screen", () => {
  applyPlayback({ queue: [track("a"), track("b")], queue_revision: 20, current_index: 0 });
  detail.playlist = { tracks: [track("a"), track("b")] };
  applyPlayback({ queue: [track("a", true), track("b")], queue_revision: 21, current_index: 0 });
  expect(detail.playlist.tracks[0].cached).toBe(true);
  expect(detail.playlist.tracks[1].cached).toBe(false);
});

test("rows that shifted under a mark are never misread", () => {
  /* Same length, different rows: an index-wise comparison would hand the flag
     at index 0 to whatever now sits there, so the identity check has to reject
     the alignment and mark only rows the payload really calls cached. */
  applyPlayback({ queue: [track("a"), track("b", true)], queue_revision: 30, current_index: 0 });
  detail.playlist = { tracks: [track("a"), track("b"), track("x")] };
  applyPlayback({ queue: [track("x", true), track("a")], queue_revision: 31, current_index: 0 });
  const rows = detail.playlist.tracks;
  expect(rows.find((row) => row.id === "x").cached).toBe(true);
  expect(rows.find((row) => row.id === "a").cached).toBe(false);
});

test("a row replaced in place still carries its mark to the list", () => {
  /* Same length and both rows already marked, so nothing about the flags
     differs — only the row does. An index whose row is not the row retained is
     an insertion or a removal that shifted the rest, never an alignment, so the
     payload's own rows have to be read rather than skipped. */
  applyPlayback({ queue: [track("a", true)], queue_revision: 40, current_index: 0 });
  detail.playlist = { tracks: [track("x")] };
  applyPlayback({ queue: [track("x", true)], queue_revision: 41, current_index: 0 });
  expect(detail.playlist.tracks[0].cached).toBe(true);
});

test("revision 0 names no generation, so it is never matched", () => {
  /* What a pre-revision engine sends, and what the shell's own default leaves
     behind. Two payloads that both say 0 describe unrelated queues, so the
     second one's rows are adopted instead of being read as "unchanged". */
  applyPlayback({ queue: [track("a")], queue_revision: 0, current_index: 0 });
  applyPlayback({ queue: [track("b")], queue_revision: 0, current_index: 0 });
  expect(playback.queue.map((row) => row.id)).toEqual(["b"]);
});

test("shared songs and equal queues never substitute for exact playback source identity", () => {
  applyPlayback({ queue: [track("shared")], current_uri: "spotify:track:shared", context: "playlist:one" });
  expect(isPlayingSource("playlist:one")).toBe(true);
  expect(isPlayingSource("playlist:two")).toBe(false);
  expect(isPlayingSource("album:one")).toBe(false);
  applyPlayback({ context: "playlist:two" });
  expect(isPlayingSource("playlist:one")).toBe(false);
  expect(isPlayingSource("playlist:two")).toBe(true);
  applyPlayback({ current_uri: null, context: "" });
  expect(isPlayingSource("playlist:two")).toBe(false);
  expect(isPlayingSource("")).toBe(false);
});

test("device authority changes adopt their queue even when revision numbers coincide", () => {
  applyPlayback({ output_device_id: "local", queue: [track("local")], queue_revision: 71 });
  const local = playback.queue;
  applyPlayback({ output_device_id: "remote", queue: [track("remote")], queue_revision: 71 });
  expect(playback.queue).not.toBe(local);
  expect(playback.queue.map((row) => row.id)).toEqual(["remote"]);
  applyPlayback({ output_device_id: "local", queue: [track("local")], queue_revision: 71 });
  expect(playback.queue.map((row) => row.id)).toEqual(["local"]);
});

test("transport omissions retain both arrays while order-only changes keep row identity", () => {
  applyPlayback({ output_device_id: "", queue: [track("a"), track("b"), track("a")],
    queue_revision: 100, upcoming: [1, 2], order_revision: 50, current_index: 0 });
  const rows = playback.queue;
  const order = playback.upcoming;
  applyPlayback({ queue_revision: 100, order_revision: 50, position_ms: 1234 });
  expect(playback.queue).toBe(rows);
  expect(playback.upcoming).toBe(order);
  applyPlayback({ queue_revision: 100, upcoming: [2], order_revision: 51, current_index: 1 });
  expect(playback.queue).toBe(rows);
  expect(playback.upcoming).toEqual([2]);
  expect(playback.current_index).toBe(1);
  applyPlayback({ upcoming: [], order_revision: 52 });
  expect(playback.upcoming).toEqual([]);
  expect(playback.queue).toBe(rows);
});

test("foreign adjacent revisions stay exact and update their duplicate occurrences", () => {
  const first = 2 ** 53 - 3;
  applyPlayback({ output_device_id: "phone", queue: [track("a"), track("a")], queue_revision: first,
    upcoming: [1], order_revision: first, current_index: 0 });
  applyPlayback({ queue: [track("a"), track("b")], queue_revision: first + 1,
    upcoming: [], order_revision: first + 1, current_index: 1 });
  expect(playback.queue.map((row) => row.id)).toEqual(["a", "b"]);
  expect(playback.upcoming).toEqual([]);
  expect(playback.current_index).toBe(1);
});
