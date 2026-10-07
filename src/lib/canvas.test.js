import { expect, test } from "bun:test";
import { createCanvasLookups, createCanvasOwner, FRAME_WAIT_MS, LOOKUP_WAIT_MS } from "./canvas.js";

const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

/** Timers on a clock the test moves. */
function fakeClock() {
  let now = 0;
  let id = 0;
  const jobs = new Map();
  return {
    now: () => now,
    wait: (fn, ms) => (jobs.set(++id, { fn, at: now + ms }), id),
    unwait: (job) => jobs.delete(job),
    advance(ms) {
      now += ms;
      for (const [job, { fn, at }] of [...jobs].sort((a, b) => a[1].at - b[1].at)) {
        if (at > now) break;
        jobs.delete(job);
        fn();
      }
    },
    get pending() { return jobs.size; },
  };
}

/** A panel: its lookups (answered by hand), the loops it holds decoded, and
    every picture it was handed. */
function panel() {
  const requests = [];
  const lookups = createCanvasLookups((id) => new Promise((resolve, reject) => requests.push({ id, resolve, reject })));
  const decoded = new Set();
  const clock = fakeClock();
  const changes = [];
  const owner = createCanvasOwner({
    lookups,
    decoded: (url) => decoded.has(url),
    changed: (picture) => changes.push(picture),
    clock,
  });
  const p = {
    owner, lookups, requests, decoded, clock, changes,
    get picture() { return changes.at(-1); },
    /** The haze's sources, in order, from the first picture after `from`. */
    hazeSince(from) {
      const out = [];
      for (const { haze } of changes.slice(from)) if (out.at(-1) !== haze) out.push(haze);
      return out;
    },
    play: (key, ahead = "", { enabled = true, visible = true } = {}) => owner.update(key, enabled, visible, ahead),
  };
  return p;
}

test("a record known to have no Canvas lights the haze from its cover at once", async () => {
  const p = panel();
  p.play("a");
  p.requests[0].resolve(null);
  await settle();
  p.play("b");
  const mark = p.changes.length;
  p.play("a");
  expect(p.requests.map((r) => r.id)).toEqual(["a", "b"]);
  expect(p.picture).toMatchObject({ url: "", haze: "cover" });
  expect(p.hazeSince(mark)).toEqual(["cover"]);
  expect(p.clock.pending).toBe(0);
});

test("a known Canvas already decoded is the picture and the haze's source at the change", async () => {
  const p = panel();
  p.play("a");
  p.requests[0].resolve({ url: "loop-a" });
  await settle();
  p.play("b");
  p.decoded.add("loop-a");
  const mark = p.changes.length;
  p.play("a");
  expect(p.picture).toMatchObject({ url: "loop-a", ready: true, haze: "video" });
  expect(p.hazeSince(mark)).toEqual(["video"]);
});

test("a known Canvas still loading holds the haze until its first frame", async () => {
  const p = panel();
  p.play("a");
  p.requests[0].resolve({ url: "loop-a" });
  await settle();
  p.play("b");
  const mark = p.changes.length;
  p.play("a");
  expect(p.picture).toMatchObject({ url: "loop-a", ready: false, haze: "hold" });
  p.clock.advance(FRAME_WAIT_MS - 1);
  p.owner.arrive("loop-a");
  expect(p.picture).toMatchObject({ ready: true, haze: "video" });
  expect(p.hazeSince(mark)).toEqual(["hold", "video"]);
  expect(p.clock.pending).toBe(0);
});

test("a first frame later than the bound leaves the haze on the cover for the whole record", async () => {
  const p = panel();
  p.play("a");
  p.requests[0].resolve({ url: "loop-a" });
  await settle();
  p.play("b");
  const mark = p.changes.length;
  p.play("a");
  p.clock.advance(FRAME_WAIT_MS);
  expect(p.picture).toMatchObject({ url: "loop-a", ready: false, haze: "cover" });
  p.owner.arrive("loop-a");
  // The panel crossfades to the loop; the haze is not lit a second time.
  expect(p.picture).toMatchObject({ url: "loop-a", ready: true, haze: "cover" });
  expect(p.hazeSince(mark)).toEqual(["hold", "cover"]);
});

