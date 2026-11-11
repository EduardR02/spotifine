/* =====================================================================
   THE HAZE — arithmetic

   One picture of what is playing sits behind every panel, and the panels
   are glass over it (Ambient.svelte, haze.worker.js). This module is the
   part of that which is arithmetic: where each pixel of the haze takes its
   light from, how bright it may be, and how it is quantised. Pure
   functions, so the worker and the tests share them.

   The haze is the picture itself, seen through soft glass — its shapes,
   edges and gradients, not a few blobs of its colour. The source is read
   at a real resolution and GRADED first, pixel by pixel in Oklab: keyed low
   against its own median, so a light picture keeps its light and shade and
   a dark one its mood, with chroma kept and the fine detail laid back over
   the compressed range (local contrast). Then it is LAID OUT: under the
   video exactly as the video shows it, and to its left as its own mirror
   image — continuous at the video's edge — magnified with distance, so the
   window is one large view of the picture rather than repeated copies.
   Then it is blurred lightly, once, given its fall-off — light has a
   source, and the haze is brightest there: beside a Canvas, at a cover's
   tile, over the player bar — and held under the ceilings.

   Blur happens in LINEAR light, as a lens does it: a convex mix of linear
   colours is never brighter than its brightest input, so the ceilings hold
   through it; the chroma the mix cancels is given back. Quantisation to 8
   bits is dithered, which is what keeps a dark gradient stretched over a
   window from breaking into bands.

   TWO RENDERERS draw it. The GPU's (haze-gl.js) is the one the worker
   uses; renderHaze here, on the CPU, is the reference it is checked
   against and the fallback where WebGL2 cannot be had. They are one
   pipeline: what both need before a pixel is drawn — where the picture
   is, the part of the source it frames, the source's statistics, the
   layout's columns, the seam — is worked out here (prepare), and every
   calibrated number lives here once: the CPU reads it directly and the
   shaders are handed it as #defines (TUNING), so the two cannot drift.
   ===================================================================== */

/**
 * The ceilings, in linear (WCAG relative) luminance.
 *
 * PEAK bounds every pixel of the haze and MEAN bounds its average. Both are
 * needed: a colourful video has a few bright regions and a dark body, and
 * only the peak constrains it; a white video is ALL peak, and a peak-only
 * ceiling would light the whole window to it. 0.10 is the grey #595959,
 * 0.045 is #3c3c3c. PEAK holds for every output pixel AFTER dithering, and
 * the glass tints are calibrated against it: any sRGB colour with relative
 * luminance at most PEAK can be behind any glass surface.
 */
export const CEILING = { peak: 0.1, mean: 0.045 };
/** The panel-closed glow is a hint of the record, not a light source. */
export const RESTRAINED = 0.5;
/** The long side of the source as the worker reads it, in pixels: enough
    for the picture's shapes to survive a light lens at window size. */
export const PROBE = 360;

/* sRGB's transfer function and Rec. 709's luminance weights. */
const SRGB_CUT = 0.04045;
const SRGB_SLOPE = 12.92;
const SRGB_OFFSET = 0.055;
const SRGB_GAMMA = 2.4;
const SRGB_LINEAR_CUT = 0.0031308;
const LUMA_R = 0.2126, LUMA_G = 0.7152, LUMA_B = 0.0722;

/* Comparisons against nothing, at the scales they guard. */
const EPS3 = 1e-3, EPS4 = 1e-4, EPS5 = 1e-5, EPS6 = 1e-6;

const lin = (c) => (c <= SRGB_CUT ? c / SRGB_SLOPE : ((c + SRGB_OFFSET) / (1 + SRGB_OFFSET)) ** SRGB_GAMMA);
const enc = (y) => (y <= SRGB_LINEAR_CUT ? y * SRGB_SLOPE : (1 + SRGB_OFFSET) * y ** (1 / SRGB_GAMMA) - SRGB_OFFSET);
const clamp01 = (v) => Math.min(1, Math.max(0, v));

/** Relative luminance of an encoded sRGB triple (0..1). */
export function luminance(r, g, b) {
  return LUMA_R * lin(r) + LUMA_G * lin(g) + LUMA_B * lin(b);
}

/* The decode, from tables: one for arbitrary encoded values (4096 steps is
   finer than the 8-bit values it is ever fed), one exact for bytes. */
const LUT = new Float32Array(4097);
for (let i = 0; i <= 4096; i++) LUT[i] = lin(i / 4096);
const linFast = (v) => LUT[v >= 1 ? 4096 : v <= 0 ? 0 : (v * 4096 + 0.5) | 0];
const LIN8 = new Float32Array(256);
for (let i = 0; i < 256; i++) LIN8[i] = lin(i / 255);
/* The encode, from a table: linear light (0..1) to sRGB, interpolated. */
const ENC = new Float32Array(16385);
for (let i = 0; i <= 16384; i++) ENC[i] = enc(i / 16384);
function encFast(y) {
  if (y <= 0) return 0;
  if (y >= 1) return 1;
  const f = y * 16384, i = f | 0;
  return ENC[i] + (ENC[i + 1] - ENC[i]) * (f - i);
}
const lum8 = (d, i) => LUMA_R * LIN8[d[i]] + LUMA_G * LIN8[d[i + 1]] + LUMA_B * LIN8[d[i + 2]];

/* The cube root of 0..1, from a table: cbrt(x) is u^(4/3) with u = x^(1/4),
   which is smooth at 0, so linear steps over u are exact to a few parts in
   10^7 — and two square roots and a lookup are a third of Math.cbrt. */
const QUARTIC = new Float64Array(8193);
for (let i = 0; i <= 8192; i++) QUARTIC[i] = (i / 8192) ** (4 / 3);
function cbrt(x) {
  if (!(x >= 0 && x <= 1)) return Math.cbrt(x);
  const u = Math.sqrt(Math.sqrt(x)) * 8192, i = u | 0;
  return i >= 8192 ? 1 : QUARTIC[i] + (QUARTIC[i + 1] - QUARTIC[i]) * (u - i);
}
/* t^0.6 of 0..1, from a table, for the fall-off's chroma. */
const POW06 = new Float64Array(4097);
for (let i = 0; i <= 4096; i++) POW06[i] = (i / 4096) ** 0.6;
function pow06(t) {
  if (t <= 0) return 0;
  if (t >= 1) return 1;
  const f = t * 4096, i = f | 0;
  return POW06[i] + (POW06[i + 1] - POW06[i]) * (f - i);
}

