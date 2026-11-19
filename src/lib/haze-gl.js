/* =====================================================================
   THE HAZE — the GPU's renderer

   renderHaze (haze.js) as WebGL2 passes, on the worker's OffscreenCanvas:
   the same pipeline stage for stage, with what both need worked out once
   by haze.js (prepare) and every calibrated number handed to the shaders
   from there (TUNING) — the shaders carry no numbers of their own. The
   CPU's renderer is the reference this one is checked against, and the
   fallback when a context cannot be had or is lost for good.

   The passes, each a full-screen triangle into a float texture:
     grade    the source over the picture's rect, graded (Oklab), with its
              own lightness kept for the local contrast
     blur ×6  three box passes each way (radiusOf), as the CPU blurs
     detail   the local contrast laid back, capped, as linear colour
     layout   laid out over the window: per column (cols) or, closed, whole
     blur ×6  the lens; and the softer lens under a cover
     mix      the lenses mixed at the panel's corners, the chroma given
              back, the fall-off (a cover's pool, a Canvas's columns, the
              bar's glow): Oklab
     measure  the mean chroma, then the means of luminance and perceived
              lightness, summed 8×8 at a time down to one texel, which the
              next pass reads — no round trip to the CPU
     haze     the ceilings, the gain, the seam, dithered: the picture
     half, frost  its frosted twin at half size, and the seam-lit frost
              at the glass's edge
   Over a cover the type's light is measured on the haze itself, so that
   pass reads back the part under the panel. Only columns reached by a
   visible softer lens or seam-lit frost sample need blurring. Scratch is
   reused after its last reader, briefly across nearby renders, never at rest.
   ===================================================================== */

import { prepare, radiusOf, noiseKeys, saturateMatrix, lightOf, toneOf, TONE_COLUMNS, TUNING } from "./haze.js";

/** The measure passes sum this many texels each way at a time. */
const BLOCK = 8;
/* Span a debounced layout change (250 ms), not the time between tracks. */
const BURST_MS = 500;
const BURST_BYTES = 8 * 1024 * 1024;

/** A number as a GLSL float literal. */
const glFloat = (v) => {
  const s = String(v);
  return /[.e]/.test(s) ? s : `${s}.0`;
};

/** TUNING as #defines; matrices by columns, as GLSL reads them. */
function defines() {
  const lines = [];
  for (const [k, v] of Object.entries(TUNING.floats)) lines.push(`#define ${k} ${glFloat(v)}`);
  for (const [k, v] of Object.entries(TUNING.ints)) lines.push(`#define ${k} ${v}`);
  for (const [k, v] of Object.entries(TUNING.uints)) lines.push(`#define ${k} ${v >>> 0}u`);
  for (const [k, m] of Object.entries(TUNING.mat3)) {
    const cols = [0, 1, 2].flatMap((c) => [m[c], m[3 + c], m[6 + c]]);
    lines.push(`#define ${k} mat3(${cols.map(glFloat).join(", ")})`);
  }
  lines.push(`#define BLOCK ${BLOCK}`);
  return lines.join("\n");
}

const VERTEX = `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

/* What every pass shares: the colour arithmetic of haze.js. */
const COMMON = `
out vec4 outColor;

float clamp01(float v) { return clamp(v, 0.0, 1.0); }
float smooth01(float t) { t = clamp01(t); return t * t * (3.0 - 2.0 * t); }
float linOf(float c) { return c <= SRGB_CUT ? c / SRGB_SLOPE : pow((c + SRGB_OFFSET) / (1.0 + SRGB_OFFSET), SRGB_GAMMA); }
vec3 linOf3(vec3 c) { return vec3(linOf(c.r), linOf(c.g), linOf(c.b)); }
float encOf(float y) {
  y = clamp01(y);
  return y <= SRGB_LINEAR_CUT ? y * SRGB_SLOPE : (1.0 + SRGB_OFFSET) * pow(y, 1.0 / SRGB_GAMMA) - SRGB_OFFSET;
}
vec3 encOf3(vec3 c) { return vec3(encOf(c.r), encOf(c.g), encOf(c.b)); }
float cbrt(float x) { return sign(x) * pow(abs(x), 1.0 / 3.0); }
vec3 labOfLinear(vec3 c) {
  vec3 lms = LMS_OF_RGB * c;
  return LAB_OF_LMS * vec3(cbrt(lms.x), cbrt(lms.y), cbrt(lms.z));
}
vec3 linearOfLab(vec3 lab) {
  vec3 lms = LMS_OF_LAB * lab;
  return RGB_OF_LMS * (lms * lms * lms);
}
float lumOf(vec3 c) { return dot(c, vec3(LUMA_R, LUMA_G, LUMA_B)); }
float perceived(vec3 lab) { return lab.x + HK_C * length(lab.yz) + HK_BLUE * max(0.0, lab.y * HK_A + lab.z * HK_B); }
float mirror(float t) {
  float a = abs(t), m = a - 2.0 * floor(a / 2.0);
  return m <= 1.0 ? m : 2.0 - m;
}
uint hash32(uint v) {
  v ^= v >> 16;
  v *= HASH_M1;
  v ^= v >> 15;
  v *= HASH_M2;
  v ^= v >> 16;
  return v;
}
float noiseAt(int i, uint key) { return float(hash32(uint(i) ^ key) >> NOISE_SHIFT) / NOISE_SCALE; }
/* A bilinear read at (u, v) in 0..1, clamped, exactly as haze.js samples. */
vec4 bilinear(sampler2D tex, float u, float v) {
  ivec2 size = textureSize(tex, 0);
  float iw = float(size.x), ih = float(size.y);
  float sx = min(iw - 1.0, max(0.0, u * iw - 0.5));
  float sy = min(ih - 1.0, max(0.0, v * ih - 0.5));
  int x0 = max(0, min(size.x - 2, int(floor(sx)))), y0 = max(0, min(size.y - 2, int(floor(sy))));
  int x1 = min(size.x - 1, x0 + 1), y1 = min(size.y - 1, y0 + 1);
  float tx = clamp01(sx - float(x0)), ty = clamp01(sy - float(y0));
  vec4 a = texelFetch(tex, ivec2(x0, y0), 0), b = texelFetch(tex, ivec2(x1, y0), 0);
  vec4 c = texelFetch(tex, ivec2(x0, y1), 0), d = texelFetch(tex, ivec2(x1, y1), 0);
  vec4 top = a + (b - a) * tx, bot = c + (d - c) * tx;
  return top + (bot - top) * ty;
}
`;

/* The grade: haze.js's gradeInto and capInto. */
const GRADE = `
uniform vec4 uEx;
uniform vec3 uTint;
uniform float uS3;
uniform float uYcap;

