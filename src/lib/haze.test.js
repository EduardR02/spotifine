import { expect, test } from "bun:test";
import {
  renderHaze,
  gradeLab,
  cropBorders,
  lightOf,
  framing,
  signatureOf,
  sigDistance,
  medoid,
  isBlank,
  solidSource,
  luminance,
  toOklab,
  fromOklab,
  frostOf,
  parseFrost,
  CEILING,
  RESTRAINED,
  SETTLE_DISTANCE,
} from "./haze.js";

/* A Canvas-shaped source (9:16) and a small haze of a 16:9 window. */
const SW = 45;
const SH = 80;
const OPEN = { w: 160, h: 90, left: 0.62, immersive: true };
const COVER = { ...OPEN, immersive: false };
/** A Canvas on screen: the video fills the panel. */
const VIDEO = { ...OPEN, video: { x: 99.2, y: 0, w: 60.8, h: 90 } };
const CLOSED = { w: 160, h: 90, left: null, immersive: false };
/** The record's colour for these tests: a rose glow. */
const ROSE = [0.92, 0.62, 0.66];

function source(pixel, w = SW, h = SH) {
  const data = new Uint8ClampedArray(w * h * 4);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) data.set([...pixel(x, y), 255], (y * w + x) * 4);
  }
  return { data, w, h };
}

const render = (src, geo = OPEN, seed = 1) =>
  renderHaze(src, geo, ROSE, { scale: geo.left == null ? RESTRAINED : 1, seed });
const at = (out, geo, x, y) => {
  const i = (y * geo.w + x) * 4;
  return [out.rgba[i] / 255, out.rgba[i + 1] / 255, out.rgba[i + 2] / 255];
};
const lumAt = (out, geo, x, y) => luminance(...at(out, geo, x, y));
const hueOf = (rgb) => {
  const [, a, b] = toOklab(...rgb);
  return (Math.atan2(b, a) * 180) / Math.PI;
};
const chromaOf = (rgb) => {
  const [, a, b] = toOklab(...rgb);
  return Math.hypot(a, b);
};
const hueNear = (h, target, tol) => Math.abs(((h - target + 540) % 360) - 180) < tol;
const seam = (geo) => Math.round(geo.left * geo.w);

const BLUE = [30, 70, 200];
const RED = [210, 50, 30];

/* ---- Tone ------------------------------------------------------------ */

test("a colour keeps its own hue: blue stays blue and red stays red", () => {
  const tint = toOklab(...ROSE);
  for (const [rgb, hue] of [[BLUE, -95], [RED, 30]]) {
    const [L, a, b] = gradeLab(toOklab(...rgb.map((v) => v / 255)), tint);
    expect(Math.hypot(a, b)).toBeGreaterThan(0.08);
    expect(hueNear((Math.atan2(b, a) * 180) / Math.PI, hue, 25)).toBe(true);
    expect(luminance(...fromOklab(L, a, b))).toBeLessThanOrEqual(CEILING.peak);
  }
});

test("a dark colour is lifted: shadow still glows, as colour", () => {
  const src = [20, 28, 60].map((v) => v / 255);
  const [L, a, b] = gradeLab(toOklab(...src), toOklab(...ROSE));
  expect(L).toBeGreaterThan(toOklab(...src)[0] * 1.1);
  expect(Math.hypot(a, b)).toBeGreaterThan(0.05);
});

/* ---- The ceilings ------------------------------------------------------ */

const SOURCES = {
  white: () => [255, 255, 255],
  yellow: () => [255, 230, 40],
  green: () => [60, 255, 90],
  cyan: () => [80, 255, 255],
  ramp: (x, y) => [x * 5, 255 - y * 3, (x * y) % 256],
  checker: (x, y) => ((x >> 2) + (y >> 2)) % 2 ? [255, 255, 255] : [255, 40, 200],
  // Bright subjects on black: the peak is reached while the mean stays low,
  // so only the per-pixel ceiling holds them.
  spot: (x, y) => (x > 4 && x < 30 && y > 10 && y < 50 ? [255, 235, 60] : [0, 0, 0]),
  stripes: (x) => (x % 12 < 5 ? [255, 255, 255] : [0, 0, 0]),
  lime: (x, y) => (y > 20 && y < 60 ? [150, 255, 60] : [2, 2, 2]),
  // White beside black: the window reaches the peak right at the panel.
  bright: (x) => (x > 30 ? [255, 255, 255] : [0, 0, 0]),
};

