import { expect, test } from "bun:test";
import { mergeLikedRows, mergeLikedPage, applyLikedDelta } from "./liked-songs.js";

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

test("an unlike of a song that is not loaded leaves the list as it is", () => {
  const collection = collectionOf([row("one")]);
  const rows = collection.tracks;
  applyLikedDelta(collection, [row("elsewhere").uri], false);
  expect(collection.tracks).toBe(rows);
});

test("re-liking a removed song restores it once and allows later cursor pages to omit its duplicate", () => {
  const collection = collectionOf([row("one")], "opaque/next", 1);
  applyLikedDelta(collection, [row("one").uri], false);
  applyLikedDelta(collection, [row("one").uri], true, [row("one")]);
  mergeLikedPage(collection, { tracks: [row("one"), row("two")], next_cursor: null });
  expect(collection.tracks.map((track) => track.id)).toEqual(["one", "two"]);
});
