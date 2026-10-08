/* The playing row's decorative meter. Four calm curves share one nominal 60 Hz
   clock, only while visible and playback/window preferences permit motion.
   Perpetual compositor animations tick at the display's rate (240 Hz on the
   owner's screen) even with stepped keyframes; explicit ticks bound that work.

   Each meter is a canvas handed over to an OffscreenCanvas, which this clock
   draws into. Its frames reach the display compositor on their own, so a tick
   costs the meter's 14×13 px and nothing of the page around it: no style,
   paint or layerization of the document and no new frame of its layers. Bars
   that were page elements moved by a transform made Chromium run the page's
   lifecycle and re-plan all of its layers on every tick, which cost more than
   the meter itself.

   A bar is drawn as the compositor drew the scaled bar element: one unscaled
   bar, rasterized the way CSS rasterizes a rounded box (an SVG rect), then
   stretched from the bottom with bilinear sampling. */
const INTERVAL_MS = 1000 / 60;
const BARS = [[1600, -300], [1300, -550], [1850, -1100], [1450, -250]];
const SHAPE = [[0, 0.28], [0.2, 0.9], [0.35, 0.45], [0.5, 1], [0.65, 0.38], [0.8, 0.72], [1, 0.28]];
/** CSS px: four 2px bars, 2px apart, 13px tall (app.css sizes the canvas). */
const BAR_W = 2;
const STEP = 4;
const W = 14;
const H = 13;
/** The picture before the clock has moved it, and the still one for reduced motion. */
const REST = [0.3, 0.3, 0.3, 0.3];
const STILL = [0.7, 0.7, 0.7, 0.7];
const bezier = (p1, p2, t) => 3 * p1 * t * (1 - t) ** 2 + 3 * p2 * t * t * (1 - t) + t ** 3;

function curveAt(f) {
  let j = 0;
  while (j < SHAPE.length - 2 && f >= SHAPE[j + 1][0]) j++;
  const [o0, v0] = SHAPE[j], [o1, v1] = SHAPE[j + 1];
  const x = (f - o0) / (o1 - o0);
  let lo = 0, hi = 1;
  for (let i = 0; i < 40; i++) {
    const mid = (lo + hi) / 2;
    if (bezier(0.42, 0.58, mid) < x) lo = mid;
    else hi = mid;
  }
  return v0 + (v1 - v0) * bezier(0, 1, (lo + hi) / 2);
}

/* All curve work is paid once, never during a tick. */
const FRAMES = BARS.map(([ms]) => Array.from(
  { length: Math.ceil(ms / INTERVAL_MS) },
  (_, i) => Number(curveAt(i / Math.ceil(ms / INTERVAL_MS)).toFixed(4)),
));
const realClock = {
  now: () => performance.now(),
  repeat(fn) {
    const timer = { id: null, active: true };
    let next = performance.now() + INTERVAL_MS;
    function run() {
      if (!timer.active) return;
      fn();
      if (!timer.active) return;
      const now = performance.now();
      // Keep fractional deadlines, skip missed slots rather than catching up,
      // and round delays up so timer truncation cannot turn 60 Hz into 62.5 Hz.
      next += INTERVAL_MS * Math.max(1, Math.floor((now - next) / INTERVAL_MS) + 1);
      timer.id = setTimeout(run, Math.max(1, Math.ceil(next - now)));
    }
    timer.id = setTimeout(run, Math.ceil(INTERVAL_MS));
    return timer;
  },
  cancel(timer) {
    timer.active = false;
    clearTimeout(timer.id);
  },
};

/** One scheduler for all visible meters; time freezes when none may move.
    `draw` gets the four bars' heights (0–1), only when one of them changed. */
export function createMeterTicker(clock = realClock) {
  const visible = new Set();
  let running = false;
  let timer = null;
  let elapsed = 0;
  let started = 0;
  function paint(entry, time) {
    let changed = false;
    for (let i = 0; i < BARS.length; i++) {
      const [ms, delay] = BARS[i];
      const frames = FRAMES[i];
      const frame = Math.floor(((time - delay) % ms) / ms * frames.length);
      if (entry.last[i] === frame) continue;
      entry.last[i] = frame;
      entry.values[i] = frames[frame];
      changed = true;
    }
    if (changed) entry.draw(entry.values);
  }
  function tick() {
    const time = elapsed + clock.now() - started;
    for (const entry of visible) paint(entry, time);
  }
  function sync() {
    if (running && visible.size) {
      if (timer !== null) return;
      started = clock.now();
      tick();
      timer = clock.repeat(tick);
    } else if (timer !== null) {
      elapsed += clock.now() - started;
      clock.cancel(timer);
      timer = null;
    }
  }
  return {
    add(draw) { return { draw, last: [], values: [...REST] }; },
    visible(entry, seen) {
      if (seen) {
        visible.add(entry);
        if (timer !== null) paint(entry, elapsed + clock.now() - started);
      } else visible.delete(entry);
      sync();
    },
    running(value) {
      running = value;
      sync();
    },
    remove(entry) {
      visible.delete(entry);
      sync();
    },
  };
}