vec3 capInto(float Lo, float A, float B) {
  float cap = P_PEAK * uS3;
  float p = perceived(vec3(Lo, A, B));
  if (p > cap) {
    float c = sqrt(A * A + B * B);
    float kc = (p - Lo) / max(c, EPS6);
    float c2 = max(c * CAP_KEEP, min(c, (cap - Lo) / kc));
    A *= c2 / max(c, EPS6);
    B *= c2 / max(c, EPS6);
    Lo = min(Lo, cap - kc * c2);
  }
  for (int i = 0; i < CAP_ITER; i++) {
    vec3 rgb = linearOfLab(vec3(Lo, A, B));
    if (any(lessThan(rgb, vec3(-GAMUT_TOL))) || any(greaterThan(rgb, vec3(1.0 + GAMUT_TOL)))) {
      A *= GAMUT_STEP;
      B *= GAMUT_STEP;
      continue;
    }
    float y = lumOf(rgb);
    if (y > uYcap) {
      Lo *= cbrt(uYcap / y) * Y_MARGIN;
      continue;
    }
    break;
  }
  return vec3(Lo, A, B);
}

float keyOf(float L) {
  if (L <= uEx.y) return G_SHADOW + (uEx.w - G_SHADOW) * clamp01((L - uEx.x) / max(EPS3, uEx.y - uEx.x));
  return uEx.w + (G_LIGHT - uEx.w) * pow(clamp01((L - uEx.y) / max(EPS3, uEx.z - uEx.y)), G_HI_GAMMA);
}

vec3 gradeInto(vec3 lab) {
  float L = lab.x, A = lab.y, B = lab.z;
  float C = sqrt(A * A + B * B);
  float source = C;
  float grey = C_GREY * min(1.0, max(GREY_FLOOR, L / GREY_L));
  float tg = clamp01(1.0 - C / grey);
  if (tg > 0.0) {
    float tc = length(uTint.yz);
    if (tc > EPS4) {
      float target = min(tc, TINT_CMAX);
      A += tg * (uTint.y / tc * target - A);
      B += tg * (uTint.z / tc * target - B);
      C = sqrt(A * A + B * B);
    }
  }
  float clay = 0.0;
  float hue = -1.0;
  vec2 warmEdge = vec2(cos(radians(WARM_FROM)), sin(radians(WARM_FROM)));
  vec2 arcEdge = vec2(cos(radians(ARC_HI)), sin(radians(ARC_HI)));
  if (warmEdge.x * B - warmEdge.y * A >= 0.0 && A * arcEdge.y - B * arcEdge.x >= 0.0) {
    hue = (A == 0.0 && B == 0.0) ? 0.0 : degrees(atan(B, A));
    if (C > EPS4 && hue >= ARC_LO) {
      if (hue <= ARC_HI) {
        hue = hue < HAZE_FOLD
          ? ARC_LO + (hue - ARC_LO) / (HAZE_FOLD - ARC_LO) * (WARM_TIP - ARC_LO)
          : GREEN_TOE + (hue - HAZE_FOLD) / (ARC_HI - HAZE_FOLD) * (ARC_HI - GREEN_TOE);
      }
      clay = hue > WARM_TIP ? 0.0 : max(0.0, (hue - ARC_LO) / (WARM_TIP - ARC_LO));
      float c = C * (1.0 - CLAY_SOFTEN * clay);
      A = c * cos(radians(hue));
      B = c * sin(radians(hue));
      C = c;
    }
  }
  float Lo = (keyOf(L) + CLAY_LIFT * clay) * uS3;
  float vib = 1.0 + G_VIBRANCE * (1.0 - smooth01(source / VIB_KNEE));
  float k = C > EPS6 ? min(C * G_CHROMA * vib * sqrt(Lo / max(L, L_FLOOR)), G_CMAX) / C : 0.0;
  A *= k;
  B *= k;
  float warm = 0.0;
  if (k > 0.0 && hue >= WARM_FROM && hue <= WARM_TO) warm = smooth01(min((hue - WARM_FROM) / WARM_IN, (WARM_TO - hue) / WARM_OUT));
  if (warm > 0.0) {
    float c = sqrt(A * A + B * B);
    float genuine = smooth01((source - WARM_PALE) / (WARM_RICH - WARM_PALE));
    float kept = min(c, WARM_BREATH + (c - WARM_BREATH) * genuine);
    float f = c > EPS6 ? (c + warm * (kept - c)) / c : 1.0;
    A *= f;
    B *= f;
  }
  return capInto(Lo, A, B);
}
`;

/* The ceilings and the gain, from the two measured texels. */
const MEANS = `
uniform sampler2D uSumC;
uniform float uS3;
uniform float uYcap;

