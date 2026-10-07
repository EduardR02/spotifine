/* =====================================================================
   CONTENT COLOUR

   The palette owns the chrome; the artwork owns the content. Everything in
   this module exists to answer one question — "what colour is this record?" —
   and to answer it in a form the interface can actually use.

   Two rules make that safe:

     1. Only the HUE comes from the picture. Lightness and chroma are imposed
        here, at fixed targets, so a white sleeve cannot produce a white header
        and a neon one cannot produce a neon header. The app stays as dark as
        it was designed to be, and every page is a different colour anyway.
        The hue is not quite untouched either: `warpHue` folds one arc of the
        wheel away, the arc that has no dark form at all. That is the only
        place a sleeve is overruled, and it is 118° out of 360.

     2. Nothing here is allowed to fail loudly. A cross-origin refusal, a
        decode error, a cover that never arrives — each one falls through to a
        deterministic hue derived from the entity id, which is also what the
        generated identity tile uses, so a coverless playlist still has a
        colour and it is the same colour as its tile.

   Why this and not a palette tint: a translucent pastel over near-black is
   always a dark low-chroma smear — that is arithmetic, not taste, and it is
   exactly how the old 20%-rose header wash turned brown. A colour picked at a
   fixed dark lightness and a real chroma never does that.
   ===================================================================== */

import { coverUrl } from "./state.svelte.js";
import { boundedReads } from "./cover-work.js";
import { warpHue, clayOf } from "./haze.js";

/* --- Oklab. Small enough to inline; the alternative is a dependency for
       twelve lines of matrix arithmetic. -------------------------------- */

const toLinear = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const toSrgb = (c) => {
  const v = c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
  return Math.round(Math.min(1, Math.max(0, v)) * 255);
};

/** sRGB 0-255 → Oklab. */
function oklab(r8, g8, b8) {
  const r = toLinear(r8 / 255);
  const g = toLinear(g8 / 255);
  const b = toLinear(b8 / 255);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** Oklch → `#rrggbb`, gamut-clipped by channel (good enough at these chromas). */
function hex(L, C, hueDeg) {
  const h = (hueDeg * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const r = toSrgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s);
  const g = toSrgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s);
  const bl = toSrgb(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s);
  return `#${((1 << 24) | (r << 16) | (g << 8) | bl).toString(16).slice(1)}`;
}

/* --- The fold -------------------------------------------------------- */

/* The arc of hues with no dark form (burnt orange through olive), and the
   fold that vacates it, live in haze.js: the ambient haze darkens colour
   too and needs the same answer, from inside a worker. */

/* --- The clamp ------------------------------------------------------- */

/**
 * One hue, four jobs. These lightnesses are the whole safety story: the header
 * sits at L 0.35 whatever the sleeve does, so the pane never brightens and the
 * white type on top never has to be re-checked.
 */
function palette(hue, chroma) {
  const c = Math.min(0.115, Math.max(0, chroma));
  const h = warpHue(hue);
  /**
   * How far up the warm flank we landed, 0 at 38° and 1 at 54°.
   *
   * This can be read off the OUTPUT hue, which is the whole reason the fold is
   * worth having: the warp guarantees nothing exists between 54 and 138, so a
   * correction keyed on the output cannot accidentally catch a hue it was not
   * meant for, and it needs no second copy of the arc's arithmetic to know
   * where it is.
   *
   * The green flank asks for nothing — 138° at L 0.35 is #1a4603, which is
   * already a pine and not an olive. The warm flank still does, because it
   * ends on a hue that reads as brown until it is both lighter and less
   * saturated: at the tip, six points of lightness and a fifth off the chroma
   * turn #622600 into #6c3d1a, mud into burnt sienna. Both corrections ramp
   * from zero at 38 so the seam with the untouched wheel stays seamless, and
   * both apply only to the wash pair — at L 0.6 the same hue is a caramel and
   * wants no help at all.
   */
  const clay = clayOf(h);
  const lift = 0.06 * clay;
  const cw = c * (1 - 0.22 * clay);
  return {
    hue: h,
    chroma: c,
    /** Header wash, top stop. Dark enough for 56px white type over it. */
    wash: hex(0.35 + lift, cw, h),
    /** Deeper stop, so the fade has a shape instead of a straight ramp. Both
        stops take the same lift, which is what keeps the fade's 0.09 span. */
    washDeep: hex(0.26 + lift, cw * 0.82, h),
    /** Coloured light: card hover shadows, artwork glow, panel tint. */
    glow: hex(0.6, Math.min(0.14, c * 1.25), h),
    /** Generated identity tile, light corner → dark corner. */
    tileA: hex(0.68, Math.min(0.14, c * 1.3), h),
    tileB: hex(0.32, Math.min(0.1, c * 0.9), h),
  };
}

