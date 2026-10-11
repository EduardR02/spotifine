import { expect, test } from "bun:test";
import { startPanelDrag } from "./panel-drag.js";

function gesture() {
  const handle = new EventTarget();
  let capture = null;
  handle.setPointerCapture = (id) => { capture = id; };
  handle.hasPointerCapture = (id) => capture === id;
  handle.releasePointerCapture = () => { capture = null; };
  const widths = [];
  const endings = [];
  const cancel = startPanelDrag({ currentTarget: handle, pointerId: 4, clientX: 500 }, {
    width: 300, min: 272, max: 400, dpr: 1.25,
    onChange: (width) => widths.push(width), onEnd: (commit) => endings.push(commit),
  });
  const send = (type, x, id = 4) => {
    const event = new Event(type);
    Object.assign(event, { clientX: x, pointerId: id });
    handle.dispatchEvent(event);
  };
  return { cancel, send, widths, endings, get capture() { return capture; } };
}

test("release persists the final clamped device-pixel width exactly once", () => {
  const p = gesture();
  p.send("pointermove", 450);
  p.send("pointerup", 430);
  p.send("lostpointercapture", 430);
  expect(p.widths).toEqual([349.6, 369.6]);
  expect(p.endings).toEqual([true]);
  expect(p.capture).toBeNull();
});

for (const interruption of ["pointercancel", "lostpointercapture", "owner teardown"]) {
  test(`${interruption} cancels tentative width without a persistence commit`, () => {
    const p = gesture();
    p.send("pointermove", 450);
    if (interruption === "owner teardown") p.cancel();
    else p.send(interruption, 450);
    p.cancel();
    p.send("pointerup", 400);
    p.send("pointermove", 400);
    expect(p.endings).toEqual([false]);
    expect(p.widths).toHaveLength(1);
    expect(p.capture).toBeNull();
  });
}

test("another pointer cannot move or finish the captured gesture", () => {
  const p = gesture();
  p.send("pointermove", 400, 5);
  p.send("pointercancel", 400, 5);
  p.send("pointerup", 400, 5);
  expect(p.widths).toEqual([]);
  expect(p.endings).toEqual([]);
  p.cancel();
});
