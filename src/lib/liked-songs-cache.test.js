import { expect, test } from "bun:test";

/* The hub is a runes module: outside Svelte `$state` is just the value it is
   handed, and what these tests are about is which page the store keeps and how
   many walks it asks the shell for — identity, not reactivity. */
globalThis.$state = (value) => value;

/* `invoke` is the Tauri bridge, which no test has. Every call lands here and is
   answered by the test, so a walk can be held open across an account change. */
const calls = [];
const walks = [];
globalThis.window = {
  __TAURI_INTERNALS__: {
    invoke(command, args) {
      calls.push({ command, args });
      return new Promise((resolve, reject) => walks.push({ resolve, reject }));
    },
  },
};

const { api, applyPlayback, savedTracksChanged } = await import("./state.svelte.js");

function page(ids) {
  return {
    tracks: ids.map((id) => ({
      id,
      uri: `spotify:track:${id}`,
      name: `Track ${id}`,
      duration_ms: 180_000,
      cached: false,
    })),
    next_cursor: null,
  };
}

const idsOf = (detail) => detail.tracks.map((row) => row.id);

test("an account change during a walk is not answered by that walk", async () => {
  applyPlayback({ auth_state: "ready", username: "alice" });
  const alice = api.browseLikedSongs(null);
  const stale = alice.then(() => ({ rejected: false }), (error) => ({ rejected: true, error }));
  expect(calls.length).toBe(1, "alice's first page is walked once");

  // The account changes while that walk is still in the air.
  applyPlayback({ auth_state: "ready", username: "bob" });
  const bob = api.browseLikedSongs(null);
  expect(calls.length).toBe(2, "bob asks for a walk of his own");

  /* A's private page cannot be returned after the account boundary. B has
     an independent walk, and only B's answer remains cached. */
  walks[0].resolve(page(["alice"]));
  walks[1].resolve(page(["bob"]));
  expect(idsOf(await bob)).toEqual(["bob"]);
  const outcome = await stale;
  expect(outcome.rejected).toBe(true);
  expect(outcome.error).toBeInstanceOf(Error);
  expect(idsOf(await api.browseLikedSongs(null))).toEqual(["bob"]);
  expect(calls.length).toBe(2, "and bob's own page is what the cache holds");
});

test("a cached page does not outlive the account it was walked for", async () => {
  applyPlayback({ auth_state: "ready", username: "carol" });
  const walked = calls.length;
  const carol = api.browseLikedSongs(null);
  walks.at(-1).resolve(page(["carol"]));
  expect(idsOf(await carol)).toEqual(["carol"]);
  expect(idsOf(await api.browseLikedSongs(null))).toEqual(["carol"]);
  expect(calls.length).toBe(walked + 1, "the second reader is served by the cache");

  applyPlayback({ auth_state: "ready", username: "dave" });
  const dave = api.browseLikedSongs(null);
  expect(calls.length).toBe(walked + 2, "a new account walks its own first page");
  walks.at(-1).resolve(page(["dave"]));
  expect(idsOf(await dave)).toEqual(["dave"]);
});

test("a change to Liked Songs in the membership index drops the cached page", async () => {
  applyPlayback({ auth_state: "ready", username: "erin" });
  const first = api.browseLikedSongs(null);
  walks.at(-1).resolve(page(["one"]));
  await first;
  const before = calls.length;
  expect(idsOf(await api.browseLikedSongs(null))).toEqual(["one"]);
  expect(calls.length).toBe(before);

  savedTracksChanged();
  const refreshed = api.browseLikedSongs(null);
  expect(calls.length).toBe(before + 1);
  walks.at(-1).resolve(page(["two", "one"]));
  expect(idsOf(await refreshed)).toEqual(["two", "one"]);
});