/**
 * The identity hue ring: what a record looks like when it has no artwork.
 *
 * Deliberately NOT the semantic accents. The old version drew generated tiles
 * from foam and rose only, which meant half of everything coverless was the
 * same pale warm and — once that same pick drove the header wash — half of all
 * playlists opened brown.
 *
 * Avoiding the mud arc is no longer this list's job — `warpHue` folds any hue
 * out of it, so a careless entry would come out fine. What the list is for is
 * that a coverless playlist should get a colour someone CHOSE, spread far
 * enough from the other seven that two unrelated records never look like the
 * same record.
 *
 * Which is exactly why every entry has to be a FIXED POINT of the warp: a
 * number here that the fold would move is a number that no longer says what
 * colour you get. Only hues below 38 and above 156 qualify, plus those two
 * exactly. The green slot used to be 142 and the fold moved it to 152, so it
 * is 158 now — still a forest green, `#004926` at the wash, and the only
 * clearing left between the arc's top edge and the sea green at 172.
 */
const IDENTITY_HUES = [14, 38, 158, 172, 198, 228, 268, 316];

/** FNV-1a, unchanged from the one the artwork tiles have always used. */
function fnv(seed) {
  let h = 0x811c9dc5;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h;
}

/**
 * A tone built from a hue the app already owns.
 *
 * Liked Songs is the one page whose colour is a decision rather than a
 * measurement — it is the rose page by definition — so it asks for rose's hue
 * (21° in Oklab) and gets it clamped to exactly the same dark as every
 * extracted header. That is why it can be deep rose without being brown:
 * `#ebbcba` faded to 20% is a warm grey, but rose's *hue* rebuilt at a real
 * chroma is a red.
 */
export const paletteFor = palette;

/** Synchronous, network-free, stable across restarts. */
export function identityTone(seed, chroma = 0.095) {
  return palette(IDENTITY_HUES[fnv(String(seed ?? "")) % IDENTITY_HUES.length], chroma);
}

/* --- Extraction ------------------------------------------------------ */

/**
 * 16x16 is not a compromise, it is the point: the browser's own downscaler
 * does the averaging in native code, and 256 samples is far more than enough
 * to find which hue a sleeve is mostly made of. A full-size read would cost
 * ~90,000 times the pixels for the same answer.
 */
const SAMPLE = 16;
const BUCKETS = 24; // 15° each

/** Pixels that cannot tell us anything about hue. */
const MIN_ALPHA = 200;
const MIN_L = 0.16; // black borders, drop shadows, letterboxing
const MAX_L = 0.93; // paper-white sleeves, blown highlights
const MIN_C = 0.035; // greys, and the near-greys that would average to mud

let canvas = null;
function scratch() {
  if (!canvas) {
    canvas = document.createElement("canvas");
    canvas.width = SAMPLE;
    canvas.height = SAMPLE;
  }
  return canvas;
}

/**
 * Reads the dominant hue out of an image.
 *
 * The image is loaded on its own `Image`, never on the one the interface is
 * showing: the displayed `<img>` must not carry `crossorigin`, because if the
 * header were ever missing that attribute would cost the ARTWORK rather than
 * just the colour. Here a refusal costs nothing but a `null`.
 */
