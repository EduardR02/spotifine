import { applyCacheMarks } from "./cache-marks.js";

const titleOrder = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });

/** Positions are stable identities within one native snapshot, including ties. */
export function orderEpisodes(index, sort = "recent") {
  const positions = Array.from(index.keys());
  if (sort === "title") {
    return positions.sort((a, b) => titleOrder.compare(index[a][0], index[b][0]));
  }
  const direction = sort === "oldest" ? 1 : -1;
  return positions.sort((a, b) => {
    const left = index[a][1];
    const right = index[b][1];
    const aKnown = Number.isFinite(left);
    const bKnown = Number.isFinite(right);
    if (aKnown !== bKnown) return aKnown ? -1 : 1;
    return aKnown ? direction * (left - right) : 0;
  });
}

/** One request in flight per view. Keep the loaded overlap as the window moves;
    after a response, fetch only holes in the latest window, never scroll history. */
export function createEpisodeWindow(load, publish) {
  let wanted = new Set();
  const rows = new Map();
  const failed = new Map();
  let active = true;
  let loading = false;

  // Native keeps an immutable browse snapshot. A mark can change while a row
  // is offscreen, before its next read starts, not just while IPC is in flight.
  let reset = false;
  const added = new Set();
  const removed = new Set();

  const notify = () => publish({ rows, error: failed.values().next().value ?? "" });
  async function fill() {
    if (!active || loading) return;
    const missing = [...wanted].filter((position) => !rows.has(position) && !failed.has(position));
    if (!missing.length) return;
    loading = true;
    try {
      const { episodes } = await load(missing);
      if (!active) return;
      if (episodes.length !== missing.length || episodes.some((e) => !e?.track?.id)) {
        throw new Error("Spotify returned incomplete episode rows.");
      }
      if (reset || removed.size || added.size) {
        const tracks = episodes.map((e) => e.track);
        if (reset) applyCacheMarks(tracks, null);
        if (removed.size) applyCacheMarks(tracks, removed, false);
        if (added.size) applyCacheMarks(tracks, added);
      }
      missing.forEach((position, i) => {
        if (wanted.has(position)) rows.set(position, episodes[i]);
      });
    } catch (reason) {
      if (!active) return;
      for (const position of missing) {
        if (wanted.has(position)) failed.set(position, String(reason));
      }
    } finally {
      loading = false;
      if (active) {
        notify();
        fill();
      }
    }
  }
  return {
    show(positions) {
      wanted = new Set(positions);
      for (const position of rows.keys()) if (!wanted.has(position)) rows.delete(position);
      for (const position of failed.keys()) if (!wanted.has(position)) failed.delete(position);
      notify();
      fill();
    },
    retry() {
      failed.clear();
      notify();
      fill();
    },
    mark(ids, cached = true) {
      if (ids === null) {
        reset = true;
        added.clear();
        removed.clear();
      } else {
        for (const id of ids) {
          (cached ? added : removed).add(id);
          (cached ? removed : added).delete(id);
        }
      }
      applyCacheMarks(Array.from(rows.values(), (e) => e.track), ids, cached);
    },
    dispose() {
      active = false;
      rows.clear();
      failed.clear();
      wanted.clear();
      added.clear();
      removed.clear();
    },
  };
}
