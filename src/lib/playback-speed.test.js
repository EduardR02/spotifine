import { afterEach, expect, spyOn, test } from "bun:test";

globalThis.$state = Object.assign((value) => value, { raw: (value) => value });
const requests = [];
globalThis.window = {
  __TAURI_INTERNALS__: {
    invoke: (command, args) => new Promise((resolve, reject) => requests.push({ command, args, resolve, reject })),
  },
};
const { api, applyPlayback, playback, positionMs } = await import("./state.svelte.js");

afterEach(() => {
  requests.length = 0;
  playback.playback_speed = 1;
  applyPlayback({ playing: false, audible_playback_speed: 1 });
});

test("the supported speed endpoints and exact normal speed reach playback", async () => {
  for (const speed of [0.5, 4, 1]) {
    const pending = api.setPlaybackSpeed(speed);
    expect(playback.playback_speed).toBe(speed);
    const request = requests.pop();
    expect(request.command).toBe("set_playback_speed");
    expect(request.args.speed).toBe(speed);
    request.resolve(null);
    await pending;
    expect(playback.playback_speed).toBe(speed);
  }
});

test("invalid speed cannot change playback or reach the native engine", async () => {
  playback.playback_speed = 1.5;
  for (const speed of [NaN, Infinity, -Infinity, 0.49, 4.01]) {
    await expect(api.setPlaybackSpeed(speed)).rejects.toThrow("between 0.5 and 4.0");
    expect(playback.playback_speed).toBe(1.5);
  }
  expect(requests).toEqual([]);
});

test("an older refusal cannot overwrite a newer accepted speed", async () => {
  const old = api.setPlaybackSpeed(2);
  const oldError = old.catch((error) => error);
  const current = api.setPlaybackSpeed(4);
  requests[1].resolve(null);
  await current;
  requests[0].reject(new Error("device refused"));
  expect((await oldError).message).toBe("device refused");
  expect(playback.playback_speed).toBe(4);
});

test("a refused current speed restores the last accepted speed", async () => {
  playback.playback_speed = 1.5;
  const pending = api.setPlaybackSpeed(4);
  requests[0].reject(new Error("device refused"));
  await expect(pending).rejects.toThrow("device refused");
  expect(playback.playback_speed).toBe(1.5);
});

test("requested speed leaves projection at the audible rate until its queued-audio boundary", async () => {
  let now = 100;
  const clock = spyOn(performance, "now").mockImplementation(() => now);
  try {
    applyPlayback({ playing: true, buffering: false, duration_ms: 60_000, position_ms: 1_000, playback_speed: 1, audible_playback_speed: 1 });
    now = 200;
    expect(positionMs()).toBe(1_100);
    const pending = api.setPlaybackSpeed(4);
    expect(playback.playback_speed).toBe(4);
    now = 300;
    expect(positionMs()).toBe(1_200);
    requests[0].resolve(null);
    await pending;
    applyPlayback({ position_ms: 1_250, audible_playback_speed: 4 });
    now = 400;
    expect(positionMs()).toBe(1_650);
  } finally {
    applyPlayback({ playing: false });
    clock.mockRestore();
  }
});

test("a partial audible-rate boundary preserves the position at the seam", () => {
  let now = 100;
  const clock = spyOn(performance, "now").mockImplementation(() => now);
  try {
    applyPlayback({ playing: true, buffering: false, duration_ms: 60_000, position_ms: 1_000, playback_speed: 4, audible_playback_speed: 1 });
    now = 200;
    applyPlayback({ audible_playback_speed: 4 });
    expect(positionMs()).toBe(1_100);
    now = 300;
    expect(positionMs()).toBe(1_500);
  } finally {
    applyPlayback({ playing: false });
    clock.mockRestore();
  }
});
