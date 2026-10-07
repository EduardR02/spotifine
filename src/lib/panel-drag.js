/** Own one captured splitter gesture independently of the handle's DOM lifetime. */
export function startPanelDrag(event, { width, min, max, dpr, onChange, onEnd }) {
  const handle = event.currentTarget;
  const pointer = event.pointerId;
  const origin = event.clientX;
  let ended = false;

  function move(next) {
    if (next.pointerId !== pointer) return;
    const value = Math.min(max, Math.max(min, width + origin - next.clientX));
    onChange(Math.floor(value * dpr + 1e-6) / dpr);
  }
  function finish(commit) {
    if (ended) return;
    ended = true;
    handle.removeEventListener("pointermove", move);
    handle.removeEventListener("pointerup", release);
    handle.removeEventListener("pointercancel", cancel);
    handle.removeEventListener("lostpointercapture", cancel);
    if (handle.hasPointerCapture(pointer)) handle.releasePointerCapture(pointer);
    onEnd(commit);
  }
  function release(next) {
    if (next.pointerId !== pointer) return;
    move(next);
    finish(true);
  }
  function cancel(next) {
    if (next && next.pointerId !== pointer) return;
    finish(false);
  }

  handle.setPointerCapture(pointer);
  handle.addEventListener("pointermove", move);
  handle.addEventListener("pointerup", release);
  handle.addEventListener("pointercancel", cancel);
  handle.addEventListener("lostpointercapture", cancel);
  return cancel;
}
