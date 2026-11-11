/* =====================================================================
   THE HAZE — the renderer, off the main thread

   Renders a haze picture when asked, and otherwise does nothing. It reads
   the source once (a GPU downscale, then one small readback), keeps those
   pixels so a change of layout or colour can be rendered again without a
   new read, and hands back each finished picture as an ImageBitmap, which
   the page shows with a compositor crossfade (Ambient.svelte). No timers,
   no loop: every message in is one piece of work, and there are only a
   handful per track.

   The picture is drawn by the GPU (haze-gl.js), in a few milliseconds, so
   the crossfade can start with the art that changed. The CPU's renderer
   (haze.js) draws it instead where WebGL2 cannot be had, while a lost
   context has not come back, or when the page asks for it (`init`).

   Messages in:
     init   { cpu }  whether to draw on the CPU only; before any render
     render { seq, key, fade, source?, video?, allowBlank?, geo, tint, scale, frost }
            render the haze for `key`, and its frosted twin when `frost`
            (parseFrost) is given. `source` (a VideoFrame or an
            ImageBitmap, transferred) replaces the kept pixels; without it
            the kept pixels are laid out again. null means unreadable.
     sample { key, frame, geo }
            one more frame of a playing Canvas's first loop, kept for
            `settle` and measured for the type's light.
     settle { seq, key, fade, geo, tint, scale, frost }
            the loop has been seen: if its representative frame is a
            different picture from the one the haze was made of, render it.
   Messages out (seq, key and fade are echoed):
     haze   { seq, key, fade, bitmap, frost, tone, grey, light, distance?, gpu, ms }
                                                     a picture to show
     blank  { seq }                                  that frame was black
     light  { key, light }                           the type's light
     done   { seq, key, light, distance }            settle had nothing to do
   ===================================================================== */

import {
  renderHaze,
  solidSource,
  framing,
  isBlank,
  lightOf,
  signatureOf,
  sigDistance,
  medoid,
  PROBE,
  SETTLE_DISTANCE,
} from "./haze.js";
import { createGlHaze } from "./haze-gl.js";

/* Downscale on the GPU, read through a CPU canvas: drawing a video frame
   straight into a CPU canvas would pull all 720x1280 pixels across. */
const probe = new OffscreenCanvas(PROBE, PROBE);
const probeCtx = probe.getContext("2d");
probeCtx.imageSmoothingQuality = "high";
const read = new OffscreenCanvas(PROBE, PROBE).getContext("2d", { willReadFrequently: true });
const stage = new OffscreenCanvas(1, 1);
const stageCtx = stage.getContext("2d");

/** The GPU's renderer; null until `init`, false where there is none. */
let gpu = null;
/** The kept source: { key, src } — src null when it could not be read. */
let kept = null;
/** A playing Canvas's first loop: { key, frames: [src], light }. */
let loop = null;
let seed = 1;

/** The whole source at PROBE on its long side, as RGBA bytes. */
function readSource(source) {
  const sw = source.displayWidth || source.width;
  const sh = source.displayHeight || source.height;
  if (!sw || !sh) return undefined;
  const k = PROBE / Math.max(sw, sh);
  const w = Math.max(8, Math.round(sw * k));
  const h = Math.max(8, Math.round(sh * k));
  probeCtx.clearRect(0, 0, w, h);
  probeCtx.drawImage(source, 0, 0, w, h);
  try {
    read.clearRect(0, 0, w, h);
    read.drawImage(probe, 0, 0, w, h, 0, 0, w, h);
    return { data: read.getImageData(0, 0, w, h).data, w, h };
  } catch {
    /* A CORS-tainted image poisons both canvases, not just this read.
       Resizing clears their origin-clean flags for the next source. */
    probe.width = PROBE;
    read.canvas.width = PROBE;
    probeCtx.imageSmoothingQuality = "high";
    return null; // tainted: a cover served without CORS
  }
}

const max = (a, b) => ({ top: Math.max(a.top, b.top), bottom: Math.max(a.bottom, b.bottom) });
/** An unreadable picture: the type must assume the worst. */
const UNKNOWN = { top: 1, bottom: 1 };

