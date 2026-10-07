<script>
  import { untrack } from "svelte";
  import {
    playback,
    ui,
    navigate,
    openCredits,
    trackCredits,
    loadTrackCredits,
    appSettings,
    canvasLookups,
  } from "../lib/state.svelte.js";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import Icon from "./Icon.svelte";
  import WindowControls from "./WindowControls.svelte";
  import { drawsWindowControls, frame } from "../lib/window-frame.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { haze } from "../lib/ambient.svelte.js";
  import { panel as panelState } from "../lib/panel.svelte.js";
  import { formatTime } from "../lib/time.js";
  import { scrollbar } from "../lib/scrollbar.js";
  import { createCanvasOwner } from "../lib/canvas.js";

  let { open } = $props();

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const next = $derived(playback.queue[playback.upcoming?.[0]] ?? null);
  const playCountFormatter = new Intl.NumberFormat();

  /* ---- The picture -----------------------------------------------------
     One layout. The picture is the Canvas when this record has one and it
     has decoded, filling the panel; otherwise — no Canvas or the cover
     asked for — it is the cover in the hero. Changing between them
     is a crossfade of two elements that never move; so is changing from
     one record's Canvas to the next one's.

     It changes once per record, with the haze (canvas.js): while what the
     record is lit from is being decided, the picture is the last record's,
     loop or cover, and the copy under it is already the new record's. */
  /** A Spotify track's id: what its Canvas is asked and kept by. */
  const trackId = (uri) => (uri?.startsWith("spotify:track:") ? uri.slice("spotify:track:".length) : "");
  const canvasTrackKey = $derived(trackId(current?.uri));
  /** The record the engine is fetching ahead, near the end of this one. */
  const aheadKey = $derived(trackId(playback.preload_uri));
  /** This record's cover, as the hero draws it. */
  const recordCover = $derived(current && {
    src: current.cover_url,
    id: current.album_id || current.uri,
    name: current.album_name || current.name,
  });
  /** This record's Canvas: the source of its loop. */
  let canvasUrl = $state("");
  /** What the haze is lit from for this record (canvas.js). */
  let hazeSource = $state("cover");
  /** The loop loaded ahead, for the record the engine is fetching ahead. */
  let warmUrl = $state("");
  /** What the panel shows (canvas.js): { loop, cover, swap }. */
  let shown = $state.raw({ loop: "", cover: null, swap: false });
  /** The outgoing cover stays intact until its fade finishes. */
  let leavingCover = $state.raw(null);
  const artCover = $derived(leavingCover ?? (videoShown ? null : shown.cover));
  const heroCover = $derived(artCover ?? shown.cover ?? recordCover);
  /** The hero's copy has scrolled up under the head. */
  let headOver = $state(false);
  let documentVisible = $state(!document.hidden);
  const pageVisible = $derived(documentVisible && !frame.minimized);
  let reducedMotion = $state(false);

  /** Every loop the panel holds, by source. */
  let loops = $state.raw({});
  /** The loop on screen; this record's, or the last one's while it is held. */
  const frontEl = $derived(loops[shown.loop] ?? null);
  let panelEl = $state(null);
  let scrollEl = $state(null);
  let detailsEl = $state(null);
  let headSentinel = $state(null);

  /** A loop is the picture. */
  const videoShown = $derived(Boolean(shown.loop));
  /** The cover-or-Canvas choice, there only while there is one. */
  const canSwap = $derived(Boolean(current) && shown.swap);
  /** The head holds that choice and, on Windows, the window's controls. */
  const hasHead = $derived(canSwap || drawsWindowControls);

  /* The haze lays out differently when the video covers the panel: what is
     under it is never seen. Written from an effect so every path in and out
     — unmounting included — lands here once. */
  $effect(() => {
    ui.immersive = open && videoShown;
  });
  $effect(() => () => {
    ui.immersive = false;
    haze.video = null;
    haze.anchor = null;
    haze.awaiting = false;
  });

  $effect(() => {
    const media = window.matchMedia("(prefers-reduced-motion: reduce)");
    const updateMotion = () => (reducedMotion = media.matches);
    const updateVisibility = () => (documentVisible = !document.hidden);
    updateMotion();
    updateVisibility();
    media.addEventListener?.("change", updateMotion);
    document.addEventListener("visibilitychange", updateVisibility);
    return () => {
      media.removeEventListener?.("change", updateMotion);
      document.removeEventListener("visibilitychange", updateVisibility);
    };
  });



  /* Canvas is an opt-in decoder, so it is asked for only while someone can
     see the answer: never with the setting off or reduced motion asked for,
     and not while the window is backgrounded — a track that changed while
     hidden is asked about when the window comes back. The one record asked
     about ahead is the one the engine itself is fetching ahead, in the last
     seconds of a track; its loop is loaded then too, so the advance goes
     straight from this Canvas to that one.

     `haze.awaiting` holds the haze on the last record's light while this
     record's source is decided, briefly and once (canvas.js), so a change
     of record is one change of the window's light. */
  const canvasOwner = createCanvasOwner({
    lookups: canvasLookups,
    decoded: (source) => (loops[source]?.readyState ?? 0) >= 2,
    changed: (picture) => {
      canvasUrl = picture.url;
      hazeSource = picture.haze;
      warmUrl = picture.warm;
      const next = picture.shown;
      if (next.loop !== shown.loop) {
        clearTimeout(leaveTimer);
        leaving = shown.loop;
        leavingUnder = Boolean(next.loop);
        leavingCover = next.loop && !shown.loop ? shown.cover : null;
        leaveTimer = leaving || leavingCover ? setTimeout(() => {
          leaving = "";
          leavingCover = null;
          leaveTimer = 0;
        }, CROSSFADE_MS) : 0;
      }
      shown = next;
      haze.awaiting = open && picture.haze === "hold";
    },
  });
  $effect(() => {
    const update = {
      key: canvasTrackKey,
      cover: recordCover,
      enabled: appSettings.animated_canvas && !reducedMotion,
      visible: open && pageVisible,
      ahead: aheadKey,
    };
    untrack(() => canvasOwner.update(update));
  });
  $effect(() => () => canvasOwner.dispose());

  /* ---- The loops ---------------------------------------------------------
     One <video> per source: the one on screen, this record's (loading under
     a held picture), the one warmed ahead, and for the length of a
     crossfade the one it replaced. A loop warmed ahead that becomes this
     record's is the same element, already decoded. The loop that leaves
     stays whole under the one that arrives while it fades in
     (.np-video.under) — two loops fading against each other would let the
     haze show through halfway — and fades out itself only to the cover;
     then its element goes, and its decoder with it. */
  /** .np-video's crossfade (app.css). */
  const CROSSFADE_MS = 600;
  let leaving = $state("");
  let leavingUnder = $state(false);
  let leaveTimer = 0;
  $effect(() => () => clearTimeout(leaveTimer));
  const sources = $derived([...new Set([leaving, shown.loop, canvasUrl, warmUrl])].filter(Boolean));

  /** Files a loop's element under its source. One let go is unloaded at
      once, rather than holding its decoder until some collection. */
  function holdLoop(node, source) {
    loops = { ...loops, [source]: node };
    return {
      destroy() {
        if (loops[source] === node) {
          const { [source]: _, ...rest } = loops;
          loops = rest;
        }
        node.removeAttribute("src");
        node.load();
      },
    };
  }

  function handleCanvasReady(event) {
    canvasOwner.arrive(event.currentTarget.getAttribute("src"));
  }

  function handleCanvasError(event) {
    if (event.currentTarget.error?.code === MediaError.MEDIA_ERR_ABORTED) return;
    canvasOwner.fail(event.currentTarget.getAttribute("src"));
  }

  /** Cover or Canvas: a crossfade, nothing else. */
  function swapSurface() {
    canvasOwner.swap();
  }

  function openAlbum(event) {
    if (event.type === "keydown" && event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    if (current?.album_id) navigate(current.uri?.startsWith("spotify:episode:") ? "show" : "album", current.album_id);
  }

  /**
   * Whether the Canvas decoder may run.
   *
   * A Canvas is the record's motion, so it moves with the record: it plays
   * while the music plays and stands still while it is paused, which is
   * also what Spotify does. Whatever lies over it — the details' glass, a
   * dialog — frosts it without stopping it. Beyond that it stops only where
   * nobody can see it at all: the window blurred or hidden, or the cover
   * shown instead. Closing the panel pauses and retains its last picture,
   * so reopening can hold it while the next Canvas loads.
   */
  const canvasPlaying = $derived(open && videoShown && pageVisible && ui.windowFocused && playback.playing);

  /* Only the loop on screen plays: one loading or warmed waits on its first
     frame, and the one leaving is gone in a moment. A held loop plays on
     while it is held, as it did before the change. */
  $effect(() => {
    const front = frontEl;
    const playing = canvasPlaying;
    for (const video of Object.values(loops)) {
      if (video === front && playing) {
        if (video.paused) video.play().catch(() => {});
      } else if (!video.paused) {
        video.pause();
      }
    }
  });

  /* The haze is drawn from this record's loop while it is the picture
     here, paused or not (a paused frame is simply a still haze) — unless
     the haze was lit from the cover before the loop arrived, which it then
     keeps. */
  $effect(() => {
    haze.video = open && hazeSource === "video" && canvasUrl && shown.loop === canvasUrl ? frontEl : null;
  });

  /* ---- Where the Canvas sits -------------------------------------------
     The video covers the panel, centred, cropped evenly — but a Canvas is a
     few hundred pixels of compressed video, and drawn much larger than
     that it goes blocky. So it never grows past CANVAS_LIMIT device pixels
     to each of its own. On a window taller (or, for a small Canvas, wider)
     than that, it stops growing, centred in the panel, and each edge that
     no longer reaches the panel's dissolves into the haze — the Canvas's
     own light — over a distance that grows with the gap it borders, so the
     change comes on from nothing as the window is dragged, never as a jump.

     Published as haze.anchor.video: the part of the video on screen, in
     window pixels, for the haze to carry its colour on from; and with it
     the panel's left edge. */
  const CANVAS_LIMIT = 1.2;
  /** The longest an edge dissolve runs. */
  const EDGE_FADE = 96;

  let stageEl = $state(null);
  let pixelRatio = $state(window.devicePixelRatio || 1);

  /* Dragging the window to another screen changes the ratio. */
  $effect(() => {
    let query = null;
    const update = () => {
      pixelRatio = window.devicePixelRatio || 1;
      query?.removeEventListener("change", update);
      query = window.matchMedia(`(resolution: ${pixelRatio}dppx)`);
      query.addEventListener("change", update);
    };
    update();
    return () => query?.removeEventListener("change", update);
  });

  const round = (v) => Math.round(v * 100) / 100;

  /** Sizes and centres one loop in the panel's box; where it went, or null
      before its size is known. */
  function fitLoop(el, box) {
    const nw = el.videoWidth;
    const nh = el.videoHeight;
    if (!nw || !nh) return null;
    const scale = Math.min(CANVAS_LIMIT / pixelRatio, Math.max(box.width / nw, box.height / nh));
    const w = nw * scale;
    const h = nh * scale;
    const sx = (box.width - w) / 2;
    const sy = (box.height - h) / 2;
    Object.assign(el.style, {
      left: `${round(sx)}px`,
      top: `${round(sy)}px`,
      width: `${round(w)}px`,
      height: `${round(h)}px`,
    });
    return { sx, sy, w, h };
  }

  /* Every loop the panel holds is placed, so the next one is already where
     it will be; the one on screen also sets the edge dissolve and is what
     the haze carries on from. */
  function placeCanvas() {
    if (!open) {
      haze.anchor = null;
      return;
    }
    const panel = panelEl;
    const stage = stageEl;
    if (!panel || !stage) return;
    const box = panel.getBoundingClientRect();
    const left = box.left / window.innerWidth;
    let fit = null;
    for (const [source, el] of Object.entries(loops)) {
      const placed = fitLoop(el, box);
      if (source === shown.loop) fit = placed;
    }
    let visible = null;
    if (fit) {
      const { sx, sy, w, h } = fit;
      const floatX = sx > 0.5;
      const floatY = sy > 0.5;
      stage.classList.toggle("float", floatX || floatY);
      if (floatX || floatY) {
        const dx = floatX ? Math.min(EDGE_FADE, sx * 3, w * 0.2) : 0;
        const dy = floatY ? Math.min(EDGE_FADE, sy * 3, h * 0.2) : 0;
        const vars = {
          "--x0": `${floatX ? round(sx) : -1}px`,
          "--x1": `${round(floatX ? sx + w : box.width + 1)}px`,
          "--y0": `${floatY ? round(sy) : -1}px`,
          "--y1": `${round(floatY ? sy + h : box.height + 1)}px`,
          "--dx": `${round(dx)}px`,
          "--dy": `${round(dy)}px`,
        };
        for (const [name, value] of Object.entries(vars)) stage.style.setProperty(name, value);
      }
      visible = {
        x: round(box.left + Math.max(0, sx)),
        y: round(box.top + Math.max(0, sy)),
        w: round(Math.min(w, box.width)),
        h: round(Math.min(h, box.height)),
      };
    }
    /* Only a video on screen has colour to carry on (`fit` is the one on
       screen). And while the edge is being dragged the haze is slid, not
       redrawn: the anchor is published once, when it is let go
       (ambient.svelte.js, THE FROST, 5). */
    if (panelState.resizing) return;
    const last = haze.anchor;
    const moved =
      !last ||
      Math.abs(last.left - left) > 0.0005 ||
      Boolean(last.video) !== Boolean(visible) ||
      (visible && ["x", "y", "w", "h"].some((k) => Math.abs(last.video[k] - visible[k]) > 0.25));
    if (moved) haze.anchor = { left, video: visible };
  }

  /* Placed again whenever one of its inputs moves: the panel's box (the
     window, the grid) through the observer and the resize event; and here
     the ratio, the element, and whether it is shown. The video's own size
     arrives with its metadata (onloadedmetadata). */
  $effect(() => {
    const panel = panelEl;
    if (!open || !panel || !stageEl) return;
    const place = () => untrack(placeCanvas);
    const observer = new ResizeObserver(place);
    observer.observe(panel);
    window.addEventListener("resize", place);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", place);
    };
  });
  $effect(() => {
    open;
    pixelRatio;
    loops;
    shown.loop;
    panelState.resizing;
    untrack(placeCanvas);
  });

  /* A sentinel in the scrolled content, the app's usual device: it fires
     once crossing and never in between. No scroll listener, and no scroll
     timeline — the latter is banned in this app (see app.css).

     It is a tall strip that ENDS at its line and reaches up past the top of
     the content, so it can only ever be intersecting or above — never
     below. That matters: an observer reports changes of intersection only,
     and a one-pixel marker that a jump (a scrollbar drag, End, a fling)
     carries straight from below the viewport to above it never intersects
     on the way, and never reports at all. */
  function watchPassed(target, rootMargin, onChange) {
    const root = scrollEl;
    if (!root || !target) return undefined;
    const observer = new IntersectionObserver(([entry]) => onChange(!entry.isIntersecting), {
      root,
      rootMargin,
    });
    observer.observe(target);
    return () => observer.disconnect();
  }
  $effect(() => watchPassed(headSentinel, "-56px 0px 0px 0px", (v) => (headOver = v)));

  function revealDetails() {
    const top = (detailsEl?.offsetTop ?? 0) - 72;
    scrollEl?.scrollTo({ top, behavior: reducedMotion ? "instant" : "smooth" });
  }

  /* Credits load only while the panel is open, cached per track id. */
  $effect(() => {
    const track = current;
    if (open && pageVisible && track?.uri?.startsWith("spotify:track:")) untrack(() => loadTrackCredits(track));
  });

  /** Groups worth showing inline; the rest live behind "all credits". */
  const PANEL_GROUPS = 3;
  const groups = $derived(trackCredits.data?.groups ?? []);
  const shownGroups = $derived(groups.slice(0, PANEL_GROUPS));
  const contributorTotal = $derived(
    groups.reduce((sum, g) => sum + (g.contributors?.length ?? 0), 0),
  );
  /** How many names each group shows before it starts counting the remainder. */
  const PANEL_NAMES = 4;

  /** The record's colour: the light the cover in the hero casts. */
  const tone = $derived(coverTone(heroCover?.src ?? "", heroCover?.id ?? ""));

  /* How much help the type needs, from the light the haze measured under
     it (1 until measured: unknown is white). The hero's glyph shadow grows
     from nothing on a dark picture to full at about the brightness where
     bare white type would fall under 3:1; the head's plates thicken the
     same way. Rounded, so a measurement that moves a little restyles
     nothing. */
  const shade = $derived(Math.round(Math.min(1, Math.max(0, (haze.lightBottom - 0.03) / 0.27)) * 20) / 20);
  const plateAlpha = $derived(Math.round((0.4 + 0.24 * Math.min(1, haze.lightTop / 0.5)) * 50) / 50);
