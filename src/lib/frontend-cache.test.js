import { expect, test } from "bun:test";

globalThis.$state = Object.assign((value) => value, { raw: (value) => value });
const {
  api, loadTrackCredits, trackCredits, openCredits, credits, closeCredits,
  applySession, navigate, goBack, loadDetail, detail,
} = await import("./state.svelte.js");

const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

/** Stubs one browse command for `body`; `calls` lists the ids it was asked for. */
async function withBrowse(command, answer, body) {
  const original = api[command];
  const calls = [];
  api[command] = async (id) => { calls.push(id); return answer(id); };
  try {
    await body(calls);
  } finally {
    api[command] = original;
  }
}

/** What App does on a route change: navigate, then load that route's detail. */
async function open(name, id) {
  navigate(name, id);
  loadDetail();
  await settle();
}

const album = (id) => ({ id, name: id, tracks: [] });

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

test("a revisited album paints the kept page at once and asks the engine nothing", async () => {
  await withBrowse("browseAlbum", album, async (calls) => {
    await open("album", "kept-album");
    const kept = detail.album;
    await open("album", "kept-album-other");
    goBack();
    // Already on screen before the route's load runs: no skeleton frame.
    expect(detail.album).toBe(kept);
    loadDetail();
    await settle();
    expect(calls).toEqual(["kept-album", "kept-album-other"]);
    expect(detail.album).toBe(kept);
  });
});

test("kept albums are bounded by count and by age", async () => {
  const realNow = Date.now;
  await withBrowse("browseAlbum", album, async (calls) => {
    await open("album", "bounded-album-first");
    // Churn beyond a generous bound, not the cache's exact capacity.
    for (let i = 0; i < 100; i++) await open("album", `bounded-album-${i}`);
    navigate("album", "bounded-album-first");
    expect(detail.album).toBeNull();
    loadDetail();
    await settle();
    expect(calls.at(-1)).toBe("bounded-album-first");
    expect(detail.album.id).toBe("bounded-album-first");

    navigate("album", "bounded-album-99");
    expect(detail.album?.id).toBe("bounded-album-99");
    try {
      // A day later the same page is a miss: skeleton, then a fresh read.
      Date.now = () => realNow.call(Date) + 24 * 60 * 60 * 1000;
      navigate("album", "bounded-album-first");
      expect(detail.album).toBeNull();
      loadDetail();
      await settle();
      expect(calls.filter((id) => id === "bounded-album-first")).toHaveLength(3);
    } finally {
      Date.now = realNow;
    }
  });
});

test("a revisited artist paints the kept page, refreshes it, and keeps it when unchanged", async () => {
  let listeners = 100;
  const artist = (id) => ({ id, name: id, monthly_listeners: listeners, top_tracks: [] });
  await withBrowse("browseArtist", artist, async (calls) => {
    await open("artist", "kept-artist");
    const kept = detail.artist;
    await open("artist", "kept-artist-other");
    goBack();
    expect(detail.artist).toBe(kept);
    loadDetail();
    await settle();
    expect(calls).toEqual(["kept-artist", "kept-artist-other", "kept-artist"]);
    // An equal answer leaves the object on screen alone, so nothing re-renders.
    expect(detail.artist).toBe(kept);
    // The refreshed payload serves the artist's other routes without asking.
    await open("discography", "kept-artist");
    expect(calls).toHaveLength(3);

    listeners = 200;
    await open("artist", "kept-artist-other");
    navigate("artist", "kept-artist");
    expect(detail.artist).toBe(kept);
    loadDetail();
    await settle();
    expect(calls).toHaveLength(5);
    expect(detail.artist).not.toBe(kept);
    expect(detail.artist.monthly_listeners).toBe(200);
  });
});

test("an account change forgets kept pages", async () => {
  applySession({ auth_state: "ready", username: "kept-pages-a" });
  await settle();
  await withBrowse("browseAlbum", album, async (calls) => {
    await open("album", "account-album");
    applySession({ auth_state: "ready", username: "kept-pages-b" });
    navigate("album", "account-album");
    expect(detail.album).toBeNull();
    loadDetail();
    await settle();
    expect(calls).toEqual(["account-album", "account-album"]);
  });
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