/** `#rrggbb` → encoded [r, g, b] in 0..1. */
export function rgbOf(hex) {
  const n = parseInt(String(hex).replace("#", ""), 16);
  if (!Number.isFinite(n)) return [1, 1, 1];
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

/* --- Oklab ------------------------------------------------------------ */

/* Björn Ottosson's matrices, by rows: linear sRGB → LMS, cube-rooted LMS →
   Lab, and back. */
const LMS_OF_RGB = [0.4122214708, 0.5363325363, 0.0514459929, 0.2119034982, 0.6806995451, 0.1073969566, 0.0883024619, 0.2817188376, 0.6299787005];
const LAB_OF_LMS = [0.2104542553, 0.793617785, -0.0040720468, 1.9779984951, -2.428592205, 0.4505937099, 0.0259040371, 0.7827717662, -0.808675766];
const LMS_OF_LAB = [1, 0.3963377774, 0.2158037573, 1, -0.1055613458, -0.0638541728, 1, -0.0894841775, -1.291485548];
const RGB_OF_LMS = [4.0767416621, -3.3077115913, 0.2309699292, -1.2684380046, 2.6097574011, -0.3413193965, -0.0041960863, -0.7034186147, 1.707614701];
const [M00, M01, M02, M10, M11, M12, M20, M21, M22] = LMS_OF_RGB;
const [N00, N01, N02, N10, N11, N12, N20, N21, N22] = LAB_OF_LMS;
const [, P01, P02, , P11, P12, , P21, P22] = LMS_OF_LAB;
const [Q00, Q01, Q02, Q10, Q11, Q12, Q20, Q21, Q22] = RGB_OF_LMS;

/** LINEAR sRGB → Oklab, into `o`. */
function labOfLinear(lr, lg, lb, o) {
  const l = cbrt(M00 * lr + M01 * lg + M02 * lb);
  const m = cbrt(M10 * lr + M11 * lg + M12 * lb);
  const s = cbrt(M20 * lr + M21 * lg + M22 * lb);
  o[0] = N00 * l + N01 * m + N02 * s;
  o[1] = N10 * l + N11 * m + N12 * s;
  o[2] = N20 * l + N21 * m + N22 * s;
  return o;
}

/** Oklab → LINEAR sRGB, unclamped (out of [0, 1] means out of gamut), into `o`. */
function linearOfLab(L, A, B, o) {
  const l0 = L + P01 * A + P02 * B;
  const m0 = L + P11 * A + P12 * B;
  const s0 = L + P21 * A + P22 * B;
  const l = l0 * l0 * l0, m = m0 * m0 * m0, s = s0 * s0 * s0;
  o[0] = Q00 * l + Q01 * m + Q02 * s;
  o[1] = Q10 * l + Q11 * m + Q12 * s;
  o[2] = Q20 * l + Q21 * m + Q22 * s;
  return o;
}

/** Encoded sRGB (0..1) → Oklab [L, a, b]. */
export function toOklab(r, g, b) {
  return labOfLinear(linFast(r), linFast(g), linFast(b), [0, 0, 0]);
}

/** Oklab → encoded sRGB (0..1), clamped. */
export function fromOklab(L, A, B) {
  const [r, g, b] = linearOfLab(L, A, B, [0, 0, 0]);
  return [enc(clamp01(r)), enc(clamp01(g)), enc(clamp01(b))];
}

/* --- The fold ---------------------------------------------------------- */

/**
 * The arc with no dark form, and the two edges where colour comes back.
 *
 * Every other hue at L 0.35 is a recognisable dark version of itself: 200° is
 * teal, 300° is aubergine, 14° is oxblood. This arc is the exception, and not
 * because of a bad choice of lightness — "amber" IS a light yellow, so there
 * is no dark amber, there is only brown. The failure runs the whole way from
 * burnt orange through olive; at the identity chroma, 68° is #5a2e00, 118° is
 * #374000 and 130° is still #294400. Warm survives up to about 38 and green
 * comes back at about 138, and between those two numbers there is nothing
 * worth showing.
 */
const ARC_LO = 38;
const ARC_HI = 156;
const WARM_TIP = 54;
const GREEN_TOE = 138;
/** Pure yellow, and the middle of the arc: the one hue with no flank to prefer. */
const FOLD = (ARC_LO + ARC_HI) / 2;

/** An angle in degrees, in [0, 360). (Floored, not `%`: V8 calls out for a
    float remainder, and this runs per pixel.) */
const degrees360 = (a) => a - 360 * Math.floor(a / 360);

/**
 * Vacate the arc by FOLDING it onto its own two edges.
 *
 * The thing to understand first is that no continuous monotonic remap can do
 * this. Push, squeeze, ease it however you like — if the map is continuous and
 * increasing, some input still lands in the middle, because that is what
 * continuity means.
 *
 * So the map folds. Below the fold hues run down the warm flank towards burnt
 * sienna; above it they run up the green flank towards pine. It is continuous
 * at 38 and at 156, which is the part that matters, because those are the
 * seams with the untouched rest of the wheel. The one discontinuity sits at
 * the fold itself, where a hue just below it opens burnt orange and one just
 * above it opens forest green.
 *
 * The cost is real and it is spread: 118° of input arrive on 34° of output, so
 * two gold sleeves that differ slightly now open the same page. That is the
 * trade being made on purpose — the arc had no variation worth keeping, only
 * different browns.
 */
export function warpHue(hue, fold = FOLD) {
  const h = degrees360(hue);
  if (h < ARC_LO || h > ARC_HI) return h;
  if (h < fold) return ARC_LO + ((h - ARC_LO) / (fold - ARC_LO)) * (WARM_TIP - ARC_LO);
  return GREEN_TOE + ((h - fold) / (ARC_HI - fold)) * (ARC_HI - GREEN_TOE);
}

/**
 * How far up the warm flank a WARPED hue landed, 0 at 38° and 1 at 54°. The
 * warm flank ends on a hue that still reads as brown until it is both
 * lighter and less saturated (see covertone's palette); this is the ramp
 * that correction is applied along, from zero at the seam with the rest of
 * the wheel. The green flank needs nothing: 138° dark is already a pine.
 */
export function clayOf(h) {
  return h > WARM_TIP ? 0 : Math.max(0, (h - ARC_LO) / (WARM_TIP - ARC_LO));
}

/* --- Tone ------------------------------------------------------------- */

/** Below this chroma (at mid lightness, GREY_L) a pixel has no hue of its
    own and takes the record's; darker pixels need less to count as
    coloured, down to GREY_FLOOR of it. What a grey pixel borrows is the
    record's hue at up to TINT_CMAX. */
const C_GREY = 0.04;
const GREY_L = 0.5;
const GREY_FLOOR = 0.3;
const TINT_CMAX = 0.12;
/** The warm flank's tip is softened this much in chroma and lifted this
    much in lightness, along clayOf. */
const CLAY_SOFTEN = 0.22;
const CLAY_LIFT = 0.04;
/** Warm light: the source chroma at which orange is only a tint (pale), and
    at which it is a colour in its own right (rich); and the chroma a pale
    warm source is left with. */
const WARM_PALE = 0.05;
const WARM_RICH = 0.13;
const WARM_BREATH = 0.018;
/** The warm band that darkens into brown, in output hue: from orange-red
    (easing in over WARM_IN from WARM_FROM, so pure red at 29° is barely
    touched) through the fold's warm flank, which ends at 54°, and out over
    WARM_OUT to WARM_TO. */
const WARM_FROM = 22;
const WARM_IN = 14;
const WARM_TO = 64;
const WARM_OUT = 6;
function warmth(hue) {
  const h = degrees360(hue);
  if (h < WARM_FROM || h > WARM_TO) return 0;
  return smooth(Math.min((h - WARM_FROM) / WARM_IN, (WARM_TO - h) / WARM_OUT));
}
/* The span the grade takes a hue's angle in, as the directions of its two
   edges: the warm band's start, and the arc's end. */
const WARM_EDGE = [Math.cos((WARM_FROM * Math.PI) / 180), Math.sin((WARM_FROM * Math.PI) / 180)];
const ARC_EDGE = [Math.cos((ARC_HI * Math.PI) / 180), Math.sin((ARC_HI * Math.PI) / 180)];
/** Where the haze's fold pivots: pale yellow light is warm, and a hazy sky
    sitting on pure yellow would otherwise split into sienna and pine. */
const HAZE_FOLD = 125;
/** Every pixel is toned this far under the peak, so that dithering, which
    can add up to one 8-bit level per channel, never crosses it. */
const DITHER_ROOM = 0.97;

const scratchLin = [0, 0, 0];

/* The grade. The picture is keyed LOW against its own median: its shadows
   land on dark colour, its highlights reach up toward the luminous end,
   and its body sits in between. A light picture therefore keeps its light
   and shade instead of becoming one bright wash, and a dark one is not
   lifted into grey. */
const G_SHADOW = 0.13;
const G_MID = 0.3;
const G_LIGHT = 0.58;
const G_HI_GAMMA = 1.3;
/** Chroma keeps more of itself than shading would when a colour is darkened
    (sqrt of the lightness ratio, not the ratio — with the source's
    lightness floored at L_FLOOR), then this boost, then a cap. */
const G_CHROMA = 1.5;
const G_CMAX = 0.2;
const L_FLOOR = 0.05;
/** Vibrance: a weak colour (a pastel, under VIB_KNEE of chroma) is lifted
    more than a strong one, up to this much more, so pastel light is still
    colour when darkened. */
const G_VIBRANCE = 0.7;
const VIB_KNEE = 0.12;
/** Mean chroma over what is seen: a picture that is one strong colour all
    over would be a wash of it; past this budget chroma is taken back, by
    the budget's ratio to C_BUDGET_EXP. */
const C_MEAN = 0.07;
const C_BUDGET_EXP = 0.9;
/** Perceived lightness: a saturated blue at a given L looks far brighter
    than a grey of it (Helmholtz–Kohlrausch). Capped per pixel, and in the
    mean over what is seen. Over the per-pixel cap chroma gives way first,
    down to CAP_KEEP of itself, and then lightness. */
const P_PEAK = 0.62;
const P_MEAN = 0.33;
const CAP_KEEP = 0.6;
/** Held in gamut and under the peak by steps: chroma by GAMUT_STEP while a
    channel is out of [0, 1] (to within GAMUT_TOL), lightness by the cube
    root of the excess, with Y_MARGIN to spare, at most CAP_ITER times. */
const GAMUT_TOL = 1e-4;
const GAMUT_STEP = 0.88;
const Y_MARGIN = 0.995;
const CAP_ITER = 16;
/** Local contrast. The grade compresses the picture's whole range and
    would flatten its edges with it; detail finer than LOCAL_SIGMA (of the
    frame's long side) is given back at LOCAL_GAIN of the source's own. The
    detail moves lightness no lower than DETAIL_FLOOR, and moves chroma
    with it by the square root of the change, within [DETAIL_KMIN,
    DETAIL_KMAX]. */
const LOCAL_SIGMA = 0.05;
const LOCAL_GAIN = 0.8;
const DETAIL_FLOOR = 0.03;
const DETAIL_KMIN = 0.6;
const DETAIL_KMAX = 1.4;

/** Oklab L plus the glow of chroma, strongest in blue-violet (HK_HUE, in
    radians, about 275°): HK_C of C everywhere, and up to HK_BLUE of C more
    by the cosine to that hue. */
const HK_C = 0.2;
const HK_BLUE = 0.2;
const HK_HUE = 4.8;
const HK_A = Math.cos(HK_HUE), HK_B = Math.sin(HK_HUE);
function perceived(L, A, B) {
  return L + HK_C * Math.sqrt(A * A + B * B) + HK_BLUE * Math.max(0, A * HK_A + B * HK_B);
}

/** The picture's lightness, from three quantiles of it: its shadows (p05),
    median and highlights (p97). */
function exposureOf(p05, p50, p97) {
  return {
    lo: Math.min(p05, p50 - 0.1),
    mid: p50,
    hi: Math.max(p97 + 0.02, p50 + 0.15),
    /* A genuinely dark picture keeps a little of its mood. */
    M: G_MID - 0.05 * smooth((0.3 - p50) / 0.25),
  };
}

/** Source lightness → haze lightness: the median onto M, the shadows onto
    G_SHADOW, the highlights up toward G_LIGHT. */
function keyOf(L, ex) {
  if (L <= ex.mid) return G_SHADOW + (ex.M - G_SHADOW) * clamp01((L - ex.lo) / Math.max(EPS3, ex.mid - ex.lo));
  return ex.M + (G_LIGHT - ex.M) * clamp01((L - ex.mid) / Math.max(EPS3, ex.hi - ex.mid)) ** G_HI_GAMMA;
}

/**
 * One colour graded, into `o` as Oklab: what has no hue borrows the
 * record's; the arc that darkens only into brown is folded away; lightness
 * is keyed to the picture (ex); chroma is kept; pale warm light stays a
 * breath of warmth rather than cardboard; and the result is under the
 * peaks (capInto). `s3` is the cube root of the haze's scale.
 */
function gradeInto(L, A, B, ex, tint, s3, ycap, o) {
  let C = Math.sqrt(A * A + B * B);
  const source = C;
  const grey = C_GREY * Math.min(1, Math.max(GREY_FLOOR, L / GREY_L));
  const tg = clamp01(1 - C / grey);
  if (tg > 0) {
    const tc = Math.sqrt(tint[1] * tint[1] + tint[2] * tint[2]);
    if (tc > EPS4) {
      const target = Math.min(tc, TINT_CMAX);
      A += tg * ((tint[1] / tc) * target - A);
      B += tg * ((tint[2] / tc) * target - B);
      C = Math.sqrt(A * A + B * B);
    }
  }
  /* Darkened, the arc from burnt orange to olive is only ever brown — a
     beige Canvas would become a khaki window. Fold it as the header wash
     does: onto burnt sienna or pine, lighter and softer at the warm tip.
     A hue outside WARM_FROM..ARC_HI is neither folded nor warm, and most
     of a picture is, so the angle is only taken inside that span (found
     by which side of its two edges the colour lies: it is under 180°). */
  let clay = 0;
  let hue = -1;
  if (WARM_EDGE[0] * B - WARM_EDGE[1] * A >= 0 && A * ARC_EDGE[1] - B * ARC_EDGE[0] >= 0) {
    hue = (Math.atan2(B, A) * 180) / Math.PI;
    if (C > EPS4 && hue >= ARC_LO) {
      hue = warpHue(hue, HAZE_FOLD);
      clay = clayOf(hue);
      const c = C * (1 - CLAY_SOFTEN * clay);
      A = c * Math.cos((hue * Math.PI) / 180);
      B = c * Math.sin((hue * Math.PI) / 180);
      C = c;
    }
  }
  const Lo = (keyOf(L, ex) + CLAY_LIFT * clay) * s3;
  const vib = 1 + G_VIBRANCE * (1 - smooth(source / VIB_KNEE));
  const k = C > EPS6 ? Math.min(C * G_CHROMA * vib * Math.sqrt(Lo / Math.max(L, L_FLOOR)), G_CMAX) / C : 0;
  A *= k;
  B *= k;
  /* Warm light kept dark is only ever brown unless it is genuinely
     saturated: fire stays fire, but cream, sand and skin in soft light turn
     to a breath of warmth on graphite, not the colour of cardboard. The
     hue is the one just set: scaling the chroma does not move it. */
  const warm = k > 0 && hue >= 0 ? warmth(hue) : 0;
  if (warm > 0) {
    const c = Math.sqrt(A * A + B * B);
    const genuine = smooth((source - WARM_PALE) / (WARM_RICH - WARM_PALE));
    const kept = Math.min(c, WARM_BREATH + (c - WARM_BREATH) * genuine);
    const f = c > EPS6 ? (c + warm * (kept - c)) / c : 1;
    A *= f;
    B *= f;
  }
  return capInto(Lo, A, B, s3, ycap, o);
}

/** Under the perceived peak (chroma gives way first, then lightness), in
    gamut, and under the luminance peak `ycap`; into `o` as Oklab. */
function capInto(Lo, A, B, s3, ycap, o) {
  const cap = P_PEAK * s3;
  const p = perceived(Lo, A, B);
  if (p > cap) {
    const c = Math.sqrt(A * A + B * B);
    const kc = (p - Lo) / Math.max(c, EPS6);
    const c2 = Math.max(c * CAP_KEEP, Math.min(c, (cap - Lo) / kc));
    A *= c2 / Math.max(c, EPS6);
    B *= c2 / Math.max(c, EPS6);
    Lo = Math.min(Lo, cap - kc * c2);
  }
  for (let i = 0; i < CAP_ITER; i++) {
    linearOfLab(Lo, A, B, scratchLin);
    const r = scratchLin[0], g = scratchLin[1], b = scratchLin[2];
    if (r < -GAMUT_TOL || g < -GAMUT_TOL || b < -GAMUT_TOL || r > 1 + GAMUT_TOL || g > 1 + GAMUT_TOL || b > 1 + GAMUT_TOL) {
      A *= GAMUT_STEP; // out of gamut at this lightness: give up chroma, keep hue
      B *= GAMUT_STEP;
      continue;
    }
    const y = LUMA_R * r + LUMA_G * g + LUMA_B * b;
    if (y > ycap) {
      Lo *= Math.cbrt(ycap / y) * Y_MARGIN;
      continue;
    }
    break;
  }
  o[0] = Lo;
  o[1] = A;
  o[2] = B;
  return o;
}

/** One colour as the haze grades it, alone (a flat picture of it), as Oklab. */
export function gradeLab(lab, tint, scale = 1) {
  const ex = exposureOf(lab[0], lab[0], lab[0]);
  return gradeInto(lab[0], lab[1], lab[2], ex, tint, Math.cbrt(scale), CEILING.peak * scale * DITHER_ROOM, [0, 0, 0]);
}

/* --- Reading the source ------------------------------------------------ */

/**
 * The picture inside any black frame: letterbox, pillarbox, or a Canvas
 * that is a small subject on black. Rows and columns are trimmed from each
 * edge of `rect` while nothing in them rises above near-black, up to 40% a
 * side, so a haze is made of the picture and not of its mount.
 * @param rect [x0, y0, x1, y1] to look inside, integers, end-exclusive
 * @returns [x0, y0, x1, y1] in pixels, end-exclusive
 */
export function cropBorders(data, w, h, rect = [0, 0, w, h]) {
  const BLACK = 20;
  const dark = (x, y) => {
    const i = (y * w + x) * 4;
    return data[i] < BLACK && data[i + 1] < BLACK && data[i + 2] < BLACK;
  };
  let [x0, y0, x1, y1] = rect;
  const rw = x1 - x0, rh = y1 - y0;
  const row = (y) => { for (let x = x0; x < x1; x++) if (!dark(x, y)) return false; return true; };
  const col = (x) => { for (let y = y0; y < y1; y++) if (!dark(x, y)) return false; return true; };
  while (y0 < rect[1] + rh * 0.4 && row(y0)) y0++;
  while (y1 > rect[3] - rh * 0.4 && row(y1 - 1)) y1--;
  while (x0 < rect[0] + rw * 0.4 && col(x0)) x0++;
  while (x1 > rect[2] - rw * 0.4 && col(x1 - 1)) x1--;
  return [x0, y0, x1, y1];
}

/**
 * The part of the source the panel frames, and the picture inside it.
 *
 * The picture's rect on screen (frameOf: the video as shown, or the open
 * panel for a cover's light) frames the source like `object-fit: cover`.
 * With the panel closed the whole source is the picture.
 * @returns { shown, rect } — both [x0, y0, x1, y1], integers
 */
export function framing(src, geo) {
  const { w, h } = src;
  const frame = frameOf(geo);
  const ratio = frame ? frame.w / frame.h : null;
  let shown = [0, 0, w, h];
  if (ratio) {
    if (w / h > ratio) {
      const cw = Math.max(1, Math.round(h * ratio));
      const x0 = Math.floor((w - cw) / 2);
      shown = [x0, 0, x0 + cw, h];
    } else {
      const ch = Math.max(1, Math.round(w / ratio));
      const y0 = Math.floor((h - ch) / 2);
      shown = [0, y0, w, y0 + ch];
    }
  }
  return { shown, rect: cropBorders(src.data, w, h, shown) };
}

/** A frame with nothing in it yet: a Canvas fading up from black. */
export function isBlank(src) {
  const d = src.data;
  for (let i = 0; i < d.length; i += 4) if (lum8(d, i) > 0.012) return false;
  return true;
}

/** The grade's statistics are read on every STAT_STEP-th pixel each way —
    a picture's distribution of lightness is the same at a quarter of its
    pixels — into a histogram of STAT_BINS. */
const STAT_STEP = 2;
const STAT_BINS = 4096;

/**
 * The picture's statistics, which both renderers grade with: its exposure
 * (exposureOf) and the share of it too grey to have a hue of its own.
 * @param rect [x0, y0, x1, y1], the picture in the source
 */
export function sourceStats(src, rect) {
  const [x0, y0, x1, y1] = rect;
  const d = src.data;
  const hist = new Uint32Array(STAT_BINS);
  const lab = [0, 0, 0];
  const greyC2 = (C_GREY * 0.5) ** 2;
  let n = 0, greys = 0;
  for (let y = y0; y < y1; y += STAT_STEP) {
    for (let x = x0; x < x1; x += STAT_STEP) {
      const i = (y * src.w + x) * 4;
      labOfLinear(LIN8[d[i]], LIN8[d[i + 1]], LIN8[d[i + 2]], lab);
      hist[Math.min(STAT_BINS - 1, Math.max(0, (lab[0] * STAT_BINS) | 0))]++;
      if (lab[1] * lab[1] + lab[2] * lab[2] < greyC2) greys++;
      n++;
    }
  }
  /* The value of rank floor(f·n), spread evenly across its bin. */
  const q = (f) => {
    const rank = Math.min(n - 1, Math.floor(f * n));
    let below = 0;
    for (let b = 0; b < STAT_BINS; b++) {
      if (below + hist[b] > rank) return (b + (rank - below + 0.5) / hist[b]) / STAT_BINS;
      below += hist[b];
    }
    return 1;
  };
  return { ex: exposureOf(q(0.05), q(0.5), q(0.97)), grey: greys / Math.max(1, n) };
}

/* Where the panel's type sits, as fractions of the panel's frame: the head
   and its tag across the top, the title, artists and meta across the lower
   part. */
const TOP_END = 0.18;
const BOTTOM_START = 0.58;

/** Where the rail's glass is sampled for its colour (toneOf): two columns
    of the frost, as fractions of its width — the rail is the window's left
    fifth or so, and the frost is a 12px blur, so two columns are its
    colour. */
export const TONE_COLUMNS = [0.07, 0.17];

/**
 * The colour of the rail's glass (haze.tone, ambient.svelte.js): the mean
 * of the sampled frost pixels, as [r, g, b] bytes. `samples` is RGBA bytes,
 * however they were gathered — the CPU path picks them out of the frost it
 * holds, the GPU path reads the same columns back in the worker.
 */
export function toneOf(samples) {
  let r = 0, g = 0, b = 0;
  for (let i = 0; i < samples.length; i += 4) {
    r += samples[i];
    g += samples[i + 1];
    b += samples[i + 2];
  }
  const n = Math.max(1, samples.length / 4);
  return [Math.round(r / n), Math.round(g / n), Math.round(b / n)];
}

/** The TONE_COLUMNS of an RGBA picture, one after the other. */
function toneColumns(rgba, w, h) {
  const out = new Uint8Array(TONE_COLUMNS.length * h * 4);
  let o = 0;
  for (const at of TONE_COLUMNS) {
    const x = Math.min(w - 1, Math.floor(w * at));
    for (let y = 0; y < h; y++) {
      const i = (y * w + x) * 4;
      out[o++] = rgba[i];
      out[o++] = rgba[i + 1];
      out[o++] = rgba[i + 2];
      out[o++] = rgba[i + 3];
    }
  }
  return out;
}

/**
 * How bright a picture is under the panel's type: the relative luminance
 * of the bright end (90th percentile) of the top and bottom bands of
 * `shown`, the rectangle the panel covers. `bottomUp` reads WebGL's
 * readPixels rows without copying them into top-down order.
 */
export function lightOf(src, shown, bottomUp = false) {
  const [x0, y0, x1, y1] = shown;
  const rows = y1 - y0;
  const topEnd = y0 + Math.max(1, Math.round(rows * TOP_END));
  const lowStart = y0 + Math.round(rows * BOTTOM_START);
  const band = (from, to) => {
    const out = new Float32Array((to - from) * (x1 - x0));
    let n = 0;
    for (let y = from; y < to; y++) {
      const row = (bottomUp ? src.h - 1 - y : y) * src.w;
      for (let x = x0; x < x1; x++) out[n++] = lum8(src.data, (row + x) * 4);
    }
    return out.length ? round3(select(out, Math.min(out.length - 1, Math.floor(out.length * 0.9)))) : 0;
  };
  return { top: band(y0, topEnd), bottom: band(lowStart, y1) };
}

/** The k-th smallest value (0-based), exactly as a sort would place it;
    reorders `a`. Quickselect, Hoare's partition. */
function select(a, k) {
  let lo = 0, hi = a.length - 1;
  while (lo < hi) {
    const pivot = a[(lo + hi) >> 1];
    let i = lo, j = hi;
    while (i <= j) {
      while (a[i] < pivot) i++;
      while (a[j] > pivot) j--;
      if (i <= j) {
        const t = a[i]; a[i] = a[j]; a[j] = t;
        i++;
        j--;
      }
    }
    if (k <= j) hi = j;
    else if (k >= i) lo = i;
    else break;
  }
  return a[k];
}

const round3 = (v) => Math.round(v * 1000) / 1000;

/* --- The loop's representative frame ----------------------------------- */

const SIG = { x: 3, y: 5 };

/** A coarse picture of a frame's layout of colour: mean Oklab per cell. */
export function signatureOf(src, rect) {
  const [x0, y0, x1, y1] = rect;
  const out = new Float32Array(SIG.x * SIG.y * 3);
  const lab = [0, 0, 0];
  for (let gy = 0; gy < SIG.y; gy++) {
    for (let gx = 0; gx < SIG.x; gx++) {
      const xa = Math.floor(x0 + ((x1 - x0) * gx) / SIG.x), xb = Math.max(xa + 1, Math.floor(x0 + ((x1 - x0) * (gx + 1)) / SIG.x));
      const ya = Math.floor(y0 + ((y1 - y0) * gy) / SIG.y), yb = Math.max(ya + 1, Math.floor(y0 + ((y1 - y0) * (gy + 1)) / SIG.y));
      let r = 0, g = 0, b = 0, n = 0;
      for (let y = ya; y < yb; y++) {
        for (let x = xa; x < xb; x++) {
          const i = (y * src.w + x) * 4;
          r += LIN8[src.data[i]]; g += LIN8[src.data[i + 1]]; b += LIN8[src.data[i + 2]]; n++;
        }
      }
      labOfLinear(r / n, g / n, b / n, lab);
      out.set(lab, (gy * SIG.x + gx) * 3);
    }
  }
  return out;
}

/** Mean Oklab distance between two signatures' cells. */
export function sigDistance(a, b) {
  let sum = 0;
  for (let i = 0; i < a.length; i += 3) sum += Math.hypot(a[i] - b[i], a[i + 1] - b[i + 1], a[i + 2] - b[i + 2]);
  return sum / (a.length / 3);
}

/** The index of the signature most like all the others. */
export function medoid(sigs) {
  let best = 0, bestSum = Infinity;
  for (let i = 0; i < sigs.length; i++) {
    let sum = 0;
    for (let j = 0; j < sigs.length; j++) if (i !== j) sum += sigDistance(sigs[i], sigs[j]);
    if (sum < bestSum) { bestSum = sum; best = i; }
  }
  return best;
}

/** A frame this far from the loop's representative one shows a different
    picture, not the same picture a moment later. */
export const SETTLE_DISTANCE = 0.08;

/* --- Blur -------------------------------------------------------------- */

/** The box radius whose three passes each way are a close Gaussian of
    σ ≈ `sigma` (σ ≈ r + 0.5). Both renderers blur with it. */
export const radiusOf = (sigma) => Math.max(1, Math.round(sigma - 0.5));

/* One pass of a box of radius r along the rows of a `ch`-channel image,
   edges clamped: a running sum per channel, in float64. */
function boxRows(src, dst, w, h, r, ch) {
  const inv = 1 / (2 * r + 1);
  const rowLen = w * ch;
  for (let y = 0; y < h; y++) {
    const base = y * rowLen;
    for (let c = 0; c < ch; c++) {
      const first = base + c, last = first + (w - 1) * ch;
      let a = 0;
      for (let k = -r; k <= r; k++) a += src[first + (k < 0 ? 0 : k > w - 1 ? w - 1 : k) * ch];
      for (let o = first, add = first + (r + 1) * ch, sub = first - r * ch; o <= last; o += ch, add += ch, sub += ch) {
        dst[o] = a * inv;
        a += src[add < last ? add : last] - src[sub > first ? sub : first];
      }
    }
  }
}

/* The same down the columns, a row at a time: one running sum per column
   and channel, so the image is read in order. */
function boxCols(src, dst, w, h, r, ch, acc) {
  const inv = 1 / (2 * r + 1);
  const rowLen = w * ch;
  acc.fill(0, 0, rowLen);
  for (let k = -r; k <= r; k++) {
    const row = (k < 0 ? 0 : k > h - 1 ? h - 1 : k) * rowLen;
    for (let j = 0; j < rowLen; j++) acc[j] += src[row + j];
  }
  for (let y = 0; y < h; y++) {
    const base = y * rowLen;
    const add = (y + r + 1 < h ? y + r + 1 : h - 1) * rowLen;
    const sub = (y - r > 0 ? y - r : 0) * rowLen;
    for (let j = 0; j < rowLen; j++) {
      dst[base + j] = acc[j] * inv;
      acc[j] += src[add + j] - src[sub + j];
    }
  }
}

/** A Gaussian-like blur of σ ≈ `sigma` pixels of a `ch`-channel image, into a new buffer. */
function blur(img, w, h, sigma, ch = 3) {
  const r = radiusOf(sigma);
  const a = new Float32Array(img.length);
  const b = new Float32Array(img.length);
  const acc = new Float64Array(w * ch);
  boxRows(img, b, w, h, r, ch);
  boxCols(b, a, w, h, r, ch, acc);
  for (let i = 1; i < 3; i++) {
    boxRows(a, b, w, h, r, ch);
    boxCols(b, a, w, h, r, ch, acc);
  }
  return a;
}

/* --- Composition ------------------------------------------------------ */

/** The haze's own lens, σ in CSS pixels: soft enough to be light, sharp
    enough that the picture's shapes and edges read through the glass. The
    planes' frost (--frost-plane, app.css) softens it further under them. */
const LENS = 6;
/** Under a cover (not a Canvas) the haze round the tile is this much
    softer, so it reads as the light the tile sits in, not a second copy. */
const UNDER_COVER = 3;
/** The panel's two left corners (--np-radius, app.css), in CSS px: the
    softer haze under a cover is the panel's surface, and is cut to them. */
const CORNER = 18;
/** Left of the picture: the magnification reached far from it, and over
    what share of the space to its left. */
const ZOOM = 2.4;
const ZOOM_REACH = 0.55;
/** How much darker (in L) the window's far edge is than the picture's,
    along the distance to FALL_EXP; chroma falls off as lightness does to
    FALL_CHROMA. */
const FALLOFF = 0.2;
const FALL_EXP = 1.2;
const FALL_CHROMA = 0.6;
/** The pool. A Canvas fills the panel from top to bottom, and its light
    falls off to the left only. A cover is a tile in the panel, POOL_Y of
    the way down it, and under the panel its light is a pool at that
    height: it falls off above and below it as well, by POOL (in L) at the
    panel's further end, along the distance to POOL_EXP — little within
    the tile's own height, most beyond it — so the tile sits in its own
    light and the type at the panel's foot on the quiet end of it. Left of
    the panel the pool dissolves, over POOL_REACH of the space there, into
    the plain fall-off: the panes keep the light they had. */
const POOL = 0.2;
const POOL_EXP = 1.3;
const POOL_Y = 0.44;
const POOL_REACH = 0.6;
/** The lens averages chroma away where colours meet; GIVE_BACK of the
    parts' mean chroma is given back, at most GIVE_MAX times what is left. */
const GIVE_BACK = 0.85;
const GIVE_MAX = 2.2;
/** With the panel closed the glow rises from the player bar: centred
    BAR_X of the window's width in, along the bottom, over BAR_R of its
    width, falling off linearly to BAR_KNEE of that and then at BAR_TAIL. */
const BAR_X = 0.06;
const BAR_R = 1.05;
const BAR_KNEE = 0.5;
const BAR_TAIL = 0.72;
/** The seam. For SEAM CSS pixels left of the video's edge — the gap, the
    planes' rounded corners — and SEAM_IN inside it — the video's own
    rounded corners, where it holds for SEAM_HOLD of SEAM_IN and fades by
    SEAM_REACH — the haze is the video's own light, at SEAM_DIM, fading into
    the graded haze. It is the video's BROAD light, softened by SEAM_SOFT
    (CSS px): the haze is one frame of a moving loop, and a sharp slice of
    it beside the video is a shape the video no longer has a moment later. */
const SEAM = 44;
const SEAM_IN = 22;
const SEAM_HOLD = 0.5;
const SEAM_REACH = 1.5;
const SEAM_DIM = 0.9;
const SEAM_SOFT = 10;
/** The glass carries the seam's light in from its edge: the frost is made
    from the haze as it is for the first SPILL CSS px under the glass, and
    from the calibrated haze (no seam) from there on — so the light flows
    from the gap into the frost and is gone before any type can lie there
    (the pane's padding is at least 18px). GAP is the gutter (--gutter):
    where the glass begins, left of the video's edge. */
const GAP = 8;
const SPILL = 16;

/* CSS's saturate(): the luminance-weighted colour matrix of Filter Effects. */
const SAT_R = 0.213, SAT_G = 0.715, SAT_B = 0.072;

/* The dither's noise: a hash of where the value is — its index in the
   image and its channel — and of the picture's seed, uniform in [0, 1). A
   hash rather than a running generator, so both renderers draw the same
   noise, and the same haze twice is the same bytes. lowbias32 (Chris
   Wellons); the top 24 bits, which a float holds exactly. */
const HASH_M1 = 0x7feb352d;
const HASH_M2 = 0x846ca68b;
const NOISE_BITS = 24;
function hash32(v) {
  v ^= v >>> 16;
  v = Math.imul(v, HASH_M1);
  v ^= v >>> 15;
  v = Math.imul(v, HASH_M2);
  v ^= v >>> 16;
  return v >>> 0;
}
/** The dither keys of a picture's haze and of its frost. */
export const noiseKeys = (seed) => [hash32(2 * seed), hash32(2 * seed + 1)];
const noise = (i, key) => (hash32((i ^ key) >>> 0) >>> (32 - NOISE_BITS)) / 2 ** NOISE_BITS;

/**
 * Every calibrated number the renderers share, by name, for the GPU's
 * shaders (haze-gl.js). The CPU reads the same constants directly.
 */
export const TUNING = {
  floats: {
    SRGB_CUT, SRGB_SLOPE, SRGB_OFFSET, SRGB_GAMMA, SRGB_LINEAR_CUT, LUMA_R, LUMA_G, LUMA_B,
    EPS3, EPS4, EPS5, EPS6,
    ARC_LO, ARC_HI, WARM_TIP, GREEN_TOE, HAZE_FOLD,
    C_GREY, GREY_L, GREY_FLOOR, TINT_CMAX, CLAY_SOFTEN, CLAY_LIFT,
    WARM_PALE, WARM_RICH, WARM_BREATH, WARM_FROM, WARM_IN, WARM_TO, WARM_OUT,
    G_SHADOW, G_LIGHT, G_HI_GAMMA, G_CHROMA, G_CMAX, L_FLOOR, G_VIBRANCE, VIB_KNEE,
    C_MEAN, C_BUDGET_EXP, P_PEAK, P_MEAN, CAP_KEEP, GAMUT_TOL, GAMUT_STEP, Y_MARGIN,
    LOCAL_GAIN, DETAIL_FLOOR, DETAIL_KMIN, DETAIL_KMAX,
    HK_C, HK_BLUE, HK_A, HK_B,
    FALL_CHROMA, POOL, POOL_EXP, POOL_REACH, GIVE_BACK, GIVE_MAX, BAR_KNEE, BAR_TAIL,
    SEAM, SEAM_IN, SEAM_HOLD, SEAM_DIM, GAP, SPILL,
    MEAN: CEILING.mean,
    NOISE_SCALE: 2 ** NOISE_BITS,
  },
  ints: { CAP_ITER, NOISE_SHIFT: 32 - NOISE_BITS },
  uints: { HASH_M1, HASH_M2 },
  mat3: { LMS_OF_RGB, LAB_OF_LMS, LMS_OF_LAB, RGB_OF_LMS },
};

const smooth = (t) => (t <= 0 ? 0 : t >= 1 ? 1 : t * t * (3 - 2 * t));
/** Mirror tiling: 0→1 over one copy, back 1→0 over the next (and the same
    below 0). */
const fold = (t) => {
  const a = Math.abs(t), m = a - 2 * Math.floor(a / 2);
  return m <= 1 ? m : 2 - m;
};

/** Bilinear read of a `ch`-channel image at (u, v) in 0..1, clamped, into out[o..]. */
function sample(img, iw, ih, ch, u, v, out, o) {
  const sx = Math.min(iw - 1, Math.max(0, u * iw - 0.5));
  const sy = Math.min(ih - 1, Math.max(0, v * ih - 0.5));
  const x0 = Math.max(0, Math.min(iw - 2, Math.floor(sx))), y0 = Math.max(0, Math.min(ih - 2, Math.floor(sy)));
  const x1 = Math.min(iw - 1, x0 + 1), y1 = Math.min(ih - 1, y0 + 1);
  const tx = Math.min(1, Math.max(0, sx - x0)), ty = Math.min(1, Math.max(0, sy - y0));
  const i00 = (y0 * iw + x0) * ch, i10 = (y0 * iw + x1) * ch, i01 = (y1 * iw + x0) * ch, i11 = (y1 * iw + x1) * ch;
  for (let c = 0; c < ch; c++) {
    const top = img[i00 + c] + (img[i10 + c] - img[i00 + c]) * tx;
    const bot = img[i01 + c] + (img[i11 + c] - img[i01 + c]) * tx;
    out[o + c] = top + (bot - top) * ty;
  }
}

/** The columns from x0 on of a `ch`-channel image, as their own image. */
function cropCols(img, w, h, x0, ch, x1 = w) {
  const cw = x1 - x0, out = new Float32Array(cw * h * ch);
  for (let y = 0; y < h; y++) out.set(img.subarray((y * w + x0) * ch, (y * w + x1) * ch), y * cw * ch);
  return out;
}

/** The picture's rect in haze pixels: the video as shown, the open panel
    (a cover's light), or null (closed: the whole window). */
function frameOf(geo) {
  if (geo.left == null) return null;
  if (geo.video) return geo.video;
  const x = geo.left * geo.w;
  return { x, y: 0, w: geo.w - x, h: geo.h };
}

/**
 * The layout, per column, left of the picture: where in the picture it
 * reads (u, before the mirror), how much it is magnified (s), and how much
 * it falls off in lightness (F) and chroma (Fc). The magnification is
 * integrated, so it grows smoothly from none at the picture's edge.
 * @returns Float32Array(w * 4) of [u, s, F, Fc]
 */
function columnsOf(frame, w) {
  const cols = new Float32Array(w * 4);
  const reach = ZOOM_REACH * Math.max(1, frame.x);
  let u = 0, prev = 0;
  for (let x = w - 1; x >= 0; x--) {
    const d = frame.x - (x + 0.5);
    const o = x * 4;
    if (d <= 0) {
      cols[o] = -d / frame.w;
      cols[o + 1] = cols[o + 2] = cols[o + 3] = 1;
      continue;
    }
    u += (d - prev) / (frame.w * (1 + (ZOOM - 1) * smooth((d + prev) / 2 / reach)));
    prev = d;
    cols[o] = u;
    cols[o + 1] = 1 + (ZOOM - 1) * smooth(d / reach);
    cols[o + 2] = 1 - FALLOFF * (d / Math.max(1, frame.x)) ** FALL_EXP;
    cols[o + 3] = cols[o + 2] ** FALL_CHROMA;
  }
  return cols;
}

/**
 * Everything both renderers need before a pixel is drawn.
 *
 * @param src   { data, w, h } — RGBA bytes of the whole source
 * @param geo   { w, h, px, left, video, immersive } — the haze's size in
 *              pixels (the window's aspect) and CSS pixels per haze pixel;
 *              the panel's left edge as a fraction of the window (null:
 *              closed); the video's rect on screen, in haze pixels (null:
 *              no video); and whether a Canvas covers the panel
 * @param tint  encoded [r, g, b] of the record's colour, for what has none
 */
export function prepare(src, geo, tint, scale = 1) {
  const { w, h } = geo;
  const px = geo.px ?? 5;
  const frame = frameOf(geo);
  const immersive = Boolean(geo.immersive);
  const video = frame && geo.video && immersive ? frame : null;
  const cover = Boolean(frame) && !immersive;
  const { shown, rect } = framing(src, geo);
  const rw = rect[2] - rect[0], rh = rect[3] - rect[1];
  const { ex, grey } = sourceStats(src, rect);
  const lens = LENS / px;
  /* With the panel closed the whole picture covers the window. */
  const k = Math.max(w / rw, h / rh);
  let seam = null;
  if (video) {
    /* The seam's columns of the haze; and the source's, from its left edge,
       that the mirror reads there, softened by `soft` source pixels. */
    const soft = SEAM_SOFT / ((frame.w * px) / rw);
    seam = {
      x0: Math.max(0, Math.floor(frame.x - SEAM / px)),
      x1: Math.min(w, Math.ceil(frame.x + (SEAM_REACH * SEAM_IN) / px)),
      soft,
      ew: Math.min(rw, Math.ceil(((SEAM + SEAM_REACH * SEAM_IN) / px / frame.w) * rw + 3 * soft) + 2),
    };
  }
  return {
    w, h, px, frame, video, cover, immersive, shown, rect, rw, rh, ex, grey, seam,
    tintLab: toOklab(...tint),
    scale,
    s3: Math.cbrt(scale),
    ycap: CEILING.peak * scale * DITHER_ROOM,
    /* The local contrast's σ, in source pixels; the lens's and the softer
       lens's under a cover, in haze pixels. */
    local: LOCAL_SIGMA * Math.max(rw, rh),
    lens,
    softer: lens * UNDER_COVER,
    /* The cover's softer lens is only ever read on the picture, so only the
       columns from its edge (and the blur's reach) are blurred. */
    softX0: cover ? Math.max(0, Math.floor(frame.x - 3 * lens * UNDER_COVER)) : 0,
    corner: CORNER / px,
    /* The means are over what is seen: left of a Canvas, everything else. */
    measureTo: frame && immersive ? Math.max(1, Math.floor(frame.x)) : w,
    cols: frame ? columnsOf(frame, w) : null,
    pool: cover ? poolOf(frame) : null,
    closed: frame ? null : { k, ox: (w - rw * k) / 2, oy: (h - rh * k) / 2, cx: w * BAR_X, radius: w * BAR_R },
  };
}

/** The pool's height, and the distance from it to the panel's further
    end, in haze pixels. */
function poolOf(frame) {
  const cy = frame.y + frame.h * POOL_Y;
  return { cy, span: Math.max(1, cy - frame.y, frame.y + frame.h - cy) };
}

/** How much of the pool a column takes: all of it under the panel, less
    with the distance left of it. */
function poolReach(frame, x) {
  return 1 - smooth((frame.x - (x + 0.5)) / (POOL_REACH * Math.max(1, frame.x)));
}

/** How far a row is from the pool's height, 0 to 1, shaped. */
function poolDrop(pool, y) {
  return Math.min(1, Math.abs(y + 0.5 - pool.cy) / pool.span) ** POOL_EXP;
}

/** How much the panel-closed glow keeps at haze pixel (x, y): its fall-off. */
function barFalloff(closed, x, y, h) {
  const dx = x + 0.5 - closed.cx, dy = y + 0.5 - h;
  const r = Math.sqrt(dx * dx + dy * dy) / closed.radius;
  return 1 - (r < BAR_KNEE ? r : BAR_KNEE + (r - BAR_KNEE) * BAR_TAIL);
}

/**
 * Render the haze on the CPU: the reference, and the fallback.
 *
 * With the panel open, the picture lies under the video exactly as the
 * video shows it (geo.video), or under the panel as a cover's light (no
 * video); to its left it carries on as its own mirror image — continuous
 * at the edge — magnified with distance up to ZOOM and falling off a
 * little in lightness to the left; under the panel a cover's light is
 * also a pool at its tile's height (POOL). With the panel closed the whole
 * picture covers the window, and a restrained glow rises from the player
 * bar, bottom left.
 *
 * @param src   { data, w, h } — RGBA bytes of the whole source
 * @param geo   see prepare
 * @param tint  encoded [r, g, b] of the record's colour, for what has none
 * @param frost the glass planes' frost (parseFrost), or null for none
 * @returns { rgba, frost, frostW, frostH, tone, gain, grey, light } — the haze as
 *          8-bit RGBA; the same haze frosted (or null), at half its size
 *          (frostW x frostH); the gain the mean ceilings took; the share of
 *          pixels that took the record's tint; and how bright the panel's
 *          picture is under its type (see lightOf)
 */
export function renderHaze(src, geo, tint, { scale = 1, seed = 1, frost = null } = {}) {
  const plan = prepare(src, geo, tint, scale);
  const { w, h, frame, cols, closed, s3, ycap } = plan;
  const one = [0, 0, 0], rgb = [0, 0, 0];

  /* The picture, graded, at the source's resolution. */
  const graded = gradeSource(src, plan);

  /* Lay it out: per column (cols) or, closed, over the whole window. Where
     a column reads in the picture is the same all the way down it. */
  const { rw, rh } = plan;
  const laid = new Float32Array(w * h * 4);
  const cx0 = new Int32Array(w), cx1 = new Int32Array(w), ctx = new Float32Array(w);
  for (let x = 0; x < w; x++) {
    const u = frame ? fold(cols[x * 4]) : (x + 0.5 - closed.ox) / (rw * closed.k);
    const sx = Math.min(rw - 1, Math.max(0, u * rw - 0.5));
    cx0[x] = Math.max(0, Math.min(rw - 2, Math.floor(sx)));
    cx1[x] = Math.min(rw - 1, cx0[x] + 1);
    ctx[x] = Math.min(1, Math.max(0, sx - cx0[x]));
  }
  for (let y = 0; y < h; y++) {
    const vRow = frame ? 0 : (y + 0.5 - closed.oy) / (rh * closed.k);
    for (let x = 0; x < w; x++) {
      const v = frame ? fold(0.5 + ((y + 0.5 - frame.y) / frame.h - 0.5) / cols[x * 4 + 1]) : vRow;
      const sy = Math.min(rh - 1, Math.max(0, v * rh - 0.5));
      const y0 = Math.max(0, Math.min(rh - 2, Math.floor(sy))), y1 = Math.min(rh - 1, y0 + 1);
      const ty = Math.min(1, Math.max(0, sy - y0));
      const tx = ctx[x];
      const i00 = (y0 * rw + cx0[x]) * 4, i10 = (y0 * rw + cx1[x]) * 4, i01 = (y1 * rw + cx0[x]) * 4, i11 = (y1 * rw + cx1[x]) * 4;
      const o = (y * w + x) * 4;
      for (let c = 0; c < 4; c++) {
        const top = graded[i00 + c] + (graded[i10 + c] - graded[i00 + c]) * tx;
        const bot = graded[i01 + c] + (graded[i11 + c] - graded[i01 + c]) * tx;
        laid[o + c] = top + (bot - top) * ty;
      }
    }
  }

  /* The lens, in linear light, with the chroma it averages away given back:
     orange and blue glass blurred together keep the dominant hue at the
     parts' mean chroma rather than turning grey-maroon. Then the fall-off,
     as colour (lightness by f, chroma by f^0.6). The result is kept as
     Oklab, in place. */
  const lensed = blur(laid, w, h, plan.lens, 4);
  const sx0 = plan.softX0, cw = w - sx0;
  const softer = plan.cover ? blur(cropCols(laid, w, h, sx0, 4), cw, h, plan.softer, 4) : null;
  /* How much of the softer lens a pixel takes: all of it on the picture,
     none off it, and in the panel's corners none outside the arc, which is
     anti-aliased — the cut-outs keep the gutter's sharper lens. */
  const R = plan.corner;
  const softness = (x, y) => {
    if (frame.x - (x + 0.5) > 0) return 0;
    const dx = frame.x + R - (x + 0.5);
    const dy = Math.max(R - (y + 0.5), y + 0.5 - (h - R));
    return dx <= 0 || dy <= 0 ? 1 : clamp01(R - Math.sqrt(dx * dx + dy * dy) + 0.5);
  };
  const measureTo = plan.measureTo;
  /* A cover's pool: how much each column takes of it. */
  const reach = plan.pool ? Float32Array.from({ length: w }, (_, x) => poolReach(frame, x)) : null;
  let sumC = 0, nC = 0;
  for (let y = 0; y < h; y++) {
    const drop = plan.pool ? POOL * poolDrop(plan.pool, y) : 0;
    for (let x = 0; x < w; x++) {
      const o = (y * w + x) * 4;
      let r = lensed[o], g = lensed[o + 1], b = lensed[o + 2], mixC = lensed[o + 3];
      const t = softer ? softness(x, y) : 0;
      if (t > 0) {
        const q = (y * cw + x - sx0) * 4;
        r += (softer[q] - r) * t;
        g += (softer[q + 1] - g) * t;
        b += (softer[q + 2] - b) * t;
        mixC += (softer[q + 3] - mixC) * t;
      }
      labOfLinear(r, g, b, one);
      const c = Math.sqrt(one[1] * one[1] + one[2] * one[2]);
      const kr = c > EPS5 ? Math.min(GIVE_MAX, Math.max(1, (mixC * GIVE_BACK) / c)) : 1;
      let f, Fc;
      if (reach) {
        f = cols[x * 4 + 2] * (1 - drop * reach[x]);
        Fc = pow06(f);
      } else if (frame) {
        f = cols[x * 4 + 2];
        Fc = cols[x * 4 + 3];
      } else {
        f = barFalloff(closed, x, y, h);
        Fc = pow06(f);
      }
      const fc = kr * Fc;
      lensed[o] = one[0] * f;
      lensed[o + 1] = one[1] * fc;
      lensed[o + 2] = one[2] * fc;
      if (x < measureTo) {
        sumC += c * fc;
        nC++;
      }
    }
  }

  /* The ceilings: every pixel under the peak; the mean chroma under its
     budget; and the mean of what is seen under both mean ceilings,
     luminance and perceived lightness — a gain that only ever dims. */
  const meanC = sumC / Math.max(1, nC);
  const kc = meanC > C_MEAN * s3 ? ((C_MEAN * s3) / meanC) ** C_BUDGET_EXP : 1;
  const out = laid;
  let sumY = 0, sumP = 0, nY = 0;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const o = (y * w + x) * 4;
      const L = lensed[o], A = lensed[o + 1] * kc, B = lensed[o + 2] * kc;
      linearOfLab(L, A, B, rgb);
      let r = clamp01(rgb[0]), g = clamp01(rgb[1]), b = clamp01(rgb[2]);
      let lum = LUMA_R * r + LUMA_G * g + LUMA_B * b, t = 1;
      if (lum > ycap) {
        const q = ycap / lum;
        r *= q; g *= q; b *= q; lum = ycap;
        t = cbrt(q);
      }
      out[o] = r; out[o + 1] = g; out[o + 2] = b;
      if (x < measureTo) {
        sumY += lum;
        sumP += perceived(L, A, B) * t;
        nY++;
      }
    }
  }
  const meanY = sumY / Math.max(1, nY), meanP = sumP / Math.max(1, nY);
  const gain = Math.min(1, (CEILING.mean * scale) / Math.max(EPS6, meanY), ((P_MEAN * s3) / Math.max(EPS6, meanP)) ** 3);

  return emit(out, gain, plan, src, seed, frost, plan.seam ? seamOf(src, plan) : null);
}

