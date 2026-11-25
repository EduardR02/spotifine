/**
 * An overlay scrollbar for one scroll container: `use:scrollbar`.
 *
 * Native bars are off app-wide (app.css). A native bar is laid out INSIDE
 * its scroller, so every scroller either reserved a 10px gutter or reflowed
 * when it began to scroll — and the gutter is where glass stopped: the
 * sticky topbar, the track head and every full-bleed band ended 10px short
 * of the pane's edge, a strip of different material down the right side.
 * This bar is drawn OVER the content instead and takes no width at all.
 *
 * It is a sibling of the scroller, not a child: a child would scroll with
 * the content, and nothing inside the scroller's subtree has to change. So
 * the scroller's parent must be a containing block (positioned), and a
 * scroller that is itself a popover must be the popover's child, so that
 * its bar is in the top layer with it.
 *
 * Cheap on purpose. Geometry is measured only when something resizes
 * (a ResizeObserver on the scroller and its direct children, which is
 * where content growth shows up); the scroll path reads `scrollTop` — the
 * one read the pane's own scroll listener already makes — and writes one
 * transform. Showing and hiding is a class on the bar, flipped only when
 * the state changes; the idle timer is one timeout re-armed by comparison,
 * not cleared and set on every event.
 *
 * @param {HTMLElement} node
 * @param {{ top?: number, bottom?: number }} [options] insets of the track
 *   in px, for a scroller whose top or bottom is covered by a floating bar.
 */
export function scrollbar(node, options = {}) {
  const IDLE_MS = 1100;
  const MIN_THUMB = 28;
  const PAD = 3;

  let insetTop = options.top ?? 0;
  let insetBottom = options.bottom ?? 0;

  const track = document.createElement("div");
  track.className = "sb";
  track.setAttribute("aria-hidden", "true");
  const thumb = document.createElement("div");
  thumb.className = "sb-thumb";
  track.append(thumb);
  node.after(track);

  let range = 0; // scrollHeight - clientHeight
  let travel = 0; // px the thumb can move
  let shown = false;
  let held = false; // pointer on the bar, or dragging it
  let wokeAt = 0;
  let timer = 0;
  let edge = Infinity; // the scroller's right edge, in client px

  function measure() {
    const viewH = node.clientHeight;
    range = Math.max(0, node.scrollHeight - viewH);
    const trackH = Math.max(0, viewH - insetTop - insetBottom);
    track.style.top = `${node.offsetTop + insetTop}px`;
    track.style.left = `${node.offsetLeft + node.offsetWidth - 12}px`;
    track.style.height = `${trackH}px`;
    const thumbH = range > 0 ? Math.max(MIN_THUMB, Math.round((trackH * viewH) / (range + viewH))) : 0;
    thumb.style.height = `${thumbH}px`;
    travel = Math.max(0, trackH - thumbH - PAD * 2);
    track.classList.toggle("none", range <= 0);
    edge = node.getBoundingClientRect().right;
    place();
  }

  function place() {
    const y = PAD + (range > 0 ? (Math.min(node.scrollTop, range) / range) * travel : 0);
    thumb.style.transform = `translate3d(0, ${y}px, 0)`;
  }

  function show(on) {
    if (shown === on) return;
    shown = on;
    track.classList.toggle("on", on);
  }

  function idle() {
    const left = IDLE_MS - (performance.now() - wokeAt);
    if (left > 16) {
      timer = setTimeout(idle, left);
      return;
    }
    timer = 0;
    if (!held) show(false);
  }

  function wake() {
    if (range <= 0) return;
    wokeAt = performance.now();
    show(true);
    if (!timer) timer = setTimeout(idle, IDLE_MS);
  }

  function onScroll() {
    place();
    wake();
  }

  /* Pointing is a reason to show the bar only near it: a bar that lit up
     for every movement anywhere over the pane would never be at rest. */
  function onPoint(event) {
    if (event.clientX > edge - 48) wake();
  }

  /* Dragging: the thumb follows the pointer 1:1 along its travel. */
  let dragFrom = 0;
  let dragTop = 0;
  function onDown(event) {
    if (event.button !== 0 || !shown) return;
    event.preventDefault();
    thumb.setPointerCapture(event.pointerId);
    held = true;
    track.classList.add("drag");
    dragFrom = event.clientY;
    dragTop = node.scrollTop;
  }
  function onMove(event) {
    if (!track.classList.contains("drag") || travel <= 0) return;
    node.scrollTop = dragTop + ((event.clientY - dragFrom) * range) / travel;
  }
  function onUp(event) {
    if (!track.classList.contains("drag")) return;
    thumb.releasePointerCapture?.(event.pointerId);
    track.classList.remove("drag");
    held = thumb.matches(":hover");
    wake();
  }
  function onEnter() {
    held = true;
    wake();
  }
  function onLeave() {
    if (track.classList.contains("drag")) return;
    held = false;
    wake();
  }
  /* The bar is not inside the scroller, so a wheel over it would scroll
     whatever is behind the scroller instead. */
  function onWheel(event) {
    node.scrollBy({ top: event.deltaY, left: 0, behavior: "instant" });
  }

  /* Direct-child changes can alter scrollHeight without resizing any surviving
     observed child (notably when the tallest child is removed). Reconcile
     targets and measure after the mutation; resize reports cover later growth. */
  const watched = new Set();
  const resize = new ResizeObserver(measure);
  function watchChildren() {
    for (const child of watched) {
      if (child.parentNode !== node) {
        watched.delete(child);
        resize.unobserve(child);
      }
    }
    for (const child of node.children) {
      if (!watched.has(child)) {
        watched.add(child);
        resize.observe(child);
      }
    }
  }
  const mutations = new MutationObserver(() => {
    watchChildren();
    measure();
  });

  function onWindowResize() {
    edge = node.getBoundingClientRect().right;
  }

  resize.observe(node);
  watchChildren();
  mutations.observe(node, { childList: true });
  window.addEventListener("resize", onWindowResize, { passive: true });
  node.addEventListener("scroll", onScroll, { passive: true });
  node.addEventListener("pointermove", onPoint, { passive: true });
  thumb.addEventListener("pointerdown", onDown);
  thumb.addEventListener("pointermove", onMove, { passive: true });
  thumb.addEventListener("pointerup", onUp);
  thumb.addEventListener("pointercancel", onUp);
  thumb.addEventListener("pointerenter", onEnter, { passive: true });
  thumb.addEventListener("pointerleave", onLeave, { passive: true });
  thumb.addEventListener("wheel", onWheel, { passive: true });

  return {
    update(next = {}) {
      insetTop = next.top ?? 0;
      insetBottom = next.bottom ?? 0;
      measure();
    },
    destroy() {
      clearTimeout(timer);
      resize.disconnect();
      mutations.disconnect();
      window.removeEventListener("resize", onWindowResize);
      node.removeEventListener("scroll", onScroll);
      node.removeEventListener("pointermove", onPoint);
      track.remove();
    },
  };
}
