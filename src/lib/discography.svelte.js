import { untrack } from "svelte";
import { loadCataloguePage, watchCacheMarks } from "./state.svelte.js";
import { applyCacheMarks } from "./cache-marks.js";

/**
 * What the artist page's shelf and the discography reader agree on.
 *
 * The two surfaces show the same catalogue at two depths — one row of covers
 * on the artist page, every track of every record in the reader — and the
 * toggle above them is the SAME control. It carries across the navigation, so
 * its definition cannot live in either view.
 */

/**
 * `types` is the exact array the engine pages on, so a selection is not a
 * client-side filter over a fetched list — it is a different query, and
 * "Compilations" costs one page of compilations rather than a walk through
 * everything looking for them.
 */
export const GROUPS = [
  { id: "all", label: "All", types: ["albums", "singles", "compilations"] },
  { id: "albums", label: "Albums", types: ["albums"] },
  { id: "singles", label: "Singles & EPs", types: ["singles"] },
  { id: "compilations", label: "Compilations", types: ["compilations"] },
];

/** The three main-catalogue keys `release_counts` carries. Appears-on is
 * intentionally a separate artist surface, not a discography filter. */
export const RELEASE_KEYS = ["albums", "singles", "compilations"];

/**
 * The payload does not label a release, so the track count does — Spotify's
 * own rule, and it is right often enough to be useful. Summaries on the shelf
 * carry no track list at all, so there the group the release came from is the
 * better answer and this is only asked in the reader.
 */
export function releaseKind(release) {
  const count = release.tracks?.length ?? 0;
  if (count === 1) return "Single";
  if (count <= 6) return "EP";
  return "Album";
}

/**
 * One paginated catalogue reader for the two surfaces that walk an artist's
 * catalogue a page at a time — the discography reader (four mixed releases)
 * and Appears On (six summaries). Both used to carry private copies of the
 * same machine: offset bookkeeping, generation guards against answers that
 * outlive the route, dedupe on append, the refill-on-key-change effect and
 * the near-bottom scroll hook. This is that machine, once.
 *
 * `getId` says whose catalogue is read; `releaseTypes` says which slice of it.
 * Either half changing is a NEW list: the caller keeps a `$effect(() =>
 * paging.reset())` and the old pages are thrown away before the first page of
 * the new selection is asked for. Everything returned is reactive.
 *
 * @param getId      () => string   artist id, or "" while none is loaded
 * @param releaseTypes () => string[] engine query types for the current scope
 * @param pageSize   pages of `pageSize` releases
 * @param mayLoad    optional extra gate checked per load (Appears On refuses
 *                   to spend a request when the summary count is zero)
 * @param seedTotal  optional total shown before the first answer lands
 * @param errorMessage message used verbatim when a page fails
 */
export function createCataloguePaging({
  getId,
  releaseTypes,
  pageSize = 4,
  mayLoad = null,
  seedTotal = null,
  errorMessage = "Could not load more of this catalogue.",
}) {
  let releases = $state([]);
  let nextOffset = $state(0);
  let total = $state(0);
  let loading = $state(false);
  let error = $state("");
  /** Identity of the list currently loaded, so a scope or artist change refills. */
  let loadedKey = "";
  let generation = 0;

  /* Whose catalogue, in which slice — either half moving is another list. */
  const keyOf = () => `${getId()}::${releaseTypes().join(",")}`;

  async function loadNext(key = keyOf(), expected = generation) {
    const id = getId();
    if (
      !id ||
      expected !== generation ||
      keyOf() !== key ||
      loading ||
      nextOffset == null ||
      (mayLoad && !untrack(mayLoad))
    ) return;
    const offset = nextOffset;
    loading = true;
    error = "";
    try {
      const page = await loadCataloguePage(id, releaseTypes(), offset, pageSize);
      // An answer that outlives its list — artist navigated away, scope
      // toggled — is discarded rather than appended to the wrong page.
      if (expected !== generation || keyOf() !== key) return;
      const known = new Set(releases.map((release) => release.id));
      for (const release of page?.releases ?? []) {
        if (!release?.id || known.has(release.id)) continue;
        releases.push(release);
        known.add(release.id);
      }
      total = page?.total ?? total;
      nextOffset = page?.next_offset ?? null;
    } catch (reason) {
      if (expected === generation) error = String(reason || errorMessage);
    } finally {
      if (expected === generation) loading = false;
    }
  }

  /** A new artist or scope throws everything away and opens the first page;
   *  the view renders its skeleton frame until that page lands. No-op while
   *  the identity is unchanged, so calling it from an effect is free. */
  function reset() {
    const key = keyOf();
    if (!getId() || key === loadedKey) return;
    loadedKey = key;
    generation += 1;
    const expected = generation;
    loading = false;
    releases = [];
    nextOffset = 0;
    total = seedTotal ? untrack(seedTotal) : 0;
    error = "";
    queueMicrotask(() => loadNext(key, expected));
  }

  /* Download marks reach the songs already on screen, in place. */
  $effect(() => watchCacheMarks((ids, cached) => {
    for (const release of releases) applyCacheMarks(release.tracks, ids, cached);
  }));

  return {
    get releases() { return releases; },
    get nextOffset() { return nextOffset; },
    get total() { return total; },
    get loading() { return loading; },
    get error() { return error; },
    loadNext,
    reset,
  };
}

/**
 * The footer sentinel both paged catalogues share: one bounded page opens the
 * view, and a further page loads whenever the footer comes within 400px of the
 * pane's bottom; each view also keeps its button as the keyboard fallback.
 * Observing reports the footer at once, so one already in reach at mount loads
 * without a scroll. A footer that stays in reach while a page lands above it
 * reports nothing new, so a view re-attaches per page: `$effect(() => {
 * paging.releases.length; return sentinelLoader(…); })`. Returns the detach
 * function.
 */
export function sentinelLoader(node, load) {
  if (!node) return;
  const root = node.closest(".scroll");
  if (!root) return;
  const observer = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) load();
  }, { root, rootMargin: "0px 0px 400px 0px" });
  observer.observe(node);
  return () => observer.disconnect();
}