/** The picture graded at the source's resolution, over plan.rect: linear
    colour, with its chroma as a fourth channel. */
function gradeSource(src, plan) {
  const { rw, rh, ex, tintLab, s3, ycap } = plan;
  const [rx0, ry0] = plan.rect;
  const S = src.data;
  const n = rw * rh;
  const one = [0, 0, 0];
  /* Grade it, pixel by pixel, BEFORE the blur: the fold is a hue
     discontinuity by design, and the blur smooths it. Then lay the
     source's fine detail back over the graded broad light. */
  const glab = new Float32Array(n * 3);
  const pair = new Float32Array(n * 2);
  for (let y = 0; y < rh; y++) {
    for (let x = 0; x < rw; x++) {
      const i = ((ry0 + y) * src.w + rx0 + x) * 4, p = y * rw + x;
      labOfLinear(LIN8[S[i]], LIN8[S[i + 1]], LIN8[S[i + 2]], one);
      const L = one[0];
      gradeInto(L, one[1], one[2], ex, tintLab, s3, ycap, one);
      glab[p * 3] = one[0]; glab[p * 3 + 1] = one[1]; glab[p * 3 + 2] = one[2];
      pair[p * 2] = L;
      pair[p * 2 + 1] = one[0];
    }
  }
  const base = blur(pair, rw, rh, plan.local, 2);
  const graded = new Float32Array(n * 4);
  const rgb = [0, 0, 0];
  for (let p = 0; p < n; p++) {
    const Ln = Math.max(DETAIL_FLOOR, base[p * 2 + 1] + LOCAL_GAIN * (pair[p * 2] - base[p * 2]));
    const k = Math.min(DETAIL_KMAX, Math.max(DETAIL_KMIN, Math.sqrt(Ln / Math.max(glab[p * 3], DETAIL_FLOOR))));
    capInto(Ln, glab[p * 3 + 1] * k, glab[p * 3 + 2] * k, s3, ycap, one);
    graded[p * 4 + 3] = Math.sqrt(one[1] * one[1] + one[2] * one[2]);
    linearOfLab(one[0], one[1], one[2], rgb);
    graded[p * 4] = clamp01(rgb[0]);
    graded[p * 4 + 1] = clamp01(rgb[1]);
    graded[p * 4 + 2] = clamp01(rgb[2]);
  }
  return graded;
}

