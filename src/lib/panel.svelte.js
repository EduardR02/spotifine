import { ui } from "./state.svelte.js";

/**
 * The now-playing panel's room and width (layout.js, THE NOW-PLAYING
 * PANEL'S WIDTH), apart from whether it was asked to be open.
 *
 *   width     the width dragged by hand, or null for the automatic one.
 *             Kept in localStorage like the open state, and read at module
 *             init so the first paint already has it.
 *   fits      written by App.svelte: there is room for the panel. While
 *             there is not it steps aside, and `ui.nowPlayingOpen` — the
 *             person's choice — is left as it was.
 *   resizing  written by App.svelte while the handle is dragged: the panel
 *             holds back what is redone once at the end (the haze's anchor,
 *             NowPlayingPanel).
 */
const WIDTH_KEY = "sr.now-playing-width";

function readWidth() {
  try {
    const value = Number(localStorage.getItem(WIDTH_KEY));
    return value > 0 ? value : null;
  } catch {
    return null;
  }
}

export const panel = $state({ width: readWidth(), fits: true, resizing: false });

/** The one write point for the dragged width; null goes back to automatic. */
export function setPanelWidth(width) {
  panel.width = width > 0 ? width : null;
  try {
    if (panel.width) localStorage.setItem(WIDTH_KEY, String(panel.width));
    else localStorage.removeItem(WIDTH_KEY);
  } catch {
    /* private mode / storage disabled: the width stays session-local */
  }
}

/** The panel is on screen: asked for, and there is room for it. */
export function nowPlayingShown() {
  return ui.nowPlayingOpen && panel.fits;
}
