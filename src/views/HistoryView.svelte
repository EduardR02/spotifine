<script>
  import { untrack } from "svelte";
  import { api, library, ui } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import Select from "../components/Select.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import { formatTime } from "../lib/time.js";
  import { observeStuck } from "../lib/sticky.js";
  import { rowWindow } from "../lib/virtual.js";
  import { localDayRelation } from "../lib/history-date.js";

  /* Must track --row-h, the same way TrackList's and QueueView's constants do.
     History used to be the one list in the app at 56px, which is a difference
     of eight pixels per row against every other list the reader has just come
     from — too small to read as a decision and large enough that the page
     feels slacker than the app around it. An archive also wants rows on the
     screen, and the extra height was buying nothing: the tallest cell here is
     a 36px sleeve beside two lines of type, exactly the track table's row. */
  const ROW_H = 48;
  const OVERSCAN = 6;
  /* One request covers several screens, so an ordinary scroll never waits on
     one, and a flick lands inside a page that is already in flight. It is also
     the engine's own default page size — see default_history_page_size. */
  const PAGE = 100;
  /* Long enough that typing a word is one request, short enough that the list
     answers while the finger is still on the key. */
  const FILTER_DEBOUNCE_MS = 180;
  const SORTS = [
    { value: "recent", label: "Newest first" },
    { value: "oldest", label: "Oldest first" },
    { value: "title", label: "Song title" },
    { value: "artist", label: "Artist" },
  ];

  /* =====================================================================
     THE WINDOW ONTO THE ARCHIVE

     The engine holds the archive and answers filtered, ordered pages of it;
     this view holds only the pages its scroll position has actually reached.
     That is the whole reason the filter and the sort travel to the engine
     rather than running here: a local filter would have to pull every row
     back to answer, which is the thing paging exists to prevent.

     Pages are kept by page index rather than as one sparse row array, so an
     archive of fifty thousand rows costs a handful of object keys instead of
     fifty thousand reactive slots.
     ===================================================================== */
  let pages = $state({});
  let total = $state(0);
  let recorded = $state(0);
  let loaded = $state(false);
  let error = $state("");
  const inflight = new Set();
  /* Bumped whenever the filter or the order changes. A page that answers to an
     older generation describes a list that is no longer on screen. */
  let generation = 0;
  let committedQuery = "";
  let committedSort = $state("recent");
  $effect(() => () => {
    generation += 1;
    inflight.clear();
  });

  let query = $state("");
  let sort = $state("recent");
  let confirmClear = $state(false);
  let clearing = $state(false);
  let clearError = $state("");

  let bodyEl = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(0);
  /* The row directly under the sticky block — what its running day names. */
  let topRow = $state(0);
  let curFirst = 0;
  let curLast = 0;
  let resetFrame = 0;
  let focusedKey = $state(null);
  function entryKey(entry, index) {
    return JSON.stringify([entry.started_at, entry.track_id ?? entry.track.uri, entry.context, index]);
  }
  // Only loaded pages participate; retaining focus never fetches the archive.
  const focusedRow = $derived.by(() => {
    if (focusedKey === null) return -1;
    for (const [page, entries] of Object.entries(pages)) {
      const offset = entries.findIndex((entry, offset) => entryKey(entry, Number(page) * PAGE + offset) === focusedKey);
      if (offset >= 0) return Number(page) * PAGE + offset;
    }
    return -1;
  });
  const renderedIndices = $derived.by(() => {
    const indices = [];
    if (focusedRow >= 0 && focusedRow < firstRow) indices.push(focusedRow);
    for (let index = firstRow; index < lastRow; index++) indices.push(index);
    if (focusedRow >= lastRow) indices.push(focusedRow);
    return indices;
  });
  function retainRowFocus(event) {
    const row = event.target.closest("[data-history-key]");
    if (row) focusedKey = row.dataset.historyKey;
  }
  function releaseRowFocus(event) {
    if (!bodyEl?.contains(event.relatedTarget)) focusedKey = null;
  }
  let debounceTimer = 0;

  /* ONE day formatter, used by the row that opens a day and by the running day
     in the sticky head, because they are the same label in two places. There
     used to be two — a long one for the head and a short one for the row —
     which is how "Today" ended up printed twice, in two different sizes,
     within 40px of each other at the top of the list. */
  const dayFormatter = new Intl.DateTimeFormat(undefined, { weekday: "short", day: "numeric", month: "short" });
  const yearDayFormatter = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });
  /* `2-digit`, not `numeric`. A column holding both "9:19" and "19:23" is a
     column with two shapes in it and no alignment; padding the hour is what
     makes the times a column rather than a list of strings. It pads in every
     locale and forces no clock system on one — en-GB gives 09:19, en-US gives
     09:19 AM. */
  const timeFormatter = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
  const countFormatter = new Intl.NumberFormat();

  function startOfDay(timestamp) {
    const date = new Date(timestamp);
    return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  }

  function sameDay(a, b) {
    return startOfDay(a) === startOfDay(b);
  }

  /** The day a run of plays belongs to, written the way a diary writes it. */
  function dayLabel(timestamp) {
    const relation = localDayRelation(timestamp);
    if (relation) return relation;
    const date = new Date(timestamp);
    if (date.getFullYear() === new Date().getFullYear()) return dayFormatter.format(date);
    return yearDayFormatter.format(date);
  }

  function clock(timestamp) {
    return timeFormatter.format(new Date(timestamp));
  }

  /* =====================================================================
     WHERE A PLAY CAME FROM

     The column used to render the KIND of source as a bordered pill —
     "Playlist", twenty times down the page, because playlists are what this
     listener plays from. A column that says the same word on every row is a
     column carrying no information, and it was taking a whole grid track to
     do it.

     Naming the actual playlist is what makes the answer worth a column, and
     the library already holds the names, so no request is added for it. A
     playlist that is no longer in the library (or an album/artist/radio, whose
     names a play does not carry) falls back to the kind — one generic word for
     the handful of rows that need it, rather than for all of them.
     ===================================================================== */
  const KINDS = { playlist: "Playlist", album: "Album", artist: "Artist", radio: "Radio", show: "Podcast" };
  // The source column can contain a screenful of playlist contexts. Index
  // the library once per change instead of scanning it for every visible row.
  const playlistById = $derived(new Map(library.map((entry) => [entry.id, entry])));


  function sourceLabel(context) {
    if (!context) return "";
    if (context === "liked") return "Liked Songs";
    if (context === "search") return "Search";
    if (context === "history") return "History";
    const separator = context.indexOf(":");
    const kind = separator === -1 ? context : context.slice(0, separator);
    const id = separator === -1 ? "" : context.slice(separator + 1);
    if (kind === "playlist" && id) {
      const playlist = playlistById.get(id);
      if (playlist?.name) return playlist.name;
    }
    return KINDS[kind] || "Queue";
  }

  /* How much of the song this play actually got through, as one number for
     BOTH kinds of play. A completed play is simply a full rail — see the
     `.hi-rail` block for why the column no longer speaks two languages. */
  function heardPct(entry) {
    if (entry.completed) return 100;
    const duration = entry.track.duration_ms || 0;
    if (duration <= 0) return 0;
    return Math.min(100, Math.max(2, Math.round((entry.ms_played / duration) * 100)));
  }

  function heardTitle(entry) {
    const heard = formatTime(entry.ms_played);
    const whole = formatTime(entry.track.duration_ms);
    return entry.completed ? `Played in full — ${whole}` : `Skipped — ${heard} of ${whole} heard`;
  }

  function artistOf(entry) {
    return (entry.track.artist_names ?? []).join(", ");
  }

  function entryAt(index) {
    if (index < 0 || index >= total) return null;
    const page = pages[Math.floor(index / PAGE)];
    return page ? (page[index % PAGE] ?? null) : null;
  }

  /* Only a chronological order puts a day's plays next to each other, so only
     a chronological order can be grouped by day. Sorted by name, a date is a
     property of the row and is written out on every one of them. */
  const grouped = $derived(committedSort === "recent" || committedSort === "oldest");
  const filtering = $derived(query.trim().length > 0);
  /* Loading counts as having a list: the skeleton rows stand under the same
     column head, so the head is never inserted or removed mid-load. */
  const hasList = $derived(!error && (!loaded || total > 0));

  /* =====================================================================
     COLUMNS

     The same shape as the track table's, deliberately: a small quiet column,
     the artwork, the title carrying the row, one context column, and a
     trailing measure. History's small quiet leading column is the CLOCK,
     because the clock is what identifies a row here in the way an ordinal
     identifies a row in a playlist — this is a journal, and a journal is read
     down its dates.

     Moving it there is also what fixed the balance. With the time on the right
     the row ran title, then 200px of nothing, then three narrow cells jammed
     against the pane edge. Now the title has the middle, and both edges carry
     something the width of a word.

     Measured against the PANE, not the window — the same reason the track
     table does: the space this list has is the window minus the rail, minus
     the inspector when it is open, minus the gutters, and a viewport media
     query doing that arithmetic by hand gets it wrong.

     DROP ORDER, context before identity: the source goes first, then the
     measure. The clock, the artwork and the title never leave.

     Each threshold is the width at which its column stops fitting — 88 + 36 +
     a 200px title + the column itself + the gaps + the row's padding — rather
     than a round number. They were 700 and 560, both about a hundred pixels
     early, which is what left a narrow pane showing a title and then a third
     of the row in blank.
     ===================================================================== */
  const NEEDS = { source: 660, played: 500 };
  const pane = $derived(ui.paneWidth || 1200);
  const colSource = $derived(pane >= NEEDS.source);
  const colPlayed = $derived(pane >= NEEDS.played);
  /* The instrument row answers to the same measure and stays ONE line at
     every width: a second line would grow the sticky block under the reader.
     What gives way is ordered. First the word on "Clear history" — the
     glyph is the same act behind the same confirmation — then the count,
     whose room the filter takes over. Each threshold is where its item
     stops fitting: a ~130px count, the 300px filter, the 140px sort, the
     103px pill or 32px glyph, three 16px gaps and the pane's two 24px pads. */
  const TOOLS_NEED = { clearLabel: 770, summary: 700 };
  const clearLabelled = $derived(pane >= TOOLS_NEED.clearLabel);
  const showSummary = $derived(pane >= TOOLS_NEED.summary);
  const dense = $derived(pane < 760);
  const cols = $derived(
    ["88px", "36px", "minmax(0, 1fr)", colSource && "minmax(0, 0.58fr)", colPlayed && "108px"]
      .filter(Boolean)
      .join(" "),
  );

  /**
   * The rows the window covers, each tagged with how it opens.
   * A row can only know either from its neighbour, so the loader below always
   * requests one row further back than the window needs.
   */
  const visible = $derived.by(() => {
    const out = [];
    for (const index of renderedIndices) {
      const entry = entryAt(index);
      const previous = entryAt(index - 1);
      const opensDay =
        grouped && !!entry && (index === 0 || (!!previous && !sameDay(previous.started_at, entry.started_at)));
      out.push({
        index,
        entry,
        opensDay,
        /* The first row of the list opens a day like any other and carries its
           date, but it must not draw the day's rule: the column head closes
           itself with a hairline eight pixels above, and two rules that close
           on nothing between them is the doubled edge that made the top of
           this page read as broken. A rule separates days; there is nothing
           above this one to separate it from. */
        headsList: index === 0,
        /* Playing the same song twice running is ordinary behaviour and the
           archive keeps every one of them. Adjacency only MEANS that in a
           chronological order; sorted by name, two identical neighbours are an
           artifact of the sort and get no treatment. */
        repeat:
          grouped && !opensDay && !!entry && !!previous && previous.track.uri === entry.track.uri,
      });
    }
    return out;
  });

  const headDay = $derived.by(() => {
    if (!grouped || !total) return "";
    const entry = entryAt(Math.min(topRow, total - 1));
    return entry ? dayLabel(entry.started_at) : "";
  });

  /* =====================================================================
     LOADING

     One request per page, at most one in flight per page, and every answer
     checked against the generation that asked for it.
     ===================================================================== */
  async function fetchPage(index, mine) {
    if (inflight.has(index) || pages[index]) return;
    inflight.add(index);
    try {
      const page = await api.getHistory(index * PAGE, PAGE, committedQuery, committedSort);
      if (mine !== generation) return;
      // Offsets from different archive snapshots cannot share one page set.
      const changed = loaded && page.recorded !== recorded;
      if (changed) {
        generation += 1;
        focusedKey = null;
        inflight.clear();
        pages = {};
      }
      total = page.total;
      recorded = page.recorded;
      pages[index] = page.entries;
      loaded = true;
      error = "";
      if (changed) loadWindow();
    } catch (reason) {
      if (mine !== generation) return;
      error = String(reason || "Could not load listening history.");
      loaded = true;
    } finally {
      // A stale generation must not remove the newer request for this index
      // after reload() has cleared and reused the in-flight set.
      if (mine === generation) inflight.delete(index);
    }
  }

  /** Requests every page the window touches, plus the row that precedes it. */
  function loadWindow() {
    const mine = generation;
    const from = Math.max(0, firstRow - 1);
    const to = loaded ? Math.min(lastRow, total) : 1;
    const first = Math.floor(from / PAGE);
    const last = Math.max(first, Math.floor(Math.max(from, to - 1) / PAGE));
    for (let index = first; index <= last; index += 1) fetchPage(index, mine);
  }

  /** Starts the list over: a new filter or order is a different list. */
  function reload() {
    generation += 1;
    focusedKey = null;
    inflight.clear();
    committedQuery = query.trim();
    committedSort = sort;
    pages = {};
    total = 0;
    loaded = false;
    error = "";
    resetWindow();
    loadWindow();
  }

  /* The filter and the order are part of the request, so changing either is a
     different list and starts the walk over. The first run is the mount: there
     is nothing to start over from, only a first page to ask for. */
  let started = false;
  let lastSort = "recent";
  $effect(() => {
    // Both are read so the effect depends on both; the values themselves
    // travel inside reload().
    void query;
    const order = sort;
    if (!started) {
      started = true;
      committedQuery = query.trim();
      committedSort = order;
      untrack(loadWindow);
      return;
    }
    // Choosing an order is one deliberate click and answers at once. A filter
    // arrives a character at a time, and each character restarts the wait, so
    // typing a word costs one request rather than one per key.
    const orderChanged = order !== lastSort;
    lastSort = order;
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(
      () => {
        debounceTimer = 0;
        untrack(reload);
      },
      orderChanged ? 0 : FILTER_DEBOUNCE_MS,
    );
    return () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = 0;
    };
  });

  /* =====================================================================
     WINDOWING
     ===================================================================== */
  function measure(scroller) {
    if (!bodyEl || !scroller) return;
    const length = loaded ? total : 0;
    const { first, last } = rowWindow(bodyEl, scroller, ROW_H, OVERSCAN, length);
    /* Which row the sticky block is standing on, measured from the BLOCK's
       bottom edge rather than the scroller's top. It used to be the scroller's
       top, which is three rows above what the head is actually covering — so
       the running day changed to the next day while that day's first rows were
       still hidden underneath it. One rect read, in a frame that already takes
       two. */
    const above = (blockEl?.getBoundingClientRect().bottom ?? 0) - bodyEl.getBoundingClientRect().top;
    topRow = Math.min(Math.max(0, Math.ceil(above / ROW_H)), Math.max(0, length - 1));
    if (first === curFirst && last === curLast) return;
    curFirst = first;
    curLast = last;
    firstRow = first;
    lastRow = last;
    loadWindow();
  }

  function resetWindow() {
    curFirst = 0;
    curLast = 0;
    firstRow = 0;
    lastRow = 0;
    topRow = 0;
    const scroller = bodyEl?.closest(".scroll");
    if (!scroller) return;
    scroller.scrollTop = 0;
    if (resetFrame) cancelAnimationFrame(resetFrame);
    resetFrame = requestAnimationFrame(() => {
      resetFrame = 0;
      measure(scroller);
    });
  }

  $effect(() => {
    total;
    loaded;
    if (!bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) return;
    let scrollFrame = 0;
    const onScroll = () => {
      if (scrollFrame) return;
      scrollFrame = requestAnimationFrame(() => {
        scrollFrame = 0;
        measure(scroller);
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    const observer = new ResizeObserver(() => measure(scroller));
    observer.observe(scroller);
    untrack(() => measure(scroller));
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      observer.disconnect();
      if (scrollFrame) cancelAnimationFrame(scrollFrame);
      if (resetFrame) cancelAnimationFrame(resetFrame);
      resetFrame = 0;
    };
  });

  /* ONE sticky object, one sentinel, one state. The instrument row and the
     column head used to be two sticky elements at two different offsets, the
     lower one positioned from the measured height of the upper one and given
     its `stuck` state by an observer watching the WRONG offset — so for the
     first 56px of every scroll the column head was covering rows while still
     fully transparent, and once it did take its glass there were two
     translucent plates and two hairlines stacked where one object belonged.
     That stack is what the owner was seeing overlap. Merged, the block sticks
     at one offset, takes one material, and closes with one hairline. */
  let blockEl = $state(null);
  let blockSentinel = $state(null);
  let blockStuck = $state(false);
  $effect(() => observeStuck(blockSentinel, (stuck) => (blockStuck = stuck)));

  async function clearAll() {
    if (clearing) return;
    clearing = true;
    clearError = "";
    try {
      await api.clearHistory();
      query = "";
      recorded = 0;
      confirmClear = false;
      reload();
    } catch (reason) {
      clearError = String(reason || "Could not clear listening history.");
    } finally {
      clearing = false;
    }
  }

  function replay(entry) {
    api.playQueue([entry.track], 0, "history").catch(() => {});
  }
</script>

<!-- A row whose page has not arrived. It is the app's OWN skeleton row —
     `.sk-row` with this list's grid pushed into `--cols` — the same object
     Album, Artist and Discography stand up while they wait, rather than a
     fourth hand-built imitation of it. Same height and same tracks as a real
     row, so nothing moves when the page lands. -->
{#snippet placeholder(i)}
  <div class="hi-row sk-row" aria-hidden="true">
    <span class="sk" style="width:46px"></span>
    <span class="sk art"></span>
    <span class="sk-stack">
      <span class="sk a" style="width:{62 - ((i * 9) % 24)}%"></span>
      <span class="sk b" style="width:{30 - ((i * 5) % 11)}%"></span>
    </span>
    {#if colSource}<span class="sk" style="width:{86 - ((i * 13) % 34)}px"></span>{/if}
    <!-- Left-anchored, unlike the trailing bar on the album and artist tables:
         what arrives in this cell is the rail, and the rail starts at the
         cell's leading edge. -->
    {#if colPlayed}<span class="sk" style="width:52px"></span>{/if}
  </div>
{/snippet}

<section class="view page history-page" class:dense style:--hi-cols={cols}>
  <header class="page-head history-head">
    <h1 class="page-title">Listening history</h1>
    <p class="sub">Every song you actually listened to, kept only on this computer.</p>
  </header>

  <div class="history-block-sentinel" bind:this={blockSentinel} aria-hidden="true"></div>
  <!-- The instrument row and the column head, as one sticky object.

       Every control in the row has a FIXED width and the summary takes
       whatever is left (the filter does, once the summary has given way).
       It used to be the other way round, and the count's own
       text width drove the layout: typing in the filter moved the filter — up
       to 45px, on every keystroke, while the caret was in it. -->
  <div class="history-block" class:stuck={blockStuck} class:solo={!hasList} bind:this={blockEl}>
    <div class="history-tools" class:no-summary={!showSummary}>
      {#if showSummary}
        <p class="detail-meta history-summary" aria-live="polite">
          {#if error}
            History unavailable
          {:else if !loaded}
            Reading the archive…
          {:else if filtering}
            <span class="num">{countFormatter.format(total)}</span> of
            <span class="num">{countFormatter.format(recorded)}</span>
            {recorded === 1 ? "play" : "plays"}
          {:else}
            <span class="num">{countFormatter.format(recorded)}</span>
            {recorded === 1 ? "play" : "plays"} recorded
          {/if}
        </p>
      {/if}
      <!-- THE FIELD, the topbar's own object: same class, same pill, same
           focus behaviour. -->
      <div class="history-control history-filter-control">
        <span class="field">
          <Icon name="search" size={14} />
          <input
            bind:value={query}
            aria-label="Filter listening history"
            placeholder="Song or artist"
            spellcheck="false"
            disabled={!!error}
            onkeydown={(event) => event.key === "Escape" && (query = "")}
          />
          {#if query}
            <button class="field-btn" type="button" aria-label="Clear filter" title="Clear filter" onclick={() => (query = "")}>
              <Icon name="x" size={11} />
            </button>
          {/if}
        </span>
      </div>
      <div class="history-control history-sort-control">
        <Select
          label="Sort listening history"
          options={SORTS}
          value={sort}
          disabled={!!error}
          onchange={(value) => (sort = value)}
        />
      </div>
      <!-- A quiet pill that opens a destructive confirmation: neutral at rest,
           love under the pointer. Short of room it keeps only its glyph. -->
      {#if clearLabelled}
        <button
          class="pill warn"
          type="button"
          aria-label="Clear listening history"
          disabled={!loaded || !recorded}
          onclick={() => (confirmClear = true)}
        >
          Clear history
        </button>
      {:else}
        <button
          class="btn-round danger"
          type="button"
          title="Clear listening history"
          aria-label="Clear listening history"
          disabled={!loaded || !recorded}
          onclick={() => (confirmClear = true)}
        >
          <Icon name="trash" size={16} />
        </button>
      {/if}
    </div>

    {#if hasList}
      <div class="hi-head">
        <!-- WHILE STUCK the leading cell stops naming its column and states
             the day the rows underneath belong to — a running head, sitting in
             the very column that writes the day, at the same margin and in the
             same type. It used to be crammed into the narrow right-aligned
             time cell at the far edge of the page, where it displaced that
             column's name, collided with the bar above it, and at the top of
             the list simply repeated the date already on row one.

             At rest it says "When", because at rest row one is right there
             saying "Today" and nothing needs to say it twice. -->
        {#if grouped && blockStuck && headDay}
          <span class="hi-running-day" aria-live="polite">{headDay}</span>
        {:else}
          <!-- Also the state while the page under the head is still in flight:
               the day is not known yet, and a cell that empties for the length
               of a request is a hole in the furniture. It says "When" until
               there is a day to say. -->
          <span class="hi-when-head">When</span>
        {/if}
        <span class="hi-track-head">Track</span>
        {#if colSource}<span>Source</span>{/if}
        {#if colPlayed}<span class="hi-played-head">Played</span>{/if}
      </div>
    {/if}
  </div>

  {#if error}
    <div class="empty history-empty failed">
      <p class="h">History unavailable</p>
      <p class="why">{error}</p>
      <div class="actions"><button class="pill" onclick={reload}>Try again</button></div>
    </div>
  {:else if loaded && !total}
    <div class="empty history-empty">
      <p class="h">{filtering ? "No plays match that" : "Nothing played yet"}</p>
      <p>
        {filtering
          ? "Try another song title or artist."
          : "A song joins this list once you have listened to about half a minute of it."}
      </p>
      {#if filtering}
        <div class="actions">
          <button class="pill" type="button" onclick={() => (query = "")}>
            <Icon name="x" size={13} /> Clear filter
          </button>
        </div>
      {/if}
    </div>
  {:else}
    <div
      class="history-list"
      bind:this={bodyEl}
      onfocusin={retainRowFocus}
      onfocusout={releaseRowFocus}
      style="position: relative; overflow-anchor: none"
      style:height="{Math.max(loaded ? total : 10, 1) * ROW_H}px"
    >
      <div style="position: absolute; inset: 0 0 auto">
        {#if !loaded}
          {#each Array.from({ length: 12 }) as _, i (i)}
            {@render placeholder(i)}
          {/each}
        {:else}
          {#each visible as row (row.entry ? entryKey(row.entry, row.index) : `pending:${row.index}`)}
            <div style="position: absolute; left: 0; right: 0" style:top="{row.index * ROW_H}px">
            {#if !row.entry}
              <!-- A row inside the window whose page is still in flight. Same
                   height, same columns: a page landing never moves anything
                   already on screen. -->
              {@render placeholder(row.index)}
            {:else}
              <div
                class="hi-row"
                data-history-key={entryKey(row.entry, row.index)}
                class:opens-day={row.opensDay}
                class:heads-list={row.headsList}
                class:repeat={row.repeat}
              >
                <!-- The whole row is the button: one tab stop, replay from
                     anywhere on it, and the accessible name is the play itself. -->
                <button class="hi-main" title={`Play ${row.entry.track.name}`} onclick={() => replay(row.entry)}>
                  <time class="hi-when" datetime={new Date(row.entry.started_at).toISOString()}>
                    {#if row.opensDay || !grouped}
                      <span class="hi-day">{dayLabel(row.entry.started_at)}</span>
                    {/if}
                    <span class="hi-clock tnum">{clock(row.entry.started_at)}</span>
                  </time>
                  <span class="hi-art">
                    <Cover
                      src={row.entry.track.cover_url}
                      id={row.entry.track.album_id || row.entry.track.uri}
                      name={row.entry.track.name}
                      size={36}
                    />
                    <span class="hi-go" aria-hidden="true"><Icon name="play" size={13} /></span>
                  </span>
                  <span class="hi-copy">
                    <strong>{row.entry.track.name}</strong>
                    <span>{artistOf(row.entry)}</span>
                  </span>
                  {#if colSource}
                    <span class="hi-source">{sourceLabel(row.entry.context)}</span>
                  {/if}
                  {#if colPlayed}
                    <span class="hi-played" title={heardTitle(row.entry)}>
                      <span class="sr-only">{row.entry.completed ? "Played in full, " : "Partial play, "}</span>
                      <span class="hi-rail" aria-hidden="true"><i style:width="{heardPct(row.entry)}%"></i></span>
                      <span class="hi-heard tnum">{formatTime(row.entry.ms_played)}</span>
                    </span>
                  {/if}
                </button>
              </div>
            {/if}
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</section>

<ConfirmDialog
  open={confirmClear}
  title="Clear listening history?"
  message="This permanently removes every locally recorded play."
  confirmLabel="Clear history"
  busyLabel="Clearing…"
  busy={clearing}
  error={clearError}
  onConfirm={clearAll}
  onCancel={() => (confirmClear = false)}
/>


<style>
  /* The page is left-aligned and full width, like every other list in the
     app. It used to be `max-width: 1020px; margin: 0 auto`, and it was the
     only centred page there is: on any normal window the whole table floated
     in the middle of the pane with 200px of dead ground either side, under a
     topbar whose search field ran on to the real edge. Nothing else here does
     that, which is most of why the page read as belonging to a different
     application. The title stands at the same height as Queue's and
     Settings'. */
  .history-page { padding-top: var(--s3); --hi-gap: var(--s4); }
  /* At a narrow pane the gaps are a bigger share of the row than any column. */
  .history-page.dense { --hi-gap: var(--s3); }
  .history-head .sub {
    max-width: 520px;
    color: var(--fg-2); font-size: var(--t-13); line-height: 1.45;
  }

  /* =====================================================================
     THE STICKY BLOCK — instrument row plus column head, one object.

     At rest it is nothing at all: no fill, no rules, controls and tracked caps
     floating on the page exactly the way the track table's head does. Once
     rows are passing underneath it takes the same glass the topbar and the
     track head wear, and closes with a single hairline at its bottom edge.
     Sizes and alignment never move between the two states.
     ===================================================================== */
  .history-block-sentinel { height: 0; pointer-events: none; }
  .history-block {
    position: sticky; top: var(--topbar-h); z-index: 21;
    margin-bottom: var(--s2);
    background: transparent;
    transition: background-color var(--d2) var(--ease);
  }
  .history-block.stuck {
    background: var(--tint-strip);
    -webkit-backdrop-filter: var(--frost-strip);
            backdrop-filter: var(--frost-strip);
  }
  .history-tools {
    display: flex; align-items: center; gap: var(--s4);
    height: 56px;
  }
  /* With no table under it — empty archive, no match, an error — the block
     ends at the controls, so the closing hairline has to move up to them. */
  .history-block.solo.stuck .history-tools { box-shadow: inset 0 -1px 0 var(--line-2); }

  /* The count is the ONE elastic thing in this bar. Everything to its right is
     fixed, so a number that gains a digit — or the whole phrase changing as a
     filter narrows — cannot move the control the reader is using. It is the
     app's ordinary meta line (`.detail-meta`), the same object the Queue page
     puts under its title; it used to be a clock glyph, a tracked-caps word and
     a bold number, which is three treatments for one short sentence. */
  .history-summary {
    flex: 1 1 0; min-width: 0;
    overflow: hidden; white-space: nowrap; text-overflow: ellipsis;
    color: var(--fg-2);
  }
  .history-summary .num { color: var(--count); font-weight: var(--w-med); }

  .history-control { display: flex; align-items: center; min-width: 0; }
  /* Once the count has given way, the filter is the elastic thing instead:
     the row stays full rather than holding a hole where the count was. */
  .history-filter-control { flex: 0 0 300px; }
  .history-tools.no-summary .history-filter-control { flex: 1 1 0; }
  .history-filter-control .field { flex: 1; }
  /* 140px because that is `.sel-btn`'s own min-width, and min-width beats
     width: at 138 the button stood 2px proud of the box that was supposed to
     be sizing it. The width is fixed rather than automatic so that choosing a
     longer option ("Newest first" → "Song title") cannot move the button
     beside it. */
  .history-sort-control { flex: 0 0 140px; }
  .history-sort-control :global(.sel-btn) { height: 32px; width: 100%; }

  /* The column head, in the track table's own language: tracked caps closed
     with a hairline. The hairline is also the whole block's bottom edge, which
     is why it is the only one — two stacked rules across a 90px band was what
     made the top of this page read as broken. */
  .hi-head {
    display: grid; grid-template-columns: var(--hi-cols);
    align-items: center; gap: var(--hi-gap);
    height: 32px; padding: 0 var(--s3);
    font-family: var(--font-small);
    font-size: var(--t-caps); font-weight: var(--w-semi); letter-spacing: var(--track-caps);
    text-transform: uppercase; color: var(--label);
    box-shadow: inset 0 -1px 0 var(--line);
    transition: box-shadow var(--d2) var(--ease);
  }
  .history-block.stuck .hi-head { box-shadow: inset 0 -1px 0 var(--line-2); }
  /* Named cells: "Track" covers the artwork column too, so it has to be told
     where it starts — otherwise it auto-places into the artwork's track. */
  .hi-when-head { grid-column: 1; }
  .hi-track-head { grid-column: 3; }
  .hi-played-head { text-align: right; }
  /* The running day is DATA sitting in a row of furniture, so it drops the
     tracked caps for the same reading type the rows' own day labels wear, and
     spans the clock and artwork columns so that a date with a weekday in it
     has somewhere to be. */
  .hi-running-day {
    grid-column: 1 / 3; min-width: 0;
    font-family: var(--font-text); font-size: var(--t-12); font-weight: var(--w-med);
    letter-spacing: normal; text-transform: none; color: var(--fg);
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }

  .history-empty {
    max-width: none; min-height: 132px; margin: 0; padding: var(--s6) var(--s3);
  }

  /* =====================================================================
     ROWS

     One template for every row, because every row is the same height — that is
     what the windowing above depends on, and it is why a day is opened with a
     rule drawn INSIDE the first row's own box rather than with a header row.
     ===================================================================== */
  .hi-row { position: relative; height: var(--row-h); }
  /* The shared skeleton row reads `--cols` and `--tl-gap`, which are the track
     table's names for the two things this list already computes. Pointing them
     at this list's values is the whole adaptation. */
  .history-list :global(.sk-row) { --cols: var(--hi-cols); --tl-gap: var(--hi-gap); }
  .hi-main {
    display: grid; grid-template-columns: var(--hi-cols);
    align-items: center; gap: var(--hi-gap);
    width: 100%; height: var(--row-h); padding: 0 var(--s3);
    border-radius: var(--r2); text-align: left;
    transition: background-color var(--d1) var(--ease);
  }
  .hi-main:hover { background: var(--hover); }

  /* A day opens with the app's own structural hairline — the one that leads
     with foam and fades into the ordinary line colour — drawn across the top
     edge of the row's box. It is a gradient, so it is a positioned element
     rather than an inset shadow; it still adds no height, which is the whole
     constraint. */
  .hi-row.opens-day::before {
    content: ""; position: absolute; inset: 0 0 auto; height: 1px;
    background: var(--rule-accent);
  }
  .hi-row.opens-day .hi-main { border-radius: 0 0 var(--r2) var(--r2); }
  /* See `headsList`: the head's own hairline is already this row's top edge. */
  .hi-row.opens-day.heads-list::before { content: none; }
  .hi-row.opens-day.heads-list .hi-main { border-radius: var(--r2); }

  /* =====================================================================
     WHEN — the clock is this list's ordinal.

     Left-aligned, tabular, and always in the same place: the day label that
     opens a run is taken OUT of the flow so that every clock in the column
     sits on one line, whether or not its row also carries a date. The label
     then rides the day rule immediately above it, which is where a chapter
     mark belongs.
     ===================================================================== */
  .hi-when {
    position: relative; display: grid; align-content: center;
    min-width: 0; height: 100%;
  }
  .hi-day {
    position: absolute; top: 3px; left: 0; max-width: 100%;
    font-family: var(--font-small); font-size: var(--t-11); font-weight: var(--w-semi);
    color: var(--fg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  .hi-clock {
    font-family: var(--font-small); font-size: var(--t-12); color: var(--fg-3);
    white-space: nowrap;
    transition: color var(--d1) var(--ease);
  }
  .hi-main:hover .hi-clock { color: var(--fg-1); }
  /* A row that opens a day carries a label above its clock, so the clock drops
     to the lower half of the cell rather than sitting under the date. The
     inset is what puts its baseline on the sleeve's bottom edge and on the
     artist line's, so the displaced clock still lands on something. */
  .hi-row.opens-day .hi-when { align-content: end; padding-bottom: 6px; }

  /* The replay affordance lives on the art: on hover (or keyboard focus) the
     sleeve dims under a scrim and the play glyph surfaces. Opacity only —
     nothing moves, so reduced motion has nothing to undo. */
  .hi-art { position: relative; display: block; flex: none; width: 36px; height: 36px; }
  .hi-go {
    position: absolute; inset: 0; z-index: 1;
    display: grid; place-items: center;
    border-radius: var(--r1);
    background: color-mix(in srgb, var(--bg-0) 55%, transparent);
    color: var(--fg);
    opacity: 0;
    transition: opacity var(--d1) var(--ease);
  }
  .hi-main:hover .hi-go, .hi-main:focus-visible .hi-go { opacity: 1; }

  .hi-copy { display: grid; gap: 3px; min-width: 0; }
  .hi-copy strong {
    font-size: var(--t-13); font-weight: var(--w-med); color: var(--fg);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    transition: color var(--d1) var(--ease);
  }
  .hi-copy > span {
    font-size: var(--t-11); color: var(--fg-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .hi-main:hover .hi-copy > span { color: var(--fg-1); }

  /* =====================================================================
     A RUN OF THE SAME SONG

     Playing something two or three times over is ordinary, and the archive is
     a record, so the rows stay. What they must not do is read as a stutter:
     three identical sleeves and three identical titles stacked at full
     strength look like a list that has failed to advance.

     So the repeats are tied to the play that started the run — a hairline down
     the artwork gutter — and stated one rung quieter. The run then reads as
     one entry with echoes under it, at exactly the same row height, and every
     repeat is still its own readable, playable row with its own clock.
     ===================================================================== */
  .hi-row.repeat :global(.art) { opacity: 0.45; transition: opacity var(--d1) var(--ease); }
  .hi-row.repeat .hi-main:hover :global(.art) { opacity: 0.8; }
  .hi-row.repeat .hi-copy strong { color: var(--fg-1); font-weight: var(--w-body); }
  .hi-row.repeat .hi-main:hover .hi-copy strong { color: var(--fg); }
  /* The connector spans exactly the gutter between two sleeves — the row
     height less the sleeve — so it is written as that arithmetic rather than
     as a number, and it stays correct if either ever moves. */
  .hi-row.repeat .hi-art::before {
    content: ""; position: absolute; left: calc(50% - 0.5px);
    top: calc((var(--row-h) - 36px) * -1); height: calc(var(--row-h) - 36px);
    width: 1px; background: var(--line-2);
  }

  /* The source, as the plain truthful text the track table's Album column is —
     the playlist's actual name where there is one. It was a bordered pill
     reading "Playlist" on every row, which is a lot of chrome around no
     information. */
  .hi-source {
    min-width: 0; font-size: var(--t-12); color: var(--fg-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    transition: color var(--d1) var(--ease);
  }
  .hi-main:hover .hi-source { color: var(--fg-1); }
  /* A play genuinely can carry no context — the engine stores an empty string
     for one — and a blank cell in an otherwise full column reads as data that
     failed to arrive rather than as data that does not exist. The mark is
     generated content, so it fills the column without also telling a screen
     reader that this row has a source called "em dash". */
  .hi-source:empty::before { content: "—"; color: var(--fg-3); }

  /* =====================================================================
     PLAYED — one object for every row.

     This column used to speak two languages: a green tick and the word
     "Played" on completed rows, a duration over a progress rail on partial
     ones. Down a real archive those alternate almost row by row, share no
     alignment and share no scale, and the eye cannot compare any two rows.

     There is only one question here — how much of this did I hear — so there
     is one answer: a rail of fixed length, filled by the fraction heard, and
     the time itself beside it. A completed play is a full rail; a four-second
     bail is a stub. The rail is the app's own signature gradient, sized to the
     WHOLE rail rather than to the fill, so a stub shows only the foam end and
     a full play runs all the way through to rose. The further a play got, the
     warmer its mark: the same object as the seek bar, at row scale.
     ===================================================================== */
  .hi-played {
    display: grid; grid-template-columns: 52px minmax(0, 1fr);
    align-items: center; gap: var(--s3);
  }
  .hi-rail {
    display: block; width: 52px; height: 3px; border-radius: var(--rf);
    background: rgba(255, 255, 255, 0.13); overflow: hidden;
  }
  .hi-rail i {
    display: block; min-width: 3px; height: 100%; border-radius: inherit;
    background-image: var(--accent-grad);
    background-size: 52px 100%;
    background-repeat: no-repeat;
  }
  .hi-heard {
    font-family: var(--font-small); font-size: var(--t-12); color: var(--fg-2);
    text-align: right; white-space: nowrap;
    transition: color var(--d1) var(--ease);
  }
  .hi-main:hover .hi-heard { color: var(--fg-1); }
</style>
