import { expect, test } from "bun:test";
import { panelLayout, snapPanel, PANE_MIN, PANEL_MIN } from "./layout.js";

/* The shell's numbers: an 8px gutter, and the rail at 240px above 1180px
   and 212px at or below it (app.css), so the pane starts 16px further in. */
const at = (width, height, extra = {}) =>
  panelLayout({ width, height, start: 16 + (width > 1180 ? 240 : 212), gutter: 8, dpr: 1, ...extra });
const pane = (width, box) => width - (16 + (width > 1180 ? 240 : 212)) - 8 - box;

test("the default window keeps the track table's columns and the panel", () => {
  const box = at(1196, 805);
  expect(box.fits).toBe(true);
  expect(pane(1196, box.width)).toBe(PANE_MIN);
  expect(box.width).toBe(292);
  expect(box.max).toBe(292);
  expect(box.min).toBe(PANEL_MIN);
});

test("a wide window gets the Canvas at its natural width, not a third", () => {
  const box = at(1920, 1080);
  expect(box.width).toBe(607); // 1080 × 9 / 16, on the pixel grid
  expect(box.max).toBe(607.5);
  expect(pane(1920, box.width)).toBeGreaterThan(PANE_MIN);
});

test("a short window caps the panel at a third when that is less", () => {
  const box = at(1500, 1000);
  expect(box.width).toBe(500); // 1500 / 3 < 562.5
});

test("never wider than a Canvas can fill sharply", () => {
  const box = at(3840, 2100, { dpr: 1.5 });
  expect(box.max).toBe(576); // 720 × 1.2 / 1.5
  expect(box.width).toBe(576);
});

test("a dragged width is kept inside its range", () => {
  expect(at(1920, 1080, { user: 400 }).width).toBe(400);
  expect(at(1920, 1080, { user: 900 }).width).toBe(607);
  expect(at(1920, 1080, { user: 100 }).width).toBe(PANEL_MIN);
  // On a smaller window the pane's minimum wins over the dragged width.
  expect(at(1196, 805, { user: 400 }).width).toBe(292);
});

test("the panel steps aside when even its narrowest would squeeze the pane", () => {
  expect(at(1181, 800).fits).toBe(true); // the wide rail's last pixel
  expect(at(1180, 800).fits).toBe(true);
  expect(at(1148, 800).fits).toBe(true);
  expect(at(1147, 800).fits).toBe(false);
  expect(at(940, 600).fits).toBe(false);
});

test("widths land on whole device pixels without passing the limit", () => {
  expect(snapPanel(292.9, 1)).toBe(292);
  expect(snapPanel(292.9, 1.25)).toBe(292.8);
  expect(snapPanel(400, 1.5)).toBe(400);
  const box = at(1197, 805, { dpr: 1.25 });
  expect(box.width).toBeLessThanOrEqual(box.max);
  expect((box.width * 1.25) % 1).toBe(0);
});
