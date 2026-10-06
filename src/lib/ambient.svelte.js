/* =====================================================================
   THE HAZE — what the panels and the layer share

   The layer (Ambient.svelte) renders the haze in a worker, rarely, and
   shows it with a compositor crossfade: still at rest, a crossfade with
   the art's own on a change of record, and no work at all in between. The
   arithmetic lives in haze.js, drawn by the GPU (haze-gl.js) or, where
   there is no WebGL2, by the CPU.

   The haze is the record's own picture seen through a light lens (haze.js,
   the "A light" pipeline): the source read at PROBE pixels, graded against
   its own median with its chroma kept and its detail laid back over the
   compressed range, then laid out — under the video exactly as the video
   shows it, and to its left as its own mirror image, continuous at the
   video's edge and magnified with distance (ZOOM), so the window is one
   large view of the picture rather than repeated copies — blurred once
   (LENS, 6px) and held under the ceilings.

   THE CONTRACT — every field of `haze`, who writes it, and what it means.
   The layer also reads `ui.immersive` (state.svelte.js), as "a Canvas
   video covers the panel": the type's light is then measured on the video.

   haze.video        written by the panel.
                     The Canvas <video> while it is the picture in the
                     panel, else null. The haze is drawn from it.

   haze.anchor       written by the panel (placeCanvas, NowPlayingPanel).
                     { left, video }, or null while the panel is closed.
                     - left: the panel's left edge as a fraction of the
                       window's width. The open panel is a full-height
                       column at the window's right edge; the haze lays the
                       panel's picture out under it (a cover as the light
                       its smaller tile sits in, cut to the panel's rounded
                       corners) and carries it on leftward.
                     - video: { x, y, w, h }, the Canvas's VISIBLE rect in
                       window CSS px, or null while no video is shown. The
                       haze frames the picture to exactly that rect
                       (haze.js, frameOf), so it meets the video where the
                       video is, whether it fills the panel or is capped
                       and dissolving (placeCanvas).
                     With no anchor the haze is the restrained glow from
                     the player bar (RESTRAINED).

   haze.awaiting     written by the panel.
                     true from a record change until the panel knows
                     whether that record has a Canvas and, if it has, until
                     the video is shown (or has failed); false otherwise.
                     While it is true the haze holds the previous record's
                     light (at most 2.5 s) instead of lighting from the
                     cover and then changing again for the Canvas, so a
                     change of record is one crossfade.

   haze.lightTop     written here. Read by the panel's type.
   haze.lightBottom  How bright the ground under the panel's type is, as
                     WCAG relative luminance (0 black, 0.18 mid grey,
                     1 white): the 90th percentile over the top 18% of the
                     panel (the head) and over its lower 42% (from 58%
                     down: title, artists, meta).
                     - With a Canvas covering the panel: measured on the
                       video as the panel frames it, and the brightest that
                       band gets over the loop (it starts from the first
                       frame, can only rise while the first loop is
                       sampled, then holds).
                     - Otherwise: measured on the haze itself under the
                       panel, so it is at most 0.10.
                     1 until measured, and 1 while the panel shows a Canvas
                     whose frames cannot be read: unknown is treated as
                     white. White type's contrast over the band is at worst
                     1.05 / (light + 0.05): 4.5:1 up to 0.183, 3:1 up to
                     0.30. Published only on a change of more than 0.01.

   use:frost         the action, worn by the glass planes (see THE FROST).

   What the glass is calibrated against (haze.js, CEILING): every pixel of
   the haze, after dithering, is an in-gamut sRGB colour of relative
   luminance at most 0.10 (#595959 as a grey) — 0.05 with the panel closed —
   within the grade's chroma and perceived-lightness caps; and its mean
   luminance is at most 0.045. The one exception is the seam band (THE
   FROST, 3), which no type ever lies over. glass.test.js frosts the
   brightest colour the haze can make exactly as CSS frosts it, and holds
   the type's greys over it on every surface.

   THE FROST — what the glass (app.css and the components) does with it:

   1. The three planes wear `use:frost`: the rail (.sidebar, Sidebar.svelte),
      the pane (.pane, App.svelte) and the bar (.player, PlayerBar.svelte).
   2. .glass-chrome and .glass-pane carry no backdrop-filter, only their
      tint, sheen, rim and lift: the layer draws the frost under them,
      clipped to their boxes and corner radii. The frost is a twin of each
      haze picture, made in the same pass at half its resolution with the
      numbers of --frost-plane (a 12px blur, then saturate and brightness;
      read from :root on every render — change the token and the frost
      follows), from the haze without the seam band, except for the first
      SPILL px under the glass (haze.js). Strips, overlays and
      plates keep a real backdrop-filter: they frost live content, not the
      haze, and they are small.
   3. The seam with the Canvas. The video is never under a plane, and never
      widened or covered: the panel keeps the gutter on its left, the pane
      and the bar end a gutter short of it as whole planes (four rounded
      corners and the rim), and the panel's own two left corners are
      rounded (--np-radius, cut by the layers that paint in it). The haze
      shows bare in that gap and in the corner cut-outs, and there — SEAM
      px left of the video's edge and SEAM_IN px inside it (haze.js) — it
      is the video's own light, at SEAM_DIM, fading into the graded haze:
      the gap reads as the video's light rather than as a dark notch, and
      the light carries on into the glass's edge (SPILL) and is gone before
      any type.
   4. The morph needs nothing: the planes stay live through it (THE LAYOUT
      MORPH, app.css) and the frost is traced in the ResizeObserver pass,
      after layout and before paint, so it takes a plane's new edge in the
      same frame the plane does.
   5. Dragging the panel's edge (App.svelte) renders nothing. The panel
      holds its anchor back for the length of the drag (panel.resizing),
      and the drag moves the pictures already on screen instead
      (shiftHaze): each is slid so the seam it was drawn for stays on the
      panel's edge — a transform, on the compositor — and stretched only
      as much as keeps the window's edges covered. When the drag ends the
      panel publishes its anchor once and the haze is drawn once for the
      new edge, fading in over the slid picture as any layout change does.
   ===================================================================== */

