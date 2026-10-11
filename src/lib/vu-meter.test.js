import { expect, spyOn, test } from "bun:test";
import { createMeterTicker, setMeterActivity, vuMeter } from "./vu-meter.js";

function clock() {
  let now = 0;
  let id = 0;
  const jobs = new Map();
  return {
    now: () => now,
    repeat: (fn) => (jobs.set(++id, fn), id),
    cancel: (job) => jobs.delete(job),
    advance(ms) {
      now += ms;
      for (const fn of jobs.values()) fn();
    },
    get pending() { return jobs.size; },
  };
}
function meter() {
  let drawn = null;
  return { draw: (values) => (drawn = [...values]), picture: () => drawn };
}

test("hidden meters do no work and all visible meters share one clock", () => {
  const time = clock();
  const ticker = createMeterTicker(time);
  const a = meter(), b = meter();
  const first = ticker.add(a.draw), second = ticker.add(b.draw);
  ticker.running(true);
  time.advance(200);
  expect(time.pending).toBe(0);
  expect(a.picture()).toBe(null);
  ticker.visible(first, true);
  ticker.visible(second, true);
  expect(time.pending).toBe(1);
  expect(a.picture()).toEqual(b.picture());
  const hidden = a.picture();
  ticker.visible(first, false);
  time.advance(300);
  expect(a.picture()).toEqual(hidden);
  expect(b.picture()).not.toEqual(hidden);
  ticker.visible(second, false);
  expect(time.pending).toBe(0);
});

test("pause freezes the picture and resume continues without replaying hidden time", () => {
  const time = clock();
  const ticker = createMeterTicker(time);
  const a = meter();
  const first = ticker.add(a.draw);
  ticker.visible(first, true);
  expect(time.pending).toBe(0);
  ticker.running(true);
  time.advance(200);
  ticker.running(false);
  const paused = a.picture();
  time.advance(5000);
  expect(time.pending).toBe(0);
  expect(a.picture()).toEqual(paused);
  ticker.running(true);
  expect(a.picture()).toEqual(paused);
  time.advance(300);
  expect(a.picture()).not.toEqual(paused);
  ticker.running(false);
  expect(time.pending).toBe(0);
});

test("removing the last visible meter cancels work and a later mount may animate", () => {
  const time = clock();
  const ticker = createMeterTicker(time);
  const a = meter(), b = meter();
  const first = ticker.add(a.draw);
  ticker.running(true);
  ticker.visible(first, true);
  ticker.remove(first);
  const removed = a.picture();
  time.advance(2000);
  expect(time.pending).toBe(0);
  expect(a.picture()).toEqual(removed);
  const second = ticker.add(b.draw);
  ticker.visible(second, true);
  expect(time.pending).toBe(1);
  const mounted = b.picture();
  time.advance(200);
  expect(b.picture()).not.toEqual(mounted);
  expect(a.picture()).toEqual(removed);
  ticker.remove(second);
  expect(time.pending).toBe(0);
});

test("a delayed real clock skips catch-up work and cancellation cannot restart it", () => {
  let now = 0;
  let id = 0;
  const jobs = new Map();
  const currentTime = spyOn(performance, "now").mockImplementation(() => now);
  const schedule = spyOn(globalThis, "setTimeout").mockImplementation((fn, delay) => {
    jobs.set(++id, { fn, delay });
    return id;
  });
  const cancel = spyOn(globalThis, "clearTimeout").mockImplementation((job) => jobs.delete(job));
  const ticker = createMeterTicker();
  const a = meter(), b = meter();
  const first = ticker.add(a.draw), second = ticker.add(b.draw);
  try {
    ticker.running(true);
    ticker.visible(first, true);
    ticker.visible(second, true);
    const initial = a.picture();
    expect(jobs.size).toBe(1);
    const late = jobs.values().next().value;
    jobs.clear();
    now = 1000;
    late.fn();
    expect(jobs.size).toBe(1);
    expect(a.picture()).not.toEqual(initial);
    expect(a.picture()).toEqual(b.picture());
    const pending = jobs.values().next().value;
    expect(pending.delay).toBeGreaterThan(0);

    ticker.running(false);
    const paused = a.picture();
    expect(jobs.size).toBe(0);
    now += 5000;
    pending.fn();
    expect(jobs.size).toBe(0);
    expect(a.picture()).toEqual(paused);
    expect(b.picture()).toEqual(paused);

    ticker.running(true);
    expect(jobs.size).toBe(1);
    expect(a.picture()).toEqual(paused);
    const resumed = jobs.values().next().value;
    jobs.clear();
    now += resumed.delay;
    resumed.fn();
    expect(a.picture()).not.toEqual(paused);
    expect(a.picture()).toEqual(b.picture());
    ticker.remove(first);
    expect(jobs.size).toBe(1);
    ticker.remove(second);
    expect(jobs.size).toBe(0);
  } finally {
    ticker.running(false);
    cancel.mockRestore();
    schedule.mockRestore();
    currentTime.mockRestore();
  }
});

