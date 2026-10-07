<script>
  import { untrack } from "svelte";
  import { api, ui, session, sessionEpoch, watchSavedTracks, watchCacheMarks, playback, isPlayingSource, togglePlay } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Icon from "../components/Icon.svelte";
  import LikedMark from "../components/LikedMark.svelte";
  import { paletteFor } from "../lib/covertone.svelte.js";
  import { detailArtSize } from "../lib/layout.js";
  import { mergeLikedRows, reconcileLikedRows, mergeLikedPage, applyLikedDelta } from "../lib/liked-songs.js";
  import { applyCacheMarks } from "../lib/cache-marks.js";

  /* Rose's own hue, rebuilt at the header's fixed dark. Every other detail
     page takes its colour from artwork; this collection has none, and does not
     want any — it is the one page in the app that is about YOU rather than
     about a record, so it gets the palette's "yours" hue at full strength. */
  const ROSE_TONE = paletteFor(21, 0.105);

  /* Same rule as every other detail header: the artwork gives way first. */
  const artSize = $derived(detailArtSize(ui.paneWidth));

  const collection = $state({ tracks: [], nextCursor: null, loadedPages: 0, removed: new Set() });
  const tracks = $derived(collection.tracks);
  const nextCursor = $derived(collection.nextCursor);
  const playingThis = $derived(isPlayingSource("liked"));
  let loading = $state(false);
  let error = $state("");
  let loadGeneration = 0;
  let changes = Promise.resolve();
  let pageRequest = null;

  async function loadPage(cursor = null, generation = loadGeneration) {
    if (loading) return;
    loading = true;
    error = "";
    try {
      await changes;
      if (generation !== loadGeneration) return;
      if (cursor != null) {
        cursor = nextCursor;
        if (cursor == null) return;
      }
      pageRequest = api.browseLikedSongs(cursor);
      const page = await pageRequest;
      if (generation !== loadGeneration) return;
      mergeLikedPage(collection, page);
    } catch (reason) {
      if (generation === loadGeneration) {
        error = String(reason || "Could not load Liked Songs.");
      }
    } finally {
      if (generation === loadGeneration) {
        pageRequest = null;
        loading = false;
      }
    }
  }

  function reloadCollection() {
    const generation = ++loadGeneration;
    collection.tracks = [];
    collection.loadedPages = 0;
    collection.removed.clear();
    changes = Promise.resolve();
    pageRequest = null;
    collection.nextCursor = null;
    loading = false;
    error = "";
    loadPage(null, generation);
  }

  $effect(() => {
    const account = session.username;
    const epoch = sessionEpoch();
    untrack(reloadCollection);
  });

  async function applySavedChange(change, generation) {
    if (generation !== loadGeneration) return;
    const uris = change?.uris?.filter((uri) => uri.startsWith("spotify:track:"));
    if (uris && typeof change.saved === "boolean") {
      if (!change.saved) {
        applyLikedDelta(collection, uris, false);
        return;
      }
      const saved = [];
      for (const uri of uris) {
        if (tracks.some((track) => track.uri === uri)) continue;
        saved.push(await api.browseTrack(uri.slice("spotify:track:".length)));
      }
      if (generation === loadGeneration) applyLikedDelta(collection, uris, true, saved);
      return;
    }
    // Unidentified external changes refresh only the pages already displayed.
    const fresh = [];
    let cursor = null;
    for (let page = 0; page < Math.max(1, collection.loadedPages); page++) {
      const result = await api.browseLikedSongs(cursor);
      if (generation !== loadGeneration) return;
      mergeLikedRows(fresh, result?.tracks);
      cursor = result?.next_cursor ?? null;
      if (!cursor) break;
    }
    reconcileLikedRows(tracks, fresh);
    collection.nextCursor = cursor;
  }

  $effect(() => watchSavedTracks((change) => {
    const generation = loadGeneration;
    changes = Promise.all([changes, pageRequest?.catch(() => {})]).then(() => applySavedChange(change, generation))
      .catch((reason) => {
        if (generation === loadGeneration) error = String(reason || "Could not update Liked Songs.");
      });
  }));

  $effect(() => watchCacheMarks((ids, cached) => applyCacheMarks(tracks, ids, cached)));

  function playFrom(index) {
    if (tracks.length) api.playQueue(tracks, index, "liked").catch(() => {});
  }

  function playOrToggle() {
    if (playingThis) togglePlay();
    else playFrom(0);
  }
</script>

<section
  class="view page liked-page wash"
  style:--tone-wash={ROSE_TONE.wash}
  style:--tone-wash-deep={ROSE_TONE.washDeep}
  style:--tone-glow={ROSE_TONE.glow}
>
  <header class="liked-head detail-head">
    <LikedMark size={artSize} />
    <div class="liked-copy">
      <span class="tag saved">Your collection</span>
      <h1 class="detail-title">Liked Songs</h1>
      <p class="detail-meta">
        <span class="num">{tracks.length} loaded {tracks.length === 1 ? "song" : "songs"}</span>
        {#if nextCursor}<span class="sep">/</span><span>More available</span>{/if}
      </p>
    </div>
  </header>

  <div class="actions liked-actions">
    <!-- ROSE, not foam, and this is the one page where that is right: the
         palette's rule is that foam is what you can DO and rose is what is
         YOURS, and on every other page those are different objects. Here the
         thing you press and the thing it belongs to are the same collection.
         A 48px solid disc is also exactly the kind of surface rose can carry —
         area, not ink. -->
    <button
      class="play-lg saved"
      title={playingThis ? (playback.playing ? "Pause" : "Resume") : "Play Liked Songs"}
      disabled={!tracks.length && !playingThis}
      onclick={playOrToggle}
    >
      <Icon name={playingThis && playback.playing ? "pause" : "play"} size={22} />
    </button>
  </div>

  {#if tracks.length}
    <div class="section liked-tracks">
      <TrackList {tracks} {playFrom} queueContext="liked" />
    </div>
  {:else if loading}
    <div class="tl liked-loading" style="--cols:28px 36px minmax(0,1fr) 52px" aria-hidden="true">
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
  {:else if error}
    <div class="empty">
      <p class="h">Liked Songs unavailable</p><p class="sub">{error}</p>
      <div class="actions"><button class="pill" onclick={() => loadPage()}>Try again</button></div>
    </div>
  {:else}
    <div class="empty"><p class="h">No liked songs found.</p><p class="sub">Songs saved to your Spotify library will appear here.</p></div>
  {/if}

  {#if tracks.length && (nextCursor || loading || error)}
    <div class="liked-more">
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      {#if nextCursor}
        <button class="pill" disabled={loading} onclick={() => loadPage(nextCursor)}>
          {loading ? "Loading…" : "Load more"}
        </button>
      {/if}
    </div>
  {/if}
</section>
