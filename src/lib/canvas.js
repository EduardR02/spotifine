/* =====================================================================
   CANVAS — what the panel shows, and what lights the haze

   The panel (NowPlayingPanel.svelte) shows a record's Canvas when it has
   one, and the haze (Ambient.svelte) is lit once per record: from that
   Canvas's first frame, or from the cover. Lit from the cover and then lit
   again a second later for the Canvas is two changes of the whole window
   for one change of record, so the source is decided once:

     known to have none   "cover", at once.
     known to have one    "hold" (the haze keeps the last record's light)
                          until its first frame is decoded, then "video";
                          "cover" if that has not happened FRAME_WAIT_MS
                          after the change.
     not known yet        "hold" while it is asked, then as above; "cover"
                          if the answer takes longer than LOOKUP_WAIT_MS.

   The panel's picture follows the same decision, so the two change in one
   moment. While the haze holds, the panel keeps exactly what it showed —
   the last record's loop, or its cover — and then goes once: straight to
   the new loop, or to the new cover. The copy under the picture is the
   new record's from the start. Whatever the haze settles on stays for the
   record; a Canvas decoded after the cover was settled on crossfades in
   over the cover in the panel, and the haze is left alone.

   An empty panel holds an empty picture until this decision settles, rather
   than flashing the new cover before a fast Canvas arrives.

   A lookup is asked for the record playing, and for the one the engine is
   fetching ahead in the last seconds of a track (its gapless preload,
   `preload_uri`), never for anything else; every answer, "none" included,
   is kept for the session. The record fetched ahead also has its loop
   loaded (`warm`), so an automatic advance goes straight from one Canvas
   to the next and the haze is lit from the new one in the same moment.

   Every answer is taken only for the record it was asked for: a skip in
   the middle of a lookup, a wait or a warm-up leaves nothing behind.
   ===================================================================== */

/** How long an unknown record's lookup is waited for. */
export const LOOKUP_WAIT_MS = 600;
/** How long after a change of record its Canvas's first frame is waited for. */
export const FRAME_WAIT_MS = 1500;

/**
 * The Canvas lookups made this session, by track id: a URL, or "" for none.
 * Concurrent askers share one request; a failure is not kept, so a later
 * look asks again. `forget` drops everything, answers still in flight
 * included (the account changed, and with it the answers).
 */
export function createCanvasLookups(load, max = 256) {
  const known = new Map();
  const pending = new Map();
  let epoch = 0;
  return {
    /** The answer, or undefined when it has not been asked. */
    peek: (id) => known.get(id),
    lookup(id) {
      if (known.has(id)) return Promise.resolve(known.get(id));
      const inflight = pending.get(id);
      if (inflight) return inflight;
      const asked = epoch;
      let request;
      try {
        request = Promise.resolve(load(id));
      } catch (error) {
        request = Promise.reject(error);
      }
      const answer = request
        .then((canvas) => {
          const url = canvas?.url || "";
          if (asked === epoch) {
            known.set(id, url);
            if (known.size > max) known.delete(known.keys().next().value);
          }
          return url;
        })
        .finally(() => {
          if (pending.get(id) === answer) pending.delete(id);
        });
      pending.set(id, answer);
      return answer;
    },
    forget() {
      epoch++;
      known.clear();
      pending.clear();
    },
  };
}

const realClock = {
  now: () => performance.now(),
  wait: (fn, ms) => setTimeout(fn, ms),
  unwait: (id) => clearTimeout(id),
};

/**
 * The panel's Canvas and picture, one record at a time.
 *
 * `decoded(url)` answers whether the panel already holds that loop with a
 * frame decoded (warmed ahead, shared with the last record, or held on
 * screen). `changed` receives { url, ready, haze, warm, shown } after every
 * update:
 *   url    the playing record's Canvas; "" while unknown, or for none
 *   ready  its first frame is decoded
 *   haze   "hold" | "cover" | "video": what the haze is lit from
 *   warm   the loop to load ahead, for the record the engine fetches ahead
 *   shown  what the panel shows: { loop, cover, swap } — the loop on screen
 *          ("" for none), the cover under it (the `cover` its record was
 *          given), and whether that record offers the cover/Canvas choice.
 *          The same object until it changes; while the haze holds, the
 *          last record's.
 */