/** What the panels and the layer share. Written only on a real change. */
export const haze = $state({
  video: null,
  anchor: null,
  awaiting: false,
  lightTop: 1,
  lightBottom: 1,
});

let onShift = null;

/** For the drag: the panel's left edge in window CSS px while it is being
    dragged, then null once it is let go (THE FROST, 5). */
export function shiftHaze(edge) {
  onShift?.(edge);
}

/** For the layer: receive shiftHaze's edges. Returns the unsubscribe. */
export function watchShift(fn) {
  onShift = fn;
  return () => {
    if (onShift === fn) onShift = null;
  };
}

/** Publish the type's light; a change of 0.01 or less is not published. */
export function publishLight(light) {
  if (!light) return;
  if (Math.abs(haze.lightTop - light.top) > 0.01) haze.lightTop = light.top;
  if (Math.abs(haze.lightBottom - light.bottom) > 0.01) haze.lightBottom = light.bottom;
}

/* ---- The frost -----------------------------------------------------------
   The glass planes' frost, pre-rendered: the layer draws a frosted twin of
   every haze picture (haze.js, frostOf) and shows it clipped to the planes
   that wear `use:frost`, so a plane needs no backdrop-filter of its own. The
   clip is one path of rounded rects, traced from the planes' boxes in the
   ResizeObserver callback — after layout, before paint — so a plane that
   changes size (the panel opening, the window resizing) is frosted to its
   new edge in the very frame it takes it. */

const planes = new Set();
let onClip = null;
let resizes = null;
let lastClip = null;

/** Svelte action: this element is a frosted glass plane. */
export function frost(node) {
  planes.add(node);
  resizes?.observe(node); // its first report traces it
  return {
    destroy() {
      planes.delete(node);
      resizes?.unobserve(node);
      trace();
    },
  };
}

/** For the layer: call `clip(css)` with a clip-path value whenever the
    frosted region changes. Returns the unsubscribe. */
export function watchFrost(clip) {
  onClip = clip;
  lastClip = null;
  resizes = new ResizeObserver(trace);
  for (const el of planes) resizes.observe(el);
  window.addEventListener("resize", trace);
  trace();
  return () => {
    onClip = null;
    resizes.disconnect();
    resizes = null;
    window.removeEventListener("resize", trace);
  };
}

function trace() {
  if (!onClip) return;
  const parts = [];
  for (const el of planes) {
    const b = el.getBoundingClientRect();
    const st = getComputedStyle(el);
    const r = [st.borderTopLeftRadius, st.borderTopRightRadius, st.borderBottomRightRadius, st.borderBottomLeftRadius].map(
      (v) => parseFloat(v) || 0,
    );
    if (b.width > 0 && b.height > 0) parts.push(roundRect({ x: b.left, y: b.top, w: b.width, h: b.height, r }));
  }
  const clip = parts.length ? `path("${parts.join(" ")}")` : "inset(50%)";
  if (clip === lastClip) return;
  lastClip = clip;
  onClip(clip);
}

const px = (v) => Math.round(v * 100) / 100;

/** A rounded rect as a path; `r` is the four corner radii, clockwise from
    the top left (a square corner is a radius of 0). */
function roundRect({ x, y, w, h, r }) {
  const [tl, tr, br, bl] = r.map((v) => px(Math.min(v, w / 2, h / 2)));
  const arc = (k, ex, ey) => (k ? `A${k} ${k} 0 0 1 ${px(ex)} ${px(ey)}` : `L${px(ex)} ${px(ey)}`);
  return (
    `M${px(x + tl)} ${px(y)}H${px(x + w - tr)}${arc(tr, x + w, y + tr)}` +
    `V${px(y + h - br)}${arc(br, x + w - br, y + h)}H${px(x + bl)}${arc(bl, x, y + h - bl)}` +
    `V${px(y + tl)}${arc(tl, x + tl, y)}Z`
  );
}
