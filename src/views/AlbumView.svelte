<script>
  import { detail, api, ui, navigate, retryDetail, playback, isPlayingSource, togglePlay } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ArtistLinks from "../components/ArtistLinks.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { spotifyLink } from "../lib/spotify-link.js";
  import HeaderMenu from "../components/HeaderMenu.svelte";
  import CopyLinkItem from "../components/CopyLinkItem.svelte";
  import { formatTotal } from "../lib/time.js";
  import { detailArtSize } from "../lib/layout.js";

  const album = $derived(detail.album);
  /* The sleeve gives way before the title does when the pane is narrow. */
  const artSize = $derived(detailArtSize(ui.paneWidth));
  const tracks = $derived(album?.tracks ?? []);
  const source = $derived(`album:${album?.id ?? ""}`);
  const playingThis = $derived(isPlayingSource(source));
  const artistIds = $derived(album?.artist_ids ?? []);
  // Keep old/partial payloads useful: the primary track id can still link the
  // first header artist, while missing parallel ids leave other names plain.
  const artistFallbackId = $derived(artistIds[0] || tracks[0]?.artist_id || "");

  /* An album always has a sleeve, so this is the page where content colour is
     least ambiguous: the header is simply the record's own colour. (The old
     `--h` here was dead — it set a variable the wash rule never read, so every
     album page washed the same default foam.) */
  const tone = $derived(coverTone(album?.cover_url ?? "", album?.id ?? ""));
  let shuffleBusy = $state(false);
  let actionError = $state("");
  $effect(() => {
    album?.id;
    actionError = "";
    shuffleBusy = false;
  });

  function playFrom(i) {
    if (tracks.length) api.playQueue(tracks, i, source).catch(() => {});
  }

  function playOrToggle() {
    if (playingThis) togglePlay();
    else if (tracks.length) api.playQueue(tracks, 0, source, { automaticStart: true }).catch(() => {});
  }

  async function shufflePlay() {
    if (!tracks.length || shuffleBusy) return;
    const id = album?.id ?? "";
    const queue = [...tracks];
    shuffleBusy = true;
    actionError = "";
    try {
      await api.setShuffle(true);
      await api.playQueue(queue, 0, `album:${id}`, { automaticStart: true });
    } catch (reason) {
      if (album?.id === id) {
        actionError = String(reason || "Could not shuffle this album.");
      }
    } finally {
      if (album?.id === id) shuffleBusy = false;
    }
  }
</script>

<section
  class="view page wash"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if detail.error && !album}
    <!-- The request failed, so this page stays a frame with an explanation in
         it rather than a skeleton that never resolves. -->
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="tag">Album</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This release could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="pill" onclick={retryDetail}>Try again</button>
        <button class="link-more" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if !album}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <!-- The frame the record arrives into: tag, title, meta line and the
           two controls, each at the size of the thing that replaces it, so
           nothing on the page moves when the payload lands. -->
      <div>
        <span class="skeleton line sm" style="width:56px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(420px,70%)"></span>
        <span class="skeleton line sm" style="width:180px"></span>
        <div class="actions">
          <span class="skeleton" style="width:48px;height:48px;border-radius:var(--rf)"></span>
          <span class="skeleton" style="width:40px;height:40px;border-radius:var(--rf)"></span>
        </div>
      </div>
    </header>
    <div class="tl" style="margin-top:var(--s6);--cols:28px minmax(0,1fr) 52px" aria-hidden="true">
      {#each Array.from({ length: 8 }) as _, i (i)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{58 - ((i * 7) % 22)}%"></span>
            <span class="sk b" style="width:{28 - ((i * 5) % 10)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
      {/each}
    </div>
  {:else}
    <header class="detail-head">
      <Cover src={album.cover_url} id={album.id} name={album.name} size={artSize} lg raised />
      <div>
        <span class="tag">Album</span>
        <h1 class="detail-title">{album.name}</h1>
        <p class="detail-meta">
          <ArtistLinks
            class="who"
            names={album.artist_names}
            ids={artistIds}
            id={artistFallbackId}
          />
          {#if album.year}
            <span class="sep">/</span><span class="num">{album.year}</span>
          {/if}
          <span class="sep">/</span><span class="num">{tracks.length} songs</span>
          {#if tracks.length}
            <span class="sep">/</span><span class="num">{formatTotal(tracks)}</span>
          {/if}
        </p>
        <div class="actions">
          <button class="play-lg" title={playingThis ? (playback.playing ? "Pause" : "Resume") : "Play"} onclick={playOrToggle} disabled={!tracks.length && !playingThis}>
            <Icon name={playingThis && playback.playing ? "pause" : "play"} size={22} />
          </button>
          <button class="btn-round lg" title="Shuffle" aria-label="Shuffle" aria-busy={shuffleBusy} onclick={shufflePlay} disabled={!tracks.length || shuffleBusy}>
            <Icon name="shuffle" size={20} />
          </button>
          <HeaderMenu label="Album actions">
            {#snippet children(close)}
              <CopyLinkItem link={spotifyLink("album", album?.id)} {close} />
            {/snippet}
          </HeaderMenu>
          {#if actionError}<span class="inline-error" role="alert">{actionError}</span>{/if}
        </div>
      </div>
    </header>

    {#if tracks.length}
      <!-- No album column and no per-row thumbnail: the art is already the
           largest thing on the page, and every row would repeat it. -->
      <div style="margin-top:var(--s6)">
        <TrackList {tracks} {playFrom} showAlbum={false} showArt={false} showPlays queueContext={source} />
      </div>
    {:else}
      <div class="empty">
        <p class="h">No tracks on this release.</p>
        <p class="sub">Nothing came back from the engine for this album.</p>
      </div>
    {/if}
  {/if}
</section>
