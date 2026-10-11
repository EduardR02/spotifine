import { beforeEach, expect, test } from "bun:test";

globalThis.$state = Object.assign((value) => value, { raw: (value) => value });
const calls = [];
let answer = () => Promise.resolve(null);
globalThis.window = {
  __TAURI_INTERNALS__: {
    invoke: (command, args) => {
      calls.push([command, args]);
      return answer(command, args);
    },
  },
};
const {
  library, libraryState, setLibrary, ui,
  createLibraryFolder, renameLibraryFolder, deleteLibraryFolder, moveLibraryItem, removeFromLibrary,
} = await import("./state.svelte.js");

const playlist = (id, last_activity = null) => ({ id, name: id.toUpperCase(), last_activity, last_played: null });
const folder = (id, children = []) => ({ kind: "folder", id, name: id.toUpperCase(), children });
const leaf = (id) => ({ kind: "playlist", id });

beforeEach(() => {
  calls.length = 0;
  answer = () => Promise.resolve(null);
  ui.error = "";
  setLibrary([playlist("a", 300), playlist("b", 200), playlist("c", 100)], {
    tree: [leaf("a"), folder("box", [leaf("b")])],
  });
});

test("a move shows at once and asks the shell for exactly that rootlist edit", async () => {
  let land;
  answer = () => new Promise((resolve) => (land = resolve));
  const pending = moveLibraryItem({ kind: "playlist", id: "a" }, "box");
  expect(libraryState.tree).toEqual([folder("box", [leaf("a"), leaf("b")])]);
  expect(calls).toEqual([["move_library_item", { item: { kind: "playlist", id: "a" }, folder: "box" }]]);
  land(null);
  expect(await pending).toBe(true);
  expect(libraryState.tree).toEqual([folder("box", [leaf("a"), leaf("b")])]);
});

test("a move that fails puts the tree back and says so; a move to where it is writes nothing", async () => {
  answer = () => Promise.reject(new Error("offline"));
  expect(await moveLibraryItem({ kind: "playlist", id: "b" }, null)).toBe(false);
  expect(libraryState.tree).toEqual([leaf("a"), folder("box", [leaf("b")])]);
  expect(ui.error).toContain("offline");
  calls.length = 0;
  expect(await moveLibraryItem({ kind: "playlist", id: "b" }, "box")).toBe(false);
  expect(calls).toEqual([]);
});

test("a failed edit does not clobber a library answer that landed meanwhile", async () => {
  let fail;
  answer = () => new Promise((_, reject) => (fail = reject));
  const pending = renameLibraryFolder("box", "Crate");
  expect(libraryState.tree[1].name).toBe("Crate");
  setLibrary(library, { tree: [folder("box", [leaf("b"), leaf("a")])] });
  fail(new Error("conflict"));
  expect(await pending).toBe(false);
  expect(libraryState.tree).toEqual([folder("box", [leaf("b"), leaf("a")])]);
});

test("create waits for the account's id; delete takes the folder's playlists with it", async () => {
  answer = (command) => Promise.resolve(command === "create_folder"
    ? { kind: "folder", id: "0123456789abcdef", name: "Gym", children: [], last_activity: 1 }
    : null);
  await createLibraryFolder("  Gym ", "box");
  expect(calls[0]).toEqual(["create_folder", { name: "Gym", parent: "box" }]);
  expect(libraryState.tree[1].children[0].id).toBe("0123456789abcdef");

  await deleteLibraryFolder("box");
  expect(calls[1]).toEqual(["delete_folder", { id: "box" }]);
  expect(libraryState.tree).toEqual([leaf("a")]);
  expect(library.map((row) => row.id)).toEqual(["a", "c"]);
});

test("removing a playlist from the library is instant and is undone if the write fails", async () => {
  answer = () => Promise.reject(new Error("nope"));
  const pending = removeFromLibrary("b");
  expect(library.map((row) => row.id)).toEqual(["a", "c"]);
  expect(libraryState.tree).toEqual([leaf("a"), folder("box")]);
  expect(await pending).toBe(false);
  expect(calls).toEqual([["delete_playlist", { id: "b" }]]);
  expect(library.map((row) => row.id)).toEqual(["a", "b", "c"]);
  expect(libraryState.tree).toEqual([leaf("a"), folder("box", [leaf("b")])]);
});
