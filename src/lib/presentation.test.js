import { afterAll, beforeAll, expect, test } from "bun:test";

/* Presentation mode as the window sees it. The shell answers
   `get_presentation` with the configured name; everything else here is what
   the window does with it. A private instance of the hub (the query makes it
   one), so its one-time event wiring runs here, presenting, and the shared
   instance the other tests use is untouched. */
globalThis.$state ??= Object.assign((value) => value, { raw: (value) => value });

const commands = [];
const handlers = new Map();
const callbacks = new Map();
let callbackId = 0;
let previousWindow;
let state;

beforeAll(async () => {
  previousWindow = globalThis.window;
  globalThis.window = {
    __TAURI_INTERNALS__: {
      transformCallback(callback) {
        callbacks.set(++callbackId, callback);
        return callbackId;
      },
      invoke(command, args) {
        commands.push(command);
        if (command === "get_presentation") return Promise.resolve("Alex Rivera");
        if (command === "plugin:event|listen") {
          handlers.set(args.event, callbacks.get(args.handler));
          return Promise.resolve(args.handler);
        }
        if (command === "get_state") {
          return Promise.resolve({
            playback: { ready: true, auth_state: "ready", username: "real-account", playing: false },
            // No library yet: the configured one is still being read.
            playlists: [], playlist_tree: [], library_fresh: false, settings: {},
          });
        }
        return Promise.resolve(null);
      },
    },
  };
  state = await import("./state.svelte.js?presentation");
  await state.initEvents();
  await new Promise((resolve) => setTimeout(resolve, 0));
});

afterAll(() => {
  if (previousWindow === undefined) delete globalThis.window;
  else globalThis.window = previousWindow;
});

test("the presented name is known before any identity, and stands in for the account's", () => {
  expect(commands[0]).toBe("get_presentation");
  expect(state.presentation.name).toBe("Alex Rivera");
  expect(state.session.username).toBe("Alex Rivera");
  expect(state.playback.username).toBe("Alex Rivera");

  handlers.get("session")({ payload: { auth_state: "ready", username: "real-account", error: null } });
  expect(state.session.username).toBe("Alex Rivera");
  handlers.get("state")({ payload: { ready: true, auth_state: "ready", username: "real-account" } });
  expect(state.playback.username).toBe("Alex Rivera");

  // Signed out is still signed out.
  state.applySession({ auth_state: "needs_login", username: "" });
  expect(state.session.username).toBeNull();
  state.applySession({ auth_state: "ready", username: "real-account" });
  expect(state.session.username).toBe("Alex Rivera");
});

test("the library is the configured one: no stored snapshot, configured order, no listening", () => {
  // Before the configured library lands, the rail keeps its loading frame.
  expect(state.libraryState.loaded).toBe(false);
  handlers.get("library_cached")({ payload: { playlists: [{ id: "stored" }], playlist_tree: [] } });
  expect(state.library.length).toBe(0);
  expect(state.libraryState.loaded).toBe(false);

  const rows = [
    { id: "p1", uri: "spotify:playlist:p1", name: "First" },
    { id: "al1", uri: "spotify:album:al1", name: "An Album" },
    { id: "p2", uri: "spotify:playlist:p2", name: "Last" },
  ];
  handlers.get("library")({ payload: { playlists: rows, playlist_tree: rows.map(({ id }) => ({ kind: "playlist", id })) } });
  expect(state.library.map((row) => row.id)).toEqual(["p1", "al1", "p2"]);
  expect(state.libraryState.fresh).toBe(true);
  expect(state.libraryKind(state.library[1])).toBe("album");
  expect(state.libraryKind(state.library[0])).toBe("playlist");

  // Playing the last one neither moves it nor makes it recently played.
  expect(state.promotePlaylist("p2", { played: true })).toBe(false);
  expect(state.library.map((row) => row.id)).toEqual(["p1", "al1", "p2"]);
  expect(state.library[2].last_played).toBeUndefined();
});

test("the account's own collection and profile are not routes, and download marks are hidden", async () => {
  state.navigate("library");
  state.navigate("liked");
  expect(state.route.name).toBe("library");
  state.navigate("profile", "Alex Rivera");
  expect(state.route.name).toBe("library");
  state.navigate("profile");
  expect(state.route.name).toBe("library");
  state.navigate("profile", "spotify");
  expect(state.route).toMatchObject({ name: "profile", id: "spotify" });
  state.navigate("album", "al1");
  expect(state.route).toMatchObject({ name: "album", id: "al1" });

  expect(state.isCached({ id: "t", cached: true })).toBe(false);

  const before = commands.length;
  await state.ensurePersonalizedDiscovery(state.library);
  expect(commands.slice(before)).not.toContain("search");
});
