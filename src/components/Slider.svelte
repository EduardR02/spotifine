<script>
  import { untrack } from "svelte";

  /**
   * Pointer-driven slider for seek and volume. Holds a local value while
   * dragging; callers choose live drag updates or a release-only commit.
   *
   * The fill is driven by `--p` (0..1) and animated with `transform: scaleX`
   * rather than `width: %`. Width would force layout and paint on every
   * playhead tick, forever, which is exactly the cost this app exists to
   * avoid; a transform stays on the compositor.
   */
  let {
    min = 0,
    max = 100,
    value,
    onCommit,
    label = "",
    step = null,
    kind = "seek",
    onDragStart = null,
    onDragChange = null,
    formatValue = null,
    gestureKey = null,
    onDragCancel = null,
  } = $props();

  let track = $state(null);
  let trackGeometry = null;
  let trackWidth = $state(0);
  let drag = $state(null);
  let hover = $state(null);
  let pointerId = null;
  let dragIdentity = null;
  /**
   * Whether the slider was last focused by a pointer rather than by the
   * keyboard. `:focus-visible` is not enough on its own here: `onPointerDown`
   * focuses the track so the arrow keys work straight after a drag, and on the
   * very first interaction after the app opens Chromium counts that
   * programmatic focus as keyboard-ish — so the first drag of a session used to
   * leave a keyboard ring on the volume control until some later click took
   * focus away (pressing Play, in practice). A drag is not keyboard
   * navigation, so the ring is suppressed until a key actually arrives.
   */
  let pointerFocus = $state(false);
  const display = $derived(drag !== null ? drag : value);
  const p = $derived(max > min ? Math.min(1, Math.max(0, (display - min) / (max - min))) : 0);
  const hoverP = $derived(
    hover !== null && max > min ? Math.min(1, Math.max(0, (hover - min) / (max - min))) : 0
  );
  const valueText = $derived(formatValue ? formatValue(display) : null);
  const tooltipId = $derived(`${kind}-slider-tooltip`);

  function refreshGeometry(element = track) {
    if (!element) {
      trackGeometry = null;
      trackWidth = 0;
      return;
    }
    const r = element.getBoundingClientRect();
    trackGeometry = { left: r.left, width: r.width };
    trackWidth = r.width;
  }

  function fromClientX(x) {
    const r = trackGeometry;
    if (!r || r.width <= 0) return min;
    return min + Math.min(1, Math.max(0, (x - r.left) / r.width)) * (max - min);
  }

  $effect(() => {
    const element = track;
    if (!element) return;
    const updateGeometry = () => {
      trackGeometry = null;
      refreshGeometry(element);
    };
    updateGeometry();
    const resizeObserver = new ResizeObserver(updateGeometry);
    resizeObserver.observe(element);
    return () => {
      resizeObserver.disconnect();
      cancelDrag(element);
      trackGeometry = null;
      trackWidth = 0;
    };
  });

  // Call the identity getter at event time too: a pointer event can arrive
  // before the reactive cancellation effect has run.
  function gestureIsCurrent() {
    return Object.is(dragIdentity, gestureKey?.());
  }

  function cancelDrag(element = track) {
    const capturedPointer = pointerId;
    if (capturedPointer === null) return;
    pointerId = null;
    dragIdentity = null;
    drag = null;
    hover = null;
    onDragCancel?.();
    if (element?.hasPointerCapture(capturedPointer)) {
      element.releasePointerCapture(capturedPointer);
    }
  }

  $effect(() => {
    const identity = gestureKey?.();
    if (pointerId !== null && !Object.is(identity, dragIdentity)) {
      untrack(() => cancelDrag());
    }
  });

  function commit(v) {
    onCommit(Math.min(max, Math.max(min, v)));
  }

  function onPointerDown(e) {
    if (pointerId !== null || e.button !== 0 || !e.isPrimary) return;
    e.preventDefault();
    refreshGeometry();
    pointerFocus = true;
    track.focus({ preventScroll: true });
    pointerId = e.pointerId;
    dragIdentity = gestureKey?.();
    drag = fromClientX(e.clientX);
    hover = drag;
    onDragStart?.(drag);
    track.setPointerCapture(e.pointerId);
  }

  function onPointerMove(e) {
    if (pointerId !== null && e.pointerId !== pointerId) return;
    if (pointerId !== null && !gestureIsCurrent()) {
      cancelDrag();
      return;
    }
    hover = fromClientX(e.clientX);
    if (drag === null) return;
    drag = hover;
    onDragChange?.(drag);
  }
  function onPointerEnter(e) {
    refreshGeometry();
    hover = fromClientX(e.clientX);
  }

  function onPointerUp(e) {
    if (drag === null || e.pointerId !== pointerId) return;
    if (!gestureIsCurrent()) {
      cancelDrag();
      return;
    }
    const v = fromClientX(e.clientX);
    drag = null;
    pointerId = null;
    dragIdentity = null;
    if (track.hasPointerCapture(e.pointerId)) track.releasePointerCapture(e.pointerId);
    commit(v);
  }

  function onPointerCancel(e) {
    if (e.pointerId === pointerId) cancelDrag();
  }

  function onKeyDown(e) {
    // A key means this is keyboard navigation, so the ring belongs.
    pointerFocus = false;
    if ((e.ctrlKey || e.metaKey || e.altKey) && e.key.startsWith("Arrow")) return;
    const span = max - min;
    const small = step ?? Math.max(1, span / 20);
    const big = span / 5;
    let next = null;
    if (e.key === "ArrowRight" || e.key === "ArrowUp") next = display + small;
    else if (e.key === "ArrowLeft" || e.key === "ArrowDown") next = display - small;
    else if (e.key === "PageUp") next = display + big;
    else if (e.key === "PageDown") next = display - big;
    else if (e.key === "Home") next = min;
    else if (e.key === "End") next = max;
    if (next !== null) {
      e.preventDefault();
      commit(next);
    }
  }
