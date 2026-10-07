<script>
  import {
    detail,
    playback,
    isPlayingSource,
    togglePlay,
    api,
    insertPlaylist,
    navigate,
    navigateArtist,
    promotePlaylist,
    retryDetail,
    route,
    ui,
  } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import HeaderMenu from "../components/HeaderMenu.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { detailArtSize } from "../lib/layout.js";
  import { formatTotal } from "../lib/time.js";

  const radio = $derived(detail.radio);
  const seed = $derived(radio?.seed ?? null);
  const tracks = $derived(radio?.tracks ?? []);
  const artistRadio = $derived(
    route.id?.startsWith("artist:") || radio?.seed_kind === "artist",
  );
  const seedArtist = $derived(radio?.seed_artist ?? null);
  const tag = $derived(artistRadio ? "Artist Radio" : "Song Radio");
  const title = $derived(
    artistRadio
      ? `${seedArtist?.name ?? "Artist"} Radio`
      : seed?.name
        ? `${seed.name} Radio`
        : "Song Radio",
  );
  const radioArtSources = $derived.by(() => {
    // RadioBrowse currently has track/artist contexts. Keep any future
    // playlist-backed context on its official cover instead of mosaicing it.
    if (!radio || !["track", "artist"].includes(radio.seed_kind)) return [];
    return [...new Set(tracks.map((track) => track?.cover_url).filter(Boolean))].slice(0, 4);
  });
  const fallbackCover = $derived(
    artistRadio ? seedArtist?.cover_url || seed?.cover_url || "" : seed?.cover_url || "",
  );
  const officialCover = $derived(!artistRadio ? radio?.cover_url || "" : "");
  const coverId = $derived(
    artistRadio ? seedArtist?.id || seed?.id || "" : seed?.album_id || seed?.id || "",
  );
  const coverName = $derived(
    artistRadio ? seedArtist?.name || seed?.name || "" : seed?.album_name || seed?.name || "",
  );
  const tone = $derived(coverTone(officialCover || fallbackCover, coverId || "radio"));
  /* Where the "…" can take you: the record the radio grew from. */
  const seedArtistId = $derived(artistRadio ? seedArtist?.id || "" : seed?.artist_ids?.[0] || seed?.artist_id || "");
  const seedArtistName = $derived(artistRadio ? seedArtist?.name || "" : seed?.artist_names?.[0] || "");
  const seedAlbumId = $derived(artistRadio ? "" : seed?.album_id || "");
  const artSize = $derived(detailArtSize(ui.paneWidth));

  const playingThis = $derived(isPlayingSource(`radio:${route.id ?? ""}`));

  // Keep a successful create id when adding tracks fails. A retry then
  // completes the same playlist instead of creating a duplicate.
  let saveState = $state({
    routeId: "__unset__",
    busy: false,
    playlistId: null,
    error: "",
  });

  let mounted = true;
  $effect(() => () => { mounted = false; });
  $effect.pre(() => {
    const routeId = route.id;
    if (saveState.routeId === routeId) return;
    saveState = { routeId, busy: false, playlistId: null, error: "" };
  });

  function playFrom(index) {
    if (!tracks[index]) return;
    api.playQueue(tracks, index, `radio:${route.id ?? ""}`).catch(() => {});
  }

  function playOrToggle() {
    if (playingThis) {
      togglePlay();
      return;
    }
    if (tracks.length) api.playQueue(tracks, 0, `radio:${route.id ?? ""}`).catch(() => {});
  }

  async function shufflePlay() {
    if (!tracks.length) return;
    const queue = tracks;
    const source = `radio:${route.id ?? ""}`;
    try {
      await api.setShuffle(true);
      await api.playQueue(queue, 0, source, { automaticStart: true });
    } catch {}
  }

  async function saveAsPlaylist() {
    if (saveState.busy || !radio || !tracks.length) return;
    const request = saveState;
    const name = title;
    const current = () => mounted && saveState === request &&
      route.name === "radio" && route.id === request.routeId;
    const uris = tracks.map((track) => track?.uri);
    if (uris.some((uri) => !uri)) {
      request.error = "This radio contains a track without a Spotify URI.";
      return;
    }

    request.busy = true;
    request.error = "";
    try {
      let playlistId = request.playlistId;
      if (!playlistId) {
        const created = await api.createPlaylist(name);
        playlistId = created?.id;
        if (!playlistId) throw new Error("Playlist creation returned no id.");
        request.playlistId = playlistId;
        /* The row has to exist before anything can promote it: the
           `promotePlaylist` below is a no-op for a playlist the library has
           never heard of, so without this the freshly saved radio was absent
           from the sidebar until the refetch, under the rootlist's not-yet-
           readable name. */
        insertPlaylist(created);
      }
      await api.addPlaylistTracks(playlistId, uris);
      promotePlaylist(playlistId);
      api.touchPlaylistActivity(playlistId).catch(() => {});
      if (current()) navigate("playlist", playlistId);
    } catch (reason) {
      if (current()) request.error = String(reason?.message ?? reason ?? "Could not save this radio.");
    } finally {
      request.busy = false;
    }
  }
