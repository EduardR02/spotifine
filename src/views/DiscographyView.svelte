<script>
  import {
    detail,
    route,
    navigate,
    navigateArtist,
    loadCataloguePage,
    playCatalogueContext,
    retryDetail,
  } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import Select from "../components/Select.svelte";
  import { GROUPS, releaseKind, createCataloguePaging, sentinelLoader } from "../lib/discography.svelte.js";
  import { observeStuck } from "../lib/sticky.js";
  import { formatTotal } from "../lib/time.js";
  import { coverTone } from "../lib/covertone.svelte.js";

  /**
   * The whole discography, read end to end.
   *
   * This is the READER, and it is a page of its own again. It was folded into
   * the artist page during the redesign, which made the artist page a
   * several-screen scroll through every track of every record — the artist
   * page's job is to introduce an artist, and the reader's job is to let you
   * work through everything they made. Two jobs, two surfaces, one toggle
   * shared between them so the selection you made on the shelf is the
   * selection you arrive here with.
   *
   * The one thing this view can do that nothing else in the app can: in "All",
   * playback runs ACROSS releases. Start a track and it keeps going into the
   * next record, because the queue is built from a flat walk of the loaded
   * catalogue rather than from one album. That is why the reader exists.
   */
  const artist = $derived(detail.artist);
  const counts = $derived(artist?.release_counts ?? {});
  /* The reader is a room off the artist's page, so it takes the artist's own
     tone — soft, like the other sub-pages — instead of a flat ground. */
  const tone = $derived(coverTone(artist?.cover_url || "", artist?.id || ""));

  const releaseTotal = $derived(
    GROUPS[0].types.reduce((sum, type) => sum + (counts[type] ?? 0), 0),
  );
  /** Empty groups are not offered: a dead tab is a worse answer than no tab. */
  const segments = $derived(
    GROUPS.filter((g) => g.id === "all" || (counts[g.id] ?? 0) > 0).map((g) => ({
      ...g,
      count: g.id === "all" ? releaseTotal : (counts[g.id] ?? 0),
    })),
  );

  /* Seeded from the route so "See all" lands on the segment you were looking
     at, then owned locally: re-pushing history on every toggle would make Back
     mean "undo the last filter click" instead of "leave the discography". */
  function selectedGroup(value) {
    return GROUPS.some((candidate) => candidate.id === value) ? value : "all";
  }

  let group = $state(selectedGroup(route.param));
  let loadedRoute = `${route.id ?? ""}:${route.param ?? ""}`;
  $effect(() => {
    const key = `${route.id ?? ""}:${route.param ?? ""}`;
    if (key === loadedRoute) return;
    loadedRoute = key;
    group = selectedGroup(route.param);
  });
  const releaseTypes = $derived(GROUPS.find((g) => g.id === group)?.types ?? GROUPS[0].types);

  /**
   * Order applies to what is LOADED, and the footer says so. The catalogue
   * arrives four releases at a time, so "oldest first" is an honest ordering of
   * what you can see rather than a claim about the whole discography.
   */
  const SORTS = [
    { value: "catalogue", label: "Catalogue order" },
    { value: "newest", label: "Newest first" },
    { value: "oldest", label: "Oldest first" },
    { value: "az", label: "A – Z" },
  ];
  let sort = $state("catalogue");

  let sentinel = $state(null);

  /**
   * The paged catalogue reader this view shares with Appears On (see
   * src/lib/discography.svelte.js). This side contributes only its identity:
   * whose catalogue, and which slice of it — picking a group asks the engine
   * for that query again, it does not filter what already arrived.
   */
  const paging = createCataloguePaging({
    getId: () => artist?.id ?? "",
    releaseTypes: () => (GROUPS.find((g) => g.id === group)?.types ?? GROUPS[0].types),
    pageSize: 4,
  });

  /**
   * Display order is distinct from the playback context. Catalogue order is
   * the backend's stable round-robin walk across selected release types; it
   * must not reshuffle already-visible releases when another page arrives.
   * Explicit sort choices order the releases currently loaded.
   */
  const ordered = $derived.by(() => {
    const items = paging.releases.map((release, index) => ({ release, index }));
    if (sort === "catalogue") return items;
    if (sort === "az") {
      return items.sort((a, b) =>
        a.release.name.localeCompare(b.release.name, undefined, { sensitivity: "base" }) ||
        a.index - b.index,
      );
    }
    // Releases with no year sink to the bottom of either direction rather than
    // pretending to be from year zero.
    const dir = sort === "oldest" ? 1 : -1;
    return items.sort((a, b) => {
      if (a.release.year == null && b.release.year == null) return a.index - b.index;
      if (a.release.year == null) return 1;
      if (b.release.year == null) return -1;
      return (a.release.year - b.release.year) * dir || a.index - b.index;
    });
  });

  /** Flat index of a track across every release loaded so far. */
  function playFrom(releaseIndex, trackIndex) {
    let index = trackIndex;
    for (let i = 0; i < releaseIndex; i++) index += paging.releases[i].tracks.length;
    playCatalogueContext(paging.releases, artist.id, releaseTypes, paging.nextOffset, index).catch(() => {});
  }

  function playRelease(releaseIndex) {
    playFrom(releaseIndex, 0);
  }

  /* A new artist or group is another list: drop what is on screen and open
     the first page of the new selection. */
  $effect(() => paging.reset());

  /* Re-attached per page: a footer still in reach loads the next one. */
  $effect(() => {
    paging.releases.length;
    return sentinelLoader(sentinel, () => paging.loadNext());
  });

  /* The control row is type on the page until releases pass under it. */
  let controlSentinel = $state(null);
  let controlsStuck = $state(false);
  $effect(() => observeStuck(controlSentinel, (stuck) => (controlsStuck = stuck)));

  /** Everyone credited on the release who is not the artist whose page this is. */
  function guests(release) {
    return (release.artist_names ?? []).filter((name) => name && name !== artist?.name);
  }
