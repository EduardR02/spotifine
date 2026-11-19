import { expect, test } from "bun:test";
import { decodeTrackWaveform, indexWaveform, traceWaveform } from "./track-waveform.js";

function payload(count, base64) {
  return { track_id: "track", duration_ms: count, interval_ms: 1, bin_count: count, peaks_base64: base64 };
}

test("decoded peaks preserve little-endian signed pairs and forgiving base64 padding", () => {
  const bytes = [0x00, 0x80, 0xff, 0x7f, 0x34, 0x92, 0xfe, 0xff];
  for (const count of [0, 1, 2]) {
    const encoded = btoa(String.fromCharCode(...bytes.slice(0, count * 4)));
    const unpadded = encoded.replace(/=+$/, "");
    for (const text of [encoded, unpadded, `\t${unpadded.slice(0, 2)}\n${unpadded.slice(2)} `]) {
      const result = decodeTrackWaveform(payload(count, text));
      expect(result.peaks).toEqual(new Int16Array([-32768, 32767, -28108, -2].slice(0, count * 2)));
      expect(result.bin_count).toBe(count);
    }
  }
});

test("malformed base64, invalid metadata and wrong payload sizes are rejected", () => {
  for (const text of ["AAAA?A==", "AAAAAA=", "AAAAAA===", "AAAA=A==", "AAAAAA==A"]) {
    expect(() => decodeTrackWaveform(payload(1, text))).toThrow();
  }
  for (const text of ["", "AAAA", "AAAAAAA="]) {
    expect(() => decodeTrackWaveform(payload(1, text))).toThrow("truncated");
  }
  for (const input of [null, payload(-1, ""), payload(1.5, ""), payload(0, null),
    { ...payload(0, ""), interval_ms: 2 }, { ...payload(0, ""), duration_ms: -1 }]) {
    expect(() => decodeTrackWaveform(input)).toThrow();
  }
});

function recordPath(width) {
  const coordinates = new Float64Array(width * 4);
  let i = 0;
  function point(x, y) { coordinates[i++] = x; coordinates[i++] = y; }
  return { coordinates, moveTo: point, lineTo: point };
}

// The original renderer scans every raw bin overlapped by a pixel. Comparing
// canvas coordinates checks fractional boundary rounding as well as the extrema.
function checkPath(waveform, index, width, center, start, end) {
  const actual = recordPath(width), expected = recordPath(width);
  traceWaveform(actual, index, width, center, start, end);
  const count = Math.min(waveform.bin_count, Math.floor(waveform.peaks.length / 2));
  const interval = waveform.interval_ms;
  const clamp = (n, min, max) => Math.min(max, Math.max(min, n));
  for (let x = 0; x < width; x++) {
    const from = clamp(Math.floor((start + (x / width) * (end - start)) / interval), 0, count - 1);
    const to = clamp(Math.ceil((start + ((x + 1) / width) * (end - start)) / interval), from + 1, count);
    let low = 32767, high = -32768;
    for (let bin = from; bin < to; bin++) {
      low = Math.min(low, waveform.peaks[bin * 2]);
      high = Math.max(high, waveform.peaks[bin * 2 + 1]);
    }
    expected.moveTo(x + 0.5, center - (high / 32768) * (center - 5));
    expected.lineTo(x + 0.5, center - (low / 32768) * (center - 5));
  }
  expect(actual.coordinates).toEqual(expected.coordinates);
}

test("intervals crossing 16/256 boundaries never borrow extrema from neighboring bins", () => {
  const count = 1043;
  const peaks = new Int16Array(count * 2);
  for (let bin = 0; bin < count; bin++) {
    // Both extrema move on every bin, so omitting or including just one edge
    // changes the result, even on intervals hundreds of bins wide.
    peaks[bin * 2] = -30000 + 23 * bin;
    peaks[bin * 2 + 1] = 500 + 17 * bin;
  }
  const waveform = { peaks, bin_count: count, interval_ms: 1 };
  for (const spiked of [false, true]) {
    if (spiked) {
      for (const bin of [15, 32, 254, 257, 511, 512, 769, 1024, count - 1]) {
        peaks[bin * 2 + (bin & 1)] = (bin & 1) ? 32767 : -32768;
      }
    }
    const index = indexWaveform(waveform);
    for (const start of [0, 1, 14, 15, 16, 17, 239, 254, 255, 256, 257, 1009, 1024, 1042]) {
      for (const length of [1, 15, 16, 17, 30, 31, 32, 240, 255, 256, 257, 271, 512, 1043]) {
        checkPath(waveform, index, 1, 100, start, start + length);
      }
    }
  }
});

test("overview and panned fractional zooms match raw paths on a four-minute envelope", () => {
  const count = 240013;
  const peaks = new Int16Array(count * 2);
  let noise = 9;
  for (let bin = 0; bin < count; bin++) {
    noise = (Math.imul(noise, 1664525) + 1013904223) >>> 0;
    const offset = (bin % 27001) - 13000;
    peaks[bin * 2] = offset - (noise & 8191);
    peaks[bin * 2 + 1] = offset + (noise >>> 19);
  }
  const waveform = { peaks, bin_count: count, interval_ms: 1 };
  const index = indexWaveform(waveform);
  expect(index.peaks).toBe(peaks);
  expect(index.blocks16.length).toBe(2 * Math.floor(count / 16));
  expect(index.blocks256.length).toBe(2 * Math.floor(count / 256));
  checkPath(waveform, index, 641, 18, 0, count);                  // overview
  checkPath(waveform, index, 965, 117.5, 0, count);               // main at fit
  checkPath(waveform, index, 731, 117.5, 137.875, 125678.125);    // medium zoom
  checkPath(waveform, index, 731, 117.5, 14327.25, 25906.625);    // ~16 bins / pixel
  checkPath(waveform, index, 965, 117.5, count - 250, count);     // sub-bin columns
});

test("empty input has no index; partial last blocks and shorter buffers are not zero padded", () => {
  expect(indexWaveform(null)).toBeNull();
  expect(indexWaveform({ peaks: new Int16Array(), bin_count: 0 })).toBeNull();
  for (const count of [1, 15, 16, 17, 255, 256, 257, 273]) {
    const peaks = new Int16Array(count * 2);
    for (let bin = 0; bin < count; bin++) {
      peaks[bin * 2] = bin === count - 1 ? -32000 : -128;
      peaks[bin * 2 + 1] = bin === count - 1 ? -1 : -16;
    }
    for (const declared of [count, count + 19, Math.max(1, count - 2)]) {
      const waveform = { peaks, bin_count: declared, interval_ms: 1 };
      const index = indexWaveform(waveform);
      checkPath(waveform, index, 1, 33, 0, count + 22);
      checkPath(waveform, index, 3, 33, count - 4.25, count + 22);
    }
  }
});