/** The seam: the video's own light, carried out of its edge, as linear
    colour and how much of it each pixel takes, over plan.seam's columns.
    Only the source's columns the mirror reads are softened. */
function seamOf(src, plan) {
  const { h, px, frame, rw, rh } = plan;
  const { x0, x1, soft, ew } = plan.seam;
  const [rx0, ry0] = plan.rect;
  const raw = new Float32Array(ew * rh * 3);
  for (let y = 0; y < rh; y++) {
    for (let x = 0; x < ew; x++) {
      const i = ((ry0 + y) * src.w + rx0 + x) * 4, o = (y * ew + x) * 3;
      raw[o] = LIN8[src.data[i]]; raw[o + 1] = LIN8[src.data[i + 1]]; raw[o + 2] = LIN8[src.data[i + 2]];
    }
  }
  const edge = blur(raw, ew, rh, soft, 3);
  const sw = Math.max(0, x1 - x0);
  const light = new Float32Array(sw * h * 3), alpha = new Float32Array(sw * h);
  for (let y = 0; y < h; y++) {
    const v = fold((y + 0.5 - frame.y) / frame.h);
    for (let x = x0; x < x1; x++) {
      const d = (frame.x - (x + 0.5)) * px;
      const a = d > 0 ? 1 - smooth(d / SEAM) : 1 - smooth((-d - SEAM_HOLD * SEAM_IN) / SEAM_IN);
      if (a <= 0) continue;
      const q = y * sw + (x - x0);
      sample(edge, ew, rh, 3, ((Math.abs(d) / px / frame.w) * rw) / ew, v, light, q * 3);
      light[q * 3] *= SEAM_DIM; light[q * 3 + 1] *= SEAM_DIM; light[q * 3 + 2] *= SEAM_DIM;
      alpha[q] = a;
    }
  }
  return { x0, w: sw, light, alpha };
}

