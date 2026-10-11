/**
 * Drag-and-drop: the one gesture controller behind "grab a row and move it".
 *
 *   - a song onto a sidebar playlist row   -> COPY there (add only; the
 *     source keeps the track — dropping on any other list adds too)
 *   - a rail playlist or folder            -> MOVE it in the library: onto a
 *     folder heading puts it in that folder, onto a row puts it beside that
 *     row (in that row's folder, or out of every folder at the top level)
 *
 * Spatial work is scheduled when the pointer or a scroller moves, and runs
 * continuously only while autoscrolling. `pointermove` only records coordinates;
 * it never reads layout. When no drag runs, this module's cost is zero.
 *
 * Reactivity contract: `trackDrag` is the reactive projection components read
 * (TrackList draws the insertion bar and parts its rows around the slot;
 * Sidebar lights up its drop targets). The DOM ghost is deliberately outside
 * Svelte — moving one transform per frame must not re-render anything.
 */

import { api, library, promotePlaylist } from "./state.svelte.js";

export const trackDrag = $state({
  active: false,
  /** The grabbed row's track object and uri. */
  track: null,
  uri: "",
  /**
   * The playlist the row was dragged FROM, when it is an owned playlist.
   * Dropping back on it is a cancel, never a self-copy.
   */
  sourcePlaylistId: null,
  sourceIndex: -1,
  /** Wrapper element of the hovered reorder zone, or null. */
  listEl: null,
  /**
   * FINAL landing index (0..length-1); -1 when no reorder target is under
   * the pointer. The insertion bar, the parting preview and the reorder
   * call all read this one number, so they cannot disagree.
   */
  slot: -1,
});

/**
 * The rail's own drag: a library row (`item`, `{ kind, id }`) in hand, and
 * the folder a release would put it in — a folder id, null for the top
 * level, undefined while no drop would do anything (over the row's own
 * folder, into itself, or outside the rail). The Sidebar lights the target
 * from this; a song drag never sets it.
 */
export const railDrag = $state({ active: false, item: null, target: undefined });

/* ------------------------------------------------------------------ */
/* Drop-zone registry                                                  */
/* ------------------------------------------------------------------ */

const zones = new Set();

/**
 * Registers a TrackList body as a gesture owner and optional reorder target.
 * Unregistering cancels that owner's press/drag, not an individual recycled
 * row. `meta()` supplies live source, occurrence lookup, length and handler.
 */
export function registerReorderZone(el, meta) {
  const zone = { el, meta };
  zones.add(zone);
  return () => {
    zones.delete(zone);
    if (press?.kind === "track" && press.ctx.ownerEl === el) endDrag(false);
  };
}

/**
 * True for ~350ms after a real drag ended. Row double-clicks check this so
 * the release of a long drag cannot land as an accidental "play".
 */
let gestureEndedAt = -1e9;
export function justDragged() {
  return performance.now() - gestureEndedAt < 350;
}

/* ------------------------------------------------------------------ */
/* Gesture internals — deliberately non-reactive                       */
/* ------------------------------------------------------------------ */

const PRESS_THRESHOLD = 6; // px of travel before a press becomes a drag
const EDGE_BAND = 64; // px autoscroll band inside a scroller
const MAX_SCROLL_SPEED = 1100; // px/s at full band penetration
const ROW_H = 48; // must track --row-h
/* Compact-pill ghost geometry. The pointer carries the settled pill by a
   fixed handle — 18px in from its left edge, vertically centred — and the
   whole thing stays under a 260px cap however long the title is. */
const PILL_MAX_WIDTH = 260;
const PILL_ANCHOR_X = 18;
const PILL_ANCHOR_Y = ROW_H / 2;
/* The lift scale, applied with transform-origin 0 0 (see positionGhost and
   the #drag-layer CSS). The pill's sliding margins are expressed in
   pre-scale coordinates, so both files need the number. */
