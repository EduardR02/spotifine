<script>
  import { formatExactTime } from "../lib/time.js";
  import { indexWaveform, traceWaveform } from "../lib/track-waveform.js";

  let {
    durationMs,
    waveform = null,
    waveformState = "idle",
    waveformError = "",
    disabled = false,
    cuts = $bindable([]),
    loopRange = $bindable(null),
    selected = $bindable(null),
    cursorMs = $bindable(0),
    previewActive = false,
    previewPlaying = false,
    previewPositionMs = null,
    previewPositionReader = null,
    onsave = null,
    oncommit = null,
    onundo = null,
    onredo = null,
    onseek = null,
  } = $props();

  const MIN_VIEW_MS = 250;
  const SNAP_PX = 8;
  const DRAG_THRESHOLD_PX = 3;
  let keySequence = 0;
  let mainStage = $state(null);
  let overviewStage = $state(null);
  let mainCanvas = $state(null);
  let playheadCursor = $state(null);
  let previewPlayhead = $state(null);
  let overviewCanvas = $state(null);
  let hoverCursor = $state(null);
  let hoverLabel = $state(null);
  let viewStart = $state(0);
  let viewEnd = $state(1);
  let viewportDuration = 0;
  let drag = $state(null);
  let mainDrawFrame = 0;
  let overviewDrawFrame = 0;
  let pointerFrame = 0;
  let hoverFrame = 0;
  let previewFrame = 0;
  let pendingPointer = null;
  let pendingHover = null;
  let mainSize = { width: 0, height: 0 };
  let overviewSize = { width: 0, height: 0 };
  let cursorValue = 0;

  const duration = $derived(Math.max(1, Math.round(Number(durationMs) || 1)));
  const waveIndex = $derived(indexWaveform(waveform));
  const viewSpan = $derived(Math.max(1, viewEnd - viewStart));
  const zoomLabel = $derived(`${Math.max(1, Math.round(duration / viewSpan))}×`);
  const selectedRange = $derived.by(() => {
    if (selected?.type === "loop") return loopRange;
    if (selected?.type === "cut") return cuts.find((range) => range._key === selected.key) ?? null;
    return null;
  });
  const selectedReadout = $derived(
    selectedRange
      ? `${selected?.type === "loop" ? "Loop" : "Cut"} · ${formatExactTime(selectedRange.start_ms)}–${formatExactTime(selectedRange.end_ms)}`
      : "No region selected",
  );
  const cursorReadout = $derived(`Cursor · ${formatExactTime(cursorValue)}`);

  function newKey(prefix) {
    keySequence += 1;
    return `${prefix}-${Date.now().toString(36)}-${keySequence.toString(36)}`;
  }

  function clamp(value, min, max) {
    return Math.min(max, Math.max(min, value));
  }

  function copyCuts(value = cuts) {
    return value.map((range) => ({ ...range }));
  }

  function copyEdit(editCuts = cuts, editLoop = loopRange) {
    return {
      cuts: copyCuts(editCuts),
      loopRange: editLoop ? { ...editLoop } : null,
    };
  }

  function editKey(edit) {
    return JSON.stringify({
      cuts: (edit?.cuts ?? []).map(({ start_ms, end_ms }) => [start_ms, end_ms]),
      loop: edit?.loopRange
        ? [edit.loopRange.start_ms, edit.loopRange.end_ms, edit.loopRange.play_count ?? null]
        : null,
    });
  }

  function sortCuts(value) {
    return value.sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms);
  }

  function occupied(type, key) {
    const ranges = cuts
      .filter((range) => type !== "cut" || range._key !== key)
      .map((range) => ({ start: range.start_ms, end: range.end_ms }));
    if (loopRange && type !== "loop") ranges.push({ start: loopRange.start_ms, end: loopRange.end_ms });
    return ranges.sort((a, b) => a.start - b.start || a.end - b.end);
  }

  function freeGaps(type, key) {
    const gaps = [];
    let cursor = 0;
    for (const range of occupied(type, key)) {
      if (range.start > cursor) gaps.push({ start: cursor, end: range.start });
      cursor = Math.max(cursor, range.end);
    }
    if (cursor < duration) gaps.push({ start: cursor, end: duration });
    return gaps;
  }

  function gapAt(ms, type, key) {
    return freeGaps(type, key).find((gap) => ms >= gap.start && ms <= gap.end) ?? null;
  }

  function nearestGap(ms, type, key) {
    let nearest = null;
    let nearestDistance = Infinity;
    for (const gap of freeGaps(type, key)) {
      if (gap.end - gap.start < 1) continue;
      const distance = ms < gap.start ? gap.start - ms : ms > gap.end ? ms - gap.end : 0;
      if (distance < nearestDistance) {
        nearest = gap;
        nearestDistance = distance;
      }
    }
    return nearest;
  }

  function sensibleSpan(gap) {
    const desired = Math.min(10_000, Math.max(1_000, Math.round(duration / 10)));
    return Math.max(1, Math.min(desired, gap.end - gap.start));
  }

  function emitCommit(before) {
    const after = copyEdit();
    if (editKey(before) !== editKey(after)) oncommit?.(before, after);
  }

  function replaceRegion(type, key, start, end) {
    start = clamp(Math.round(start), 0, duration - 1);
    end = clamp(Math.round(end), start + 1, duration);
    if (type === "loop") {
      if (!loopRange || loopRange._key !== key) return;
      loopRange = { ...loopRange, start_ms: start, end_ms: end };
      return;
    }
    cuts = sortCuts(cuts.map((range) => (
      range._key === key ? { ...range, start_ms: start, end_ms: end } : range
    )));
  }

  function addRegion(type) {
    if (disabled || (type === "loop" && loopRange)) {
      if (loopRange && type === "loop") selected = { type: "loop", key: loopRange._key };
      return;
    }
    const gap = nearestGap(cursorValue, type, null);
    if (!gap) return;
    const before = copyEdit();
    const span = sensibleSpan(gap);
    const anchor = clamp(cursorValue, gap.start, gap.end);
    const start = Math.round(clamp(anchor - span / 2, gap.start, gap.end - span));
    const range = {
      _key: newKey(type),
      start_ms: start,
      end_ms: start + span,
      ...(type === "loop" ? { play_count: 2 } : {}),
    };
    if (type === "loop") loopRange = range;
    else cuts = sortCuts([...cuts, range]);
    selected = { type, key: range._key };
    emitCommit(before);
  }

  function removeRegion(type, key) {
    if (disabled) return;
    const before = copyEdit();
    if (type === "loop") {
      if (!loopRange || loopRange._key !== key) return;
      loopRange = null;
    } else {
      if (!cuts.some((range) => range._key === key)) return;
      cuts = cuts.filter((range) => range._key !== key);
    }
    if (selected?.type === type && selected.key === key) selected = null;
    emitCommit(before);
  }

  function removeSelected() {
    if (!selected) return;
    removeRegion(selected.type, selected.key);
  }

  function setViewport(start, end) {
    const span = clamp(end - start, Math.min(MIN_VIEW_MS, duration), duration);
    let nextStart = start;
    if (nextStart < 0) nextStart = 0;
    if (nextStart + span > duration) nextStart = duration - span;
    viewStart = Math.max(0, nextStart);
    viewEnd = Math.min(duration, viewStart + span);
  }

  function fit() {
    setViewport(0, duration);
  }

  function zoomAt(factor, anchor = (viewStart + viewEnd) / 2) {
    const span = clamp(viewSpan * factor, Math.min(MIN_VIEW_MS, duration), duration);
    const ratio = viewSpan > 0 ? (anchor - viewStart) / viewSpan : 0.5;
    setViewport(anchor - span * ratio, anchor + span * (1 - ratio));
  }

  function panBy(deltaMs) {
    setViewport(viewStart + deltaMs, viewEnd + deltaMs);
  }

  function msFromClientX(clientX, element = mainStage, start = viewStart, end = viewEnd) {
    const rect = element?.getBoundingClientRect();
    if (!rect?.width) return start;
    return start + clamp((clientX - rect.left) / rect.width, 0, 1) * (end - start);
  }

  function snap(ms, type, key, altKey) {
    if (altKey || !mainSize.width) return Math.round(ms);
    const threshold = (viewSpan / mainSize.width) * SNAP_PX;
    let best = ms;
    let distance = threshold + 1;
    const candidates = [0, duration, cursorValue];
    for (const range of occupied(type, key)) candidates.push(range.start, range.end);
    for (const candidate of candidates) {
      const nextDistance = Math.abs(ms - candidate);
      if (nextDistance <= threshold && nextDistance < distance) {
        best = candidate;
        distance = nextDistance;
      }
    }
    return Math.round(best);
  }

  function region(type, key) {
    return type === "loop" ? loopRange : cuts.find((range) => range._key === key);
  }

  function updateMarker(node, ms, visible = true) {
    if (!node || !mainStage) return;
    const value = clamp(Number(ms) || 0, 0, duration);
    const x = clamp((value - viewStart) / viewSpan, 0, 1) * 100;
    node.style.left = `${x}%`;
    node.hidden = !visible;
  }

  function updateCursorDom(ms, visible = true) {
    cursorValue = clamp(Math.round(Number(ms) || 0), 0, duration);
    updateMarker(playheadCursor, cursorValue, visible);
  }
  function updatePreviewDom(ms) {
    const value = Number(ms);
    const visible = ms !== null && ms !== undefined && Number.isFinite(value);
    updateMarker(previewPlayhead, visible ? value : 0, visible);
  }

  function stopPreviewAnimation() {
    if (!previewFrame) return;
    cancelAnimationFrame(previewFrame);
    previewFrame = 0;
  }

  function previewAnimationRunning() {
    return previewActive && previewPlaying && typeof previewPositionReader === "function";
  }

  function applyPreviewFrame() {
    previewFrame = 0;
    if (!previewAnimationRunning() || document.hidden) return;
    updatePreviewDom(previewPositionReader());
    previewFrame = requestAnimationFrame(applyPreviewFrame);
  }

  function startPreviewAnimation() {
    if (!previewAnimationRunning() || document.hidden || previewFrame) return;
    updatePreviewDom(previewPositionReader());
    previewFrame = requestAnimationFrame(applyPreviewFrame);
  }

  function updatePreviewFallback() {
    if (!previewAnimationRunning()) updatePreviewDom(previewPositionMs);
  }

  function applyHover() {
    hoverFrame = 0;
    const point = pendingHover;
    pendingHover = null;
    if (!point || !mainStage || drag) return;
    const value = msFromClientX(point.clientX);
    updateMarker(hoverCursor, value, true);
    if (hoverLabel) {
      const rect = mainStage.getBoundingClientRect();
      const x = clamp(point.clientX - rect.left, 30, rect.width - 30);
      hoverLabel.style.left = `${x}px`;
      hoverLabel.textContent = formatExactTime(value);
      hoverLabel.hidden = false;
    }
  }

  function scheduleHover(event) {
    const points = event.getCoalescedEvents?.() ?? [];
    const point = points.length ? points[points.length - 1] : event;
    pendingHover = { clientX: point.clientX };
    if (!hoverFrame) hoverFrame = requestAnimationFrame(applyHover);
  }

  function hideHover() {
    pendingHover = null;
    if (hoverCursor) hoverCursor.hidden = true;
    if (hoverLabel) hoverLabel.hidden = true;
  }

  function beginPointer(event, mode, type = null, key = null, edge = null) {
    if (event.button !== 0 && event.button !== 1) return;
    const pan = event.button === 1;
    if (disabled) return;
    event.preventDefault();
    const element = mode === "overview" ? overviewStage : mainStage;
    if (!element) return;
    const anchorMs = mode === "overview"
      ? msFromClientX(event.clientX, overviewStage, 0, duration)
      : msFromClientX(event.clientX);
    const snapshot = {
      ...copyEdit(),
      selected: selected ? { ...selected } : null,
      start: viewStart,
      end: viewEnd,
      cursor: cursorValue,
    };
    let kind = pan ? "pan" : mode;
    let gap = null;

    if (mode === "create" && !pan) {
      if (event.shiftKey) {
        gap = gapAt(anchorMs, "cut", null);
        if (!gap || gap.end - gap.start < 1) return;
        kind = "create-intent";
      } else {
        kind = "cursor";
      }
    } else if ((mode === "move" || mode === "resize") && !pan) {
      selected = { type, key };
    }

    if (mode === "overview") {
      const onViewport = event.target?.closest?.(".overview-window");
      if (!onViewport) setViewport(anchorMs - viewSpan / 2, anchorMs + viewSpan / 2);
      kind = "overview";
    }

    drag = {
      pointerId: event.pointerId,
      element,
      kind,
      type,
      key,
      edge,
      anchorX: event.clientX,
      anchorMs,
      original: region(type, key) ? { ...region(type, key) } : null,
      snapshot,
      gap,
      viewStart,
      viewEnd,
      currentMs: anchorMs,
      moved: false,
    };
    if (kind === "cursor") updateCursorDom(anchorMs, true);
    element.setPointerCapture(event.pointerId);
  }

  function applyPendingPointer() {
    pointerFrame = 0;
    const point = pendingPointer;
    pendingPointer = null;
    const active = drag;
    if (!active || !point) return;
    const distance = Math.abs(point.clientX - active.anchorX);
    if (distance > DRAG_THRESHOLD_PX) active.moved = true;

    if (active.kind === "cursor") {
      active.currentMs = msFromClientX(point.clientX);
      updateCursorDom(active.currentMs, true);
      return;
    }
    if (active.kind === "pan") {
      const rect = mainStage?.getBoundingClientRect();
      if (!rect?.width) return;
      const delta = ((active.anchorX - point.clientX) / rect.width) * (active.viewEnd - active.viewStart);
      setViewport(active.viewStart + delta, active.viewEnd + delta);
      return;
    }
    if (active.kind === "overview") {
      const rect = overviewStage?.getBoundingClientRect();
      if (!rect?.width) return;
      const delta = ((point.clientX - active.anchorX) / rect.width) * duration;
      setViewport(active.viewStart + delta, active.viewEnd + delta);
      return;
    }
    if (active.kind === "create-intent") {
      if (!active.moved || !active.gap) return;
      const start = clamp(
        Math.round(snap(active.anchorMs, "cut", null, point.altKey)),
        active.gap.start,
        active.gap.end - 1,
      );
      const regionKey = newKey("cut");
      const created = { _key: regionKey, start_ms: start, end_ms: start + 1 };
      cuts = sortCuts([...cuts, created]);
      selected = { type: "cut", key: regionKey };
      active.kind = "create";
      active.type = "cut";
      active.key = regionKey;
      active.edge = "create";
      active.original = { ...created };
    }

    const current = msFromClientX(point.clientX);
    const original = active.original;
    if (!original) return;
    const gap = active.gap ?? gapAt((original.start_ms + original.end_ms) / 2, active.type, active.key);
    if (!gap) return;

    if (active.kind === "create") {
      let end = snap(current, active.type, active.key, point.altKey);
      end = clamp(end, gap.start, gap.end);
      const anchor = snap(active.anchorMs, active.type, active.key, point.altKey);
      const start = clamp(Math.min(anchor, end), gap.start, gap.end - 1);
      end = clamp(Math.max(anchor, end), start + 1, gap.end);
      replaceRegion(active.type, active.key, start, end);
      return;
    }

    if (active.kind === "resize") {
      if (active.edge === "start") {
        const start = clamp(snap(current, active.type, active.key, point.altKey), gap.start, original.end_ms - 1);
        replaceRegion(active.type, active.key, start, original.end_ms);
      } else {
        const end = clamp(snap(current, active.type, active.key, point.altKey), original.start_ms + 1, gap.end);
        replaceRegion(active.type, active.key, original.start_ms, end);
      }
      return;
    }

    const span = original.end_ms - original.start_ms;
    let start = original.start_ms + (current - active.anchorMs);
    const end = start + span;
    const snappedStart = snap(start, active.type, active.key, point.altKey);
    const snappedEnd = snap(end, active.type, active.key, point.altKey);
    if (Math.abs(snappedStart - start) <= Math.abs(snappedEnd - end)) start = snappedStart;
    else start += snappedEnd - end;
    start = clamp(start, gap.start, gap.end - span);
    replaceRegion(active.type, active.key, start, start + span);
  }

  function onPointerMove(event) {
    if (!drag || event.pointerId !== drag.pointerId) {
      if (!drag) scheduleHover(event);
      return;
    }
    const points = event.getCoalescedEvents?.() ?? [];
    const point = points.length ? points[points.length - 1] : event;
    pendingPointer = { clientX: point.clientX, altKey: event.altKey };
    if (!pointerFrame) pointerFrame = requestAnimationFrame(applyPendingPointer);
  }

  function finishPointer(event, cancel = false) {
    if (!drag || (event && event.pointerId !== drag.pointerId)) return;
    if (!cancel && event) {
      pendingPointer = { clientX: event.clientX, altKey: event.altKey };
      if (pointerFrame) cancelAnimationFrame(pointerFrame);
      applyPendingPointer();
    }
    const active = drag;
    drag = null;
    pendingPointer = null;
    if (pointerFrame) cancelAnimationFrame(pointerFrame);
    pointerFrame = 0;
    if (cancel) {
      cuts = active.snapshot.cuts;
      loopRange = active.snapshot.loopRange;
      selected = active.snapshot.selected;
      viewStart = active.snapshot.start;
      viewEnd = active.snapshot.end;
      cursorMs = active.snapshot.cursor;
      updateCursorDom(active.snapshot.cursor, true);
    } else if (active.kind === "cursor") {
      const nextCursor = clamp(Math.round(active.currentMs ?? active.anchorMs), 0, duration);
      cursorMs = nextCursor;
      updateCursorDom(nextCursor, true);
      if (previewActive) onseek?.(nextCursor);
    } else if (active.kind === "create" || active.kind === "move" || active.kind === "resize") {
      emitCommit(active.snapshot);
    }
    if (active.element?.hasPointerCapture?.(active.pointerId)) active.element.releasePointerCapture(active.pointerId);
    if (!cancel && event) scheduleHover(event);
  }

  function removeOnContext(event, type, key) {
    event.preventDefault();
    event.stopPropagation();
    removeRegion(type, key);
  }

  function regionStyle(range) {
    const left = ((range.start_ms - viewStart) / viewSpan) * 100;
    const right = ((range.end_ms - viewStart) / viewSpan) * 100;
    return `left:${left}%;width:${Math.max(0, right - left)}%`;
  }

  function overviewWindowStyle() {
    return `left:${(viewStart / duration) * 100}%;width:${(viewSpan / duration) * 100}%`;
  }

  function nudgeHandle(event, type, key, edge) {
    const rangeValue = region(type, key);
    if (!rangeValue || disabled) return;
    const direction = event.key === "ArrowLeft" || event.key === "ArrowDown" ? -1
      : event.key === "ArrowRight" || event.key === "ArrowUp" ? 1 : 0;
    let next = null;
    if (direction) {
      const amount = event.ctrlKey || event.metaKey ? 100 : event.shiftKey ? 10 : 1;
      next = rangeValue[edge === "start" ? "start_ms" : "end_ms"] + direction * amount;
    } else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = duration;
    if (next === null) return;
    event.preventDefault();
    const before = copyEdit();
    selected = { type, key };
    const gap = gapAt((rangeValue.start_ms + rangeValue.end_ms) / 2, type, key);
    if (!gap) return;
    if (edge === "start") replaceRegion(type, key, clamp(next, gap.start, rangeValue.end_ms - 1), rangeValue.end_ms);
    else replaceRegion(type, key, rangeValue.start_ms, clamp(next, rangeValue.start_ms + 1, gap.end));
    emitCommit(before);
  }

  function onWheel(event) {
    if (event.ctrlKey) {
      event.preventDefault();
      zoomAt(Math.exp(event.deltaY * 0.002), msFromClientX(event.clientX));
    } else if (event.shiftKey) {
      event.preventDefault();
      panBy(((event.deltaX || event.deltaY) / Math.max(1, mainSize.width)) * viewSpan);
    }
  }

  function isTyping(target) {
    return target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" ||
      target.tagName === "SELECT" || target.isContentEditable);
  }

  function onGlobalKey(event) {
    const command = event.ctrlKey || event.metaKey;
    if (command && event.key.toLowerCase() === "s") {
      if (!isTyping(event.target)) {
        event.preventDefault();
        onsave?.();
      }
      return;
    }
    if (isTyping(event.target)) return;
    if (command && event.key.toLowerCase() === "z") {
      event.preventDefault();
      if (event.shiftKey) onredo?.();
      else onundo?.();
      return;
    }
    if (command && event.key.toLowerCase() === "y") {
      event.preventDefault();
      onredo?.();
      return;
    }
    if (event.key === "Escape") {
      if (drag) finishPointer(null, true);
      else selected = null;
      return;
    }
    if (disabled) return;
    if (event.key === "+" || event.key === "=") {
      event.preventDefault(); zoomAt(0.5);
    } else if (event.key === "-" || event.key === "_") {
      event.preventDefault(); zoomAt(2);
    } else if (event.key === "0") {
      event.preventDefault(); fit();
    } else if (event.key.toLowerCase() === "c") {
      event.preventDefault(); addRegion("cut");
    } else if (event.key.toLowerCase() === "l") {
      event.preventDefault(); addRegion("loop");
    } else if ((event.key === "Delete" || event.key === "Backspace") && selected) {
      event.preventDefault(); removeSelected();
    }
  }

  function resizeCanvas(canvas, size) {
    if (!canvas || !size.width || !size.height) return null;
    const dpr = window.devicePixelRatio || 1;
    const width = Math.max(1, Math.round(size.width * dpr));
    const height = Math.max(1, Math.round(size.height * dpr));
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    const context = canvas.getContext("2d");
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    context.clearRect(0, 0, size.width, size.height);
    return context;
  }

  function drawWaveform(canvas, size, start, end, quiet = false) {
    const context = resizeCanvas(canvas, size);
    if (!context) return;
    const styles = getComputedStyle(canvas);
    const center = size.height / 2;
    const index = waveIndex;
    context.lineWidth = 1;
    context.strokeStyle = styles.getPropertyValue(quiet ? "--wave-quiet" : "--wave-main").trim() || "#9ccfd8";
    context.beginPath();
    if (!index) {
      context.globalAlpha = 0.35;
      context.moveTo(0, Math.round(center) + 0.5);
      context.lineTo(size.width, Math.round(center) + 0.5);
    } else {
      context.globalAlpha = quiet ? 0.62 : 0.9;
      traceWaveform(context, index, size.width, center, start, end);
    }
    context.stroke();
    context.globalAlpha = 1;
  }

  function drawMain() {
    mainDrawFrame = 0;
    drawWaveform(mainCanvas, mainSize, viewStart, viewEnd);
  }

  function drawOverview() {
    overviewDrawFrame = 0;
    drawWaveform(overviewCanvas, overviewSize, 0, duration, true);
  }

  function scheduleMainDraw() {
    if (!mainDrawFrame) mainDrawFrame = requestAnimationFrame(drawMain);
  }

  function scheduleOverviewDraw() {
    if (!overviewDrawFrame) overviewDrawFrame = requestAnimationFrame(drawOverview);
  }

  function scheduleDraw() {
    scheduleMainDraw();
    scheduleOverviewDraw();
  }

  function cancelDraws() {
    if (mainDrawFrame) cancelAnimationFrame(mainDrawFrame);
    if (overviewDrawFrame) cancelAnimationFrame(overviewDrawFrame);
    mainDrawFrame = 0;
    overviewDrawFrame = 0;
  }

  $effect(() => {
    const nextDuration = duration;
    if (viewportDuration === nextDuration) return;
    const previousDuration = viewportDuration;
    viewportDuration = nextDuration;
    if (!previousDuration) setViewport(0, nextDuration);
    else setViewport(viewStart, viewStart + Math.min(viewSpan, nextDuration));
  });

  function drawWaveformsForData() {
    waveIndex;
    duration;
    scheduleDraw();
  }

  function drawMainForViewport() {
    viewStart;
    viewEnd;
    scheduleMainDraw();
  }

  function updateMarkers() {
    cursorMs;
    updateCursorDom(cursorMs, true);
    updatePreviewFallback();
  }

  $effect(() => drawWaveformsForData());
  $effect(() => drawMainForViewport());
  $effect(() => updateMarkers());

  $effect(() => {
    const running = previewAnimationRunning();
    const onVisibilityChange = () => {
      if (document.hidden) stopPreviewAnimation();
      else if (running) startPreviewAnimation();
      else updatePreviewDom(previewPositionMs);
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    if (running && !document.hidden) startPreviewAnimation();
    else {
      stopPreviewAnimation();
      updatePreviewDom(previewPositionMs);
    }
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
      stopPreviewAnimation();
    };
  });

  $effect(() => {
    const main = mainCanvas;
    const overview = overviewCanvas;
    if (!main || !overview) return;
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const size = { width: Math.round(entry.contentRect.width), height: Math.round(entry.contentRect.height) };
        if (entry.target === main) {
          mainSize = size;
          scheduleMainDraw();
        } else {
          overviewSize = size;
          scheduleOverviewDraw();
        }
      }
    });
    observer.observe(main);
    observer.observe(overview);
    const mainRect = main.getBoundingClientRect();
    const overviewRect = overview.getBoundingClientRect();
    mainSize = { width: Math.round(mainRect.width), height: Math.round(mainRect.height) };
    overviewSize = { width: Math.round(overviewRect.width), height: Math.round(overviewRect.height) };
    scheduleDraw();
    window.addEventListener("keydown", onGlobalKey);
    return () => {
      observer.disconnect();
      window.removeEventListener("keydown", onGlobalKey);
      cancelDraws();
      if (pointerFrame) cancelAnimationFrame(pointerFrame);
      if (hoverFrame) cancelAnimationFrame(hoverFrame);
      stopPreviewAnimation();
    };
  });


