import { expect, test } from "bun:test";
import { occurrenceIdentity, occurrenceKeys, occurrences } from "./occurrence-keys.js";

const song = (id, extra = {}) => ({ id, uri: `spotify:track:${id}`, ...extra });

test("rows keep their keys across snapshots, and duplicates are told apart and counted", () => {
  const a = song("a", { added_at: 1 });
  const b = song("b", { added_at: 2 });
  const keys = occurrenceKeys([a, b, { ...a }]);
  // A row is what it plays plus when it was added; the repeat is occurrence 1.
  expect(new Set(keys).size).toBe(3);
  expect(occurrenceIdentity(keys[0])).toBe(occurrenceIdentity(keys[2]));
  expect(keys[0]).not.toBe(keys[2]);

  // The same rows sent again, reordered, with another row gone: each keeps
  // its key, so the table moves the row instead of rebuilding it.
  const later = occurrenceKeys([{ ...b }, { ...a }]);
  expect(later[0]).toBe(keys[1]);
  expect(later[1]).toBe(keys[0]);

  // Where a key sits, and how many rows share its identity: a reader holding
  // on to a duplicate lets go when that count moves.
  const index = occurrences(keys);
  expect(index.indexOf(keys[2])).toBe(2);
  expect(index.indexOf("missing")).toBe(-1);
  expect(index.multiplicity(keys[0])).toBe(2);
  expect(index.multiplicity(keys[1])).toBe(1);
  expect(occurrences(later).multiplicity(keys[0])).toBe(1);
});

test("the same song queued from two places, or added twice, is two rows", () => {
  const fromPlaylist = song("same", { context: "playlist:one" });
  const fromAlbum = song("same", { context: "album:two" });
  const [first, second] = occurrenceKeys([fromPlaylist, fromAlbum]);
  expect(occurrenceIdentity(first)).not.toBe(occurrenceIdentity(second));
  const [early, late] = occurrenceKeys([song("again", { added_at: 1 }), song("again", { added_at: 2 })]);
  expect(occurrenceIdentity(early)).not.toBe(occurrenceIdentity(late));
  // A row with no URI (a local file, an episode stub) is keyed by its id.
  const [local] = occurrenceKeys([{ id: "local-1" }]);
  expect(local.startsWith("local-1")).toBe(true);
});