float kcOf() {
  vec4 s = texelFetch(uSumC, ivec2(0), 0);
  float meanC = s.x / max(1.0, s.y);
  return meanC > C_MEAN * uS3 ? pow(C_MEAN * uS3 / meanC, C_BUDGET_EXP) : 1.0;
}
`;

/* The same ordered sum for every blur. Only the first horizontal pass
   can read a wider source, or project the grade's two lightness lanes. */
function boxShader({ crop = false, local = false } = {}) {
  return `
uniform sampler2D uTex;
uniform ivec2 uDir;
uniform int uR;
${crop ? "uniform int uX0;" : ""}
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy)${crop ? " + ivec2(uX0, 0)" : ""}, last = textureSize(uTex, 0) - 1;
  vec4 acc = vec4(0.0);
  for (int k = -uR; k <= uR; k++) acc += texelFetch(uTex, clamp(p + uDir * k, ivec2(0), last), 0);
  outColor = ${local ? "vec4(acc.xw, 0.0, 1.0)" : "acc"} / float(2 * uR + 1);
}`;
}

const PASSES = {
  box: boxShader(),
  boxCrop: boxShader({ crop: true }),
  boxLocal: boxShader({ local: true }),

  sum: `
uniform sampler2D uTex;
void main() {
  ivec2 o = ivec2(gl_FragCoord.xy) * BLOCK, size = textureSize(uTex, 0);
  vec4 s = vec4(0.0);
  for (int j = 0; j < BLOCK; j++) for (int i = 0; i < BLOCK; i++) {
    ivec2 q = o + ivec2(i, j);
    if (q.x < size.x && q.y < size.y) s += texelFetch(uTex, q, 0);
  }
  outColor = s;
}`,

  grade: `${GRADE}
uniform sampler2D uSrc;
uniform ivec2 uOrigin;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec3 lab = labOfLinear(linOf3(texelFetch(uSrc, uOrigin + p, 0).rgb));
  outColor = vec4(gradeInto(lab), lab.x);
}`,

  detail: `${GRADE}
uniform sampler2D uGlab;
uniform sampler2D uBase;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec4 g = texelFetch(uGlab, p, 0), b = texelFetch(uBase, p, 0);
  float Ln = max(DETAIL_FLOOR, b.x + LOCAL_GAIN * (g.w - b.y));
  float k = min(DETAIL_KMAX, max(DETAIL_KMIN, sqrt(Ln / max(g.x, DETAIL_FLOOR))));
  vec3 lab = capInto(Ln, g.y * k, g.z * k);
  outColor = vec4(clamp(linearOfLab(lab), 0.0, 1.0), length(lab.yz));
}`,

  layout: `
uniform sampler2D uGraded;
uniform sampler2D uCols;
uniform bool uFramed;
uniform vec4 uFrame;
uniform vec3 uClosed;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec2 size = vec2(textureSize(uGraded, 0));
  float u, v;
  if (uFramed) {
    vec4 col = texelFetch(uCols, ivec2(p.x, 0), 0);
    u = mirror(col.x);
    v = mirror(0.5 + ((float(p.y) + 0.5 - uFrame.y) / uFrame.w - 0.5) / col.y);
  } else {
    u = (float(p.x) + 0.5 - uClosed.x) / (size.x * uClosed.z);
    v = (float(p.y) + 0.5 - uClosed.y) / (size.y * uClosed.z);
  }
  outColor = bilinear(uGraded, u, v);
}`,

  mix: `
