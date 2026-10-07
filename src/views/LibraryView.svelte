<script>
  import { untrack } from "svelte";
  import {
    library,
    libraryState,
    route,
    playback,
    navigate,
    ui,
    ensurePersonalizedDiscovery,
    mergePersonalizedPlaylists,
  } from "../lib/state.svelte.js";
  import { playPlaylistById, playLikedSongs, cardPlay } from "../lib/play.js";
  import { coverTone, cardTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import LikedMark from "../components/LikedMark.svelte";
  import Icon from "../components/Icon.svelte";
  import { playlistSubtitle } from "../lib/artist.js";
  import { rowWindow } from "../lib/virtual.js";
  const greeting = $derived.by(() => {
    const h = new Date().getHours();
    if (h < 5) return "Good night";
    if (h < 12) return "Good morning";
    if (h < 18) return "Good afternoon";
    return "Good evening";
  });

  /*
   * Home has one source of truth: the loaded library. The backend orders it by
   * `last_activity` for the sidebar; listening history is the explicit
   * exception and is copied/sorted by `last_played` here.
   */
  const uniqueLibrary = $derived.by(() => {
    const seen = new Set();
    return library.filter((playlist) => {
      const id = playlist?.id;
      if (!id || seen.has(id)) return false;
      seen.add(id);
      return true;
    });
  });

  /*
   * Provisional shelves stay empty until the fresh rootlist lands. The cached
   * snapshot can carry stale `last_played` rows; deriving shelves from it
   * would mount a listening-history band that the fresh answer immediately
   * reshuffles. Until then Home renders one skeleton frame that reserves the
   * settled hierarchy — no cached row mounts early enough to be reordered.
   */
  const recentlyPlayed = $derived.by(() => {
    if (!libraryState.fresh) return [];
    return uniqueLibrary
      .filter((playlist) => Number.isFinite(Number(playlist?.last_played)))
      .slice()
      .sort((left, right) => Number(right.last_played) - Number(left.last_played));
  });

  const CARD_MIN = 158;
  const CARD_GAP = 20;
  const cardsPerRow = $derived.by(() => {
    const usable = (ui.paneWidth || 1200) - 2 * 24;
    const count = Math.floor((usable + CARD_GAP) / (CARD_MIN + CARD_GAP));
    return Math.max(2, Math.min(7, count));
  });

  const recentShelf = $derived(recentlyPlayed.slice(0, cardsPerRow));
  const recentIds = $derived(new Set(recentShelf.map((playlist) => playlist.id)));

  /* Search discovery is lazy and session-cached. Waiting for the library
     answer means the conditional Discover Weekly query never races an empty
     rootlist snapshot. */
  $effect(() => {
    const home = route.name === "library";
    const ready = playback.ready === true && playback.auth_state === "ready";
    const answered = libraryState.loaded;
    if (!home || !ready || !answered) return;
    untrack(() => ensurePersonalizedDiscovery(library));
  });

  /* Same gate: personalized cards join only once the rootlist is
     authoritative, so the band mounts already final instead of refilling. */
  const madeForYouCandidates = $derived.by(() => {
    if (!libraryState.fresh) return [];
    return mergePersonalizedPlaylists(uniqueLibrary).filter(
      (playlist) => !recentIds.has(playlist.id),
    );
  });
  const madeForYouInitialCount = $derived(cardsPerRow * 2);
  const madeForYou = $derived(
    madeForYouCandidates.slice(0, madeForYouInitialCount),
  );
  const canSeeAllMadeForYou = $derived(
    madeForYouCandidates.length > madeForYouInitialCount,
  );
  const shownIds = $derived(
    new Set([
      ...recentShelf,
      // Keep personalized cards out of Your library: See all reveals them in
      // one canonical place rather than duplicating them below.
      ...madeForYouCandidates,
    ].map((playlist) => playlist.id)),
  );
  const remainingLibrary = $derived(
    uniqueLibrary.filter((playlist) => !shownIds.has(playlist.id)),
  );

  let pageEl = $state(null);
  let libraryGrid = $state(null);
  const gridGeometry = $state({ columns: 1, width: 0, height: 0, gap: 0 });
  let firstCard = $state(0);
  let lastCard = $state(14);
  let focusedCard = $state("");
  const libraryCards = $derived([{ id: "liked" }, ...remainingLibrary]);
  const cardIndex = $derived(new Map(libraryCards.map((card, index) => [card.id, index])));
  const gridRows = $derived(Math.ceil(libraryCards.length / gridGeometry.columns));
  const gridPitch = $derived(gridGeometry.height + gridGeometry.gap);
  const renderedCards = $derived.by(() => {
    const indices = new Set();
    for (let i = firstCard; i < Math.min(lastCard, libraryCards.length); i++) indices.add(i);
    const focused = cardIndex.get(focusedCard);
    if (focused != null) {
      indices.add(focused);
      if (focused > 0) indices.add(focused - 1);
      if (focused + 1 < libraryCards.length) indices.add(focused + 1);
    }
    return [...indices].sort((a, b) => a - b).map((index) => ({ card: libraryCards[index], index }));
  });

  function measureCardWindow() {
    const scroller = libraryGrid?.closest(".scroll");
    if (!scroller || !gridGeometry.height) return;
    const { first, last } = rowWindow(libraryGrid, scroller, gridPitch, 2, gridRows);
    firstCard = first * gridGeometry.columns;
    lastCard = Math.min(libraryCards.length, last * gridGeometry.columns);
  }

  function retainCardFocus(event) {
    const cell = event.target.closest("[data-library-card]");
    if (cell) focusedCard = cell.dataset.libraryCard;
  }

  function releaseCardFocus(event) {
    if (!libraryGrid?.contains(event.relatedTarget)) focusedCard = "";
  }

  $effect(() => {
    const grid = libraryGrid;
    const page = pageEl;
    if (!grid || !page) return;
    const scroller = grid.closest(".scroll");
    if (!scroller) return;
    let sample = null;
    let frame = 0;
    const observer = new ResizeObserver(() => measureGeometry());
    function measureGeometry() {
      const style = getComputedStyle(grid);
      const widths = style.gridTemplateColumns.split(" ").map(Number.parseFloat).filter(Number.isFinite);
      const card = grid.querySelector(".library-cell > .card");
      if (!widths.length || !card) return;
      if (sample !== card) {
        if (sample) observer.unobserve(sample);
        sample = card;
        observer.observe(sample);
      }
      gridGeometry.columns = widths.length;
      gridGeometry.width = widths[0];
      gridGeometry.gap = Number.parseFloat(style.rowGap) || 0;
      gridGeometry.height = card.getBoundingClientRect().height;
      measureCardWindow();
    }
    const schedule = () => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        measureCardWindow();
      });
    };
    observer.observe(grid);
    observer.observe(page);
    scroller.addEventListener("scroll", schedule, { passive: true });
    untrack(measureGeometry);
    return () => {
      observer.disconnect();
      scroller.removeEventListener("scroll", schedule);
      if (frame) cancelAnimationFrame(frame);
    };
  });
  $effect(() => {
    libraryCards;
    recentShelf;
    madeForYou;
    if (libraryGrid) untrack(measureCardWindow);
  });

  const tonePlaylist = $derived(
    libraryState.fresh ? (recentShelf[0] ?? uniqueLibrary[0] ?? null) : null,
  );
  const tone = $derived(
    coverTone(
      tonePlaylist?.cover_url || tonePlaylist?.cover_urls,
      tonePlaylist?.id ?? "home",
    ),
  );

  const busy = $state({ id: "" });
  let error = $state("");

  async function play(id) {
    error = "";
    error = await cardPlay(busy, id, () => playPlaylistById(id), "Could not play this playlist.");
  }

  async function playLiked() {
    error = "";
    error = await cardPlay(busy, "liked", playLikedSongs, "Could not play Liked Songs.");
  }

