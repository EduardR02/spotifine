function decodePeaks(base64, binCount) {
  // Older macOS WebViews have atob but not the native typed-array decoder.
  const bytes = Uint8Array.fromBase64 ? Uint8Array.fromBase64(base64) : atob(base64);
  if (bytes.length !== binCount * 4) throw new Error("The waveform payload was truncated.");
  if (typeof bytes !== "string") {
    // Both supported targets (Windows x64 and Apple Silicon) are little-endian.
    return new Int16Array(bytes.buffer, bytes.byteOffset, binCount * 2);
  }
  const peaks = new Int16Array(binCount * 2);
  for (let i = 0; i < peaks.length; i++) {
    peaks[i] = bytes.charCodeAt(i * 2) | (bytes.charCodeAt(i * 2 + 1) << 8);
  }
  return peaks;
}

export function decodeTrackWaveform(payload) {
  if (!payload || typeof payload !== "object") throw new Error("The waveform response was empty.");
  const binCount = Number(payload.bin_count);
  const interval = Number(payload.interval_ms);
  const duration = Number(payload.duration_ms);
  if (!Number.isInteger(binCount) || binCount < 0 || interval !== 1 ||
      !Number.isInteger(duration) || duration < 0 || typeof payload.peaks_base64 !== "string") {
    throw new Error("The waveform response was invalid.");
  }
  return {
    track_id: payload.track_id,
    duration_ms: duration,
    interval_ms: interval,
    bin_count: binCount,
    peaks: decodePeaks(payload.peaks_base64, binCount),
  };
}

// Index only complete blocks. The next finer level always has the exact tail.
function summarize16(pairs, count) {
  const blocks = new Int16Array(Math.floor(count / 16) * 2);
  for (let dst = 0; dst < blocks.length; dst += 2) {
    let low = 32767, high = -32768;
    const end = (dst + 2) * 16;
    for (let i = dst * 16; i < end; i += 2) {
      low = Math.min(low, pairs[i]);
      high = Math.max(high, pairs[i + 1]);
    }
    blocks[dst] = low;
    blocks[dst + 1] = high;
  }
  return blocks;
}

export function indexWaveform(waveform) {
  const peaks = waveform?.peaks;
  const count = Math.min(Number(waveform?.bin_count) || 0, peaks ? Math.floor(peaks.length / 2) : 0);
  if (!count) return null;
  const interval = Math.max(1, Number(waveform.interval_ms) || 1);
  const blocks16 = summarize16(peaks, count);
  const blocks256 = summarize16(blocks16, blocks16.length / 2);
  return { peaks, count, interval, blocks16, blocks256 };
}

export function traceWaveform(context, index, width, center, start, end) {
  const { peaks, count, interval, blocks16, blocks256 } = index;
  const span = end - start;
  for (let x = 0; x < width; x++) {
    const from = Math.min(count - 1, Math.max(0, Math.floor((start + (x / width) * span) / interval)));
    const to = Math.min(count, Math.max(from + 1, Math.ceil((start + ((x + 1) / width) * span) / interval)));
    let low = 32767, high = -32768, bin = from;
    if (to - from >= 16) {
      // Enter each coarser level only at an aligned boundary. Only consume
      // whole blocks inside this pixel's [from, to), never a neighbor's spike.
      for (; bin & 15; bin++) {
        low = Math.min(low, peaks[bin * 2]);
        high = Math.max(high, peaks[bin * 2 + 1]);
      }
      for (; (bin & 255) && bin + 16 <= to; bin += 16) {
        const i = (bin >>> 4) * 2;
        low = Math.min(low, blocks16[i]);
        high = Math.max(high, blocks16[i + 1]);
      }
      for (; bin + 256 <= to; bin += 256) {
        const i = (bin >>> 8) * 2;
        low = Math.min(low, blocks256[i]);
        high = Math.max(high, blocks256[i + 1]);
      }
      for (; bin + 16 <= to; bin += 16) {
        const i = (bin >>> 4) * 2;
        low = Math.min(low, blocks16[i]);
        high = Math.max(high, blocks16[i + 1]);
      }
    }
    // At fine zoom this is the entire query; no summary reads or allocations.
    for (; bin < to; bin++) {
      low = Math.min(low, peaks[bin * 2]);
      high = Math.max(high, peaks[bin * 2 + 1]);
    }
    context.moveTo(x + 0.5, center - (high / 32768) * (center - 5));
    context.lineTo(x + 0.5, center - (low / 32768) * (center - 5));
  }
}
