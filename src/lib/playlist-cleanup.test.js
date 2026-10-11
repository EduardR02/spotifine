import { expect, test } from "bun:test";
import {
  addedCutoff, cleanupChoices, cleanupDuration, cleanupMatches, cleanupSelection,
  filterCleanupChoices, cleanupPreview, parseLocalDate,
} from "./playlist-cleanup.js";

const tracks = [
  { uri: "spotify:track:a", name: "Live session", artist_ids: ["a"], artist_names: ["Artist A"], duration_ms: 180000 },
  { uri: "spotify:track:b", name: "Alive", artist_ids: ["b"], artist_names: ["Artist B"], duration_ms: 210000 },
  { uri: "spotify:track:a", name: "Alternate details", artist_ids: ["b"], artist_names: ["Artist B"], duration_ms: 240000 },
  { uri: "spotify:track:c", name: "Night drive", artist_ids: ["a"], artist_names: ["Artist A"], duration_ms: 300000 },
];

test("preview includes every copy the URI removal will delete, including nonmatching duplicates", () => {
  const artist = { field: "artist", choice: cleanupChoices(tracks, "artist").find((choice) => choice.id === "a") };
  const shorter = { field: "duration", operator: "shorter", duration: cleanupDuration("3:30") };
  const preview = cleanupPreview(tracks, [artist, shorter], "all");
  expect(preview.rows.map(({ index, extra }) => ({ index, extra }))).toEqual([
    { index: 0, extra: false }, { index: 2, extra: true },
  ]);
  expect(preview.uris).toEqual(["spotify:track:a"]);
  expect(preview.extraCount).toBe(1);
  expect(cleanupPreview(tracks, [artist, shorter], "any").rows.map(({ index }) => index)).toEqual([0, 2, 3]);
});

test("duration comparisons exclude exact boundaries and unknown lengths", () => {
  const rows = [...tracks, { uri: "spotify:track:unknown", duration_ms: 0 }];
  const rule = { field: "duration", operator: "longer", duration: cleanupDuration("3:30") };
  expect(cleanupPreview(rows, [rule], "all").rows.map(({ index }) => index)).toEqual([0, 2, 3]);
  // Row zero is included only because row two selects its URI.
  expect(cleanupPreview(rows, [{ ...rule, operator: "shorter" }], "all").rows.map(({ index }) => index)).toEqual([0, 2]);
});

test("whole-word filtering is Unicode-aware and never interprets user text as regex", () => {
  const rows = ["Live", "Alive", "LIVE session", "élive", "live_edition", "Live (2020)", "Other"].map((name, index) => ({ name, uri: `spotify:track:${index}` }));
  const rule = { field: "song", text: "live", operator: "words" };
  expect(cleanupPreview(rows, [rule], "all").rows.map(({ index }) => index)).toEqual([0, 2, 5]);
  expect(cleanupPreview(rows, [{ ...rule, operator: "contains" }], "all").rows.map(({ index }) => index)).toEqual([0, 1, 2, 3, 4, 5]);
  expect(cleanupPreview(rows, [{ ...rule, text: "(2020)", operator: "contains" }], "all").rows.map(({ index }) => index)).toEqual([5]);
  expect(cleanupPreview(rows, [{ ...rule, text: ".*" }], "all").rows).toEqual([]);
});

test("choice search matches Unicode-normalized names without treating accents as interchangeable", () => {
  const rows = [
    { uri: "spotify:track:f", name: "Song", artist_names: ["ＡＣ／ＤＣ"], artist_ids: [""], duration_ms: 200000 },
    { uri: "spotify:track:g", name: "Other", artist_names: ["Beyoncé"], artist_ids: ["bey"], duration_ms: 200000 },
  ];
  const choices = cleanupChoices(rows, "artist");
  expect(filterCleanupChoices(choices, "AC/DC").map(({ name }) => name)).toEqual(["ＡＣ／ＤＣ"]);
  expect(filterCleanupChoices(choices, "BEYONCÉ").map(({ id }) => id)).toEqual(["bey"]);
  expect(filterCleanupChoices(choices, "BEYONCE")).toEqual([]);
  expect(filterCleanupChoices(choices, "  ")).toHaveLength(2);
  expect(filterCleanupChoices(choices, "no such artist")).toEqual([]);
});

