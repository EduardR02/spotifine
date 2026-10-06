import { flushSync } from "svelte";

/**
 * Sizes that depend on how much room the reading pane actually has.
 *
 * Kept out of the stylesheet because the pane's width is not the window's:
 * subtract the rail, the inspector when it is open, and three gutters. A
 * media query cannot see any of that, and the two that tried to derive it by
 * hand disagreed with each other. `ui.paneWidth` is the one measurement.
 */

/**
 * The cover on a detail header — playlist, album, Liked Songs.
 *
 * 184px is right when the title beside it has room to be 56px type. When the
 * pane is narrow the artwork is the thing that gives way: a record you have
 * already opened does not need to be recognised from across the room, but its
 * name and its controls still need to fit on one line.
 */
export function detailArtSize(pane) {
  const width = pane || 1200;
  if (width >= 760) return 184;
  if (width >= 560) return 144;
  return 104;
}

/* =====================================================================
   THE NOW-PLAYING PANEL'S WIDTH

   The panel runs top to bottom at the window's right edge, and a 9:16
   Canvas fills it at the window's full height when it is as wide as the
   Canvas is there: its NATURAL width. Never wider — past that the Canvas
   would have to be blown up — and never wider than a Canvas can fill
   sharply: 720px (the common width) at 1.2 device pixels to each of its
   own. Narrower than natural, the Canvas keeps the full height and is
   cropped evenly to its centre (placeCanvas, NowPlayingPanel), as Spotify's
   own apps crop it and as artists are told to compose it for.

   Left alone it is a third of the window, or its natural width if that is
   less. A width dragged by hand (the handle in the gap, App.svelte)
   replaces the third and is kept. Either way it never squeezes the reading
   pane below PANE_MIN; and when even PANEL_MIN would, the panel steps
   aside on its own until there is room again, without touching whether it
   was asked to be open.

   PANE_MIN protects title/artist and duration, not optional metadata.
   At 400px, with album, added and artwork dropped, the title cell has
   196px after the narrow pane's padding, row padding, gaps and controls.
   TrackList owns the independent thresholds for its optional columns.

   PANEL_MIN holds the head, the hero and the cards.
   ===================================================================== */
export const PANE_MIN = 400;
export const PANEL_MIN = 272;
const CANVAS_ASPECT = 9 / 16;
const CANVAS_SHARP = 720 * 1.2;

/**
 * The panel's box: its width in CSS px, flush with the window's right edge,
 * and the range a drag may take it through.
 *
 * @param {object} at
 * @param {number} at.width   the window's width, CSS px
 * @param {number} at.height  the window's height, CSS px
 * @param {number} at.start   where the reading pane starts, CSS px from the
 *                            left: the pane, the gutter and the panel share
 *                            what is right of it
 * @param {number} at.gutter  the gap between the pane and the panel
 * @param {number} [at.dpr]   device pixels per CSS px
 * @param {number|null} [at.user] the width dragged by hand, or null
 * @returns {{ fits: boolean, width: number, min: number, max: number }}
 */
export function panelLayout({ width, height, start, gutter, dpr = 1, user = null }) {
  const natural = Math.min(height * CANVAS_ASPECT, CANVAS_SHARP / dpr);
  const min = Math.min(PANEL_MIN, natural);
  const room = width - start - gutter - PANE_MIN;
  const max = Math.max(min, Math.min(natural, room));
  const want = user ?? Math.min(natural, width / 3);
  return { fits: room >= min, width: snapPanel(Math.min(max, Math.max(min, want)), dpr), min, max };
}

/**
 * A panel width on whole device pixels, never past `width`: the window's
 * right edge is on the grid, so the panel's left edge — the video's, the
 * pane's and the bar's right ones — lands on it too, where a rim and a
 * corner stay crisp.
 */
export function snapPanel(width, dpr = 1) {
  return Math.floor(width * dpr + 1e-6) / dpr;
}

const reducedMotion =
  typeof window !== "undefined" && window.matchMedia
    ? window.matchMedia("(prefers-reduced-motion: reduce)")
    : null;

/**
 * Change the shell's layout ONCE and let the compositor animate the result.
 *
 * Opening or closing the now-playing panel moves every column in the
 * window. Animating the grid would lay the whole app out on every frame of
 * the motion. A view transition lays it out once: the browser snapshots the
 * named surfaces (see THE LAYOUT MORPH in app.css), applies `update`
 * synchronously, and animates the snapshots on the compositor.
 *
 * The names exist only while `.morphing` is on the root, so at rest none of
 * the surfaces carries the stacking context a name implies.
 */
let morphs = 0;

export function morphLayout(update) {
  const root = document.documentElement;
  if (!document.startViewTransition || reducedMotion?.matches || document.hidden) {
    update();
    return;
  }
  morphs += 1;
  root.classList.add("morphing");
  const transition = document.startViewTransition(() => {
    update();
    flushSync();
  });
  /* A skipped morph — another one started mid-flight, or no frame came to
     capture — still applies `update`; only its animation is lost, so its
     rejection is not an error. The names stay while any morph needs them. */
  transition.ready.catch(() => {});
  transition.finished
    .catch(() => {})
    .finally(() => {
      morphs -= 1;
      if (!morphs) root.classList.remove("morphing");
    });
}