</script>

<section
  class="view page wash"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if detail.error && !radio}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="tag">{tag}</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This {artistRadio ? "artist" : "song"} radio could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="pill" onclick={retryDetail}>Try again</button>
      </div>
    </div>
  {:else if !radio}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="skeleton line sm" style="width:84px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(440px,72%)"></span>
        <span class="skeleton line sm" style="width:180px"></span>
      </div>
    </header>
    <div class="tl" style="margin-top:var(--s6);--cols:28px 36px minmax(0,1fr) 52px" aria-hidden="true">
      {#each Array.from({ length: 7 }) as _, i (i)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk art"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{64 - ((i * 7) % 24)}%"></span>
            <span class="sk b" style="width:{32 - ((i * 5) % 11)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
      {/each}
    </div>
  {:else}
    <header class="detail-head">
      <Cover
        src={officialCover || (radioArtSources.length >= 4 ? "" : fallbackCover)}
        srcs={radioArtSources}
        id={coverId}
        name={coverName}
        size={artSize}
        lg
        raised
      />
      <div>
        <span class="tag">{tag}</span>
        <h1 class="detail-title">{title}</h1>
        <p class="detail-meta">
          <span class="who">Spotify</span>
          <span class="sep">/</span><span class="num">{tracks.length} songs</span>
          {#if tracks.length}
            <span class="sep">/</span><span class="num">{formatTotal(tracks)}</span>
          {/if}
        </p>
        <div class="actions">
          <button
            class="play-lg"
            title={playingThis ? (playback.playing ? "Pause" : "Resume") : "Play"}
            onclick={playOrToggle}
            disabled={!tracks.length}
          >
            <Icon name={playingThis && playback.playing ? "pause" : "play"} size={22} />
          </button>
          <button class="btn-round lg" title="Shuffle" aria-label="Shuffle" onclick={shufflePlay} disabled={!tracks.length}>
            <Icon name="shuffle" size={20} />
          </button>
          <button
            class="btn-round lg"
            title={saveState.busy ? "Saving…" : saveState.playlistId ? "Retry saving as a playlist" : "Save as playlist"}
            aria-label={saveState.playlistId ? "Retry saving as a playlist" : "Save as playlist"}
            aria-busy={saveState.busy}
            onclick={saveAsPlaylist}
            disabled={!tracks.length || saveState.busy}
          >
            <Icon name="playlist-add" size={20} />
          </button>
          {#if seedArtistId || seedAlbumId}
            <HeaderMenu label="Radio actions">
              {#snippet children(close)}
                {#if seedAlbumId}
                  <button class="menu-item" role="menuitem" onclick={() => { close(); navigate("album", seedAlbumId); }}><Icon name="album" size={16} />Go to album</button>
                {/if}
                {#if seedArtistId}
                  <button class="menu-item" role="menuitem" onclick={() => { close(); navigateArtist(seedArtistId, seedArtistName); }}><Icon name="artist" size={16} />Go to artist</button>
                {/if}
              {/snippet}
            </HeaderMenu>
          {/if}
        </div>
        {#if saveState.error}
          <p class="inline-error" role="alert">{saveState.error}</p>
        {/if}
      </div>
    </header>

    {#if tracks.length}
      <div style="margin-top:var(--s6)">
        <TrackList {tracks} {playFrom} queueContext={`radio:${route.id ?? ""}`} />
      </div>
    {:else}
      <div class="empty">
        <p>No recommendations are available for this {artistRadio ? "artist" : "song"}.</p>
      </div>
    {/if}
  {/if}
</section>