test("an unknown record holds the haze for its lookup, then follows the answer", async () => {
  const p = panel();
  p.play("a");
  expect(p.picture).toMatchObject({ url: "", haze: "hold" });
  expect(p.requests.map((r) => r.id)).toEqual(["a"]);
  p.clock.advance(LOOKUP_WAIT_MS - 100);
  p.requests[0].resolve({ url: "loop-a" });
  await settle();
  expect(p.picture).toMatchObject({ url: "loop-a", ready: false, haze: "hold" });
  // The frame is waited for from the change of record, not from the answer.
  p.clock.advance(FRAME_WAIT_MS - LOOKUP_WAIT_MS + 99);
  expect(p.picture.haze).toBe("hold");
  p.clock.advance(1);
  expect(p.picture.haze).toBe("cover");

  p.play("b");
  p.requests[1].resolve(null);
  await settle();
  expect(p.picture).toMatchObject({ url: "", haze: "cover" });
  expect(p.clock.pending).toBe(0);
});

test("a lookup slower than its bound lights the cover, and its late Canvas only reaches the panel", async () => {
  const p = panel();
  p.play("a");
  const mark = p.changes.length - 1;
  p.clock.advance(LOOKUP_WAIT_MS);
  expect(p.picture).toMatchObject({ url: "", haze: "cover" });
  p.requests[0].resolve({ url: "loop-a" });
  await settle();
  p.owner.arrive("loop-a");
  expect(p.picture).toMatchObject({ url: "loop-a", ready: true, haze: "cover" });
  expect(p.hazeSince(mark)).toEqual(["hold", "cover"]);
});

test("a failed lookup is the cover now and is asked again on the next visit", async () => {
  const p = panel();
  p.play("a");
  p.requests[0].reject(new Error("offline"));
  await settle();
  expect(p.picture).toMatchObject({ url: "", haze: "cover" });
  p.play("b");
  p.play("a");
  expect(p.requests.map((r) => r.id)).toEqual(["a", "b", "a"]);
});

test("a superseded record's answer, timer and frame never land on the next one", async () => {
  const p = panel();
  p.play("a");
  p.play("b");
  p.requests[0].resolve({ url: "loop-a" });
  await settle();
  expect(p.picture).toMatchObject({ url: "", haze: "hold" });
  p.owner.arrive("loop-a");
  expect(p.picture).toMatchObject({ ready: false, haze: "hold" });
  // a's answer is kept all the same: going back to it needs no request.
  expect(p.lookups.peek("a")).toBe("loop-a");
  p.clock.advance(LOOKUP_WAIT_MS - 1);
  p.requests[1].resolve({ url: "loop-b" });
  await settle();
  p.owner.arrive("loop-b");
  expect(p.picture).toMatchObject({ url: "loop-b", ready: true, haze: "video" });
  p.clock.advance(FRAME_WAIT_MS * 2);
  expect(p.picture.haze).toBe("video");
});

test("rapid skips hold the haze once and light it only for where they stop", async () => {
  const p = panel();
  const mark = p.changes.length;
  for (const key of ["a", "b", "c", "d"]) {
    p.play(key);
    p.clock.advance(150);
  }
  for (const request of p.requests.slice(0, 3)) request.resolve({ url: `loop-${request.id}` });
  await settle();
  for (const key of ["a", "b", "c"]) p.owner.arrive(`loop-${key}`);
  expect(p.picture).toMatchObject({ url: "", haze: "hold" });
  p.requests[3].resolve({ url: "loop-d" });
  await settle();
  p.owner.arrive("loop-d");
  expect(p.hazeSince(mark)).toEqual(["hold", "video"]);
  expect(p.picture.url).toBe("loop-d");
});

test("Canvas off asks for nothing and warms nothing; on again asks", async () => {
  const p = panel();
  p.play("a", "b", { enabled: false });
  expect(p.picture).toMatchObject({ url: "", haze: "cover", warm: "" });
  expect(p.requests).toHaveLength(0);
  p.play("a", "b");
  expect(p.picture.haze).toBe("hold");
  expect(p.requests.map((r) => r.id)).toEqual(["a", "b"]);
  p.requests[0].resolve({ url: "loop-a" });
  p.requests[1].resolve({ url: "loop-b" });
  await settle();
  expect(p.picture).toMatchObject({ url: "loop-a", warm: "loop-b" });
  p.play("a", "b", { enabled: false });
  expect(p.picture).toMatchObject({ url: "", haze: "cover", warm: "" });
});

test("a window nobody can see asks nothing until it is seen", async () => {
  const p = panel();
  p.play("a", "b", { visible: false });
  expect(p.requests).toHaveLength(0);
  expect(p.clock.pending).toBe(0);
  expect(p.picture.warm).toBe("");
  p.play("a", "b");
  expect(p.requests.map((r) => r.id)).toEqual(["a", "b"]);
  expect(p.picture.haze).toBe("hold");
});

