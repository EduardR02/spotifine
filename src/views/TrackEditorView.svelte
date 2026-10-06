<script>
  import { untrack } from "svelte";
  import { api, route, playback, positionMs as projectedPlaybackPositionMs, getTrackEditorTrack, goBack, setNavigationGuard, allocatePreviewLeaseId } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import TrackEditWaveform from "../components/TrackEditWaveform.svelte";
  import { formatExactTime, formatTime, parseExactTime } from "../lib/time.js";

  const playlistId = $derived(route.param || null);
  const track = $derived(getTrackEditorTrack(route.id, playlistId));
  const duration = $derived(Math.max(1, Math.round(Number(track?.duration_ms) || 1)));
  const ready = $derived(!!track && !loading && !loadError);
  let loading = $state(true), saving = $state(false), loadError = $state(""), actionError = $state("");
  let cuts = $state([]), loopRange = $state(null), selected = $state(null);
  let cursorMs = $state(0), previewState = $state("idle"), previewSignature = $state("");
  let previewBusy = $state(false);
  let undoStack = $state([]), redoStack = $state([]);
  let enabled = $state(false), definitionExists = $state(false), baseline = $state("");
  let retryGeneration = $state(0), waveform = $state(null), waveformState = $state("idle"), waveformError = $state("");
  let definitionGeneration = 0, waveformGeneration = 0, previewGeneration = 0, keySequence = 0;
  let previewRequest = $state(null), previewQueueAwaiting = $state(null), previewQueueIdentity = $state("");
  let previewCutsSource = null, previewCutsSignature = "", previewCuts = [];
  // The backend invocation outlives the optimistic editor generation: route
  // teardown must be able to wait for a command that is no longer allowed to
  // paint this component.
  let previewInvocation = null;
  let previewContext = null;

  function keyed(range, prefix) {
    return {
      _key: range?._key || `${prefix}-${++keySequence}`,
      start_ms: Math.round(Number(range?.start_ms) || 0),
      end_ms: Math.round(Number(range?.end_ms) || 0),
    };
  }
  function keyedLoop(range) {
    const playCount = range?.play_count;
    return {
      ...keyed(range, "loop"),
      play_count: playCount === undefined ? undefined : Number(playCount),
    };
  }
  function hydrateCuts(value) {
    return (value ?? []).map((range) => keyed(range, "cut")).sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms);
  }
  function hydrateLoop(value) {
    return value == null ? null : keyedLoop(value);
  }
  function canonicalCuts(value = cuts) {
    return value.map(({ start_ms, end_ms }) => ({ start_ms: Math.round(start_ms), end_ms: Math.round(end_ms) })).sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms);
  }
  function canonicalLoop(value = loopRange) {
    if (!value) return null;
    return {
      start_ms: Math.round(value.start_ms),
      end_ms: Math.round(value.end_ms),
      play_count: value.play_count === undefined ? undefined : Number(value.play_count),
    };
  }
  function snapshot(editCuts = cuts, editLoop = loopRange) {
    return JSON.stringify({ cuts: canonicalCuts(editCuts), loop_range: canonicalLoop(editLoop) });
  }
  function editSignature(edit) {
    return snapshot(edit?.cuts ?? [], edit?.loopRange ?? edit?.loop_range ?? null);
  }
  function copyEdit(editCuts = cuts, editLoop = loopRange) {
    return {
      cuts: editCuts.map((range) => ({ ...range })),
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
  function commitHistory(before, after = copyEdit()) {
    if (editKey(before) === editKey(after)) return;
    undoStack = [...undoStack, copyEdit(before.cuts, before.loopRange)];
    redoStack = [];
    markPreviewStale();
  }
  function applyEdit(edit) {
    cuts = (edit?.cuts ?? []).map((range) => ({ ...range }));
    loopRange = edit?.loopRange ? { ...edit.loopRange } : null;
    if (selected?.type === "loop" && !loopRange) selected = null;
    if (selected?.type === "cut" && !cuts.some((range) => range._key === selected.key)) selected = null;
  }
  function undo() {
    if (!undoStack.length || !ready || saving) return;
    const current = copyEdit();
    const target = undoStack[undoStack.length - 1];
    undoStack = undoStack.slice(0, -1);
    redoStack = [...redoStack, current];
    applyEdit(target);
    markPreviewStale();
  }
  function redo() {
    if (!redoStack.length || !ready || saving) return;
    const current = copyEdit();
    const target = redoStack[redoStack.length - 1];
    redoStack = redoStack.slice(0, -1);
    undoStack = [...undoStack, current];
    applyEdit(target);
    markPreviewStale();
  }
  function clearHistory() {
    undoStack = [];
    redoStack = [];
  }
  const dirty = $derived(ready && snapshot() !== baseline);
  const validation = $derived.by(() => {
    const cutErrors = cuts.map(() => "");
    let loopError = "";
    cuts.forEach((range, index) => {
      if (range.start_ms < 0 || range.start_ms >= range.end_ms) cutErrors[index] = "The start must be before the end.";
      else if (range.end_ms > duration) cutErrors[index] = "This cut extends past the track.";
      else if (index && range.start_ms < cuts[index - 1].end_ms) cutErrors[index] = "Cuts must be ordered and cannot overlap.";
    });
    if (loopRange) {
      if (!Number.isInteger(loopRange.play_count) || loopRange.play_count < 2 || loopRange.play_count > 32) {
        loopError = "Plays must be a whole number from 2 to 32 (total passes).";
      } else if (loopRange.start_ms < 0 || loopRange.start_ms >= loopRange.end_ms) {
        loopError = "The loop start must be before the end.";
      } else if (loopRange.end_ms > duration) {
        loopError = "The loop extends past the track.";
      } else {
        const overlap = cuts.findIndex((cut) => cut.start_ms < loopRange.end_ms && loopRange.start_ms < cut.end_ms);
        if (overlap >= 0) {
          loopError = `The loop overlaps cut ${overlap + 1}.`;
          cutErrors[overlap] ||= "This cut overlaps the loop.";
        }
      }
    }
    return { cutErrors, loopError, firstError: cutErrors.find(Boolean) || loopError };
  });
  function active(id, sourcePlaylist, generation = definitionGeneration) {
    return generation === definitionGeneration && route.name === "track-editor" && route.id === id && (route.param || null) === sourcePlaylist;
  }
  function currentRange(type, key) {
    return type === "loop" ? loopRange : cuts.find((range) => range._key === key);
  }
  function setRange(type, key, field, value) {
    if (!ready || saving) return;
    const current = currentRange(type, key);
    if (!current) return;
    const before = copyEdit();
    let next = Math.max(0, Math.min(duration, Math.round(Number(value) || 0)));
    next = field === "start_ms" ? Math.min(next, current.end_ms - 1) : Math.max(next, current.start_ms + 1);
    if (type === "loop") loopRange = { ...current, [field]: next };
    else cuts = cuts.map((range) => range._key === key ? { ...range, [field]: next } : range).sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms);
    selected = { type, key };
    actionError = "";
    commitHistory(before);
  }

  function setPlayCount(key, value) {
    if (!ready || saving || !loopRange || loopRange._key !== key) return;
    const before = copyEdit();
    const numeric = Number(value);
    loopRange = {
      ...loopRange,
      play_count: Number.isFinite(numeric) ? numeric : 0,
    };
    selected = { type: "loop", key };
    actionError = "";
    commitHistory(before);
  }
  function commitTime(event, type, key, field) {
    const input = event.currentTarget, value = parseExactTime(input.value), current = currentRange(type, key);
    if (value === null || !current) {
      input.setCustomValidity("Use seconds, m:ss.mmm, or h:mm:ss.mmm.");
      input.reportValidity();
      if (current) input.value = formatExactTime(current[field]);
      return;
    }
    input.setCustomValidity("");
    setRange(type, key, field, value);
    input.value = formatExactTime(currentRange(type, key)[field]);
  }
  function nudgeTime(event, type, key, field) {
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    const current = currentRange(type, key);
    if (!current) return;
    event.preventDefault();
    const amount = event.ctrlKey || event.metaKey ? 100 : event.shiftKey ? 10 : 1;
    setRange(type, key, field, current[field] + (event.key === "ArrowUp" ? amount : -amount));
    event.currentTarget.value = formatExactTime(currentRange(type, key)[field]);
  }
  function removeCut(key) {
    if (!ready || saving) return;
    const current = cuts.find((range) => range._key === key);
    if (!current) return;
    const before = copyEdit();
    cuts = cuts.filter((range) => range._key !== key);
    if (selected?.type === "cut" && selected.key === key) selected = null;
    commitHistory(before);
  }
  function removeLoop() {
    if (!ready || saving || !loopRange) return;
    const before = copyEdit();
    loopRange = null;
    if (selected?.type === "loop") selected = null;
    commitHistory(before);
  }
  async function save() {
    if (!dirty || !ready || !track || saving || validation.firstError || (!cuts.length && !loopRange)) return;
    const id = track.id, sourcePlaylist = playlistId, generation = definitionGeneration;
    const nextCuts = canonicalCuts(), nextLoop = canonicalLoop();
    saving = true;
    actionError = "";
    try {
      const definition = await api.saveTrackEdit(id, duration, nextCuts, nextLoop);
      if (!active(id, sourcePlaylist, generation)) return;
      loopRange = hydrateLoop(definition?.loop_range);
      baseline = snapshot();
      definitionExists = true;
      selected = null;
      markPreviewStale();
    } catch (reason) {
      if (active(id, sourcePlaylist, generation)) actionError = String(reason || "Could not save this edit.");
    } finally {
      if (active(id, sourcePlaylist, generation)) saving = false;
    }
  }
  async function removeDefinition() {
    if (!ready || !track || saving || !definitionExists) return;
    const id = track.id, sourcePlaylist = playlistId, generation = definitionGeneration;
    saving = true;
    actionError = "";
    try {
      await api.deleteTrackEdit(id);
      if (!active(id, sourcePlaylist, generation)) return;
      cuts = [];
      loopRange = null;
      selected = null;
      baseline = snapshot([], null);
      definitionExists = false;
      enabled = false;
      markPreviewStale();
    } catch (reason) {
      if (active(id, sourcePlaylist, generation)) actionError = String(reason || "Could not remove this edit.");
    } finally {
      if (active(id, sourcePlaylist, generation)) saving = false;
    }
  }
  async function setEnabled(value) {
    if (!ready || !playlistId || !track || saving) return;
    const id = track.id, sourcePlaylist = playlistId, generation = definitionGeneration;
    saving = true;
    actionError = "";
    try {
      await api.setPlaylistTrackEditEnabled(sourcePlaylist, id, value);
      if (active(id, sourcePlaylist, generation)) enabled = value;
    } catch (reason) {
      if (active(id, sourcePlaylist, generation)) actionError = String(reason || "Could not update this playlist.");
    } finally {
      if (active(id, sourcePlaylist, generation)) saving = false;
    }
  }
  const currentQueueTrack = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const editorTrackCurrent = $derived(
    !!track && !!currentQueueTrack &&
      (currentQueueTrack.id === track.id || currentQueueTrack.uri === track.uri),
  );
  const queueEditSignature = $derived(
    currentQueueTrack?.effective_edit ? editSignature(currentQueueTrack.effective_edit) : "",
  );
  const queueIdentity = $derived([
    playback.current_index,
    playback.queue.length,
    currentQueueTrack?.id || "",
    currentQueueTrack?.uri || "",
    currentQueueTrack?.context || "",
    queueEditSignature,
  ].join("|"));
  const draftSignature = $derived(snapshot());
  const previewQueueEditMatches = $derived(
    editorTrackCurrent &&
      !!previewSignature &&
      !!currentQueueTrack?.effective_edit &&
      queueEditSignature === previewSignature,
  );
  const previewQueueMatches = $derived(
    previewQueueEditMatches &&
      (!previewQueueIdentity || queueIdentity === previewQueueIdentity),
  );
  const previewQueueActive = $derived(
    previewQueueMatches &&
      !previewBusy &&
      (previewState === "playing" || previewState === "paused" || previewState === "stale"),
  );
  const previewNeedsUpdate = $derived(!!previewSignature && draftSignature !== previewSignature);
  const previewActionLabel = $derived(
    previewBusy ? "Preparing preview…" : previewNeedsUpdate ? "Update preview" : "Preview draft",
  );
  const previewStatus = $derived.by(() => {
    if (previewBusy) {
      return { label: "Preparing…", copy: "Applying your cuts and loop.", active: false };
    }
    if (previewState === "playing") {
      return { label: "Playing", copy: "You are hearing the edited version.", active: previewIsCurrent };
    }
    if (previewState === "paused") {
      return { label: "Paused", copy: "Resume or update the preview.", active: previewIsCurrent };
    }
    if (previewState === "stale") {
      return { label: "Draft changed", copy: "Update preview to hear the latest edit.", active: false };
    }
    if (previewState === "error") {
      return { label: "Preview failed", copy: "Try again. Your edit is unchanged.", active: false };
    }
    return { label: "Ready", copy: "Play the preview to hear your edit.", active: false };
  });

  function markPreviewStale() {
    if (!previewSignature || draftSignature === previewSignature) return;
    if (previewState !== "idle" && previewState !== "stale") previewState = "stale";
  }

  function resetPreviewLifecycle() {
    previewGeneration += 1;
    previewRequest = null;
    previewQueueAwaiting = null;
    previewQueueIdentity = "";
    previewBusy = false;
    previewState = "idle";
    previewSignature = "";
  }
  function reportPreviewRestoreError(context, reason) {
    if (previewContext !== context || route.name !== "track-editor" ||
        route.id !== context.id || (route.param || null) !== context.sourcePlaylist) return;
    actionError = String(reason || "Could not restore playback after preview.");
  }

  function maybeRestorePreview(context) {
    if (!context || context.restoreRequested || !context.attempted) return;
    context.restoreRequested = true;
    let restore;
    try {
      restore = api.restorePreview(context.previewLeaseId);
    } catch (reason) {
      reportPreviewRestoreError(context, reason);
      return;
    }
    Promise.resolve(restore).catch((reason) => reportPreviewRestoreError(context, reason));
  }

  function teardownPreview(context) {
    if (!context || context.tornDown) return;
    context.tornDown = true;
    resetPreviewLifecycle();
    const record = context.invocation;
    if (record && !record.settled) {
      // The backend command may still be compiling. Restore only after that
      // command settles so an older preview cannot race the cleanup.
      record.promise.then(
        () => maybeRestorePreview(context),
        () => maybeRestorePreview(context),
      );
    } else {
      maybeRestorePreview(context);
    }
  }

  function previewRequestActive(request) {
    return !!request &&
      previewRequest?.generation === request.generation &&
      previewGeneration === request.generation &&
      route.name === "track-editor" &&
      route.id === request.id &&
      (route.param || null) === request.sourcePlaylist &&
      track?.id === request.id;
  }

  function pausePreview() {
    if (!previewQueueActive) return;
    const generation = previewGeneration, id = track?.id, sourcePlaylist = playlistId;
    api.pause().then(() => {
      if (generation !== previewGeneration || route.name !== "track-editor" || route.id !== id ||
          (route.param || null) !== sourcePlaylist) return;
      if (previewState === "playing") previewState = "paused";
    }).catch((reason) => {
      if (generation === previewGeneration && route.name === "track-editor" && route.id === id &&
          (route.param || null) === sourcePlaylist) {
        actionError = String(reason || "Could not pause preview.");
      }
    });
  }

  function resumePreview() {
    if (!previewQueueActive) return;
    const generation = previewGeneration, id = track?.id, sourcePlaylist = playlistId;
    api.play().then(() => {
      if (generation !== previewGeneration || route.name !== "track-editor" || route.id !== id ||
          (route.param || null) !== sourcePlaylist) return;
      if (previewState === "paused") previewState = "playing";
    }).catch((reason) => {
      if (generation === previewGeneration && route.name === "track-editor" && route.id === id &&
          (route.param || null) === sourcePlaylist) {
        actionError = String(reason || "Could not resume preview.");
      }
    });
  }

  async function playPreview() {
    if (!ready || !track || previewBusy || previewQueueAwaiting || validation.firstError) return;
    const context = previewContext;
    if (!context) return;
    const id = track.id;
    const sourcePlaylist = playlistId;
    const nextCuts = canonicalCuts();
    const nextLoop = canonicalLoop();
    const signature = snapshot(nextCuts, nextLoop);
    const request = {
      generation: ++previewGeneration,
      id,
      sourcePlaylist,
      signature,
      baselineQueue: queueIdentity,
      acceptedQueueIdentity: previewQueueIdentity,
      recoveryState: previewQueueActive ? previewState : null,
    };
    previewRequest = request;
    previewQueueAwaiting = null;
    previewBusy = true;
    previewState = "loading";
    actionError = "";
    const position = Math.max(0, Math.min(duration, Math.round(Number(cursorMs) || 0)));
    context.attempted = true;
    let invocation;
    try {
      invocation = Promise.resolve(api.previewTrackEdit(
        track,
        nextCuts,
        nextLoop,
        position,
        context.previewLeaseId,
      ));
    } catch (reason) {
      invocation = Promise.reject(reason);
    }
    const record = { promise: invocation, settled: false };
    context.invocation = record;
    previewInvocation = invocation;
    invocation.then(
      () => {
        record.settled = true;
      },
      () => {
        record.settled = true;
      },
    );
    try {
      await invocation;
      if (!previewRequestActive(request)) return;
      const targetQueue = editorTrackCurrent && queueEditSignature === request.signature;
      previewSignature = request.signature;
      previewQueueIdentity = targetQueue ? queueIdentity : "";
      previewRequest = null;
      if (targetQueue) {
        previewQueueAwaiting = null;
        previewBusy = false;
        previewState = draftSignature === request.signature
          ? (playback.playing ? "playing" : "paused")
          : "stale";
      } else {
        previewQueueAwaiting = {
          generation: request.generation,
          baselineQueue: request.baselineQueue,
        };
      }
    } catch (reason) {
      if (!previewRequestActive(request)) return;
      const recoveryState = request.recoveryState;
      previewRequest = null;
      previewBusy = false;
      // Keep the last accepted queue usable when a replacement compile fails:
      // the draft is still stale, but Pause/Resume must not disappear.
      previewState = recoveryState && previewQueueMatches ? "stale" : "error";
      actionError = String(reason || "Could not preview this edit.");
    } finally {
      if (context?.invocation === record) context.invocation = null;
      if (previewInvocation === invocation) previewInvocation = null;
    }
  }

  function sourceToCompiled(position) {
    const source = Math.max(0, Math.min(duration, Math.round(Number(position) || 0)));
    let removed = 0;
    for (const cut of canonicalCuts()) {
      if (source <= cut.start_ms) break;
      if (source < cut.end_ms) return Math.max(0, cut.start_ms - removed);
      removed += cut.end_ms - cut.start_ms;
    }
    const compiled = Math.max(0, source - removed);
    return playback.duration_ms > 0 ? Math.min(compiled, playback.duration_ms) : compiled;
  }

  function compiledToSource(position, editCuts = []) {
    const transportDuration = playback.duration_ms > 0
      ? playback.duration_ms
      : Math.max(0, duration - editCuts.reduce((total, cut) => total + (cut.end_ms - cut.start_ms), 0));
    const compiled = Math.max(0, Math.min(transportDuration, Math.round(Number(position) || 0)));
    let removed = 0;
    for (const cut of editCuts) {
      const seam = cut.start_ms - removed;
      if (compiled < seam) break;
      removed += cut.end_ms - cut.start_ms;
    }
    return Math.min(duration, compiled + removed);
  }

  function seekPreview(position) {
    if (!previewIsCurrent || !editorTrackCurrent) return;
    const generation = previewGeneration, id = track?.id, sourcePlaylist = playlistId;
    api.seek(sourceToCompiled(position)).catch((reason) => {
      if (generation === previewGeneration && route.name === "track-editor" && route.id === id &&
          (route.param || null) === sourcePlaylist) {
        actionError = String(reason || "Could not seek preview.");
      }
    });
  }
  $effect(() => {
    const routeName = route.name;
    const routeId = route.id;
    const sourcePlaylist = route.param || null;
    if (routeName !== "track-editor" || !routeId) return;
    const context = {
      id: routeId,
      sourcePlaylist,
      previewLeaseId: allocatePreviewLeaseId(),
      attempted: false,
      invocation: null,
      restoreRequested: false,
      tornDown: false,
    };
    previewContext = context;
    return () => teardownPreview(context);
  });
  const previewIsCurrent = $derived(
    previewQueueMatches &&
      draftSignature === previewSignature &&
      (previewState === "playing" || previewState === "paused"),
  );

  function readPreviewPosition() {
    if (!previewIsCurrent || !editorTrackCurrent || !currentQueueTrack?.effective_edit) return null;
    const editCuts = currentQueueTrack.effective_edit.cuts;
    if (previewCutsSource !== editCuts || previewCutsSignature !== queueEditSignature) {
      previewCutsSource = editCuts;
      previewCutsSignature = queueEditSignature;
      previewCuts = canonicalCuts(editCuts ?? []);
    }
    return compiledToSource(projectedPlaybackPositionMs(), previewCuts);
  }

  const previewPositionMs = $derived.by(() => readPreviewPosition());

  $effect(() => {
    const queueKey = queueIdentity;
    const request = previewRequest;
    const awaiting = previewQueueAwaiting;
    const requestTarget = !!request && editorTrackCurrent && queueEditSignature === request.signature;
    const requestAccepted = !!request &&
      !!previewSignature &&
      queueEditSignature === previewSignature &&
      queueKey === request.acceptedQueueIdentity;
    if (request && previewBusy) {
      if (requestTarget || requestAccepted || queueKey === request.baselineQueue) return;
      resetPreviewLifecycle();
      return;
    }
    if (!previewSignature) return;
    if (awaiting?.generation === previewGeneration) {
      if (!previewQueueEditMatches) {
        if (queueKey === awaiting.baselineQueue) return;
        resetPreviewLifecycle();
        return;
      }
      previewQueueIdentity = queueKey;
      previewQueueAwaiting = null;
      previewBusy = false;
      if (playback.error) {
        previewState = "error";
        actionError = String(playback.error);
      } else {
        previewState = draftSignature === previewSignature
          ? (playback.playing ? "playing" : "paused")
          : "stale";
      }
      return;
    }
    if (!previewQueueMatches) {
      resetPreviewLifecycle();
      return;
    }
    if (playback.error) {
      previewState = "error";
      actionError = String(playback.error);
      return;
    }
    if (draftSignature !== previewSignature) {
      if (previewState === "playing" || previewState === "paused") previewState = "stale";
      return;
    }
    if (previewState === "stale" || previewState === "playing" || previewState === "paused") {
      const nextState = playback.playing ? "playing" : "paused";
      if (previewState !== nextState) previewState = nextState;
    }
  });

  $effect(() => {
    const id = track?.id, sourcePlaylist = playlistId;
    retryGeneration;
    saving = false;
    const generation = ++definitionGeneration;
    loading = !!id;
    loadError = actionError = "";
    cursorMs = 0;
    resetPreviewLifecycle();
    clearHistory();
    cuts = [];
    loopRange = selected = null;
    enabled = definitionExists = false;
    baseline = snapshot([], null);
    if (!id) return;
    untrack(() => api.getTrackEdit(id, sourcePlaylist).then((status) => {
      if (!active(id, sourcePlaylist, generation)) return;
      cuts = hydrateCuts(status?.definition?.cuts);
      loopRange = hydrateLoop(status?.definition?.loop_range);
      definitionExists = !!status?.definition;
      enabled = !!status?.enabled;
      baseline = snapshot();
    }).catch((reason) => {
      if (active(id, sourcePlaylist, generation)) loadError = String(reason || "Could not load this edit.");
    }).finally(() => {
      if (active(id, sourcePlaylist, generation)) loading = false;
    }));
  });

  $effect(() => {
    const id = track?.id, generation = ++waveformGeneration;
    waveform = null;
    waveformError = "";
    waveformState = id ? "loading" : "idle";
    if (!id) return;
    untrack(() => api.getTrackWaveform(id).then((result) => {
      if (generation !== waveformGeneration || route.name !== "track-editor" || route.id !== id) return;
      waveform = result;
      waveformState = "ready";
    }).catch((reason) => {
      if (generation !== waveformGeneration || route.name !== "track-editor" || route.id !== id) return;
      waveformError = String(reason || "Could not build the waveform.");
      waveformState = "error";
    }));
    return () => { api.cancelTrackWaveform(id).catch(() => {}); };
  });
  $effect(() => {
    if (!dirty) return;
    return setNavigationGuard(() => window.confirm("Discard your unsaved playback edit?"));
  });
  $effect(() => {
    if (!dirty) return;
    const warn = (event) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  });
</script>

<section class="view page edit-page">
  <button class="page-back" onclick={goBack}><Icon name="back" size={14} />Back</button>
  {#if !track}
    <div class="empty"><h1 class="h">Track unavailable</h1><p class="sub">Open the editor from a track’s menu.</p></div>
  {:else}
    <header class="edit-head">
      <Cover src={track.cover_url} id={track.album_id || track.uri} name={track.name} size={76} lg />
      <div class="edit-title">
        <span class="tag">Track edit</span>
        <h1>{track.name}</h1>
        <p>{(track.artist_names ?? []).join(", ")} · <span class="tnum">{formatTime(track.duration_ms)}</span> original</p>
      </div>
    </header>
    <section class="repair-sheet glass-card" aria-busy={loading}>
      <div class="sheet-head">
        <div class="sheet-intro"><p class="caps">Original timeline</p><h2>Remove sections or repeat a passage.</h2></div>
        <p class="sheet-note">Cuts and loops change playback only — the audio file is never modified.</p>
      </div>
      <div class="preview-strip">
        <div class="preview-actions">
          <button class="preview-button" disabled={!ready || previewBusy || !!validation.firstError} onclick={playPreview} title={previewActionLabel}>
            <Icon name="play" size={14} />
            {previewActionLabel}
          </button>
          {#if previewQueueActive}
            <button class="preview-pause" onclick={playback.playing ? pausePreview : resumePreview} title={playback.playing ? "Pause preview" : "Resume preview"}>
              <Icon name={playback.playing ? "pause" : "play"} size={13} />
              {playback.playing ? "Pause" : "Resume"}
            </button>
          {/if}
        </div>
        <div class="preview-copy">
          <strong>{previewStatus.label}</strong>
          <span class="tnum">{previewStatus.copy} · cursor {formatExactTime(cursorMs)}</span>
        </div>
        <span class="preview-status" class:active={previewStatus.active} class:stale={previewState === "stale"} aria-hidden="true"></span>
      </div>
      <TrackEditWaveform
        durationMs={duration}
        {waveform}
        {waveformState}
        {waveformError}
        disabled={!ready || saving}
        bind:cuts
        bind:loopRange
        bind:selected
        bind:cursorMs
        previewActive={previewIsCurrent}
        previewPlaying={previewIsCurrent && playback.playing}
        previewPositionMs={previewPositionMs}
        previewPositionReader={readPreviewPosition}
        onsave={save}
        oncommit={commitHistory}
        onundo={undo}
        onredo={redo}
        onseek={seekPreview}
      />
      {#if loading}<p class="definition-status">Loading your saved edit…</p>
      {:else if loadError}<div class="definition-error" role="alert"><span>{loadError}</span><button class="pill" onclick={() => (retryGeneration += 1)}>Try again</button></div>{/if}

      <section class="range-section">
        <div class="range-heading"><div><h3>Cuts</h3><p>These sections are removed from playback.</p></div><span class="range-count tnum">{cuts.length}</span></div>
        {#if !cuts.length}<p class="empty-range">No cuts yet. Shift-drag across empty waveform space or press C.</p>{/if}
        <div class="range-list">
          {#each cuts as cut, index (cut._key)}
            <div class="exact-row cut-row" class:selected={selected?.type === "cut" && selected.key === cut._key}>
              <button class="region-index" onclick={() => (selected = { type: "cut", key: cut._key })} aria-label={`Select cut ${index + 1}`}><span></span>{String(index + 1).padStart(2, "0")}</button>
              <label>Start<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(cut.start_ms)} inputmode="decimal" aria-label={`Cut ${index + 1} start time`} onkeydown={(event) => nudgeTime(event, "cut", cut._key, "start_ms")} onchange={(event) => commitTime(event, "cut", cut._key, "start_ms")} /></label>
              <span class="arrow">→</span>
              <label>End<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(cut.end_ms)} inputmode="decimal" aria-label={`Cut ${index + 1} end time`} onkeydown={(event) => nudgeTime(event, "cut", cut._key, "end_ms")} onchange={(event) => commitTime(event, "cut", cut._key, "end_ms")} /></label>
              <span class="range-duration tnum">{formatExactTime(cut.end_ms - cut.start_ms)}</span>
              <button class="remove-region" disabled={!ready || saving} title={`Remove cut ${index + 1}`} onclick={() => removeCut(cut._key)}><Icon name="x" size={12} /></button>
              {#if validation.cutErrors[index]}<p class="range-error" role="alert">{validation.cutErrors[index]}</p>{/if}
            </div>
          {/each}
        </div>
      </section>

      <section class="range-section">
        <div class="range-heading"><div><h3>Loop</h3><p>Repeat one section several times. Plays is how many times it plays in all.</p></div><span class="loop-chip">{loopRange ? "Set" : "Off"}</span></div>
        {#if loopRange}
          <div class="exact-row loop-row" class:selected={selected?.type === "loop"}>
            <button class="region-index" onclick={() => (selected = { type: "loop", key: loopRange._key })} aria-label="Select loop"><span></span>LP</button>
            <label>Start<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(loopRange.start_ms)} inputmode="decimal" aria-label="Loop start time" onkeydown={(event) => nudgeTime(event, "loop", loopRange._key, "start_ms")} onchange={(event) => commitTime(event, "loop", loopRange._key, "start_ms")} /></label>
            <span class="arrow">→</span>
            <label>End<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(loopRange.end_ms)} inputmode="decimal" aria-label="Loop end time" onkeydown={(event) => nudgeTime(event, "loop", loopRange._key, "end_ms")} onchange={(event) => commitTime(event, "loop", loopRange._key, "end_ms")} /></label>
            <label class="play-count-field">Plays<input class="time-input tnum" type="number" min="2" max="32" step="1" disabled={!ready || saving} value={loopRange.play_count ?? ""} aria-label="Total loop passes" onchange={(event) => setPlayCount(loopRange._key, event.currentTarget.value)} /></label>
            <span class="range-duration tnum">{Number.isInteger(loopRange.play_count) ? `${loopRange.play_count} total` : "Invalid"}</span>
            <button class="remove-region" disabled={!ready || saving} title="Remove loop" onclick={removeLoop}><Icon name="x" size={12} /></button>
            {#if validation.loopError}<p class="range-error" role="alert">{validation.loopError}</p>{/if}
          </div>
        {:else}<p class="empty-range">No loop. Use Add loop or press L to create one.</p>{/if}
      </section>

      {#if playlistId}<label class="enable-row"><input class="set-check enable-check" type="checkbox" checked={enabled} disabled={!ready || saving || !definitionExists} onchange={(event) => setEnabled(event.currentTarget.checked)} /><span><strong>Use edited version in this playlist</strong><small>Other playlists play the original.</small></span></label>{/if}
      {#if actionError}<p class="edit-error" role="alert">{actionError}</p>{/if}
      {#if validation.firstError}<p class="edit-error" role="alert">Fix the highlighted range before saving.</p>{/if}
      <footer class="edit-footer">
        <div class="footer-left">
          <div class="history-actions" aria-label="Edit history">
            <button class="pill sm" disabled={!undoStack.length || !ready || saving} onclick={undo} title="Undo (Ctrl+Z)">Undo</button>
            <button class="pill sm" disabled={!redoStack.length || !ready || saving} onclick={redo} title="Redo (Ctrl+Shift+Z)">Redo</button>
          </div>
          <div class="save-state" class:dirty><span></span>{saving ? "Saving edit…" : dirty ? "Unsaved changes" : definitionExists ? "Saved" : "No edit saved"}</div>
        </div>
        <div><button class="pill warn" disabled={!ready || saving || !definitionExists} onclick={removeDefinition}>Delete edit</button><button class="pill accent" disabled={!dirty || !ready || saving || !!validation.firstError || (!cuts.length && !loopRange)} onclick={save}>{saving ? "Saving…" : "Save edit"}</button></div>
      </footer>
    </section>
  {/if}
</section>

<style>
.edit-page { max-width: 1240px; margin-inline: auto; }
/* Same left-aligned quiet column as every other empty state — centring it
   would dress a missing track up as a modal event. */
.edit-head p { font-size: var(--t-12); }
.edit-head { display: flex; align-items: center; gap: var(--s4); margin-bottom: var(--s5); }
.edit-head :global(.art) { flex: none; }
.edit-title { min-width: 0; }
.edit-head h1 {
  margin: 6px 0 5px; overflow: hidden;
  font: var(--w-bold) clamp(28px, 4vw, 42px)/1.02 var(--font-display);
  letter-spacing: -.025em; text-overflow: ellipsis; white-space: nowrap;
}
.edit-head p, .sheet-note, .range-heading p, .empty-range { color: var(--fg-2); }
/* A card on the pane (.glass-card, in the markup). */
.repair-sheet { position: relative; overflow: clip; border-radius: var(--r3); }
.sheet-head {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(260px, 430px);
  align-items: end; gap: var(--s6); padding: var(--s5) var(--s6) var(--s4);
  border-top: 2px solid color-mix(in srgb, var(--foam) 58%, transparent);
  border-bottom: 1px solid var(--line);
}
.sheet-intro { min-width: 0; }
.sheet-head h2 { margin-top: 4px; font: var(--w-bold) var(--t-20) var(--font-display); letter-spacing: -.015em; }
.sheet-note { margin: 0; font-size: var(--t-12); line-height: 1.45; text-align: right; }
.preview-strip {
  display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center;
  gap: var(--s3); margin: var(--s3) var(--s6) var(--s2); padding: 9px 12px;
  border: 1px solid color-mix(in srgb, var(--accent) 28%, var(--line));
  border-left: 2px solid color-mix(in srgb, var(--accent) 72%, transparent);
  border-radius: var(--r2); background: color-mix(in srgb, var(--accent) 7%, transparent);
}
.preview-actions { display: flex; align-items: center; gap: 6px; flex: none; }
.preview-button, .preview-pause {
  display: inline-flex; align-items: center; gap: 7px; min-height: 34px;
  border-radius: var(--r2); white-space: nowrap;
}
.preview-button {
  padding: 0 var(--s3); border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
  background: var(--accent); color: var(--accent-ink);
  font-size: var(--t-12); font-weight: var(--w-semi);
  transition: filter var(--d1) var(--ease), transform var(--d1) var(--ease);
}
.preview-pause {
  gap: 6px; padding: 0 10px; border: 1px solid var(--line-2);
  background: var(--raise-1); color: var(--fg-1); font-size: var(--t-11);
  transition: color var(--d1) var(--ease), border-color var(--d1) var(--ease), background-color var(--d1) var(--ease);
}
.preview-pause:hover { border-color: color-mix(in srgb, var(--fg) 22%, transparent); background: var(--raise-2); }
.preview-button:hover:not(:disabled) { filter: brightness(1.08); }
.preview-button:active:not(:disabled) { transform: scale(0.98); }
.preview-copy { display: grid; min-width: 0; gap: 2px; color: var(--fg-2); font-size: var(--t-11); }
.preview-copy strong { color: var(--fg-1); font-size: var(--t-12); }
.preview-copy span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.preview-status {
  display: inline-flex; align-items: center; gap: 6px;
  margin-left: auto; padding: 3px 9px; border: 1px solid var(--line);
  border-radius: var(--rf); color: var(--fg-3); font: var(--t-11) var(--font-small);
  white-space: nowrap;
}
/* A status lamp, not a second headline: the label already runs in the copy,
   so the pill leads with the dot and lets colour carry the state. */
.preview-status::before { content: ""; width: 5px; height: 5px; border-radius: 50%; background: var(--fg-3); }
.preview-status.active { border-color: color-mix(in srgb, var(--accent) 40%, transparent); color: var(--accent); }
.preview-status.active::before { background: var(--accent); }
.preview-status.stale { border-color: color-mix(in srgb, var(--gold) 40%, transparent); color: var(--gold); }
.preview-status.stale::before { background: var(--gold); }
:global(.repair-sheet>.waveform-editor) { padding: 0 var(--s6) var(--s4); }
.definition-status, .definition-error {
  min-height: 42px; margin: 0 var(--s6); padding: 10px var(--s3);
  border-top: 1px solid var(--line); color: var(--fg-2);
}
.definition-error { display: flex; align-items: center; justify-content: space-between; color: var(--rose); }
.range-section { padding: var(--s5) var(--s6); border-top: 1px solid var(--line-2); }
.range-heading {
  display: flex; align-items: center; justify-content: space-between;
  gap: var(--s4); margin-bottom: var(--s3);
}
.range-heading h3 { font: var(--w-bold) var(--t-15) var(--font-display); letter-spacing: -.01em; }
.range-heading p, .empty-range { font: var(--t-12) var(--font-small); }
.range-count, .loop-chip {
  flex: none; padding: 3px 8px; border: 1px solid var(--line);
  border-radius: var(--rf); font: var(--t-11) var(--font-small);
}
/* A quantity, so it wears the app's count colour like every other count. */
.range-count { color: var(--count); }
.loop-chip { border-color: color-mix(in srgb, var(--gold) 28%, var(--line)); color: var(--gold); }
.range-list { display: grid; gap: 6px; }
.exact-row {
  display: grid; align-items: end; column-gap: var(--s2); row-gap: 5px;
  min-height: 62px; padding: 7px 8px 7px 0; border: 1px solid transparent;
  border-radius: var(--r2); background: var(--raise-1);
  transition: border-color var(--d1) var(--ease), background-color var(--d1) var(--ease);
}
.exact-row.cut-row {
  grid-template-columns: 44px minmax(120px, 1.1fr) 18px minmax(120px, 1.1fr) minmax(100px, .8fr) 38px;
}
.exact-row.loop-row {
  grid-template-columns: 44px minmax(120px, 1.1fr) 18px minmax(120px, 1.1fr) minmax(84px, .6fr) minmax(100px, .8fr) 38px;
}
.exact-row:hover:not(.selected) { border-color: var(--line-2); }
/* Selection is the region's hue as a wash under the row, matching the
   tint the region paints on the waveform above — one fact, two surfaces. */
.exact-row.cut-row.selected {
  border-color: color-mix(in srgb, var(--rose-ink) 38%, var(--line));
  background: color-mix(in srgb, var(--rose-ink) 7%, var(--raise-1));
}
.loop-row.selected {
  border-color: color-mix(in srgb, var(--gold) 38%, var(--line));
  background: color-mix(in srgb, var(--gold) 7%, var(--raise-1));
}
.region-index {
  align-self: stretch; display: flex; align-items: center; gap: 7px; padding-left: 10px;
  border: 0; color: var(--fg-2); background: transparent;
  font: var(--t-11) var(--font-mono);
  transition: color var(--d1) var(--ease);
}
.region-index:hover { color: var(--fg); }
.region-index span { width: 3px; height: 22px; border-radius: var(--rf); background: var(--rose-ink); }
.loop-row .region-index span { background: var(--gold); }
.exact-row label { display: grid; gap: 4px; color: var(--fg-2); font: var(--t-11) var(--font-small); }
.time-input {
  width: 100%; height: 34px; padding: 0 9px; border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: var(--r1); outline: 0; background: rgb(0 0 0 / 0.22); color: var(--fg); font: var(--t-12) var(--font-mono);
  transition: border-color var(--d1) var(--ease), box-shadow var(--d1) var(--ease), opacity var(--d1) var(--ease);
}
.time-input:hover:not(:disabled):not(:focus) { border-color: color-mix(in srgb, var(--fg) 18%, var(--line-2)); }
.time-input:focus { border-color: var(--accent); box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 14%, transparent); }
.time-input:disabled { opacity: .45; }
.arrow, .range-duration, .remove-region { align-self: center; margin-top: 16px; }
.arrow { color: var(--fg-3); text-align: center; }
.range-duration { padding: 0 var(--s2); color: var(--fg-2); font: var(--t-11) var(--font-mono); white-space: nowrap; }
.remove-region {
  display: grid; place-items: center; width: 34px; height: 34px; margin-inline: auto;
  border: 1px solid transparent; border-radius: var(--r1); color: var(--fg-2); background: transparent;
  transition: color var(--d1) var(--ease), border-color var(--d1) var(--ease), background-color var(--d1) var(--ease);
}
.remove-region:hover:not(:disabled) {
  border-color: color-mix(in srgb, var(--rose-ink) 34%, transparent); color: var(--rose-ink);
  background: color-mix(in srgb, var(--rose-ink) 10%, transparent);
}
.range-error { grid-column: 2/-1; color: var(--love); font-size: var(--t-11); }
.enable-row {
  display: flex; align-items: flex-start; gap: var(--s3); margin: 0 var(--s6) var(--s5); padding: var(--s4);
  border: 1px solid var(--line-2); border-radius: var(--r2); background: var(--raise-1);
  transition: border-color var(--d1) var(--ease), opacity var(--d1) var(--ease);
}
.enable-row:hover:not(:has(.enable-check:disabled)) { border-color: color-mix(in srgb, var(--fg) 18%, var(--line-2)); }
.enable-row:focus-within { border-color: color-mix(in srgb, var(--accent) 45%, transparent); }
/* The commit is gated on a saved definition existing; while it is gated the
   whole row steps back rather than leaving an active-looking control dead. */
.enable-row:has(.enable-check:disabled) { opacity: .55; }
.enable-row span { display: grid; gap: 2px; }
.enable-row strong { font-weight: var(--w-semi); }
.enable-row small { color: var(--fg-2); font-size: var(--t-11); }
/* The app's one checkbox (.set-check, in the markup), on the title's line.
   Gated, the whole row steps back, so the box does not dim a second time. */
.enable-check { margin-top: 1px; flex: none; }
.enable-check:disabled { opacity: 1; }
.edit-error { margin: 0 var(--s6) var(--s3); color: var(--love); font-size: var(--t-12); }
.edit-footer {
  position: sticky; z-index: 8; bottom: 0; display: flex; align-items: center;
  justify-content: space-between; min-height: 64px; padding: var(--s3) var(--s6);
  /* Strip glass: always covering, since the sheet runs on under it. */
  border-top: 1px solid var(--line-2); background: var(--tint-strip);
  -webkit-backdrop-filter: var(--frost-strip);
          backdrop-filter: var(--frost-strip);
}
.edit-footer>div, .footer-left, .history-actions { display: flex; align-items: center; gap: var(--s2); }
.save-state { color: var(--fg-2); font-size: var(--t-12); white-space: nowrap; }
.save-state>span { display: inline-block; width: 7px; height: 7px; margin-right: 7px; border-radius: 50%; background: var(--fg-3); }
.save-state.dirty>span { background: var(--gold); }
@media(max-width:760px) {
  .sheet-head { grid-template-columns: 1fr; gap: var(--s2); padding: var(--s4); }
  .sheet-note { max-width: 540px; text-align: left; }
  .preview-strip { margin-inline: var(--s4); }
  :global(.repair-sheet>.waveform-editor) { padding-inline: var(--s4); }
  .range-section { padding: var(--s4); }
  .exact-row.cut-row { grid-template-columns: 40px 1fr 18px 1fr 44px; }
  .exact-row.loop-row { grid-template-columns: 40px 1fr 18px 1fr 90px 44px; }
  .range-duration { display: none; }
  .edit-footer { padding-inline: var(--s4); }
}
@media(max-width:560px) {
  .sheet-head { display: block; }.sheet-note { margin-top: var(--s2); }
  .preview-strip { grid-template-columns: 1fr; align-items: flex-start; }
  .preview-actions { flex-wrap: wrap; }.preview-status { margin-left: 0; }
  .exact-row, .exact-row.cut-row, .exact-row.loop-row {
    grid-template-columns: 38px minmax(0, 1fr) 40px; row-gap: 6px;
  }
  .exact-row label:first-of-type { grid-column: 2; }
  .exact-row label:nth-of-type(2) { grid-column: 2; grid-row: 2; }
  .region-index { grid-row: 1/3; }.arrow { display: none; }
  .remove-region { grid-column: 3; grid-row: 1/3; }
  .loop-row .play-count-field { grid-column: 2; grid-row: 3; }
  .loop-row .region-index, .loop-row .remove-region { grid-row: 1/4; }
  .edit-footer { align-items: flex-start; flex-direction: column; }
  .edit-footer>div:last-child { width: 100%; justify-content: flex-end; }
}
@media(max-width:420px) {
  .edit-head :global(.art) { display: none; }.edit-head { margin-bottom: var(--s4); }
}
</style>