/** The haze on the CPU, as bitmaps. */
function renderOnCpu(src, m, options) {
  const out = renderHaze(src, m.geo, m.tint, options);
  const bitmapOf = (rgba, w, h) => {
    if (stage.width !== w || stage.height !== h) {
      stage.width = w;
      stage.height = h;
    }
    stageCtx.putImageData(new ImageData(rgba, w, h), 0, 0);
    return stage.transferToImageBitmap();
  };
  return {
    bitmap: bitmapOf(out.rgba, m.geo.w, m.geo.h),
    frost: out.frost ? bitmapOf(out.frost, out.frostW, out.frostH) : null,
    tone: out.tone,
    grey: out.grey,
    light: out.light,
  };
}

/** The haze on the GPU, or null when it cannot draw it now. A renderer
    that fails is not asked again. */
function renderOnGpu(src, m, options) {
  if (!gpu) return null;
  try {
    return gpu.render(src, m.geo, m.tint, options);
  } catch (error) {
    console.warn("haze: the GPU renderer failed; drawing on the CPU from now on.", error);
    gpu = false;
    return null;
  }
}

/** Render and hand over a picture. `seen` is the light already measured on
    other frames of the same loop, which the type must also clear. */
function paint(m, src, seen = null, distance = undefined) {
  const t0 = performance.now();
  const source = src ?? solidSource(m.tint);
  const options = { scale: m.scale, seed: seed++, frost: m.frost ?? null };
  const drawn = renderOnGpu(source, m, options);
  const out = drawn ?? renderOnCpu(source, m, options);
  const light = !src ? UNKNOWN : seen ? max(seen, out.light) : out.light;
  postMessage(
    {
      type: "haze",
      seq: m.seq,
      key: m.key,
      fade: m.fade,
      bitmap: out.bitmap,
      frost: out.frost,
      tone: out.tone,
      grey: out.grey,
      light,
      distance,
      gpu: Boolean(drawn),
      ms: performance.now() - t0,
    },
    out.frost ? [out.bitmap, out.frost] : [out.bitmap],
  );
}

onmessage = ({ data: m }) => {
  switch (m.type) {
    case "init": {
      if (m.cpu) {
        gpu = false;
        break;
      }
      try {
        gpu = createGlHaze() ?? false;
      } catch (error) {
        console.warn("haze: no GPU renderer; drawing on the CPU.", error);
        gpu = false;
      }
      break;
    }
    case "render": {
      if (m.source !== undefined) {
        const src = m.source ? readSource(m.source) : null;
        m.source?.close();
        if (src === undefined) break;
        if (src && m.video && !m.allowBlank && isBlank(src)) {
          postMessage({ type: "blank", seq: m.seq });
          break;
        }
        if (kept?.key !== m.key) loop = null;
        kept = { key: m.key, src };
      }
      if (kept?.key !== m.key) break;
      paint(m, kept.src, loop?.key === m.key ? loop.light : null);
      break;
    }
    case "sample": {
      const src = kept?.key === m.key && kept.src ? readSource(m.frame) : null;
      m.frame.close();
      if (!src) break;
      const light = lightOf(src, framing(src, m.geo).shown);
      if (loop?.key !== m.key) {
        loop = { key: m.key, frames: [kept.src], light: lightOf(kept.src, framing(kept.src, m.geo).shown) };
      }
      const before = loop.light;
      loop.frames.push(src);
      loop.light = max(before, light);
      /* Brighter is answered at once: the type's shadow should not wait
         for the loop to finish to learn about a flash. */
      if (loop.light.top > before.top + 0.02 || loop.light.bottom > before.bottom + 0.02) {
        postMessage({ type: "light", key: m.key, light: loop.light });
      }
      break;
    }
    case "settle": {
      if (!loop || loop.key !== m.key || kept?.key !== m.key) {
        postMessage({ type: "done", seq: m.seq, key: m.key, light: null });
        break;
      }
      const { frames, light } = loop;
      loop = null;
      const sigs = frames.map((src) => signatureOf(src, framing(src, m.geo).rect));
      const pick = medoid(sigs);
      const distance = pick === 0 ? 0 : sigDistance(sigs[pick], sigs[0]);
      /* The haze was made of frames[0]. Only a clearly different picture is
         worth a (very slow) change; the same picture a moment later is not. */
      if (distance > SETTLE_DISTANCE) {
        kept = { key: m.key, src: frames[pick] };
        paint(m, kept.src, light, distance);
      } else {
        postMessage({ type: "done", seq: m.seq, key: m.key, light, distance });
      }
      break;
    }
  }
};
