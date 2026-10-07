import { expect, spyOn, test } from "bun:test";
import { createMeterTicker } from "./vu-meter.js";

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
  const bars = Array.from({ length: 4 }, () => ({ style: { transform: "" } }));
  return { bars, picture: () => bars.map((bar) => bar.style.transform) };
}

test("hidden meters do no work and all visible meters share one clock", () => {
  const time = clock();
  const ticker = createMeterTicker(time);
  const a = meter(), b = meter();
  const first = ticker.add(a.bars), second = ticker.add(b.bars);
  ticker.running(true);
  time.advance(200);
  expect(time.pending).toBe(0);
  expect(a.picture()).toEqual(["", "", "", ""]);
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
  const first = ticker.add(a.bars);
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
  const first = ticker.add(a.bars);
  ticker.running(true);
  ticker.visible(first, true);
  ticker.remove(first);
  const removed = a.picture();
  time.advance(2000);
  expect(time.pending).toBe(0);
  expect(a.picture()).toEqual(removed);
  const second = ticker.add(b.bars);
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
  const first = ticker.add(a.bars), second = ticker.add(b.bars);
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
    expect(Number.isInteger(pending.delay)).toBe(true);

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
