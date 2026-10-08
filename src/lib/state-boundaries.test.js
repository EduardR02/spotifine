import { afterEach, beforeEach, expect, test } from "bun:test";

globalThis.$state ??= Object.assign((value) => value, { raw: (value) => value });
const {
  api, applyPlayback, applySession, playback, session, route, navigate,
  library, libraryState, setLibrary, detail, loadDetail,
  loadTrackCredits, trackCredits, openCredits, credits, closeCredits,
  loadPlaylistRecommendations, removeFromLibrary, addToLibrary,
  renameLibraryFolder, moveLibraryItem, lookupSavedIn, invalidateSavedIn, nowSaved, playCatalogueContext,
} = await import("./state.svelte.js");
const { setFollowingArtist } = await import("./personal.svelte.js");
const settle = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const folder = (name) => ({ kind: "folder", id: "box", name, children: [] });
const track = (id) => ({ id, uri: `spotify:track:${id}`, name: id });
let previousWindow, requests, accountNumber = 0;

beforeEach(() => {
  previousWindow = globalThis.window;
  requests = [];
  globalThis.window = { __TAURI_INTERNALS__: {
    invoke: (command, args) => new Promise((resolve, reject) => requests.push({ command, args, resolve, reject })),
  } };
  applyPlayback({ ready: true, auth_state: "ready", username: `boundary-${++accountNumber}`, playing: false });
});
afterEach(() => {
  applySession({ auth_state: "logged_out", username: null });
  applyPlayback({ playing: false });
  closeCredits();
  if (previousWindow === undefined) delete globalThis.window;
  else globalThis.window = previousWindow;
});

test("panel and dialog share pending credits, including a reopened dialog", async () => {
  loadTrackCredits(track("credits-shared"));
  openCredits(track("credits-shared"));
  closeCredits();
  openCredits(track("credits-shared"));
  expect(requests.filter((request) => request.command === "browse_track_credits")).toHaveLength(1);
  requests[0].resolve({ groups: [{ title: "Performed by", contributors: [{ name: "Singer" }] }] });
  await settle();
  expect(trackCredits.loading).toBe(false);
  expect(credits.loading).toBe(false);
  expect(credits.data.groups[0].contributors[0].name).toBe("Singer");
  expect(trackCredits.data).toBe(credits.data);
});

test("superseded credits still warm the shared cache and rejected reads may retry", async () => {
  loadTrackCredits(track("credits-away"));
  loadTrackCredits(track("credits-current"));
  requests[0].resolve({ groups: [], id: "credits-away" });
  requests[1].resolve({ groups: [], id: "credits-current" });
  await settle();
  openCredits(track("credits-away"));
  expect(credits.data.id).toBe("credits-away");
  expect(trackCredits.data.id).toBe("credits-current");
  expect(requests).toHaveLength(2);
  openCredits(track("credits-retry"));
  requests[2].reject(new Error("offline"));
  await settle();
  expect(credits.loading).toBe(false);
  expect(credits.data).toBeNull();
  expect(credits.error).toBeTruthy();
  openCredits(track("credits-retry"));
  requests[3].resolve({ groups: [{ title: "Performed by", contributors: [{ name: "Retry singer" }] }], id: "credits-retry" });
  await settle();
  expect(credits.data.id).toBe("credits-retry");
  expect(credits.data.groups[0].contributors[0].name).toBe("Retry singer");
  expect(credits.error).toBeNull();
});

test("logout clears private library/detail and late detail cannot publish into another account", async () => {
  setLibrary([{ id: "private", name: "Private collection" }], { fresh: true, tree: [folder("Private folder")] });
  navigate("playlist", "private");
  loadDetail();
  const old = requests[0];
  applySession({ auth_state: "logged_out", username: null });
  expect(library).toHaveLength(0);
  expect(libraryState.loaded).toBe(false);
  expect(libraryState.tree).toEqual([]);
  expect(detail.playlist).toBeNull();
  expect(route.name).toBe("library");
  applyPlayback({ ready: true, auth_state: "ready", username: "boundary-new", playing: false });
  old.resolve({ id: "private", name: "Private collection", tracks: [track("private-track")] });
  await settle();
  expect(detail.playlist).toBeNull();
  expect(library).toHaveLength(0);
});

