import { tick } from "svelte";
import { api, applyPlayback, canvasLookups, playback, ui, setNowPlayingOpen } from "../src/lib/state.svelte.js";
import { haze } from "../src/lib/ambient.svelte.js";
import { personal } from "../src/lib/personal.svelte.js";
import { frame } from "../src/lib/window-frame.svelte.js";
import { toneSettled } from "../src/lib/covertone.svelte.js";

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
    check(haze.awaiting && requests.length === 2, "reopened panel awaits Canvas B");
    requests[0].resolve(null);
    await settle();
    check(haze.awaiting, "superseded Canvas request cannot clear the reopened panel's waiting state");
    requests[1].resolve({ url: "data:video/mp4;base64," });
    await settle();
    const oldVideo = document.querySelector(".np-video");
    check(!!oldVideo, "accepted source mounts its video");
    const heldSource = document.querySelector(".np-video.shown")?.getAttribute("src") ?? null;
    current("canvas-c");
    await settle();
    oldVideo.dispatchEvent(new Event("canplay"));
    await settle();
    check(haze.awaiting && (document.querySelector(".np-video.shown")?.getAttribute("src") ?? null) === heldSource, "old source cannot become the incoming picture; an outgoing picture may stay held");
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
    current("canvas-d");
    await settle();
    requests[2].resolve({ url: "data:video/mp4;base64,AAAA" });
    await settle();
    check((document.querySelector(".np-video.shown")?.getAttribute("src") ?? null) === heldSource, "hidden track change rejects old Canvas completion without clearing retained art");
    Object.defineProperty(document, "hidden", { configurable: true, value: false });
    document.dispatchEvent(new Event("visibilitychange"));
    await settle();
    check(requests[3]?.key === "canvas-d", "visible panel requests the deferred current track");
    current("canvas-e");
    await settle();
    check(requests[4]?.key === "canvas-e", "fresh visible track starts a lookup");
    setNowPlayingOpen(false);
    await settle();
    const closedSource = "data:video/mp4;base64,CCCC";
    requests[4].resolve({ url: closedSource });
    await settle();
    check(![...document.querySelectorAll(".np-video")].some((video) => video.getAttribute("src") === closedSource), "late closed lookup does not mount a decoder");
    setNowPlayingOpen(true);
    await settle();
    check(requests.length === 5 && [...document.querySelectorAll(".np-video")].some((video) => video.getAttribute("src") === closedSource), "reopening adopts the cached lookup without another request");
    results.push("Canvas close/reopen, stale readiness and hidden-track ownership");

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
      if (command === "previous" || command === "next") return Promise.resolve();
      return oldInvoke(command, args);
    };
    const splitter = document.querySelector(".np-resize");
    check(!!splitter, "splitter exists for modifier shortcut regression");
    const widthBefore = splitter.getAttribute("aria-valuenow");
    const storedWidth = localStorage.getItem("sr.now-playing-width");
    for (const key of ["ArrowLeft", "ArrowRight"]) {
      splitter.dispatchEvent(new KeyboardEvent("keydown", { key, ctrlKey: true, bubbles: true, cancelable: true }));
    }
    await settle();
    check(splitter.getAttribute("aria-valuenow") === widthBefore && localStorage.getItem("sr.now-playing-width") === storedWidth, "transport modifiers do not resize or persist splitter width");
    check(shortcutCalls.includes("previous") && shortcutCalls.includes("next"), "splitter modifier keys retain transport shortcut ownership");
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

/** The hidden library mark must not keep doing track-membership reads. */
export async function playerLibraryControlRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const oldRead = api.getTrackPlaylists;
  const oldQueue = playback.queue;
  const oldIndex = playback.current_index;
  const oldUri = playback.current_uri;
  const oldOpen = ui.nowPlayingOpen;
  let oldLibrary = null;
  const requests = [];
  async function libraryControl(enabled = null) {
    document.querySelector(".player").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 50, clientY: 50 }));
    await settle();
    const item = [...document.querySelectorAll('[role="menuitemcheckbox"]')].find((node) => node.textContent.includes("Library checkmark"));
    check(!!item, "library preference menu item exists");
    const shown = item.getAttribute("aria-checked") === "true";
    if (enabled !== null && shown !== enabled) item.click();
    item.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await settle();
    return shown;
  }
  function current(id) {
    applyPlayback({ queue: [{ id, uri: `spotify:track:${id}`, name: id, duration_ms: 180_000 }], current_index: 0, current_uri: `spotify:track:${id}`, playing: false });
  }
  try {
    setNowPlayingOpen(false);
    await settle();
    oldLibrary = await libraryControl(true);
    api.getTrackPlaylists = (uri) => new Promise((resolve) => requests.push({ uri, resolve }));
    current("membership-before-hide");
    await settle();
    check(requests.length === 1, "visible mark asks for current membership");
    await libraryControl(false);
    current("membership-while-hidden");
    window.__harness.emit("memberships_changed", {});
    await settle();
    check(requests.length === 1, "hidden mark skips both track and membership-event reads");
    requests[0].resolve([{ id: "old", name: "Old membership" }]);
    await settle();
    await libraryControl(true);
    check(requests.length === 2 && requests[1].uri === "spotify:track:membership-while-hidden", "re-enabled mark refreshes the latest track");
    check(!document.querySelector(".p-saved-trigger"), "old in-flight membership does not show on the new track");
    requests[1].resolve([{ id: "fresh", name: "Fresh membership" }]);
    await settle();
    check(document.querySelector(".p-saved-trigger")?.getAttribute("aria-label") === "Saved in Fresh membership", "re-enabled mark shows the fresh answer");
    return ["Hidden library membership reads and fresh reactivation"];
  } finally {
    api.getTrackPlaylists = oldRead;
    applyPlayback({ queue: oldQueue, current_index: oldIndex, current_uri: oldUri, playing: false });
    if (oldLibrary !== null) await libraryControl(oldLibrary);
    setNowPlayingOpen(oldOpen);
    await settle();
  }
}

