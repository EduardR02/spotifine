import { expect, test } from "bun:test";
import {
  findFolder, folderContents, folderTargets, insertFirst, moveItem, parentOf,
  removeFolder, removePlaylist, renameFolder,
} from "./library-tree.js";

const leaf = (id) => ({ kind: "playlist", id });
const folder = (id, children = []) => ({ kind: "folder", id, name: id.toUpperCase(), children });
const P = (id) => ({ kind: "playlist", id });
const F = (id) => ({ kind: "folder", id });

/** a, outer[b, inner[c]], empty */
const library = () => [leaf("a"), folder("outer", [leaf("b"), folder("inner", [leaf("c")])]), folder("empty")];

test("parents are named by folder id, and the top level is null", () => {
  const tree = library();
  expect(parentOf(tree, P("a"))).toBeNull();
  expect(parentOf(tree, P("b"))).toBe("outer");
  expect(parentOf(tree, P("c"))).toBe("inner");
  expect(parentOf(tree, F("inner"))).toBe("outer");
  expect(parentOf(tree, P("unnamed"))).toBeNull();
});

test("a move lands first in its folder and leaves the old tree untouched", () => {
  const tree = library();
  const before = JSON.stringify(tree);
  const moved = moveItem(tree, P("a"), "inner");
  expect(JSON.stringify(tree)).toBe(before);
  expect(moved).toEqual([folder("outer", [leaf("b"), folder("inner", [leaf("a"), leaf("c")])]), folder("empty")]);
  // The untouched folder is the same object: only the edited path is new.
  expect(moved[1]).toBe(tree[2]);
});

test("folders move with their contents, in and out, nested", () => {
  const tree = library();
  // Out of every folder: just after the top-level folder it was in, as the
  // rootlist MOV places it.
  expect(moveItem(tree, F("inner"), null)).toEqual([leaf("a"), folder("outer", [leaf("b")]), folder("inner", [leaf("c")]), folder("empty")]);
  expect(moveItem(tree, F("empty"), "inner")).toEqual([leaf("a"), folder("outer", [leaf("b"), folder("inner", [folder("empty"), leaf("c")])])]);
  expect(moveItem(tree, P("c"), null)).toEqual([leaf("a"), folder("outer", [leaf("b"), folder("inner")]), leaf("c"), folder("empty")]);
  // A playlist the tree never named was at the top level.
  expect(moveItem(tree, P("z"), "empty")[2]).toEqual(folder("empty", [leaf("z")]));
});

test("moves that change nothing or cannot exist answer null", () => {
  const tree = library();
  expect(moveItem(tree, P("a"), null)).toBeNull();
  expect(moveItem(tree, P("c"), "inner")).toBeNull();
  expect(moveItem(tree, F("outer"), "outer")).toBeNull();
  expect(moveItem(tree, F("outer"), "inner")).toBeNull();
  expect(moveItem(tree, P("a"), "missing")).toBeNull();
  expect(moveItem(tree, F("missing"), "empty")).toBeNull();
});

test("create, rename and delete edit the one folder they name", () => {
  const tree = library();
  const created = insertFirst(tree, "inner", { kind: "folder", id: "new", name: "New", children: [], last_activity: 9 });
  expect(findFolder(created, "inner").children[0].id).toBe("new");
  expect(insertFirst(tree, null, leaf("top"))[0]).toEqual(leaf("top"));
  expect(findFolder(renameFolder(tree, "inner", "Synthwave"), "inner").name).toBe("Synthwave");
  expect(findFolder(tree, "inner").name).toBe("INNER");

  const removed = removeFolder(tree, "outer");
  expect(removed.tree).toEqual([leaf("a"), folder("empty")]);
  expect(removed.playlistIds).toEqual(["b", "c"]);
  expect(removeFolder(tree, "missing")).toEqual({ tree, playlistIds: [] });
  expect(removePlaylist(tree, "c")).toEqual([leaf("a"), folder("outer", [leaf("b"), folder("inner")]), folder("empty")]);
  expect(folderContents(findFolder(tree, "outer"))).toEqual({ playlists: 2, folders: 1 });
});

test("move targets skip the current folder, and a folder's own subtree", () => {
  const tree = library();
  expect(folderTargets(tree, P("a"))).toEqual([
    { id: "outer", name: "OUTER", depth: 0 },
    { id: "inner", name: "INNER", depth: 1 },
    { id: "empty", name: "EMPTY", depth: 0 },
  ]);
  expect(folderTargets(tree, P("c")).map((target) => target.id)).toEqual(["outer", "empty"]);
  expect(folderTargets(tree, F("outer")).map((target) => target.id)).toEqual(["empty"]);
  expect(folderTargets(tree, F("inner")).map((target) => target.id)).toEqual(["empty"]);
});