const GHOST_SCALE = 1.028;

let press = null; // pending press before the threshold, then live drag state
let pointerX = 0;
let pointerY = 0;
let ghostWrap = null; // positioned wrap holding the pill ghost
let pillEl = null; // the pill inside ghostWrap; its margins carry the anchor
let originRect = null; // where the ghost came from (fly-back target)
let layer = null; // #drag-layer, created once per activation
let rafId = 0;
let lastFrame = 0;
let scrollers = []; // scrollable elements resolved once per drag

/**
 * A row was pressed. Nothing visible happens until the pointer travels past
 * the threshold; press-and-release stays the no-op click it always was.
 *
 * ctx: { event, rowEl, ownerEl, source, occurrence, multiplicity, index, track, playlistId }
 *   `playlistId` present => move semantics (owned playlist source).
 */
export function pressTrack(ctx) {
  if (press) return;
  const e = ctx.event;
  if (e.button !== 0 || e.isPrimary === false) return;
  const t = ctx.track;
  if (!t?.uri || t.unavailable) return;

  hold("track", ctx, e);
}

/** Starts a press: nothing visible until the pointer travels past the
    threshold. `kind` is "track" or "library". */
function hold(kind, ctx, e) {
  press = {
    kind,
    active: false,
    ctx,
    pointerId: e.pointerId,
    rowEl: ctx.rowEl,
    startX: e.clientX,
    startY: e.clientY,
    grabX: 0,
    grabY: 0,
    dropRow: null,
    railTarget: undefined,
  };
  window.addEventListener("pointermove", onPressMove);
  window.addEventListener("pointerup", onPressUp);
  window.addEventListener("pointercancel", onPressCancel);
  window.addEventListener("keydown", onPressKey, true);
}

/**
 * A rail row was pressed. ctx: { event, rowEl, item, accepts(target),
 * drop(target) } — `accepts` says whether a release over `target` would
 * change anything, `drop` carries it out.
 */
export function pressLibraryItem(ctx) {
  if (press) return;
  const e = ctx.event;
  if (e.button !== 0 || e.isPrimary === false || !ctx.item) return;
  hold("library", ctx, e);
}

/**
 * The folder a release at a window point would move the row into. A folder
 * heading means that folder; a row means the folder that row sits in; the
 * rail's empty space means the top level. Outside the rail, nothing.
 */
function railHit(x, y) {
  const el = document.elementFromPoint(x, y);
  if (!el?.closest?.(".lib-list")) return undefined;
  const heading = el.closest("[data-folder]");
  if (heading) return heading.dataset.folder;
  const row = el.closest("[data-parent]");
  return row ? row.dataset.parent || null : null;
}

/** `railHit`, kept only when the drop would change something; asked once
    per new target rather than per frame. */
function railTargetAt(held, x, y) {
  const target = railHit(x, y);
  if (target === undefined) return undefined;
  if (held.railTarget && target === held.railTarget.raw) return held.railTarget.value;
  const value = held.ctx.accepts(target) ? target : undefined;
  held.railTarget = { raw: target, value };
  return value;
}

function releaseListeners() {
  window.removeEventListener("pointermove", onPressMove);
  window.removeEventListener("pointerup", onPressUp);
  window.removeEventListener("pointercancel", onPressCancel);
  window.removeEventListener("keydown", onPressKey, true);
}

function onPressMove(e) {
  if (!press || e.pointerId !== press.pointerId) return;
  pointerX = e.clientX;
  pointerY = e.clientY;
  if (press.active) {
    scheduleFrame();
    return;
  }
  const dx = e.clientX - press.startX;
  const dy = e.clientY - press.startY;
  if (dx * dx + dy * dy < PRESS_THRESHOLD * PRESS_THRESHOLD) return;
  activate();
}

function onPressKey(e) {
  if (!press || e.key !== "Escape") return;
  e.preventDefault();
  e.stopPropagation();
  endDrag(false);
}

