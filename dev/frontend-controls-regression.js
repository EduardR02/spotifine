import { tick } from "svelte";
import { api, applyPlayback, canvasLookups, playback, ui, setNowPlayingOpen } from "../src/lib/state.svelte.js";
import { haze } from "../src/lib/ambient.svelte.js";
import { personal } from "../src/lib/personal.svelte.js";

const settle = async () => {
  for (let i = 0; i < 6; i++) { await Promise.resolve(); await tick(); }
};
const check = (condition, message) => { if (!condition) throw new Error(message); };

/** Run in the synthetic UI harness, never the owner's account-backed surface. */
export async function frontendControlRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const oldSelect = api.selectOutput;
  const oldInvoke = window.__TAURI_INTERNALS__.invoke;
  const oldPersonal = personal.status;
  const oldQueue = playback.queue;
  const oldIndex = playback.current_index;
  const oldUri = playback.current_uri;
  const oldOpen = ui.nowPlayingOpen;
  const originalHidden = Object.getOwnPropertyDescriptor(document, "hidden");
  const requests = [];
  const results = [];
  const track = (id) => ({ id, uri: `spotify:track:${id}`, name: id, duration_ms: 180_000 });
  function current(id) {
    applyPlayback({ queue: [track(id)], current_index: 0, current_uri: `spotify:track:${id}`, playing: false });
  }
  try {
    setNowPlayingOpen(false);
    await settle();
    // Lookups are kept for the session: start from none, and keep none of these.
    canvasLookups.forget();
    window.__TAURI_INTERNALS__.invoke = (command, args) => command === "browse_canvas"
      ? new Promise((resolve, reject) => requests.push({ key: args.id, resolve, reject }))
      : oldInvoke(command, args);
    current("canvas-a");
    setNowPlayingOpen(true);
    await settle();
    check(requests.length === 1, "first panel requests Canvas A");
    setNowPlayingOpen(false);
    await settle();
    current("canvas-b");
    setNowPlayingOpen(true);
    await settle();
    check(haze.awaiting && requests.length === 2, "replacement panel awaits Canvas B");
    requests[0].resolve(null);
    await settle();
    check(haze.awaiting, "detached panel cannot clear the replacement's waiting state");
    requests[1].resolve({ url: "data:video/mp4;base64," });
    await settle();
    const oldVideo = document.querySelector(".np-video");
    check(!!oldVideo, "accepted source mounts its video");
    current("canvas-c");
    await settle();
    oldVideo.dispatchEvent(new Event("canplay"));
    await settle();
    check(haze.awaiting && !document.querySelector(".np-panel").classList.contains("video"), "old source cannot become ready during a new request");
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
    current("canvas-d");
    await settle();
    requests[2].resolve({ url: "data:video/mp4;base64,AAAA" });
    await settle();
    check(!document.querySelector(".np-panel").classList.contains("video"), "hidden track change rejects old Canvas completion");
    Object.defineProperty(document, "hidden", { configurable: true, value: false });
    document.dispatchEvent(new Event("visibilitychange"));
    await settle();
    check(requests[3]?.key === "canvas-d", "visible panel requests the deferred current track");
    results.push("Canvas unmount, stale readiness and hidden-track ownership");

    let answerDevices;
    let discoveries = 0;
    window.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command !== "personal_api_devices") return oldInvoke(command, args);
      discoveries++;
      return new Promise((resolve) => { answerDevices = resolve; });
    };
    document.querySelector(".device-btn").click();
    await settle();
    check(discoveries === 1, "device discovery begins");
    personal.status = { ...oldPersonal, connected: false };
    await settle();
    answerDevices([]);
    await settle();
    personal.status = oldPersonal;
    await settle();
    document.querySelector(".device-btn").click();
    await settle();
    check(discoveries === 2, "reactivation can start discovery again");
    answerDevices([]);
    await settle();
    results.push("Device discovery invalidation and reactivation");

    let answerTransfer;
    let transfers = 0;
    api.selectOutput = () => {
      transfers++;
      return new Promise((resolve) => { answerTransfer = resolve; });
    };
    document.querySelector(".device-row").click();
    await settle();
    check(transfers === 1, "transfer begins");
    personal.status = { ...oldPersonal, connected: false };
    await settle();
    answerTransfer();
    await settle();
    personal.status = oldPersonal;
    await settle();
    document.querySelector(".device-btn").click();
    await settle();
    answerDevices([]);
    await settle();
    const local = document.querySelector(".device-row");
    check(!local.disabled, "reactivation clears stale transfer busy state");
    local.click();
    await settle();
    check(transfers === 2, "a new transfer may begin after invalidation");
    answerTransfer();
    await settle();
    results.push("Device transfer invalidation and reactivation");

    document.querySelector(".device-btn").click();
    await settle();
    const shortcutCalls = [];
    window.__TAURI_INTERNALS__.invoke = (command, args) => {
      shortcutCalls.push(command);
      return oldInvoke(command, args);
    };
    for (const target of [
      document.querySelector(".p-volume button"),
      document.querySelector(".speed-preset"),
      document.querySelector('.rail-hit[role="slider"]'),
    ]) {
      check(!!target, "native Space regression target exists");
      const event = new KeyboardEvent("keydown", { key: " ", code: "Space", bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      check(!event.defaultPrevented, "global shortcut preserves the control's Space action");
    }
    const handled = new KeyboardEvent("keydown", { key: " ", code: "Space", bubbles: true, cancelable: true });
    handled.preventDefault();
    document.querySelector(".player").dispatchEvent(handled);
    check(!shortcutCalls.includes("play") && !shortcutCalls.includes("pause"), "handled/native Space does not toggle playback");
    document.querySelector(".player").dispatchEvent(new KeyboardEvent("keydown", { key: " ", code: "Space", bubbles: true, cancelable: true }));
    await settle();
    check(shortcutCalls.includes("play"), "noninteractive background Space still controls playback");
    results.push("Native, handled and background Space ownership");
    return results;
  } finally {
    canvasLookups.forget();
    api.selectOutput = oldSelect;
    window.__TAURI_INTERNALS__.invoke = oldInvoke;
    personal.status = oldPersonal;
    if (originalHidden) Object.defineProperty(document, "hidden", originalHidden);
    else delete document.hidden;
    document.dispatchEvent(new Event("visibilitychange"));
    setNowPlayingOpen(false);
    await settle();
    applyPlayback({ queue: oldQueue, current_index: oldIndex, current_uri: oldUri, playing: false });
    setNowPlayingOpen(oldOpen);
    await settle();
  }
}
