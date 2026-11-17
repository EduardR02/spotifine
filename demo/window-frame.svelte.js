/* The window frame, as the demo build has it in place of
   src/lib/window-frame.svelte.js (demo/vite.config.js).

   The demo has no native window: no controls of its own to draw, and no
   room to leave for macOS's. Embedded in a page, its window is the frame it
   sits in, so that frame scrolled out of view is the window minimized: the
   playhead, the meter, the Canvas and the haze rest until it is back. */

export const os = "other";
export const drawsWindowControls = false;

export const frame = $state({ maximized: false, minimized: false });

let watching = false;

export function watchFrame() {
  if (watching || window.parent === window) return;
  watching = true;
  // Inside a frame, the implicit root is the top page's viewport.
  new IntersectionObserver(([entry]) => {
    frame.minimized = !entry.isIntersecting;
  }).observe(document.documentElement);
}

export const minimizeWindow = () => {};
export const toggleMaximizeWindow = () => {};
export const closeWindow = () => {};
