<script>
  import { untrack } from "svelte";
  import { route, search, api, focusSearch, watchCacheMarks } from "../lib/state.svelte.js";
  import { applyCacheMarks } from "../lib/cache-marks.js";
  import TrackList from "../components/TrackList.svelte";
  import Icon from "../components/Icon.svelte";

  const query = $derived(String(route.id || search.query || "").trim());
  let tracks = $state([]);
  let loading = $state(false);
  let error = $state("");
  let generation = 0;

  /* This view is entered only through SearchView's See all action. That click
     is the lazy-load boundary: ordinary searches never issue this limit=50
     request or resolve its extra artwork. */
  $effect(() => {
    const q = query;
    if (!q) return;
    const mine = ++generation;
    tracks = untrack(() => search.query === q ? (search.results?.tracks ?? []) : []);
    loading = true;
    error = "";
    api.search(q, 50)
      .then((result) => {
        if (mine !== generation) return;
        tracks = result?.tracks ?? tracks;
      })
      .catch((reason) => {
        if (mine === generation) error = String(reason || "Could not load more songs.");
      })
      .finally(() => {
        if (mine === generation) loading = false;
      });
    return () => { generation += 1; };
  });

  /* Download marks reach these songs in place. */
  $effect(() => watchCacheMarks((ids, cached) => applyCacheMarks(tracks, ids, cached)));

  function playFrom(index) {
    if (tracks.length) api.playQueue(tracks, index, "search").catch(() => {});
  }
</script>

<section class="view page search-songs-page">
  <header class="results-head">
    <div>
      <span class="tag">Song results</span>
      <h1 class="page-title">{query ? `Songs for “${query}”` : "Songs"}</h1>
      <p class="results-summary">
        {#if loading}
          Loading the full result set…
        {:else}
          {tracks.length} {tracks.length === 1 ? "song" : "songs"}
        {/if}
      </p>
    </div>
    {#if tracks.length}
      <button class="play-lg" title="Play song results" onclick={() => playFrom(0)}>
        <Icon name="play" size={18} />
      </button>
    {/if}
  </header>

  {#if tracks.length}
    <TrackList {tracks} {playFrom} queueContext="search" />
    {#if loading}<p class="inline-status" aria-live="polite">Loading more songs…</p>{/if}
    {#if error}<p class="inline-error results-error" role="alert">{error}</p>{/if}
  {:else if loading}
    <!-- `--cols` is normally set by TrackList from the same booleans that pick
         its cells; a standalone skeleton has to name its own. -->
    <div class="tl" style="--cols:28px 36px minmax(0,1fr) 52px" aria-hidden="true">
      {#each Array.from({ length: 10 }) as _, index (index)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk art"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{66 - ((index * 7) % 26)}%"></span>
            <span class="sk b" style="width:{31 - ((index * 5) % 12)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
      {/each}
    </div>
  {:else}
    <div class="empty">
      <p class="h">No songs found.</p>
      <p class="sub">{error || "Try a different title, artist, or spelling."}</p>
      <div class="actions"><button class="pill" onclick={focusSearch}><Icon name="search" size={14} />Search again</button></div>
    </div>
  {/if}
</section>
