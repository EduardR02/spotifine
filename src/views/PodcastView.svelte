<script>
  import { tick, untrack } from "svelte";
  import { api, route, session, navigate, ui, playback, isPlayingSource, togglePlay, setPageTitle, watchCacheMarks } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { detailArtSize } from "../lib/layout.js";
  import { spotifyLink } from "../lib/spotify-link.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import HeaderMenu from "../components/HeaderMenu.svelte";
  import CopyLinkItem from "../components/CopyLinkItem.svelte";
  import Menu from "../components/Menu.svelte";
  import { rowWindow } from "../lib/virtual.js";
  import { filterEpisodes, orderEpisodes, indexEpisodeSearch } from "../lib/episodes.js";
  import { applyCacheMarks } from "../lib/cache-marks.js";

  /**
   * A show and its episodes, or one episode — audio only, in the app's own
   * idiom: the record header every page has, and episode rows that behave
   * like track rows (the art plays, the title opens, the rest waits for the
   * pointer), rather than a column of filled buttons.
   */
  let response = $state(null);
  const data = $derived(response?.kind === route.name && response.id === route.id ? response.value : null);
  const isShow = $derived(route.name === "show");
  const record = $derived(isShow ? data : data?.track);
  let error = $state("");
  let busy = $state(false);
  let playError = $state("");
  let retry = $state(0);
  let expanded = $state(false);
  let descriptionBody = $state(null);
  let clipped = $state(false);
  let generation = 0;
  const publishedDate = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
  const tone = $derived(coverTone(record?.cover_url, record?.id || "podcast"));
  const artSize = $derived(detailArtSize(ui.paneWidth));
  let query = $state("");
  let sort = $state("recent");
  const SORTS = [
    { value: "recent", label: "Newest first" },
    { value: "oldest", label: "Oldest first" },
    { value: "title", label: "Episode title" },
  ];
  let sortOpen = $state(false);
  let sortButton = $state(null);
  // browseShow eagerly resolves the show's metadata-listed episodes. Only the
  // DOM is windowed below; search, ordering and playback use the whole result.
  const allEpisodes = $derived(isShow ? data?.episodes ?? [] : []);
  const episodeSearch = $derived(indexEpisodeSearch(allEpisodes));
  const orderedEpisodes = $derived(orderEpisodes(allEpisodes, sort));
  const episodes = $derived(filterEpisodes(orderedEpisodes, query, episodeSearch));
  const filtering = $derived(query.trim().length > 0);

  /* ---------------- Windowed episode rows ----------------
     A show can carry hundreds of episodes; only the rows near the viewport
     are in the DOM, at a fixed height, the way the track table does it. A
     focused row (and its neighbours) stays rendered while it holds focus. */
  const ROW_H = 112;
  let pageElement = $state(null);
  let listElement = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(12);
  let focusedEpisode = $state("");
  const focusedRow = $derived(focusedEpisode ? episodes.findIndex((episode) => episode.track.id === focusedEpisode) : -1);
  let currentFirst = 0;
  let currentLast = 12;
  const renderedRows = $derived.by(() => {
    const indices = [];
    const focusFirst = Math.max(0, focusedRow - 1);
    const focusLast = Math.min(episodes.length, focusedRow + 2);
    if (focusedRow >= 0) {
      for (let i = focusFirst; i < Math.min(firstRow, focusLast); i++) indices.push(i);
    }
    for (let i = firstRow; i < Math.min(lastRow, episodes.length); i++) indices.push(i);
    if (focusedRow >= 0) {
      for (let i = Math.max(lastRow, focusFirst); i < focusLast; i++) indices.push(i);
    }
    return indices;
  });
  function measureRows(scroller) {
    if (!listElement) return;
    const { first, last } = rowWindow(listElement, scroller, ROW_H, 6, episodes.length);
    if (first === currentFirst && last === currentLast) return;
    currentFirst = first;
    currentLast = last;
    firstRow = first;
    lastRow = last;
  }
  function retainFocus(event) {
    const row = event.target.closest("[data-episode-index]");
    if (row) focusedEpisode = row.dataset.episodeId;
  }
  function releaseFocus(event) {
    if (!listElement?.contains(event.relatedTarget)) focusedEpisode = "";
  }
  $effect(() => {
    const list = listElement;
    const page = pageElement;
    if (!list || !page) return;
    const scroller = list.closest(".scroll");
    if (!scroller) return;
    let frame = 0;
    const schedule = () => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        measureRows(scroller);
      });
    };
    scroller.addEventListener("scroll", schedule, { passive: true });
    const observer = new ResizeObserver(schedule);
    observer.observe(scroller);
    observer.observe(page);
    untrack(() => measureRows(scroller));
    return () => {
      scroller.removeEventListener("scroll", schedule);
      observer.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  });
  $effect(() => {
    episodes;
    let active = true;
    // Measure after the filtered list's height reaches the DOM, including when
    // an empty result removes it and clearing the search mounts it again.
    tick().then(() => {
      if (!active) return;
      const scroller = listElement?.closest(".scroll");
      if (scroller) measureRows(scroller);
    });
    return () => { active = false; };
  });

  function publication(value) {
    if (value == null) return "";
    const date = new Date(value);
    return Number.isFinite(date.getTime()) ? publishedDate.format(date) : "";
  }
  /** "1 hr 20 min", "48 min": an episode's length is read, not timed. */
  function length(ms) {
    const minutes = Math.max(1, Math.round((Number(ms) || 0) / 60000));
    const h = Math.floor(minutes / 60);
    const m = minutes % 60;
    return h ? (m ? `${h} hr ${m} min` : `${h} hr`) : `${m} min`;
  }
  function unavailable(episode) {
    return !episode.track || episode.track.unavailable;
  }
  /** The description's own paragraphs; single line breaks survive inside them. */
  const paragraphs = $derived(
    String(data?.description ?? "").split(/\n\s*\n/).map((part) => part.trim()).filter(Boolean),
  );

  $effect(() => {
    const body = descriptionBody;
    data?.description;
    if (!body) return;
    const measure = () => { clipped = expanded || body.scrollHeight - body.clientHeight > 2; };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(body);
    return () => observer.disconnect();
  });
  $effect(() => {
    const kind = route.name;
    const id = route.id;
    const account = session.username;
    const attempt = retry;
    generation++;
    focusedEpisode = "";
    query = "";
    sort = "recent";
    currentFirst = firstRow = 0;
    currentLast = lastRow = 12;
    response = null;
    error = "";
    playError = "";
    expanded = false;
    busy = false;
    if (!account) {
      error = "Sign in to Spotify to browse podcasts.";
      return;
    }
    let active = true;
    busy = true;
    const load = kind === "show" ? api.browseShow(id) : api.browseEpisode(id);
    load.then((value) => {
      if (!(kind === "show" ? value?.id && Array.isArray(value.episodes) : value?.track?.id)) throw new Error("Spotify returned incomplete podcast details.");
      if (active) response = { kind, id, value };
    })
      .catch((reason) => { if (active) error = String(reason); })
      .finally(() => { if (active) busy = false; });
    return () => { active = false; };
  });
  $effect(() => {
    if (record?.name) untrack(() => setPageTitle(record.name));
  });

  /* ---------------- Playback ---------------- */
  const playingUri = $derived(playback.current_uri);
  const playableEpisodes = $derived(episodes.filter((episode) => !unavailable(episode)));
  const source = $derived(isShow ? `show:${data?.id ?? ""}` : `episode:${data?.track?.id ?? ""}`);
  const playingHere = $derived(isShow ? isPlayingSource(source) : !!record?.uri && record.uri === playingUri);

  $effect(() => watchCacheMarks((ids, cached) => {
    if (isShow) {
      for (const episode of allEpisodes) {
        if (episode.track) applyCacheMarks([episode.track], ids, cached);
      }
    } else if (record) applyCacheMarks([record], ids, cached);
  }));

  async function startEpisodeQueue(episode) {
    if (unavailable(episode)) return;
    const started = generation;
    playError = "";
    try {
      const available = isShow ? playableEpisodes : [episode];
      await api.playQueue(available.map((item) => item.track), available.indexOf(episode), source);
    } catch (reason) {
      if (started === generation) playError = String(reason);
    }
  }
  function play(episode) {
    if (unavailable(episode)) return;
    if (episode.track?.uri === playingUri) togglePlay();
    else startEpisodeQueue(episode);
  }
  async function enqueue(episode) {
    if (unavailable(episode)) return;
    const started = generation;
    playError = "";
    try {
      await api.addQueue(episode.track, `show:${episode.show_id}`);
    } catch (reason) {
      if (started === generation) playError = String(reason);
    }
  }
  /** Start at the first playable displayed episode, then follow that order. */
  function playShow() {
    if (playingHere) {
      togglePlay();
      return;
    }
    const first = playableEpisodes[0];
    if (first) startEpisodeQueue(first);
  }

  /* One row menu for the whole list, hung off the row's "…". */
  let rowMenu = $state(null);
  $effect(() => {
    route.id;
    query;
    sort;
    untrack(() => (rowMenu = null));
  });
