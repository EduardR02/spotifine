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