</script>

<section
  class="view page dx-page wash soft"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if artist}
    <header class="dx-title-block">
      <button class="page-back" onclick={() => navigateArtist(artist.id, artist.name)}>
        <Icon name="back" size={13} />{artist.name}
      </button>
      <div class="dx-title-row">
        <h1 class="page-title">Discography</h1>
        <span class="section-count tnum">{releaseTotal}</span>
      </div>
    </header>

    <div class="dx-controls-sentinel" bind:this={controlSentinel} aria-hidden="true"></div>
    <!-- One row: what to read on the left, in what order on the right. Sticky,
         because on a discography several screens long the control that changes
         what you are reading has to still be reachable while you read it. -->
    <div class="dx-controls" class:stuck={controlsStuck}>
      <div class="seg" role="tablist" aria-label="Release types">
        {#each segments as option (option.id)}
          <button
            class="seg-btn"
            class:on={group === option.id}
            role="tab"
            aria-selected={group === option.id}
            onclick={() => (group = option.id)}
          >
            {option.label}<span class="seg-n tnum">{option.count}</span>
          </button>
        {/each}
      </div>
      <Select
        options={SORTS}
        value={sort}
        label="Sort releases"
        onchange={(value) => (sort = value)}
      />
    </div>

    {#if group === "all"}
      <!-- The one thing the reader can do that nothing else in the app can,
           said once, where the behaviour is. -->
      <p class="dx-note">
        <Icon name="queue" size={13} />Playing anything here runs on through the rest of
        the catalogue.
      </p>
    {/if}

    {#each ordered as entry (entry.release.id)}
      {@const release = entry.release}
      {@const others = guests(release)}
      <article class="release">
        <div class="release-side">
          <!-- Same rule as every other cover in the app: the artwork OPENS the
               record, the button on it plays. `.release-open` is a transparent
               button over the tile rather than a button around it, because a
               button cannot contain a button. -->
          <div class="release-art">
            <Cover src={release.cover_url} id={release.id} name={release.name} size={148} lg raised />
            <button
              class="release-open"
              aria-label={`Open ${release.name}`}
              title={`Open ${release.name}`}
              onclick={() => navigate("album", release.id)}
            ></button>
            <button
              class="release-play"
              aria-label={`Play ${release.name}`}
              title={`Play ${release.name}`}
              onclick={() => playRelease(entry.index)}
            >
              <Icon name="play" size={16} />
            </button>
          </div>
        </div>
        <div class="release-body">
          <h3>
            <button class="release-name" onclick={() => navigate("album", release.id)}>
              {release.name}
            </button>
          </h3>
          <p class="release-meta">
            <span class="kind">{releaseKind(release)}</span>
            {#if release.year}<span class="num">{release.year}</span>{/if}
            <span class="sep">/</span><span class="num">{release.tracks.length} songs</span>
            {#if release.tracks.length}
              <span class="sep">/</span><span class="num">{formatTotal(release.tracks)}</span>
            {/if}
            {#if others.length}
              <span class="sep">/</span><span class="release-with">with {others.join(", ")}</span>
            {/if}
          </p>
          <TrackList
            tracks={release.tracks}
            playFrom={(trackIndex) => playFrom(entry.index, trackIndex)}
            showAlbum={false}
            showArt={false}
            showHead={false}
            showPlays
            layoutKey={ordered}
            queueContext={`artist:${route.id ?? ""}`}
          />
        </div>
      </article>
    {/each}

    {#if !paging.releases.length && paging.loading}
      <!-- The frame a release arrives into, drawn before it does: cover slot,
           title, meta, six rows. Nothing moves when the batch lands. -->
      {#each Array.from({ length: 2 }) as _, i (i)}
        <article class="release" aria-hidden="true">
          <div class="release-side"><span class="skeleton release-sk-art"></span></div>
          <div class="release-body">
            <span class="skeleton line lg" style="height:26px;width:min(280px,60%)"></span>
            <span class="skeleton line sm" style="margin-bottom:var(--s5)"></span>
            {#each Array.from({ length: 6 }) as __, k (k)}
              <div class="sk-row" style="--cols:28px minmax(0,1fr) 52px">
                <span class="sk" style="width:12px"></span>
                <span class="sk-stack">
                  <span class="sk a" style="width:{56 - ((k * 7) % 20)}%"></span>
                  <span class="sk b" style="width:{27 - ((k * 5) % 9)}%"></span>
                </span>
                <span class="sk" style="width:28px;justify-self:end"></span>
              </div>
            {/each}
          </div>
        </article>
      {/each}
    {/if}

    <div class="dx-foot" bind:this={sentinel}>
      {#if paging.loading && paging.releases.length}
        <span class="dx-status">Loading releases…</span>
      {:else if paging.error && paging.nextOffset != null}
        <button class="pill" onclick={() => paging.loadNext()}>
          Try again
        </button>
      {:else if paging.nextOffset != null && paging.releases.length}
        <!-- Near-footer scrolling does this automatically; the button is the
             keyboard fallback and explicit retry affordance. -->
        <button class="pill" onclick={() => paging.loadNext()}>
          Load more<Icon name="chevron-down" size={14} />
        </button>
      {:else if paging.releases.length}
        <span class="dx-status">
          <span class="tnum">{paging.releases.length}</span>{paging.total && paging.total !== paging.releases.length
            ? ` of ${paging.total}`
            : ""} releases
        </span>
      {/if}
    </div>
    {#if paging.error}<p class="inline-error" role="alert">{paging.error}</p>{/if}

    {#if !paging.loading && !paging.releases.length && !paging.error}
      <div class="empty">
        <p class="h">Nothing under this heading.</p>
        <p class="sub">Try another release type.</p>
      </div>
    {/if}
  {:else if detail.error}
    <header class="dx-title-block">
      <div class="dx-title-row"><h1 class="page-title">Discography</h1></div>
    </header>
    <div class="empty failed">
      <p class="h">This artist could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions"><button class="pill" onclick={retryDetail}>Try again</button></div>
    </div>
  {:else}
    <!-- Same frame as the loaded page, so the header does not jump when the
         artist arrives. -->
    <header class="dx-title-block">
      <span class="skeleton line sm"></span>
      <span class="skeleton line lg" style="height:40px;width:min(340px,60%)"></span>
    </header>
    <div class="dx-controls" aria-hidden="true">
      <span class="skeleton" style="width:min(420px,70%);height:36px;border-radius:18px"></span>
    </div>
  {/if}
</section>

<style>
  .dx-page { padding-top: var(--s5); }
  .dx-title-block { padding-bottom: var(--s4); }
  .dx-title-row {
    display: flex; align-items: baseline; gap: var(--s3);
    margin-top: var(--s2);
  }

  .dx-controls-sentinel { height: 0; pointer-events: none; }
  .dx-controls {
    position: sticky; top: var(--topbar-h); z-index: 15;
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s4) var(--s5); flex-wrap: wrap;
    padding: var(--s3) 0;
    /* Same rule as the track-list head: no plate until it is covering
       something, and then the same strip glass as the topbar it sticks
       under. */
    background: transparent;
    transition: background-color var(--d2) var(--ease), box-shadow var(--d2) var(--ease);
  }
  .dx-controls.stuck {
    background: var(--tint-strip);
    -webkit-backdrop-filter: var(--frost-strip);
            backdrop-filter: var(--frost-strip);
    box-shadow: inset 0 -1px 0 var(--line-2);
  }
  /* `space-between` puts a lone item at flex-START on a wrapped line, so on a
     narrow pane the order control dropped to the left under the toggle rather
     than staying opposite it. `margin-left: auto` is right in both cases. */
  .dx-controls > :global(.sel) { margin-left: auto; }

  .dx-note {
    display: flex; align-items: center; gap: var(--s2);
    margin-top: var(--s2); padding-bottom: var(--s2);
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-11);
  }
  .dx-note :global(svg) { color: var(--accent); flex: none; }

  /* Cover and tracks side by side: the release stays one readable object as
     you scroll past it, instead of a header that scrolls away from its list.
     148px is the smallest size at which sleeve typography is still readable. */
  .release {
    display: grid; grid-template-columns: 148px minmax(0, 1fr); gap: var(--s6);
    padding: var(--s6) 0; border-top: 1px solid var(--line);
  }
  /* The sticky cover shares a scroller with the sticky topbar AND the sticky
     toggle above it, so its offset is measured from the bottom of both:
     34px of control plus 2 x 12px of padding, plus a little air. */
  .release-side {
    position: sticky; top: calc(var(--topbar-h) + 66px); align-self: start;
  }
  .release-art { position: relative; width: 148px; height: 148px; border-radius: var(--r3); }
  .release-sk-art { display: block; width: 148px; height: 148px; border-radius: var(--r3); }
  .release-open {
    position: absolute; inset: 0; z-index: 1;
    border-radius: var(--r3); background: none;
  }
  .release-open:focus-visible { outline-offset: 3px; }
  .release-play {
    position: absolute; right: var(--s2); bottom: var(--s2); z-index: 2;
    width: 34px; height: 34px; border-radius: var(--rf);
    display: grid; place-items: center;
    background: var(--accent); color: var(--accent-ink);
    box-shadow: 0 8px 18px -4px rgba(0, 0, 0, 0.7);
    opacity: 0; transform: translateY(4px);
    transition: opacity var(--d1) var(--ease), transform var(--d2) var(--ease),
                background-color var(--d1) var(--ease);
  }
  .release-art:hover .release-play,
  .release-play:focus-visible { opacity: 1; transform: none; }
  .release-play:hover { background: var(--accent-hi); }

  .release-body h3 { margin: 0; }
  .release-name {
    max-width: 100%; text-align: left;
    font-family: var(--font-display);
    font-size: var(--t-20); font-weight: var(--w-bold);
    letter-spacing: -0.015em; line-height: 1.2; color: var(--fg);
    transition: color var(--d1) var(--ease);
  }
  .release-name:hover { color: var(--accent); }
  .release-meta {
    margin: var(--s1) 0 var(--s3);
    font-size: var(--t-12); color: var(--fg-2);
    display: flex; align-items: center; gap: var(--s2); flex-wrap: wrap;
  }
  .release-meta .sep { color: var(--fg-3); }
  .release-with { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .dx-foot { min-height: 72px; display: grid; place-items: center; padding: var(--s4) 0; }
  .dx-status { font-size: var(--t-12); color: var(--fg-2); }

  /* Below this the cover rail costs more width than it earns and the release
     becomes a stacked block. */
  @media (max-width: 760px) {
    .release { grid-template-columns: 1fr; }
    .release-side { position: static; }
  }
</style>
