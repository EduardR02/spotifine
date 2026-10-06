<script>
  import { untrack } from "svelte";
  import { playback, ui, coverUrl as imageUrl } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { haze, publishLight, frost, watchFrost, watchShift } from "../lib/ambient.svelte.js";
  import { rgbOf, parseFrost, RESTRAINED } from "../lib/haze.js";

  /* =====================================================================
     THE HAZE — the layer

     One picture behind every panel: the playing record's light, drawn from
     the Canvas while the panel shows it and from the cover otherwise.

     It is rendered RARELY and changes on the compositor. A worker
     (lib/haze.worker.js) renders a still picture when something it depends
     on changes — the record, the panel's layout, the window's size — and
     the page shows it by fading a canvas in over the last one with a WAAPI
     opacity animation: every visible change runs at the display's frame
     rate with no script and no canvas work per frame. At rest nothing runs
     at all: no sampling, no worker, no uploads, a static backdrop.

     A change of record is one change with the art: the worker draws on the
     GPU in a few milliseconds, the picture is asked for as soon as the new
     cover (or Canvas) is there — which is when the art changes too — and it
     crossfades as the panel's picture does.

     A playing Canvas's first loop is looked at a few more times, about a
     second apart, and then never again: the type's light is the brightest
     the loop gets, and if the frame the haze was made of turns out to be
     unlike the rest of the loop, the loop's most typical frame replaces it
     over ten seconds — slow enough not to be seen as motion.

     Every picture has a frosted twin: the haze as the glass planes show it,
     rendered in the same pass (haze.js, frostOf) and clipped here to the
     planes that wear `use:frost` (ambient.svelte.js). The planes need no
     backdrop-filter, so nothing re-blurs the window when something in them
     moves; the twin fades with its haze as one layer.
     ===================================================================== */

  /** Crossfades: a new record with the art, on the panel's own crossfade
      of its picture (app.css, .np-video and .np-art: 600ms var(--ease));
      a layout change a touch slower; and the loop's settling is not meant
      to be watched. */
  const FADES = {
    first: { ms: 1600, easing: "cubic-bezier(0.4, 0, 0.2, 1)" },
    track: { ms: 600, easing: "cubic-bezier(0.2, 0, 0, 1)" },
    layout: { ms: 900, easing: "cubic-bezier(0.4, 0, 0.2, 1)" },
    settle: { ms: 10000, easing: "cubic-bezier(0.45, 0, 0.55, 1)" },
  };
  /** How long a record's Canvas is waited for (haze.awaiting) before the
      cover lights the haze instead. */
  const CANVAS_WAIT_MS = 2500;
  /** A black first frame (a Canvas fading up) is skipped this many times. */
  const BLANK_TRIES = 6;
  /** Window pixels per haze pixel, and the most haze pixels across. The
      picture is seen through a light lens (haze.js, LENS), so it needs a
      real resolution: at two window pixels per haze pixel the compositor's
      bilinear upscale is finer than the lens and never shows its steps. */
  const DENSITY = 2;
  const MAX_W = 1000;
  /** The first loop is sampled at most this many times over at most this long. */
  const PASS_SAMPLES = 10;
  const PASS_MAX_MS = 12000;
  /** The harness can have the haze drawn on the CPU, the fallback, to check
      it: dev/ui-harness.html?real&haze=cpu. */
  const CPU_ONLY = Boolean(import.meta.env?.DEV) && new URLSearchParams(location.search).get("haze") === "cpu";

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const trackKey = $derived(current?.id || current?.uri || "");
  const coverUrl = $derived(current?.cover_url ?? "");
  const tone = $derived(coverTone(coverUrl, current?.album_id || current?.uri || ""));
  const tint = $derived(rgbOf(tone.glow));

  let enabled = $state(true);
  let pageVisible = $state(!document.hidden);
  /** Bumped on every resize; the size itself is read when rendering. */
  let resized = $state(0);
  /** A Canvas whose frames cannot be read (served without CORS). */
  let unreadable = $state("");
  let layerEls = $state([]);
  let ambientEl = $state(null);
  let worker = $state.raw(null);

  $effect(() => {
    const media = window.matchMedia("(prefers-reduced-transparency: reduce)");
    const update = () => (enabled = !media.matches);
    const visibility = () => (pageVisible = !document.hidden);
    const resize = () => resized++;
    update();
    media.addEventListener?.("change", update);
    document.addEventListener("visibilitychange", visibility);
    window.addEventListener("resize", resize);
    return () => {
      media.removeEventListener?.("change", update);
      document.removeEventListener("visibilitychange", visibility);
      window.removeEventListener("resize", resize);
    };
  });

  /** Counters, for the harness to read. Not state: nothing renders them. */
  const stats = import.meta.env.DEV ? { posts: 0, received: 0, renders: 0, uploads: 0, samples: 0, workerMs: 0, gpu: null, fades: [], settles: [] } : null;
  if (import.meta.env?.DEV) {
    window.__haze = stats;
    /* The harness can frost planes that do not wear the action yet. */
    let worn = [];
    stats.frostPlanes = (selector) => {
      for (const handle of worn) handle.destroy();
      worn = selector ? [...document.querySelectorAll(selector)].map((el) => frost(el)) : [];
      return worn.length;
    };
  }

  /* The frosted region: one clip for every layer's twin, set as a custom
     property so a change is one style write. */
  $effect(() => {
    const el = ambientEl;
    if (!el) return;
    return watchFrost((clip) => el.style.setProperty("--frost-clip", clip));
  });

  /** The planes' frost as the worker needs it, read from the glass token. */
  function frostFor(geo) {
    const token = getComputedStyle(document.documentElement).getPropertyValue("--frost-plane");
    return parseFrost(token, geo.px);
  }

  /* ---- The layers ---------------------------------------------------------
     Three stacked layers, each a haze canvas and its frosted twin clipped
     to the planes. A new picture goes into a free one, on top, and
     fades in over whatever is showing; once it is fully in, everything under
     it is hidden and free again. Three, so that a picture can arrive while
     another is still fading in (a track skipped during a crossfade, or a
     new record during the loop's slow settle) and simply fade in over the
     mix. A fourth arrival waits for a layer; only the latest is kept. */
  /** Visible layers, bottom to top: { el, anim, left, covered } — from the
      geometry the picture was drawn for: the panel's edge, and whether a
      Canvas covered the panel out to the window's right edge. */
  let stack = [];
  let queued = null;
  let z = 1;

  function show(bitmap, frosted, fade, geo) {
    const el = layerEls.find((c) => c && !stack.some((s) => s.el === c));
    if (!el) {
      queued?.bitmap.close();
      queued?.frosted?.close();
      queued = { bitmap, frosted, fade, geo };
      return;
    }
    const [hazeCanvas, frostClip] = el.children;
    const frostCanvas = frostClip.firstElementChild;
    hazeCanvas.getContext("bitmaprenderer").transferFromImageBitmap(bitmap);
    if (stats) stats.uploads++;
    frostClip.hidden = !frosted;
    if (frosted) {
      frostCanvas.getContext("bitmaprenderer").transferFromImageBitmap(frosted);
      if (stats) stats.uploads++;
    }
    el.style.zIndex = String(++z);
    el.style.visibility = "visible";
    const { ms, easing } = FADES[fade];
    const entry = {
      el,
      anim: el.animate([{ opacity: 0 }, { opacity: 1 }], { duration: ms, easing, fill: "forwards" }),
      left: geo?.left ?? null,
      covered: coversPanel(geo),
    };
    slide(entry, dragEdge);
    stack.push(entry);
    if (stats) stats.fades.push({ fade, ms, at: Math.round(performance.now()), key: asked?.key });
    entry.anim.finished.then(
      () => {
        el.style.opacity = "1";
        entry.anim.cancel();
        entry.anim = null;
        for (const below of stack.splice(0, stack.indexOf(entry))) retire(below);
        if (queued) {
          const next = queued;
          queued = null;
          show(next.bitmap, next.frosted, next.fade, next.geo);
        }
      },
      () => {},
    );
  }

  function retire(entry) {
    entry.anim?.cancel();
    entry.el.style.visibility = "hidden";
    entry.el.style.opacity = "0";
    slide(entry, null);
  }

  /* ---- The drag ------------------------------------------------------------
     While the panel's edge is dragged nothing is rendered: each picture on
     screen is slid so the seam it was drawn at stays on the edge
     (ambient.svelte.js, THE FROST, 5). A slide uncovers a strip at one side
     of the window; the picture is stretched about the seam just enough to
     cover it — the left side always, the right only where the panel shows
     the haze (a cover, or a Canvas that stops short of an edge). The
     frost's clip is the planes' own and stays put: only the pictures inside
     it move. When the drag ends the slides stay as they are, and go with
     their layers when the picture drawn for the new edge has faded in over
     them. */
  let dragEdge = null;

  /** A Canvas hid the haze under the whole panel, out to the window's edge. */
  function coversPanel(geo) {
    const v = geo?.immersive ? geo.video : null;
    return Boolean(v && v.y <= 0.5 && v.y + v.h >= geo.h - 0.5 && v.x + v.w >= geo.w - 0.5);
  }

  $effect(() => watchShift((edge) => {
    dragEdge = edge;
    if (edge != null) for (const entry of stack) slide(entry, edge);
  }));

  /** Slide a layer's pictures so their seam is at `edge`; null puts them
      back where they were drawn. */
  function slide(entry, edge) {
    const [hazeCanvas, frostClip] = entry.el.children;
    const canvases = [hazeCanvas, frostClip.firstElementChild];
    const vw = window.innerWidth || 1;
    const from = entry.left == null ? null : entry.left * vw;
    if (edge == null || from == null || Math.abs(edge - from) < 0.25) {
      for (const c of canvases) c.style.transform = "";
      return;
    }
    const stretch = edge > from
      ? edge / Math.max(1, from)
      : entry.covered ? 1 : (vw - edge) / Math.max(1, vw - from);
    const transform = `translateX(${(edge - from).toFixed(2)}px) scaleX(${stretch.toFixed(4)})`;
    const origin = `${from.toFixed(2)}px 0`;
    for (const c of canvases) {
      c.style.transformOrigin = origin;
      c.style.transform = transform;
    }
  }

  /* ---- The worker -------------------------------------------------------- */

  /** The latest request; only its answer is shown. */
  let seq = 0;
  /** What the latest request was for: { key, kind, el, geo, tintKey }. */
  let asked = null;
  /** The source whose pixels the worker holds. */
  let held = null;
  /** Share of the shown haze that took the record's tint: a tint change
      only re-renders a haze that has some of it. */
  let grey = 0;
  let lit = false;
  let blankTries = 0;
  let timer = 0;
  let waitUntil = 0;
  let lastTrack = "";
  let lastAwaiting = false;
  let lastDiscrete = "";
  /** The source the type's light was last measured on. */
  let lightKey = "";

  function post(message, transfer = []) {
    if (stats) stats.posts++;
    worker.postMessage(message, transfer);
  }

  $effect(() => {
    if (!enabled || !layerEls[0]) return;
    const w = new Worker(new URL("../lib/haze.worker.js", import.meta.url), { type: "module" });
    w.onmessage = ({ data: m }) => receive(m);
    w.postMessage({ type: "init", cpu: CPU_ONLY });
    worker = w;
    return () => {
      w.terminate();
      worker = null;
      stopPass();
      clearTimeout(timer);
      timer = 0;
      queued?.bitmap.close();
      queued?.frosted?.close();
      queued = null;
      stack = [];
      asked = held = null;
      lit = false;
      lastDiscrete = "";
    };
  });

  function receive(m) {
    if (stats) stats.received++;
    switch (m.type) {
      case "haze": {
        if (stats) {
          stats.renders++;
          stats.workerMs += m.ms;
          stats.gpu = m.gpu;
          if (m.distance !== undefined) stats.settles.push(+m.distance.toFixed(3));
        }
        if (m.seq !== seq) {
          m.bitmap.close();
          m.frost?.close();
          return;
        }
        show(m.bitmap, m.frost, lit ? m.fade : "first", asked?.geo);
        lit = true;
        grey = m.grey;
        blankTries = 0;
        lightKey = m.key;
        if (!unreadableShown()) publishLight(m.light);
        if (m.fade !== "settle" && m.fade !== "layout" && asked?.kind === "video" && asked.key === m.key) {
          startPass(asked.el, m.key);
        }
        return;
      }
      case "blank":
        if (m.seq !== seq) return;
        held = null;
        asked = null;
        blankTries++;
        schedule(150);
        return;
      case "light":
        if (m.key === lightKey && !unreadableShown()) publishLight(m.light);
        return;
      case "done":
        if (stats && m.distance !== undefined) stats.settles.push(+m.distance.toFixed(3));
        if (m.key === lightKey && !unreadableShown()) publishLight(m.light);
        return;
    }
  }

  /* ---- Sources ----------------------------------------------------------- */

  /** The decoded cover, as a bitmap. A cover that cannot be had answers
      with the record's own two colours, so the haze never holds the last
      record's light. `for` is the cover URL it answers. */
  let still = $state.raw(null);
  $effect(() => {
    const url = coverUrl;
    const colours = [tone.tileA, tone.tileB];
    if (!current) return;
    let cancelled = false;
    const settle = (bitmap, key) => {
      if (cancelled) return bitmap?.close();
      /* The worker gets copies; this one is only kept to hand over again,
         so the one it replaces can go now rather than at some GC. */
      still?.bitmap.close();
      still = { key, for: url, bitmap };
    };
    const fallback = () => {
      if (cancelled) return;
      const c = Object.assign(document.createElement("canvas"), { width: 16, height: 16 });
      const g = c.getContext("2d");
      const ramp = g.createLinearGradient(0, 0, 16, 16);
      ramp.addColorStop(0, colours[0]);
      ramp.addColorStop(1, colours[1]);
      g.fillStyle = ramp;
      g.fillRect(0, 0, 16, 16);
      createImageBitmap(c).then((b) => settle(b, `tile:${colours.join(":")}`));
    };
    if (!url) {
      fallback();
      return () => (cancelled = true);
    }
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => createImageBitmap(img).then((b) => settle(b, url), fallback);
    img.onerror = fallback;
    img.src = imageUrl(url);
    return () => (cancelled = true);
  });

  /** The Canvas while the panel shows it and its frames can be read. */
  const video = $derived(haze.video && haze.video.currentSrc !== unreadable ? haze.video : null);
  /** The panel shows a Canvas whose frames cannot be read: its brightness
      is unknown, and nothing measured on the cover speaks for it. */
  const unreadableShown = () => Boolean(haze.video && haze.video.currentSrc === unreadable);

  /* ---- When to render -------------------------------------------------- */

  function geometry() {
    const vw = window.innerWidth || 1;
    const vh = window.innerHeight || 1;
    const w = Math.round(Math.min(MAX_W, Math.max(200, vw / DENSITY)));
    const h = Math.max(1, Math.round((w * vh) / vw));
    const px = vw / w;
    const left = haze.anchor ? haze.anchor.left : null;
    /* The video's rect on screen, in haze pixels: the haze carries the
       picture on from exactly where the video shows it. */
    const v = left != null ? haze.anchor.video : null;
    const video = v ? { x: v.x / px, y: v.y / px, w: v.w / px, h: v.h / px } : null;
    return { w, h, px, left, video, immersive: left != null && ui.immersive };
  }

  function sameGeo(a, b) {
    return Boolean(a && b) && a.w === b.w && a.h === b.h && a.immersive === b.immersive &&
      (a.left == null) === (b.left == null) && Math.abs((a.left ?? 0) - (b.left ?? 0)) < 0.004 &&
      sameRect(a.video, b.video);
  }

  /** The same video rect, to within half a haze pixel. */
  function sameRect(a, b) {
    if (!a || !b) return a === b;
    return ["x", "y", "w", "h"].every((k) => Math.abs(a[k] - b[k]) < 0.5);
  }

  /** What the haze should be made of now, or that it should wait. */
  function desired(geo) {
    const el = video;
    if (el && el.readyState >= 2) return { kind: "video", key: `video:${el.currentSrc}`, el };
    /* While the panel finds out whether this record has a Canvas, and loads
       it, the haze holds rather than lighting from the cover for a second
       and then changing again. */
    if (haze.awaiting && geo.left != null && performance.now() < waitUntil) {
      return { kind: "wait", until: waitUntil };
    }
    if (still && still.for === coverUrl) return { kind: "still", key: still.key, bitmap: still.bitmap };
    return { kind: "wait" };
  }

  /* Everything the picture depends on. A discrete change — the record, the
     source, the layout's shape, the colour — is answered at once, in the
     next task, once whatever changed with it has landed; the continuous
     ones (a window being resized, the panel's edge moving with it) wait
     until they stop. */
  $effect(() => {
    const deps = [
      worker,
      trackKey,
      video?.currentSrc ?? "",
      still?.key ?? "",
      haze.anchor?.left ?? null,
      haze.anchor?.video ?? null,
      ui.immersive,
      haze.awaiting,
      resized,
      tint.join(","),
      pageVisible,
    ];
    if (!deps[0]) return;
    untrack(() => {
      /* The wait for a Canvas is bounded from the moment it starts: a new
         record, or the panel starting to look for one. */
      if (trackKey !== lastTrack || (haze.awaiting && !lastAwaiting)) {
        waitUntil = performance.now() + CANVAS_WAIT_MS;
      }
      if (trackKey !== lastTrack) blankTries = 0;
      lastTrack = trackKey;
      lastAwaiting = haze.awaiting;
      if (!pageVisible) {
        haltPass();
        return;
      }
      armPass();
      const discrete = [deps[1], deps[2], deps[3], deps[4] == null, deps[5] == null, deps[6], deps[7], deps[9]].join("|");
      schedule(discrete !== lastDiscrete ? 0 : 250);
      lastDiscrete = discrete;
    });
  });

  function schedule(delay) {
    clearTimeout(timer);
    timer = setTimeout(flush, Math.max(0, delay));
  }

  function flush() {
    timer = 0;
    if (!worker || !pageVisible) return;
    const geo = geometry();
    const want = desired(geo);
    if (want.kind === "wait") {
      if (want.until) schedule(want.until - performance.now());
      return;
    }
    const tintKey = tint.join(",");
    const newSource = want.key !== asked?.key;
    const newGeo = !sameGeo(geo, asked?.geo);
    const newTint = tintKey !== asked?.tintKey && grey > 0.04;
    if (!newSource && !newGeo && !newTint) return;
    render(want, geo, tintKey, newSource ? "track" : "layout");
  }

  async function render(want, geo, tintKey, fade) {
    const id = ++seq;
    asked = { ...want, geo, tintKey };
    const message = {
      type: "render",
      seq: id,
      key: want.key,
      fade,
      geo,
      tint: [...tint],
      scale: geo.left == null ? RESTRAINED : 1,
      frost: frostFor(geo),
    };
    const transfer = [];
    if (want.key !== held) {
      stopPass();
      if (want.kind === "video") {
        let frame;
        try {
          frame = new VideoFrame(want.el);
        } catch (error) {
          asked = null;
          if (error?.name === "SecurityError") {
            /* A cross-origin Canvas without CORS: its light comes from the
               cover, and the type assumes the brightest picture there is. */
            unreadable = want.el.currentSrc;
            publishLight({ top: 1, bottom: 1 });
          } else {
            schedule(150);
          }
          return;
        }
        message.source = frame;
        message.video = true;
        message.allowBlank = want.el.paused || blankTries >= BLANK_TRIES;
        transfer.push(frame);
      } else {
        /* A bitmap can only be transferred once: the worker gets a copy. */
        const copy = await createImageBitmap(want.bitmap).catch(() => null);
        if (id !== seq) return copy?.close();
        message.source = copy;
        if (copy) transfer.push(copy);
      }
      held = want.key;
    }
    post(message, transfer);
  }

  /* ---- The first loop ------------------------------------------------------ */

  let pass = null;

  function startPass(el, key) {
    stopPass();
    const loopMs = Number.isFinite(el.duration) && el.duration > 0 ? el.duration * 1000 : 8000;
    const span = Math.min(loopMs, PASS_MAX_MS);
    pass = { el, key, span, every: Math.min(1500, Math.max(500, span / 8)), covered: 0, taken: 0, timer: 0, vfc: 0 };
    el.addEventListener("play", armPass);
    el.addEventListener("pause", haltPass);
    armPass();
  }

  /* One frame per interval of PLAYED time: a paused Canvas presents no
     frames, and the pass waits for it (the panel pauses it whenever the
     window loses focus or the details cover it). */
  function armPass() {
    const p = pass;
    if (!p || p.timer || p.vfc || p.el.paused || !pageVisible) return;
    p.timer = setTimeout(() => {
      p.timer = 0;
      if (pass !== p || p.el.paused) return;
      p.vfc = p.el.requestVideoFrameCallback(() => {
        p.vfc = 0;
        if (pass !== p || !worker) return;
        let frame;
        try {
          frame = new VideoFrame(p.el);
        } catch {
          return stopPass();
        }
        if (stats) stats.samples++;
        post({ type: "sample", key: p.key, frame, geo: geometry() }, [frame]);
        p.covered += p.every;
        p.taken++;
        if (p.covered >= p.span || p.taken >= PASS_SAMPLES) finishPass();
        else armPass();
      });
    }, p.every);
  }

  function haltPass() {
    const p = pass;
    if (!p) return;
    clearTimeout(p.timer);
    if (p.vfc) p.el.cancelVideoFrameCallback(p.vfc);
    p.timer = p.vfc = 0;
  }

  function stopPass() {
    const p = pass;
    if (!p) return;
    haltPass();
    p.el.removeEventListener("play", armPass);
    p.el.removeEventListener("pause", haltPass);
    pass = null;
  }

  /* The settle is answered under the current request's number: it is only
     shown if nothing newer has been asked for since, and anything newer is
     rendered from whichever frame the worker settled on. */
  function finishPass() {
    const p = pass;
    stopPass();
    if (asked?.key !== p.key) return;
    post({
      type: "settle",
      seq,
      key: p.key,
      fade: "settle",
      geo: asked.geo,
      tint: [...tint],
      scale: 1,
      frost: frostFor(asked.geo),
    });
  }
</script>

{#if enabled}
  <div class="ambient" aria-hidden="true" bind:this={ambientEl}>
    {#each [0, 1, 2] as i (i)}
      <div class="layer" bind:this={layerEls[i]}>
        <canvas width="1" height="1"></canvas>
        <div class="frost" hidden><canvas width="1" height="1"></canvas></div>
      </div>
    {/each}
  </div>
{/if}

<style>
  /* Behind everything, stretched over the window by the compositor: each
     picture is half the window's size (DENSITY), its frost a quarter, and
     both are already blurred, so the bilinear upscale is all the smoothing
     they need. The layers only ever
     change opacity, on the compositor, and are hidden once covered. Each
     layer's frosted twin lies over its haze, cut to the planes by its own
     box, so a picture slid during a drag (slide) moves under the cut. */
  .ambient {
    position: fixed;
    inset: 0;
    z-index: 0;
    pointer-events: none;
    background: var(--bg-0);
  }
  .layer {
    position: absolute;
    inset: 0;
    opacity: 0;
    visibility: hidden;
    will-change: opacity;
  }
  canvas,
  .frost {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  .frost {
    clip-path: var(--frost-clip, inset(50%));
  }
  .frost[hidden] {
    display: none;
  }
</style>
