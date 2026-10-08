import { expect, test } from "bun:test";

/* The hub is a runes module: outside Svelte `$state` is just the value it is
   handed, and what these tests are about is where a row sits in one array —
   ordering, not reactivity. */
globalThis.$state = Object.assign((value) => value, { raw: (value) => value });

/* `invoke` is the Tauri bridge, which no test has. Nothing here calls a
   command; the stub only keeps the module's import side from throwing. */
globalThis.window = {
  __TAURI_INTERNALS__: {
    invoke: () => new Promise(() => {}),
  },
};

const { library, insertPlaylist, setLibrary } = await import("./state.svelte.js");

function playlist(id, last_activity = null) {
  return {
    id,
    uri: `spotify:playlist:${id}`,
    name: `Playlist ${id}`,
    tracks_total: 0,
    last_activity,
    last_played: null,
  };
}

const idsOf = () => library.map((row) => row.id);

/* The whole point of the fix: the optimistic row and the answer that replaces
   it a moment later must put the playlist in the same place. A row that
   appears on top and then sinks is the bug wearing a different hat. */
test("a created playlist leads the library, and the refetch leaves it there", () => {
  setLibrary([playlist("busy", 500), playlist("never-used")]);
  expect(idsOf()).toEqual(["busy", "never-used"]);

  // The backend answers with the finished row, stamped as it installed it.
  insertPlaylist(playlist("new", 900));
  expect(idsOf()).toEqual(["new", "busy", "never-used"]);
  expect(library[0].name).toBe("Playlist new");

  /* The rootlist event lands: the backend carried that stamp forward, so the
     sort here reaches the same answer the optimistic insert did. */
  setLibrary([playlist("new", 900), playlist("busy", 500), playlist("never-used")], {
    fresh: true,
  });
  expect(idsOf()).toEqual(["new", "busy", "never-used"]);
});

test("an unstamped row is still placed first, so the rail never waits on the server", () => {
  setLibrary([playlist("busy", 500)]);
  insertPlaylist(playlist("new"));
  expect(idsOf()).toEqual(["new", "busy"]);
  // Stamped locally, so the next sort keeps it rather than sinking it to the
  // bottom with every playlist that has no activity at all.
  expect(library[0].last_activity).toBeGreaterThan(0);
});

test("a refresh that beat the answer home does not get a second row", () => {
  setLibrary([playlist("new", 900), playlist("busy", 500)]);
  insertPlaylist({ ...playlist("new", 900), name: "Road Trip", tracks_total: 3 });
  expect(idsOf()).toEqual(["new", "busy"]);
  // The known-good fields still land on the row that is already there.
  expect(library[0].name).toBe("Road Trip");
  expect(library[0].tracks_total).toBe(3);
});

test("a playlist with no id is not a row", () => {
  setLibrary([playlist("busy", 500)]);
  expect(insertPlaylist(undefined)).toBe(false);
  expect(insertPlaylist({ name: "nameless" })).toBe(false);
  expect(idsOf()).toEqual(["busy"]);
});
