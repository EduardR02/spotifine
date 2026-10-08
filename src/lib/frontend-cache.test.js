import { expect, test } from "bun:test";

globalThis.$state = Object.assign((value) => value, { raw: (value) => value });
const { api, loadTrackCredits, trackCredits, openCredits, credits, closeCredits } = await import("./state.svelte.js");

const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

test("credits retain recently used entries and evict old payloads shared by panel and dialog", async () => {
  const original = api.browseTrackCredits;
  const calls = [];
  api.browseTrackCredits = async (id) => { calls.push(id); return { groups: [], id }; };
  try {
    for (let i = 0; i < 128; i++) {
      loadTrackCredits({ id: `bounded-credit-${i}` });
      await settle();
    }
    openCredits({ id: "bounded-credit-0" });
    expect(credits.loading).toBe(false);
    expect(calls).toHaveLength(128);
    loadTrackCredits({ id: "bounded-credit-128" });
    await settle();
    openCredits({ id: "bounded-credit-0" });
    expect(calls).toHaveLength(129);
    openCredits({ id: "bounded-credit-1" });
    await settle();
    expect(calls).toHaveLength(130);
    expect(credits.data.id).toBe("bounded-credit-1");
    expect(trackCredits.data.id).toBe("bounded-credit-128");
  } finally {
    api.browseTrackCredits = original;
    closeCredits();
  }
});

test("final cover tones retain recent palettes and evict older entries", async () => {
  const oldWindow = globalThis.window;
  const oldImage = globalThis.Image;
  globalThis.window = { __TAURI_INTERNALS__: { convertFileSrc: (path) => path } };
  globalThis.Image = class {
    set src(value) { queueMicrotask(() => this.onerror()); }
  };
  try {
    const { coverTone } = await import("./covertone.svelte.js");
    let evicted;
    for (let i = 0; i < 512; i++) {
      coverTone(`bounded-tone-${i}`, `seed-${i}`);
      await settle();
      if (i === 1) evicted = coverTone("bounded-tone-1", "seed-1");
    }
    const oldest = coverTone("bounded-tone-0", "seed-0");
    coverTone("bounded-tone-512", "seed-512");
    await settle();
    expect(coverTone("bounded-tone-0", "seed-0")).toBe(oldest);
    expect(coverTone("bounded-tone-1", "seed-1")).not.toBe(evicted);
    await settle();
  } finally {
    globalThis.Image = oldImage;
    globalThis.window = oldWindow;
  }
});