test("no pixel of the haze is brighter than the peak, after dithering, whatever the source", () => {
  for (const [name, pixel] of Object.entries(SOURCES)) {
    for (const [geo, cap] of [[OPEN, CEILING.peak], [COVER, CEILING.peak], [CLOSED, CEILING.peak * RESTRAINED], [VIDEO, CEILING.peak]]) {
      const out = render(source(pixel), geo);
      // Beside a video the seam is the video's own light (see below); the
      // rest of the window holds the peak.
      const x1 = geo.video ? Math.floor(geo.video.x - 44 / 5) : geo.w;
      let worst = 0;
      for (let y = 0; y < geo.h; y++) for (let x = 0; x < x1; x++) worst = Math.max(worst, lumAt(out, geo, x, y));
      if (worst > cap) throw new Error(`${name}: ${worst} over ${cap}`);
    }
  }
});

test("a white Canvas is a dim haze in the record's colour, and a dark window", () => {
  const geo = COVER; // the haze is seen everywhere, under the panel too
  const out = render(source(SOURCES.white), geo);
  let sum = 0;
  for (let y = 0; y < geo.h; y++) for (let x = 0; x < geo.w; x++) sum += lumAt(out, geo, x, y);
  expect(sum / (geo.w * geo.h)).toBeLessThanOrEqual(CEILING.mean * 1.02);
  const [r, g, b] = at(out, geo, 150, 45);
  expect(r).toBeGreaterThan(g + 0.03);
  expect(r).toBeGreaterThan(b + 0.02);
  expect(chromaOf([r, g, b])).toBeGreaterThan(0.04);
});

test("pale warm light is warm graphite, not sienna; fire stays fire", () => {
  // Cream and sand, as a pale Canvas in soft light is.
  for (const pale of [[236, 222, 196], [214, 190, 150]]) {
    const rgb = at(render(source(() => pale), COVER), COVER, 150, 45);
    expect(chromaOf(rgb)).toBeLessThan(0.03);
    expect(rgb[0]).toBeGreaterThanOrEqual(rgb[2]); // a breath of warmth, not grey-blue
  }
  // A genuinely saturated orange keeps its colour: a whole window of it is
  // held to the mean chroma budget, and it is still several times the cream.
  const fire = at(render(source(() => [235, 110, 20]), COVER), COVER, 150, 45);
  expect(chromaOf(fire)).toBeGreaterThan(0.06);
  expect(hueNear(hueOf(fire), 45, 15)).toBe(true);
  // And a red is untouched by it.
  const red = at(render(source(() => [205, 40, 40]), COVER), COVER, 150, 45);
  expect(chromaOf(red)).toBeGreaterThan(0.06);
});

test("the panel-closed glow is restrained", () => {
  const src = source(() => [220, 130, 90]);
  const mean = (geo) => {
    const out = render(src, geo);
    let sum = 0;
    for (let y = 0; y < geo.h; y++) for (let x = 0; x < geo.w; x++) sum += lumAt(out, geo, x, y);
    return sum / (geo.w * geo.h);
  };
  expect(mean(CLOSED)).toBeLessThan(mean(COVER) * 0.75);
});

test("a picture that cannot be read is the record's own colour, under the ceiling", () => {
  const out = render(solidSource(ROSE));
  const rgb = at(out, OPEN, 80, 45);
  expect(luminance(...rgb)).toBeLessThanOrEqual(CEILING.peak);
  expect(rgb[0]).toBeGreaterThan(rgb[1]);
});

/* ---- The layout -------------------------------------------------------- */