/** Quantise a finished haze (linear, under the peak) with its gain and its
    seam, and everything measured on it: the frost (of the haze without the
    seam, except at the glass's edge: see SPILL), the type's light. */
function emit(out, gain, plan, src, seed, frost, seam) {
  const { w, h, px, frame, shown } = plan;
  const [key, frostKey] = noiseKeys(seed);
  /* Quantise, dithered: each channel rounds up with the probability of its
     fraction, so a gradient's average is exact and its steps dissolve. */
  const rgba = new Uint8ClampedArray(w * h * 4);
  const encoded = frost ? new Float32Array(w * h * 3) : null;
  /* The columns whose frost the seam reaches, from SPILL under the glass
     (and the frost's blur beyond it) to the seam's end: the haze as it is
     there, seam included. Even-aligned, for the frost's half resolution. */
  const reach = frost ? Math.ceil(3 * frost.sigma) + 2 : 0;
  const lx0 = frost && seam ? Math.max(0, Math.floor(frame.x - (GAP + SPILL) / px - reach) & ~1) : 0;
  const lx1 = frost && seam ? Math.min(w, seam.x0 + seam.w + reach) : 0;
  const lw = lx1 - lx0;
  const lit = lw > 0 ? new Float32Array(lw * h * 3) : null;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const p = y * w + x, o = p * 4, e = p * 3;
      let r = out[o] * gain, g = out[o + 1] * gain, b = out[o + 2] * gain;
      let er = encFast(r), eg = encFast(g), eb = encFast(b);
      if (encoded) {
        encoded[e] = er; encoded[e + 1] = eg; encoded[e + 2] = eb;
      }
      const s = seam && x >= seam.x0 && x < seam.x0 + seam.w ? y * seam.w + (x - seam.x0) : -1;
      const a = s >= 0 ? seam.alpha[s] : 0;
      if (a > 0) {
        r += (seam.light[s * 3] - r) * a;
        g += (seam.light[s * 3 + 1] - g) * a;
        b += (seam.light[s * 3 + 2] - b) * a;
        er = encFast(r); eg = encFast(g); eb = encFast(b);
      }
      if (lit && x >= lx0 && x < lx1) {
        const l = (y * lw + x - lx0) * 3;
        lit[l] = er; lit[l + 1] = eg; lit[l + 2] = eb;
      }
      rgba[o] = Math.floor(er * 255 + noise(o, key));
      rgba[o + 1] = Math.floor(eg * 255 + noise(o + 1, key));
      rgba[o + 2] = Math.floor(eb * 255 + noise(o + 2, key));
      rgba[o + 3] = 255;
    }
  }
  /* The type's light: over a Canvas it is the picture; over a cover (or
     nothing) the type in the panel sits on the haze itself. */
  const light = frame && !plan.immersive
    ? lightOf({ data: rgba, w, h }, [Math.min(w - 1, Math.ceil(frame.x)), 0, w, h])
    : lightOf(src, shown);
  /* The frost is at half the haze's resolution: its blur is many times
     coarser than two haze pixels, so nothing is lost, and it is a quarter
     of the work and of the texture. */
  const fw = Math.ceil(w / 2), fh = Math.ceil(h / 2);
  let frosted = null;
  if (frost) {
    const half = { ...frost, sigma: frost.sigma / 2 };
    const values = frostValues(halve(encoded, w, h), fw, fh, half);
    if (lit) {
      /* The glass's edge: the frost of the haze as it is, handing over to
         the calibrated frost across SPILL. */
      const hw = Math.ceil(lw / 2), fx0 = lx0 / 2;
      const edge = frostValues(halve(lit, lw, h), hw, fh, half);
      for (let x = 0; x < hw && fx0 + x < fw; x++) {
        const k = spillOf(frame, px, fx0 + x);
        if (k <= 0) continue;
        for (let y = 0; y < fh; y++) {
          const q = (y * fw + fx0 + x) * 3, e = (y * hw + x) * 3;
          for (let c = 0; c < 3; c++) values[q + c] += (edge[e + c] - values[q + c]) * k;
        }
      }
    }
    frosted = quantise(values, fw, fh, frostKey);
  }
  return {
    rgba,
    frost: frosted,
    frostW: fw,
    frostH: fh,
    tone: frosted ? toneOf(toneColumns(frosted, fw, fh)) : null,
    gain,
    grey: plan.grey,
    light,
  };
}