uniform sampler2D uLensed;
uniform sampler2D uSofter;
uniform int uSoftX;
uniform sampler2D uCols;
uniform bool uFramed;
uniform bool uCover;
uniform vec4 uFrame;
uniform float uCorner;
uniform vec2 uPool;
uniform vec2 uBar;
uniform float uH;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  float x = float(p.x) + 0.5, y = float(p.y) + 0.5;
  vec4 l = texelFetch(uLensed, p, 0);
  if (uCover && uFrame.x - x <= 0.0) {
    float R = uCorner;
    float dx = uFrame.x + R - x, dy = max(R - y, y - (uH - R));
    float t = dx <= 0.0 || dy <= 0.0 ? 1.0 : clamp01(R - sqrt(dx * dx + dy * dy) + 0.5);
    if (t > 0.0) l += (texelFetch(uSofter, ivec2(p.x - uSoftX, p.y), 0) - l) * t;
  }
  vec3 lab = labOfLinear(l.rgb);
  float c = length(lab.yz);
  float kr = c > EPS5 ? min(GIVE_MAX, max(1.0, l.a * GIVE_BACK / c)) : 1.0;
  float f, Fc;
  if (uCover) {
    float reach = 1.0 - smooth01((uFrame.x - x) / (POOL_REACH * max(1.0, uFrame.x)));
    float drop = POOL * pow(min(1.0, abs(y - uPool.x) / uPool.y), POOL_EXP);
    f = texelFetch(uCols, ivec2(p.x, 0), 0).z * (1.0 - drop * reach);
    Fc = pow(f, FALL_CHROMA);
  } else if (uFramed) {
    vec4 col = texelFetch(uCols, ivec2(p.x, 0), 0);
    f = col.z;
    Fc = col.w;
  } else {
    float r = length(vec2(x - uBar.x, y - uH)) / uBar.y;
    f = 1.0 - (r < BAR_KNEE ? r : BAR_KNEE + (r - BAR_KNEE) * BAR_TAIL);
    Fc = pow(max(f, 0.0), FALL_CHROMA);
  }
  outColor = vec4(lab.x * f, lab.yz * (kr * Fc), 0.0);
}`,

  measureC: `
uniform sampler2D uLab;
uniform int uMeasureTo;
void main() {
  ivec2 o = ivec2(gl_FragCoord.xy) * BLOCK, size = textureSize(uLab, 0);
  vec4 s = vec4(0.0);
  for (int j = 0; j < BLOCK; j++) for (int i = 0; i < BLOCK; i++) {
    ivec2 q = o + ivec2(i, j);
    if (q.x < min(size.x, uMeasureTo) && q.y < size.y) s += vec4(length(texelFetch(uLab, q, 0).yz), 1.0, 0.0, 0.0);
  }
  outColor = s;
}`,

  measureYP: `${MEANS}
uniform sampler2D uLab;
uniform int uMeasureTo;
void main() {
  float kc = kcOf();
  ivec2 o = ivec2(gl_FragCoord.xy) * BLOCK, size = textureSize(uLab, 0);
  vec4 s = vec4(0.0);
  for (int j = 0; j < BLOCK; j++) for (int i = 0; i < BLOCK; i++) {
    ivec2 q = o + ivec2(i, j);
    if (q.x >= min(size.x, uMeasureTo) || q.y >= size.y) continue;
    vec3 lab = texelFetch(uLab, q, 0).xyz;
    lab.yz *= kc;
    vec3 rgb = clamp(linearOfLab(lab), 0.0, 1.0);
    float y = lumOf(rgb), t = 1.0;
    if (y > uYcap) {
      t = cbrt(uYcap / y);
      y = uYcap;
    }
    s += vec4(y, perceived(lab) * t, 1.0, 0.0);
  }
  outColor = s;
}`,

  raw: `
uniform sampler2D uSrc;
uniform ivec2 uOrigin;
void main() {
  outColor = vec4(linOf3(texelFetch(uSrc, uOrigin + ivec2(gl_FragCoord.xy), 0).rgb), 1.0);
}`,
};

/* The finished haze at a pixel: the ceilings, the gain, and the seam. */
const HAZE = `${MEANS}
uniform sampler2D uLab;
uniform sampler2D uSumYP;
uniform sampler2D uEdge;
uniform float uScale;
uniform float uPx;
uniform bool uSeam;
uniform ivec2 uSeamX;
uniform vec4 uFrame;
uniform float uRw;