test("the haze is the panel's picture carried on out of the panel, magnified", () => {
  // Blue on the left of the frame, red on the right.
  const out = render(source((x) => (x < SW / 2 ? BLUE : RED)));
  const s = seam(OPEN);
  // Just outside the panel: the frame's left edge, blue, continuous.
  expect(hueNear(hueOf(at(out, OPEN, s - 2, 45)), -95, 30)).toBe(true);
  expect(hueNear(hueOf(at(out, OPEN, s + 2, 45)), -95, 30)).toBe(true);
  // Under the panel's right side, the frame's right side: red.
  expect(hueNear(hueOf(at(out, OPEN, OPEN.w - 4, 45)), 30, 30)).toBe(true);
  // Mirrored and magnified: further out, the frame's right half comes round.
  const panelW = OPEN.w - s;
  expect(hueNear(hueOf(at(out, OPEN, Math.round(s - panelW * 1.3), 45)), 30, 35)).toBe(true);
});

test("a patchy frame is a patchy haze, where the frame has its patches", () => {
  // Four quadrants of distinct colour.
  const quad = (x, y) => (y < SH / 2 ? (x < SW / 2 ? BLUE : RED) : x < SW / 2 ? [40, 170, 70] : [190, 40, 180]);
  const out = render(source(quad), COVER);
  const x0 = seam(COVER);
  const q = (fx, fy) => hueOf(at(out, COVER, Math.round(x0 + (COVER.w - x0) * fx), Math.round(COVER.h * fy)));
  expect(hueNear(q(0.2, 0.2), -95, 35)).toBe(true);
  expect(hueNear(q(0.8, 0.2), 30, 35)).toBe(true);
  expect(hueNear(q(0.2, 0.8), 145, 35)).toBe(true);
  expect(hueNear(q(0.8, 0.8), -30, 40)).toBe(true);
});

test("the light is strongest at the panel and falls off to the far edge, as colour", () => {
  const out = render(source(() => [40, 90, 210]));
  const s = seam(OPEN);
  expect(lumAt(out, OPEN, s - 2, 45)).toBeGreaterThan(lumAt(out, OPEN, 3, 45) * 1.5);
  expect(chromaOf(at(out, OPEN, 3, 45))).toBeGreaterThan(0.04);
});

test("a cover's light is a pool at its tile's height under the panel, and the panes keep theirs", () => {
  const flat = source(() => [40, 90, 210]);
  const out = render(flat, COVER);
  const s = seam(COVER);
  // Under the panel: brightest at the tile's height, quieter toward its foot and its head.
  const x = Math.round((s + COVER.w) / 2);
  expect(lumAt(out, COVER, x, 40)).toBeGreaterThan(lumAt(out, COVER, x, 86) * 1.3);
  expect(lumAt(out, COVER, x, 40)).toBeGreaterThan(lumAt(out, COVER, x, 3) * 1.15);
  // At the window's far side the pool has dissolved: a column is one light, top to bottom.
  const far = (y) => lumAt(out, COVER, 3, y) + lumAt(out, COVER, 4, y) + lumAt(out, COVER, 5, y);
  expect(Math.abs(far(40) - far(86)) / far(40)).toBeLessThan(0.08);
  // A Canvas fills the panel and has no pool: its light does not fall off down a column.
  const canvas = render(flat, OPEN);
  const beside = (y) => lumAt(canvas, OPEN, s - 3, y) + lumAt(canvas, OPEN, s - 4, y) + lumAt(canvas, OPEN, s - 5, y);
  expect(Math.abs(beside(40) - beside(86)) / beside(40)).toBeLessThan(0.08);
});

test("the haze lies under the video exactly as it is shown, and carries on from its edge", () => {
  // A video that floats in the panel, narrower and shorter than it.
  const geo = { ...OPEN, video: { x: 112, y: 12, w: 36, h: 64 } };
  const out = render(source((x) => (x < SW / 2 ? BLUE : RED)), geo);
  expect(hueNear(hueOf(at(out, geo, 116, 45)), -95, 30)).toBe(true);
  expect(hueNear(hueOf(at(out, geo, 144, 45)), 30, 30)).toBe(true);
  // Left of its edge (past the seam), its own left side carries on.
  expect(hueNear(hueOf(at(out, geo, 100, 45)), -95, 30)).toBe(true);
});

