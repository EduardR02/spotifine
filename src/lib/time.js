/** m:ss / h:mm:ss from milliseconds. */
export function formatTime(ms) {
  if (!Number.isFinite(ms) || ms < 0) ms = 0;
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** Exact editor time with millisecond precision. */
export function formatExactTime(ms) {
  const value = Math.max(0, Math.round(Number(ms) || 0));
  const totalSeconds = Math.floor(value / 1000);
  const milliseconds = String(value % 1000).padStart(3, "0");
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = String(totalSeconds % 60).padStart(2, "0");
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}.${milliseconds}`
    : `${minutes}:${seconds}.${milliseconds}`;
}

/**
 * Parses decimal seconds, m:ss.mmm, or h:mm:ss.mmm into integer milliseconds.
 * Returns null for partial, negative, or out-of-radix values.
 */
export function parseExactTime(value) {
  const source = String(value ?? "").trim();
  if (!source) return null;
  if (!source.includes(":")) {
    if (!/^(?:\d+\.?\d*|\.\d+)$/.test(source)) return null;
    const seconds = Number(source);
    return Number.isFinite(seconds) ? Math.round(seconds * 1000) : null;
  }
  const parts = source.split(":");
  if (parts.length !== 2 && parts.length !== 3) return null;
  if (!parts.every((part) => /^\d+(?:\.\d+)?$/.test(part))) return null;
  const seconds = Number(parts.at(-1));
  const minutes = Number(parts.at(-2));
  if (!Number.isFinite(seconds) || !Number.isInteger(minutes) || seconds >= 60) return null;
  if (parts.length === 2) return Math.round((minutes * 60 + seconds) * 1000);
  const hours = Number(parts[0]);
  if (!Number.isInteger(hours) || minutes >= 60) return null;
  return Math.round((hours * 3600 + minutes * 60 + seconds) * 1000);
}

/** Human-readable binary byte size for cache and diagnostics surfaces. */
export function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = -1;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const decimals = value >= 10 || Number.isInteger(value) ? 0 : 1;
  return `${value.toFixed(decimals)} ${units[unit]}`;
}

/**
 * Summed duration of a track list, as "8 hr 12 min" or "38 min 12 sec".
 *
 * Under an hour it keeps seconds: a five-track playlist rounded to whole
 * minutes threw away most of what the number was telling you.
 */
export function formatTotal(tracks) {
  const ms = tracks.reduce((sum, t) => sum + (t?.duration_ms || 0), 0);
  if (!Number.isFinite(ms) || ms <= 0) return "0 sec";
  const total = Math.round(ms / 1000);
  if (total >= 3600) {
    const totalMinutes = Math.round(total / 60);
    const h = Math.floor(totalMinutes / 60);
    const m = totalMinutes % 60;
    return m > 0 ? `${h} hr ${m} min` : `${h} hr`;
  }
  const m = Math.floor(total / 60);
  const s = total % 60;
  if (m === 0) return `${s} sec`;
  return s > 0 ? `${m} min ${s} sec` : `${m} min`;
}