test("the real scheduler bounds nominal cadence to 60 ticks per second without catch-up", () => {
  let now = 0, id = 0, ticks = 0;
  const jobs = new Map();
  const currentTime = spyOn(performance, "now").mockImplementation(() => now);
  const schedule = spyOn(globalThis, "setTimeout").mockImplementation((fn, delay) => {
    jobs.set(++id, { fn, at: now + delay });
    return id;
  });
  const cancel = spyOn(globalThis, "clearTimeout").mockImplementation((job) => jobs.delete(job));
  const ticker = createMeterTicker();
  const entry = ticker.add(() => {});
  try {
    ticker.running(true);
    ticker.visible(entry, true);
    while (jobs.size) {
      const [job, scheduled] = jobs.entries().next().value;
      if (scheduled.at > 1000) break;
      jobs.delete(job);
      now = scheduled.at;
      scheduled.fn();
      ticks++;
      expect(jobs.size).toBe(1);
    }
    expect(ticks).toBeGreaterThanOrEqual(59);
    expect(ticks).toBeLessThanOrEqual(60);
    ticker.visible(entry, false);
    expect(jobs.size).toBe(0);
  } finally {
    ticker.remove(entry);
    cancel.mockRestore();
    schedule.mockRestore();
    currentTime.mockRestore();
  }
});

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function settleSprites() {
  // Drain the decode/bitmap/ready promise chain without real timers.
  for (let i = 0; i < 12; i++) await Promise.resolve();
}

