import { expect, test } from "bun:test";
import { boundedReads } from "./cover-work.js";

const step = () => new Promise((resolve) => setTimeout(resolve, 0));

test("one key reads once and every caller gets that read's answer", async () => {
  let reads = 0;
  const read = boundedReads(
    async (value) => {
      reads += 1;
      return `${value}!`;
    },
    { max: 8, limit: 2 },
  );
  expect(await Promise.all([read("a", "a"), read("a", "a")])).toEqual(["a!", "a!"]);
  expect(reads).toBe(1);
  // A key that has already settled is answered from the memo.
  expect(await read("a", "a")).toBe("a!");
  expect(reads).toBe(1);
  expect(await read("b", "b")).toBe("b!");
  expect(reads).toBe(2);
});

test("reads run in the order asked for, never more than the limit at once", async () => {
  let active = 0;
  let peak = 0;
  const started = [];
  const read = boundedReads(
    async (value) => {
      active += 1;
      peak = Math.max(peak, active);
      started.push(value);
      await step();
      active -= 1;
      return value;
    },
    { max: 16, limit: 3 },
  );

  const asked = ["a", "b", "c", "d", "e", "f"];
  expect(await Promise.all(asked.map((value) => read(value, value)))).toEqual(asked);
  expect(peak).toBe(3);
  expect(started).toEqual(asked);
});

test("the memo keeps only its newest keys", async () => {
  let reads = 0;
  const read = boundedReads(
    async (value) => {
      reads += 1;
      return value;
    },
    { max: 2, limit: 1 },
  );

  await read("a", "a");
  await read("b", "b");
  await read("c", "c");
  expect(reads).toBe(3);
  // "a" fell out when "c" arrived, "c" did not.
  await read("a", "a");
  await read("c", "c");
  expect(reads).toBe(4);
});

test("a reader that throws still lets the queue drain", async () => {
  const read = boundedReads(
    (value) => {
      if (value === "bad") throw new Error("could not read");
      return Promise.resolve(value);
    },
    { max: 4, limit: 1 },
  );

  await expect(read("bad", "bad")).rejects.toThrow("could not read");
  expect(await read("good", "good")).toBe("good");
});

