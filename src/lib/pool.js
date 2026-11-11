/* =====================================================================
   THE POOL — the light a row takes under the pointer (app.css, THE POOL).

   One listener for the window. A move over a row writes the pointer's
   place in that row, in pixels from its left edge, on that row alone; the
   row's own hover plate draws the light there. The row's left edge is read
   once, as the pointer enters it — a row does not move sideways under a
   pointer that is on it — so a move reads nothing and lays nothing out.
   With the pointer still, or anywhere but on a row, nothing runs.

   A write is a style pass for the row and a repaint of it, so there are
   as few as the eye needs: the place is kept to two pixels, and written at
   most once every GAP milliseconds — the light is 250px of soft glow, and
   thirty places a second is as smooth as it can be seen to be. The last
   place of a gesture is always written, a moment late at most, so the
   light never rests beside the pointer.
   ===================================================================== */

const ROWS = ".nav-item, .lib-row, .tl-row, .ep-row, .hi-main, .np-next, .menu-item, .p-saved-row";
/** The least time between two writes, in ms. */
const GAP = 32;

/** Start following the pointer. Returns the stop. */
export function watchPool() {
  const fine = window.matchMedia("(pointer: fine)");
  const still = window.matchMedia("(prefers-reduced-motion: reduce)");
  let row = null;
  let left = 0;
  /** The place last written, and the place the pointer is at. */
  let written = NaN;
  let wanted = 0;
  /** When the next write may happen, and the timer holding one back. */
  let due = 0;
  let timer = 0;

  const write = () => {
    timer = 0;
    if (!row || wanted === written) return;
    due = performance.now() + GAP;
    written = wanted;
    row.style.setProperty("--px", `${wanted}px`);
  };
  const leave = () => {
    if (timer) clearTimeout(timer);
    timer = 0;
    row?.style.removeProperty("--px");
    row = null;
  };
  const move = (event) => {
    if (event.pointerType !== "mouse" || !fine.matches || still.matches) return;
    const hit = event.target instanceof Element ? event.target.closest(ROWS) : null;
    if (hit !== row) {
      // Read before anything is written: a read after the old row's style
      // was touched would make the browser restyle first.
      if (hit) left = hit.getBoundingClientRect().left;
      leave();
      row = hit;
      written = NaN;
      due = 0;
    }
    if (!row) return;
    wanted = Math.round((event.clientX - left) / 2) * 2;
    if (wanted === written) return;
    const wait = due - performance.now();
    if (wait <= 0) write();
    else if (!timer) timer = setTimeout(write, wait);
  };

  window.addEventListener("pointermove", move, { passive: true });
  return () => {
    window.removeEventListener("pointermove", move);
    leave();
  };
}