function onPressUp(e) {
  if (!press || e.pointerId !== press.pointerId) return;
  pointerX = e.clientX;
  pointerY = e.clientY;
  endDrag(press.active);
}

function onPressCancel(e) {
  if (!press || e.pointerId !== press.pointerId) return;
  endDrag(false);
}

function activate() {
  const ctx = press.ctx;
  press.active = true;

  layer = ensureLayer();
  ghostWrap = buildGhost(press.rowEl); // also fixes press.grabX/grabY

  if (press.kind === "library") {
    railDrag.active = true;
    railDrag.item = ctx.item;
    railDrag.target = undefined;
    scrollers = [];
    const rail = press.rowEl.closest(".lib-list");
    if (rail) {
      scrollers.push(rail);
      rail.addEventListener("scroll", scheduleFrame, { passive: true });
    }
    document.documentElement.classList.add("dragging-track");
    positionGhost();
    lastFrame = performance.now();
    scheduleFrame();
    return;
  }

  const d = trackDrag;
  d.active = true;
  d.track = ctx.track;
  d.uri = ctx.track.uri;
  d.sourcePlaylistId = ctx.playlistId ?? null;
  d.sourceIndex = ctx.index;
  d.listEl = null;
  d.slot = -1;

  scrollers = [];
  const paneScroller = press.rowEl.closest(".scroll");
  if (paneScroller) scrollers.push(paneScroller);
  const rail = document.querySelector(".lib-list");
  if (rail && rail !== paneScroller) scrollers.push(rail);
  for (const el of scrollers) el.addEventListener("scroll", scheduleFrame, { passive: true });

  document.documentElement.classList.add("dragging-track");
  positionGhost();
  lastFrame = performance.now();
  scheduleFrame();
}

function ensureLayer() {
  let el = document.getElementById("drag-layer");
  if (!el) {
    el = document.createElement("div");
    el.id = "drag-layer";
    document.body.appendChild(el);
  }
  return el;
}

/**
 * Builds the ghost: the pressed row folded into a compact pill carrying its
 * art tile and title block — cloned nodes, so Cover's fallback tiles keep
 * working offline — styled entirely by the #drag-layer block in app.css.
 *
 * The gesture opens at the row's FULL geometry: the wrap starts as wide as
 * the row, under the pointer exactly where it closed, then compresses over
 * var(--d2)/var(--ease). Two CSS transitions share that one clock — the
 * wrap's width eases down to the pill's natural width while the pill slides
 * inside the wrap so the grab point travels to the pill's handle (18px in
 * from its left edge, vertically centred). One continuous morph: frame()'s
 * per-frame transform math is untouched by all of this, and reduced motion
 * lands directly in pill form because the media query kills the transitions.
 * Measuring the natural width costs one layout read per activation, never
 * per frame.
 */
function buildGhost(rowEl) {
  const rect = rowEl.getBoundingClientRect();
  originRect = rect;
  press.grabX = pointerX - rect.left;
  press.grabY = pointerY - rect.top;

  const wrap = document.createElement("div");
  wrap.className = "tl-drag-ghost";
  const pill = document.createElement("div");
  pill.className = "tl-drag-pill glass-overlay";
  /* A song row carries .c-art/.c-title; a rail row its cover and name. */
  const art = rowEl.querySelector(".c-art, .art");
  if (art) pill.appendChild(art.cloneNode(true));
  const title = rowEl.querySelector(".c-title, .lib-name, .lib-folder-name");
  if (title) pill.appendChild(title.cloneNode(true));
  wrap.appendChild(pill);
  layer.appendChild(wrap);
  pillEl = pill;

  /* Natural pill width clamped to the design cap, measured off an
     unconstrained render that is corrected before any frame can paint it. */
  wrap.style.width = "max-content";
  const pillWidth = Math.min(pill.getBoundingClientRect().width, PILL_MAX_WIDTH);

  /* Start geometry first — the row's own footprint, pill flush — committed
     with a style flush so the transitions have something to ease FROM; then
     the end state: fold to the pill, slide it under the pointer's handle. */
  wrap.style.width = `${rect.width}px`;
  pill.style.marginLeft = "0px";
  pill.style.marginTop = "0px";
  void wrap.offsetWidth;
  wrap.style.width = `${pillWidth}px`;
  pill.style.marginLeft = `${(press.grabX - PILL_ANCHOR_X) / GHOST_SCALE}px`;
  pill.style.marginTop = `${(press.grabY - PILL_ANCHOR_Y) / GHOST_SCALE}px`;

  return wrap;
}

