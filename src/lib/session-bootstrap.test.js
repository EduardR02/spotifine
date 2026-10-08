import { expect, test } from "bun:test";

globalThis.$state ??= Object.assign((value) => value, { raw: (value) => value });
const { applyPlayback, initEvents, playback, session, library, libraryState } = await import("./state.svelte.js");

test("ready playback hydrates identity and a late startup snapshot cannot replace a newer session", async () => {
  const previousWindow = globalThis.window;
  const previousPlayback = { ...playback };
  const previousSession = { ...session };
  const previousLibrary = [...library];
  const previousLibraryState = { ...libraryState };
  const callbacks = new Map();
  const handlers = new Map();
  let callbackId = 0;
  let resolveSnapshot;
  const snapshot = new Promise((resolve) => { resolveSnapshot = resolve; });
  globalThis.window = {
    __TAURI_INTERNALS__: {
      transformCallback(callback) {
        callbacks.set(++callbackId, callback);
        return callbackId;
      },
      invoke(command, args) {
        if (command === "plugin:event|listen") {
          handlers.set(args.event, callbacks.get(args.handler));
          return Promise.resolve(args.handler);
        }
        if (command === "get_state") return snapshot;
        return Promise.reject(new Error(`Unexpected command: ${command}`));
      },
    },
  };
  try {
    applyPlayback({ auth_state: "logged_out", username: null, playing: false });
    applyPlayback({ ready: true, auth_state: "ready", username: "alice", playing: false });
    expect(session.username).toBe("alice");
    expect(playback.username).toBe("alice");
    applyPlayback({ position_ms: 100 });
    expect(session.username).toBe("alice");

    applyPlayback({ auth_state: "logged_out", username: null, playing: false });
    expect(session.username).toBeNull();
    await initEvents();
    handlers.get("session")({ payload: { auth_state: "ready", username: "bob", error: null } });
    resolveSnapshot({
      playback: { ready: true, auth_state: "ready", username: "alice", playing: false },
      playlists: [],
      library_fresh: false,
    });
    // Drain the command's promise adoption and reducer callbacks, not a timed
    // server wait: the snapshot is already resolved before this task boundary.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(session.username).toBe("bob");
    expect(playback.username).toBe("bob");
    expect(playback.auth_state).toBe("ready");
  } finally {
    applyPlayback({ playing: false });
    Object.assign(playback, previousPlayback);
    Object.assign(session, previousSession);
    library.splice(0, library.length, ...previousLibrary);
    Object.assign(libraryState, previousLibraryState);
    if (previousWindow === undefined) delete globalThis.window;
    else globalThis.window = previousWindow;
  }
});
