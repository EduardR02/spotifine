import { expect, test } from "bun:test";

globalThis.$state = Object.assign((value) => value, { raw: (value) => value });
const { api, loadTrackCredits, trackCredits, openCredits, credits, closeCredits } = await import("./state.svelte.js");

const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

test("credits retain recently used entries and evict old payloads shared by panel and dialog", async () => {
  const original = api.browseTrackCredits;
  const calls = [];
  api.browseTrackCredits = async (id) => { calls.push(id); return { groups: [], id }; };
  try {
    loadTrackCredits({ id: "bounded-credit-recent" });
    await settle();
    // Churn beyond a generous retained-payload bound, not the cache's exact capacity.
    for (let i = 0; i < 1024; i++) {
      loadTrackCredits({ id: `bounded-credit-${i}` });
      await settle();
      if (i % 32 === 0) openCredits({ id: "bounded-credit-recent" });
    }
    const reads = calls.length;
    openCredits({ id: "bounded-credit-recent" });
    expect(calls).toHaveLength(reads);
    openCredits({ id: "bounded-credit-0" });
    await settle();
    expect(calls).toHaveLength(reads + 1);
    expect(credits.data.id).toBe("bounded-credit-0");
    expect(trackCredits.data.id).toBe("bounded-credit-1023");
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
    coverTone("bounded-tone-recent", "recent");
    await settle();
    const recent = coverTone("bounded-tone-recent", "recent");
    let evicted;
    // Keep a hot palette while cold entries exceed a generous memory bound.
    for (let i = 0; i < 1024; i++) {
      coverTone(`bounded-tone-${i}`, `seed-${i}`);
      await settle();
      if (i === 0) evicted = coverTone("bounded-tone-0", "seed-0");
      if (i % 32 === 0) coverTone("bounded-tone-recent", "recent");
    }
    expect(coverTone("bounded-tone-recent", "recent")).toBe(recent);
    expect(coverTone("bounded-tone-0", "seed-0")).not.toBe(evicted);
    await settle();
  } finally {
    globalThis.Image = oldImage;
    globalThis.window = oldWindow;
  }
});
