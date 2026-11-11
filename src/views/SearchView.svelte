<script>
  import { untrack } from "svelte";
  import { search, api, navigate, navigateArtist, focusSearch, queueSearch, retrySearch, playback, togglePlay } from "../lib/state.svelte.js";
  import { playAlbumById, playPlaylistById, cardPlay } from "../lib/play.js";
  import { coverTone, cardTone } from "../lib/covertone.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { playlistSubtitle } from "../lib/artist.js";
  /* The query field lives in the topbar; this view only renders results. */

  /* Five, because five is what the loaded page shows (`visibleTracks`). A
     loading state with the wrong row count is a loading state that reflows. */
  const SKELETON_ROWS = Array.from({ length: 5 });

  const tracks = $derived(search.results?.tracks ?? []);
  const albums = $derived(search.results?.albums ?? []);
  const playlists = $derived(search.results?.playlists ?? []);
  const artists = $derived(search.results?.artists ?? []);
  const shows = $derived(search.results?.shows ?? []);
  const episodes = $derived(search.results?.episodes ?? []);
  const empty = $derived(!tracks.length && !albums.length && !playlists.length && !artists.length && !shows.length && !episodes.length);
  const visibleTracks = $derived(tracks.slice(0, 5));

  /* Top result: Spotify's own cross-kind ranking, not "whichever artist
     happened to come back first". Ranking by kind was answering a different
     question than the one typed — `top 50` returned Ariana Grande, `OK
     Computer` returned Radiohead rather than the record, and `Bohemian
     Rhapsody` returned Queen rather than the song. The engine reads the
     ranking out of the same searchDesktop response the groups come from; see
     `search_top_ref` in browse.rs.

     `sub` is the credit line under the name. An artist has none: the kind
     line already says everything a portrait and a name do not. */
  const TOP_LABEL = { track: "Song", album: "Album", artist: "Artist", playlist: "Playlist", show: "Podcast", episode: "Episode" };
  const top = $derived.by(() => {
    const ranked = search.results?.top;
    if (!ranked) return null;
    let hit = ranked;
    if (ranked.kind === "episode") {
      const episode = episodes.find((item) => item.track.uri === ranked.uri);
      if (!episode) return null;
      hit = { ...episode.track, kind: "episode", show_name: episode.show_name };
    }
    /* A playlist names its OWNER here, not its description. Everywhere else a
       playlist's own words are the most useful second line, but this line is
       one line in a 320px column: "Dein tägliches Update zu den aktuell am…"
       is not an answer, and "Spotify" is exactly the fact that separates the
       official Discover Weekly from someone's copy of it. */
    const sub = hit.kind === "artist" ? "" : hit.kind === "playlist"
      ? hit.owner || playlistSubtitle(hit) : hit.kind === "show"
        ? hit.publisher : hit.kind === "episode"
          ? hit.show_name : (hit.artist_names ?? []).join(", ");
    return { ...hit, label: TOP_LABEL[hit.kind] ?? "Result", sub };
  });

  function openTop() {
    if (!top) return;
    // A song is kept as the row it is in the Songs list — the whole track,
    // which is what playing it again takes; a top hit that is not in the
    // list is played now but not kept.
    const row = top.kind === "track" ? tracks.find((t) => t.id === top.id) : null;
    if (row) keep("track", row);
    else if (top.kind !== "track") keep(top.kind, top, top.sub);
    if (top.kind === "artist") return navigateArtist(top.id, top.name);
    if (top.kind === "album") return navigate("album", top.id);
    if (top.kind === "playlist") return navigate("playlist", top.id);
    if (top.kind === "show") return navigate("show", top.id);
    if (top.kind === "episode") return navigate("episode", top.id);
    /* A song opens by playing. Start the queue at the ranked track's own
       position when the Songs list contains it, so what plays next is the
       rest of the list rather than a one-track queue that stops dead. */
    const at = tracks.findIndex((t) => t.id === top.id);
    const queue = at >= 0 ? tracks : [top];
    playError = "";
    api.playQueue(queue, Math.max(at, 0), "search").catch((error) => { playError = String(error); });
  }

  /** An episode from the results plays alone: its show is one click away. */
  function playEpisode(episode) {
    playError = "";
    keep("episode", episode.track, episode.show_name);
    if (episode.track.uri === playback.current_uri) {
      togglePlay();
      return;
    }
    api.playQueue([episode.track], 0, `episode:${episode.track.id}`).catch((error) => { playError = String(error); });
  }

  function playTrack(i) {
    playError = "";
    if (tracks[i]) keep("track", tracks[i]);
    if (tracks.length) api.playQueue(tracks, i, "search").catch((error) => { playError = String(error); });
  }

  /* Opening a result from its card: the page it leads to, remembered. */
  function openPlaylist(pl) {
    keep("playlist", pl, pl.owner || playlistSubtitle(pl));
    navigate("playlist", pl.id);
  }
  function openAlbum(al) {
    keep("album", al, al.artist_names.join(", "));
    navigate("album", al.id);
  }
  function openShow(show) {
    keep("show", show, show.publisher);
    navigate("show", show.id);
  }
  function openEpisode(episode) {
    keep("episode", episode.track, episode.show_name);
    navigate("episode", episode.track.id);
  }
  function openArtist(ar) {
    keep("artist", ar);
    navigateArtist(ar.id, ar.name);
  }

  const busy = $state({ id: "" });
  let playError = $state("");

  /* The card opens; only this button plays. A result card knows an id and
     nothing else, so playing it costs one browse first. */
  async function play(id, load, failure) {
    playError = "";
    playError = await cardPlay(busy, id, () => load(id), failure);
  }

  /* The body under the failure heading carries only what the heading and the
     retry button do not already say. When a failure has no cause of its own,
     state.svelte.js answers with SEARCH_FAILURE_MESSAGE — "Search could not
     load. Try again." — which restates both, so the line is dropped instead
     of saying the same thing three times. While Spotify is still connecting
     the waiting notice is the actual cause, and the view says it in its own
     words; afterwards any remaining text IS the underlying cause. */
  const searchCause = $derived.by(() => {
    const raw = search.error;
    if (!raw) return "";
    if (search.link?.error) return raw;
    if (!(playback.ready === true && playback.auth_state === "ready")) {
      return "Spotify hasn't finished connecting.";
    }
    return raw === "Search could not load. Try again." ? "" : raw;
  });

  /* ---- What search remembers -------------------------------------------
     Two short lists, newest first, in localStorage: the queries that came
     back, and the results that were actually opened or played from them.
     The second is what the empty page is for — the thing searched for last
     week is one click away, with its cover, instead of a word to type
     again. Reading is wrapped because a webview with storage disabled
     throws on access rather than returning null, and a search page must
     not fail to render over a history list. */
  const RECENTS_KEY = "sr.recent-searches";
  const OPENED_KEY = "sr.search-opened";
  const RECENTS_MAX = 8;
  const OPENED_MAX = 8;

  function readList(key, max, valid) {
    try {
      const raw = JSON.parse(localStorage.getItem(key) ?? "[]");
      return Array.isArray(raw) ? raw.filter(valid).slice(0, max) : [];
    } catch {
      return [];
    }
  }
  function writeList(key, list) {
    try {
      localStorage.setItem(key, JSON.stringify(list));
    } catch {
      /* private mode / storage disabled: the list is simply not persisted */
    }
  }

  let recents = $state(readList(RECENTS_KEY, RECENTS_MAX, (s) => typeof s === "string" && s.trim()));
  let opened = $state(readList(OPENED_KEY, OPENED_MAX, (e) => e && typeof e.id === "string" && typeof e.name === "string" && TOP_LABEL[e.kind] &&
    (e.kind !== "track" || typeof e.track?.uri === "string")));

  function writeRecents(next) {
    recents = next.slice(0, RECENTS_MAX);
    writeList(RECENTS_KEY, recents);
  }
  function writeOpened(next) {
    opened = next.slice(0, OPENED_MAX);
    writeList(OPENED_KEY, opened);
  }

  /* A query typed in steps comes back once per pause — "just a", then "just
     a gent" — and every step used to be kept. The newest entry is replaced
     when the query only extends or trims it. */
  function remember(q) {
    const lower = q.toLowerCase();
    writeRecents([q, ...recents.filter((r, i) => {
      const other = r.toLowerCase();
      return other !== lower && !(i === 0 && (lower.startsWith(other) || other.startsWith(lower)));
    })]);
  }

  /** Remember a result that was opened or played. A song keeps the whole
      track, which is what playing it again takes. */
  function keep(kind, item, sub = "") {
    const entry = {
      kind,
      id: item.id,
      name: item.name,
      sub: kind === "track" ? (item.artist_names ?? []).join(", ") : sub || "",
      cover_url: item.cover_url ?? "",
      cover_urls: item.cover_urls ?? [],
      track: kind === "track" ? $state.snapshot(item) : null,
    };
    writeOpened([entry, ...opened.filter((e) => !(e.kind === kind && e.id === item.id))]);
  }

  function discard(entry) {
    writeOpened(opened.filter((e) => e !== entry));
  }

  function reopen(entry) {
    writeOpened([entry, ...opened.filter((e) => e !== entry)]);
    if (entry.kind === "artist") return navigateArtist(entry.id, entry.name);
    if (entry.kind !== "track") return navigate(entry.kind, entry.id);
    playError = "";
    if (entry.track.uri === playback.current_uri) return togglePlay();
    api.playQueue([$state.snapshot(entry.track)], 0, "search").catch((error) => { playError = String(error); });
  }

  function forget(q) {
    writeRecents(recents.filter((r) => r !== q));
  }

  function rerun(q) {
    search.query = q;
    remember(q);
    queueSearch(q);
  }

  /* Record only queries that actually came back. `search.results` is the sole
     tracked read: everything else runs untracked, because `remember` both
     reads and writes `recents` and tracking that would make this effect
     re-trigger itself forever. */
  $effect(() => {
    if (!search.results || search.link) return;
    untrack(() => {
      const q = search.query.trim();
      if (q) remember(q);
    });
  });