function readHue(url) {
  return new Promise((resolve) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.decoding = "async";
    img.onerror = () => resolve(null);
    img.onload = () => {
      try {
        const c = scratch();
        const ctx = c.getContext("2d", { willReadFrequently: true });
        if (!ctx) return resolve(null);
        ctx.clearRect(0, 0, SAMPLE, SAMPLE);
        ctx.drawImage(img, 0, 0, SAMPLE, SAMPLE);
        const { data } = ctx.getImageData(0, 0, SAMPLE, SAMPLE);

        /* Sum a and b per hue bucket, weighted. Summing the OPPONENT axes and
           not the angles is what keeps 359° and 1° from averaging to cyan. */
        const sumA = new Float64Array(BUCKETS);
        const sumB = new Float64Array(BUCKETS);
        const weight = new Float64Array(BUCKETS);
        let best = -1;
        let bestWeight = 0;

        for (let i = 0; i < data.length; i += 4) {
          if (data[i + 3] < MIN_ALPHA) continue;
          const [L, a, b] = oklab(data[i], data[i + 1], data[i + 2]);
          if (L < MIN_L || L > MAX_L) continue;
          const C = Math.hypot(a, b);
          if (C < MIN_C) continue;
          /* Weight by chroma ABOVE the threshold, not by chroma: a sleeve of
             warm-white paper is thousands of pixels each a hair over the line,
             and weighting them by their absolute chroma let them out-vote the
             handful of genuinely coloured ones. Measured from the threshold, a
             near-grey contributes near-nothing, which is the truth about it.
             Mid lightnesses are preferred for the same reason — a record's
             identity is its body colour, not its shadow or its specular. */
          const w = (C - MIN_C) * (1 - Math.min(1, Math.abs(L - 0.55) / 0.55) * 0.7);
          let angle = (Math.atan2(b, a) * 180) / Math.PI;
          if (angle < 0) angle += 360;
          const k = Math.floor(angle / (360 / BUCKETS)) % BUCKETS;
          sumA[k] += a * w;
          sumB[k] += b * w;
          weight[k] += w;
        }

        /* A bucket plus its two neighbours, so a hue that straddles a boundary
           is not beaten by a narrower one that happens to sit in the middle. */
        for (let k = 0; k < BUCKETS; k++) {
          const total =
            weight[k] +
            weight[(k + 1) % BUCKETS] * 0.5 +
            weight[(k + BUCKETS - 1) % BUCKETS] * 0.5;
          if (total > bestWeight) {
            bestWeight = total;
            best = k;
          }
        }
        /* Read the image fine, found no colour in it: a black-and-white
           sleeve, a black square, a scan of typed paper. Distinct from
           `null`, which means we never got to look — see the caller. */
        if (best < 0 || bestWeight <= 0) return resolve("mono");

        const a = sumA[best];
        const b = sumB[best];
        let angle = (Math.atan2(b, a) * 180) / Math.PI;
        if (angle < 0) angle += 360;
        /* How much of the image agreed, 0..1, and it is what stops a greyscale
           sleeve from getting a confidently coloured page. One wide field of
           colour earns full chroma; a muted or monochrome cover earns almost
           none and its header comes out a near-neutral dark — which is the
           honest answer, because it does not HAVE a colour. */
        const agreement = Math.min(1, bestWeight / (SAMPLE * SAMPLE * 0.03));
        resolve({ hue: angle, chroma: 0.028 + 0.087 * agreement });
      } catch {
        /* Tainted canvas: the cover protocol did not send CORS headers. The
           artwork is fine, we simply do not get to know its colour. */
        resolve(null);
      }
    };
    img.src = url;
  });
}

/**
 * One image, one decode.
 *
 * `coverTone`'s own cache is keyed by the whole pool, which is right — four
 * cells are one answer — and it means a cover that appears both on its own and
 * as one of a playlist's four cells answers to two keys and had the identical
 * file decoded twice: 2.5ms of main-thread work for a 640px sleeve, on the
 * common path, because a playlist's cells ARE the covers the album cards show.
 * Keyed per url here, so the second ask shares the first one's promise instead
 * of decoding the same picture again.
 *
 * The queue is the other half of the same measurement. A library page asks for
 * two hundred covers at once, and left alone the browser starts all two
 * hundred decodes in the order they were asked for rather than the order they
 * are looked at. Four at a time, drained in that order, keeps the top of the
 * page — the part on screen — at the front.
 */
const readHueOnce = boundedReads((local) => readHue(local), {
  max: 256, // a session walks a whole library; the oldest sample leaves first
  limit: 4,
});

/* --- Store ----------------------------------------------------------- */

/** key → palette. Reactive so a late extraction repaints the header. */
const tones = $state({});
const inflight = new Set();
const TONE_CACHE_MAX = 512;
const toneRecency = new Set();

function touchTone(key) {
  toneRecency.delete(key);
  toneRecency.add(key);
  if (toneRecency.size > TONE_CACHE_MAX) {
    const oldest = toneRecency.values().next().value;
    toneRecency.delete(oldest);
    delete tones[oldest];
  }
}