test("a namesake without an id matches on the folded name", () => {
  const rows = [
    { uri: "spotify:track:x", name: "One", artist_names: ["JAY-Z"], artist_ids: [""], duration_ms: 2000 },
    { uri: "spotify:track:y", name: "Two", artist_names: ["jay-z"], artist_ids: [""], duration_ms: 3000 },
    { uri: "spotify:track:z", name: "Three", artist_names: ["Jay Z"], artist_ids: [""], duration_ms: 4000 },
  ];
  const choice = cleanupChoices(rows, "artist").find((item) => item.normalized === "jay-z");
  // The two spellings of one artist are one choice, and the rule reaches both
  // rows by name because neither carries an id to be trusted instead.
  expect(choice.count).toBe(2);
  expect(cleanupPreview(rows, [{ field: "artist", choice }], "all").rows.map(({ index }) => index)).toEqual([0, 1]);
});

test("artist and album rules read backwards as is-not, on the one predicate", () => {
  const rows = [
    { uri: "spotify:track:a", name: "One", artist_names: ["Artist A"], artist_ids: ["a1"], album_name: "Album A", album_id: "al1" },
    { uri: "spotify:track:b", name: "Two", artist_names: ["Artist B"], artist_ids: ["b1"], album_name: "Album A", album_id: "al1" },
    { uri: "spotify:track:c", name: "Three", artist_names: ["Artist A"], artist_ids: ["a1"], album_name: "Album B", album_id: "al2" },
  ];
  const artist = cleanupChoices(rows, "artist").find((choice) => choice.id === "a1");
  const album = cleanupChoices(rows, "album").find((choice) => choice.id === "al1");
  const indices = (rules) => cleanupPreview(rows, rules, "all").rows.map(({ index }) => index);
  expect(indices([{ field: "artist", operator: "is", choice: artist }])).toEqual([0, 2]);
  expect(indices([{ field: "artist", operator: "is-not", choice: artist }])).toEqual([1]);
  expect(indices([{ field: "album", operator: "is-not", choice: album }])).toEqual([2]);
  // A negation is an ordinary predicate, not an exclusion: under AND it is
  // only one of the conditions a song has to meet.
  expect(indices([
    { field: "artist", operator: "is-not", choice: artist },
    { field: "album", operator: "is", choice: album },
  ])).toEqual([1]);
  expect(indices([
    { field: "artist", operator: "is-not", choice: artist },
    { field: "album", operator: "is-not", choice: album },
  ])).toEqual([]);
});

test("title rules negate in both readings, literal text included", () => {
  const rows = ["Live", "Alive", "Live (2020)", "Other"].map((name, index) => ({ name, uri: `spotify:track:${index}` }));
  const indices = (rule) => cleanupPreview(rows, [rule], "all").rows.map(({ index }) => index);
  expect(indices({ field: "song", text: "live", operator: "not-contains" })).toEqual([3]);
  // "Alive" holds the letters but not the word, so it survives only the
  // whole-word negation — the two negations are two different questions.
  expect(indices({ field: "song", text: "live", operator: "not-words" })).toEqual([1, 3]);
  expect(indices({ field: "song", text: ".*", operator: "not-contains" })).toEqual([0, 1, 2, 3]);
});

test("no longer playable matches the flag and nothing else", () => {
  const rows = [
    { uri: "spotify:track:a", name: "One", unavailable: true },
    { uri: "spotify:track:b", name: "Two", unavailable: false },
    { uri: "spotify:track:c", name: "Three" },
  ];
  const preview = cleanupPreview(rows, [{ field: "unavailable" }], "all");
  expect(preview.rows.map(({ index }) => index)).toEqual([0]);
  expect(preview.uris).toEqual(["spotify:track:a"]);
  expect(preview.undatedCount).toBe(0);
});

