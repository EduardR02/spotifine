/* The playing row's decorative meter. Four calm curves share one nominal 60 Hz
   clock, only while visible and playback/window preferences permit motion.
   Perpetual compositor animations still woke Chromium at the display's
   240 Hz rate even with stepped keyframes; explicit ticks bound that work. */
const INTERVAL_MS = 1000 / 60;
const BARS = [[1600, -300], [1300, -550], [1850, -1100], [1450, -250]];
const SHAPE = [[0, 0.28], [0.2, 0.9], [0.35, 0.45], [0.5, 1], [0.65, 0.38], [0.8, 0.72], [1, 0.28]];
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

/* All curve/string work is paid once, never during a tick. */
const FRAMES = BARS.map(([ms]) => Array.from(
  { length: Math.ceil(ms / INTERVAL_MS) },
  (_, i) => `scaleY(${curveAt(i / Math.ceil(ms / INTERVAL_MS)).toFixed(4)})`,
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

/** One scheduler for all visible meters; time freezes when none may move. */
export function createMeterTicker(clock = realClock) {
  const visible = new Set();
  let running = false;
  let timer = null;
  let elapsed = 0;
  let started = 0;
  function paint(entry, time) {
    for (let i = 0; i < entry.bars.length; i++) {
      const [ms, delay] = BARS[i];
      const frames = FRAMES[i];
      const frame = Math.floor(((time - delay) % ms) / ms * frames.length);
      const transform = frames[frame];
      if (entry.last[i] === transform) continue;
      entry.last[i] = transform;
      entry.bars[i].style.transform = transform;
    }
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
    add(bars) { return { bars, last: [] }; },
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

const ticker = createMeterTicker();
const meters = new Map();
let requestedActivity = false;
let observer = null;
let motion = null;
function updateActivity() {
  const reduced = motion?.matches === true;
  ticker.running(requestedActivity && !reduced);
  if (reduced) for (const entry of meters.values()) {
    for (const bar of entry.bars) bar.style.removeProperty("transform");
    entry.last.length = 0;
  }
}

/** App owns playback/focus/document/native-minimize truth, not a DOM class. */
export function setMeterActivity(active) {
  requestedActivity = active;
  updateActivity();
}

/** Mounted only for current rows. One observer also notices display:none on
    hover, so an obscured meter has no ticking clock or layout reads. */
export function vuMeter(node) {
  if (!observer) {
    observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const meter = meters.get(entry.target);
        if (meter) ticker.visible(meter, entry.isIntersecting);
      }
    });
    motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    motion.addEventListener("change", updateActivity);
  }
  const entry = ticker.add([...node.children]);
  meters.set(node, entry);
  observer.observe(node);
  updateActivity();
  return {
    destroy() {
      observer.unobserve(node);
      meters.delete(node);
      ticker.remove(entry);
      if (!meters.size) {
        observer.disconnect();
        observer = null;
        motion.removeEventListener("change", updateActivity);
        motion = null;
      }
    },
  };
}
