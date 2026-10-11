import { mount, unmount, tick } from "svelte";
import { api, applyPlayback, applySession, canvasLookups, playback, ui, setNowPlayingOpen, detail, session, route, navigate, library, libraryState, setLibrary, followed } from "../src/lib/state.svelte.js";
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
    const freshMark = document.querySelector(".p-saved-trigger");
    check(!!freshMark, "re-enabled mark shows the fresh answer");
    freshMark.focus();
    await settle();
    check(document.querySelector(".p-saved-row")?.textContent.includes("Fresh membership"), "membership panel exposes the fresh returned collection");
    current("membership-after-focused-removal");
    await settle();
    check(!freshMark.isConnected && !document.querySelector(".p-saved-panel:popover-open"), "removing a focused membership mark closes its panel without transferring old focus state");
    check(requests.length === 3 && requests[2].uri === "spotify:track:membership-after-focused-removal", "focused branch removal still starts the replacement track's membership read");
    requests[2].resolve([{ id: "replacement", name: "Replacement membership" }]);
    await settle();
    const replacementMark = document.querySelector(".p-saved-trigger");
    check(!!replacementMark && !document.querySelector(".p-saved-panel:popover-open"), "the replacement mark does not inherit the removed mark's open focus state");
    replacementMark.focus();
    await settle();
    check(document.querySelector(".p-saved-panel:popover-open .p-saved-row")?.textContent.includes("Replacement membership"), "membership focus opens the replacement collection after focused branch teardown");
    document.querySelector('.p-volume [role="slider"]').focus();
    await settle();
    check(!document.querySelector(".p-saved-panel:popover-open"), "ordinary keyboard focus leaving the replacement mark closes the recovered panel");
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

/** Compiled production tables, real DOM focus/popovers and gesture ownership.
    Mount only in the isolated synthetic surface and restore all shared state. */
