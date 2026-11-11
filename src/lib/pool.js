/* =====================================================================
   THE POOL — the light a row takes under the pointer (app.css, THE POOL).

   One listener for the window. A move over a row writes the pointer's
   place in that row, in pixels from its left edge, on that row alone; the
   row's own hover plate draws the light there. The row's left edge is read
   once, as the pointer enters it — a row does not move sideways under a
   pointer that is on it — so a move reads nothing and lays nothing out.
   With the pointer still, or anywhere but on a row, nothing runs.
   ===================================================================== */

const ROWS = ".nav-item, .lib-row, .tl-row, .ep-row, .hi-main, .np-next, .menu-item, .p-saved-row";

/** Start following the pointer. Returns the stop. */
export function watchPool() {
  const fine = window.matchMedia("(pointer: fine)");
  const still = window.matchMedia("(prefers-reduced-motion: reduce)");
  let row = null;
  let left = 0;

  const leave = () => {
    row?.style.removeProperty("--px");
    row = null;
  };
  const move = (event) => {
    if (event.pointerType !== "mouse" || !fine.matches || still.matches) return;
    const hit = event.target instanceof Element ? event.target.closest(ROWS) : null;
    if (hit !== row) {
      leave();
      row = hit;
      if (row) left = row.getBoundingClientRect().left;
    }
    if (row) row.style.setProperty("--px", `${Math.round(event.clientX - left)}px`);
  };

  window.addEventListener("pointermove", move, { passive: true });
  return () => {
    window.removeEventListener("pointermove", move);
    leave();
  };
}
