import { expect, test } from "bun:test";
import { types } from "node:util";
import { mergeLikedRows, mergeLikedPage, applyLikedDelta, applyLikedChange } from "./liked-songs.js";

const row = (id, name = id) => ({ id, uri: `spotify:track:${id}`, name });
const collectionOf = (tracks = [], nextCursor = null, loadedPages = 0) =>
  ({ tracks, nextCursor, loadedPages, removed: new Set() });

test("next pages retain loaded rows and reject duplicates or rows unliked during the request", () => {
  const first = row("one");
  const rows = [first, row("two")];
  const merged = mergeLikedRows(rows, [row("two"), row("removed"), row("three"), row("three"), {}], new Set(["spotify:track:removed"]));
  // A new list for the raw store; the list on screen is never edited.
  expect(merged).not.toBe(rows);
  expect(rows.map((track) => track.id)).toEqual(["one", "two"]);
  expect(merged[0]).toBe(first);
  expect(merged.map((track) => track.id)).toEqual(["one", "two", "three"]);
});

test("a page with nothing new hands back the same list, so the table is not given a new one", () => {
  const rows = [row("one"), row("two")];
  expect(mergeLikedRows(rows, [row("two"), row("one")])).toBe(rows);
  expect(mergeLikedRows(rows, null)).toBe(rows);
});

test("new likes precede the loaded collection without dropping later pages", () => {
  const rows = [row("one"), row("two"), row("later")];
  const later = rows[2];
  const merged = mergeLikedRows(rows, [row("new"), row("one"), row("new")], new Set(), true);
  expect(merged.map((track) => track.id)).toEqual(["new", "one", "two", "later"]);
  expect(merged[3]).toBe(later);
});

test("likes and unlikes preserve loaded pages and the opaque continuation used by the next page", () => {
  const collection = collectionOf();
  mergeLikedPage(collection, { tracks: [row("one"), row("two")], next_cursor: "opaque/page-two" });
  mergeLikedPage(collection, { tracks: [row("three"), row("four")], next_cursor: "opaque/page-three" });
  const loaded = collection.tracks;
  const retained = loaded[3];
  applyLikedDelta(collection, [row("two").uri], false);
  const afterUnlike = collection.tracks;
  expect(afterUnlike).not.toBe(loaded);
  expect(loaded.map((track) => track.id)).toEqual(["one", "two", "three", "four"]);
  applyLikedDelta(collection, [row("new").uri], true, [row("new")]);
  expect(collection.nextCursor).toBe("opaque/page-three");
  expect(collection.loadedPages).toBe(2);
  expect(collection.tracks.map((track) => track.id)).toEqual(["new", "one", "three", "four"]);
  expect(collection.tracks[3]).toBe(retained);
  mergeLikedPage(collection, { tracks: [row("two"), row("four"), row("five")], next_cursor: null });
  expect(collection.tracks.map((track) => track.id)).toEqual(["new", "one", "three", "four", "five"]);
  expect(collection.nextCursor).toBeNull();
  expect(collection.loadedPages).toBe(3);
});

test("an unlike outside the loaded prefix leaves current rows intact and fences its later page", () => {
  const collection = collectionOf([row("one")]);
  const rows = collection.tracks;
  applyLikedDelta(collection, [row("elsewhere").uri], false);
  expect(collection.tracks).toBe(rows);
  mergeLikedPage(collection, { tracks: [row("elsewhere"), row("later")], next_cursor: null });
  expect(collection.tracks.map((track) => track.id)).toEqual(["one", "later"]);
});

test("re-liking a removed song restores it once and allows later cursor pages to omit its duplicate", () => {
  const collection = collectionOf([row("one")], "opaque/next", 1);
  applyLikedDelta(collection, [row("one").uri], false);
  applyLikedDelta(collection, [row("one").uri], true, [row("one")]);
  mergeLikedPage(collection, { tracks: [row("one"), row("two")], next_cursor: null });
  expect(collection.tracks.map((track) => track.id)).toEqual(["one", "two"]);
});

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

// The mounted view serializes saved events; the production consumer below
// owns their asynchronous reads and the collection commit, not this harness.
function savedEvents(collection, api, isCurrent = () => true) {
  let changes = Promise.resolve();
  return (change) => {
    changes = changes.then(() => applyLikedChange(collection, change, api, isCurrent));
    return changes;
  };
}

test("an authoritative return cancels its old unlike before another song is unliked", async () => {
  const a = row("a");
  const b = row("b");
  const collection = collectionOf([a, b], "opaque/next", 1);
  const emit = savedEvents(collection, {
    browseLikedSongs: async () => ({ tracks: [a, b], next_cursor: "opaque/refreshed" }),
  });
  await emit({ uris: [a.uri, row("unloaded").uri], saved: false });
  expect(collection.tracks).toEqual([b]);
  await emit(null);
  const refreshed = collection.tracks;
  expect(refreshed).toEqual([a, b]);
  expect(refreshed[0]).toBe(a);
  expect(collection.removed.has(a.uri)).toBe(false);
  expect(collection.removed.has(row("unloaded").uri)).toBe(true);

  await emit({ uris: [b.uri], saved: false });
  expect(collection.tracks).toEqual([a]);
  expect(refreshed).toEqual([a, b]);
  expect(collection.nextCursor).toBe("opaque/refreshed");
  expect(collection.loadedPages).toBe(1);
  mergeLikedPage(collection, {
    tracks: [a, b, row("unloaded"), row("later")],
    next_cursor: null,
  });
  expect(collection.tracks.map((track) => track.id)).toEqual(["a", "later"]);
});

