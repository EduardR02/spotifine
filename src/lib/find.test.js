import { expect, test } from "bun:test";
import { findTerms, findTracks } from "./find.js";

const tracks = [
  { name: "Nothing Can Touch Us", artist_names: ["Just A Gent", "Jemma Cher"] },
  { name: "Lying & Loving", artist_names: ["Electric Mantis", "MOTHICA"] },
  { name: "Déjà Vu", artist_names: ["Beyoncé"] },
  { name: "Touch", artist_names: ["Daft Punk"] },
  { name: "Untitled" },
];
const names = (query) => findTracks(tracks, findTerms(query)).rows.map((t) => t.name);

test("a query matches the song's name or any of its artists, in any case", () => {
  expect(names("touch")).toEqual(["Nothing Can Touch Us", "Touch"]);
  expect(names("MOTHICA")).toEqual(["Lying & Loving"]);
  expect(names("jemma")).toEqual(["Nothing Can Touch Us"]);
});

test("every word has to match, in any order and across name and artists", () => {
  expect(names("gent touch")).toEqual(["Nothing Can Touch Us"]);
  expect(names("touch punk")).toEqual(["Touch"]);
  expect(names("touch mothica")).toEqual([]);
});

test("accents do not matter either way", () => {
  expect(names("deja beyonce")).toEqual(["Déjà Vu"]);
  expect(names("DÉJÀ")).toEqual(["Déjà Vu"]);
});

test("a blank query has no terms, and a found row knows its place in the list", () => {
  expect(findTerms("   ")).toEqual([]);
  expect(findTracks(tracks, findTerms("touch")).at).toEqual([0, 3]);
  expect(names("untitled")).toEqual(["Untitled"]);
});