function positionGhost() {
  if (!ghostWrap) return;
  ghostWrap.style.transform =
    `translate3d(${pointerX - press.grabX}px, ${pointerY - press.grabY}px, 0) scale(${GHOST_SCALE})`;
}

/** Ease-in weight for the autoscroll band: gentle at the edge, brisk deeper. */
function bandPenetration(distance) {
  const t = 1 - distance / EDGE_BAND;
  return t * t;
}

/**
 * One hit-test pass at a window point: a reorder zone containing the point
 * wins outright; otherwise a sidebar playlist row is the candidate. Over a
 * list that takes no drops (sorted view, read-only list) neither applies and
 * the drop cancels. Shared by the rAF loop and by endDrag — the pointer can
 * come up before the loop's first pass, and the drop must still resolve.
 */
function hitTest(x, y, held = press) {
  let zoneEl = null;
  let slot = -1;
  for (const z of zones) {
    if (!z.el.isConnected || z.el !== held?.ctx.ownerEl) continue;
    const r = z.el.getBoundingClientRect();
    if (x < r.left || x > r.right || y < r.top || y >= r.bottom) continue;
    const meta = z.meta();
    if (meta.source !== held.ctx.source) continue;
    const from = meta.indexOf(held.ctx.occurrence, held.ctx.multiplicity);
    if (meta.canReorder && meta.length > 0 && from >= 0) {
      trackDrag.sourceIndex = from;
      const last = meta.length - 1;
      const raw = Math.round((y - r.top) / ROW_H);
      if (raw <= from) {
        /* Upward and at-origin: every original boundary is a real slot. */
        slot = Math.max(0, Math.min(last, raw));
      } else if (raw === from + 1) {
        /* The WHOLE immediate next row means "land right after me". The
           boundary between the dragged row and this one decodes to a
           no-op, so it must never be offered — offering it is what made
           one-slot-down drags feel impossible. */
        slot = Math.min(last, from + 1);
      } else {
        /* Deeper pointers are judged by midpoint WITHOUT the dragged row
           present — which is exactly the final landing index. */
        slot = Math.max(0, Math.min(last, raw - 1));
      }
      zoneEl = z.el;
    }
    break; // at most one zone contains the point; stop at the first overlap
  }
  const dropRow =
    zoneEl ? null : (document.elementFromPoint(x, y)?.closest?.(".lib-row[data-pid]") ?? null);
  return { zoneEl, slot, dropRow };
}

/** A frame runs on movement and keeps running only while an edge is scrolling. */
function scheduleFrame() {
  if (!press?.active || rafId) return;
  lastFrame = performance.now();
  rafId = requestAnimationFrame(frame);
}