float gainOf() {
  vec4 s = texelFetch(uSumYP, ivec2(0), 0);
  float n = max(1.0, s.z);
  float r = P_MEAN * uS3 / max(EPS6, s.y / n);
  return min(1.0, min(MEAN * uScale / max(EPS6, s.x / n), r * r * r));
}
vec3 hazeLinear(ivec2 p, float kc, float gain) {
  vec3 lab = texelFetch(uLab, p, 0).xyz;
  vec3 rgb = clamp(linearOfLab(vec3(lab.x, lab.yz * kc)), 0.0, 1.0);
  float y = lumOf(rgb);
  if (y > uYcap) rgb *= uYcap / y;
  return rgb * gain;
}
vec3 withSeam(vec3 rgb, ivec2 p) {
  if (!uSeam || p.x < uSeamX.x || p.x >= uSeamX.y) return rgb;
  float d = (uFrame.x - (float(p.x) + 0.5)) * uPx;
  float a = d > 0.0 ? 1.0 - smooth01(d / SEAM) : 1.0 - smooth01((-d - SEAM_HOLD * SEAM_IN) / SEAM_IN);
  if (a <= 0.0) return rgb;
  float v = mirror((float(p.y) + 0.5 - uFrame.y) / uFrame.w);
  float ew = float(textureSize(uEdge, 0).x);
  vec3 light = bilinear(uEdge, (abs(d) / uPx / uFrame.z * uRw) / ew, v).rgb * SEAM_DIM;
  return rgb + (light - rgb) * a;
}
`;

PASSES.haze = `${HAZE}
uniform uint uKey;
uniform int uW;
uniform int uH;
void main() {
  ivec2 p = ivec2(int(gl_FragCoord.x), uH - 1 - int(gl_FragCoord.y));
  vec3 e = encOf3(withSeam(hazeLinear(p, kcOf(), gainOf()), p));
  int i = (p.y * uW + p.x) * 4;
  outColor = vec4(floor(e.r * 255.0 + noiseAt(i, uKey)), floor(e.g * 255.0 + noiseAt(i + 1, uKey)), floor(e.b * 255.0 + noiseAt(i + 2, uKey)), 255.0) / 255.0;
}`;

PASSES.half = `${HAZE}
uniform bool uLit;
uniform int uHalfX;
uniform int uW;
uniform int uH;
vec3 encodedAt(ivec2 p, float kc, float gain) {
  vec3 rgb = hazeLinear(p, kc, gain);
  return encOf3(uLit ? withSeam(rgb, p) : rgb);
}
void main() {
  float kc = kcOf(), gain = gainOf();
  ivec2 h = ivec2(gl_FragCoord.xy) + ivec2(uHalfX, 0);
  int x0 = 2 * h.x, y0 = 2 * h.y, x1 = min(uW - 1, x0 + 1), y1 = min(uH - 1, y0 + 1);
  vec3 s = encodedAt(ivec2(x0, y0), kc, gain) + encodedAt(ivec2(x1, y0), kc, gain) + encodedAt(ivec2(x0, y1), kc, gain) + encodedAt(ivec2(x1, y1), kc, gain);
  outColor = vec4(s / 4.0, 1.0);
}`;

PASSES.frost = `
uniform sampler2D uFrost;
uniform sampler2D uFrostLit;
uniform int uLitX;
uniform ivec2 uLitRange;
uniform mat3 uSat;
uniform float uBright;
uniform vec4 uFrame;
uniform float uPx;
uniform uint uKey;
uniform int uW;
uniform int uH;
/* The gathered twin (haze.js, parseEdge): the frost under the plane's tint,
   through --frost-edge, less the tint laid back on it. */
uniform bool uGather;
uniform ivec2 uToneX;
uniform float uGain;
uniform mat3 uEdgeSat;
uniform float uAlpha;
uniform vec3 uTint;
vec3 frosted(vec3 b) { return clamp(clamp(uSat * b, 0.0, 1.0) * uBright, 0.0, 1.0); }
vec3 gathered(vec3 v) {
  vec3 f = clamp(uEdgeSat * clamp(mix(v, uTint, uAlpha) * uGain, 0.0, 1.0), 0.0, 1.0);
  return clamp((f - uAlpha * uTint) / (1.0 - uAlpha), 0.0, 1.0);
}
void main() {
  ivec2 fp = ivec2(gl_FragCoord.xy);
  bool toneRow = uGather && fp.y >= uH;
  ivec2 p = uGather ? fp : ivec2(fp.x, uH - 1 - fp.y);
  if (toneRow) {
    /* Extra rows pack the *ungathered* frost at the rail's two columns.
       Address and dither at the original pixel, not at its atlas slot. */
    int slot = (fp.y - uH) * uW + fp.x;
    p = ivec2(slot < uH ? uToneX.x : uToneX.y, slot % uH);
  }
  vec3 v = frosted(texelFetch(uFrost, p, 0).rgb);
  if (p.x >= uLitRange.x && p.x < uLitRange.y) {
    float k = 1.0 - smooth01(((uFrame.x - 2.0 * (float(p.x) + 0.5)) * uPx - GAP) / SPILL);
    if (k > 0.0) v += (frosted(texelFetch(uFrostLit, ivec2(p.x - uLitX, p.y), 0).rgb) - v) * k;
  }
  if (uGather && !toneRow) v = gathered(v);
  int i = (p.y * uW + p.x) * 4;
  outColor = vec4(floor(v.r * 255.0 + noiseAt(i, uKey)), floor(v.g * 255.0 + noiseAt(i + 1, uKey)), floor(v.b * 255.0 + noiseAt(i + 2, uKey)), 255.0) / 255.0;
}`;

/** Complete fragment shaders with the shared TUNING definitions. */
export function shaderSources() {
  const head = `#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;\n${defines()}\n${COMMON}`;
  return Object.fromEntries(Object.entries(PASSES).map(([name, body]) => [name, head + body]));
}

/**
 * The GPU's renderer, or null where WebGL2 (with float render targets)
 * cannot be had. `render` answers null while the context is lost; the
 * caller renders on the CPU then.
 */