test("added rules compare strictly against the date's local midnight", () => {
  const cutoff = parseLocalDate("2024-03-01");
  // Local midnight, not the UTC midnight `new Date("2024-03-01")` would build:
  // read either way, a rule fires on the wrong day for half the world.
  const midnight = new Date(cutoff);
  expect([midnight.getFullYear(), midnight.getMonth(), midnight.getDate(), midnight.getHours()]).toEqual([2024, 2, 1, 0]);
  expect(parseLocalDate("2024-02-31")).toBeNull();
  expect(parseLocalDate("2024-13-01")).toBeNull();
  expect(parseLocalDate("soon")).toBeNull();
  expect(parseLocalDate(undefined)).toBeNull();

  const rows = [
    { uri: "spotify:track:a", name: "A minute earlier", added_at: cutoff - 1 },
    { uri: "spotify:track:b", name: "Exactly then", added_at: cutoff },
    { uri: "spotify:track:c", name: "A minute later", added_at: cutoff + 1 },
    { uri: "spotify:track:d", name: "No date at all", added_at: 0 },
  ];
  const before = cleanupPreview(rows, [{ field: "added-before", date: cutoff }], "all");
  const after = cleanupPreview(rows, [{ field: "added-after", date: cutoff }], "all");
  // Strict both ways: the entry added at that exact instant is on neither side.
  expect(before.rows.map(({ index }) => index)).toEqual([0]);
  expect(after.rows.map(({ index }) => index)).toEqual([2]);
  expect(before.undatedCount).toBe(1);
  expect(after.undatedCount).toBe(1);
});

test("older-than counts calendar units, never fixed-length ones", () => {
  // 31 March minus one month is the last day of February. A 30-day
  // approximation lands on 1 March and a `setMonth` rollover on 3 March: both
  // would take songs added on the 29th that the rule never named.
  const month = new Date(addedCutoff(1, "months", new Date(2023, 2, 31, 12).getTime()));
  expect([month.getMonth(), month.getDate(), month.getHours()]).toEqual([1, 28, 12]);
  // A leap day minus a year is the 28th, not 1 March.
  const year = new Date(addedCutoff(1, "years", new Date(2024, 1, 29, 9).getTime()));
  expect([year.getFullYear(), year.getMonth(), year.getDate(), year.getHours()]).toEqual([2023, 1, 28, 9]);
  // Days and weeks are local calendar days, so the time of day is kept.
  const weeks = new Date(addedCutoff(2, "weeks", new Date(2024, 0, 20, 8, 30).getTime()));
  expect([weeks.getMonth(), weeks.getDate(), weeks.getHours(), weeks.getMinutes()]).toEqual([0, 6, 8, 30]);
  const days = new Date(addedCutoff(3, "days", new Date(2024, 0, 20, 8, 30).getTime()));
  expect([days.getMonth(), days.getDate(), days.getHours(), days.getMinutes()]).toEqual([0, 17, 8, 30]);

  const before = addedCutoff(3, "days", new Date(2024, 4, 10).getTime());
  const rows = [
    { uri: "spotify:track:a", name: "Older", added_at: before - 1 },
    { uri: "spotify:track:b", name: "Exactly", added_at: before },
    { uri: "spotify:track:c", name: "Newer", added_at: before + 1 },
    { uri: "spotify:track:d", name: "No date" },
  ];
  const preview = cleanupPreview(rows, [{ field: "older", amount: 3, unit: "days", before }], "all");
  expect(preview.rows.map(({ index }) => index)).toEqual([0]);
  expect(preview.undatedCount).toBe(1);
});

test("a row with no added date is disclosed only when the date is what excluded it", () => {
  const cutoff = parseLocalDate("2024-03-01");
  const rows = [
    { uri: "spotify:track:a", name: "One", artist_names: ["Artist A"], artist_ids: ["a1"], added_at: 0 },
    { uri: "spotify:track:b", name: "Two", artist_names: ["Artist B"], artist_ids: ["b1"], added_at: 0 },
    { uri: "spotify:track:c", name: "Three", artist_names: ["Artist A"], artist_ids: ["a1"], added_at: cutoff + 1 },
  ];
  const artist = { field: "artist", choice: cleanupChoices(rows, "artist").find((choice) => choice.id === "a1") };
  const rules = [{ field: "added-after", date: cutoff }, artist];
  // AND: the undated row its other rule would have taken is the one the date
  // skipped. The second undated row was never a candidate either way.
  expect(cleanupPreview(rows, rules, "all")).toMatchObject({
    undatedCount: 1, uris: ["spotify:track:c"], removalCount: 1,
  });
  // OR: the artist rule takes the first undated row directly, so the only row
  // the date alone excluded is the other one — same count, a different row.
  expect(cleanupPreview(rows, rules, "any")).toMatchObject({
    undatedCount: 1, uris: ["spotify:track:a", "spotify:track:c"], removalCount: 2,
  });
  // A row the date rule can read is decided by it, so nothing is skipped.
  expect(cleanupPreview(rows.slice(2), rules, "all").undatedCount).toBe(0);
});