/** The unscaled bar at `dpr`, once per colour and ratio for the session. */
const sprites = new Map();
function spriteFor(color, dpr) {
  const key = `${color} ${dpr}`;
  let sprite = sprites.get(key);
  if (!sprite) {
    sprite = { bitmap: null, ready: null };
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${Math.ceil(BAR_W * dpr)}" height="${Math.ceil(H * dpr)}" ` +
      `viewBox="0 0 ${BAR_W} ${H}" preserveAspectRatio="none"><rect width="${BAR_W}" height="${H}" rx="1" fill="${color}"/></svg>`;
    const img = new Image();
    img.src = `data:image/svg+xml,${encodeURIComponent(svg)}`;
    sprite.ready = img.decode().then(() => createImageBitmap(img)).then((bitmap) => (sprite.bitmap = bitmap));
    sprites.set(key, sprite);
  }
  return sprite;
}

const ticker = createMeterTicker();
const meters = new Map();
let requestedActivity = false;
let observer = null;
let motion = null;
let resolution = null;

/** Size the backing store to the device pixels and fetch the bar for them. */
function fit(meter) {
  const dpr = window.devicePixelRatio || 1;
  meter.canvas.width = Math.round(W * dpr);
  meter.canvas.height = Math.round(H * dpr);
  meter.ctx.setTransform(meter.canvas.width / W, 0, 0, meter.canvas.height / H, 0, 0);
  const sprite = spriteFor(meter.color, dpr);
  meter.want = sprite;
  meter.sprite = sprite.bitmap;
  if (!sprite.bitmap) {
    sprite.ready.then(() => {
      if (meters.get(meter.node) !== meter || meter.want !== sprite) return;
      meter.sprite = sprite.bitmap;
      draw(meter, meter.shown);
    });
  }
}

function draw(meter, values) {
  meter.shown = values;
  const { ctx, sprite } = meter;
  if (!sprite) return;
  ctx.clearRect(0, 0, W, H);
  for (let i = 0; i < values.length; i++) {
    const h = H * values[i];
    ctx.drawImage(sprite, i * STEP, H - h, BAR_W, h);
  }
}

function watchResolution() {
  resolution?.removeEventListener("change", onResolution);
  resolution = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
  resolution.addEventListener("change", onResolution);
}

/* Dragged to a screen of another density: redraw each meter for its pixels. */
function onResolution() {
  watchResolution();
  for (const meter of meters.values()) {
    fit(meter);
    draw(meter, meter.shown);
  }
}

function updateActivity() {
  const reduced = motion?.matches === true;
  ticker.running(requestedActivity && !reduced);
  for (const meter of meters.values()) {
    if (reduced) {
      meter.entry.last.length = 0;
      draw(meter, STILL);
    } else if (meter.shown === STILL) {
      draw(meter, REST);
    }
  }
}

/** App owns playback/focus/document/native-minimize truth, not a DOM class. */
export function setMeterActivity(active) {
  requestedActivity = active;
  updateActivity();
}

/** Mounted on the current rows' canvas. One observer also notices display:none
    on hover, so an obscured meter has no ticking clock or drawing. */
export function vuMeter(node) {
  if (!observer) {
    observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const meter = meters.get(entry.target);
        if (meter) ticker.visible(meter.entry, entry.isIntersecting);
      }
    });
    motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    motion.addEventListener("change", updateActivity);
    watchResolution();
  }
  const canvas = node.transferControlToOffscreen();
  const meter = { node, canvas, ctx: canvas.getContext("2d"), color: getComputedStyle(node).color, want: null, sprite: null, shown: REST };
  meter.entry = ticker.add((values) => draw(meter, values));
  meters.set(node, meter);
  fit(meter);
  draw(meter, REST);
  observer.observe(node);
  updateActivity();
  return {
    destroy() {
      observer.unobserve(node);
      meters.delete(node);
      ticker.remove(meter.entry);
      if (!meters.size) {
        observer.disconnect();
        observer = null;
        motion.removeEventListener("change", updateActivity);
        motion = null;
        resolution.removeEventListener("change", onResolution);
        resolution = null;
      }
    },
  };
}
