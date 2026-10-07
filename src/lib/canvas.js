/* =====================================================================
   CANVAS — the loop the panel shows, and what lights the haze

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

   Whatever the haze settles on stays for the record. A Canvas decoded
   after the haze was lit from the cover crossfades in over the cover in
   the panel, and the haze is left alone.

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
 * The panel's Canvas, one record at a time.
 *
 * `decoded(url)` answers whether the panel already holds that loop with a
 * frame decoded (warmed ahead, or shared with the last record). `changed`
 * receives { url, ready, haze, warm, resetPreference } after every update:
 *   url    the playing record's Canvas; "" while unknown, or for none
 *   ready  its first frame is decoded
 *   haze   "hold" | "cover" | "video": what the haze is lit from
 *   warm   the loop to load ahead, for the record the engine fetches ahead
 *   resetPreference  a new record: a look at the cover is not carried over
 */
export function createCanvasOwner({ lookups, decoded, changed, clock = realClock }) {
  let disposed = false;
  let key = "";
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
  let reset = false;
  let ahead = "";
  let aheadTicket = 0;
  let warm = "";

  function publish() {
    if (disposed) return;
    changed({ url, ready, haze, warm, resetPreference: reset });
    reset = false;
  }
  function stop() {
    if (timer) clock.unwait(timer);
    timer = 0;
  }
  /** The haze stops holding; a record is lit once. */
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
    reset = true;
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
    reset = true;
    haze = allowed && key ? "hold" : "cover";
  }
  function start() {
    started = true;
    since = clock.now();
    const known = lookups.peek(key);
    if (known !== undefined) return known ? found(known) : none();
    until(LOOKUP_WAIT_MS);
    const ticket = generation;
    const settle = (answer) => {
      if (disposed || ticket !== generation) return;
      if (answer) found(answer);
      else none();
      publish();
    };
    lookups.lookup(key).then(settle, () => settle(""));
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

  /** The record playing (its track id, "" for none), whether its Canvas
      may be shown, whether the window is seen, and the record the engine
      is fetching ahead ("" for none). */
  function update(nextKey, enabled, seen, next) {
    if (disposed) return;
    if (nextKey !== key || enabled !== allowed) {
      key = nextKey;
      allowed = enabled;
      begin();
    }
    visible = seen;
    if (haze === "hold" && !started && visible) start();
    prepare(next);
    publish();
  }
  /** A loop's first frame is decoded. */
  function arrive(source) {
    if (disposed || !source || source !== url || ready) return;
    ready = true;
    light("video");
    publish();
  }
  /** A loop could not be loaded: the cover is the picture, and a loop
      warmed for nothing is not loaded again for this preload. */
  function fail(source) {
    if (disposed || !source) return;
    if (source === warm) warm = "";
    if (source === url) none();
    publish();
  }
  function dispose() {
    disposed = true;
    generation++;
    aheadTicket++;
    stop();
  }
  return { update, arrive, fail, dispose };
}