export async function playlistInteractionRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const [{ default: TrackList }, { default: PlaylistView }, { trackDrag }] = await Promise.all([
    import("../src/components/TrackList.svelte"),
    import("../src/views/PlaylistView.svelte"),
    import("../src/lib/dnd.svelte.js"),
  ]);
  const saved = {
    playlist: detail.playlist, route: { ...route },
    skip: api.setPlaylistTrackExcluded, play: api.playQueue,
    touch: api.touchPlaylist, edit: api.getTrackEdit, memberships: api.getTrackPlaylists,
    add: api.addPlaylistTracks, activity: api.touchPlaylistActivity, reorder: api.reorderPlaylistTracks,
  };
  const accountState = {
    playback: { ...playback }, detail: { ...detail }, followed: { ...followed },
    library: library.map((playlist) => ({ ...playlist })), libraryState: { ...libraryState }, username: session.username,
  };
  const host = document.createElement("div");
  host.className = "scroll";
  host.style.cssText = "position:fixed;left:300px;top:80px;width:900px;height:280px;overflow:auto;z-index:9999";
  document.body.appendChild(host);
  let component = null;
  const results = [];
  const rows = Array.from({ length: 80 }, (_, i) => ({
    id: i < 2 ? "duplicate" : `ui-track-${i}`,
    uri: i < 2 ? "spotify:track:duplicate" : `spotify:track:ui-track-${i}`,
    name: `UI Song ${i}`, album_id: `ui-album-${i}`, album_name: `UI Album ${i}`,
    artist_names: i === 0 ? ["UI Artist", "Other Artist"] : ["UI Artist"],
    artist_ids: i === 0 ? ["ui-artist", "other-artist"] : ["ui-artist"],
    duration_ms: 180_000, added_at: 1_700_000_000_000 + i,
  }));
  const playlist = (id, tracks = rows, excluded = []) => ({
    id, name: id, owner: session.username, owner_id: session.username,
    snapshot_id: `snapshot-${id}`, tracks, excluded_track_ids: excluded,
  });
  const frames = async () => {
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    await settle();
  };
  const mountTable = () => mount(TrackList, {
    target: host,
    props: {
      get tracks() { return detail.playlist.tracks; },
      get playlistId() { return detail.playlist.id; },
      get queueContext() { return `playlist:${detail.playlist.id}`; },
      onReorder: (from, to) => moves.push({ id: detail.playlist.id, from, to }),
      playFrom() {},
    },
  });
  const moves = [];
  const localReorders = [];
  let pointer = 200;
  function held(row) {
    const r = row.getBoundingClientRect();
    const point = { pointerId: ++pointer, isPrimary: true, button: 0, clientX: r.left + r.width / 2, clientY: r.top + 24 };
    row.dispatchEvent(new PointerEvent("pointerdown", { ...point, bubbles: true, cancelable: true }));
    window.dispatchEvent(new PointerEvent("pointermove", { ...point, clientX: point.clientX + 10, bubbles: true }));
    return point;
  }
  function release(point, row) {
    const r = row.getBoundingClientRect();
    window.dispatchEvent(new PointerEvent("pointerup", { ...point, clientX: r.left + r.width / 2, clientY: r.top + 40, bubbles: true }));
    // Native pointer release produces a click too. Dispatch it so the drag's
    // release-click guard cannot consume the next intentional menu action.
    row.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, clientX: r.left + r.width / 2, clientY: r.top + 40 }));
  }
  async function dropComponent() {
    if (component) await unmount(component);
    component = null;
    host.replaceChildren();
  }
  async function rowMenu(index = 0) {
    host.querySelector(`.tl-row[data-i="${index}"] [title="More options"]`).click();
    await settle();
  }
  const menuItem = (text) => [...document.querySelectorAll(".track-menu .menu-item")]
    .find((button) => button.textContent.trim() === text);
  try {
    navigate("library");
    await settle();
    api.getTrackEdit = () => Promise.resolve({});
    api.getTrackPlaylists = () => Promise.resolve([]);
    detail.playlist = playlist("ui-owner-a");
    component = mountTable();
    await frames();
    const focused = host.querySelector('.tl-row[data-i="0"] .c-album');
    focused.focus({ preventScroll: true });
    const mountedBefore = host.querySelectorAll(".tl-row").length;
    detail.playlist = playlist("ui-owner-a", rows.map((track) => ({ ...track })).reverse());
    await settle();
    check(document.activeElement === focused && focused.textContent.trim() === "UI Album 0", "background reverse retains the focused duplicate occurrence, not the absolute index or first same URI");
    check(focused.closest(".tl-row").dataset.i === "79", "focused row follows its authoritative new position");
    check(host.querySelectorAll(".tl-row").length <= mountedBefore + 1, "focus retention adds one offscreen row, not the whole table");
    results.push("Track focus identity across fresh authoritative reversal and duplicate URI");

    detail.playlist = playlist("ui-owner-a");
    await frames();
    const point = held(host.querySelector('.tl-row[data-i="1"]'));
    check(trackDrag.active, "row movement activates a real held drag");
    await dropComponent();
    check(!trackDrag.active, "table unmount cancels its held drag");
    detail.playlist = playlist("ui-owner-b");
    component = mountTable();
    await frames();
    release(point, host.querySelector('.tl-row[data-i="3"]'));
    check(moves.length === 0, "release after navigation cannot reorder a newly mounted playlist");
    const switched = held(host.querySelector('.tl-row[data-i="1"]'));
    detail.playlist = playlist("ui-owner-c");
    await settle();
    release(switched, host.querySelector('.tl-row[data-i="3"]'));
    check(!trackDrag.active && moves.length === 0, "changing the retained table owner cancels rather than retargeting a drag");
    const ordinary = held(host.querySelector('.tl-row[data-i="1"]'));
    release(ordinary, host.querySelector('.tl-row[data-i="3"]'));
    check(moves.length === 1 && moves[0].id === "ui-owner-c" && moves[0].from === 1 && moves[0].to === 3, "ordinary same-table reorder retains its exact source and landing occurrence");
    const ambiguousRows = [rows[0], { ...rows[0] }, ...rows.slice(2)];
    detail.playlist = playlist("ui-owner-c", ambiguousRows);
    await settle();
    const ambiguousFocus = host.querySelector('.tl-row[data-i="0"] .c-album');
    ambiguousFocus.focus({ preventScroll: true });
    const ambiguous = held(host.querySelector('.tl-row[data-i="1"]'));
    detail.playlist = playlist("ui-owner-c", ambiguousRows.slice(1));
    await settle();
    release(ambiguous, host.querySelector('.tl-row[data-i="3"]'));
    check(moves.length === 1, "duplicate multiplicity change invalidates a held occurrence instead of taking the first same URI");
    check(document.activeElement !== ambiguousFocus, "indistinguishable duplicate multiplicity change invalidates focus before the reused row can stand for a different occurrence");
    detail.playlist = playlist("ui-owner-c", ambiguousRows);
    await frames();
    const equalCountFocus = host.querySelector('.tl-row[data-i="0"] .c-album');
    equalCountFocus.focus({ preventScroll: true });
    detail.playlist = playlist("ui-owner-c", [ambiguousRows[1], ambiguousRows[0], ...ambiguousRows.slice(2)].map((track) => ({ ...track })));
    await settle();
    check(document.activeElement !== equalCountFocus, "fresh indistinguishable duplicates invalidate focus even when their multiplicity stays equal");
    results.push("Held drag unmount/owner/duplicate cancellation and ordinary same-list reorder");
    detail.playlist = playlist("ui-owner-c");
    await frames();
    const virtualSource = host.querySelector('.tl-row[data-i="1"]');
    const virtualDrag = held(virtualSource);
    host.scrollTop = 2300;
    host.dispatchEvent(new Event("scroll"));
    await frames();
    check(!virtualSource.isConnected && trackDrag.active, "normal window recycling does not cancel its connected table owner's drag");
    const hostRect = host.getBoundingClientRect();
    const virtualTarget = [...host.querySelectorAll(".tl-row")].find((row) => {
      const rect = row.getBoundingClientRect();
      return rect.top >= hostRect.top && rect.bottom <= hostRect.bottom;
    });
    check(!!virtualTarget, "windowed drag has a connected visible landing row");
    release(virtualDrag, virtualTarget);
    check(moves.length === 2 && moves[1].from === 1 && moves[1].to > 30, "same-list drag resolves a virtualized landing without requiring its source row DOM");
    host.scrollTop = 0;
    host.dispatchEvent(new Event("scroll"));
    await frames();
    const copies = [];
    api.addPlaylistTracks = (id, uris) => { copies.push({ id, uris }); return Promise.resolve(); };
    api.touchPlaylistActivity = () => Promise.resolve();
    const destination = document.querySelector(".lib-row[data-pid]");
    check(!!destination, "synthetic library provides a real cross-list drop target");
    const copied = held(host.querySelector('.tl-row[data-i="1"]'));
    release(copied, destination);
    await settle();
    check(copies.length === 1 && copies[0].id === destination.dataset.pid && copies[0].uris[0] === rows[1].uri && moves.length === 2, "cross-list song drops still add to the destination without reordering either playlist");
    results.push("Virtualized source-row recycling and cross-list add drops");

    detail.playlist = playlist("ui-owner-c");
    await settle();
    await rowMenu();
    const parentTrigger = menuItem("Add to playlist");
    check(!!parentTrigger, "playlist submenu trigger exists");
    parentTrigger.click();
    await settle();
    const submenu = document.querySelector('.track-submenu[aria-label="Add to playlist"]');
    check(!!submenu, "playlist submenu opens");
    submenu.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await settle();
    check(document.activeElement === parentTrigger && !!document.querySelector(".track-menu") && !document.querySelector(".track-submenu"), "submenu Escape returns focus to the open parent menu trigger");
    parentTrigger.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await settle();
    check(!document.querySelector(".track-menu"), "second Escape dismisses the parent menu");
    await rowMenu();
    const artistTrigger = menuItem("Go to artist");
    artistTrigger.click();
    await settle();
    document.querySelector('.track-submenu[aria-label="Go to artist"]').dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await settle();
    check(document.activeElement === artistTrigger && !!document.querySelector(".track-menu"), "artist submenu Escape also returns to its parent trigger");
    artistTrigger.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await settle();
    results.push("Nested menu Escape focus and group dismissal");

    await dropComponent();
    const localSource = `ui-local-duplicate-${crypto.randomUUID()}`;
    const localRows = [
      { ...rows[0], name: "Duplicate A1", album_name: "Duplicate album A1" },
      { ...rows[0], name: "Duplicate A2", album_name: "Duplicate album A2" },
      { ...rows[2], name: "Distinct B" },
    ];
    api.reorderPlaylistTracks = (id, from, to) => new Promise((resolve) => localReorders.push({ id, from, to, resolve }));
    navigate("playlist", localSource);
    detail.playlist = playlist(localSource, localRows);
    component = mount(PlaylistView, { target: host });
    await frames();
    host.querySelector(".tl").scrollIntoView({ block: "start" });
    await frames();
    const locallyFocused = host.querySelector('.tl-row[data-i="0"] .c-album');
    locallyFocused.focus({ preventScroll: true });
    const localDrag = held(host.querySelector('.tl-row[data-i="0"]'));
    release(localDrag, host.querySelector('.tl-row[data-i="1"]'));
    await settle();
    check(localReorders.length === 1 && localReorders[0].id === localSource && localReorders[0].from === 0 && localReorders[0].to === 1, "local duplicate reorder targets the captured storage occurrence");
    check(document.activeElement === locallyFocused && locallyFocused.textContent.trim() === localRows[0].album_name && locallyFocused.closest(".tl-row").dataset.i === "1", "known local duplicate move carries the same focused DOM and metadata to its new occurrence position");
    check(detail.playlist.tracks.map((track) => track.name).join(",") === [localRows[1], localRows[0], localRows[2]].map((track) => track.name).join(","), "optimistic duplicate reorder keeps actual row order rather than rebasing metadata onto old ordinal keys");
    localReorders[0].resolve();
    await settle();
    window.__harness.emit("playlist", {
      ...detail.playlist,
      tracks: detail.playlist.tracks.map((track) => ({ ...track, cached: true })),
    });
    await settle();
    check(document.activeElement !== locallyFocused, "an actual playlist refresh replacing opaque duplicate rows through a legitimate mutable field invalidates focus rather than guessing occurrence identity");
    await dropComponent();
    navigate("library");
    await settle();
    results.push("Known local exact-duplicate reorder preserves focus; fresh ambiguous duplicates invalidate focus");
    detail.playlist = playlist("ui-skip-source", rows.slice(0, 4));
    const skips = [];
    api.setPlaylistTrackExcluded = (id, trackId, excluded) => new Promise((resolve, reject) => skips.push({ id, trackId, excluded, resolve, reject }));
    component = mount(PlaylistView, { target: host });
    await frames();
    host.querySelector(".tl").scrollIntoView({ block: "start" });
    await frames();
    await rowMenu();
    menuItem("Skip during playlist playback").click();
    await settle();
    check(skips.length === 1 && host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "pending skip appears optimistically");
    detail.playlist = playlist("ui-skip-source", rows.slice(0, 4), [rows[2].id]);
    await settle();
    check(host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped") && host.querySelector('.tl-row[data-i="2"]').classList.contains("skipped"), "pre-ACK authoritative replacement overlays pending intent while preserving another authoritative skip");
    skips[0].reject(new Error("synthetic skip refusal"));
    await settle();
    check(!host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped") && host.querySelector('.tl-row[data-i="2"]').classList.contains("skipped"), "failure rolls back against the live snapshot, not a captured exclusions array");
    await rowMenu();
    menuItem("Skip during playlist playback").click();
    await settle();
    detail.playlist = playlist("ui-skip-source", rows.slice(0, 4), [rows[2].id]);
    await settle();
    skips[1].resolve();
    await settle();
    check(host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "ACK cannot erase intent before its authoritative post-command refresh");
    detail.playlist = playlist("ui-skip-source", rows.slice(0, 4), [rows[0].id, rows[2].id]);
    await settle();
    detail.playlist = playlist("ui-skip-source", rows.slice(0, 4), [rows[2].id]);
    await settle();
    check(!host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "a later authoritative change wins after pending acknowledgement is reconciled");
    results.push("Pending skip overlay, live failure rollback and authoritative final reconciliation");

    await rowMenu();
    menuItem("Skip during playlist playback").click();
    await settle();
    document.querySelector(".track-menu").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await settle();
    await rowMenu();
    menuItem("Include in playlist playback").click();
    await settle();
    check(skips.length === 3 && !host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "newer same-track intent overlays immediately while the older write remains in flight");
    skips[2].resolve();
    await settle();
    check(skips.length === 4 && skips[3].excluded === false && !host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "old ACK dispatches the queued newer intent without erasing its optimistic state");
    detail.playlist = playlist("ui-skip-source", rows.slice(0, 4), [rows[0].id, rows[2].id]);
    await settle();
    check(!host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "latest include survives an authoritative snapshot of the older skip");
    skips[3].reject(new Error("synthetic newer include refusal"));
    await settle();
    check(host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "failed latest include restores the live authoritative skip, not the original base");
    results.push("Latest repeated skip intent owns serialized writes and live rollback");
    await rowMenu();
    menuItem("Include in playlist playback").click();
    await settle();
    document.querySelector(".track-menu").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await settle();
    await rowMenu();
    menuItem("Skip during playlist playback").click();
    await settle();
    detail.playlist = playlist("ui-skip-destination", rows.slice(0, 4), [rows[2].id]);
    await settle();
    skips[4].resolve();
    await settle();
    check(skips.length === 6 && skips[5].id === "ui-skip-source" && skips[5].excluded, "queued intent keeps its captured source after ordinary navigation");
    skips[5].reject(new Error("old playlist queued write refused"));
    await settle();
    check(!host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped") && host.querySelector('.tl-row[data-i="2"]').classList.contains("skipped"), "old playlist completion cannot roll back another live playlist's authoritative skips");
    detail.playlist = playlist("ui-skip-source", rows.slice(0, 4), [rows[2].id]);
    await settle();
    results.push("Queued skip source ownership across ordinary navigation");

    const plays = [];
    const touches = [];
    api.playQueue = (_queue, _index, source) => new Promise((resolve) => plays.push({ source, resolve }));
    api.touchPlaylist = (id) => { touches.push(id); return Promise.resolve(); };
    host.querySelector(".play-lg").click();
    await settle();
    check(plays[0]?.source === "playlist:ui-skip-source", "play captures its source playlist");
    detail.playlist = playlist("ui-play-destination", rows.slice(0, 4));
    await settle();
    plays[0].resolve();
    await settle();
    check(touches.length === 1 && touches[0] === "ui-skip-source", "late play acknowledgement credits the source, not the newly opened playlist");
    results.push("Playlist play source attribution after ordinary navigation");
    detail.playlist = playlist("ui-account-source", rows.slice(0, 4));
    await settle();
    host.querySelector(".play-lg").click();
    await rowMenu();
    menuItem("Skip during playlist playback").click();
    await settle();
    check(plays.length === 2 && skips.length === 7, "play and skip are held before an account transition");
    applySession({ auth_state: "ready", username: "ui-other-account" });
    detail.playlist = playlist("ui-account-source", rows.slice(0, 4), []);
    await settle();
    plays[1].resolve();
    skips[6].reject(new Error("old account reply"));
    await settle();
    check(touches.length === 1 && !host.querySelector('.tl-row[data-i="0"]').classList.contains("skipped"), "old account play and skip replies cannot credit or roll back the new account's same-ID playlist");
    results.push("Account generation fences pending playlist play and skip replies");
    return results;
  } finally {
    window.dispatchEvent(new PointerEvent("pointercancel", { pointerId: pointer, bubbles: true }));
    await dropComponent();
    for (const request of localReorders) request.resolve();
    await settle();
    host.remove();
    api.setPlaylistTrackExcluded = saved.skip;
    api.playQueue = saved.play;
    api.touchPlaylist = saved.touch;
    api.getTrackEdit = saved.edit;
    api.getTrackPlaylists = saved.memberships;
    api.addPlaylistTracks = saved.add;
    api.touchPlaylistActivity = saved.activity;
    api.reorderPlaylistTracks = saved.reorder;
    applySession({ auth_state: accountState.playback.auth_state, username: accountState.username });
    applyPlayback({ ...accountState.playback, queue_revision: 0, order_revision: 0 });
    setLibrary(accountState.library, { fresh: accountState.libraryState.fresh, tree: accountState.libraryState.tree });
    libraryState.loaded = accountState.libraryState.loaded;
    Object.assign(followed, accountState.followed);
    Object.assign(detail, accountState.detail);
    detail.playlist = saved.playlist;
    navigate(saved.route.name, saved.route.id, saved.route.param);
    await settle();
  }
}

/** Retained keyboard anchors remain bounded while production queue/history
    windows scroll, and never attach to a recycled row after removal/filtering. */
export async function queueHistoryFocusRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const [{ default: QueueView }, { default: HistoryView }] = await Promise.all([
    import("../src/views/QueueView.svelte"), import("../src/views/HistoryView.svelte"),
  ]);
  const savedPlayback = { ...playback };
  const savedHistory = api.getHistory;
  const savedRoute = { ...route };
  const host = document.createElement("div");
  host.className = "scroll";
  host.style.cssText = "position:fixed;left:30px;top:80px;width:900px;height:320px;overflow:auto;z-index:9999";
  document.body.appendChild(host);
  let component;
  const tracks = Array.from({ length: 200 }, (_, i) => ({
    id: `focus-${i}`, uri: `spotify:track:focus-${i}`, name: `Focus ${i}`,
    artist_names: ["Focus artist"], duration_ms: 180_000, context: "playlist:focus",
  }));
  const frames = async () => {
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    await settle();
  };
  async function drop() {
    if (component) await unmount(component);
    component = null;
    host.replaceChildren();
    host.scrollTop = 0;
  }
  try {
    navigate("library");
    await settle();
    applyPlayback({ queue: tracks, queue_revision: 0, current_index: 0, current_uri: tracks[0].uri, upcoming: tracks.slice(1).map((_, i) => i + 1), context: "playlist:focus", playing: false });
    component = mount(QueueView, { target: host });
    await frames();
    const queueButton = [...host.querySelectorAll(".tl-row")].find((row) => row.textContent.includes("Focus 2"))?.querySelector('[title="Remove from queue"]');
    check(!!queueButton, "queue focus target is mounted");
    queueButton.focus({ preventScroll: true });
    host.scrollTop = 4800;
    host.dispatchEvent(new Event("scroll"));
    await frames();
    check(document.activeElement === queueButton && queueButton.isConnected, "scrolling the queue retains its focused offscreen occurrence");
    check(host.querySelectorAll(".tl-row").length < 30, "queue focus retention remains bounded");
    const shortened = tracks.slice(1);
    applyPlayback({ queue: shortened, queue_revision: 0, current_index: 0, current_uri: shortened[0].uri, upcoming: shortened.slice(1).map((_, i) => i + 1) });
    await settle();
    check(document.activeElement === queueButton && queueButton.closest(".tl-row").textContent.includes("Focus 2"), "removing an earlier queue row preserves the same focused occurrence, not its old index");
    applyPlayback({ queue: shortened.filter((track) => track.id !== "focus-2"), queue_revision: 0, upcoming: Array.from({ length: shortened.length - 2 }, (_, i) => i + 1) });
    await settle();
    check(document.activeElement !== queueButton && !queueButton.isConnected, "removing the focused occurrence does not transfer focus to its replacement");
    const duplicates = [tracks[0], { ...tracks[0] }, ...tracks.slice(1)];
    applyPlayback({ queue: duplicates, queue_revision: 0, current_index: 0, current_uri: tracks[0].uri, upcoming: duplicates.slice(1).map((_, i) => i + 1) });
    host.scrollTop = 0;
    host.dispatchEvent(new Event("scroll"));
    await frames();
    const duplicateFocus = host.querySelector(".tl-row .go");
    duplicateFocus.focus({ preventScroll: true });
    applyPlayback({ queue: duplicates.slice(1), queue_revision: 0, current_index: 0, upcoming: Array.from({ length: duplicates.length - 2 }, (_, i) => i + 1) });
    await settle();
    check(document.activeElement !== duplicateFocus, "changing indistinguishable queue duplicate multiplicity invalidates focus rather than reusing the first same URI");
    applyPlayback({ queue: duplicates, queue_revision: 0, current_index: 0, current_uri: tracks[0].uri, upcoming: duplicates.slice(1).map((_, i) => i + 1) });
    host.scrollTop = 0;
    host.dispatchEvent(new Event("scroll"));
    await frames();
    const equalCountQueueFocus = host.querySelector(".tl-row .go");
    equalCountQueueFocus.focus({ preventScroll: true });
    applyPlayback({ queue: [duplicates[1], duplicates[0], ...duplicates.slice(2)].map((track) => ({ ...track })), queue_revision: 0 });
    await settle();
    check(document.activeElement !== equalCountQueueFocus, "fresh exact-duplicate queue reorder invalidates focus without requiring a cardinality change");
    await drop();

    const entries = tracks.map((track, i) => ({
      track_id: track.id, track, started_at: 1_700_000_000_000 + i * 60_000,
      context: "playlist:focus", ms_played: 60_000, completed: false,
    }));
    const reads = [];
    api.getHistory = (offset, limit, query) => {
      reads.push({ offset, query });
      const selected = query ? entries.filter((entry) => entry.track.name === query) : entries;
      return Promise.resolve({ entries: selected.slice(offset, offset + limit), total: selected.length, recorded: entries.length });
    };
    component = mount(HistoryView, { target: host });
    await frames();
    host.querySelector(".history-list").scrollIntoView({ block: "start" });
    await frames();
    const historyButton = host.querySelector('.hi-main[title="Play Focus 0"]');
    check(!!historyButton, "history focus target is loaded");
    historyButton.focus({ preventScroll: true });
    host.scrollTop = 5600;
    host.dispatchEvent(new Event("scroll"));
    await frames();
    check(document.activeElement === historyButton && historyButton.isConnected, "paged history scroll keeps its focused play mounted");
    check(host.querySelectorAll(".hi-row").length < 30 && reads.some((read) => read.offset === 100), "history retains one anchor and requests the next bounded page, not all rows");
    const filter = host.querySelector('[aria-label="Filter listening history"]');
    filter.value = "Focus 150";
    filter.dispatchEvent(new Event("input", { bubbles: true }));
    const deadline = performance.now() + 3000;
    while (!reads.some((read) => read.query === "Focus 150")) {
      check(performance.now() < deadline, "history filter commits before the regression deadline");
      await new Promise((resolve) => setTimeout(resolve, 20));
      await settle();
    }
    await frames();
    check(!historyButton.isConnected && document.activeElement !== historyButton, "filter generation drops the old focused occurrence rather than recycling it as another play");
    return ["Queue bounded offscreen focus, earlier-row removal and focused-row removal", "History bounded offscreen focus across pagination and filter invalidation"];
  } finally {
    await drop();
    host.remove();
    api.getHistory = savedHistory;
    applyPlayback({ ...savedPlayback, queue_revision: 0, order_revision: 0 });
    navigate(savedRoute.name, savedRoute.id, savedRoute.param);
    await settle();
  }
}

/** Exercise the real inline rule builder; no removal command is ever sent. */
export async function cleanupOperatorRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const { default: PlaylistCleanup } = await import("../src/components/PlaylistCleanup.svelte");
  const host = document.createElement("div");
  document.body.appendChild(host);
  let component;
  const tracks = [
    { id: "rule-a", uri: "spotify:track:rule-a", name: "Rule A", artist_names: ["Artist A"], artist_ids: ["artist-a"], album_id: "album-a", album_name: "Album A" },
    { id: "rule-b", uri: "spotify:track:rule-b", name: "Rule B", artist_names: ["Artist B"], artist_ids: ["artist-b"], album_id: "album-b", album_name: "Album B" },
  ];
  const pick = async (label, text) => {
    host.querySelector(`[aria-label="${label}"][role="combobox"]`).click();
    await settle();
    const option = [...host.querySelectorAll(`[role="listbox"][aria-label="${label}"] [role="option"]`)].find((node) => node.textContent.trim() === text);
    check(!!option, `${label} exposes ${text}`);
    option.click();
    await settle();
  };
  async function addChoice(name) {
    const input = host.querySelector("#cleanup-value");
    input.focus();
    input.value = name;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await settle();
    const choice = host.querySelector("#cleanup-suggestions [role=option]");
    check(!!choice, "playlist-local identity suggestion exists");
    choice.click();
    await settle();
  }
  try {
    for (const field of ["artist", "album"]) {
      component = mount(PlaylistCleanup, { target: host, props: { playlist: { id: "ui-rule-source", tracks, snapshot_id: "rules" }, onClose() {} } });
      await settle();
      if (field === "album") await pick("Rule type", "Album");
      await addChoice(field === "artist" ? "Artist A" : "Album A");
      await addChoice(field === "artist" ? "Artist A" : "Album A");
      check(host.querySelectorAll(".cleanup-chips .chip").length === 1, `${field} identical rule deduplicates`);
      await pick(`Match ${field}`, "is not");
      await addChoice(field === "artist" ? "Artist A" : "Album A");
      check(host.querySelectorAll(".cleanup-chips .chip").length === 2, `${field} opposite operator remains a distinct rule`);
      check(host.querySelectorAll(".cleanup-results tbody tr").length === 0, `${field} opposite rules in AND select no removals`);
      await pick("Match rules", "any rule (OR)");
      check([...host.querySelectorAll(".cleanup-results tbody tr .cleanup-song strong")].map((node) => node.textContent).join(",") === tracks.map((track) => track.name).join(","), `${field} opposite rules in OR select both actual entries`);
      await unmount(component);
      component = null;
      host.replaceChildren();
    }
    return ["Artist/album opposite operators remain distinct; identical dedupe, AND none and OR all"];
  } finally {
    if (component) await unmount(component);
    host.remove();
    await settle();
  }
}

