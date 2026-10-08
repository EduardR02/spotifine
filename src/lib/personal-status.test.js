import { beforeAll, expect, test } from "bun:test";

globalThis.$state ??= Object.assign((value) => value, { raw: (value) => value });
let applyPlayback, personal, personalConnected, refreshPersonal, watchPersonal;
beforeAll(async () => {
  ({ applyPlayback } = await import("./state.svelte.js"));
  ({ personal, personalConnected, refreshPersonal, watchPersonal } = await import("./personal.svelte.js"));
});

test("personal connection refreshes after startup playback login and cannot regress from its earlier status", async () => {
  const previousWindow = globalThis.window;
  const requests = [];
  globalThis.window = { __TAURI_INTERNALS__: {
    transformCallback: () => 1,
    invoke(command, args) {
      if (command === "plugin:event|listen") return Promise.resolve(1);
      return new Promise((resolve, reject) => requests.push({ command, args, resolve, reject }));
    },
  } };
  try {
    applyPlayback({ auth_state: "needs_login", username: null, playing: false });
    watchPersonal();
    const starting = refreshPersonal();
    expect(requests.length).toBe(1);
    applyPlayback({ ready: true, auth_state: "ready", username: "status-ready", playing: false });
    watchPersonal();
    watchPersonal();
    const ready = refreshPersonal();
    expect(requests.map((request) => request.command)).toEqual(["personal_api_status", "personal_api_status"]);
    requests[1].resolve({ connected: true, account_id: "status-ready" });
    await ready;
    expect(personalConnected()).toBe(true);
    requests[0].resolve({ connected: false, account_id: null });
    await starting;
    expect(personalConnected()).toBe(true);
    expect(personal.loading).toBe(false);
  } finally { globalThis.window = previousWindow; }
});