test("same-account temporary disconnect preserves offline content but fences in-flight pages", async () => {
  const account = session.username;
  const offline = { id: "offline", tracks: [track("kept")] };
  setLibrary([{ id: "offline", name: "Offline" }], { fresh: true });
  navigate("playlist", "offline");
  detail.playlist = offline;
  const pending = api.browseLikedSongs();
  const rejected = pending.then(() => ({ rejected: false }), (error) => ({ rejected: true, error }));
  applyPlayback({ auth_state: "disconnected", username: null, playing: false });
  expect(library[0].id).toBe("offline");
  expect(detail.playlist).toBe(offline);
  expect(route.id).toBe("offline");
  requests[0].resolve({ tracks: [track("old-page")], next_cursor: null });
  const outcome = await rejected;
  expect(outcome.rejected).toBe(true);
  expect(outcome.error).toBeInstanceOf(Error);
  applyPlayback({ ready: true, auth_state: "ready", username: account, playing: false });
  await settle();
  expect(detail.playlist).toBe(offline);
  expect(library[0].id).toBe("offline");
  expect(requests).toHaveLength(1);
});

test("account switch discards pending recommendations and does not cache their late answer", async () => {
  const old = loadPlaylistRecommendations("shared", "snapshot");
  const rejected = old.then(() => ({ rejected: false }), (error) => ({ rejected: true, error }));
  applyPlayback({ ready: true, auth_state: "ready", username: "recommendation-new", playing: false });
  const current = loadPlaylistRecommendations("shared", "snapshot");
  expect(requests).toHaveLength(2);
  requests[1].resolve({ tracks: [track("new-account")] });
  const tracks = await current;
  requests[0].resolve({ tracks: [track("old-account")] });
  const outcome = await rejected;
  expect(outcome.rejected).toBe(true);
  expect(outcome.error).toBeInstanceOf(Error);
  expect(await loadPlaylistRecommendations("shared", "snapshot")).toBe(tracks);
  expect(tracks[0].id).toBe("new-account");
});

test("account switch also drops already-resolved recommendation and credits answers", async () => {
  const first = loadPlaylistRecommendations("shared-resolved", "snapshot");
  requests[0].resolve({ tracks: [track("account-a")] });
  await first;
  loadTrackCredits(track("credits-account"));
  requests[1].resolve({ groups: [], id: "account-a" });
  await settle();
  applyPlayback({ ready: true, auth_state: "ready", username: "resolved-new", playing: false });
  const second = loadPlaylistRecommendations("shared-resolved", "snapshot");
  openCredits(track("credits-account"));
  expect(requests).toHaveLength(4);
  requests[2].resolve({ tracks: [track("account-b")] });
  requests[3].resolve({ groups: [], id: "account-b" });
  expect((await second)[0].id).toBe("account-b");
  await settle();
  expect(credits.data.id).toBe("account-b");
});

test("failed old-account collection mutations cannot restore or insert rows into the new library", async () => {
  setLibrary([{ id: "old", name: "Old" }]);
  const removal = removeFromLibrary("old");
  const addition = addToLibrary({ id: "old-added", name: "Old added", tracks: [track("not-summary")] });
  applyPlayback({ ready: true, auth_state: "ready", username: "mutation-new", playing: false });
  setLibrary([{ id: "new", name: "New" }]);
  requests[0].reject(new Error("old removal failed"));
  requests[1].resolve({ id: "old-added", name: "Old added" });
  expect(await removal).toBe(false);
  expect(await addition).toBe(false);
  expect(library.map((row) => row.id)).toEqual(["new"]);
});

test("queued optimistic folder edits use only settled rollback baselines", async () => {
  setLibrary([], { tree: [folder("Original")] });
  const first = renameLibraryFolder("box", "First");
  const second = renameLibraryFolder("box", "Second");
  expect(libraryState.tree[0].name).toBe("First");
  expect(requests).toHaveLength(1);
  requests[0].reject(new Error("first failed"));
  expect(await first).toBe(false);
  await settle();
  expect(libraryState.tree[0].name).toBe("Second");
  requests[1].reject(new Error("second failed"));
  expect(await second).toBe(false);
  expect(libraryState.tree[0].name).toBe("Original");
  expect(await moveLibraryItem({ kind: "folder", id: "box" }, "box")).toBe(false);
});