</script>

<span
  class={kind === "vol" ? "vol" : "rail-hit"}
  class:pointer-focus={pointerFocus}
  style:--track-w="{trackWidth}px"
  role="slider"
  tabindex="0"
  aria-label={label}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={Math.round(display)}
  aria-valuetext={valueText}
  aria-describedby={kind === "seek" ? tooltipId : undefined}
  bind:this={track}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerenter={onPointerEnter}
  onpointerleave={() => (hover = null)}
  onpointerup={onPointerUp}
  onpointercancel={onPointerCancel}
  onlostpointercapture={onPointerCancel}
  onkeydown={onKeyDown}
  onblur={() => (pointerFocus = false)}
>
  {#if kind === "vol"}
    <span class="vol-rail"><span class="vol-fill" style:--p={p}></span></span>
  {:else}
    <span class="rail"><span class="rail-fill" style:--p={p}></span></span>
    <span class="rail-knob" style:--p={p}></span>
    <span
      id={tooltipId}
      class="seek-tip glass-overlay"
      class:visible={hover !== null}
      style:--tip-x="{hoverP * 100}%"
      role="tooltip"
    >
      {formatValue ? formatValue(hover ?? display) : Math.round(hover ?? display)}
    </span>
  {/if}
</span>

<style>
  [role="slider"] {
    touch-action: none;
  }
  .vol {
    height: 28px;
    border-radius: var(--r1);
  }
  .vol:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 4px;
  }
  /* A pointer drag focuses the track (so the arrow keys work straight after
     it), and on the first interaction of a session Chromium also treats that
     focus as `:focus-visible`. The outline is for keyboard navigation, so it
     waits for a key: this rule outranks the global `:focus-visible` ring. */
  [role="slider"].pointer-focus:focus-visible {
    outline: none;
  }
</style>