/** How much of the seam-lit frost the frost's column `fx` (half
    resolution) takes: all of it at the glass's edge, none past SPILL. */
function spillOf(frame, px, fx) {
  const d = (frame.x - 2 * (fx + 0.5)) * px - GAP;
  return 1 - smooth(d / SPILL);
}

/* --- The frost --------------------------------------------------------- */

/** A 3-channel image at half its size, each pixel the mean of its 2x2 block
    (the last row and column clamped). */
function halve(img, w, h) {
  const hw = Math.ceil(w / 2), hh = Math.ceil(h / 2), out = new Float32Array(hw * hh * 3);
  for (let y = 0; y < hh; y++) {
    const y0 = 2 * y, y1 = Math.min(h - 1, y0 + 1);
    for (let x = 0; x < hw; x++) {
      const x0 = 2 * x, x1 = Math.min(w - 1, x0 + 1), o = (y * hw + x) * 3;
      for (let c = 0; c < 3; c++) {
        out[o + c] = (img[(y0 * w + x0) * 3 + c] + img[(y0 * w + x1) * 3 + c] + img[(y1 * w + x0) * 3 + c] + img[(y1 * w + x1) * 3 + c]) / 4;
      }
    }
  }
  return out;
}

/**
 * Read the glass's frost token — `blur(36px) saturate(1.45) brightness(0.7)`
 * — as the numbers frostOf needs. Any function missing is the identity; a
 * token with no blur is no frost at all.
 * @param value      the token's computed value
 * @param pxPerHaze  CSS pixels per haze pixel
 */
