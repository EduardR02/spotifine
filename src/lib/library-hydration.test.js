import { beforeAll, expect, test } from "bun:test";

globalThis.$state ??= Object.assign((value) => value, { raw: (value) => value });
let applyPlayback, library, setLibrary, hydrateLibraryCovers;
beforeAll(async () => {
  ({ applyPlayback, library, setLibrary, hydrateLibraryCovers } = await import("./state.svelte.js"));
});

function bridge() {
  const previous = globalThis.window;
  const requests = [];
  globalThis.window = { __TAURI_INTERNALS__: { invoke(command, args) {
    return new Promise((resolve, reject) => requests.push({ command, args, resolve, reject }));
  } } };
  return { requests, restore() { globalThis.window = previous; } };
}
const row = (id, extra = {}) => ({ id, name: id, cover_url: "", cover_urls: [], last_activity: null, ...extra });

test("visible cover hydration fills missing artwork without browsing or changing row identity/order", async () => {
  const { requests, restore } = bridge();
  try {
    applyPlayback({ ready: true, auth_state: "ready", username: "cover-visible", playing: false });
    setLibrary([row("recent", { last_activity: 900 }), row("visible"), row("offscreen"), row("custom", { cover_url: "custom-cover" })]);
    const original = library[1];
    const first = hydrateLibraryCovers(["visible", "custom"]);
    const duplicate = hydrateLibraryCovers(["visible"]);
    await Promise.resolve();
    expect(requests.map(({ command, args }) => ({ command, args }))).toEqual([
      { command: "hydrate_library_covers", args: { ids: ["visible"] } },
    ]);
    requests[0].resolve([{ id: "visible", name: "stale-title", cover_url: "", cover_urls: ["a", "b"], last_activity: 1 }]);
    await Promise.all([first, duplicate]);
    expect(library.map(({ id }) => id)).toEqual(["recent", "visible", "offscreen", "custom"]);
    expect(library[1]).toBe(original);
    expect(original.name).toBe("visible");
    expect(original.cover_urls).toEqual(["a", "b"]);
    expect(library[2].cover_urls).toEqual([]);
    expect(library[3].cover_url).toBe("custom-cover");
  } finally { restore(); }
});

test("a header from the prior account cannot populate the current account's same playlist", async () => {
  const { requests, restore } = bridge();
  try {
    applyPlayback({ ready: true, auth_state: "ready", username: "cover-old", playing: false });
    setLibrary([row("shared")]);
    const old = hydrateLibraryCovers(["shared"]);
    await Promise.resolve();
    applyPlayback({ auth_state: "ready", username: "cover-new", playing: false });
    setLibrary([row("shared", { name: "new-account" })]);
    const current = hydrateLibraryCovers(["shared"]);
    requests[0].resolve([{ id: "shared", cover_url: "old-account-cover" }]);
    await old;
    expect(library[0].cover_url).toBe("");
    expect(requests.length).toBe(2);
    requests[1].resolve([{ id: "shared", cover_url: "new-account-cover" }]);
    await current;
    expect(library[0].cover_url).toBe("new-account-cover");
    expect(library[0].name).toBe("new-account");
  } finally { restore(); }
});

test("a refused header can be retried and a newer cover wins over an in-flight header", async () => {
  const { requests, restore } = bridge();
  try {
    applyPlayback({ ready: true, auth_state: "ready", username: "cover-retry", playing: false });
    setLibrary([row("retry")]);
    const first = hydrateLibraryCovers(["retry"]);
    await Promise.resolve();
    requests[0].reject("header unavailable");
    await expect(first).rejects.toBe("header unavailable");
    const retry = hydrateLibraryCovers(["retry"]);
    await Promise.resolve();
    library[0].cover_url = "newer-custom-cover";
    requests[1].resolve([{ id: "retry", cover_url: "old-header-cover" }]);
    await retry;
    expect(library[0].cover_url).toBe("newer-custom-cover");
  } finally { restore(); }
});