export function createGlHaze() {
  if (typeof OffscreenCanvas !== "function") return null;
  const canvas = new OffscreenCanvas(1, 1);
  const gl = canvas.getContext("webgl2", {
    alpha: false,
    antialias: false,
    depth: false,
    stencil: false,
    preserveDrawingBuffer: false,
  });
  if (!gl) return null;
  let programs = null;
  let fbo = null;
  let lost = false;

  function setup() {
    if (!gl.getExtension("EXT_color_buffer_float")) throw new Error("no float render targets");
    gl.disable(gl.DITHER);
    const vs = shader(gl.VERTEX_SHADER, VERTEX);
    programs = {};
    for (const [name, source] of Object.entries(shaderSources())) {
      const fs = shader(gl.FRAGMENT_SHADER, source);
      const p = gl.createProgram();
      gl.attachShader(p, vs);
      gl.attachShader(p, fs);
      gl.linkProgram(p);
      gl.deleteShader(fs);
      programs[name] = { p, uniforms: {} };
    }
    gl.deleteShader(vs);

    // A status query may block until compilation and linking finish. Give the
    // driver every independent program before asking it for the first result.
    for (const [name, { p, uniforms }] of Object.entries(programs)) {
      if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(`haze ${name}: ${gl.getProgramInfoLog(p)}`);
      const count = gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS);
      for (let i = 0; i < count; i++) {
        const info = gl.getActiveUniform(p, i);
        uniforms[info.name] = { loc: gl.getUniformLocation(p, info.name), type: info.type };
      }
    }
    fbo = gl.createFramebuffer();
  }

  function shader(type, source) {
    const s = gl.createShader(type);
    gl.shaderSource(s, source);
    gl.compileShader(s);
    return s;
  }

  canvas.addEventListener("webglcontextlost", (event) => {
    event.preventDefault();
    lost = true;
    releaseBurst();
  });
  canvas.addEventListener("webglcontextrestored", () => {
    try {
      setup();
      lost = false;
    } catch {
      /* Still lost to us: the CPU renders. */
    }
  });
  setup();

  /* A render owns each texture it acquires exactly once. `free` contains
     scratch whose last reader has run; `previous` contains only scratch
     from the last successful render. Uploads are never pooled. */
  const textures = [], free = [], previous = [];
  let burstTimer = 0;

  function releaseBurst() {
    clearTimeout(burstTimer);
    burstTimer = 0;
    for (const tex of previous) gl.deleteTexture(tex.t);
    previous.length = 0;
  }

  function take(pool, w, h, format) {
    for (let i = 0; i < pool.length; i++) {
      const tex = pool[i];
      if (tex.w === w && tex.h === h && tex.format === format) {
        pool[i] = pool[pool.length - 1];
        pool.pop();
        return tex;
      }
    }
    return null;
  }

  function texture(w, h, data = null, format = gl.RGBA32F) {
    if (!data) {
      const recycled = take(free, w, h, format);
      if (recycled) return recycled;
      const carried = take(previous, w, h, format);
      if (carried) {
        textures.push(carried);
        return carried;
      }
    }
    const t = gl.createTexture();
    const tex = { t, w, h, format, scratchBytes: data ? 0 : w * h * (format === gl.RG32F ? 8 : 16) };
    textures.push(tex);
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texImage2D(gl.TEXTURE_2D, 0, format, w, h, 0, format === gl.RG32F ? gl.RG : gl.RGBA,
      format === gl.RGBA8 ? gl.UNSIGNED_BYTE : gl.FLOAT, data);
    return tex;
  }

  function finishFrame(complete) {
    /* Don't leave storage attached to the shared FBO when deleting its
       texture, including when the one-shot idle timer drops the cache. */
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, null, 0);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    releaseBurst(); // unmatched previous sizes cannot accumulate across resizes
    let budget = complete ? BURST_BYTES : 0;
    for (const tex of textures) {
      if (tex.scratchBytes && tex.scratchBytes <= budget) {
        previous.push(tex);
        budget -= tex.scratchBytes;
      } else {
        gl.deleteTexture(tex.t);
      }
    }
    textures.length = free.length = 0;
    if (previous.length) burstTimer = setTimeout(releaseBurst, BURST_MS);
  }

  /** One pass of `name` into `target` (a texture, or null: the canvas). */
  function run(name, target, values) {
    const { p, uniforms } = programs[name];
    gl.useProgram(p);
    let unit = 0;
    for (const [key, v] of Object.entries(values)) {
      const u = uniforms[key];
      if (!u) continue; // not used by this pass
      switch (u.type) {
        case gl.SAMPLER_2D:
          gl.activeTexture(gl.TEXTURE0 + unit);
          gl.bindTexture(gl.TEXTURE_2D, v.t);
          gl.uniform1i(u.loc, unit++);
          break;
        case gl.FLOAT: gl.uniform1f(u.loc, v); break;
        case gl.FLOAT_VEC2: gl.uniform2fv(u.loc, v); break;
        case gl.FLOAT_VEC3: gl.uniform3fv(u.loc, v); break;
        case gl.FLOAT_VEC4: gl.uniform4fv(u.loc, v); break;
        case gl.INT: gl.uniform1i(u.loc, v); break;
        case gl.BOOL: gl.uniform1i(u.loc, v ? 1 : 0); break;
        case gl.INT_VEC2: gl.uniform2iv(u.loc, v); break;
        case gl.UNSIGNED_INT: gl.uniform1ui(u.loc, v); break;
        case gl.FLOAT_MAT3: gl.uniformMatrix3fv(u.loc, false, v); break;
      }
    }
    if (target) {
      gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, target.t, 0);
      gl.viewport(0, 0, target.w, target.h);
    } else {
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.viewport(0, 0, canvas.width, canvas.height);
    }
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  /** Three box pairs; a cropped first pass still samples its source at
      the original coordinates. All later passes use texture-local ones. */
  function blur(src, sigma, x0 = 0, width = src.w, format = gl.RGBA32F) {
    const r = radiusOf(sigma);
    const a = texture(width, src.h, null, format), b = texture(width, src.h, null, format);
    const first = format === gl.RG32F ? "boxLocal" : x0 ? "boxCrop" : "box";
    let from = src;
    for (let i = 0; i < 3; i++) {
      run(i === 0 ? first : "box", a, { uTex: from, uDir: [1, 0], uR: r, uX0: x0 });
      run("box", b, { uTex: a, uDir: [0, 1], uR: r });
      from = b;
    }
    free.push(a);
    return b;
  }

  /** A measure pass, then sums of BLOCK² texels down to one. */
  function measure(name, w, h, values) {
    let t = texture(Math.ceil(w / BLOCK), Math.ceil(h / BLOCK));
    run(name, t, values);
    while (t.w > 1 || t.h > 1) {
      const next = texture(Math.ceil(t.w / BLOCK), Math.ceil(t.h / BLOCK));
      run("sum", next, { uTex: t });
      free.push(t);
      t = next;
    }
    return t;
  }

  /**
   * Render a haze and its frosted twin — renderHaze's arguments, and its
   * answers as bitmaps: { bitmap, frost, gather, tone, grey, light } (gather as
   * RGBA bytes, { data, w, h }).
   */
  function render(src, geo, tint, { scale = 1, seed = 1, frost = null } = {}) {
    if (lost || gl.isContextLost()) return null;
    const plan = prepare(src, geo, tint, scale);
    const { w, h, rw, rh, frame, ex } = plan;
    const [key, frostKey] = noiseKeys(seed);
    let bitmap = null, frosted = null, complete = false;
    try {
      canvas.width = w;
      canvas.height = h;
      const source = texture(src.w, src.h, new Uint8Array(src.data.buffer, src.data.byteOffset, src.data.length), gl.RGBA8);
      const origin = [plan.rect[0], plan.rect[1]];
      const tone = { uEx: [ex.lo, ex.mid, ex.hi, ex.M], uTint: plan.tintLab, uS3: plan.s3, uYcap: plan.ycap };

      const glab = texture(rw, rh);
      run("grade", glab, { ...tone, uSrc: source, uOrigin: origin });
      const local = blur(glab, plan.local, 0, rw, gl.RG32F);
      const graded = texture(rw, rh);
      run("detail", graded, { ...tone, uGlab: glab, uBase: local });
      free.push(local, glab);

      const at = frame ? [frame.x, frame.y, frame.w, frame.h] : [0, 0, 1, 1];
      const cols = plan.cols ? texture(w, 1, plan.cols) : texture(1, 1, new Float32Array(4));
      const closed = plan.closed ? [plan.closed.ox, plan.closed.oy, plan.closed.k] : [0, 0, 1];
      const laid = texture(w, h);
      run("layout", laid, { uGraded: graded, uCols: cols, uFramed: Boolean(frame), uFrame: at, uClosed: closed });
      free.push(graded);
      const lensed = blur(laid, plan.lens);
      /* Only cover columns at/after its first pixel center use the softer
         lens. Keep all three horizontal radii to their left. */
      const softX = plan.cover ? Math.max(0, Math.ceil(frame.x - 0.5) - 3 * radiusOf(plan.softer)) : 0;
      const softer = plan.cover && softX < w ? blur(laid, plan.softer, softX, w - softX) : lensed;
      free.push(laid);
      const lab = texture(w, h);
      run("mix", lab, {
        uLensed: lensed,
        uSofter: softer,
        uSoftX: softX,
        uCols: cols,
        uFramed: Boolean(frame),
        uCover: plan.cover,
        uFrame: at,
        uCorner: plan.corner,
        uPool: plan.pool ? [plan.pool.cy, plan.pool.span] : [0, 1],
        uBar: plan.closed ? [plan.closed.cx, plan.closed.radius] : [0, 1],
        uH: h,
      });
      free.push(lensed);
      if (softer !== lensed) free.push(softer);

      const measured = Math.min(w, plan.measureTo);
      const sumC = measure("measureC", measured, h, { uLab: lab, uMeasureTo: measured });
      const means = { uSumC: sumC, uS3: plan.s3, uYcap: plan.ycap };
      const sumYP = measure("measureYP", measured, h, { ...means, uLab: lab, uMeasureTo: measured });

      let edge = source; // a valid, unused sampler where there is no seam
      if (plan.seam) {
        const raw = texture(plan.seam.ew, rh);
        run("raw", raw, { uSrc: source, uOrigin: origin });
        edge = blur(raw, plan.seam.soft);
        free.push(raw);
      }
      const haze = {
        ...means,
        uLab: lab,
        uSumYP: sumYP,
        uEdge: edge,
        uScale: plan.scale,
        uPx: plan.px,
        uSeam: Boolean(plan.seam),
        uSeamX: plan.seam ? [plan.seam.x0, plan.seam.x1] : [0, 0],
        uFrame: at,
        uRw: rw,
        uW: w,
        uH: h,
      };
      run("haze", null, { ...haze, uKey: key });

      /* The type's light: over a Canvas it is the picture; over a cover (or
         nothing) the type in the panel sits on the haze itself. */
      let light;
      if (frame && !plan.immersive) {
        const x0 = Math.min(w - 1, Math.ceil(frame.x)), lw = w - x0;
        const rows = new Uint8Array(lw * h * 4);
        gl.readPixels(x0, 0, lw, h, gl.RGBA, gl.UNSIGNED_BYTE, rows);
        light = lightOf({ data: rows, w: lw, h }, [0, 0, lw, h], true);
      } else {
        light = lightOf(src, plan.shown);
      }
      bitmap = canvas.transferToImageBitmap();

      let gather = null;
      let railTone = null;
      if (frost) {
        const fw = Math.ceil(w / 2), fh = Math.ceil(h / 2);
        const sigma = frost.sigma / 2;
        const half = texture(fw, fh);
        run("half", half, { ...haze, uLit: false, uHalfX: 0 });
        const blurred = blur(half, sigma);
        free.push(half);
        let lit = blurred;
        let litX = 0, litLo = 0, litHi = 0;
        if (plan.seam && plan.seam.x0 < plan.seam.x1) {
          /* Only seam texels can make the two halves differ. Their blur
             reaches 3r on either side; the exact blur of that output
             strip needs another 3r of source (or the real image edge). */
          const reach = 3 * radiusOf(sigma);
          litLo = Math.max(0, Math.floor(plan.seam.x0 / 2) - reach);
          litHi = Math.min(fw, Math.ceil(plan.seam.x1 / 2) + reach);
          litX = Math.max(0, litLo - reach);
          const litEnd = Math.min(fw, litHi + reach);
          const halfLit = texture(litEnd - litX, fh);
          run("half", halfLit, { ...haze, uLit: true, uHalfX: litX });
          lit = blur(halfLit, sigma);
        }
        canvas.width = fw;
        canvas.height = fh;
        const m = saturateMatrix(frost.saturate);
        const edge = frost.edge;
        const e = saturateMatrix(edge?.saturate ?? 1);
        const toneX = TONE_COLUMNS.map((at) => Math.min(fw - 1, Math.floor(fw * at)));
        const frostPass = {
          uFrost: blurred,
          uFrostLit: lit,
          uLitX: litX,
          uLitRange: [litLo, litHi],
          uSat: [m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8]],
          uBright: frost.brightness,
          uFrame: at,
          uPx: plan.px,
          uKey: frostKey,
          uW: fw,
          uH: fh,
          uGather: false,
          uToneX: toneX,
          uGain: edge?.gain ?? 1,
          uEdgeSat: [e[0], e[3], e[6], e[1], e[4], e[7], e[2], e[5], e[8]],
          uAlpha: edge?.alpha ?? 0,
          uTint: edge?.tint ?? [0, 0, 0],
        };
        run("frost", null, frostPass);
        const toneBytes = toneX.length * fh * 4;
        if (!edge) {
          /* No gather draw: read the tone from the frost before transfer. */
          const columns = new Uint8Array(toneBytes);
          toneX.forEach((x, i) => {
            gl.readPixels(x, 0, 1, fh, gl.RGBA, gl.UNSIGNED_BYTE, columns, i * fh * 4);
          });
          railTone = toneOf(columns);
        }
        frosted = canvas.transferToImageBitmap();
        /* One read: gather rows already top-down, then both undistorted
           tone columns and at most a row of unused padding. */
        if (edge) {
          const rowsH = fh + Math.ceil(toneX.length * fh / fw);
          canvas.height = rowsH;
          run("frost", null, { ...frostPass, uGather: true });
          const rows = new Uint8Array(fw * rowsH * 4);
          gl.readPixels(0, 0, fw, rowsH, gl.RGBA, gl.UNSIGNED_BYTE, rows);
          const n = fw * fh * 4;
          gather = { data: new Uint8ClampedArray(rows.buffer, 0, n), w: fw, h: fh };
          railTone = toneOf(rows.subarray(n, n + toneBytes));
        }
      }
      if (gl.isContextLost()) return null;
      complete = true;
      return { bitmap, frost: frosted, gather, tone: railTone, grey: plan.grey, light };
    } finally {
      if (!complete) {
        bitmap?.close();
        frosted?.close();
      }
      finishFrame(complete);
      /* Even between close renders, no full-size drawing buffer. */
      canvas.width = canvas.height = 1;
    }
  }

  return { render, context: gl };
}
