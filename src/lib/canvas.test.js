import { expect, test } from "bun:test";
import { createCanvasOwner } from "./canvas.js";

const settle = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
function panel() {
  const requests = [];
  const changes = [];
  let picture;
  const owner = createCanvasOwner((key) => new Promise((resolve, reject) => requests.push({ key, resolve, reject })), (next) => {
    picture = next;
    changes.push(next);
  });
  return { owner, requests, changes, get picture() { return picture; } };
}

test("a hidden track change invalidates the earlier pending Canvas", async () => {
  const p = panel();
  p.owner.update("a", true, true);
  await settle();
  p.owner.update("b", true, false);
  p.requests[0].resolve({ url: "canvas-a" });
  await settle();
  expect(p.picture.ready).toBe(false);
  expect(p.picture.accepted).toBe(false);
  expect(p.picture.waiting).toBe(false);
  expect(p.requests).toHaveLength(1);
  p.owner.update("b", true, true);
  await settle();
  expect(p.requests[1].key).toBe("b");
});

test("a closed panel cannot clear a replacement panel's waiting state", async () => {
  let waiting = false;
  let answer;
  const old = createCanvasOwner(() => new Promise((resolve) => { answer = resolve; }), (picture) => { waiting = picture.waiting; });
  old.update("a", true, true);
  await settle();
  old.dispose();
  const current = createCanvasOwner(() => new Promise(() => {}), (picture) => { waiting = picture.waiting; });
  current.update("b", true, true);
  answer(null);
  await settle();
  expect(waiting).toBe(true);
  current.dispose();
});

test("the previous video's readiness and error cannot arrive during a new track request", async () => {
  const p = panel();
  p.owner.update("a", true, true);
  await settle();
  p.requests[0].resolve({ url: "canvas-a" });
  await settle();
  p.owner.arrive("canvas-a");
  expect(p.picture.ready).toBe(true);
  p.owner.update("b", true, true);
  p.owner.arrive("canvas-a");
  p.owner.fail("canvas-a");
  expect(p.picture.ready).toBe(false);
  expect(p.picture.waiting).toBe(true);
  await settle();
  p.requests[1].resolve({ url: "canvas-b" });
  await settle();
  p.owner.arrive("canvas-a");
  expect(p.picture.ready).toBe(false);
  p.owner.arrive("canvas-b");
  expect(p.picture.ready).toBe(true);
  expect(p.picture.waiting).toBe(false);
});

test("a confirmed shared source can arrive without requesting or reloading it again", async () => {
  const p = panel();
  p.owner.update("a", true, true);
  await settle();
  p.requests[0].resolve({ url: "shared" });
  await settle();
  p.owner.arrive("shared");
  p.owner.update("b", true, true);
  await settle();
  p.requests[1].resolve({ url: "shared" });
  await settle();
  p.owner.arrive("shared");
  p.owner.update("b", true, true);
  await settle();
  expect(p.picture.ready).toBe(true);
  expect(p.requests).toHaveLength(2);
});

test("missing/failed Canvas and setting disable restore the cover", async () => {
  const p = panel();
  p.owner.update("a", true, true);
  await settle();
  p.requests[0].reject(new Error("unavailable"));
  await settle();
  expect(p.picture.url).toBe("");
  expect(p.picture.waiting).toBe(false);
  p.owner.update("b", true, true);
  await settle();
  p.owner.update("b", false, true);
  p.requests[1].resolve({ url: "too-late" });
  await settle();
  expect(p.picture.url).toBe("");
  expect(p.picture.ready).toBe(false);
});