export function createCanvasOwner({ lookups, decoded, changed, clock = realClock }) {
  let disposed = false;
  let key = "";
  let cover = null;
  let allowed = false;
  let visible = false;
  /** Bumped by every change of record; an answer for an older one is dropped. */
  let generation = 0;
  /** The record's decision has begun. It waits for the window to be seen. */
  let started = false;
  let since = 0;
  let timer = 0;
  let url = "";
  let ready = false;
  let haze = "cover";
  /** A look at this record's cover; the next record arrives on its Canvas. */
  let preferCover = false;
  let shown = { loop: "", cover: null, swap: false };
  let ahead = "";
  let aheadTicket = 0;
  let warm = "";

  /** Keep the last picture, or an empty panel, while the source is decided. */
  function settle() {
    if (!visible || haze === "hold") return;
    const swap = Boolean(url && ready);
    const loop = swap && !preferCover ? url : "";
    if (loop !== shown.loop || cover !== shown.cover || swap !== shown.swap) shown = { loop, cover, swap };
  }
  function publish() {
    if (disposed) return;
    settle();
    changed({ url, ready, haze, warm, shown });
  }
  function stop() {
    if (timer) clock.unwait(timer);
    timer = 0;
  }
  /** The haze and the picture stop holding; a record is lit once. */
  function light(source) {
    if (haze !== "hold") return;
    stop();
    haze = source;
  }
  /** Lit from the cover `ms` from now, unless it is lit by then. */
  function until(ms) {
    stop();
    const ticket = generation;
    timer = clock.wait(() => {
      timer = 0;
      if (ticket !== generation || haze !== "hold") return;
      haze = "cover";
      publish();
    }, Math.max(0, ms));
  }
  function none() {
    url = "";
    ready = false;
    preferCover = false;
    light("cover");
  }
  function found(source) {
    url = source;
    ready = decoded(source);
    if (ready) light("video");
    else if (haze === "hold") until(since + FRAME_WAIT_MS - clock.now());
  }
  function begin() {
    generation++;
    stop();
    started = false;
    url = "";
    ready = false;
    preferCover = false;
    haze = allowed && key ? "hold" : "cover";
  }
  function start() {
    started = true;
    since = clock.now();
    const known = lookups.peek(key);
    if (known !== undefined) return known ? found(known) : none();
    until(LOOKUP_WAIT_MS);
    const ticket = generation;
    const answer = (source) => {
      if (disposed || ticket !== generation) return;
      if (source) found(source);
      else none();
      publish();
    };
    lookups.lookup(key).then(answer, () => answer(""));
  }
  /** The record fetched ahead, while Canvas is on and the window is seen. */
  function prepare(next) {
    const wanted = allowed && visible && next !== key ? next : "";
    if (wanted === ahead) return;
    ahead = wanted;
    const ticket = ++aheadTicket;
    const known = wanted ? lookups.peek(wanted) : "";
    warm = known ?? "";
    if (known !== undefined) return;
    lookups.lookup(wanted).then((answer) => {
      if (disposed || ticket !== aheadTicket || answer === warm) return;
      warm = answer;
      publish();
    }, () => {});
  }

  /**
   * The record playing: its track id ("" for none, or for an episode) and
   * its cover (any value, handed back in `shown`); whether its Canvas may
   * be shown; whether the window is seen; and the record the engine is
   * fetching ahead ("" for none).
   */
  function update({ key: nextKey, cover: nextCover = null, enabled, visible: seen, ahead: next = "" }) {
    if (disposed) return;
    cover = nextCover;
    if (nextKey !== key || enabled !== allowed) {
      key = nextKey;
      allowed = enabled;
      begin();
    }
    visible = seen;
    /* Closing keeps the displayed picture, not a decision still downloading
       behind it. Its answer may fill the lookup cache, but is adopted only
       when visible again, with a fresh bounded wait. */
    if (!visible && started && !ready) {
      generation++;
      stop();
      started = false;
      url = "";
      haze = allowed && key ? "hold" : "cover";
    }
    if (haze === "hold" && !started && visible) start();
    prepare(next);
    publish();
  }
  /** A loop's first frame is decoded. */
  function arrive(source) {
    if (disposed || !visible || !source || source !== url || ready) return;
    ready = true;
    light("video");
    publish();
  }
  /** A loop could not be loaded. The record's own means the cover from now
      on; a held one gives way to the cover it covered; and one warmed for
      nothing is not loaded again for this preload. */
  function fail(source) {
    if (disposed || !source) return;
    if (source === warm) warm = "";
    if (source === url) none();
    if (haze === "hold" && source === shown.loop) shown = { ...shown, loop: "", swap: false };
    publish();
  }
  /** The cover/Canvas choice, for a record whose loop is on offer. */
  function swap() {
    if (disposed || haze === "hold" || !(url && ready)) return;
    preferCover = !preferCover;
    publish();
  }
  function dispose() {
    disposed = true;
    generation++;
    aheadTicket++;
    stop();
  }
  return { update, arrive, fail, swap, dispose };
}