/** Production PlayerBar preview and Slider handlers with a scoped capture API
    stand-in: Chromium cannot capture a programmatically dispatched pointer. */
export async function seekSliderRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const saved = {
    playback: { ...playback }, detail: { ...detail }, followed: { ...followed },
    library: library.map((playlist) => ({ ...playlist })), libraryState: { ...libraryState }, username: session.username,
    route: { ...route }, seek: api.seek, volume: api.setVolume, previous: api.previous, next: api.next,
  };
  const seeks = [], volumes = [], transport = [];
  const captures = [];
  let pointer = 500;
  const track = (id) => ({ id, uri: `spotify:track:${id}`, name: id, duration_ms: 180_000 });
  const current = (id) => applyPlayback({ queue: [track(id)], queue_revision: 0, current_index: 0, current_uri: `spotify:track:${id}`, duration_ms: 180_000, position_ms: 0, context: "playlist:seek-regression", playing: false });
  function captureBoundary(node) {
    const descriptors = ["setPointerCapture", "releasePointerCapture", "hasPointerCapture"].map((key) => [key, Object.getOwnPropertyDescriptor(node, key)]);
    let captured = null;
    Object.defineProperties(node, {
      setPointerCapture: { configurable: true, value: (id) => { captured = id; } },
      releasePointerCapture: { configurable: true, value: (id) => { if (captured === id) captured = null; } },
      hasPointerCapture: { configurable: true, value: (id) => captured === id },
    });
    captures.push(() => {
      for (const [key, descriptor] of descriptors) {
        if (descriptor) Object.defineProperty(node, key, descriptor);
        else delete node[key];
      }
    });
    return () => captured;
  }
  function begin(node, fraction = 0.6) {
    const rect = node.getBoundingClientRect();
    const point = { pointerId: ++pointer, isPrimary: true, button: 0, clientX: rect.left + rect.width * fraction, clientY: rect.top + rect.height / 2 };
    node.dispatchEvent(new PointerEvent("pointerdown", { ...point, bubbles: true, cancelable: true }));
    return point;
  }
  function end(node, point, type = "pointerup") {
    node.dispatchEvent(new PointerEvent(type, { ...point, bubbles: true, cancelable: true }));
  }
  try {
    api.seek = (value) => { seeks.push(value); return Promise.resolve(); };
    api.setVolume = (value) => { volumes.push(value); return Promise.resolve(); };
    api.previous = () => { transport.push("previous"); return Promise.resolve(); };
    api.next = () => { transport.push("next"); return Promise.resolve(); };
    current("seek-a");
    await settle();
    const seek = document.querySelector('.p-seek [role="slider"]');
    const volume = document.querySelector('.p-volume [role="slider"]');
    check(!!seek && !!volume, "seek and volume controls are mounted");
    const heldPointer = captureBoundary(seek);
    const volumePointer = captureBoundary(volume);
    const oldTrack = begin(seek);
    check(heldPointer() === oldTrack.pointerId, "seek acquires the gesture's capture");
    current("seek-b");
    end(seek, oldTrack);
    await settle();
    check(seeks.length === 0 && heldPointer() === null && seek.getAttribute("aria-valuenow") === "0", "same-task track transition cancels old seek, capture and preview before the reactive flush");
    applyPlayback({ queue: [track("seek-duplicate"), track("seek-duplicate")], queue_revision: 0, current_index: 0, current_uri: "spotify:track:seek-duplicate", position_ms: 0 });
    await settle();
    const duplicate = begin(seek);
    applyPlayback({ current_index: 1, current_uri: "spotify:track:seek-duplicate", position_ms: 0 });
    end(seek, duplicate);
    await settle();
    check(seeks.length === 0 && heldPointer() === null, "same-URI next occurrence cannot inherit a held seek");
    const stable = begin(seek);
    applyPlayback({ position_ms: 1200, playing: false });
    applyPlayback({ queue: [...playback.queue, track("seek-tail")], queue_revision: 0 });
    end(seek, stable);
    await settle();
    check(seeks.length === 1 && Math.abs(seeks[0] - playback.duration_ms * 0.6) <= 1 && heldPointer() === null, "stable occurrence heartbeat and tail append commit the held seek position once");
    for (const type of ["pointercancel", "lostpointercapture"]) {
      const cancelled = begin(seek, 0.8);
      end(seek, cancelled, type);
      end(seek, cancelled);
      await settle();
      check(seeks.length === 1 && heldPointer() === null, `${type} clears preview/capture without committing`);
    }
    applyPlayback({ volume: 20 });
    await settle();
    const heldVolume = begin(volume, 0.7);
    end(volume, heldVolume);
    await settle();
    check(volumes.length > 0 && volumes.every((value) => Math.abs(value - 70) <= 1) && volumePointer() === null, "ordinary volume gesture commits the pointer's percentage and releases capture");
    for (const node of [seek, volume]) {
      const beforeSeek = seeks.length, beforeVolume = volumes.length;
      node.focus();
      for (const modifiers of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }]) {
        for (const key of ["ArrowLeft", "ArrowRight"]) {
          node.dispatchEvent(new KeyboardEvent("keydown", { key, ...modifiers, bubbles: true, cancelable: true }));
        }
      }
      await settle();
      check(seeks.length === beforeSeek && volumes.length === beforeVolume, "modified transport/navigation arrows do not edit seek or volume");
      node.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }));
      await settle();
      check(node === seek ? seeks.length === beforeSeek + 1 : volumes.length === beforeVolume + 1, "unmodified slider arrow retains its ordinary action");
    }
    check(transport.filter((value) => value === "previous").length === 4 && transport.filter((value) => value === "next").length === 4, "Ctrl/Meta arrows reach transport exactly once from both sliders");
    const account = begin(seek);
    applySession({ auth_state: "ready", username: "seek-other-account" });
    end(seek, account);
    await settle();
    check(seeks.length === 2 && heldPointer() === null, "account transition cancels the old held seek even before a track changes");
    return ["Occurrence/account-bound seek cancellation and preview/capture cleanup", "Heartbeat/append stability, cancellation and ordinary volume/keyboard", "Modified slider arrows remain transport/navigation-only"];
  } finally {
    for (const node of document.querySelectorAll('.p-seek [role="slider"], .p-volume [role="slider"]')) {
      node.dispatchEvent(new PointerEvent("pointercancel", { pointerId: pointer, bubbles: true }));
    }
    for (const restore of captures) restore();
    api.seek = saved.seek;
    api.setVolume = saved.volume;
    api.previous = saved.previous;
    api.next = saved.next;
    applySession({ auth_state: saved.playback.auth_state, username: saved.username });
    applyPlayback({ ...saved.playback, queue_revision: 0, order_revision: 0 });
    setLibrary(saved.library, { fresh: saved.libraryState.fresh, tree: saved.libraryState.tree });
    libraryState.loaded = saved.libraryState.loaded;
    Object.assign(followed, saved.followed);
    Object.assign(detail, saved.detail);
    navigate(saved.route.name, saved.route.id, saved.route.param);
    await settle();
  }
}
