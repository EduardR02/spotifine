import { expect, test } from "bun:test";
import { localDayRelation } from "./history-date.js";

test("relative history labels follow local dates across month and year boundaries", () => {
  const now = new Date(2026, 0, 1, 0, 15).getTime();
  expect(localDayRelation(new Date(2026, 0, 1, 23, 59).getTime(), now)).toBe("Today");
  expect(localDayRelation(new Date(2025, 11, 31, 0, 1).getTime(), now)).toBe("Yesterday");
  expect(localDayRelation(new Date(2025, 11, 30, 23, 59).getTime(), now)).toBe("");
});

test("Yesterday survives both daylight-saving clock changes", () => {
  // The child has a fixed local timezone without changing the test runner's timezone.
  const moduleUrl = new URL("./history-date.js", import.meta.url).href;
  const result = Bun.spawnSync([process.execPath, "--eval", `
    import { localDayRelation } from ${JSON.stringify(moduleUrl)};
    const cases = [
      [new Date(2026, 2, 8, 12), new Date(2026, 2, 9, 12)],
      [new Date(2026, 10, 1, 12), new Date(2026, 10, 2, 12)],
    ];
    console.log(JSON.stringify(cases.map(([entry, now]) => localDayRelation(entry.getTime(), now.getTime()))));
  `], { env: { ...process.env, TZ: "America/New_York" } });
  expect(result.exitCode).toBe(0);
  expect(JSON.parse(new TextDecoder().decode(result.stdout))).toEqual(["Yesterday", "Yesterday"]);
});