function frame(now) {
  rafId = 0;
  if (!press?.active) return;
  const dt = Math.max(0, Math.min(48, now - lastFrame)) / 1000;
  lastFrame = now;
  const d = trackDrag;
  let scrolling = false;

  for (const el of scrollers) {
    const maxScroll = el.scrollHeight - el.clientHeight;
    if (maxScroll <= 0) continue;
    const r = el.getBoundingClientRect();
    if (pointerX < r.left || pointerX > r.right) continue;
    const fromTop = pointerY - r.top;
    const fromBottom = r.bottom - pointerY;
    if (fromTop >= 0 && fromTop < EDGE_BAND) {
      el.scrollTop -= MAX_SCROLL_SPEED * bandPenetration(fromTop) * dt;
      scrolling ||= el.scrollTop > 0;
    } else if (fromBottom >= 0 && fromBottom < EDGE_BAND) {
      el.scrollTop += MAX_SCROLL_SPEED * bandPenetration(fromBottom) * dt;
      scrolling ||= el.scrollTop < maxScroll;
    }
  }

  if (press.kind === "library") {
    const target = railTargetAt(press, pointerX, pointerY);
    if (railDrag.target !== target) railDrag.target = target;
  } else {
    const { zoneEl, slot, dropRow } = hitTest(pointerX, pointerY);
    if (d.listEl !== zoneEl) d.listEl = zoneEl;
    if (d.slot !== slot) d.slot = slot;
    press.dropRow = dropRow;
  }

  positionGhost();
  // Keep edge intent alive when the first elapsed slice rounds below one pixel.
  // Continuing from the RAF timestamp also includes the full next frame interval.
  if (scrolling) rafId = requestAnimationFrame(frame);
}

function endDrag(commit) {
  const p = press;
  press = null;
  releaseListeners();
  cancelAnimationFrame(rafId);
  rafId = 0;
  for (const el of scrollers) el.removeEventListener("scroll", scheduleFrame);
  scrollers = [];

  if (p?.kind === "library") {
    const wasActive = railDrag.active;
    let moved = false;
    if (commit && wasActive) {
      const target = railTargetAt(p, pointerX, pointerY);
      if (target !== undefined) {
        p.ctx.drop(target);
        moved = true;
      }
    }
    railDrag.active = false;
    railDrag.item = null;
    railDrag.target = undefined;
    if (wasActive) {
      document.documentElement.classList.remove("dragging-track");
      retireGhost(moved ? "dissolve" : "return");
      gestureEndedAt = performance.now();
      suppressReleaseClick();
    }
    return;
  }

  const d = trackDrag;
  const wasActive = d.active;
  let outcome = "cancel";

  let source = null;
  if (p) for (const zone of zones) {
    if (zone.el === p.ctx.ownerEl && zone.el.isConnected &&
        zone.meta().source === p.ctx.source) {
      source = zone;
      break;
    }
  }
  if (commit && wasActive && source && source.meta().indexOf(p.ctx.occurrence, p.ctx.multiplicity) >= 0) {
    /* The last pointermove may still be waiting for its frame when the pointer
       comes up. Resolve at the final coordinates rather than using the
       previous frame's target. */
    const hit = hitTest(pointerX, pointerY, p);
    d.listEl = hit.zoneEl;
    d.slot = hit.slot;
    p.dropRow = hit.dropRow;
    let matched = null;
    if (d.listEl) {
      for (const z of zones) {
        if (z.el === d.listEl) { matched = z; break; }
      }
    }
    if (matched && matched === source) {
      const meta = matched.meta();
      const from = meta.indexOf(p.ctx.occurrence, p.ctx.multiplicity);
      /* `slot` is already the FINAL index — bar, parting preview and this
         call all derive from it; no coordinate conversion left. */
      const to = d.slot;
      if (meta.canReorder && from >= 0 && to !== from && to >= 0 && to < meta.length) {
        meta.run(from, to);
        outcome = "reorder";
      }
    } else if (p.dropRow) {
      const pid = p.dropRow.dataset.pid;
      if (pid !== d.sourcePlaylistId) {
        commitToPlaylist(pid, d.uri);
        pulse(p.dropRow);
        outcome = "playlist";
      }
    }
  }

  d.active = false;
  d.track = null;
  d.uri = "";
  d.sourcePlaylistId = null;
  d.sourceIndex = -1;
  d.listEl = null;
  d.slot = -1;

  if (wasActive) {
    document.documentElement.classList.remove("dragging-track");
    retireGhost(outcome === "cancel" ? "return" : "dissolve");
    gestureEndedAt = performance.now();
    suppressReleaseClick();
  }
}