let spriteTestColor = 0;
async function withMeterEnvironment(run) {
  const keys = ["window", "Image", "createImageBitmap", "getComputedStyle", "IntersectionObserver"];
  const previous = new Map(keys.map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const decodes = [], bitmaps = [], mounted = new Set(), media = new Map();
  const color = `rgb(1, 2, ${++spriteTestColor})`;
  const environment = {
    decodes,
    bitmaps,
    mount() {
      const calls = [];
      const transforms = [];
      const ctx = {
        setTransform: (...args) => transforms.push(args),
        clearRect() {},
        drawImage: (...args) => calls.push(args),
      };
      const canvas = { width: 0, height: 0, getContext: () => ctx };
      const node = { transferControlToOffscreen: () => canvas };
      const action = vuMeter(node);
      const meter = {
        calls, transforms, canvas,
        destroy() {
          if (!mounted.delete(meter)) return;
          action.destroy();
        },
      };
      mounted.add(meter);
      return meter;
    },
    density(dpr) {
      const old = media.get(`(resolution: ${window.devicePixelRatio}dppx)`);
      window.devicePixelRatio = dpr;
      for (const listener of [...old.listeners]) listener();
    },
  };
  const replacements = {
    window: {
      devicePixelRatio: 1,
      matchMedia(query) {
        const result = {
          matches: false,
          listeners: new Set(),
          addEventListener: (_, fn) => result.listeners.add(fn),
          removeEventListener: (_, fn) => result.listeners.delete(fn),
        };
        media.set(query, result);
        return result;
      },
    },
    Image: class {
      decode() {
        const result = deferred();
        decodes.push(result);
        return result.promise;
      }
    },
    createImageBitmap() {
      const result = deferred();
      bitmaps.push(result);
      return result.promise;
    },
    getComputedStyle: () => ({ color }),
    IntersectionObserver: class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  };
  for (const key of keys) {
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value: replacements[key] });
  }
  try {
    setMeterActivity(false);
    await run(environment);
  } finally {
    for (const meter of mounted) meter.destroy();
    setMeterActivity(false);
    for (const [key, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
}

for (const stage of ["decode", "bitmap"]) {
  test(`a failed meter ${stage} is handled and a same-colour remount retries`, async () => {
    await withMeterEnvironment(async ({ mount, decodes, bitmaps }) => {
      const first = mount(), shared = mount();
      await settleSprites();
      expect(decodes.length).toBe(1);
      if (stage === "decode") {
        decodes[0].reject(new Error("decode unavailable"));
      } else {
        decodes[0].resolve();
        await settleSprites();
        expect(bitmaps.length).toBe(1);
        bitmaps[0].reject(new Error("bitmap unavailable"));
      }
      await settleSprites();
      expect(first.calls).toEqual([]);
      expect(shared.calls).toEqual([]);
      shared.destroy();

      const retry = mount();
      await settleSprites();
      expect(decodes.length).toBe(2);
      decodes[1].resolve();
      await settleSprites();
      const bitmap = { close: () => { throw new Error("shared sprite must stay open"); } };
      bitmaps.at(-1).resolve(bitmap);
      await settleSprites();
      expect(retry.calls.length).toBe(4);
      expect(retry.calls.every(([sprite]) => sprite === bitmap)).toBe(true);
      // A successful newer attempt must not revive a failed mount's request.
      expect(first.calls).toEqual([]);
      expect(shared.calls).toEqual([]);
    });
  });
}

test("destroyed meters ignore late sprite completion without closing another mount's shared bitmap", async () => {
  await withMeterEnvironment(async ({ mount, decodes, bitmaps }) => {
    const removed = mount(), alive = mount();
    await settleSprites();
    expect(decodes.length).toBe(1);
    removed.destroy();
    decodes[0].resolve();
    await settleSprites();
    let closed = 0;
    const bitmap = { close: () => closed++ };
    bitmaps[0].resolve(bitmap);
    await settleSprites();
    expect(removed.calls).toEqual([]);
    expect(alive.calls.length).toBe(4);
    expect(closed).toBe(0);
    alive.destroy();
    const remount = mount();
    expect(remount.calls.length).toBe(4);
    expect(remount.calls.every(([sprite]) => sprite === bitmap)).toBe(true);
    expect(decodes.length).toBe(1);
    expect(closed).toBe(0);
  });
});

test("a stale density sprite cannot draw into a refitted or destroyed meter", async () => {
  await withMeterEnvironment(async ({ mount, decodes, bitmaps, density }) => {
    const meter = mount();
    await settleSprites();
    density(2);
    await settleSprites();
    expect(decodes.length).toBe(2);
    expect(meter.canvas.width).toBe(28);
    expect(meter.canvas.height).toBe(26);
    expect(meter.transforms.at(-1)).toEqual([2, 0, 0, 2, 0, 0]);

    decodes[0].resolve();
    await settleSprites();
    const oldBitmap = {};
    bitmaps[0].resolve(oldBitmap);
    await settleSprites();
    expect(meter.calls).toEqual([]);
    decodes[1].resolve();
    await settleSprites();
    const currentBitmap = {};
    bitmaps[1].resolve(currentBitmap);
    await settleSprites();
    expect(meter.calls.length).toBe(4);
    expect(meter.calls.map(([sprite, x, , width]) => [sprite, x, width])).toEqual([
      [currentBitmap, 0, 2], [currentBitmap, 4, 2],
      [currentBitmap, 8, 2], [currentBitmap, 12, 2],
    ]);

    density(3);
    await settleSprites();
    decodes[2].resolve();
    await settleSprites();
    meter.destroy();
    bitmaps[2].resolve({});
    await settleSprites();
    expect(meter.calls.length).toBe(4);
  });
});

test("a decode rejection after the last meter is destroyed still permits a later mount", async () => {
  await withMeterEnvironment(async ({ mount, decodes, bitmaps }) => {
    const removed = mount();
    await settleSprites();
    removed.destroy();
    decodes[0].reject(new Error("late decode failure"));
    await settleSprites();
    expect(removed.calls).toEqual([]);
    expect(decodes.length).toBe(1);
    expect(bitmaps.length).toBe(0);

    const retry = mount();
    await settleSprites();
    expect(decodes.length).toBe(2);
    decodes[1].resolve();
    await settleSprites();
    bitmaps[0].resolve({});
    await settleSprites();
    expect(retry.calls.length).toBe(4);
    expect(removed.calls).toEqual([]);
  });
});
