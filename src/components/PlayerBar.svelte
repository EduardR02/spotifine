<script>
  import { tick, untrack } from "svelte";
  import {
    playback,
    api,
    togglePlay,
    navigate,
    positionMs,
    playbackIdentity,
    ui,
    setNowPlayingOpen,
    nowSaved,
    lookupSavedIn,
    invalidateSavedIn,
  } from "../lib/state.svelte.js";
  import { session } from "../lib/state.svelte.js";
  import { personalConnected, watchPersonal, setLiked } from "../lib/personal.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import Slider from "./Slider.svelte";
  import DevicePicker from "./DevicePicker.svelte";
  import Menu from "./Menu.svelte";
  import { formatTime } from "../lib/time.js";
  import { morphLayout } from "../lib/layout.js";
  import { frost } from "../lib/ambient.svelte.js";
  import { panel as nowPlayingPanel, nowPlayingShown } from "../lib/panel.svelte.js";
  import { scrollbar } from "../lib/scrollbar.js";
  import { listen } from "@tauri-apps/api/event";

  const CONTROLS_KEY = "sr.player-bar-controls";
  const CONTROL_OPTIONS = [
    { key: "speed", label: "Speed controls", icon: "clock" },
    { key: "mute", label: "Mute button", icon: "volume" },
    { key: "artwork", label: "Cover artwork", icon: "album" },
    { key: "metadata", label: "Title and artist", icon: "artist" },
    { key: "library", label: "Library checkmark / heart", icon: "heart" },
  ];

  // Restore before first paint, like the now-playing panel preferences.
  function readControls() {
    let saved;
    try {
      saved = JSON.parse(localStorage.getItem(CONTROLS_KEY));
    } catch {
      // Unavailable or invalid storage leaves every control visible.
    }
    return {
      speed: saved?.speed !== false,
      mute: saved?.mute !== false,
      artwork: saved?.artwork !== false,
      metadata: saved?.metadata !== false,
      library: saved?.library !== false,
    };
  }

  const controls = $state(readControls());
  let playerBar = $state(null);
  let barMenu = $state(null);
  // The library mark: the check for anything saved, the heart for any other song.
  const hasMark = $derived(controls.library && !!current
    && (nowSaved.refs.length > 0 || current.uri?.startsWith("spotify:track:")));
  const hasNow = $derived(controls.metadata || controls.artwork || hasMark);

  function openBarMenu(event) {
    const keyboard = event.type === "keydown";
    if (keyboard && event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
    if (event.target.closest(".speed-menu, .p-saved-panel")) return;
    event.preventDefault();
    event.stopPropagation();
    speedOpen = false;
    savedHushed = true;
    savedHover = savedFocus = false;
    const rect = keyboard ? event.target.getBoundingClientRect() : null;
    barMenu = {
      at: keyboard
        ? { x: rect.left + rect.width / 2, y: rect.top }
        : { x: event.clientX, y: event.clientY },
    };
  }

  function toggleControl(key) {
    controls[key] = !controls[key];
    if (key === "speed" && !controls.speed) speedOpen = false;
    try {
      localStorage.setItem(CONTROLS_KEY, JSON.stringify(controls));
    } catch {
      // Like panel width, the preference stays session-local without storage.
    }
  }

  let dragPos = $state(null);

  const VOLUME_INTERVAL_MS = 50;
  let volumeControl = $state(null);
  let volumeDraft = $state(null);
  let volumeError = $state("");
  let restoreVolume = $state(70);
  let pendingVolume = null;
  let activeVolume = null;
  let volumeTimer = null;
  let volumeUrgent = false;
  let volumeSentAt = -Infinity;
  let volumeDisposed = false;
  const volumePercent = $derived(volumeDraft ?? playback.volume);

  $effect(() => {
    const percent = playback.volume;
    if (percent > 0) restoreVolume = percent;
  });

  // At most one command is in flight and one latest value is waiting.
  // Leading/trailing updates keep a held drag audible, unlike a debounce.
  async function flushVolume() {
    clearTimeout(volumeTimer);
    volumeTimer = null;
    if (volumeDisposed || activeVolume !== null || pendingVolume === null) return;
    const delay = VOLUME_INTERVAL_MS - (performance.now() - volumeSentAt);
    if (!volumeUrgent && delay > 0) {
      volumeTimer = setTimeout(flushVolume, delay);
      return;
    }
    const target = pendingVolume;
    pendingVolume = null;
    activeVolume = target;
    volumeUrgent = false;
    volumeSentAt = performance.now();
    try {
      await api.setVolume(target);
    } catch (error) {
      if (!volumeDisposed && pendingVolume === null) {
        volumeError = `Could not change volume: ${String(error)}`;
      }
    } finally {
      activeVolume = null;
      if (!volumeDisposed) {
        if (pendingVolume === null) volumeDraft = null;
        else flushVolume();
      }
    }
  }

  function changeVolume(percent, final = false) {
    if (!Number.isFinite(percent)) return;
    const next = Math.min(100, Math.max(0, Math.round(percent)));
    volumeError = "";
    volumeDraft = next;
    pendingVolume = next === activeVolume
      || (activeVolume === null && next === playback.volume) ? null : next;
    volumeUrgent = final;
    if (pendingVolume === null && activeVolume === null) volumeDraft = null;
    flushVolume();
  }

  function toggleMute() {
    if (volumePercent > 0) {
      restoreVolume = volumePercent;
      changeVolume(0, true);
    } else {
      changeVolume(restoreVolume, true);
    }
  }

  $effect(() => {
    volumeDisposed = false;
    return () => {
      volumeDisposed = true;
      clearTimeout(volumeTimer);
      pendingVolume = null;
    };
  });

  $effect(() => {
    const node = volumeControl;
    if (!node) return;
    let remainder = 0;
    let lastWheelAt = -Infinity;
    const onWheel = (event) => {
      // Leave pinch-to-zoom and horizontal navigation to the browser.
      if (event.ctrlKey || event.metaKey || !Number.isFinite(event.deltaY) || !event.deltaY
        || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
      event.preventDefault();
      const now = performance.now();
      const pixels = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 100 : 1);
      const delta = Math.max(-10, Math.min(10, -pixels / 20));
      if (now - lastWheelAt > 200 || Math.sign(remainder) !== Math.sign(delta)) remainder = 0;
      lastWheelAt = now;
      remainder += delta;
      const steps = Math.trunc(remainder);
      if (!steps) return;
      remainder -= steps;
      untrack(() => changeVolume(volumePercent + steps));
    };
    node.addEventListener("wheel", onWheel, { passive: false });
    return () => node.removeEventListener("wheel", onWheel);
  });

  function numberMs(value) {
    const ms = Number(value);
    return Number.isFinite(ms) ? Math.max(0, ms) : 0;
  }

  /**
   * Queue edit ranges stay in source coordinates. The player rail, however,
   * is the engine's compiled one-pass timeline: each cut collapses to one
   * seam and everything after it shifts left by the removed duration.
   */
  function makeEditTimeline(edit, originalValue, wireValue) {
    const originalDuration = numberMs(originalValue);
    const cuts = (edit?.cuts ?? [])
      .map((range) => ({
        start: Number(range?.start_ms),
        end: Number(range?.end_ms),
      }))
      .filter((range) => (
        Number.isFinite(range.start)
        && Number.isFinite(range.end)
        && range.start >= 0
        && range.end > range.start
        && range.end <= originalDuration
      ))
      .sort((left, right) => left.start - right.start || left.end - right.end)
      .reduce((valid, range) => {
        const previous = valid[valid.length - 1];
        if (!previous || range.start >= previous.end) valid.push(range);
        return valid;
      }, []);
    const removedDuration = cuts.reduce((total, cut) => total + cut.end - cut.start, 0);
    const onePassDuration = Math.max(0, originalDuration - removedDuration);
    const wireDuration = numberMs(wireValue);
    const compiledDuration = edit && wireDuration > 0 ? wireDuration : onePassDuration;

    const sourceToCompiled = (sourceValue) => {
      const source = Math.min(originalDuration, Math.max(0, numberMs(sourceValue)));
      let removedBefore = 0;
      for (const cut of cuts) {
        if (source <= cut.start) break;
        if (source < cut.end) return cut.start - removedBefore;
        removedBefore += cut.end - cut.start;
      }
      return source - removedBefore;
    };
    const percent = (value) => compiledDuration > 0
      ? Math.min(100, Math.max(0, (value / compiledDuration) * 100))
      : 0;

    const seamByPosition = new Map();
    for (const cut of cuts) {
      const position = sourceToCompiled(cut.start);
      const existing = seamByPosition.get(position);
      const sourceText = `${formatTime(cut.start)}–${formatTime(cut.end)}`;
      if (existing) {
        existing.sourceText += `, ${sourceText}`;
      } else {
        seamByPosition.set(position, {
          position,
          percent: percent(position),
          sourceText,
        });
      }
    }
    const seams = [...seamByPosition.values()].map((seam) => ({
      ...seam,
      title: `Cut seam at compiled ${formatTime(seam.position)}; removed source range ${seam.sourceText}.`,
    }));

    const rawLoop = edit?.loop_range;
    const loopStart = Number(rawLoop?.start_ms);
    const loopEnd = Number(rawLoop?.end_ms);
    const loopValid = (
      Number.isFinite(loopStart)
      && Number.isFinite(loopEnd)
      && loopStart >= 0
      && loopEnd > loopStart
      && loopEnd <= originalDuration
      && cuts.every((cut) => cut.end <= loopStart || loopEnd <= cut.start)
    );
    const loop = loopValid ? {
      start: sourceToCompiled(loopStart),
      end: sourceToCompiled(loopEnd),
      sourceStart: loopStart,
      sourceEnd: loopEnd,
    } : null;
    if (loop) {
      loop.startPercent = percent(loop.start);
      loop.endPercent = percent(loop.end);
      loop.widthPercent = Math.max(0, loop.endPercent - loop.startPercent);
      loop.title = `Loop span mapped to compiled ${formatTime(loop.start)}–${formatTime(
        loop.end,
      )}; source ${formatTime(loop.sourceStart)}–${formatTime(loop.sourceEnd)}.`;
    }

    const durationTitle = `Edited playback · compiled one-pass duration ${formatTime(
      compiledDuration,
    )}; original duration ${formatTime(originalDuration)}.`;
    const markerTitle = [
      durationTitle,
      seams.length
        ? `Cut seams at compiled ${seams.map((seam) => `${formatTime(seam.position)} (source ${seam.sourceText})`).join(", ")}.`
        : "",
      loop?.title ?? "",
    ].filter(Boolean).join(" ");

    return {
      originalDuration,
      onePassDuration,
      compiledDuration,
      seams,
      loop,
      markerTitle,
    };
  }

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null
  );

  const localOutput = $derived(!playback.output_device_id);
  const effectiveEdit = $derived(localOutput ? current?.effective_edit ?? null : null);
  const editTimeline = $derived.by(() => makeEditTimeline(
    effectiveEdit,
    current?.duration_ms,
    playback.duration_ms,
  ));
  const editIndicator = $derived.by(() => {
    if (!effectiveEdit) return null;
    return {
      label: "Edited",
      title: editTimeline.markerTitle,
    };
  });

  /**
   * The track the bar is looking at, as the one thing the saved-in lookup
   * depends on.
   *
   * `current` can be a fresh row object after a queue edit. The URI is that
   * row's identity, so metadata updates never restart the lookup.
   */
  const currentUri = $derived(current?.uri ?? null);

  $effect(() => {
    if (!controls.library) {
      untrack(invalidateSavedIn);
      return;
    }
    const uri = currentUri;
    const account = session.username;
    untrack(() => {
      nowSaved.refs = [];
      lookupSavedIn(uri);
    });
  });
  $effect(() => watchPersonal());
  /* Every change to the index — a like here or elsewhere, a playlist edit —
     re-reads the mark, so it never waits for the next track change. */
  $effect(() => {
    if (!controls.library) return;
    const event = listen("memberships_changed", () => lookupSavedIn(currentUri));
    return () => event.then((off) => off()).catch(() => {});
  });

  /* Saved ANYWHERE — Liked Songs or a playlist of your own — is the check;
     saved nowhere is the heart outline. Both answered by the shell's index,
     which holds only Liked Songs and playlists you own. */
  const inLiked = $derived(nowSaved.refs.some((ref) => ref.id === "liked"));
  const savedLabel = $derived(`Saved in ${nowSaved.refs.map((ref) => ref.name).join(", ")}`);
  let savedMark = $state(null);
  let heart = $state(null);
  let liking = $state(false);

  /**
   * The "Saved in" panel opens while the pointer is over the check or the
   * panel, or the focus is in either, as a CSS :hover/:focus-within pair once
   * did — but it is a popover now, in the top layer, so the bar's own layer
   * and edge cannot clip it. It is placed once, above the check, when it
   * opens. Leaving waits a beat, so the pointer can cross the gap (the panel's
   * ::after bridges it); Escape shuts it until the pointer or the focus
   * leaves and comes back.
   */
  let savedGroup = $state(null);
  let savedPanel = $state(null);
  let savedHover = $state(false);
  let savedFocus = $state(false);
  let savedHushed = $state(false);
  let savedAt = $state({ left: 0, bottom: 0, bridge: 0 });
  const savedOpen = $derived((savedHover || savedFocus) && !savedHushed);

  /* Clear of the bar, 8px over its top edge, with the bridge reaching back
     down to the check. */
  function placeSaved() {
    const rect = savedMark?.getBoundingClientRect();
    const bar = savedMark?.closest(".player")?.getBoundingClientRect();
    if (!rect || !bar) return;
    const bottom = window.innerHeight - bar.top + 8;
    savedAt = { left: Math.max(8, rect.left - 8), bottom, bridge: window.innerHeight - bottom - rect.top };
  }

  $effect(() => {
    const group = savedGroup;
    if (!group) return;
    let leaveTimer = 0;
    const enter = () => {
      clearTimeout(leaveTimer);
      if (!savedHover) placeSaved();
      savedHover = true;
    };
    const leave = () => {
      clearTimeout(leaveTimer);
      leaveTimer = setTimeout(() => {
        savedHover = false;
        if (!savedFocus) savedHushed = false;
      }, 120);
    };
    // Native focus events can fire synchronously while Svelte removes a
    // focused saved-panel branch. They own focus state, not the rendering
    // reaction that happened to trigger the browser event.
    const focusIn = () => untrack(() => {
      if (!savedFocus && !savedHover) placeSaved();
      savedFocus = true;
    });
    const focusOut = (event) => untrack(() => {
      if (group.contains(event.relatedTarget)) return;
      savedFocus = false;
      if (!savedHover) savedHushed = false;
    });
    const key = (event) => {
      if (event.key !== "Escape" || !savedOpen) return;
      event.preventDefault();
      event.stopPropagation();
      savedHushed = true;
      if (group.contains(document.activeElement)) savedMark?.focus();
    };
    const resize = () => savedOpen && placeSaved();
    group.addEventListener("pointerenter", enter);
    group.addEventListener("pointerleave", leave);
    group.addEventListener("focusin", focusIn);
    group.addEventListener("focusout", focusOut);
    group.addEventListener("keydown", key);
    window.addEventListener("resize", resize);
    return () => {
      clearTimeout(leaveTimer);
      group.removeEventListener("pointerenter", enter);
      group.removeEventListener("pointerleave", leave);
      group.removeEventListener("focusin", focusIn);
      group.removeEventListener("focusout", focusOut);
      group.removeEventListener("keydown", key);
      window.removeEventListener("resize", resize);
      savedHover = savedFocus = savedHushed = false;
    };
  });

  $effect(() => {
    const panel = savedPanel;
    if (!panel) return;
    if (savedOpen) {
      if (!panel.matches(":popover-open")) panel.showPopover();
    } else if (panel.matches(":popover-open")) {
      panel.hidePopover();
    }
  });

  /** Likes or unlikes the playing track. Focus follows the mark it becomes,
      so a keyboard like does not drop the caret on the floor. */
  async function like(saved) {
    const uri = currentUri;
    if (!uri || liking || !personalConnected()) return;
    const hadFocus = document.activeElement === heart || !!savedMark?.parentElement?.contains(document.activeElement);
    liking = true;
    try {
      await setLiked([uri], saved);
    } catch (reason) {
      ui.error = `Could not ${saved ? "save to" : "remove from"} Liked Songs. ${reason instanceof Error ? reason.message : String(reason ?? "")}`.trim();
    } finally {
      liking = false;
    }
    if (!hadFocus || uri !== currentUri) return;
    await tick();
    (nowSaved.refs.length ? savedMark : heart)?.focus();
  }

  function openSaved(id) {
    if (id === "liked") navigate("liked");
    else navigate("playlist", id);
  }


  const HEART = "M12 20.2l-1.25-1.13C6.2 14.95 3.3 12.3 3.3 9.05 3.3 6.4 5.37 4.35 8 4.35c1.48 0 2.9.69 4 1.8 1.1-1.11 2.52-1.8 4-1.8 2.63 0 4.7 2.05 4.7 4.7 0 3.25-2.9 5.9-7.45 10.03L12 20.2Z";

  const pos = $derived(dragPos !== null ? dragPos : positionMs());
  // `track` is the engine's name for repeat-one; anything outside
  // off/context/track is rejected outright by the Rust command layer.
  function cycleRepeat() {
    const order = ["off", "context", "track"];
    const next = order[(order.indexOf(playback.repeat) + 1) % order.length];
    api.setRepeat(next).catch(() => {});
  }

  const repeatTitle = $derived(
    playback.repeat === "track"
      ? "Repeat one"
      : playback.repeat === "context"
        ? "Repeat all"
        : "Repeat off"
  );

  /**
   * Speed is held in HUNDREDTHS here, not as a float. The engine arms its
   * pitch-preserving stretcher on `speed != 1.0` exactly, so the one value
   * that must survive the UI unharmed is 1 - and 100/100 is exactly 1.0 in
   * binary, where accumulating 0.05 steps would not be. It also makes the
   * step grid, the clamp and the equality checks plain integer work.
   */
  const SPEED_MIN = 50;
  const SPEED_MAX = 400;
  const SPEED_STEP = 5;
  const SPEED_PRESETS = [75, 100, 150, 200];

  let speedDraft = $state(null);
  let speedOpen = $state(false);
  let speedButton = $state(null);
  let speedMenu = $state(null);
  let speedAnchor = $state({ left: 0, bottom: 0 });
  const SPEED_INTERVAL_MS = 80;
  let speedTimer = null;
  let pendingSpeed = null;
  let activeSpeed = null;
  let speedSentAt = -Infinity;
  let speedUrgent = false;
  let speedDisposed = false;
  let speedError = $state("");

  const speedPercent = $derived(
    localOutput ? speedDraft ?? Math.round((playback.playback_speed || 1) * 100) : 100
  );
  const speedLabel = $derived(formatSpeed(speedPercent));

  function formatSpeed(percent) {
    return (percent / 100).toFixed(2).replace(/0$/, "");
  }

  /* The widest label the readout can show, whose width it reserves. Its
     figures are tabular, so the longest label on the grid is the widest. */
  const SPEED_WIDEST = Array.from(
    { length: (SPEED_MAX - SPEED_MIN) / SPEED_STEP + 1 },
    (_, i) => formatSpeed(SPEED_MIN + i * SPEED_STEP),
  ).reduce((widest, label) => (label.length > widest.length ? label : widest));

  /** One in-flight command, with only the newest intent waiting behind it. */
  async function flushSpeed() {
    clearTimeout(speedTimer);
    speedTimer = null;
    if (speedDisposed || !localOutput || activeSpeed !== null || pendingSpeed === null) return;
    const delay = SPEED_INTERVAL_MS - (performance.now() - speedSentAt);
    if (!speedUrgent && delay > 0) {
      speedTimer = setTimeout(flushSpeed, delay);
      return;
    }
    const target = pendingSpeed;
    pendingSpeed = null;
    activeSpeed = target;
    speedUrgent = false;
    speedSentAt = performance.now();
    try {
      await api.setPlaybackSpeed(target / 100);
    } catch (error) {
      if (!speedDisposed && pendingSpeed === null) speedError = `Could not change speed: ${String(error)}`;
    } finally {
      activeSpeed = null;
      if (!speedDisposed) {
        if (pendingSpeed === null) speedDraft = null;
        else flushSpeed();
      }
    }
  }

  function commitSpeed(percent, final = false) {
    if (!localOutput || !Number.isFinite(percent)) return;
    const snapped = Math.round(percent / SPEED_STEP) * SPEED_STEP;
    const next = Math.min(SPEED_MAX, Math.max(SPEED_MIN, snapped));
    speedError = "";
    speedDraft = next;
    pendingSpeed = next === activeSpeed
      || (activeSpeed === null && next === Math.round(playback.playback_speed * 100)) ? null : next;
    speedUrgent = final;
    if (pendingSpeed === null && activeSpeed === null) speedDraft = null;
    flushSpeed();
  }

  $effect(() => {
    speedDisposed = false;
    return () => {
      speedDisposed = true;
      clearTimeout(speedTimer);
      pendingSpeed = null;
    };
  });

  $effect(() => {
    if (localOutput) return;
    speedOpen = false;
    speedDraft = null;
    pendingSpeed = null;
    clearTimeout(speedTimer);
  });

  function placeSpeedMenu() {
    const rect = speedButton?.getBoundingClientRect();
    if (!rect) return;
    speedAnchor = {
      left: rect.left + rect.width / 2,
      bottom: window.innerHeight - rect.top + 8,
    };
  }

  function toggleSpeedMenu() {
    if (!localOutput) return;
    speedOpen = !speedOpen;
    if (speedOpen) placeSpeedMenu();
  }

  /* Non-passive on both surfaces: a speed gesture must not scroll the page. */
  function speedWheel(node) {
    const onWheel = (event) => {
      if (!localOutput || event.ctrlKey || event.metaKey || !Number.isFinite(event.deltaY)
        || !event.deltaY || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
      event.preventDefault();
      untrack(() => commitSpeed(speedPercent + (event.deltaY < 0 ? SPEED_STEP : -SPEED_STEP)));
    };
    node.addEventListener("wheel", onWheel, { passive: false });
    return { destroy: () => node.removeEventListener("wheel", onWheel) };
  }

  $effect(() => {
    if (!speedOpen) return;
    const node = speedMenu;
    node?.showPopover?.();
    const reposition = () => placeSpeedMenu();
    window.addEventListener("resize", reposition);
    /* Light dismiss closes the popover without telling the component. */
    const onToggle = (event) => {
      if (event.newState === "closed") speedOpen = false;
    };
    node?.addEventListener("toggle", onToggle);
    return () => {
      window.removeEventListener("resize", reposition);
      node?.removeEventListener("toggle", onToggle);
      node?.hidePopover?.();
    };
  });
</script>

{#snippet glassHeart()}
  <!-- Drawn here rather than from the sprite, because its stroke is the
       glass: light at the top fading down its sides, over a dark hairline
       that keeps it on the brightest haze, the way a plane's rim does. -->
  <svg class="glass-heart" width="18" height="18" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
    <defs>
      <linearGradient id="glass-heart-light" x1="0" y1="0" x2="0" y2="1">
        <stop class="gh-top" offset="0" />
        <stop class="gh-foot" offset="1" />
      </linearGradient>
    </defs>
    <path class="gh-edge" d={HEART} />
    <path class="gh-light" d={HEART} stroke="url(#glass-heart-light)" />
  </svg>
{/snippet}

<div class="player glass-chrome" use:frost bind:this={playerBar}
  role="toolbar" aria-label="Playback controls" tabindex="0"
  oncontextmenu={openBarMenu} onkeydown={openBarMenu}>
  <div class="p-body" class:compact-now={!controls.metadata} class:no-now={!hasNow}
    class:no-art={!controls.artwork} class:no-mark={!hasMark}>
    {#if hasNow}
    <div class="p-now" class:idle={!current}>
      {#if current}
        {#if controls.artwork}
        <button
          class="p-art-btn"
          title={current.uri?.startsWith("spotify:episode:") ? "Go to podcast" : "Go to album"}
          onclick={() => current.album_id && navigate(current.uri?.startsWith("spotify:episode:") ? "show" : "album", current.album_id)}
        >
          <Cover
            src={current.cover_url}
            id={current.album_id || current.uri}
            name={current.album_name || current.name}
            size={48}
            class="p-art"
          />
        </button>
        {/if}
        {#if controls.metadata}
        <span class="p-meta">
          <span class="p-title-line">
            {#if current.album_id}
              <button
                class="p-title"
                title={current.uri?.startsWith("spotify:episode:") ? "Go to podcast" : "Go to album"}
                onclick={() => navigate(current.uri?.startsWith("spotify:episode:") ? "show" : "album", current.album_id)}
              >{current.name}</button>
            {:else}
              <span class="p-title">{current.name}</span>
            {/if}
          </span>
          {#if current.uri?.startsWith("spotify:episode:")}
            <button class="p-artists" disabled={!current.album_id} onclick={() => navigate("show", current.album_id)}>{current.album_name || current.artist_names?.[0]}</button>
          {:else}
            <ArtistLinks
              class="p-artists"
              names={current.artist_names}
              ids={current.artist_ids ?? []}
              id={current.artist_id}
            />
          {/if}
        </span>
        {/if}
        {#if controls.library}
        {#if nowSaved.refs.length}
          <!-- Marks live BESIDE the two-line text block, not inside its first
               line: .p-now centres its children, so the check faces the whole
               title+artists stack instead of hanging off the song name. -->
          <span class="p-saved" bind:this={savedGroup}>
            <button
              class="p-saved-trigger"
              bind:this={savedMark}
              aria-label={savedLabel}
              aria-expanded={savedOpen}
              aria-controls="p-saved-panel"
            >
              <span class="p-saved-mark"><Icon name="check" size={10} /></span>
            </button>
            <!-- A popover, so it is drawn in the top layer, over the bar's
                 edge and everything else, while staying next to the check in
                 the DOM: Tab walks from the check straight into its rows. -->
            <span
              id="p-saved-panel"
              class="p-saved-panel glass-overlay"
              popover="manual"
              role="group"
              aria-label={savedLabel}
              bind:this={savedPanel}
              style:left="{savedAt.left}px"
              style:bottom="{savedAt.bottom}px"
              style:--bridge="{savedAt.bridge}px"
            >
              <span class="p-saved-scroll" use:scrollbar>
                <span class="p-saved-head">Saved in</span>
                {#each nowSaved.refs as ref (ref.id)}
                  <button
                    class="p-saved-row"
                    title="Open {ref.name}"
                    onclick={() => openSaved(ref.id)}
                  >{ref.name}</button>
                {/each}
                {#if inLiked && personalConnected()}
                  <span class="p-saved-sep" aria-hidden="true"></span>
                  <button class="p-saved-row p-saved-remove" disabled={liking} onclick={() => like(false)}>
                    Remove from Liked Songs
                  </button>
                {/if}
              </span>
            </span>
          </span>
        {:else if current.uri?.startsWith("spotify:track:")}
          <!-- Saved nowhere: the heart as an outline of glass, and nothing
               else — no plate, no ring. It is a state first; with the personal
               app connected it is also the way to like the song. -->
          {#if personalConnected()}
            <button
              class="p-heart"
              bind:this={heart}
              disabled={liking}
              title="Save to Liked Songs"
              aria-label="Save to Liked Songs"
              onclick={() => like(true)}
            >{@render glassHeart()}</button>
          {:else}
            <span
              class="p-heart inert"
              role="img"
              aria-label="Not in Liked Songs"
              title="Not in Liked Songs. Liking needs your Spotify developer app, in Settings."
            >{@render glassHeart()}</span>
          {/if}
        {/if}
        {/if}
        {#if controls.metadata && editIndicator}
          <span
            class="p-edit-indicator"
            title={editIndicator.title}
            aria-label={editIndicator.title}
          >
            <span class="p-edit-mark" aria-hidden="true"></span>
            {editIndicator.label}
          </span>
        {/if}
      {:else}
        <!-- Idle holds the same 48px slot, so the bar does not jump the moment
             the first track lands. -->
        {#if controls.artwork}
        <span class="art p-art" aria-hidden="true"></span>
        {/if}
        {#if controls.metadata}
        <span class="p-meta">
          <span class="p-title">Nothing playing</span>
          <span class="p-artists">Pick something from your library</span>
        </span>
        {/if}
      {/if}
    </div>
    {/if}

    <div class="p-center">
      <div class="transport">
        <button
          class="ctl"
          class:on={playback.shuffle}
          title={playback.shuffle ? "Disable shuffle" : "Enable shuffle"}
          onclick={() => api.setShuffle(!playback.shuffle).catch(() => {})}
        >
          <Icon name="shuffle" size={17} />
        </button>
        <button class="ctl" title="Previous" onclick={() => api.previous().catch(() => {})}>
          <Icon name="previous" size={19} />
        </button>
        <button
          class="play-btn"
          title={playback.playing ? "Pause" : "Play"}
          onclick={togglePlay}
          disabled={!playback.queue.length}
        >
          <Icon name={playback.playing ? "pause" : "play"} size={20} />
        </button>
        <button class="ctl" title="Next" onclick={() => api.next().catch(() => {})}>
          <Icon name="next" size={19} />
        </button>
        <button class="ctl" class:on={playback.repeat !== "off"} title={repeatTitle} onclick={cycleRepeat}>
          <Icon name={playback.repeat === "track" ? "repeat-one" : "repeat"} size={17} />
        </button>
      </div>

      <div class="p-seek">
        <span class="p-time l">{formatTime(pos)}</span>
        <div
          class="p-seek-slider"
          title={effectiveEdit ? editTimeline.markerTitle : undefined}
        >
          <Slider
            min={0}
            max={playback.duration_ms || 0}
            value={positionMs()}
            label="Seek"
            step={5000}
            formatValue={formatTime}
            gestureKey={playbackIdentity}
            onCommit={(v) => {
              dragPos = null;
              api.seek(v).catch(() => {});
            }}
            onDragStart={(v) => (dragPos = v)}
            onDragChange={(v) => (dragPos = v)}
            onDragCancel={() => (dragPos = null)}
          />
          {#if effectiveEdit}
            <span class="p-seek-markers" aria-hidden="true">
              {#if editTimeline.loop && editTimeline.loop.widthPercent > 0}
                <span
                  class="p-loop-band"
                  style:left="{editTimeline.loop.startPercent}%"
                  style:width="{editTimeline.loop.widthPercent}%"
                  title={editTimeline.loop.title}
                ></span>
              {/if}
              {#each editTimeline.seams as seam (seam.percent)}
                <span
                  class="p-cut-seam"
                  style:left="{seam.percent}%"
                  title={seam.title}
                ></span>
              {/each}
            </span>
          {/if}
        </div>
        <span class="p-time r">{formatTime(playback.duration_ms)}</span>
      </div>
    </div>

    <div class="p-right">
      {#if controls.speed}
      <button
        class="p-speed"
        class:on={speedPercent !== 100}
        bind:this={speedButton}
        use:speedWheel
        disabled={!localOutput}
        title={localOutput ? "Playback speed — scroll to adjust, double-click to reset" : "Spotify devices play original audio at normal speed"}
        aria-label="Playback speed"
        aria-expanded={speedOpen}
        onclick={toggleSpeedMenu}
        ondblclick={() => commitSpeed(100, true)}
        data-widest="{SPEED_WIDEST}×"
      >
        <span>{speedLabel}×</span>
      </button>
      <div
        class="speed-menu glass-overlay"
        popover="auto"
        bind:this={speedMenu}
        style:left="{speedAnchor.left}px"
        style:bottom="{speedAnchor.bottom}px"
      >
        <div class="speed-head">
          <span class="speed-value">{speedLabel}×</span>
        </div>
        <div class="speed-slider" use:speedWheel>
          <Slider
            min={SPEED_MIN}
            max={SPEED_MAX}
            value={speedPercent}
            label="Playback speed"
            step={SPEED_STEP}
            kind="speed"
            formatValue={(v) => formatSpeed(Math.round(v / SPEED_STEP) * SPEED_STEP) + "×"}
            onDragStart={(v) => commitSpeed(v)}
            onDragChange={(v) => commitSpeed(v)}
            onCommit={(v) => commitSpeed(v, true)}
          />
        </div>
        <div class="speed-presets">
          {#each SPEED_PRESETS as preset}
            <button
              class="speed-preset"
              class:on={speedPercent === preset}
              onclick={() => commitSpeed(preset, true)}
            >
              {formatSpeed(preset)}×
            </button>
          {/each}
        </div>
        {#if speedError}<p class="speed-error" role="status">{speedError}</p>{/if}
      </div>
      {/if}
      <DevicePicker />
      <!-- With no room the panel steps aside on its own, and the button waits
           for the room rather than changing what was asked for. -->
      <button
        class="btn-round"
        class:on={nowPlayingShown()}
        disabled={!nowPlayingPanel.fits}
        title={nowPlayingPanel.fits ? "Now playing details" : "Now playing needs a wider window"}
        onclick={() => morphLayout(() => setNowPlayingOpen(!ui.nowPlayingOpen))}
      >
        <Icon name="panel" size={18} />
      </button>
      <div class="p-volume" class:no-mute={!controls.mute} bind:this={volumeControl}>
        {#if controls.mute}
        <button
          class="btn-round"
          class:on={volumePercent === 0}
          title={volumePercent === 0 ? `Unmute (${restoreVolume}%)` : `Mute (${volumePercent}%)`}
          aria-label={volumePercent === 0 ? `Unmute to ${restoreVolume}%` : "Mute"}
          aria-pressed={volumePercent === 0}
          onclick={toggleMute}
        >
          <Icon name="volume" size={18} />
        </button>
        {/if}
        <Slider
          min={0}
          max={100}
          value={volumePercent}
          label="Volume"
          step={5}
          kind="vol"
          formatValue={(v) => `${Math.round(v)}%`}
          onDragStart={(v) => changeVolume(v)}
          onDragChange={(v) => changeVolume(v)}
          onCommit={(v) => changeVolume(v, true)}
        />
        {#if volumeError}
          <span class="p-volume-error glass-overlay" role="status">{volumeError}</span>
        {/if}
      </div>
    </div>
  </div>
</div>

{#if barMenu}
  <Menu at={barMenu.at} returnTo={playerBar} label="Player bar controls" onclose={() => (barMenu = null)}>
    {#each CONTROL_OPTIONS as option, i (option.key)}
      {#if i === 2}<div class="menu-sep" role="separator"></div>{/if}
      <button class="menu-item" role="menuitemcheckbox" aria-checked={controls[option.key]}
        onclick={() => toggleControl(option.key)}>
        <Icon name={option.icon} size={16} /><span class="menu-label">{option.label}</span>
        <span class="mark control-check">{#if controls[option.key]}<Icon name="check" size={14} />{/if}</span>
      </button>
    {/each}
  </Menu>
{/if}

<style>
  .control-check { width: 14px; height: 14px; }
  .p-volume {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s3);
    flex: none;
  }
  .p-volume-error {
    position: absolute;
    right: 0;
    bottom: calc(100% + 8px);
    width: max-content;
    max-width: 280px;
    padding: var(--s2) var(--s3);
    border-radius: var(--r2);
    color: var(--fg-1);
    font-size: var(--t-12);
  }
  /* The speed is a control like its neighbours, not a chip among them: the
     same 32px round target, the same bare glyph-weight at rest and the same
     plate under the pointer as the icon buttons — only its glyph is a
     number. Off 1.0 it takes the foam and the dot every other "on" control
     in the bar wears. The widest label stands invisible under the live one,
     so the control's width never follows the value. */
  .p-speed {
    position: relative;
    display: inline-grid;
    place-items: center;
    height: 32px;
    padding: 0 var(--s2);
    border-radius: var(--rf);
    color: var(--fg-2);
    font-family: var(--font-number);
    font-size: var(--t-12);
    font-weight: var(--w-med);
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.01em;
    cursor: pointer;
    transition:
      color var(--d1) var(--ease),
      background-color var(--d1) var(--ease);
  }
  .p-speed::before, .p-speed > span { grid-area: 1 / 1; }
  .p-speed::before { content: attr(data-widest); visibility: hidden; }
  .p-speed:hover:not(:disabled) { color: var(--fg); background: var(--hover-2); }
  .p-speed[aria-expanded="true"] { color: var(--fg); background: var(--hover-2); }
  .p-speed.on, .p-speed.on:hover:not(:disabled) { color: var(--accent); }
  .p-speed.on::after {
    content: ""; position: absolute; bottom: 1px; left: 50%; margin-left: -1.5px;
    width: 3px; height: 3px; border-radius: 50%; background: var(--accent);
  }

  /* Top layer, so the panel is never clipped by the player bar's own
     stacking context - the same escape the row menu uses. */
  .speed-menu {
    position: fixed;
    inset: auto auto auto 0;
    transform: translateX(-50%);
    margin: 0;
    padding: var(--s3) var(--s3) var(--s1);
    width: 280px;
    border: 0;
    border-radius: var(--r3);
    color: var(--fg-1);
  }
  .speed-slider { display: flex; }
  .speed-error { margin: var(--s2) 0; color: var(--love); font-size: var(--t-11); line-height: 1.4; }
  .p-title-line {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    min-width: 0;
  }
  .p-title-line .p-title {
    min-width: 0;
    flex: 1;
  }
  .p-edit-indicator {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    padding: 2px 5px;
    border: 1px solid color-mix(in srgb, var(--gold) 34%, transparent);
    border-radius: var(--rf);
    background: color-mix(in srgb, var(--gold) 7%, transparent);
    color: color-mix(in srgb, var(--gold) 78%, var(--fg-1));
    font-family: var(--font-small);
    font-size: 10px;
    font-weight: var(--w-semi);
    letter-spacing: 0.04em;
    line-height: 1.15;
    white-space: nowrap;
  }
  .p-edit-mark {
    width: 5px;
    height: 5px;
    flex: none;
    border: 1px solid currentColor;
    border-radius: 50%;
  }

  /* Saved-in mark: a quiet rose check that opens the list of the user's
     containers holding this track. Rose, not foam, because the mark is a
     membership statement - "this song lives in your playlists" is what is
     yours. Hover or keyboard focus opens its panel, overlay glass in the
     top layer, like the speed menu. */
  .p-saved {
    position: relative;
    display: inline-flex;
    flex: none;
    align-items: center;
    align-self: center;
    cursor: default;
    outline: none;
  }
  .p-saved-trigger {
    display: grid; place-items: center;
    width: 32px; height: 32px;
    border-radius: var(--rf);
  }
  .p-saved-mark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    border-radius: var(--rf);
    /* A SOLID rose disc with the mark punched out in ground, not a tinted
       ghost of one: a 12px glyph on a 14% wash read as a smudge. Filled is
       how the app already says "yours" at small scale (the liked heart),
       and the dark punch-out keeps it crisp instead of muddy. */
    color: var(--bg-0);
    background: var(--rose-ink);
    transition:
      background var(--d1) var(--ease),
      box-shadow var(--d1) var(--ease);
  }
  .p-saved:hover .p-saved-mark,
  .p-saved:focus-within .p-saved-mark {
    background: color-mix(in srgb, var(--rose-ink) 82%, #ffffff);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--rose-ink) 22%, transparent);
  }
  /* Overlay glass (.glass-overlay) in the top layer, placed in window
     coordinates by placeSaved; the list scrolls inside it, so its overlay
     bar is laid inside the glass. `inset`, `margin`, `border`, `padding` and
     `overflow` undo the UA's [popover] rule, which centres a popover and
     clips it — the bridge below hangs outside the box. It eases in and out
     like the CSS panel it replaced; the exit runs because `display` and the
     top layer are held for the transition. */
  .p-saved-panel {
    position: fixed;
    inset: auto;
    margin: 0;
    padding: 0;
    border: 0;
    overflow: visible;
    min-width: 200px;
    max-width: 280px;
    border-radius: var(--r3);
    transition:
      opacity var(--d1) var(--ease),
      transform var(--d1) var(--ease),
      overlay var(--d1) allow-discrete,
      display var(--d1) allow-discrete;
  }
  .p-saved-panel:not(:popover-open) {
    opacity: 0;
    transform: translateY(4px);
  }
  @starting-style {
    .p-saved-panel:popover-open {
      opacity: 0;
      transform: translateY(4px);
    }
  }
  .p-saved-scroll {
    display: flex;
    flex-direction: column;
    gap: 1px;
    max-height: 232px;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: var(--s3);
  }
  /* Invisible bridge across the gap, so the pointer can travel from the
     mark into the panel without the hover chain breaking mid-way. (After,
     not before: the glass rim is the before.) */
  .p-saved-panel::after {
    content: "";
    position: absolute;
    bottom: calc(var(--bridge, 8px) * -1);
    left: 0;
    right: 0;
    height: var(--bridge, 8px);
  }
  .p-saved-head {
    margin-bottom: var(--s2);
    color: var(--fg-3);
    font-family: var(--font-small);
    font-size: 10px;
    font-weight: var(--w-semi);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .p-saved-row {
    overflow: hidden;
    padding: 3px 6px;
    margin: 0 -6px;
    border: 0;
    border-radius: var(--r2);
    background: none;
    color: var(--fg-1);
    font: inherit;
    font-size: var(--t-12);
    line-height: 1.35;
    text-align: left;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    transition:
      color var(--d1) var(--ease),
      background var(--d1) var(--ease);
  }
  .p-saved-row:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.08);
    color: var(--fg);
  }
  /* The unsaved heart: a 32px target around an 18px glyph, and no surface of
     its own — the glyph IS the glass. Its light comes up a step under the
     pointer and warms toward the ink of what is yours. Static at rest. */
  .p-heart {
    display: grid; place-items: center; flex: none;
    width: 32px; height: 32px;
    --gh-top: rgb(255 255 255 / 0.82);
    --gh-foot: rgb(255 255 255 / 0.34);
  }
  .p-heart.inert { cursor: default; }
  button.p-heart:hover:not(:disabled), button.p-heart:focus-visible {
    --gh-top: color-mix(in srgb, var(--rose-ink) 55%, #ffffff);
    --gh-foot: color-mix(in srgb, var(--rose-ink) 70%, transparent);
  }
  button.p-heart:active:not(:disabled) .glass-heart { transform: scale(0.92); }
  button.p-heart:disabled { opacity: 0.5; }
  .glass-heart { display: block; overflow: visible; transition: transform var(--d1) var(--ease); }
  .gh-top { stop-color: var(--gh-top); transition: stop-color var(--d1) var(--ease); }
  .gh-foot { stop-color: var(--gh-foot); transition: stop-color var(--d1) var(--ease); }
  .gh-edge { fill: none; stroke: rgb(0 0 0 / 0.32); stroke-width: 3.2; stroke-linejoin: round; }
  .gh-light { fill: none; stroke-width: 1.7; stroke-linejoin: round; }

  .p-saved-sep { height: 1px; margin: var(--s2) 0 var(--s1); background: rgba(255, 255, 255, 0.08); }
  .p-saved-row.p-saved-remove { color: var(--fg-2); }
  .p-saved-row.p-saved-remove:hover:not(:disabled) { color: var(--fg); }

  .p-seek-slider {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
    height: 12px;
  }
  .p-seek-markers {
    position: absolute;
    inset: 0;
    z-index: 2;
    pointer-events: none;
  }
  .p-loop-band {
    position: absolute;
    top: 50%;
    height: 8px;
    transform: translateY(-50%);
    border: 1px solid color-mix(in srgb, var(--gold) 48%, transparent);
    border-radius: var(--rf);
    background: color-mix(in srgb, var(--gold) 20%, transparent);
  }
  .p-cut-seam {
    position: absolute;
    top: 50%;
    width: 1px;
    height: 12px;
    transform: translate(-50%, -50%);
    background: color-mix(in srgb, var(--gold) 82%, var(--fg));
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--bg-0) 75%, transparent);
  }
  .speed-menu:not(:popover-open) {
    display: none;
  }
  .speed-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--s3);
    margin-bottom: var(--s2);
  }
  .speed-value {
    font-size: var(--t-15);
    font-variant-numeric: tabular-nums;
  }
  /* The presets are menu items laid in a row: the same plate lit under the
     pointer, the same step from --fg-1 to --fg. The current one wears foam. */
  .speed-presets {
    display: flex;
    gap: 2px;
    margin: var(--s2) calc(var(--s2) * -1) 0;
  }
  .speed-preset {
    flex: 1;
    height: 36px;
    border-radius: var(--r2);
    color: var(--fg-1);
    font-size: var(--t-12);
    font-variant-numeric: tabular-nums;
    transition: background-color var(--d1) var(--ease), color var(--d1) var(--ease);
  }
  .speed-preset:hover,
  .speed-preset:focus-visible {
    background: rgba(255, 255, 255, 0.08);
    color: var(--fg);
    outline: none;
  }
  .speed-preset.on {
    color: var(--accent);
  }
</style>