</script>

<aside
  class="np-panel"
  class:video={videoShown}
  bind:this={panelEl}
  aria-label="Now playing details"
  hidden={!open}
  style:--tone-glow={open && pageVisible ? tone.glow : undefined}
  style:--np-shade={shade}
  style:--plate-a={plateAlpha}
>
  {#if current}
    <!-- The picture layer, pinned under the scrolling column. -->
    <div class="np-stage" bind:this={stageEl} aria-hidden="true">
      {#each sources as source (source)}
        <video
          class="np-video"
          class:shown={source === shown.loop}
          class:under={source === leaving && leavingUnder}
          use:holdLoop={source}
          src={source}
          crossorigin="anonymous"
          muted
          loop
          playsinline
          preload="auto"
          onloadedmetadata={placeCanvas}
          onresize={placeCanvas}
          oncanplay={handleCanvasReady}
          onerror={handleCanvasError}
        ></video>
      {/each}
    </div>

    <div class="np-scroll" bind:this={scrollEl} use:scrollbar={{ top: hasHead ? 56 : 0 }}>
      <section class="np-hero">
        <div class="np-art" class:away={videoShown || !artCover}>
          {#if artCover}
            <div class="np-art-tile">
              <Cover src={artCover.src} id={artCover.id} name={artCover.name} fill lg />
            </div>
          {/if}
        </div>
        {#if open}
          {#key current.id}
            <div class="np-hero-copy">
              <span class="np-sentinel np-head-line" bind:this={headSentinel} aria-hidden="true"></span>
              {@render identity()}
            </div>
          {/key}
        {/if}
      </section>

      {#if open}
      {#key current.id}
        <div class="np-details" bind:this={detailsEl}>
          {#if current.album_id || current.album_name}
            <button
              class="np-card glass-card np-album"
              disabled={!current.album_id}
              onclick={() => current.album_id && navigate(current.uri?.startsWith("spotify:episode:") ? "show" : "album", current.album_id)}
            >
              <Cover src={current.cover_url} id={current.album_id || current.uri} name={current.album_name || ""} size={40} />
              <span class="np-album-copy">
                <span class="np-label">{current.uri?.startsWith("spotify:episode:") ? "Podcast" : "Album"}</span>
                <span class="np-album-name">{current.album_name || (current.uri?.startsWith("spotify:episode:") ? "Unknown podcast" : "Unknown album")}</span>
              </span>
              {#if current.album_id}<Icon name="fwd" size={13} />{/if}
            </button>
          {/if}

          <!-- Gold is the app's "who made it" hue, and the one warm accent with
               enough chroma to survive as 11px caps. -->
          {#if current.uri?.startsWith("spotify:track:")}
          <section class="np-card glass-card np-credits">
            <div class="np-section-head">
              <span class="tag credit">Credits</span>
              {#if contributorTotal}<span class="np-count tnum">{contributorTotal}</span>{/if}
            </div>

            {#if trackCredits.loading}
              <div class="np-credit-line" aria-label="Loading credits">
                <span class="skeleton line sm"></span>
                <span class="skeleton line"></span>
              </div>
            {:else if trackCredits.error}
              <p class="np-muted">Credits unavailable.</p>
            {:else if shownGroups.length}
              {#each shownGroups as group, groupIndex (`${group.title}-${groupIndex}`)}
                {@const people = group.contributors ?? []}
                {@const shown = people.slice(0, PANEL_NAMES)}
                <div class="np-credit-line">
                  <span class="np-role">{group.title}</span>
                  <p>
                    {shown.map((c) => c.name).join(", ")}{#if people.length > shown.length}<span class="np-more-inline"
                      >&nbsp;+{people.length - shown.length}</span
                    >{/if}
                  </p>
                </div>
              {/each}
              <button class="np-link credit" onclick={() => openCredits(current)}>
                {#if groups.length > PANEL_GROUPS || contributorTotal > shownGroups.reduce((n, g) => n + Math.min(PANEL_NAMES, g.contributors?.length ?? 0), 0)}
                  All {contributorTotal} credits
                {:else}
                  Full credits
                {/if}<Icon name="fwd" size={12} />
              </button>
            {:else}
              <p class="np-muted">No contributors listed for this track.</p>
            {/if}
          </section>
          {/if}

          {#if next}
            <section class="np-card glass-card np-upnext">
              <div class="np-section-head">
                <h3 class="caps">Up next</h3>
                <button class="np-link" onclick={() => navigate("queue")}>Queue<Icon name="fwd" size={12} /></button>
              </div>
              <button class="np-next" onclick={() => navigate("queue")}>
                <Cover src={next.cover_url} id={next.album_id || next.uri} name={next.name} size={38} />
                <span class="np-next-copy">
                  <strong>{next.name}</strong>
                  <span>{(next.artist_names ?? []).join(", ")}</span>
                </span>
              </button>
            </section>
          {/if}
        </div>
      {/key}
      {/if}
    </div>
  {:else}
    <div class="np-empty">
      <p>Nothing playing</p>
      <span>Start a song to see its artwork, artists, album, and credits.</span>
    </div>
  {/if}

  {#if hasHead}
    <!-- Its empty part moves the window: the panel is the window's top
         right corner. -->
    <header class="np-head" class:glass-strip={headOver} data-tauri-drag-region="deep">
      {#if canSwap}
        <button
          class="np-swap glass-plate"
          type="button"
          title={videoShown ? "Show the cover art" : "Show the Canvas animation"}
          onclick={swapSurface}
        >
          <Icon name="swap" size={11} />{videoShown ? "Cover" : "Canvas"}
        </button>
      {/if}
      {#if drawsWindowControls}<WindowControls />{/if}
    </header>
  {/if}
</aside>

{#snippet identity()}
  <h2 class="np-title">
    {#if current.album_id}
      <span class="np-title-link" role="link" tabindex="0" title={current.uri?.startsWith("spotify:episode:") ? "Go to podcast" : "Go to album"} onclick={openAlbum} onkeydown={openAlbum}
        >{current.name}</span
      >
    {:else}
      {current.name}
    {/if}
  </h2>
  {#if current.uri?.startsWith("spotify:episode:")}
    <button class="np-artists" disabled={!current.album_id} onclick={() => navigate("show", current.album_id)}>{current.album_name || current.artist_names?.[0]}</button>
  {:else}
    <ArtistLinks
      class="np-artists"
      names={current.artist_names}
      ids={current.artist_ids ?? []}
      id={current.artist_id}
    />
  {/if}
  <div class="np-meta-row">
    <p class="np-meta">
      {#if current.duration_ms}<span class="tnum">{formatTime(current.duration_ms)}</span>{/if}
      {#if current.duration_ms && current.play_count}<span class="np-dot" aria-hidden="true"></span>{/if}
      {#if current.play_count}<span><span class="tnum">{playCountFormatter.format(current.play_count)}</span> plays</span>{/if}
    </p>
    <!-- The one hint that the hero is the top of a column: the details are
         a scroll away, and this is the scroll. -->
    <button class="np-more glass-plate" type="button" title={current.uri?.startsWith("spotify:episode:") ? "Podcast and up next" : "Album, credits and up next"} onclick={revealDetails}>
      <Icon name="chevron-down" size={15} />
    </button>
  </div>
{/snippet}