export function parseFrost(value, pxPerHaze) {
  const num = (name) => {
    const m = String(value ?? "").match(new RegExp(name + String.raw`\(\s*([\d.]+)(px|%)?\s*\)`));
    if (!m) return null;
    return m[2] === "%" ? Number(m[1]) / 100 : Number(m[1]);
  };
  const blurPx = num("blur");
  if (!blurPx) return null;
  return { sigma: blurPx / pxPerHaze, saturate: num("saturate") ?? 1, brightness: num("brightness") ?? 1 };
}

/** CSS's saturate(s) as a colour matrix, by rows. */
export function saturateMatrix(s) {
  return [
    SAT_R + (1 - SAT_R) * s, SAT_G - SAT_G * s, SAT_B - SAT_B * s,
    SAT_R - SAT_R * s, SAT_G + (1 - SAT_G) * s, SAT_B - SAT_B * s,
    SAT_R - SAT_R * s, SAT_G - SAT_G * s, SAT_B + (1 - SAT_B) * s,
  ];
}

/* The frost's values, encoded and before quantising: blurred, saturated
   and dimmed on ENCODED values, each clamped to [0, 1] as the filter chain
   clamps between its steps. */
function frostValues(encoded, w, h, frost) {
  const b = blur(encoded, w, h, frost.sigma);
  const m = saturateMatrix(frost.saturate), k = frost.brightness;
  for (let p = 0; p < w * h; p++) {
    const o = p * 3;
    const r = b[o], g = b[o + 1], bl = b[o + 2];
    for (let c = 0; c < 3; c++) b[o + c] = clamp01(clamp01(m[c * 3] * r + m[c * 3 + 1] * g + m[c * 3 + 2] * bl) * k);
  }
  return b;
}