test("an unlike delivered during a held refresh wins even when that URI had an older tombstone", async () => {
  const a = row("a");
  const b = row("b");
  const collection = collectionOf([a, b], "opaque/next", 1);
  const response = deferred();
  const started = deferred();
  const emit = savedEvents(collection, {
    browseLikedSongs(cursor) {
      expect(cursor).toBeNull();
      started.resolve();
      return response.promise;
    },
  });
  await emit({ uris: [a.uri], saved: false });
  const refresh = emit(null);
  await started.promise;
  const unlike = emit({ uris: [a.uri], saved: false });
  response.resolve({ tracks: [a, b], next_cursor: "opaque/refreshed" });
  await refresh;
  await unlike;
  expect(collection.tracks).toEqual([b]);
  expect(collection.removed.has(a.uri)).toBe(true);
  mergeLikedPage(collection, { tracks: [a, row("later")], next_cursor: null });
  expect(collection.tracks.map((track) => track.id)).toEqual(["b", "later"]);
});

test("a new like delivered during a refresh is not lost from the complete plain queue prefix", async () => {
  const a = row("a");
  const b = row("b");
  const newlySaved = row("new");
  const collection = collectionOf([a, b], "opaque/next", 1);
  const response = deferred();
  const started = deferred();
  const lookedUp = [];
  const emit = savedEvents(collection, {
    browseLikedSongs() {
      started.resolve();
      return response.promise;
    },
    async browseTrack(id) {
      lookedUp.push(id);
      return newlySaved;
    },
  });
  const refresh = emit(null);
  await started.promise;
  const like = emit({ uris: [newlySaved.uri], saved: true });
  response.resolve({ tracks: [a, b], next_cursor: "opaque/refreshed" });
  await refresh;
  await like;
  expect(lookedUp).toEqual(["new"]);
  expect(collection.tracks).toEqual([newlySaved, a, b]);
  expect(collection.tracks[0]).toBe(newlySaved);
  expect(collection.tracks[2]).toBe(b);
  expect(types.isProxy(collection.tracks)).toBe(false);
  expect(collection.tracks.some((track) => types.isProxy(track))).toBe(false);
  expect(collection.nextCursor).toBe("opaque/refreshed");
  expect(collection.loadedPages).toBe(1);
});

test("a refresh walks only the loaded prefix with the returned opaque cursors", async () => {
  const collection = collectionOf([row("old")], "opaque/old-next", 2);
  const cursors = [];
  const a = row("a");
  const b = row("b");
  await applyLikedChange(collection, null, {
    async browseLikedSongs(cursor) {
      cursors.push(cursor);
      return cursor === null
        ? { tracks: [a], next_cursor: "opaque/page-two?offset=not-a-number" }
        : { tracks: [a, b], next_cursor: "opaque/page-three" };
    },
  }, () => true);
  expect(cursors).toEqual([null, "opaque/page-two?offset=not-a-number"]);
  expect(collection.tracks).toEqual([a, b]);
  expect(collection.tracks[0]).toBe(a);
  expect(collection.tracks[1]).toBe(b);
  expect(collection.nextCursor).toBe("opaque/page-three");
  expect(collection.loadedPages).toBe(2);
});

test("a held refresh cannot mutate a collection that crossed its account generation", async () => {
  const collection = collectionOf([row("alice")], "opaque/alice", 1);
  const response = deferred();
  let generation = 0;
  const captured = generation;
  const refresh = applyLikedChange(collection, null, {
    browseLikedSongs: () => response.promise,
  }, () => generation === captured);
  generation++;
  const bobRows = [row("bob")];
  collection.tracks = bobRows;
  collection.nextCursor = "opaque/bob";
  collection.removed.add(row("alice").uri);
  response.resolve({ tracks: [row("alice")], next_cursor: "opaque/stale" });
  await refresh;
  expect(collection.tracks).toBe(bobRows);
  expect(collection.nextCursor).toBe("opaque/bob");
  expect(collection.removed.has(row("alice").uri)).toBe(true);

  let staleReads = 0;
  await applyLikedChange(collection, null, {
    browseLikedSongs() { staleReads++; },
  }, () => generation === captured);
  expect(staleReads).toBe(0);
});

test("an incomplete refresh keeps the displayed prefix, cursor and removals intact", async () => {
  const a = row("a");
  const collection = collectionOf([row("kept")], "opaque/current", 2);
  const loaded = collection.tracks;
  collection.removed.add(a.uri);
  const failure = new Error("second page failed");
  const refresh = applyLikedChange(collection, null, {
    async browseLikedSongs(cursor) {
      if (cursor === null) return { tracks: [a], next_cursor: "opaque/second" };
      throw failure;
    },
  }, () => true);
  await expect(refresh).rejects.toBe(failure);
  expect(collection.tracks).toBe(loaded);
  expect(collection.nextCursor).toBe("opaque/current");
  expect(collection.loadedPages).toBe(2);
  expect(collection.removed.has(a.uri)).toBe(true);
});
