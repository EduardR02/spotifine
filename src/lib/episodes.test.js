import { expect, test } from "bun:test";
import { createEpisodeWindow, orderEpisodes } from "./episodes.js";

const index = [
  ["Épisode 2", 100, false],
  ["Episode 10", null, true],
  ["Episode 3", 300, true],
  ["Episode 4", 300, false],
  ["Episode 11", NaN, true],
  ["Episode 2", null, true],
];

test("publication order is stable; unknown dates are last in either direction", () => {
  expect(orderEpisodes(index)).toEqual([2, 3, 0, 1, 4, 5]);
  expect(orderEpisodes(index, "oldest")).toEqual([0, 2, 3, 1, 4, 5]);
  expect(index[0][0]).toBe("Épisode 2");
});

test("title order compares numbers naturally and retains source order for collator ties", () => {
  expect(orderEpisodes(index, "title")).toEqual([0, 5, 2, 3, 1, 4]);
});

function fixture() {
  const requests = [];
  let view;
  const owner = createEpisodeWindow((positions) => {
    const request = Promise.withResolvers();
    requests.push({ ...request, positions });
    return request.promise;
  }, (next) => { view = next; });
  const arrive = async (i, cached = false) => {
    const { positions, resolve } = requests[i];
    resolve({ episodes: positions.map((position) => ({ track: { id: String(position), cached } })) });
    await Promise.resolve();
  };
  return { owner, requests, arrive, view: () => view };
}

test("scrolling retains overlapping rows and requests only missing positions from the latest window", async () => {
  const { owner, requests, arrive, view } = fixture();
  owner.show([9, 1, 7]);
  await arrive(0);
  const retained = view().rows.get(7);
  owner.show([7, 1, 3]);
  owner.show([7, 3, 5]);
  owner.show([7, 3, 6]);
  expect(view().rows.get(7)).toBe(retained);
  expect(requests.map((r) => r.positions)).toEqual([[9, 1, 7], [3]]);
  await arrive(1);
  expect(requests[2].positions).toEqual([6]);
  await arrive(2);
  expect([...view().rows.keys()]).toEqual([7, 3, 6]);
  expect(view().rows.get(7)).toBe(retained);
});

test("live cache marks survive late rows, evictions and cache resets", async () => {
  const { owner, arrive, view } = fixture();
  owner.show([0, 2]);
  owner.mark(new Set(["2"]), true);
  await arrive(0);
  expect(view().rows.get(2).track.cached).toBe(true);
  owner.mark(null, false);
  expect(view().rows.get(2).track.cached).toBe(false);
  owner.show([80]);
  owner.mark(new Set(["0", "80"]), true);
  owner.mark(new Set(["0"]), false);
  await arrive(1);
  expect(view().rows.get(80).track.cached).toBe(true);
  owner.show([0, 2]);
  await arrive(2, true);
  expect([...view().rows.values()].map((e) => e.track.cached)).toEqual([false, false]);
});

test("a stale failure does not poison the new viewport; retry keeps successful overlap", async () => {
  const { owner, requests, arrive, view } = fixture();
  owner.show([0]);
  await arrive(0);
  const retained = view().rows.get(0);
  owner.show([0, 1]);
  owner.show([0, 2]);
  requests[1].reject(new Error("old viewport"));
  await Promise.resolve();
  expect(view().error).toBe("");
  requests[2].reject(new Error("offline"));
  await Promise.resolve();
  expect(view().error).toContain("offline");
  owner.retry();
  expect(view().rows.get(0)).toBe(retained);
  expect(requests[3].positions).toEqual([2]);
  await arrive(3);
  expect(view().error).toBe("");
});

test("a response after the view is disposed cannot publish or launch another fetch", async () => {
  const { owner, requests, arrive, view } = fixture();
  owner.show([0, 1]);
  owner.show([2, 3]);
  const last = view();
  owner.dispose();
  await arrive(0);
  expect(view()).toBe(last);
  expect(view().rows.size).toBe(0);
  expect(requests).toHaveLength(1);
});