test("the seam is the video's own light, and the glass carries it in only at its edge", () => {
  // A light blue Canvas, far brighter than the haze may be.
  const sky = [120, 170, 240];
  const out = renderHaze(source(() => sky), VIDEO, ROSE, { frost: parseFrost("blur(12px) saturate(1.45) brightness(0.7)", 5) });
  const edge = Math.floor(VIDEO.video.x);
  const skyY = luminance(...sky.map((v) => v / 255));
  // In the gap: nearly the video itself, so the edge does not step to dark.
  expect(lumAt(out, VIDEO, edge - 1, 45)).toBeGreaterThan(skyY * 0.6);
  // Past the seam: the graded haze, under the peak.
  expect(lumAt(out, VIDEO, edge - 12, 45)).toBeLessThanOrEqual(CEILING.peak);
  // The frost, by how far its pixel lies left of the video's edge (CSS px;
  // the glass begins 8px out, and type 18px further in at the least).
  const frostAt = (x, y) => [0, 1, 2].map((c) => out.frost[(y * out.frostW + x) * 4 + c] / 255);
  const cssOut = (x) => (VIDEO.video.x - (2 * x + 1)) * 5;
  const under = (css) => Math.floor((VIDEO.video.x - css / 5) / 2);
  // Just under the glass's edge it is lit by the video, in its colour...
  const lit = frostAt(under(12), 22);
  expect(luminance(...lit)).toBeGreaterThan(luminance(...frostAt(under(80), 22)) * 1.5);
  expect(hueNear(hueOf(lit), hueOf(sky.map((v) => v / 255)), 25)).toBe(true);
  // ...and wherever type can lie, the glass is calibrated against the peak.
  let worst = 0;
  for (let y = 0; y < out.frostH; y++) {
    for (let x = 0; x < out.frostW && cssOut(x) >= 8 + 16; x++) worst = Math.max(worst, luminance(...frostAt(x, y)));
  }
  expect(worst).toBeLessThanOrEqual(CEILING.peak);
});

/* ---- Quantisation ------------------------------------------------------- */

test("a flat colour is dithered between its two nearest levels, not rounded to one", () => {
  const s0 = seam(OPEN) + 4;
  for (const flat of [[90, 80, 110], [70, 95, 100], [110, 85, 80]]) {
    const out = render(source(() => flat));
    for (let c = 0; c < 3; c++) {
      const levels = new Set();
      let sum = 0, n = 0;
      for (let y = 0; y < OPEN.h; y++) {
        for (let x = s0; x < OPEN.w; x++) {
          const v = out.rgba[(y * OPEN.w + x) * 4 + c];
          levels.add(v);
          sum += v;
          n++;
        }
      }
      const [lo, hi] = [...levels].sort((p, q) => p - q);
      expect(levels.size).toBe(2);
      expect(hi - lo).toBe(1);
      expect(sum / n).toBeGreaterThan(lo);
      expect(sum / n).toBeLessThan(hi);
    }
  }
});

test("a gradient stretched over the window has no bands", () => {
  // The falloff from the panel's edge is a smooth gradient, the same down
  // every column: rounded, each column would be one level and the levels
  // would step in plateaus; dithered, the column means follow the curve.
  const out = render(source(() => [60, 70, 110]));
  const x1 = seam(OPEN) - 2;
  for (let c = 0; c < 3; c++) {
    const cols = [];
    for (let x = 0; x < x1; x++) {
      let sum = 0;
      for (let y = 0; y < OPEN.h; y++) sum += out.rgba[(y * OPEN.w + x) * 4 + c];
      cols.push(sum / OPEN.h);
    }
    for (let x = 2; x < cols.length - 2; x++) {
      const line = (cols[x - 2] + cols[x - 1] + cols[x + 1] + cols[x + 2]) / 4;
      expect(Math.abs(cols[x] - line)).toBeLessThan(0.3);
    }
  }
});

