/** Update download marks in place; null resets all rows, false removes IDs. */
export function applyCacheMarks(tracks, ids, cached = true) {
  for (const track of tracks ?? []) {
    if (ids === null) track.cached = false;
    else if (ids.has(track.id)) track.cached = cached;
  }
}
