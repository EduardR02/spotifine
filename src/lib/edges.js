/* =====================================================================
   THE EDGES — the gather at the planes' edges, baked (app.css, THE EDGE).

   Just inside each plane's rim the glass gathers the light behind it: the
   frost there, brighter and richer, in a band that is a bright line at the
   rim with a faint tail, weighted toward the light. Behind a plane is only
   the haze, which is still between records, so the band is drawn into four
   small canvases per plane — the top and bottom strips with their corners,
   and the two sides between them — from the frost's gathered twin (haze.js,
   parseEdge), with the band's shape baked into their alpha. They lie in the
   haze layer, over the frost and under the planes, and are drawn again only
   when a picture is shown or a plane moves.

   They are plain pictures on purpose. The band was a backdrop-filter with
   a mask on every plane, which the compositor filtered again for every
   frame in which anything inside a plane changed (a playing row's meter,
   the playhead), and which, lying under each plane's content, lifted most
   of that content onto layers of its own. A CSS mask is no better here: a
   masked layer is a render pass the compositor draws on every frame.

   The band's numbers are these, not tokens: the band is drawn here and
   nowhere else.
   ===================================================================== */

/** The strip's depth in CSS px: the corner's square, which holds the band. */
const DEPTH = 12;
/** The band along an edge, inward from the rim: [px, alpha] — a line
    against the rim and under it a tail at a third of its strength. */
const RUN = [[0, 0.86], [1, 0.75], [2, 0.51], [3, 0.31], [4, 0.21], [5, 0.14], [6, 0.085], [7, 0.045], [8, 0.02], [9.5, 0]];
/** The same fall round a corner, out from the corner's centre (12px in). */
const TURN = [[0, 0], [2.5, 0], [4, 0.02], [5, 0.045], [6, 0.085], [7, 0.14], [8, 0.21], [9, 0.31], [10, 0.51], [11, 0.75], [12, 0.86]];
/** The weighting toward the light: a 168deg gradient over the plane. */
const FALL_DEG = 168;
const FALL = [[0, 1], [0.38, 0.62], [1, 0.34]];
/** The plane's sheen (--glass-sheen) lay on the glass the filter gathered,
    and was lifted with it by the gain past one; the twin is drawn under
    the plane and its sheen, so this is the rest of that lift, seen through
    the tint: the sheen x (2.4 - 1) / (1 - 0.37), at the sheen's place. */
const SHEEN = { at: [0.14, 0], size: [1.25, 0.92], stops: [[0, 0.078], [0.46, 0.027], [0.7, 0]] };

let scratch = null;
/* Every surface here is the CPU's: a strip drawn on the CPU is a picture
   the haze layer paints with its other strips, on one layer, where a
   GPU canvas is a layer of its own, and the compositor handles each one on
   every frame. They are drawn a few times a record, from the twin the haze
   worker read back where the stall is nobody's frame (holdTwin). */
const CPU = { willReadFrequently: true };

/** The gathered twin ({ data, w, h }: RGBA bytes from the haze worker), as a
    CPU picture to draw the strips from. */
export function holdTwin({ data, w, h }) {
  const copy = new OffscreenCanvas(w, h);
  copy.getContext("2d", CPU).putImageData(new ImageData(data, w, h), 0, 0);
  return copy;
}

/** The four strips of a plane w x h, in its own CSS px: [x, y, w, h]. */
export function stripsOf(w, h) {
  if (w < 2 * DEPTH || h < 2 * DEPTH) return [];
  return [
    [0, 0, w, DEPTH],
    [0, h - DEPTH, w, DEPTH],
    [0, DEPTH, DEPTH, h - 2 * DEPTH],
    [w - DEPTH, DEPTH, DEPTH, h - 2 * DEPTH],
  ];
}

/**
 * Draw one strip of a plane's band into `canvas`.
 * @param canvas  the strip's canvas, laid at box + strip by the caller
 * @param twin    the gathered twin of the window's frost (holdTwin)
 * @param box     the plane: { x, y, w, h, r } in CSS px (r: four radii)
 * @param strip   [x, y, w, h] in the plane's CSS px (stripsOf)
 * @param view    { w, h, dpr, slide }: the window, and how a drag has slid
 *                the picture (Ambient.svelte, slide: { from, edge, stretch })
 *                or null
 */