/** Real browser artwork/bitmap work, including native minimize with unchanged
    WebView focus/visibility. No account-backed surface is permitted. */
export async function ambientArtworkRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const oldQueue = playback.queue;
  const oldIndex = playback.current_index;
  const oldUri = playback.current_uri;
  const oldOpen = ui.nowPlayingOpen;
  const oldFrame = { maximized: frame.maximized, minimized: frame.minimized };
  const originalHidden = Object.getOwnPropertyDescriptor(document, "hidden");
  const NativeImage = window.Image;
  const bitmapOf = window.createImageBitmap;
  const images = [];
  const bitmapReads = [];
  const nonce = `${Date.now()}-${Math.random()}`;
  const cover = (id) => `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="#48bdad"/><title>${nonce}-${id}</title></svg>`)}`;
  function current(id) {
    const url = cover(id);
    applyPlayback({ queue: [{ id, uri: `spotify:track:${id}`, name: id, cover_url: url }], current_index: 0, current_uri: `spotify:track:${id}`, playing: false });
    return url;
  }
  const waitFor = async (condition) => {
    const until = performance.now() + 5000;
    while (!condition()) {
      check(performance.now() < until, "artwork work completes before the regression deadline");
      await new Promise((resolve) => setTimeout(resolve, 20));
      await settle();
    }
  };
  try {
    setNowPlayingOpen(false);
    window.__harness.emit("window-state", { maximized: oldFrame.maximized, minimized: false });
    Object.defineProperty(document, "hidden", { configurable: true, value: false });
    document.dispatchEvent(new Event("visibilitychange"));
    await settle();
    window.Image = new Proxy(NativeImage, {
      construct(target, args) {
        const image = Reflect.construct(target, args);
        images.push(image);
        return image;
      },
    });
    window.createImageBitmap = (...args) => {
      if (args[0] instanceof HTMLImageElement) bitmapReads.push(args[0].src);
      return bitmapOf(...args);
    };
    const first = current("artwork-visible");
    await waitFor(() => toneSettled(first) && bitmapReads.includes(first));
    await new Promise((resolve) => setTimeout(resolve, 100));
    check(bitmapReads.filter((url) => url === first).length === 1, "palette settlement does not decode the successful ambient cover twice");

    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
    const hidden = current("artwork-hidden");
    await settle();
    await new Promise((resolve) => setTimeout(resolve, 100));
    check(!images.some((image) => image.src === hidden), "hidden tracks do not start ambient or hue image reads");
    Object.defineProperty(document, "hidden", { configurable: true, value: false });
    document.dispatchEvent(new Event("visibilitychange"));
    await waitFor(() => bitmapReads.includes(hidden));

    window.__harness.emit("window-state", { maximized: oldFrame.maximized, minimized: true });
    const minimized = current("artwork-native-minimized");
    await settle();
    check(document.hidden === false && frame.minimized, "native visibility remains authoritative even when WebView stays visible");
    await new Promise((resolve) => setTimeout(resolve, 100));
    check(!images.some((image) => image.src === minimized), "native-minimized tracks do not start artwork source reads");
    window.__harness.emit("window-state", { maximized: oldFrame.maximized, minimized: false });
    await waitFor(() => bitmapReads.includes(minimized));
    return ["Successful-cover decode sharing, hidden source deferral and native minimize visibility"];
  } finally {
    window.Image = NativeImage;
    window.createImageBitmap = bitmapOf;
    if (originalHidden) Object.defineProperty(document, "hidden", originalHidden);
    else delete document.hidden;
    document.dispatchEvent(new Event("visibilitychange"));
    window.__harness.emit("window-state", oldFrame);
    applyPlayback({ queue: oldQueue, current_index: oldIndex, current_uri: oldUri, playing: false });
    setNowPlayingOpen(oldOpen);
    await settle();
  }
}
