import { expect, test } from "bun:test";
import { createRealMode } from "./ui-harness-real.js";

test("real mode never substitutes fixture profiles or accepts personal writes", async () => {
  const previousFetch = globalThis.fetch;
  const previousDocument = globalThis.document;
  const forwarded = [];
  const mocked = [];
  const playback = {};
  globalThis.document = { body: { dataset: {} }, title: "" };
  globalThis.fetch = async (url, options) => {
    if (url === "/__real/status") return { json: async () => ({ live: false }) };
    const { cmd, args } = JSON.parse(options.body);
    forwarded.push({ cmd, args });
    return { json: async () => ({ ok: false, source: "none", error: "native app is closed" }) };
  };
  try {
    const mode = await createRealMode({
      playback,
      settings: {},
      mock: { invoke: async (cmd) => { mocked.push(cmd); return { username: "fixture", playlists: [] }; } },
      state: { navigate() {}, navigateArtist() {} },
      emit() {}, emitState() {},
      setCurrent(index) { playback.current_index = playback.queue.length ? index : -1; },
      clone: structuredClone,
      shuffleBag: { set() {} },
      redrawShuffleBag() {},
    });
    expect(mode.library).toEqual([]);
    expect(playback.ready).toBe(false);
    expect(playback.username).toBe("");
    expect(playback.queue).toEqual([]);
    for (const [cmd, args] of [
      ["browse_profile", { username: "listener" }],
      ["browse_show", { id: "show" }],
      ["browse_episode", { id: "episode" }],
      ["personal_api_status", {}],
    ]) {
      await expect(mode.invoke(cmd, args)).rejects.toThrow("no fixture data");
    }
    expect(mocked).toEqual([]);
    const before = forwarded.length;
    for (const cmd of ["personal_api_configure", "personal_api_authorize", "personal_api_set_saved", "select_output"]) {
      await expect(mode.invoke(cmd, {})).rejects.toThrow("native Spotifine app");
    }
    expect(forwarded.length).toBe(before);
    expect(forwarded.some(({ cmd }) => cmd === "personal_api_devices")).toBe(false);
  } finally {
    globalThis.fetch = previousFetch;
    globalThis.document = previousDocument;
  }
});
