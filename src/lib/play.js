import { api, promotePlaylist } from "./state.svelte.js";

/**
 * Playing a record you can only see the cover of.
 *
 * A card in a grid knows an id and a name and nothing else — the tracks live
 * behind a browse call. Every grid had the same problem and solved it
 * differently or, in the library's and search's case, not at all: their play
 * buttons were decorative `<span>`s that did nothing when clicked. One card
 * rule (the artwork opens, the button plays) needs one implementation of the
 * button, so it lives here rather than three times in three views.
 *
 * Both throw. The caller decides whether a failure is worth a message.
 */

export async function playAlbumById(id) {
  const album = await api.browseAlbum(id);
  const tracks = album?.tracks ?? [];
  if (!tracks.length) throw new Error("No playable tracks were returned for this release.");
  await api.playQueue(tracks, 0, `album:${id}`, { automaticStart: true });
}

/**
 * Liked Songs from a card, without opening the page.
 *
 * The collection is paginated and the page owns its own cursor, so this plays
 * the first page and stops there — deliberately. Anything else would mean
 * walking an unbounded collection before a single note came out, which is the
 * exact cost the lazy queue exists to avoid.
 */
export async function playLikedSongs() {
  const page = await api.browseLikedSongs(null);
  const tracks = page?.tracks ?? [];
  if (!tracks.length) throw new Error("No liked songs came back.");
  await api.playQueue(tracks, 0, "liked", { automaticStart: true });
}

export async function playPlaylistById(id) {
  const playlist = await api.browsePlaylist(id);
  const tracks = playlist?.tracks ?? [];
  if (!tracks.length) throw new Error("This playlist has no playable songs.");
  // Queue first: a failed play must not create either a Home listening-history
  // stamp or a library-activity promotion. A card is as much a play source as
  // the playlist page's big button.
  await api.playQueue(tracks, 0, `playlist:${id}`, { automaticStart: true });
  promotePlaylist(id, { played: true });
  api.touchPlaylist(id).catch(() => {});
}

/**
 * The one busy-guard behind every card's play button.
 *
 * A card play is two awaits (browse, then queue) during which the button must
 * look busy, further clicks must be refused, and exactly one failure message
 * must survive. Every grid view had its own copy of that choreography; this
 * is the single one.
 *
 * `busy` is the caller's own `$state({ id: "" })` slot: runes cannot live in a
 * plain `.js` module, so the view owns the reactive object and hands over the
 * proxy — mutations here stay reactive in the view's template. `load` is a
 * thunk so callers with arguments close over them. Returns the message to
 * display ("" on success or when `stillCurrent` says the caller has moved on),
 * so the view keeps one `error = await cardPlay(...)` line.
 */
export async function cardPlay(busy, id, load, failure, stillCurrent = null) {
  if (busy.id) return "";
  busy.id = id;
  try {
    await load();
    return "";
  } catch (reason) {
    if (stillCurrent && !stillCurrent()) return "";
    return String(reason || failure);
  } finally {
    busy.id = "";
  }
}