</script>

<div class="waveform-editor" class:dragging={!!drag}>
  <div class="wave-tools" role="toolbar" aria-label="Waveform edit tools">
    <div class="tool-group regions">
      <button class="tool-button cut-tool" disabled={disabled} onclick={() => addRegion("cut")} title="Add cut (C)">Add cut</button>
      <button class="tool-button loop-tool" disabled={disabled} onclick={() => addRegion("loop")} title="Add loop at cursor (L)">{loopRange ? "Select loop" : "Add loop at cursor"}</button>
    </div>
    <output class="selection-readout tnum" aria-live="polite">{selectedReadout} <span>·</span> {cursorReadout}</output>
    <div class="tool-group zoom" aria-label="Zoom controls">
      <button class="zoom-button" onclick={() => zoomAt(2)} title="Zoom out (−)" aria-label="Zoom out">−</button>
      <span class="zoom-level tnum">{zoomLabel}</span>
      <button class="zoom-button" onclick={() => zoomAt(0.5)} title="Zoom in (+)" aria-label="Zoom in">+</button>
      <button class="fit-button" onclick={fit} title="Fit whole track (0)">Fit</button>
    </div>
  </div>

  <div class="main-stage" class:panning={drag?.kind === "pan"} bind:this={mainStage}
    role="region" title="Hold Alt while dragging to position freely." aria-label="Track waveform. Shift-drag empty space to add a cut. Hold Alt while dragging to position freely."
    onwheel={onWheel} onpointerdown={(event) => beginPointer(event, "create")}
    onpointermove={onPointerMove} onpointerleave={() => { if (!drag) hideHover(); }}
    onpointerup={(event) => finishPointer(event)} onpointercancel={(event) => finishPointer(event, true)}
    oncontextmenu={(event) => event.preventDefault()}>
    <canvas bind:this={mainCanvas} aria-hidden="true"></canvas>
    <span class="playhead-cursor" bind:this={playheadCursor} hidden aria-hidden="true"></span>
    <span class="preview-playhead" bind:this={previewPlayhead} hidden aria-hidden="true"></span>
    <span class="hover-cursor" bind:this={hoverCursor} hidden aria-hidden="true"></span>
    <span class="hover-time tnum" bind:this={hoverLabel} hidden aria-hidden="true"></span>
    {#if waveformState === "loading"}<span class="wave-status">Reading waveform…</span>
    {:else if waveformState === "error"}<span class="wave-status error" title={waveformError}>Waveform unavailable · the time fields still work</span>{/if}
    <div class="region-layer">
      {#each cuts as cut (cut._key)}
        <div class="edit-region cut-region" class:selected={selected?.type === "cut" && selected.key === cut._key} style={regionStyle(cut)}
          role="group" aria-label={`Cut from ${formatExactTime(cut.start_ms)} to ${formatExactTime(cut.end_ms)}`} oncontextmenu={(event) => removeOnContext(event, "cut", cut._key)}>
          <button type="button" class="region-body" aria-label={`Move cut from ${formatExactTime(cut.start_ms)} to ${formatExactTime(cut.end_ms)}`} onpointerdown={(event) => { event.stopPropagation(); beginPointer(event, "move", "cut", cut._key); }}></button>
          <button type="button" class="region-handle start" role="slider" aria-label="Cut start" aria-valuemin="0" aria-valuemax={cut.end_ms - 1} aria-valuenow={cut.start_ms} aria-valuetext={formatExactTime(cut.start_ms)} onkeydown={(event) => nudgeHandle(event, "cut", cut._key, "start")} onpointerdown={(event) => { event.stopPropagation(); beginPointer(event, "resize", "cut", cut._key, "start"); }}></button>
          <button type="button" class="region-handle end" role="slider" aria-label="Cut end" aria-valuemin={cut.start_ms + 1} aria-valuemax={duration} aria-valuenow={cut.end_ms} aria-valuetext={formatExactTime(cut.end_ms)} onkeydown={(event) => nudgeHandle(event, "cut", cut._key, "end")} onpointerdown={(event) => { event.stopPropagation(); beginPointer(event, "resize", "cut", cut._key, "end"); }}></button>
        </div>
      {/each}
      {#if loopRange}
        <div class="edit-region loop-region" class:selected={selected?.type === "loop"} style={regionStyle(loopRange)}
          role="group" aria-label={`Loop from ${formatExactTime(loopRange.start_ms)} to ${formatExactTime(loopRange.end_ms)}`} oncontextmenu={(event) => removeOnContext(event, "loop", loopRange._key)}>
          <button type="button" class="region-body" aria-label={`Move loop from ${formatExactTime(loopRange.start_ms)} to ${formatExactTime(loopRange.end_ms)}`} onpointerdown={(event) => { event.stopPropagation(); beginPointer(event, "move", "loop", loopRange._key); }}></button>
          <button type="button" class="region-handle start" role="slider" aria-label="Loop start" aria-valuemin="0" aria-valuemax={loopRange.end_ms - 1} aria-valuenow={loopRange.start_ms} aria-valuetext={formatExactTime(loopRange.start_ms)} onkeydown={(event) => nudgeHandle(event, "loop", loopRange._key, "start")} onpointerdown={(event) => { event.stopPropagation(); beginPointer(event, "resize", "loop", loopRange._key, "start"); }}></button>
          <button type="button" class="region-handle end" role="slider" aria-label="Loop end" aria-valuemin={loopRange.start_ms + 1} aria-valuemax={duration} aria-valuenow={loopRange.end_ms} aria-valuetext={formatExactTime(loopRange.end_ms)} onkeydown={(event) => nudgeHandle(event, "loop", loopRange._key, "end")} onpointerdown={(event) => { event.stopPropagation(); beginPointer(event, "resize", "loop", loopRange._key, "end"); }}></button>
        </div>
      {/if}
    </div>
    <span class="time-mark start tnum">{formatExactTime(viewStart)}</span>
    <span class="time-mark end tnum">{formatExactTime(viewEnd)}</span>
  </div>

  <div class="overview-stage" bind:this={overviewStage} role="region" aria-label="Track overview. Click to recenter or drag the viewport."
    onpointerdown={(event) => beginPointer(event, "overview")} onpointermove={onPointerMove}
    onpointerup={(event) => finishPointer(event)} onpointercancel={(event) => finishPointer(event, true)}>
    <canvas bind:this={overviewCanvas} aria-hidden="true"></canvas>
    <span class="overview-window" style={overviewWindowStyle()} aria-hidden="true"></span>
  </div>
  <p class="wave-hint">Shift-drag empty space creates a cut · middle-drag pans · Shift + wheel pans · Ctrl + wheel zooms · Hold Alt while dragging to position freely.</p>
</div>

<style>
  .waveform-editor { --wave-main: var(--foam); --wave-quiet: var(--fg-2); min-width: 0; }
  .wave-tools {
    display: grid; grid-template-columns: auto minmax(180px, 1fr) auto;
    align-items: center; gap: var(--s3); min-height: 42px; margin-bottom: 8px;
  }
  .tool-group { display: flex; align-items: center; gap: 6px; }
  .tool-button, .fit-button, .zoom-button {
    display: inline-flex; align-items: center; justify-content: center;
    height: 34px; border: 1px solid var(--line-2); color: var(--fg-1);
    background: var(--raise-1); font-family: var(--font-small);
  }
  .tool-button { min-width: 92px; padding: 0 var(--s3); border-radius: var(--r2); font-size: var(--t-12); }
  .tool-button:hover:not(:disabled), .fit-button:hover, .zoom-button:hover {
    color: var(--fg); border-color: color-mix(in srgb, var(--fg) 22%, transparent); background: var(--raise-2);
  }
  .cut-tool { border-color: color-mix(in srgb, var(--rose-ink) 30%, var(--line-2)); }
  .loop-tool { border-color: color-mix(in srgb, var(--gold) 30%, var(--line-2)); }
  .cut-tool::before, .loop-tool::before {
    content: ""; display: inline-block; width: 7px; height: 7px; margin-right: 7px;
    border-radius: 1px; background: var(--rose-ink);
  }
  .loop-tool::before { background: var(--gold); }
  .selection-readout {
    display: inline-flex; align-items: center; justify-content: center; min-width: 0;
    height: 30px; padding: 0 var(--s3); overflow: hidden; border: 1px solid var(--line);
    border-radius: var(--rf); background: rgb(0 0 0 / 0.22);
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-12);
    text-align: center; text-overflow: ellipsis; white-space: nowrap;
  }
  .selection-readout span { margin-inline: 6px; color: var(--fg-3); }
  .zoom { justify-content: flex-end; }
  .zoom-button { width: 34px; border-radius: var(--r2); font-size: 18px; line-height: 1; }
  .fit-button { min-width: 50px; padding: 0 10px; border-radius: var(--r2); font-size: var(--t-12); }
  .zoom-level { width: 38px; color: var(--fg-2); font: var(--t-11) var(--font-mono); text-align: center; }
  .main-stage, .overview-stage {
    /* A well pressed into the glass, like every field on it. */
    position: relative; overflow: hidden; border: 1px solid rgba(255, 255, 255, 0.08);
    background: rgb(0 0 0 / 0.22); user-select: none; touch-action: pan-y;
  }
  .main-stage { height: 238px; border-radius: var(--r2) var(--r2) 0 0; cursor: crosshair; }
  .main-stage::after {
    content: ""; position: absolute; z-index: 1; top: 50%; right: 0; left: 0;
    height: 1px; background: var(--line); pointer-events: none;
  }
  .main-stage.panning, .dragging .overview-window { cursor: grabbing; }
  canvas { display: block; width: 100%; height: 100%; }
  .wave-status {
    position: absolute; z-index: 8; top: var(--s3); left: 50%; max-width: calc(100% - 32px);
    padding: 4px 9px; transform: translateX(-50%); border: 1px solid var(--line);
    border-radius: var(--rf); background: color-mix(in srgb, var(--bg-1) 88%, transparent);
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-11);
    pointer-events: none; white-space: nowrap;
  }
  .wave-status.error { color: var(--rose); }
  .playhead-cursor, .preview-playhead, .hover-cursor { position: absolute; top: 0; bottom: 0; width: 2px; pointer-events: none; }
  .playhead-cursor { z-index: 5; width: 3px; background: var(--bg-0); box-shadow: 0 0 0 1px var(--accent), 0 0 9px color-mix(in srgb, var(--accent) 72%, transparent); }
  .playhead-cursor::before {
    content: ""; position: absolute; top: 0; left: 50%; width: 9px; height: 9px;
    transform: translate(-50%, -1px) rotate(45deg); border: 2px solid var(--bg-0);
    border-radius: 2px; background: var(--accent);
  }
  .preview-playhead {
    z-index: 4; width: 0; border-left: 2px dashed var(--gold); opacity: .95;
    filter: drop-shadow(0 0 3px color-mix(in srgb, var(--bg-0) 88%, transparent));
    will-change: left;
  }
  .preview-playhead::after {
    content: ""; position: absolute; bottom: 0; left: 50%; width: 8px; height: 8px;
    transform: translate(-50%, 1px); border: 2px solid var(--gold);
    border-radius: 50%; background: var(--bg-1);
  }
  .hover-cursor { z-index: 6; width: 1px; background: color-mix(in srgb, var(--fg) 76%, transparent); }
  .hover-time {
    position: absolute; z-index: 7; top: 8px; transform: translateX(-50%); padding: 3px 6px;
    border: 1px solid var(--line-2); border-radius: var(--rf);
    background: color-mix(in srgb, var(--bg-1) 92%, transparent); color: var(--fg);
    font-size: 10px; pointer-events: none; white-space: nowrap;
  }
  .region-layer { position: absolute; z-index: 2; inset: 0; overflow: hidden; pointer-events: none; }
  .edit-region {
    position: absolute; top: 0; bottom: 0; min-width: 1px; border-inline: 1px solid currentColor;
    color: var(--rose-ink); background: color-mix(in srgb, var(--rose-ink) 16%, transparent); pointer-events: auto;
  }
  .edit-region.loop-region { color: var(--gold); background: color-mix(in srgb, var(--gold) 12%, transparent); }
  .edit-region.selected { box-shadow: inset 0 0 0 1px currentColor; }
  .region-body { position: absolute; inset: 0; width: 100%; cursor: grab; }
  .region-body:active { cursor: grabbing; }
  .region-handle { position: absolute; z-index: 3; top: 50%; width: 44px; height: 44px; transform: translateY(-50%); cursor: ew-resize; }
  .region-handle.start { left: -22px; }.region-handle.end { right: -22px; }
  .region-handle::after {
    content: ""; position: absolute; top: 8px; bottom: 8px; left: 20px; width: 3px;
    border-radius: var(--rf); background: currentColor; box-shadow: 0 0 0 1px color-mix(in srgb, var(--bg-0) 76%, transparent);
  }
  .time-mark { position: absolute; z-index: 4; bottom: 7px; color: var(--fg-2); font-family: var(--font-mono); font-size: 10px; pointer-events: none; }
  .time-mark.start { left: 9px; }.time-mark.end { right: 9px; }
  .overview-stage { height: 46px; margin-top: 4px; border-top: 0; border-radius: 0 0 var(--r2) var(--r2); cursor: crosshair; }
  .overview-window {
    position: absolute; top: 0; bottom: 0; min-width: 4px; border: 1px solid var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--bg-0) 50%, transparent); cursor: grab;
  }
  .wave-hint { margin: 8px 2px 0; color: var(--fg-3); font-family: var(--font-small); font-size: var(--t-11); text-align: left; }
  @media (max-width: 760px) {
    .wave-tools { grid-template-columns: minmax(0, 1fr) auto; }
    .selection-readout { grid-column: 1 / -1; grid-row: 2; justify-content: flex-start; padding-inline: 0; border: 0; background: transparent; text-align: left; }
    .main-stage { height: 212px; }.wave-hint { text-align: left; }
  }
  @media (max-width: 500px) {
    .wave-tools { grid-template-columns: 1fr; }.zoom { justify-content: flex-start; }
    .selection-readout { grid-column: 1; grid-row: auto; }.main-stage { height: 190px; }
  }
</style>
