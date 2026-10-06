import { expect, test } from "bun:test";

globalThis.$state = (value) => value;
globalThis.window = { __TAURI_INTERNALS__: { invoke: () => new Promise(() => {}) } };
const { library, setLibrary, promotePlaylist, libraryRailEntries } = await import("./state.svelte.js");

const playlist = (id, last_activity = null, name = id) => ({ id, name, last_activity, last_played: null });
const leaf = (row) => ({ kind: "playlist", id: row.id });
const folder = (id, children) => ({ kind: "folder", id, name: id, children });
const ids = (entries) => entries.map((entry) => entry.kind === "folder" ? `folder:${entry.id}` : entry.playlist.id);

test("rootlist positions do not override library activity or a successful play promotion", () => {
  const older = playlist("older", 100);
  const newer = playlist("newer", 500);
  const tree = [leaf(older), leaf(newer)];
  setLibrary([older, newer]);
  expect(ids(libraryRailEntries(library, tree))).toEqual(["newer", "older"]);
  promotePlaylist("older", { played: true });
  expect(ids(libraryRailEntries(library, tree))).toEqual(["older", "newer"]);
  expect(library[0].last_played).toBe(library[0].last_activity);
});

test("nested folders move with their most active child and keep the child once", () => {
  const quiet = playlist("quiet", 10);
  const recent = playlist("recent", 600);
  const outside = playlist("outside", 300);
  setLibrary([quiet, outside, recent]);
  const tree = [leaf(outside), folder("outer", [leaf(quiet), folder("inner", [leaf(recent)])]), leaf(recent)];
  const entries = libraryRailEntries(library, tree);
  expect(ids(entries)).toEqual(["folder:outer", "folder:inner", "recent", "quiet", "outside"]);
  expect(entries.map((entry) => entry.depth)).toEqual([0, 1, 2, 1, 0]);
});

test("explicit pins lift once and do not strand folder headings", () => {
  const pinned = playlist("pinned", 10);
  const active = playlist("active", 900);
  setLibrary([active, pinned]);
  const tree = [folder("outer", [folder("inner", [leaf(pinned)])]), leaf(active)];
  expect(ids(libraryRailEntries(library, tree, ["pinned"]))).toEqual(["pinned", "active"]);
  expect(library.map((row) => row.id)).toEqual(["active", "pinned"]);
});

test("filter matches are flat, ordered, and retain deliberate pin priority", () => {
  const quiet = playlist("quiet", 10, "Road quiet");
  const recent = playlist("recent", 600, "Road recent");
  const other = playlist("other", 900, "Unrelated");
  setLibrary([quiet, other, recent]);
  const tree = [folder("outer", [leaf(quiet), folder("inner", [leaf(recent)])]), leaf(other)];
  const entries = libraryRailEntries(library, tree, ["quiet"], " ROAD ");
  expect(ids(entries)).toEqual(["quiet", "recent"]);
  expect(entries.map((entry) => entry.depth)).toEqual([0, 0]);
  expect(libraryRailEntries(library, tree, [], "missing")).toEqual([]);
});

test("missing tree data retains sorted library rows and genuinely empty folders", () => {
  setLibrary([playlist("older", 100), playlist("newer", 500)]);
  expect(ids(libraryRailEntries(library, null))).toEqual(["newer", "older"]);
  expect(ids(libraryRailEntries(library, [folder("empty", [])]))).toEqual(["newer", "older", "folder:empty"]);
});

test("a collapsed folder keeps its heading and count and hides its rows, except to a filter", () => {
  const quiet = playlist("quiet", 10, "Road quiet");
  const recent = playlist("recent", 600, "Road recent");
  const outside = playlist("outside", 300, "Elsewhere");
  setLibrary([quiet, outside, recent]);
  const tree = [folder("outer", [leaf(quiet), folder("inner", [leaf(recent)])]), leaf(outside)];
  const shut = libraryRailEntries(library, tree, [], "", ["outer"]);
  expect(ids(shut)).toEqual(["folder:outer", "outside"]);
  expect(shut[0]).toMatchObject({ collapsed: true, count: 2 });
  const inner = libraryRailEntries(library, tree, [], "", ["inner"]);
  expect(ids(inner)).toEqual(["folder:outer", "folder:inner", "quiet", "outside"]);
  expect(inner[1]).toMatchObject({ collapsed: true, count: 1, depth: 1 });
  expect(ids(libraryRailEntries(library, tree, [], "road", ["outer"]))).toEqual(["recent", "quiet"]);
});

test("a folder this app made stands on its own stamp, and every entry names its folder", () => {
  const older = playlist("older", 100);
  const newer = playlist("newer", 500);
  setLibrary([older, newer]);
  const made = { kind: "folder", id: "made", name: "Made", children: [], last_activity: 900 };
  const tree = [leaf(older), made, folder("box", [leaf(newer)]), folder("empty", [])];
  const entries = libraryRailEntries(library, tree);
  expect(ids(entries)).toEqual(["folder:made", "folder:box", "newer", "older", "folder:empty"]);
  expect(entries.map((entry) => entry.parent)).toEqual([null, null, "box", null, null]);
  // A stamp older than what a folder holds does not hold it back.
  const stale = { ...made, last_activity: 50, children: [leaf(newer)] };
  expect(ids(libraryRailEntries(library, [stale, leaf(older)]))).toEqual(["folder:made", "newer", "older"]);
});