test("queued old-account folder edits never issue writes for the new account", async () => {
  setLibrary([], { tree: [folder("Original")] });
  const first = renameLibraryFolder("box", "First");
  const queued = renameLibraryFolder("box", "Second");
  applyPlayback({ ready: true, auth_state: "ready", username: "folder-new", playing: false });
  setLibrary([], { tree: [folder("New account")] });
  requests[0].resolve(null);
  expect(await first).toBe(false);
  expect(await queued).toBe(false);
  expect(requests).toHaveLength(1);
  expect(libraryState.tree[0].name).toBe("New account");
});

test("hiding membership state fences a pending lookup without issuing another read", async () => {
  const lookup = lookupSavedIn("spotify:track:saved");
  invalidateSavedIn();
  requests[0].resolve([{ id: "liked", name: "Liked Songs" }]);
  await lookup;
  expect(nowSaved.refs).toEqual([]);
  expect(requests).toHaveLength(1);
});

test("an old-account artist follow completion cannot patch the new account", async () => {
  const { followed } = await import("./state.svelte.js");
  followed.loaded = true;
  const pending = setFollowingArtist({ id: "old-artist", name: "Old artist" }, true);
  const rejected = pending.then(() => ({ rejected: false }), (error) => ({ rejected: true, error }));
  applyPlayback({ ready: true, auth_state: "ready", username: "follow-new", playing: false });
  followed.loaded = true;
  followed.artists = [{ id: "new-artist", name: "New artist" }];
  requests[0].resolve(null);
  const outcome = await rejected;
  expect(outcome.rejected).toBe(true);
  expect(outcome.error).toBeInstanceOf(Error);
  expect(followed.artists.map((artist) => artist.id)).toEqual(["new-artist"]);
});

test("installing a catalogue source backfills a tail whose state event preceded the command reply", async () => {
  playback.shuffle = false;
  const first = track("first-tail");
  const pending = playCatalogueContext([{ id: "first-release", tracks: [first] }], "deferred-artist", ["albums"], 4, 0);
  applyPlayback({ queue: [first], queue_revision: 0, current_index: 0, current_uri: first.uri, playing: false });
  await settle();
  expect(requests.map((request) => request.command)).toEqual(["play_queue"]);
  requests[0].resolve(null);
  await pending;
  await settle();
  expect(requests[1].command).toBe("browse_artist_catalogue");
  expect(requests[1].args.offset).toBe(4);
  requests[1].resolve({ releases: [{ id: "next-release", tracks: [track("next-page")] }], next_offset: null });
  await settle();
  expect(requests[2].command).toBe("add_queue_batch");
  expect(requests[2].args.tracks[0].id).toBe("next-page");
  requests[2].resolve(null);
  await settle();
  expect(playback.current_index).toBe(0);
});

test("a superseded catalogue setup cannot fetch or append its old tail", async () => {
  playback.shuffle = false;
  const first = track("superseded-tail");
  const pending = playCatalogueContext([{ id: "superseded-release", tracks: [first] }], "superseded-artist", ["albums"], 4, 0);
  applyPlayback({ queue: [first], queue_revision: 0, current_index: 0, current_uri: first.uri, playing: false });
  const replacement = api.playQueue([track("replacement")], 0, "replacement");
  requests[1].resolve(null);
  await replacement;
  requests[0].resolve(null);
  await pending;
  await settle();
  expect(requests.map((request) => request.command)).toEqual(["play_queue", "play_queue"]);
});

test("clearing the panel track fences its pending credits without losing the reusable answer", async () => {
  loadTrackCredits(track("cleared-credits"));
  loadTrackCredits(null);
  requests[0].resolve({ groups: [], id: "cleared-credits" });
  await settle();
  expect(trackCredits.id).toBeNull();
  expect(trackCredits.data).toBeNull();
  openCredits(track("cleared-credits"));
  expect(credits.data.id).toBe("cleared-credits");
  expect(requests).toHaveLength(1);
});

test("a missing identity during disconnect does not hide a later real account switch", async () => {
  setLibrary([{ id: "account-a-private", name: "A private" }], { fresh: true });
  applyPlayback({ auth_state: "disconnected", username: null, playing: false });
  expect(library[0].id).toBe("account-a-private");
  applyPlayback({ ready: true, auth_state: "ready", username: "reconnect-account-b", playing: false });
  expect(library).toHaveLength(0);
  expect(libraryState.loaded).toBe(false);
  expect(route.name).toBe("library");
});
