/* =====================================================================
   FIND — narrowing a list of songs that is already on the page.

   A query is folded (case and accents off) and split into words; a song
   matches when every word is somewhere in its name or its artists' names,
   in any order — "gent touch" finds "Nothing Can Touch Us" by Just A Gent.
   A song's folded text is built once, the first time it is looked at, and
   kept with the song, so a keystroke over a playlist of thousands compares
   strings and builds none.
   ===================================================================== */

/** Text as it is matched: accents off, lower case. */
export function fold(text) {
  return String(text ?? "").normalize("NFD").replace(/\p{M}+/gu, "").toLocaleLowerCase();
}

/** The words of a query, folded. Empty for a blank query. */
export function findTerms(query) {
  return fold(query).split(/\s+/).filter(Boolean);
}

const haystacks = new WeakMap();

function haystack(track) {
  let text = haystacks.get(track);
  if (text === undefined) {
    text = fold(`${track.name ?? ""}\n${(track.artist_names ?? []).join("\n")}`);
    haystacks.set(track, text);
  }
  return text;
}

/**
 * The songs of `tracks` that match every term, with where each one is in
 * `tracks` (a row found plays from its own place in the whole list).
 * @returns {{ rows: object[], at: number[] }}
 */
export function findTracks(tracks, terms) {
  const rows = [], at = [];
  for (let i = 0; i < tracks.length; i++) {
    const text = haystack(tracks[i]);
    if (terms.every((term) => text.includes(term))) {
      rows.push(tracks[i]);
      at.push(i);
    }
  }
  return { rows, at };
}