</script>

{#snippet likedCard(canPlay)}
  <div class="card liked-card">
    <div class="card-art">
      <LikedMark fill />
      <button class="card-open" aria-label="Open Liked Songs" onclick={() => navigate("liked")}
      ></button>
      {#if canPlay}
        <button
          class="card-play saved"
          aria-label="Play Liked Songs"
          title="Play Liked Songs"
          disabled={!!busy.id}
          onclick={playLiked}
        >
          <Icon name={busy.id === "liked" ? "more" : "play"} size={15} />
        </button>
      {/if}
    </div>
    <button class="card-copy" onclick={() => navigate("liked")}>
      <span class="card-name">Liked Songs</span>
      <span class="card-sub">Your collection</span>
    </button>
  </div>
{/snippet}

{#snippet playlistCard(pl)}
  <div class="card" use:cardTone={[pl.cover_url || pl.cover_urls, pl.id]}>
    <div class="card-art">
      <Cover src={pl.cover_url} srcs={pl.cover_urls ?? []} id={pl.id} name={pl.name} fill lg />
      <button
        class="card-open"
        aria-label={`Open ${pl.name}`}
        onclick={() => navigate("playlist", pl.id)}
      ></button>
      <button
        class="card-play"
        aria-label={`Play ${pl.name}`}
        title={`Play ${pl.name}`}
        disabled={!!busy.id}
        onclick={() => play(pl.id)}
      >
        <Icon name={busy.id === pl.id ? "more" : "play"} size={15} />
      </button>
    </div>
    <button class="card-copy" onclick={() => navigate("playlist", pl.id)}>
      <span class="card-name">{pl.name}</span>
      <span class="card-sub">{playlistSubtitle(pl)}</span>
    </button>
  </div>
{/snippet}

{#snippet skeletonCard(seed)}
  <div class="card" aria-hidden="true">
    <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
    <span class="card-copy">
      <span class="skeleton line" style="width:{74 - ((seed * 11) % 30)}%;height:12px;margin:0"></span>
      <span class="skeleton line" style="width:38%;height:10px;margin:6px 0 0"></span>
    </span>
  </div>
{/snippet}

<!-- `soft`: a third of the usual wash. A greeting is not a 56px record title,
     and the first row of covers sits inside this field — at full strength the
     colour would compete with the artwork it is supposed to introduce. -->
<section
  class="view page wash soft"
  bind:this={pageEl}
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  <div style="padding:var(--s4) 0 var(--s6)">
    <h1 class="page-title">{greeting}</h1>
  </div>

{#if !libraryState.fresh}
  <!-- One coherent home loading frame. The cached snapshot still hydrates the
       sidebar and detail views, but Home itself waits for the authoritative
       rootlist so nothing real mounts ahead of shelves the fresh answer may
       reorder. The frame reserves the settled hierarchy — Recently played,
       Made for you, Your library — at real card geometry and no cached art;
       the fresh event then replaces it exactly once, atomically. -->
  <div class="section home-shelf-section" style="margin-top:0">
    <div class="section-head"><h2 class="section-title">Recently played</h2></div>
    <div class="grid home-shelf" style={`--shelf-columns:${cardsPerRow}`}>
      {#each Array.from({ length: cardsPerRow }) as _, i (i)}
        {@render skeletonCard(i)}
      {/each}
    </div>
  </div>

  <div class="section home-shelf-section">
    <div class="section-head"><h2 class="section-title">Made for you</h2></div>
    <div class="grid home-shelf" style={`--shelf-columns:${cardsPerRow}`}>
      {#each Array.from({ length: madeForYouInitialCount }) as _, i (i)}
        {@render skeletonCard(cardsPerRow + i)}
      {/each}
    </div>
  </div>

  <div class="section">
    <div class="section-head"><h2 class="section-title">Your library</h2></div>
    <div class="grid">
      {#each Array.from({ length: 12 }) as _, i (i)}
        {@render skeletonCard(i)}
      {/each}
    </div>
  </div>
{:else}
  {#if recentShelf.length}
    <div class="section home-shelf-section" style="margin-top:0">
      <div class="section-head"><h2 class="section-title">Recently played</h2></div>
      <div
        class="grid home-shelf"
        style={`--shelf-columns:${Math.max(2, recentShelf.length)}`}
      >
        {#each recentShelf as pl (pl.id)}
          {@render playlistCard(pl)}
        {/each}
      </div>
    </div>
  {/if}

  {#if madeForYou.length}
    <div class="section home-shelf-section">
      <div class="section-head">
        <h2 class="section-title">Made for you</h2>
        {#if canSeeAllMadeForYou}
          <button
            class="link-more"
            type="button"
            onclick={() => navigate("made-for-you")}
          >
            See all
          </button>
        {/if}
      </div>
      <div
        class="grid home-shelf"
        style={`--shelf-columns:${cardsPerRow}`}
      >
        {#each madeForYou as pl (pl.id)}
          {@render playlistCard(pl)}
        {/each}
      </div>
    </div>
  {/if}

  {#if uniqueLibrary.length}
    <div
      class="section"
      class:home-first={!recentShelf.length && !madeForYou.length}
    >
      <div class="section-head"><h2 class="section-title">Your library</h2></div>
      <div class="grid library-window" bind:this={libraryGrid}
        style:height={gridGeometry.height ? `${Math.max(0, gridRows * gridPitch - gridGeometry.gap)}px` : null}
        onfocusin={retainCardFocus} onfocusout={releaseCardFocus}>
        {#each renderedCards as entry (entry.card.id)}
          <div class="library-cell" data-library-card={entry.card.id}
            style:position={gridGeometry.height ? "absolute" : null}
            style:width={gridGeometry.height ? `${gridGeometry.width}px` : null}
            style:left={`${(entry.index % gridGeometry.columns) * (gridGeometry.width + gridGeometry.gap)}px`}
            style:top={`${Math.floor(entry.index / gridGeometry.columns) * gridPitch}px`}>
            {#if entry.card.id === "liked"}{@render likedCard(true)}
            {:else}{@render playlistCard(entry.card)}{/if}
          </div>
        {/each}
      </div>
    </div>
  {:else}
    <!-- Only reachable once fresh: an empty rootlist is a real answer now,
         not a loading state, so the frame above has already been replaced. -->
    <div class="section" style="margin-top:0">
      <div class="section-head"><h2 class="section-title">Your library</h2></div>
      <div class="grid">
        {@render likedCard(false)}
      </div>
      <p class="sub" style="margin-top:var(--s5)">
        No playlists yet — anything you save in Spotify shows up here.
      </p>
    </div>
  {/if}
{/if}

  {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
</section>

<style>
  .library-window { position: relative; overflow-anchor: none; }
  .library-cell { min-width: 0; }
  /*
   * Shelves reuse the exact card object and cover interactions from `.grid`.
   * The column count follows the pane width, so Made for you always stays
   * within two responsive rows; See all owns the unbounded view.
   */
  .grid.home-shelf {
    grid-template-columns: repeat(var(--shelf-columns), minmax(0, 1fr));
  }
  /* Equal columns turn a thin shelf — Recently played with a single entry —
     into one balloon at half the pane. The card keeps the shelf's own scale
     instead; the unasked-for track simply stays empty. Skeleton shelves share
     the class and sit at cardsPerRow columns, far under the cap, so the
     skeleton→fresh swap keeps its geometry. */
  .grid.home-shelf > .card {
    max-width: 300px;
  }
  .home-first { margin-top: 0; }
  @media (max-width: 620px) {
    .home-shelf { gap: var(--s4); }
  }
</style>