export function drawStrip(canvas, twin, box, strip, view) {
  const [sx, sy, sw, sh] = strip;
  const W = Math.max(1, Math.round(sw * view.dpr)), H = Math.max(1, Math.round(sh * view.dpr));
  if (canvas.width !== W) canvas.width = W;
  if (canvas.height !== H) canvas.height = H;
  const ctx = canvas.getContext("2d", CPU);
  const kx = W / sw, ky = H / sh;
  /* Plane-local CSS px from here on. */
  const local = (c) => c.setTransform(kx, 0, 0, ky, -sx * kx, -sy * ky);

  /* The twin under this strip: the whole picture laid over the window as
     the frost is (slid with it in a drag), so it is sampled as the frost
     is, neighbours and all, and only this strip of it is drawn. */
  local(ctx);
  ctx.globalCompositeOperation = "copy";
  const k = view.slide ? view.slide.stretch : 1;
  const x0 = view.slide ? view.slide.edge - view.slide.from * k : 0;
  ctx.drawImage(twin, x0 - box.x, -box.y, view.w * k, view.h);

  // The sheen's lift.
  ctx.globalCompositeOperation = "lighter";
  const rx = SHEEN.size[0] * box.w, ry = SHEEN.size[1] * box.h, cx = SHEEN.at[0] * box.w, cy = SHEEN.at[1] * box.h;
  ctx.translate(cx, cy);
  ctx.scale(1, ry / rx);
  const sheen = ctx.createRadialGradient(0, 0, 0, 0, 0, rx);
  for (const [at, a] of SHEEN.stops) sheen.addColorStop(at, `rgb(255 255 255 / ${a})`);
  ctx.fillStyle = sheen;
  ctx.fillRect(sx - cx, (sy - cy) * (rx / ry), sw, sh * (rx / ry));

  // The band's shape, as alpha: cut to the plane's corners.
  /* Exactly the strip's size: drawing a canvas copies all of it, and a
     scratch grown to the tallest and widest strip at once copied a plane's
     worth of pixels into every strip. */
  scratch ??= new OffscreenCanvas(1, 1);
  if (scratch.width !== W) scratch.width = W;
  if (scratch.height !== H) scratch.height = H;
  const m = scratch.getContext("2d", CPU);
  m.setTransform(1, 0, 0, 1, 0, 0);
  m.globalCompositeOperation = "source-over";
  m.clearRect(0, 0, W, H);
  local(m);
  m.save();
  m.beginPath();
  m.roundRect(0, 0, box.w, box.h, box.r);
  m.clip();
  const run = (x0, y0, x1, y1) => {
    const g = m.createLinearGradient(x0, y0, x1, y1);
    for (const [at, a] of RUN) g.addColorStop(at / 9.5, `rgb(0 0 0 / ${a})`);
    return g;
  };
  const d = DEPTH, w = box.w, h = box.h, b = 9.5;
  /* The band along the foot and the right side starts where CSS put it: a
     9.5px tile laid at 100% lands on a whole device pixel, half a pixel
     past the edge, and the band was drawn so. */
  const yb = Math.round((h - b) * view.dpr) / view.dpr, xb = Math.round((w - b) * view.dpr) / view.dpr;
  /* Only the pieces that reach this strip. */
  const meets = (x, y, pw, ph) => x < sx + sw && x + pw > sx && y < sy + sh && y + ph > sy;
  const piece = (style, x, y, pw, ph) => {
    if (!meets(x, y, pw, ph)) return;
    m.fillStyle = style();
    m.fillRect(x, y, pw, ph);
  };
  piece(() => run(0, 0, 0, b), d, 0, w - 2 * d, b);
  piece(() => run(0, yb + b, 0, yb), d, yb, w - 2 * d, h - yb);
  piece(() => run(0, 0, b, 0), 0, d, b, h - 2 * d);
  piece(() => run(xb + b, 0, xb, 0), xb, d, w - xb, h - 2 * d);
  for (const [cx2, cy2, qx, qy] of [[d, d, 0, 0], [w - d, d, w - d, 0], [d, h - d, 0, h - d], [w - d, h - d, w - d, h - d]]) {
    piece(() => {
      const g = m.createRadialGradient(cx2, cy2, 0, cx2, cy2, d);
      for (const [at, a] of TURN) g.addColorStop(at / d, `rgb(0 0 0 / ${a})`);
      return g;
    }, qx, qy, d, d);
  }
  m.restore();
  // The weighting toward the light, as CSS lays a 168deg gradient on a box.
  const rad = (FALL_DEG * Math.PI) / 180;
  const dx = Math.sin(rad), dy = -Math.cos(rad);
  const half = (Math.abs(w * dx) + Math.abs(h * dy)) / 2;
  const fall = m.createLinearGradient(w / 2 - dx * half, h / 2 - dy * half, w / 2 + dx * half, h / 2 + dy * half);
  for (const [at, a] of FALL) fall.addColorStop(at, `rgb(0 0 0 / ${a})`);
  m.globalCompositeOperation = "destination-in";
  m.fillStyle = fall;
  m.fillRect(sx, sy, sw, sh);

  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalCompositeOperation = "destination-in";
  ctx.drawImage(scratch, 0, 0, W, H, 0, 0, W, H);
  ctx.globalCompositeOperation = "source-over";
}