</script>

<section class="view page">
  <div class="search-intro">
    <h1 class="page-title">
      {search.link ? (search.results?.tracks?.[0]?.name ?? "Open a Spotify link") : search.submitted && search.query ? `Results for “${search.query}”` : "Search"}
    </h1>
    <!-- Refining a query keeps the previous results on screen, which is right
         — but then nothing on the page says a newer answer is coming. One
         quiet line does, and it occupies a fixed slot so the results below it
         do not shift when it appears. -->
    <p class="search-status" class:on={search.busy && (!!search.results || !!search.link)} role="status">
      {search.busy ? (search.link ? "Opening Spotify link…" : "Searching…") : ""}
    </p>
  </div>

  {#if !search.submitted}
    <!-- The page before a query. It holds only what search itself knows:
         what was opened from it, to go straight back to, and what was typed
         into it, to run again. Nothing is fetched to fill it. -->
    {#if opened.length}
      <div class="section search-idle">
        <div class="section-head">
          <h2 class="section-title">Recent</h2>
          <button class="link-more" onclick={() => writeOpened([])}>Clear</button>
        </div>
        <div class="recent-grid">
          {#each opened as entry (entry.kind + entry.id)}
            {@const playing = entry.kind === "track" && entry.track.uri === playback.current_uri}
            <div class="recent glass-card" class:playing>
              <button
                class="recent-open"
                title={entry.kind === "track" ? `Play ${entry.name}` : undefined}
                onclick={() => reopen(entry)}
              >
                <Cover
                  src={entry.cover_url}
                  srcs={entry.cover_urls}
                  id={entry.id}
                  name={entry.name}
                  size={40}
                  circle={entry.kind === "artist"}
                />
                <span class="recent-copy">
                  <span class="recent-name">{entry.name}</span>
                  <span class="recent-sub">
                    {#if playing}<Icon name={playback.playing ? "pause" : "play"} size={9} />{/if}
                    {entry.sub ? `${TOP_LABEL[entry.kind]} · ${entry.sub}` : TOP_LABEL[entry.kind]}
                  </span>
                </span>
              </button>
              <button class="recent-x" title="Remove" aria-label={`Remove ${entry.name}`} onclick={() => discard(entry)}>
                <Icon name="x" size={11} />
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}
    {#if recents.length}
      <div class="section" class:search-idle={!opened.length}>
        <div class="section-head">
          <h2 class="section-title">Searches</h2>
          <button class="link-more" onclick={() => writeRecents([])}>Clear</button>
        </div>
        <div class="chips">
          {#each recents as q (q)}
            <span class="chip">
              <button class="label" onclick={() => rerun(q)}>{q}</button>
              <button class="x" title="Remove" onclick={() => forget(q)}>
                <Icon name="x" size={11} />
              </button>
            </span>
          {/each}
        </div>
      </div>
    {/if}
    {#if !opened.length && !recents.length}
      <div class="empty">
        <p class="h">Search all of Spotify.</p>
        <p class="sub">Songs, albums, artists, playlists and audio podcasts come back from one query, and a pasted Spotify link opens directly. What you open from here is kept on this page.</p>
      </div>
    {/if}
    {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}
  {:else if search.error && !search.results}
    <div class="empty failed" role="alert">
      <p class="h">{search.link ? "This link couldn't open." : "Search couldn't load."}</p>
      <!-- One fact per line: the heading names the failure, the cause line
           (when there is one) says why, the button owns the retry. -->
      {#if searchCause}<p class="why">{searchCause}</p>{/if}
      <div class="actions">
        {#if search.link?.error}
          <button class="pill" onclick={focusSearch}>Edit link</button>
        {:else}
          <button class="pill" onclick={retrySearch}>Try again</button>
        {/if}
      </div>
    </div>
  {:else if !search.results}
    <!-- =================================================================
         The frame the results arrive into.

         This used to be six bare rows in a one-column strip, which shares no
         geometry at all with what actually lands — a two-column split with a
         top-result panel beside five songs, then grids of albums, playlists
         and artists. So every search visibly rebuilt the page: the strip
         vanished, four sections appeared, and the whole thing jumped.

         Now the wait IS the page, with its content missing: same split, same
         panel, same row heights, same card grid. Nothing moves when the
         payload lands, which is the only thing a loading state is actually
         for. Static, deliberately — a shimmer animates for exactly as long as
         the request takes, which is the idle cost this app exists to avoid.
         ================================================================= -->
    <div class="search-split" aria-busy="true" aria-label="Searching">
      <div class="split-top">
        <div class="section-head"><h2 class="section-title">Top result</h2></div>
        <div class="top-result glass-card sk-top">
          <span class="skeleton" style="width:92px;height:92px;border-radius:var(--r3)"></span>
          <span style="min-width:0;display:block">
            <span class="skeleton line" style="width:74%;height:24px;margin:0"></span>
            <span class="skeleton line" style="width:44%;height:12px;margin:10px 0 0"></span>
          </span>
        </div>
      </div>
      <div class="split-songs">
        <div class="section-head"><h2 class="section-title">Songs</h2></div>
        <div class="tl" style="--cols:28px 36px minmax(0,1fr) 52px 28px">
          {#each SKELETON_ROWS as _, i (i)}
            <div class="sk-row">
              <span class="sk" style="width:12px"></span>
              <span class="sk art"></span>
              <span class="sk-stack">
                <span class="sk a" style="width:{68 - ((i * 9) % 26)}%"></span>
                <span class="sk b" style="width:{34 - ((i * 5) % 12)}%"></span>
              </span>
              <span class="sk" style="width:28px;justify-self:end"></span>
              <span></span>
            </div>
          {/each}
        </div>
      </div>
    </div>

    {#each ["Playlists", "Albums", "Artists"] as heading (heading)}
      <div class="section" aria-hidden="true">
        <div class="section-head"><h2 class="section-title">{heading}</h2></div>
        <div class="grid">
          {#each Array.from({ length: 6 }) as _, i (i)}
            <div class="card">
              <span
                class="skeleton"
                style="display:block;aspect-ratio:1;width:100%;border-radius:{heading === 'Artists'
                  ? 'var(--rf)'
                  : 'var(--r3)'}"
              ></span>
              <span class="card-copy">
                <span class="skeleton line" style="width:{72 - ((i * 13) % 28)}%;height:12px;margin:0"></span>
                <span class="skeleton line" style="width:40%;height:10px;margin:6px 0 0"></span>
              </span>
            </div>
          {/each}
        </div>
      </div>
    {/each}
  {:else if empty}
    <div class="empty">
      <p class="h">No results for “{search.query}”.</p>
      <p class="sub">Check the spelling, or try a shorter query.</p>
      <div class="actions">
        <button class="pill" onclick={focusSearch}>
          <Icon name="search" size={14} />Try another search
        </button>
      </div>
    </div>
    {#if search.error}
      <p class="inline-error" role="status">
        {search.error}
        <button class="link-more" onclick={retrySearch}>Try again</button>
      </p>
    {/if}
  {:else}
    <div class="search-split">
      {#if top}
        {@const topTone = coverTone(top.cover_url || top.cover_urls, top.id)}
        <div class="split-top">
          <div class="section-head"><h2 class="section-title">Top result</h2></div>
          <!-- The one big coloured object on a results page. Search has no
               subject of its own to take a header wash from, but the top
               result IS a subject, so the panel around it takes that record's
               colour rather than being the fourth grey rectangle on screen. -->
          <button
            class="top-result glass-card"
            style:--tone-wash={topTone.wash}
            style:--tone-glow={topTone.glow}
            onclick={openTop}
            aria-label={search.link?.kind === "track" ? `Play ${top.name}` : undefined}
          >
            <Cover
              src={top.cover_url}
              srcs={top.cover_urls ?? []}
              id={top.id}
              name={top.name}
              size={92}
              lg
              circle={top.kind === "artist"}
            />
            <span style="min-width:0">
              <span class="tr-name">{top.name}</span>
              <span class="tr-sub">{top.sub ? `${top.label} / ${top.sub}` : top.label}</span>
              {#if search.link?.kind === "track"}
                <span class="tr-sub"><Icon name="play" size={11} /> Play song</span>
              {/if}
            </span>
          </button>
        </div>
      {/if}

      {#if tracks.length}
        <div class="split-songs">
          <div class="section-head">
            <h2 class="section-title">Songs</h2>
            {#if tracks.length > 5}
              <!-- The larger request starts only after this explicit click.
                   Keeping it on its own route also gives long results the full pane. -->
              <button class="link-more" onclick={() => navigate("search-songs", search.query)}>See all</button>
            {/if}
          </div>
          <TrackList
            tracks={visibleTracks}
            playFrom={playTrack}
            showAlbum={search.link?.kind === "track"}
            disableWindowing
            queueContext="search"
          />
        </div>
      {/if}
    </div>
    {#if search.error}
      <p class="inline-error" role="status">
        {search.error}
        <button class="link-more" onclick={retrySearch}>Try again</button>
      </p>
    {/if}

    {#if playlists.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Playlists</h2></div>
        <div class="grid">
          {#each playlists as pl (pl.id)}
            <div class="card" use:cardTone={[pl.cover_url || pl.cover_urls, pl.id]}>
              <div class="card-art">
                <Cover
                  src={pl.cover_url}
                  srcs={pl.cover_urls ?? []}
                  id={pl.id}
                  name={pl.name}
                  fill
                  lg
                />
                <button
                  class="card-open"
                  aria-label={`Open ${pl.name}`}
                  onclick={() => openPlaylist(pl)}
                ></button>
                <button
                  class="card-play"
                  aria-label={`Play ${pl.name}`}
                  title={`Play ${pl.name}`}
                  disabled={!!busy.id}
                  onclick={() => {
                    keep("playlist", pl, pl.owner || playlistSubtitle(pl));
                    play(pl.id, playPlaylistById, "Could not play this playlist.");
                  }}
                >
                  <Icon name={busy.id === pl.id ? "more" : "play"} size={15} />
                </button>
              </div>
              <button class="card-copy" onclick={() => openPlaylist(pl)}>
                <span class="card-name">{pl.name}</span>
                <span class="card-sub">{playlistSubtitle(pl)}</span>
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    {#if albums.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Albums</h2></div>
        <div class="grid">
          {#each albums as al (al.id)}
            <div class="card" use:cardTone={[al.cover_url, al.id]}>
              <div class="card-art">
                <Cover src={al.cover_url} id={al.id} name={al.name} fill lg />
                <button
                  class="card-open"
                  aria-label={`Open ${al.name}`}
                  onclick={() => openAlbum(al)}
                ></button>
                <button
                  class="card-play"
                  aria-label={`Play ${al.name}`}
                  title={`Play ${al.name}`}
                  disabled={!!busy.id}
                  onclick={() => {
                    keep("album", al, al.artist_names.join(", "));
                    play(al.id, playAlbumById, "Could not play this release.");
                  }}
                >
                  <Icon name={busy.id === al.id ? "more" : "play"} size={15} />
                </button>
              </div>
              <button class="card-copy" onclick={() => openAlbum(al)}>
                <span class="card-name">{al.name}</span>
                <!-- One expression, not two adjacent ones: `{year} · {names}`
                     across an {#if} loses the space after the middot, which is
                     why these read as "2019 ·Ceramic Hands". -->
                <span class="card-sub"
                  >{[al.year || null, al.artist_names.join(", ")].filter(Boolean).join(" · ")}</span
                >
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}

    {#if shows.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Podcasts</h2></div>
        <div class="grid">
          {#each shows as show (show.id)}
            <div class="card" use:cardTone={[show.cover_url, show.id]}>
              <div class="card-art">
                <Cover src={show.cover_url} id={show.id} name={show.name} fill lg />
                <button class="card-open" aria-label={`Open ${show.name}`} onclick={() => openShow(show)}></button>
              </div>
              <button class="card-copy" onclick={() => openShow(show)}>
                <span class="card-name">{show.name}</span>
                <span class="card-sub">{show.publisher || "Podcast"}</span>
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}
    {#if episodes.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Episodes <span class="section-note">audio only</span></h2></div>
        <div class="grid">
          {#each episodes as episode (episode.track.id)}
            <div class="card" use:cardTone={[episode.track.cover_url, episode.track.id]}>
              <div class="card-art">
                <Cover src={episode.track.cover_url} id={episode.track.id} name={episode.track.name} fill lg />
                <button class="card-open" aria-label={`Open ${episode.track.name}`} onclick={() => openEpisode(episode)}></button>
                {#if !episode.track.unavailable}
                  <button
                    class="card-play"
                    aria-label={`${episode.track.uri === playback.current_uri && playback.playing ? "Pause" : "Play"} ${episode.track.name}`}
                    title={`${episode.track.uri === playback.current_uri && playback.playing ? "Pause" : "Play"} ${episode.track.name}`}
                    onclick={() => playEpisode(episode)}
                  >
                    <Icon name={episode.track.uri === playback.current_uri && playback.playing ? "pause" : "play"} size={15} />
                  </button>
                {/if}
              </div>
              <button class="card-copy" onclick={() => openEpisode(episode)}>
                <span class="card-name">{episode.track.name}</span>
                <span class="card-sub">{episode.show_name}</span>
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}
    {#if artists.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Artists</h2></div>
        <div class="grid">
          {#each artists as ar (ar.id)}
            <!-- No play button here, and that is not an omission: an artist is
                 not a record. The whole card opens, which is the same rule the
                 others follow with the exception removed. -->
            <button class="card" use:cardTone={[ar.cover_url, ar.id]} onclick={() => openArtist(ar)}>
              <span class="card-art">
                <Cover src={ar.cover_url} id={ar.id} name={ar.name} fill circle />
              </span>
              <span class="card-copy">
                <span class="card-name">{ar.name}</span>
                <span class="card-sub">Artist</span>
              </span>
            </button>
          {/each}
        </div>
      </div>
    {/if}
  {/if}
</section>
