import { afterEach, beforeEach, expect, test } from "bun:test";

globalThis.$state = (value) => value;
const { api, applyPlayback, playback, positionMs } = await import("./state.svelte.js");
let previousWindow;
let requests;

beforeEach(() => {
  previousWindow = globalThis.window;
  requests = [];
  globalThis.window = { __TAURI_INTERNALS__: {
    invoke: (command, args) => new Promise((resolve, reject) => requests.push({ command, args, resolve, reject })),
  } };
  applyPlayback({ playing: false, buffering: false, duration_ms: 180_000, position_ms: 1_000 });
});
afterEach(() => {
  applyPlayback({ playing: false });
  globalThis.window = previousWindow;
});

test("an older rejected seek cannot undo a newer confirmed seek", async () => {
  const old = api.seek(20_000).catch((error) => error);
  const current = api.seek(40_000);
  requests[1].resolve(null);
  await current;
  applyPlayback({ position_ms: 40_000 });
  requests[0].reject(new Error("old seek refused"));
  await old;
  expect(positionMs()).toBe(40_000);
});

test("seek rejection never overwrites a subsequent authoritative pause", async () => {
  applyPlayback({ playing: true });
  const pending = api.seek(20_000).catch((error) => error);
  applyPlayback({ playing: false, position_ms: 20_000 });
  requests[0].reject(new Error("seek refused"));
  await pending;
  expect(playback.playing).toBe(false);
  expect(positionMs()).toBe(20_000);
});

test("an optimistic pause owns the seek anchor before its state event arrives", async () => {
  applyPlayback({ playing: true });
  const pending = api.seek(20_000).catch((error) => error);
  const paused = api.pause();
  requests[1].resolve(null);
  await paused;
  requests[0].reject(new Error("seek refused"));
  await pending;
  expect(playback.playing).toBe(false);
  expect(positionMs()).toBeGreaterThanOrEqual(20_000);
});

test("a track change owns its position even when an old seek later fails", async () => {
  const pending = api.seek(20_000).catch((error) => error);
  applyPlayback({ current_uri: "spotify:track:new", position_ms: 300 });
  requests[0].reject(new Error("old track gone"));
  await pending;
  expect(positionMs()).toBe(300);
});

test("a failed current seek without intervening authority restores its prior projection", async () => {
  const pending = api.seek(20_000).catch((error) => error);
  expect(positionMs()).toBe(20_000);
  requests[0].reject(new Error("seek refused"));
  await pending;
  expect(positionMs()).toBe(1_000);
  expect(playback.playing).toBe(false);
});