let releaseGuardTimer = 0;

/**
 * A drag that ends over what it started on leaves the browser primed to
 * synthesize a click at the release point. Artist names are role="link"
 * SPANS rather than buttons precisely so they can ellipsize (see
 * ArtistLinks.svelte), which means that synthetic click passes TrackList's
 * interactive-cell press guard and navigates. After a REAL drag, swallow
 * the first click that follows within ~400ms — capture phase,
 * preventDefault + stopPropagation, gone once it has fired. The timeout
 * disarms the trap when no click comes, so a drag released into empty
 * space can never eat the user's NEXT intentional click.
 */
function suppressReleaseClick() {
  clearTimeout(releaseGuardTimer);
  const swallow = (e) => {
    disarm();
    e.preventDefault();
    e.stopPropagation();
  };
  const disarm = () => {
    document.removeEventListener("click", swallow, true);
    clearTimeout(releaseGuardTimer);
  };
  document.addEventListener("click", swallow, true);
  releaseGuardTimer = setTimeout(disarm, 400);
}

/**
 * Copy onto a sidebar playlist. Deliberately an ADD, never a move: dragging
 * out of a playlist leaves the original in place. Failures stay silent like
 * every other fire-and-forget playlist edit — the next reconciliation paints
 * the server's truth.
 */
function commitToPlaylist(pid, uri) {
  const target = library.find((p) => p.id === pid);
  api.addPlaylistTracks(pid, [uri])
    .then(() => {
      if (target) promotePlaylist(target.id);
      api.touchPlaylistActivity(pid).catch(() => {});
    })
    .catch(() => {});
}

/** Brief accent wash on the playlist row that received the drop. WAAPI, not
    a class: Svelte reconciliation cannot strip an animation mid-flight. */
function pulse(rowEl) {
  try {
    rowEl.animate(
      [
        { background: "color-mix(in srgb, var(--rose-ink) 22%, transparent)" },
        { background: "rgba(255,255,255,0)" },
      ],
      { duration: 550, easing: "ease-out" },
    );
  } catch {
    /* No WAAPI: the drop itself is feedback enough */
  }
}

/**
 * Ghost exit paths. Cancel flies the row home — the gesture undoes itself in
 * front of the user, and the pill unfolds back into the row's footprint on
 * the way (same clock as the compression, run backwards). A committed drop
 * dissolves in place: the real row is already where the ghost was, so
 * flying it anywhere would be a lie. Each ghost owns its removal, so a quick
 * second drag cannot strand the first one in the page.
 */
function retireGhost(mode) {
  if (!ghostWrap) return;
  const wrap = ghostWrap;
  const pill = pillEl;
  ghostWrap = null;
  pillEl = null;

  if (mode === "return" && originRect) {
    /* The inline transition list must carry width itself: writing it replaces
       the stylesheet's transition property wholesale. The pill re-declares
       its own margins to match the flight rather than var(--d2). */
    const flight = "220ms cubic-bezier(0.2, 0, 0, 1)";
    wrap.style.transition =
      `transform ${flight}, width ${flight}, opacity 220ms linear`;
    wrap.style.transform =
      `translate3d(${originRect.left}px, ${originRect.top}px, 0) scale(1)`;
    wrap.style.width = `${originRect.width}px`;
    if (pill) {
      pill.style.transition = `margin-left ${flight}, margin-top ${flight}`;
      pill.style.marginLeft = "0px";
      pill.style.marginTop = "0px";
    }
    wrap.style.opacity = "0.55";
    setTimeout(() => wrap.remove(), 240);
  } else {
    wrap.style.transition = "opacity 130ms linear";
    wrap.style.opacity = "0";
    setTimeout(() => wrap.remove(), 150);
  }
}

