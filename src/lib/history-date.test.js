import { expect, test } from "bun:test";
import { localDayRelation } from "./history-date.js";

test("relative history labels follow local dates across month and year boundaries", () => {
  const now = new Date(2026, 0, 1, 0, 15).getTime();
  const today = localDayRelation(new Date(2025, 5, 15, 12).getTime(), new Date(2025, 5, 15, 18).getTime());
  const yesterday = localDayRelation(new Date(2025, 5, 14, 12).getTime(), new Date(2025, 5, 15, 18).getTime());
  expect(today).not.toBe(yesterday);
  expect(localDayRelation(new Date(2026, 0, 1, 23, 59).getTime(), now)).toBe(today);
  expect(localDayRelation(new Date(2025, 11, 31, 0, 1).getTime(), now)).toBe(yesterday);
  const older = localDayRelation(new Date(2025, 11, 30, 23, 59).getTime(), now);
  expect(older).not.toBe(today);
  expect(older).not.toBe(yesterday);
});

test("the prior-day relation survives both daylight-saving clock changes", () => {
  // The child has a fixed local timezone without changing the test runner's timezone.
  const moduleUrl = new URL("./history-date.js", import.meta.url).href;
  const result = Bun.spawnSync([process.execPath, "--eval", `
    import { localDayRelation } from ${JSON.stringify(moduleUrl)};
    const cases = [
      [new Date(2026, 2, 8, 12), new Date(2026, 2, 9, 12)],
      [new Date(2026, 10, 1, 12), new Date(2026, 10, 2, 12)],
    ];
    const yesterday = localDayRelation(new Date(2026, 0, 15, 12).getTime(), new Date(2026, 0, 16, 12).getTime());
    console.log(JSON.stringify({ yesterday, labels: cases.map(([entry, now]) => localDayRelation(entry.getTime(), now.getTime())) }));
  `], { env: { ...process.env, TZ: "America/New_York" } });
  expect(result.exitCode).toBe(0);
  const { yesterday, labels } = JSON.parse(new TextDecoder().decode(result.stdout));
  expect(labels).toEqual([yesterday, yesterday]);
});