test("the record the engine fetches ahead is warmed, and an advance onto it is lit at once", async () => {
  const p = panel();
  p.play("a");
  p.requests[0].resolve(null);
  await settle();
  p.play("a", "b");
  p.requests[1].resolve({ url: "loop-b" });
  await settle();
  expect(p.picture).toMatchObject({ url: "", warm: "loop-b" });
  p.decoded.add("loop-b");
  // The advance is one engine state: the new record, and the preload cleared.
  const mark = p.changes.length;
  p.play("b", "");
  expect(p.picture).toMatchObject({ url: "loop-b", ready: true, haze: "video", warm: "" });
  expect(p.hazeSince(mark)).toEqual(["video"]);
  expect(p.requests).toHaveLength(2);
});

test("a preload made obsolete takes its warmed loop with it", async () => {
  const p = panel();
  p.play("a", "b");
  p.requests[0].resolve(null);
  p.requests[1].resolve({ url: "loop-b" });
  await settle();
  expect(p.picture.warm).toBe("loop-b");
  // An edit points the preload elsewhere, then a pick clears it.
  p.play("a", "c");
  expect(p.picture.warm).toBe("");
  p.requests[2].resolve({ url: "loop-c" });
  await settle();
  expect(p.picture.warm).toBe("loop-c");
  p.play("x", "");
  expect(p.picture.warm).toBe("");
  // A late answer for a preload that has gone stays out.
  p.play("x", "d");
  p.play("x", "");
  p.requests.find((r) => r.id === "d").resolve({ url: "loop-d" });
  await settle();
  expect(p.picture.warm).toBe("");
});

test("an episode has no Canvas, and the song after it is still warmed", async () => {
  const p = panel();
  p.play("", "b");
  expect(p.picture).toMatchObject({ url: "", haze: "cover" });
  expect(p.requests.map((r) => r.id)).toEqual(["b"]);
  p.requests[0].resolve({ url: "loop-b" });
  await settle();
  expect(p.picture.warm).toBe("loop-b");
});

test("repeat-one fetches the playing record ahead, which is not warmed twice", () => {
  const p = panel();
  p.play("a", "a");
  expect(p.requests.map((r) => r.id)).toEqual(["a"]);
  expect(p.picture.warm).toBe("");
});

test("a loop that fails is the cover, and the previous loop's events cannot land", async () => {
  const p = panel();
  p.play("a");
  p.requests[0].resolve({ url: "loop-a" });
  await settle();
  p.owner.arrive("loop-a");
  p.play("b");
  p.owner.arrive("loop-a");
  p.owner.fail("loop-a");
  expect(p.picture).toMatchObject({ url: "", ready: false, haze: "hold" });
  p.requests[1].resolve({ url: "loop-b" });
  await settle();
  p.owner.fail("loop-b");
  expect(p.picture).toMatchObject({ url: "", haze: "cover", resetPreference: true });
});

test("a closed panel publishes nothing more", async () => {
  const p = panel();
  p.play("a", "b");
  const count = p.changes.length;
  p.owner.dispose();
  p.requests[0].resolve({ url: "loop-a" });
  p.requests[1].resolve({ url: "loop-b" });
  await settle();
  p.clock.advance(FRAME_WAIT_MS);
  p.owner.arrive("loop-a");
  expect(p.changes).toHaveLength(count);
});

test("lookups are shared while in flight, kept when answered, and forgotten with the account", async () => {
  let calls = 0;
  let answer;
  const lookups = createCanvasLookups(() => (calls++, new Promise((resolve) => { answer = resolve; })));
  const first = lookups.lookup("a");
  const second = lookups.lookup("a");
  expect(first).toBe(second);
  answer({ url: "loop-a" });
  expect(await first).toBe("loop-a");
  expect(lookups.peek("a")).toBe("loop-a");
  await lookups.lookup("a");
  expect(calls).toBe(1);

  const late = lookups.lookup("b");
  lookups.forget();
  answer(null);
  expect(await late).toBe("");
  expect(lookups.peek("a")).toBeUndefined();
  expect(lookups.peek("b")).toBeUndefined();
});

test("the lookup memory is bounded, oldest first", async () => {
  const lookups = createCanvasLookups((id) => ({ url: `loop-${id}` }), 2);
  for (const id of ["a", "b", "c"]) await lookups.lookup(id);
  expect(lookups.peek("a")).toBeUndefined();
  expect(lookups.peek("c")).toBe("loop-c");
});