/* ---- The frost ----------------------------------------------------------- */

test("the frost is what CSS blur(), saturate() and brightness() make of the haze", () => {
  const f = parseFrost("blur(36px) saturate(1.45) brightness(0.7)", 5);
  expect(f).toEqual({ sigma: 7.2, saturate: 1.45, brightness: 0.7 });
  expect(parseFrost("none", 5)).toBe(null);
  // A flat field, so the blur is the identity and only the colour matrix shows.
  const w = 40, h = 24, rgb = [0.3, 0.12, 0.2];
  const encoded = new Float32Array(w * h * 3).map((_, i) => rgb[i % 3]);
  const out = frostOf(encoded, w, h, f);
  const s = f.saturate;
  const m = [
    [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
    [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
    [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
  ];
  const expected = m.map((row) => Math.min(1, Math.max(0, row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2])) * f.brightness * 255);
  for (let c = 0; c < 3; c++) {
    let sum = 0;
    for (let p = 0; p < w * h; p++) sum += out[p * 4 + c];
    // Dithered: the mean is the exact value, within a fraction of a level.
    expect(Math.abs(sum / (w * h) - expected[c])).toBeLessThan(0.15);
  }
});

/* ---- Reading the source --------------------------------------------------- */

test("a black frame around the picture is cropped away", () => {
  // A 16:9 picture letterboxed in the middle of a 9:16 frame.
  const src = source((x, y) => (y > 27 && y < 53 ? [200, 60, 40] : [4, 4, 4]));
  const [x0, y0, x1, y1] = cropBorders(src.data, SW, SH);
  expect([x0, x1]).toEqual([0, SW]);
  expect([y0, y1]).toEqual([28, 53]);
  // So the haze is the picture's red, not black, top to bottom.
  const out = render(src);
  for (const y of [2, 45, 87]) expect(chromaOf(at(out, OPEN, OPEN.w - 10, y))).toBeGreaterThan(0.06);
});

test("the type's light is the bright end of the video under it, and of the haze over a cover", () => {
  // White across the lower part of the frame, black above.
  const src = source((x, y) => (y > SH * 0.62 ? [255, 255, 255] : [0, 0, 0]));
  const video = lightOf(src, framing(src, OPEN).shown);
  expect(video.bottom).toBeGreaterThan(0.95);
  expect(video.top).toBeLessThan(0.01);
  expect(render(src, OPEN).light.bottom).toBeGreaterThan(0.95);
  // Over a cover the type sits on the haze, which is under the peak.
  const cover = render(src, COVER).light;
  expect(cover.bottom).toBeLessThanOrEqual(CEILING.peak);
  expect(cover.bottom).toBeGreaterThan(0);
});

test("a black frame is blank; a dark frame with a light in it is not", () => {
  expect(isBlank(source(() => [3, 3, 5]))).toBe(true);
  expect(isBlank(source((x, y) => (x === 20 && y === 40 ? [180, 160, 90] : [3, 3, 5])))).toBe(false);
});

test("the loop's most typical frame is found, and only a different picture counts as a change", () => {
  const rect = [0, 0, SW, SH];
  const a = signatureOf(source((x) => (x < SW / 2 ? BLUE : RED)), rect);
  const a2 = signatureOf(source((x) => (x < SW / 2 + 2 ? BLUE : RED)), rect);
  const a3 = signatureOf(source((x) => (x < SW / 2 - 1 ? BLUE : RED)), rect);
  const b = signatureOf(source(() => [40, 170, 70]), rect);
  expect(sigDistance(a, a2)).toBeLessThan(SETTLE_DISTANCE);
  expect(sigDistance(a, b)).toBeGreaterThan(SETTLE_DISTANCE);
  // A first frame from a brief green cut; the loop is mostly blue and red.
  expect(medoid([b, a, a2, a3])).not.toBe(0);
});
