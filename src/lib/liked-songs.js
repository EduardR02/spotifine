/*
 * Liked Songs as an immutable list. The view keeps `collection.tracks` in
 * `$state.raw` — a deep proxy over thousands of rows made every Play pay for
 * serializing through it — so nothing here edits the array it is handed: a
 * change assigns a new one, and every row that stays keeps its identity.
 */

/** A cursor page or newly saved rows, each merged in once: a new array, or
    `rows` itself when nothing in `incoming` is new. */
export function mergeLikedRows(rows, incoming, removed = new Set(), prepend = false) {
  const seen = new Set(rows.map((row) => row.uri));
  const additions = [];
  for (const row of incoming ?? []) {
    if (!row?.uri || removed.has(row.uri) || seen.has(row.uri)) continue;
    seen.add(row.uri);
    additions.push(row);
  }
  if (!additions.length) return rows;
  return prepend ? [...additions, ...rows] : [...rows, ...additions];
}

/** A landed page advances only its opaque cursor and the rows it adds. */
export function mergeLikedPage(collection, page) {
  collection.tracks = mergeLikedRows(collection.tracks, page?.tracks, collection.removed);
  collection.loadedPages++;
  collection.nextCursor = page?.next_cursor ?? null;
}

/** Local membership changes never invalidate loaded pages or their cursor. */
export function applyLikedDelta(collection, uris, saved, additions = []) {
  if (saved) {
    for (const uri of uris) collection.removed.delete(uri);
    collection.tracks = mergeLikedRows(collection.tracks, additions, collection.removed, true);
    return;
  }
  for (const uri of uris) collection.removed.add(uri);
  const kept = collection.tracks.filter((row) => !collection.removed.has(row.uri));
  if (kept.length !== collection.tracks.length) collection.tracks = kept;
}

/** Saved changes run in the view's serial queue, after any cursor page in flight.
    A refresh is authoritative for the returned prefix; later deltas run next. */
export async function applyLikedChange(collection, change, api, isCurrent) {
  if (!isCurrent()) return;
  const uris = change?.uris?.filter((uri) => uri.startsWith("spotify:track:"));
  if (uris && typeof change.saved === "boolean") {
    if (!change.saved) {
      applyLikedDelta(collection, uris, false);
      return;
    }
    const saved = [];
    for (const uri of uris) {
      if (collection.tracks.some((track) => track.uri === uri)) continue;
      saved.push(await api.browseTrack(uri.slice("spotify:track:".length)));
      if (!isCurrent()) return;
    }
    applyLikedDelta(collection, uris, true, saved);
    return;
  }

  // Unidentified external changes refresh only the pages already displayed.
  let fresh = [];
  let cursor = null;
  for (let page = 0; page < Math.max(1, collection.loadedPages); page++) {
    const result = await api.browseLikedSongs(cursor);
    if (!isCurrent()) return;
    fresh = mergeLikedRows(fresh, result?.tracks);
    cursor = result?.next_cursor ?? null;
    if (!cursor) break;
  }
  // A returned URI supersedes an older unlike. Absent URIs retain their
  // tombstones so later cursor pages cannot restore those removed songs.
  for (const track of fresh) collection.removed.delete(track.uri);
  collection.tracks = fresh;
  collection.nextCursor = cursor;
}