/** 3-channel encoded values (0..1) as dithered 8-bit RGBA. */
function quantise(values, w, h, key) {
  const out = new Uint8ClampedArray(w * h * 4);
  for (let p = 0; p < w * h; p++) {
    const o = p * 3, q = p * 4;
    out[q] = Math.floor(values[o] * 255 + noise(q, key));
    out[q + 1] = Math.floor(values[o + 1] * 255 + noise(q + 1, key));
    out[q + 2] = Math.floor(values[o + 2] * 255 + noise(q + 2, key));
    out[q + 3] = 255;
  }
  return out;
}

/**
 * The haze as the glass planes show it: what `backdrop-filter: blur()
 * saturate() brightness()` would make of it, computed once per haze
 * instead of by the compositor on every frame anything moves.
 *
 * The same arithmetic as CSS, so a plane over this looks as it did over
 * the live filter and the glass's calibration holds: the blur, the
 * saturate matrix and the brightness all act on ENCODED values, each
 * clamped to [0, 1] as the filter chain clamps between its steps. Then
 * dithered like the haze, which the live filter could not be.
 *
 * @param encoded  Float32Array(w * h * 3) of the haze, encoded, 0..1
 * @param frost    { sigma (haze px), saturate, brightness } — see parseFrost
 */
export function frostOf(encoded, w, h, frost, key = 0) {
  return quantise(frostValues(encoded, w, h, frost), w, h, key);
}

/** A source of one colour: what stands in for a picture that cannot be read. */
export function solidSource(rgb) {
  const data = new Uint8ClampedArray(4 * 4 * 4);
  for (let i = 0; i < data.length; i += 4) {
    data[i] = Math.round(rgb[0] * 255);
    data[i + 1] = Math.round(rgb[1] * 255);
    data[i + 2] = Math.round(rgb[2] * 255);
    data[i + 3] = 255;
  }
  return { data, w: 4, h: 4 };
}