/**
 * What is actually on screen, which is what the page is allowed to take its
 * colour from. `Cover.svelte` builds the same list — distinct, non-empty,
 * capped at four — and then only draws all four when it has all four; with one
 * to three it falls back to a single tile of the first. Reading a cover the
 * viewer cannot see would be a colour with no visible source, so the tiers
 * match here too.
 */
function coverPool(covers) {
  const list = Array.isArray(covers) ? covers : [covers];
  const pool = [...new Set(list.filter(Boolean))].slice(0, 4);
  return pool.length >= 4 ? pool : pool.slice(0, 1);
}

/**
 * The tone for a piece of content, available synchronously.
 *
 * Returns the identity fallback immediately and upgrades to the extracted
 * colour when it lands, which means a page never waits on a canvas read to
 * paint and never flashes a neutral header first.
 *
 * `covers` is a url or a list of them, because a playlist without artwork of
 * its own is drawn as a mosaic of four records and there is no reason the page
 * should take its colour from whichever of the four happens to be first. That
 * was a real bug and it is what the fold above was wrongly blamed for: the
 * playlist "headphone demo" measures 37.8°/C 0.051 on cell one — a taupe that
 * lands a fifth of a degree under `ARC_LO`, where the fold cannot reach it —
 * while cell two measures 234.2°/C 0.084. The page opened brown standing next
 * to three tiles that were not.
 *
 * So read every cell and PICK the strongest, never average. `readHue` derives
 * chroma monotonically from how much of an image agreed on one hue, so the
 * highest chroma IS the cell that most clearly has a colour. Averaging would
 * repeat the mistake `MIN_C` is there to prevent, one level up: four unrelated
 * records are four unrelated hues, and their mean is mud — which is the
 * complaint we would be answering with a different spelling of itself.
 */
export function coverTone(covers, seed = "") {
  const pool = coverPool(covers);
  const key = pool.join("|") || `seed:${seed}`;
  const hit = tones[key];
  if (hit) {
    touchTone(key);
    return hit;
  }

  if (pool.length && !inflight.has(key)) {
    inflight.add(key);
    Promise.all(
      pool.map((url) => readHueOnce(url, coverUrl(url)).catch(() => null)),
    )
      .then((found) => {
        /* `null` never got looked at, `"mono"` was looked at and had no colour
           in it; neither can win a contest scored on chroma, so both drop out
           here and the vote is between the cells that actually measured. */
        let best = null;
        for (const f of found) if (f && f !== "mono" && (!best || f.chroma > best.chroma)) best = f;
        /* Cache the miss as well, as the fallback: one failed read per cover.
           A cover that HAS no colour — a black-and-white sleeve, a black
           square — gets the identity hue at a chroma so low it is effectively
           a neutral dark. That is deliberate and it is the honest answer: a
           monochrome record should not open a blue page. It also keeps the
           scale continuous, because a nearly-monochrome sleeve lands on almost
           exactly the same tone through the measured path. */
        tones[key] = best ? palette(best.hue, best.chroma) : identityTone(seed || key, 0.022);
        touchTone(key);
      })
      .catch(() => {
        tones[key] = identityTone(seed || key);
        touchTone(key);
      })
      .finally(() => inflight.delete(key));
  }
  return identityTone(seed || key);
}

/** Whether coverTone's answer for these covers is final: measured (or
    failed to be), or there is no cover to measure. Reactive, like it. */
export function toneSettled(covers) {
  const pool = coverPool(covers);
  return !pool.length || Boolean(tones[pool.join("|")]);
}

/** Lazy card colour: pointer/focus, or visibility for an always-coloured pick. */
export function cardTone(node, initial) {
  let source = $state(initial);
  let active = $state(false);
  const activate = () => { active = true; };
  node.addEventListener("pointerenter", activate, { once: true });
  node.addEventListener("focusin", activate, { once: true });
  const observer = initial[2] ? new IntersectionObserver(([entry]) => {
    if (!entry.isIntersecting) return;
    activate();
    observer.disconnect();
  }, { rootMargin: "120px" }) : null;
  observer?.observe(node);
  $effect(() => {
    if (!active) return;
    const tone = coverTone(source[0], source[1]);
    node.style.setProperty("--tone-glow", tone.glow);
    node.style.setProperty("--tone-wash", tone.wash);
  });
  return {
    update(value) { source = value; },
    destroy() {
      node.removeEventListener("pointerenter", activate);
      observer?.disconnect();
      node.removeEventListener("focusin", activate);
    },
  };
}