</script>

<section
  bind:this={pageElement}
  aria-busy={busy}
  class="view page wash podcast-page"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if busy}
    <header class="detail-head" role="status" aria-label="Loading podcast details">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="skeleton line sm" style="width:72px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(440px,72%)"></span>
        <span class="skeleton line sm" style="width:180px"></span>
        <div class="actions">
          <span class="skeleton" style="width:48px;height:48px;border-radius:var(--rf)"></span>
          <span class="skeleton" style="width:40px;height:40px;border-radius:var(--rf)"></span>
        </div>
      </div>
    </header>
  {:else if error}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="tag">{isShow ? "Podcast" : "Episode"}</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This {isShow ? "podcast" : "episode"} could not be loaded.</p>
      <p class="why">{error}</p>
      <div class="actions">
        <button class="pill" onclick={() => retry++}>Try again</button>
        <button class="link-more" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if data}
    <header class="detail-head">
      <Cover src={record.cover_url || ""} id={record.id} name={record.name} size={artSize} lg raised />
      <div>
        <span class="tag">{isShow ? "Podcast" : "Episode"}</span>
        <h1 class="detail-title podcast-title" class:long={record.name.length > 44}>{record.name}</h1>
        <p class="detail-meta">
          {#if isShow}
            {#if data.publisher}<span class="who">{data.publisher}</span><span class="sep">/</span>{/if}
            <span class="num">{allEpisodes.length} {allEpisodes.length === 1 ? "episode" : "episodes"}</span>
          {:else}
            {#if data.show_id}
              <button class="who" title="Go to podcast" onclick={() => navigate("show", data.show_id)}>{data.show_name || "Podcast"}</button>
              <span class="sep">/</span>
            {/if}
            {#if publication(data.published_at)}<span class="num">{publication(data.published_at)}</span><span class="sep">/</span>{/if}
            <span class="num">{length(record.duration_ms)}</span>
          {/if}
        </p>
        <div class="actions">
          {#if isShow}
            <button
              class="play-lg"
              title={playingHere ? (playback.playing ? "Pause" : "Resume") : "Play episodes in displayed order"}
              disabled={!playableEpisodes.length && !playingHere}
              onclick={playShow}
            >
              <Icon name={playingHere && playback.playing ? "pause" : "play"} size={22} />
            </button>
            <HeaderMenu label="Podcast actions">
              {#snippet children(close)}
                <CopyLinkItem link={spotifyLink("show", data.id)} {close} />
              {/snippet}
            </HeaderMenu>
          {:else}
            <button
              class="play-lg"
              title={playingHere ? (playback.playing ? "Pause" : "Resume") : "Play"}
              disabled={unavailable(data)}
              onclick={() => play(data)}
            >
              <Icon name={playingHere && playback.playing ? "pause" : "play"} size={22} />
            </button>
            <button class="btn-round lg" title="Add to queue" aria-label="Add to queue" disabled={unavailable(data)} onclick={() => enqueue(data)}>
              <Icon name="queue-add" size={20} />
            </button>
            <HeaderMenu label="Episode actions">
              {#snippet children(close)}
                {#if data.show_id}
                  <button class="menu-item" role="menuitem" onclick={() => { close(); navigate("show", data.show_id); }}><Icon name="podcast" size={16} />Go to podcast</button>
                {/if}
                <CopyLinkItem link={spotifyLink("episode", record.id)} {close} />
              {/snippet}
            </HeaderMenu>
          {/if}
        </div>
        {#if !isShow && unavailable(data)}
          <p class="start-blocked">{record.unavailable_reason || "Audio is unavailable for this episode."}</p>
        {/if}
        {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}
      </div>
    </header>

    {#if isShow}
      {#if data.description}
        <!-- Three lines of what the show is, and the rest on request. -->
        <div class="show-about">
          <p bind:this={descriptionBody} class:expanded>{data.description}</p>
          {#if clipped}
            <button class="link-more" aria-expanded={expanded} onclick={() => (expanded = !expanded)}>{expanded ? "Show less" : "Read more"}</button>
          {/if}
        </div>
      {/if}
      <section class="section episodes">
        <div class="section-head episode-tools">
          <h2 class="section-title">Episodes <span class="section-count">{allEpisodes.length}</span></h2>
          <!-- Finding and ordering, quiet at the head's far end: the field,
               and the order behind one round glyph rather than a select. -->
          <span class="episode-controls">
          <span class="field sm episode-search">
            <Icon name="search" size={14} />
            <input
              bind:value={query}
              aria-label="Search show episodes"
              placeholder="Title or description"
              spellcheck="false"
              onkeydown={(event) => { if (event.key === "Escape") query = ""; }}
            />
            {#if query}
              <button class="field-btn" type="button" aria-label="Clear episode search" title="Clear episode search" onclick={() => (query = "")}>
                <Icon name="x" size={11} />
              </button>
            {/if}
          </span>
          <button
            class="btn-round"
            class:on={sort !== "recent"}
            bind:this={sortButton}
            title={`Order · ${SORTS.find((option) => option.value === sort)?.label}`}
            aria-label="Order episodes"
            aria-haspopup="menu"
            aria-expanded={sortOpen}
            onclick={() => (sortOpen = !sortOpen)}
          >
            <Icon name="sort" size={17} />
          </button>
          </span>
        </div>
        {#if sortOpen}
          <Menu anchor={sortButton} align="end" label="Order episodes" onclose={() => (sortOpen = false)}>
            {#snippet children(close)}
              {#each SORTS as option (option.value)}
                <button
                  class="menu-item"
                  role="menuitemradio"
                  aria-checked={sort === option.value}
                  onclick={() => { sort = option.value; close(true); }}
                >
                  {option.label}
                  {#if sort === option.value}<span class="mark sort-mark"><Icon name="check" size={14} /></span>{/if}
                </button>
              {/each}
            {/snippet}
          </Menu>
        {/if}
        {#if filtering}
          <p class="sub episode-results" role="status">{episodes.length} of {allEpisodes.length} episodes match your search.</p>
        {/if}
        {#if !episodes.length}
          {#if filtering}
            <div class="empty"><p class="h">No matching episodes.</p><p class="sub">Try a different title or description.</p></div>
          {:else}
            <div class="empty"><p class="h">No episodes yet.</p><p class="sub">This podcast has nothing to play.</p></div>
          {/if}
        {:else}
          <div
            class="episode-list"
            bind:this={listElement}
            style:height={`${episodes.length * ROW_H}px`}
            style:--row={`${ROW_H}px`}
            onfocusin={retainFocus}
            onfocusout={releaseFocus}
          >
            {#each renderedRows as index (episodes[index].track.id)}
              {@const episode = episodes[index]}
              {@const date = publication(episode.published_at)}
              {@const current = !!episode.track?.uri && episode.track.uri === playingUri}
              <article
                class="ep-row"
                class:current
                class:unavailable={unavailable(episode)}
                data-episode-index={index}
                data-episode-id={episode.track.id}
                style:top={`${index * ROW_H}px`}
              >
                <span class="ep-art">
                  <Cover src={episode.track.cover_url || data.cover_url || ""} id={episode.track.id} name={episode.track.name} size={60} />
                  {#if !unavailable(episode)}
                    <button
                      class="ep-play"
                      title={current ? (playback.playing ? "Pause" : "Resume") : "Play"}
                      aria-label={`${current ? (playback.playing ? "Pause" : "Resume") : "Play"} ${episode.track.name}`}
                      onclick={() => play(episode)}
                    >
                      <span class="ep-play-disc"><Icon name={current && playback.playing ? "pause" : "play"} size={14} /></span>
                    </button>
                  {/if}
                </span>
                <div class="ep-copy">
                  <button class="ep-title" title={episode.track.name} onclick={() => navigate("episode", episode.track.id)}>{episode.track.name}</button>
                  <p class="ep-meta">
                    {#if date}<span>{date}</span><span class="ep-dot" aria-hidden="true"></span>{/if}
                    <span>{length(episode.track.duration_ms)}</span>
                    {#if episode.track?.cached}
                      <span class="t-cached" title="Downloaded — this plays from the local cache"><Icon name="cached" size={13} /><span class="sr-only">Downloaded</span></span>
                    {/if}
                    {#if unavailable(episode)}
                      <span class="ep-dot" aria-hidden="true"></span><span class="ep-why">{episode.track.unavailable_reason || "Audio unavailable"}</span>
                    {/if}
                  </p>
                  {#if episode.description}<p class="ep-desc">{episode.description}</p>{/if}
                </div>
                <div class="ep-actions">
                  <button class="btn-round" title="Add to queue" aria-label={`Add ${episode.track.name} to queue`} disabled={unavailable(episode)} onclick={() => enqueue(episode)}>
                    <Icon name="queue-add" size={17} />
                  </button>
                  <button
                    class="btn-round"
                    title="More"
                    aria-label={`More for ${episode.track.name}`}
                    aria-haspopup="menu"
                    aria-expanded={rowMenu?.episode === episode}
                    onclick={(event) => (rowMenu = rowMenu?.episode === episode ? null : { episode, anchor: event.currentTarget })}
                  >
                    <Icon name="more" size={17} />
                  </button>
                </div>
              </article>
            {/each}
          </div>
        {/if}
      </section>
    {:else if paragraphs.length}
      <section class="section episode-about">
        <div class="section-head"><h2 class="section-title">About</h2></div>
        <div class="about">
          {#each paragraphs as paragraph, i (i)}<p>{paragraph}</p>{/each}
        </div>
      </section>
    {/if}
  {/if}
</section>

{#if rowMenu}
  <Menu anchor={rowMenu.anchor} align="end" label="Episode actions" onclose={() => (rowMenu = null)}>
    {#snippet children(close)}
      <button class="menu-item" role="menuitem" onclick={() => { const id = rowMenu.episode.track.id; close(); navigate("episode", id); }}><Icon name="episode" size={16} />Open episode</button>
      <CopyLinkItem link={spotifyLink("episode", rowMenu.episode.track.id)} {close} />
    {/snippet}
  </Menu>
{/if}

<style>
  /* An episode's name can be a sentence: at most three lines of it. */
  .podcast-title {
    text-wrap: balance;
    display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden;
  }
  /* The show's description: three lines, then Read more. */
  .show-about { max-width: 68ch; margin: 0 0 var(--s2); }
  .show-about p {
    margin: 0 0 var(--s2);
    color: var(--fg-1); font-size: var(--t-13); line-height: 1.6; overflow-wrap: anywhere;
    display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden;
  }
  .show-about p.expanded { display: block; white-space: pre-line; }
  .episodes { margin-top: var(--s7); }
  .episode-list { position: relative; }
  .episode-tools { align-items: center; flex-wrap: wrap; }
  .episode-controls { display: flex; align-items: center; gap: var(--s2); margin-left: auto; }
  .episode-search { width: 240px; }
  .sort-mark { color: var(--accent); }
  .episode-results { margin: 0 0 var(--s3); font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-3); }

  /* ---- An episode row: the track row's idiom at an episode's size. ---- */
  .ep-row {
    position: absolute; left: 0; right: 0; height: var(--row);
    display: grid; grid-template-columns: 60px minmax(0, 1fr) auto;
    align-items: center; gap: var(--s4);
    padding: 0 var(--s3);
    border-radius: var(--r2);
    transition: background-color var(--d1) var(--ease);
  }
  .ep-row:hover, .ep-row:focus-within { background: var(--hover); }
  /* The playing episode is marked exactly as the playing track row is. */
  .ep-row.current {
    background: linear-gradient(90deg, var(--accent-wash), transparent 46%);
    box-shadow: inset 2px 0 var(--accent);
  }
  .ep-row.current:hover, .ep-row.current:focus-within {
    background: linear-gradient(90deg, var(--accent-wash), transparent 46%), var(--hover);
  }
  .ep-row.current .ep-title { color: var(--accent); }

  /* The art plays, as a card's does: a foam disc rises into it under the
     pointer, over the picture dimmed a step. The playing episode keeps its. */
  .ep-art { position: relative; width: 60px; height: 60px; border-radius: var(--r1); }
  .ep-art :global(.art) { transition: filter var(--d1) var(--ease); }
  .ep-row:hover .ep-art :global(.art), .ep-row:focus-within .ep-art :global(.art),
  .ep-row.current .ep-art :global(.art) { filter: brightness(0.62); }
  .ep-play {
    position: absolute; inset: 0; display: grid; place-items: center;
    border-radius: inherit; opacity: 0;
    transition: opacity var(--d1) var(--ease);
  }
  .ep-row:hover .ep-play, .ep-play:focus-visible, .ep-row.current .ep-play { opacity: 1; }
  .ep-play-disc {
    display: grid; place-items: center; width: 30px; height: 30px;
    border-radius: var(--rf); background: var(--accent); color: var(--accent-ink);
    box-shadow: 0 6px 14px -4px rgba(0, 0, 0, 0.7);
    transition: background-color var(--d1) var(--ease), transform var(--d1) var(--ease);
  }
  .ep-play:hover .ep-play-disc { background: var(--accent-hi); }
  .ep-play:active .ep-play-disc { transform: scale(0.93); }

  .ep-copy { min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .ep-title {
    min-width: 0; max-width: 100%; text-align: left;
    color: var(--fg); font-size: var(--t-13); font-weight: var(--w-med); line-height: 1.35;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    transition: color var(--d1) var(--ease);
  }
  .ep-title:hover { text-decoration: underline; text-decoration-color: var(--line-2); text-underline-offset: 3px; }
  .ep-meta {
    display: flex; align-items: center; gap: var(--s2); min-width: 0;
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-11);
    font-variant-numeric: tabular-nums; white-space: nowrap;
  }
  .ep-dot { width: 3px; height: 3px; border-radius: 50%; background: var(--fg-3); flex: none; }
  .ep-why { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .ep-desc {
    margin-top: 2px;
    color: var(--fg-3); font-size: var(--t-12); line-height: 1.5; overflow-wrap: anywhere;
    display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow: hidden;
  }
  .ep-row:hover .ep-desc { color: var(--fg-2); }

  /* Queue and "…" wait for the pointer or the keyboard, like a row's kebab. */
  .ep-actions { display: flex; align-items: center; gap: 2px; opacity: 0; transition: opacity var(--d1) var(--ease); }
  .ep-row:hover .ep-actions, .ep-row:focus-within .ep-actions { opacity: 1; }
  .ep-actions:has([aria-expanded="true"]) { opacity: 1; }

  .ep-row.unavailable .ep-art, .ep-row.unavailable .ep-title, .ep-row.unavailable .ep-desc { opacity: 0.5; }

  /* ---- The episode's About: a readable column, its paragraphs kept. ---- */
  .episode-about { max-width: 68ch; }
  .about p {
    margin: 0 0 var(--s4);
    color: var(--fg-1); font-size: var(--t-13); line-height: 1.7;
    white-space: pre-line; overflow-wrap: anywhere;
    user-select: text;
  }
  .about p:last-child { margin-bottom: 0; }
</style>