test("unticking one occurrence keeps the whole URI, and every copy with it", () => {
  // Two rows of one song, as a playlist really holds them: one matching, one
  // with different credits. Removal is by URI, so both go or neither does.
  const rows = [
    { uri: "spotify:track:a", name: "Live session", artist_ids: ["a1"], artist_names: ["Artist A"] },
    { uri: "spotify:track:b", name: "Night drive", artist_ids: ["b1"], artist_names: ["Artist B"] },
    { uri: "spotify:track:a", name: "Remaster", artist_ids: ["b1"], artist_names: ["Artist B"] },
  ];
  const rule = { field: "song", text: "live", operator: "contains" };
  const all = cleanupPreview(rows, [rule], "all");
  expect(all.rows.map(({ index, extra, kept }) => ({ index, extra, kept }))).toEqual([
    { index: 0, extra: false, kept: false },
    { index: 2, extra: true, kept: false },
  ]);
  expect(all.uris).toEqual(["spotify:track:a"]);
  expect(all.removalCount).toBe(2);
  expect(all.duplicateCount).toBe(1);
  expect(all.extraCount).toBe(1);

  const keptOne = cleanupPreview(rows, [rule], "all", new Set(["spotify:track:a"]));
  expect(keptOne.rows.map(({ index, kept }) => ({ index, kept }))).toEqual([
    { index: 0, kept: true }, { index: 2, kept: true },
  ]);
  expect(keptOne.uris).toEqual([]);
  expect(keptOne.keptUris).toEqual(["spotify:track:a"]);
  expect(keptOne.removalCount).toBe(0);
  expect(keptOne.duplicateCount).toBe(0);
  expect(keptOne.extraCount).toBe(0);

  // What stays still counts, and the disclosure counts only what is going.
  const rowsWithThird = [...rows, { uri: "spotify:track:c", name: "Live at home", artist_ids: ["c1"], artist_names: ["Artist C"] }];
  const mixed = cleanupPreview(rowsWithThird, [rule], "all", new Set(["spotify:track:a"]));
  expect(mixed.uris).toEqual(["spotify:track:c"]);
  expect(mixed.removalCount).toBe(1);
  expect(mixed.duplicateCount).toBe(0);
  expect(mixed.rows.map(({ index, kept }) => ({ index, kept }))).toEqual([
    { index: 0, kept: true }, { index: 2, kept: true }, { index: 3, kept: false },
  ]);

  // A mark outlives only the candidates: out of the rule's reach, it is not
  // reported, which is what lets the dialog forget it for good.
  const other = [{ field: "song", text: "night", operator: "contains" }];
  expect(cleanupPreview(rows, other, "all", new Set(["spotify:track:a"])).keptUris).toEqual([]);
  const nothing = new Set(["spotify:track:a"]);
  expect(cleanupPreview(rows, [], "all", nothing)).toEqual({
    rows: [], uris: [], keptUris: [], removalCount: 0, duplicateCount: 0,
    extraCount: 0, missingCount: 0, undatedCount: 0,
  });
});

test("one match run serves every selection, and no selection can change it", () => {
  // The dialog keeps a run open while the reader ticks rows. That is the whole
  // point of the split — a tick costs a pass over the entries instead of a
  // re-run of every rule over every entry — and it also means a run must not
  // carry a mark from the tick before it into the next selection.
  const run = cleanupMatches(tracks, [{ field: "song", text: "live", operator: "contains" }], "all");
  const untouched = cleanupSelection(run, new Set());
  expect(untouched.removalCount).toBe(3);
  const unticked = cleanupSelection(run, new Set(["spotify:track:a"]));
  expect(unticked.removalCount).toBe(1);
  expect(unticked.keptUris).toEqual(["spotify:track:a"]);
  // Read, never written: the same run answers the first selection again, and
  // the one-shot call agrees with both.
  expect(cleanupSelection(run, new Set())).toEqual(untouched);
  expect(cleanupPreview(tracks, [{ field: "song", text: "live", operator: "contains" }], "all")).toEqual(untouched);
});

