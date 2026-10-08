<script>
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";
  import Select from "./Select.svelte";
  import { scrollbar } from "../lib/scrollbar.js";
  import { api } from "../lib/state.svelte.js";
  import { formatTime } from "../lib/time.js";
  import {
    addedCutoff, cleanupChoices, filterCleanupChoices, cleanupDuration, cleanupMatches, cleanupSelection,
    parseLocalDate, ruleLabel,
  } from "../lib/playlist-cleanup.js";

  /**
   * The four menus are the app's own listbox. A native <select> paints its
   * button — and unavoidably its popup — with OS chrome: light grey, its own
   * font, its own radius. On this sheet it was the one shape nothing in the
   * design system could reach.
   */
  const GROUPINGS = [
    { value: "all", label: "all rules (AND)" },
    { value: "any", label: "any rule (OR)" },
  ];
  const FIELDS = [
    { value: "artist", label: "Artist" },
    { value: "album", label: "Album" },
    { value: "song", label: "Song title" },
    { value: "duration", label: "Duration" },
    { value: "added-before", label: "Added before" },
    { value: "added-after", label: "Added after" },
    { value: "older", label: "Older than" },
    { value: "unavailable", label: "No longer playable" },
  ];
  /** Artist and album carry the verb, so the rule reads `Artist is not X` on
      its chip rather than leaving the reader to guess which way it goes. */
  const IDENTITY_MATCHES = [
    { value: "is", label: "is" },
    { value: "is-not", label: "is not" },
  ];
  const TEXT_MATCHES = [
    { value: "contains", label: "Contains text" },
    { value: "not-contains", label: "Doesn't contain" },
    { value: "words", label: "Whole words" },
    { value: "not-words", label: "No whole words" },
  ];
  const DURATION_MATCHES = [
    { value: "shorter", label: "Shorter than" },
    { value: "longer", label: "Longer than" },
  ];
  const AGE_UNITS = [
    { value: "days", label: "days" },
    { value: "weeks", label: "weeks" },
    { value: "months", label: "months" },
    { value: "years", label: "years" },
  ];
  /**
   * One line under the builder per rule kind, because the comparisons are
   * strict and two of them cannot see the whole playlist: a reader is owed the
   * reason a song they expected is not in the list, before they go looking.
   */
  const FIELD_HELP = {
    artist: "Select a suggestion, or use ↑ / ↓ and Enter. All suggestions come from this playlist.",
    album: "Select a suggestion, or use ↑ / ↓ and Enter. All suggestions come from this playlist.",
    song: "Text is case-insensitive and literal. Whole words matches “live”, not “alive”.",
    duration: "Duration comparisons are strict. Songs with unknown duration do not match.",
    "added-before": "Local midnight of that date, strictly: a song added at that instant does not match. Entries with no added date never match.",
    "added-after": "Local midnight of that date, strictly: a song added at that instant does not match. Entries with no added date never match.",
    older: "Counted back from today on the calendar, so a month is a calendar month and not 30 days. Entries with no added date never match.",
    unavailable: "Entries Spotify reports as no longer playable on this account. A network or device failure is not this.",
  };
  /**
   * The suggestion list's preferred height, mirroring its max-height. The list
   * is dropped above the field instead of below it when the scrolling body
   * cannot give it that much room, and is capped to whichever side it lands
   * on: the body clips what leaves it, and a menu with invisible rows is worse
   * than a short one. The floor is two rows plus their captions — below that
   * the list stops being a list.
   */
  const SUGGESTION_ROOM = 200;
  const SUGGESTION_MIN = 120;
  /**
   * How many matches the dropdown will render. It is a six-row scroller and
   * typing is the way to narrow it, so past this point the rows are only a
   * larger DOM and a longer walk for the arrow keys. The cap lands in
   * `suggestions` itself rather than in the markup, so the row an arrow key
   * names and the row on screen are always the same list.
   */
  const SUGGESTION_LIMIT = 50;

  let { playlist, onClose } = $props();
  let dialog = $state(null);
  let firstInput = $state(null);
  let source = $state.raw(null);
  let rules = $state([]);
  let grouping = $state("all");
  let field = $state("artist");
  let query = $state("");
  let identityOperator = $state("is");
  let textOperator = $state("contains");
  let durationOperator = $state("shorter");
  let dateValue = $state("");
  let amount = $state(1);
  let unit = $state("months");
  /** Songs the reader unticked in the preview. Kept by URI, never by row. */
  let keptUris = $state.raw(new Set());
  let activeChoice = $state(-1);
  let suggestionsOpen = $state(false);
  let suggestionsUp = $state(false);
  let suggestionsRoom = $state(SUGGESTION_ROOM);
  let reviewing = $state(false);
  let busy = $state(false);
  let reloading = $state(false);
  let error = $state("");
  let draftError = $state("");
  let removed = $state(null);
  let nextRule = 0;
  const PREVIEW_PAGE_SIZE = 100;
  let previewPage = $state(0);

  /**
   * Whether the playlist has moved past the revision this preview was built
   * from. The snapshot id answers it: the preview renders the frozen `source`
   * below, so a refresh that only moves metadata cannot change what it
   * computes, and the engine re-checks the same id at removal
   * (`expected_snapshot_id`). That check is the barrier — this is the warning
   * the reader sees. `tracks_total` is the one other field a refresh can move
   * in place without replacing the detail.
   *
   * Asked with those two fields, never with a serialised copy of the
   * playlist: the old fingerprint rebuilt a 7-field tuple for every track on
   * every evaluation to answer the same question.
   */
  function playlistMoved() {
    return !!source && (
      playlist?.snapshot_id !== source.snapshot_id
      || playlist?.tracks_total !== source.tracks_total
    );
  }
  const stale = $derived(playlistMoved());
  const tracks = $derived(source?.tracks ?? []);
  const choices = $derived(field === "artist" || field === "album" ? cleanupChoices(tracks, field) : []);
  const suggestionMatches = $derived(filterCleanupChoices(choices, query));
  const suggestions = $derived(suggestionMatches.slice(0, SUGGESTION_LIMIT));
  /* Two clocks, two derivations: the rules decide what is selected, the ticks
     decide what is kept. One derivation over both would re-run every rule over
     every entry to account for a single unticked song. */
  const matching = $derived(cleanupMatches(tracks, rules, grouping));
  const preview = $derived(cleanupSelection(matching, keptUris));
  const pageCount = $derived(Math.max(1, Math.ceil(preview.rows.length / PREVIEW_PAGE_SIZE)));
  const page = $derived(Math.min(previewPage, pageCount - 1));
  const pageRows = $derived(preview.rows.slice(page * PREVIEW_PAGE_SIZE, (page + 1) * PREVIEW_PAGE_SIZE));
  const frozen = $derived(reviewing || busy || removed !== null || stale);
  /* The rules are frozen behind the confirmation, but unticking one more song
     is the point of the confirmation: it can only ever shrink the removal, so
     the ticks stay live until the playlist itself moves under them. */
  const locked = $derived(busy || removed !== null || stale);
  const ready = $derived(rules.length > 0 && preview.uris.length > 0 && !stale && !!source?.snapshot_id);

  /* A mark outlives the rule that produced it only while its song is still a
     candidate: once a rule change drops the URI there is nothing left to keep,
     and the mark is forgotten rather than left to reappear if that rule comes
     back. A refreshed revision is not a rule change, so marks that survive one
     stay exactly where they were. */
  $effect(() => {
    if (preview.keptUris.length !== keptUris.size) keptUris = new Set(preview.keptUris);
  });

  $effect(() => {
    if (!dialog || dialog.open) return;
    useLatest();
    dialog.showModal();
    queueMicrotask(() => firstInput?.focus());
  });

  function useLatest() {
    /* Holding the object is the freeze: the playlist is a raw snapshot that a
       refresh or a reorder replaces and never edits (see `detail`). Only its
       download marks move in place, and no rule reads one. */
    source = playlist;
    reviewing = false;
    error = "";
    previewPage = 0;
    suggestionsOpen = false;
    activeChoice = -1;
  }

  function close() {
    if (!busy) onClose?.();
  }

  function nativeCancel(event) {
    event.preventDefault();
    if (suggestionsOpen) suggestionsOpen = false;
    else if (reviewing && !busy) reviewing = false;
    else close();
  }

  function changeField(value) {
    field = value;
    query = "";
    draftError = "";
    activeChoice = -1;
    suggestionsOpen = false;
  }

  /** Measures only when a list is about to open, never per keystroke: which
      side it hangs on and how tall it may be are properties of the scroll
      position, not of the query, and a list already on screen was measured
      when it opened. */
  function openSuggestions() {
    if (field !== "artist" && field !== "album") return;
    if (suggestionsOpen) return;
    let up = false;
    let room = SUGGESTION_ROOM;
    const wrap = firstInput?.closest(".cleanup-input-wrap");
    const body = wrap?.closest(".cleanup-body");
    if (wrap && body) {
      const wrapRect = wrap.getBoundingClientRect();
      const bodyRect = body.getBoundingClientRect();
      const below = bodyRect.bottom - parseFloat(getComputedStyle(body).paddingBottom) - wrapRect.bottom;
      const above = wrapRect.top - bodyRect.top;
      up = below < SUGGESTION_ROOM && above > below;
      room = Math.max(SUGGESTION_MIN, Math.min(SUGGESTION_ROOM, (up ? above : below) - 4));
    }
    suggestionsUp = up;
    suggestionsRoom = room;
    suggestionsOpen = true;
  }

  /**
   * The rule the builder's controls describe right now, or null with the
   * reason in `draftError`. Artist and album need a suggestion because their
   * rule is the playlist's own choice, not the text being typed.
   */
  function draftRule(choice) {
    if (field === "artist" || field === "album") {
      if (!choice) {
        draftError = "Choose a suggestion from this playlist.";
        return null;
      }
      return { field, operator: identityOperator, choice };
    }
    if (field === "song") {
      const text = query.trim();
      if (!text) {
        draftError = "Enter some song title text first.";
        return null;
      }
      return { field, text, operator: textOperator };
    }
    if (field === "duration") {
      const duration = cleanupDuration(query);
      if (duration === null) {
        draftError = "Enter a positive duration, such as 3:30 or 210 seconds.";
        return null;
      }
      return { field, duration, operator: durationOperator };
    }
    if (field === "older") {
      /* A fractional count of months has no calendar meaning, and a negative
         one would run the comparison backwards into the future. */
      if (!Number.isSafeInteger(amount) || amount < 1) {
        draftError = "Enter a whole number of days, weeks, months or years.";
        return null;
      }
      /* The cutoff is fixed here, when the reader picks it, not on every
         evaluation: the moment is part of the rule they read back on the chip
         and the one the preview is answering with. */
      return { field, amount, unit, before: addedCutoff(amount, unit) };
    }
    if (field === "added-before" || field === "added-after") {
      const date = parseLocalDate(dateValue);
      if (date === null) {
        draftError = "Pick a date, or type it as YYYY-MM-DD.";
        return null;
      }
      return { field, date };
    }
    // No longer playable takes no parameters, so the field is the whole rule.
    return { field };
  }

  /**
   * Whether two rules are the same rule. Drives only the chip list — adding a
   * rule twice would make the list longer, never the result smaller — so it
   * compares what the rule reads back as, not the object it was built from.
   */
  function sameRule(a, b) {
    if (a.field !== b.field) return false;
    if (a.field === "artist" || a.field === "album") return a.choice.key === b.choice.key;
    if (a.field === "song") return a.operator === b.operator && a.text === b.text;
    if (a.field === "duration") return a.operator === b.operator && a.duration === b.duration;
    if (a.field === "older") return a.amount === b.amount && a.unit === b.unit;
    if (a.field === "unavailable") return true;
    return a.date === b.date;
  }

  function addRule(choice = null) {
    if (frozen) return;
    draftError = "";
    const drafted = draftRule(choice);
    if (!drafted) return;
    const rule = { ...drafted, label: ruleLabel(drafted) };
    if (!rules.some((existing) => sameRule(existing, rule))) rules = [...rules, { ...rule, id: nextRule++ }];
    previewPage = 0;
    query = "";
    suggestionsOpen = false;
    activeChoice = -1;
    queueMicrotask(() => firstInput?.focus());
  }

  /**
   * Unticking states something about a song, not about a row: Spotify removes
   * by URI and takes every copy, so one untick drops the whole URI from the
   * removal set and the song's other copies are shown kept along with it.
   */
  function toggleKept(uri) {
    if (locked || !uri) return;
    const next = new Set(keptUris);
    if (next.has(uri)) next.delete(uri);
    else next.add(uri);
    keptUris = next;
  }

  function restoreAll() {
    if (locked) return;
    keptUris = new Set();
  }

  function suggestionKey(event) {
    if (field !== "artist" && field !== "album") return;
    if (event.key === "Escape" && suggestionsOpen) {
      event.preventDefault();
      event.stopPropagation();
      suggestionsOpen = false;
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      suggestionsOpen = true;
      if (!suggestions.length) return;
      activeChoice = event.key === "ArrowDown"
        ? (activeChoice + 1) % suggestions.length
        : (activeChoice < 0 ? suggestions.length - 1 : (activeChoice - 1 + suggestions.length) % suggestions.length);
      queueMicrotask(() => document.getElementById(`cleanup-choice-${activeChoice}`)?.scrollIntoView({ block: "nearest" }));
    } else if (event.key === "Enter" && suggestionsOpen && activeChoice >= 0) {
      event.preventDefault();
      addRule(suggestions[activeChoice]);
    }
  }

  async function reload() {
    if (reloading || busy) return;
    reloading = true;
    error = "";
    try {
      // Browse returns its cache immediately and emits a fresh playlist later.
      // Never replace the live detail with that possibly older return value.
      await api.browsePlaylist(playlist.id);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause || "Could not reload this playlist.");
    } finally {
      reloading = false;
    }
  }

  async function remove() {
    if (busy || !reviewing || !ready || removed !== null) return;
    // Recheck synchronously at the destructive boundary, not just in the UI.
    if (playlistMoved()) return;
    const count = preview.removalCount;
    busy = true;
    error = "";
    try {
      await api.removePlaylistTracks(source.id, preview.uris, source.snapshot_id);
      removed = count;
      reviewing = false;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause || "Could not remove these songs. Try again.");
    } finally {
      busy = false;
    }
  }
</script>

<dialog
  class="confirm-dialog cleanup-dialog glass-overlay"
  bind:this={dialog}
  aria-labelledby="cleanup-title"
  aria-describedby="cleanup-description"
  aria-busy={busy}
  oncancel={nativeCancel}
  onclick={(event) => { if (event.target === dialog) close(); }}
>
  <div class="cleanup-sheet">
    <header class="cleanup-head">
      <span class="tag">Playlist cleanup</span>
      <h2 id="cleanup-title">{removed !== null ? "Cleanup complete" : reviewing ? "Remove these songs?" : "Find songs to remove"}</h2>
      <p id="cleanup-description">
        {#if removed !== null}
          Removed {removed} {removed === 1 ? "playlist entry" : "playlist entries"} from “{source.name}”.
          The playlist refreshes automatically.
        {:else if reviewing}
          Remove these {preview.removalCount} entries from “{source.name}”? This cannot be undone here.
          Your liked songs and other playlists stay unchanged.
        {:else}
          Choose rules for “{source?.name ?? playlist.name}”. Nothing is removed until you review and confirm.
        {/if}
      </p>
      {#if removed === null}
        <button type="button" class="btn-round cleanup-close" aria-label="Close" title="Close" disabled={busy} onclick={close}>
          <Icon name="x" size={16} />
        </button>
      {/if}
    </header>

    {#if removed === null}
      <div class="cleanup-body" use:scrollbar>
        {#if stale && !busy}
          <div class="cleanup-notice glass-card alert" role="alert">
            <p>This playlist changed. Your preview is out of date; nothing more can be removed from it.</p>
            <button type="button" class="pill" onclick={useLatest}>Review latest playlist</button>
          </div>
        {:else if !source?.snapshot_id}
          <div class="cleanup-notice glass-card" role="status">
            <p>Waiting for a verified playlist revision. Reload before reviewing removals.</p>
            <button type="button" class="pill" disabled={reloading} onclick={reload}>{reloading ? "Reloading…" : "Reload playlist"}</button>
          </div>
        {/if}

        <fieldset class="cleanup-rules" disabled={frozen}>
          <div class="cleanup-band">
            <h3 class="caps">Rules</h3>
            <span class="cleanup-match">
              <span class="caps" aria-hidden="true">Match</span>
              <Select
                label="Match rules"
                options={GROUPINGS}
                value={grouping}
                disabled={frozen}
                onchange={(value) => { grouping = value; previewPage = 0; }}
              />
            </span>
          </div>
          <p class="cleanup-help">{grouping === "all" ? "A song must meet every rule. Use this to narrow your selection." : "A song can meet any rule. Use this for several artists or albums."}</p>

          {#if rules.length}
            <ul class="chips cleanup-chips" aria-label="Active removal rules">
              {#each rules as rule (rule.id)}
                <li class="chip">
                  <span class="label" title={rule.label}>{rule.label}</span>
                  <button
                    type="button"
                    class="x"
                    title="Remove rule"
                    aria-label={`Remove rule: ${rule.label}`}
                    onclick={() => { rules = rules.filter((item) => item.id !== rule.id); }}
                  >
                    <Icon name="x" size={11} />
                  </button>
                </li>
              {/each}
            </ul>
          {/if}

          <form class="cleanup-builder" onsubmit={(event) => { event.preventDefault(); addRule(); }}>
            <div class="cleanup-field">
              <span class="caps" aria-hidden="true">Rule</span>
              <Select label="Rule type" options={FIELDS} value={field} disabled={frozen} onchange={changeField} />
            </div>
            {#if field === "artist" || field === "album"}
              <div class="cleanup-field">
                <span class="caps" aria-hidden="true">Match</span>
                <Select label={`Match ${field}`} options={IDENTITY_MATCHES} value={identityOperator} disabled={frozen} onchange={(value) => (identityOperator = value)} />
              </div>
            {:else if field === "song"}
              <div class="cleanup-field">
                <span class="caps" aria-hidden="true">Match text</span>
                <Select label="Match text" options={TEXT_MATCHES} value={textOperator} disabled={frozen} onchange={(value) => (textOperator = value)} />
              </div>
            {:else if field === "duration"}
              <div class="cleanup-field">
                <span class="caps" aria-hidden="true">Compare</span>
                <Select label="Compare duration" options={DURATION_MATCHES} value={durationOperator} disabled={frozen} onchange={(value) => (durationOperator = value)} />
              </div>
            {/if}
            {#if field === "added-before" || field === "added-after"}
              <!-- The one native control the sheet keeps: a date picker is a
                   calendar the reader already knows, and the app's own input
                   material is enough to make it belong here. -->
              <div class="cleanup-field">
                <label class="caps" for="cleanup-value">Date</label>
                <span class="field cleanup-date">
                  <input
                    id="cleanup-value"
                  bind:this={firstInput}
                  bind:value={dateValue}
                  type="date"
                  aria-invalid={!!draftError}
                  oninput={() => (draftError = "")}
                  />
                </span>
              </div>
            {:else if field === "older"}
              <div class="cleanup-field">
                <label class="caps" for="cleanup-value">Count</label>
                <span class="field cleanup-amount">
                  <input
                    id="cleanup-value"
                    class="tnum"
                  bind:this={firstInput}
                  bind:value={amount}
                  type="number"
                  min="1"
                  step="1"
                  aria-invalid={!!draftError}
                  oninput={() => (draftError = "")}
                  />
                </span>
              </div>
              <div class="cleanup-field">
                <span class="caps" aria-hidden="true">Unit</span>
                <Select label="Age unit" options={AGE_UNITS} value={unit} disabled={frozen} onchange={(value) => (unit = value)} />
              </div>
            {:else if field !== "unavailable"}
              <div class="cleanup-field cleanup-input-wrap">
                <label class="caps" for="cleanup-value">{field === "artist" || field === "album" ? `Find ${field} in this playlist` : field === "song" ? "Song title text" : "Time (m:ss or seconds)"}</label>
                <span class="field">
                  <input
                    id="cleanup-value"
                  bind:this={firstInput}
                  bind:value={query}
                  role={field === "artist" || field === "album" ? "combobox" : undefined}
                  aria-autocomplete={field === "artist" || field === "album" ? "list" : undefined}
                  aria-expanded={field === "artist" || field === "album" ? suggestionsOpen : undefined}
                  aria-controls={field === "artist" || field === "album" ? "cleanup-suggestions" : undefined}
                  aria-activedescendant={suggestionsOpen && activeChoice >= 0 ? `cleanup-choice-${activeChoice}` : undefined}
                  aria-invalid={!!draftError}
                  autocomplete="off"
                  placeholder={field === "duration" ? "3:30" : field === "song" ? "e.g. live" : `Type an ${field} name…`}
                  oninput={() => { activeChoice = -1; openSuggestions(); draftError = ""; }}
                  onfocus={openSuggestions}
                  onblur={() => { suggestionsOpen = false; }}
                  onkeydown={suggestionKey}
                  />
                </span>
                {#if suggestionsOpen && !frozen && (field === "artist" || field === "album")}
                  <div class="cleanup-suggestions glass-overlay" class:up={suggestionsUp} id="cleanup-suggestions" role="listbox" aria-label={`${field === "artist" ? "Artists" : "Albums"} in this playlist${suggestionMatches.length > suggestions.length ? `, showing the first ${suggestions.length} of ${suggestionMatches.length} matches` : ""}`}>
                    <div class="cleanup-suggestions-scroll" use:scrollbar style:max-height="{suggestionsRoom}px">
                    {#each suggestions as choice, index (choice.key)}
                      {@const highlightStart = choice.name.toLowerCase().indexOf(query.trim().toLowerCase())}
                      {@const highlightEnd = highlightStart + query.trim().length}
                      <button
                        type="button"
                        role="option"
                        id={`cleanup-choice-${index}`}
                        aria-selected={activeChoice === index}
                        tabindex="-1"
                        onpointerdown={(event) => event.preventDefault()}
                        onclick={() => addRule(choice)}
                      >
                        <span>
                          {#if query.trim() && highlightStart >= 0}
                            {choice.name.slice(0, highlightStart)}<mark>{choice.name.slice(highlightStart, highlightEnd)}</mark>{choice.name.slice(highlightEnd)}
                          {:else}{choice.name}{/if}
                          {#if choice.hint}<small>{field === "artist" ? `Including ${choice.hint}` : choice.hint}</small>{/if}
                        </span>
                        <span class="tnum">{choice.count} {choice.count === 1 ? "entry" : "entries"}</span>
                      </button>
                    {:else}
                      <p>No {field === "artist" ? "artists" : "albums"} in this playlist match.</p>
                    {/each}
                    {#if suggestionMatches.length > suggestions.length}
                      <p>Showing the first {suggestions.length} of {suggestionMatches.length} matches — keep typing to narrow them.</p>
                    {/if}
                    </div>
                  </div>
                {/if}
              </div>
            {/if}
            {#if field !== "artist" && field !== "album"}
              <button type="submit" class="pill">Add rule</button>
            {/if}
          </form>
          {#if draftError}<p class="inline-error" role="alert">{draftError}</p>{/if}
          <p class="cleanup-help">{FIELD_HELP[field]}</p>
        </fieldset>

        <section class="cleanup-preview" aria-labelledby="cleanup-preview-title">
          <div class="cleanup-band">
            <h3 class="caps" id="cleanup-preview-title">{reviewing ? "Entries to remove" : "Removal preview"}</h3>
            <span class="cleanup-meta">
              {#if preview.keptUris.length}
                <button type="button" class="link-more cleanup-restore" disabled={locked} onclick={restoreAll}>
                  {preview.keptUris.length} kept · Restore all
                </button>
              {/if}
              <span class="cleanup-count tnum" role="status">{preview.removalCount} of {tracks.length} entries</span>
            </span>
          </div>
          {#if preview.duplicateCount || preview.extraCount || preview.undatedCount || preview.missingCount}
            <ul class="cleanup-notes">
              {#if preview.duplicateCount}
                <li class="warn">
                  <span class="dot" aria-hidden="true"></span>
                  Includes {preview.duplicateCount} duplicate {preview.duplicateCount === 1 ? "entry" : "entries"}. Every copy of each song still going is removed from this playlist with it.
                </li>
              {/if}
              {#if preview.extraCount}
                <li class="warn">
                  <span class="dot" aria-hidden="true"></span>
                  {preview.extraCount} {preview.extraCount === 1 ? "entry has" : "entries have"} different details but is another copy of a song being removed. These are marked below and will also be removed.
                </li>
              {/if}
              {#if preview.undatedCount}
                <li>
                  <span class="dot" aria-hidden="true"></span>
                  {preview.undatedCount} {preview.undatedCount === 1 ? "entry has" : "entries have"} no added date, and the added-date rules cannot match {preview.undatedCount === 1 ? "it" : "them"}. {preview.undatedCount === 1 ? "It is" : "They are"} left out of this preview.
                </li>
              {/if}
              {#if preview.missingCount}
                <li>
                  <span class="dot" aria-hidden="true"></span>
                  {preview.missingCount} matching {preview.missingCount === 1 ? "entry has" : "entries have"} no Spotify URI and cannot be removed. They are not included in the removal count.
                </li>
              {/if}
            </ul>
          {/if}
          <!-- svelte-ignore a11y_no_noninteractive_tabindex
               A hundred rows a page against a window of six: without focus the
               rest of a page is unreachable from the keyboard, since the
               pagination only ever moves in hundred-row steps. Same deliberate
               exception Select.svelte makes for its own list. -->
          <div class="cleanup-results" use:scrollbar role="region" tabindex="0" aria-label="Playlist entries that will be removed">
            {#if preview.rows.length}
              <table>
                <colgroup>
                  <col class="cleanup-col-keep" />
                  <col class="cleanup-col-idx" />
                  <col class="cleanup-col-art" />
                  <col />
                  <col class="cleanup-col-album" />
                  <col class="cleanup-col-time" />
                </colgroup>
                <thead>
                  <tr>
                    <!-- The tick column's head is its screen-reader name: the
                         row it governs is the entry beside it. -->
                    <th class="caps" scope="col"><span class="sr-only">Remove</span></th>
                    <th class="caps" scope="col">#</th>
                    <!-- The artwork column has no visible heading, so the head
                         keeps a blank cell where the rows keep their tile —
                         which is what keeps “Song / artist” over the titles
                         rather than over the artwork. Same gap the track
                         table's own head leaves. -->
                    <th class="caps" scope="col"><span class="sr-only">Artwork</span></th>
                    <th class="caps" scope="col">Song / artist</th>
                    <th class="caps" scope="col">Album</th>
                    <th class="caps" scope="col">Time</th>
                  </tr>
                </thead>
                <tbody>
                  {#each pageRows as row (row.index)}
                    <tr class:cleanup-extra={row.extra && !row.kept} class:cleanup-kept={row.kept}>
                      <!-- Ticked means going: the box is how a reader drops
                           one song from the rule's result without touching the
                           rule. It is a real checkbox, so it answers the
                           keyboard, and it is named after the song it governs
                           because the row's own text is not part of a label. -->
                      <td class="cleanup-keep">
                        <input
                          class="set-check"
                          type="checkbox"
                          checked={!row.kept}
                          disabled={locked}
                          aria-label={`Remove ${row.track.name || "this entry"} from this playlist`}
                          onchange={() => toggleKept(row.track.uri)}
                        />
                      </td>
                      <td class="cleanup-idx tnum">{row.index + 1}</td>
                      <td class="cleanup-art">
                        <!-- The track table's row art at the track table's
                             size. A song is recognised by its sleeve before
                             its name is read, which is the whole point of the
                             preview. `cover_url` may be absent — a playlist
                             entry need not carry one — and Cover falls back to
                             its generated identity tile exactly as it does in
                             TrackList. -->
                        <Cover
                          src={row.track.cover_url}
                          id={row.track.album_id || row.track.uri}
                          name={row.track.album_name || row.track.name}
                          size={36}
                          class="c-art"
                        />
                      </td>
                      <td class="cleanup-song">
                        <strong>{row.track.name || "Untitled song"}</strong>
                        <span>{(row.track.artist_names ?? []).join(", ") || "Unknown artist"}</span>
                        {#if row.kept}<small class="kept">Kept · not removed</small>
                        {:else if row.extra}<small>Another copy · also removed</small>{/if}
                      </td>
                      <td class="cleanup-album">{row.track.album_name || "Unknown album"}</td>
                      <td class="cleanup-time tnum">{row.track.duration_ms > 0 ? formatTime(row.track.duration_ms) : "—"}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {:else}
              <div class="empty cleanup-empty">
                {#if rules.length}
                  <p class="h">No removable songs match</p>
                  <p class="sub">Adjust your rules, or switch Match to “any rule”.</p>
                {:else}
                  <p class="h">Nothing to preview yet</p>
                  <p class="sub">Add a rule above to see every entry it would remove.</p>
                {/if}
              </div>
            {/if}
          </div>
          {#if pageCount > 1}
            <nav class="cleanup-pagination" aria-label="Removal preview pages">
              <button class="pill sm" disabled={page === 0} onclick={() => { previewPage = page - 1; }}>Previous</button>
              <span class="cleanup-count tnum" role="status">{page * PREVIEW_PAGE_SIZE + 1}–{Math.min((page + 1) * PREVIEW_PAGE_SIZE, preview.rows.length)} of {preview.rows.length}</span>
              <button class="pill sm" disabled={page === pageCount - 1} onclick={() => { previewPage = page + 1; }}>Next</button>
            </nav>
          {/if}
        </section>
      </div>
    {/if}

    {#if error}
      <div class="cleanup-error" role="alert">
        <p>{error}</p>
        <button type="button" class="pill" disabled={busy || reloading} onclick={reload}>{reloading ? "Reloading…" : "Reload playlist"}</button>
      </div>
    {/if}

    <footer class="dialog-actions cleanup-foot">
      {#if removed !== null}
        <button class="pill accent" onclick={close}>Done</button>
      {:else}
        <button class="pill" disabled={busy} onclick={close}>Cancel</button>
        {#if reviewing}
          <button class="pill" disabled={busy} onclick={() => { reviewing = false; error = ""; }}>Edit rules</button>
          <button class="pill danger" disabled={busy || !ready} onclick={remove}>{busy ? "Removing…" : `${error ? "Retry removing" : "Remove"} ${preview.removalCount} entries`}</button>
        {:else}
          <button class="pill accent" disabled={!ready} onclick={() => { reviewing = true; suggestionsOpen = false; }}>Review {preview.removalCount} removals</button>
        {/if}
      {/if}
    </footer>
  </div>
</dialog>

<style>
  /* THREE BANDS — head, scrolling body, actions — instead of one long sheet
     with a sticky footer.

     The sticky footer is what drew the buttons over the content. It sat at
     z-index 5 while the suggestion list opens at 4, so the action row painted
     across the artist suggestions underneath it; and because `sticky` was
     resolved against the dialog's own scroller, it also slid over the tail of
     the preview table. A band that never moves cannot cover anything, and both
     popups now live inside the band that scrolls — `overflow-y: auto` clips
     them at the actions rather than under them. */
  /* Overlay glass (.glass-overlay, in the markup), which makes a modal a
     sheet. */
  .cleanup-dialog {
    display: flex; flex-direction: column;
    width: min(860px, calc(100vw - var(--s8)));
    max-height: calc(100dvh - var(--s8));
    overflow: hidden;
  }
  /* Positioned: the body's overlay scrollbar is laid against it. */
  .cleanup-sheet { position: relative; display: flex; flex-direction: column; flex: 1; min-height: 0; padding: 0; }

  /* --- head. Fixed, so the thing you are about to do never scrolls away. */
  .cleanup-head {
    position: relative; flex: none;
    padding: var(--s5) var(--s8) var(--s4) var(--s6);
    border-bottom: 1px solid var(--line);
  }
  .cleanup-head h2 {
    margin-top: var(--s2);
    font-family: var(--font-display); font-size: var(--t-23);
    font-weight: var(--w-bold); letter-spacing: -0.02em; line-height: 1.15;
  }
  .cleanup-head > p { margin-top: var(--s2); max-width: 66ch; color: var(--fg-1); font-size: var(--t-13); }
  .cleanup-close { position: absolute; top: var(--s4); right: var(--s5); color: var(--fg-2); }

  /* --- body. The only scroller. The rule builder is fixed and the preview
     takes the slack, so a crowded rule list scrolls the body while a short one
     leaves the dialog exactly as tall as its content. */
  .cleanup-body {
    flex: 1 1 auto; min-height: 0; overflow-y: auto;
    /* Its scrollbar is an overlay (lib/scrollbar.js): nothing is reserved
       for it, and nothing shifts when it appears. */
    display: flex; flex-direction: column; gap: var(--s4);
    padding: var(--s4) var(--s6) var(--s5);
  }

  /* A blocking notice is a plate with its action on the same line as its
     sentence: stacked, the two made a tall box with a stranded button. */
  /* A card on the sheet (.glass-card, in the markup). */
  .cleanup-notice {
    position: relative;
    flex: none; display: flex; align-items: center; justify-content: space-between;
    gap: var(--s4); padding: var(--s3) var(--s4);
    border-radius: var(--r2);
    color: var(--fg-1); font-size: var(--t-12);
  }
  .cleanup-notice.alert { background: var(--glass-sheen), var(--danger-wash); }
  .cleanup-notice p { min-width: 0; }
  .cleanup-notice button { flex: none; }

  .cleanup-rules { flex: none; border: 0; padding: 0; margin: 0; min-width: 0; }

  /* A section header: the field label at one end, the control or the count it
     governs at the other. This is the dialog's whole hierarchy — no boxes. */
  .cleanup-band {
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s3); min-height: 24px;
  }
  .cleanup-band .caps { margin: 0; }
  .cleanup-match { display: flex; align-items: center; gap: var(--s2); }
  .cleanup-count { color: var(--count); font-weight: var(--w-med); font-size: var(--t-12); }
  /* The kept reset sits beside the count it changes, in the app's quiet-link
     register: it is a way back, not a second action competing with Review. */
  .cleanup-meta { display: flex; align-items: center; gap: var(--s3); }
  .cleanup-restore:disabled { opacity: 0.45; cursor: default; }
  .cleanup-restore:disabled:hover { color: var(--fg-2); }

  .cleanup-help { margin-top: var(--s2); color: var(--fg-2); font-size: var(--t-12); }

  /* The chips are the app's shared pill (see .chip in app.css) rather than a
     local rectangle: same height, same round × plate as the search filters.
     `title` carries the label a 220px cap ellipsises.
     A `ul` arrives with the browser's own 40px inline padding, which put the
     rules 40px right of every section label on this sheet; it is a flex row,
     so the marker box has nothing to do either. */
  .cleanup-chips { margin: var(--s3) 0 0; padding: 0; list-style: none; }
  /* Rose, not foam. A chip here is a constraint the owner typed, and foam on
     this sheet already means the one action that removes songs; the shared
     grey pill, in a dark sheet with this many controls, read as disabled
     chrome rather than as the rules the whole dialog is about. Rose-ink under
     the tint and rose for the text, for the reason --saved-wash gives: a
     pale warm at 15% over near-black loses its chroma and goes brown. */
  .cleanup-chips .chip {
    background: var(--saved-wash);
    border-color: color-mix(in srgb, var(--rose-ink) 32%, transparent);
    color: color-mix(in srgb, var(--rose) 86%, var(--fg));
  }
  .cleanup-chips .chip:hover {
    background: color-mix(in srgb, var(--rose-ink) 22%, transparent);
    color: var(--rose);
  }
  .cleanup-chips .chip .x { color: color-mix(in srgb, var(--rose) 60%, transparent); }
  .cleanup-chips .chip .x:hover { color: var(--rose); background: color-mix(in srgb, var(--rose-ink) 26%, transparent); }

  .cleanup-builder { display: flex; flex-wrap: wrap; align-items: flex-end; gap: var(--s3); margin-top: var(--s4); }
  .cleanup-field { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; }
  .cleanup-field .caps { margin: 0; }
  .cleanup-input-wrap { flex: 1 1 260px; position: relative; }
  /* THE FIELD, 34px tall like every control it stands beside in this row. */
  .cleanup-builder .field { height: 34px; }
  /* A count and a date need no more room than they hold. `color-scheme: dark`
     is set app-wide, which is what keeps the native calendar mark and the
     number spinners light on this sheet rather than the OS's own grey. */
  .cleanup-amount { width: 88px; }
  .cleanup-date { width: 168px; }

  /* Overlay glass (.glass-overlay, in the markup): the same object as
     .sel-list and .menu, so the two dropdowns on this sheet are one thing.
     The list scrolls inside the glass, so its overlay bar is laid inside it. */
  .cleanup-suggestions {
    position: absolute; z-index: 3; top: calc(100% + var(--s1)); left: 0; right: 0;
    border-radius: var(--r3);
  }
  .cleanup-suggestions-scroll { max-height: 200px; overflow-y: auto; overscroll-behavior: contain; padding: var(--s1); }
  .cleanup-suggestions.up { top: auto; bottom: calc(100% + var(--s1)); }
  .cleanup-suggestions button {
    display: flex; align-items: center; justify-content: space-between; gap: var(--s3);
    width: 100%; min-height: 32px; padding: var(--s1) var(--s2) var(--s1) var(--s3);
    border-radius: var(--r2); color: var(--fg-1); font-size: var(--t-13); text-align: left;
  }
  /* Same active plate as the listbox: hover and the arrow-key position are the
     same fact, so they are the same colour. */
  .cleanup-suggestions button:hover,
  .cleanup-suggestions button[aria-selected="true"] { background: rgba(255, 255, 255, 0.08); color: var(--fg); }
  .cleanup-suggestions button > span:first-child { min-width: 0; }
  .cleanup-suggestions small {
    display: block; margin-top: 1px;
    font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-2);
  }
  .cleanup-suggestions .tnum { white-space: nowrap; color: var(--count); font-size: var(--t-12); font-weight: var(--w-med); }
  .cleanup-suggestions mark { background: transparent; color: var(--accent); font-weight: var(--w-bold); }
  .cleanup-suggestions p { padding: var(--s3); color: var(--fg-2); font-size: var(--t-12); }

  /* --- preview. The rule that opens it leads with the accent and fades into
     the ordinary hairline, the way every structural rule in this app does.
     Not shrinkable: the preview is what the sheet is for, and the body around
     it is the scroller. */
  .cleanup-preview {
    --preview-floor: 252px;
    position: relative; /* the results' overlay scrollbar is laid against it */
    display: flex; flex-direction: column;
    flex: none;
    padding-top: var(--s4);
    border-top: 1px solid transparent;
    border-image: var(--rule-accent) 1;
  }
  .cleanup-notes { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s3); list-style: none; padding: 0; }
  .cleanup-notes li {
    display: flex; align-items: center; gap: var(--s2);
    color: var(--fg-1); font-size: var(--t-12);
  }
  .cleanup-notes .dot { flex: none; width: 5px; height: 5px; border-radius: 50%; background: var(--fg-3); }
  .cleanup-notes .warn .dot { background: var(--love); }

  /* Capped, then scrolled: 250 entries must not push the actions off the
     bottom, and the head stays put while the rows move under it. The inset
     rule closes the region and marks the row a scroll cut mid-height as a
     cut, rather than as a clipping accident.

     The floor is four rows and their head. It used to be 96px, which is one
     row and a half: on a window shorter than the sheet, the region gave way
     to its minimum, the body never overflowed, and the result was a preview
     that showed one song with no scrollbar anywhere on the sheet — the one
     thing the dialog exists to show, traded away to keep the dialog short.
     Now the floor holds, the sheet grows past the window, and the body
     scrolls: `32dvh` was the old cap, which is what let a short window
     squeeze the region down to that 96px. */
  .cleanup-results {
    flex: 1 1 auto; min-height: var(--preview-floor); max-height: 320px; overflow: auto;
    margin-top: var(--s3);
    box-shadow: inset 0 -1px 0 var(--line);
  }
  .cleanup-results table { width: 100%; border-collapse: collapse; table-layout: fixed; text-align: left; }
  /* Always stuck, so always a strip: the rows scroll under its frost. */
  .cleanup-results th {
    height: 28px; padding: 0 var(--s3);
    background: var(--tint-strip);
    -webkit-backdrop-filter: var(--frost-strip);
            backdrop-filter: var(--frost-strip);
    position: sticky; top: 0; z-index: 1;
    box-shadow: inset 0 -1px 0 var(--line-2);
    text-align: left;
  }
  .cleanup-results td { padding: var(--s2) var(--s3); vertical-align: middle; border-bottom: 1px solid var(--line); }
  /* Flush with the section labels above: the table's outer edges are the
     dialog's text edge, so only the inner columns get padding. */
  .cleanup-results th:first-child, .cleanup-results td:first-child { padding-left: 0; }
  .cleanup-results th:last-child, .cleanup-results td:last-child { padding-right: 0; }
  .cleanup-col-idx { width: 48px; }
  /* Artwork: the 36px tile the track table gives a row, plus this table's own
     12px column gutter. The cell spends its padding on the right only, so the
     sleeve's left edge is the column's edge and the title keeps starting the
     same 12px from it that every other column keeps from its neighbour. */
  .cleanup-col-art { width: 48px; }
  .cleanup-results td.cleanup-art { padding-left: 0; }
  .cleanup-col-album { width: 30%; }
  .cleanup-col-time { width: 62px; }
  /* 16px of box, then the 12px gutter every other column keeps: the tick sits
     at the dialog's own text edge, ahead of the row number. */
  .cleanup-col-keep { width: 28px; }

  /* The app's one checkbox (.set-check, in the markup), a step smaller to
     sit in a table row. */
  .cleanup-keep .set-check { width: 16px; height: 16px; }
  /* A column head sits over its values, and both of these are right-aligned.
     The first one is the tick head, which holds only screen-reader text. */
  .cleanup-results th:first-child, .cleanup-results th:last-child { text-align: right; }

  .cleanup-idx {
    font-family: var(--font-number); font-size: var(--t-12); font-weight: var(--w-med);
    color: var(--fg-3); text-align: right;
  }
  .cleanup-song strong {
    display: block; color: var(--fg); font-size: var(--t-13); font-weight: var(--w-med);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .cleanup-song span {
    display: block; margin-top: 1px;
    font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .cleanup-song small { display: block; margin-top: 1px; color: var(--love); font-size: var(--t-11); font-weight: var(--w-med); }
  /* A kept row says so in grey, not in love: it is not a warning, it is the
     one row on this sheet that is not going anywhere. */
  .cleanup-song small.kept { color: var(--fg-2); }
  .cleanup-album { color: var(--fg-2); font-size: var(--t-12); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cleanup-time { color: var(--fg-2); font-family: var(--font-number); font-size: var(--t-12); text-align: right; }
  /* Love is the palette's "you cannot undo it", so the rows that go as a
     second copy of a selected song are washed in it rather than left to a
     small caption to explain. */
  tr.cleanup-extra { background: var(--danger-wash); }
  /* A kept row is dimmed the way the app dims a row that is not going to play
     (see .tl-row.unavailable), with its tick left at full strength: the tick
     is the way back, and the one control on the row that still does anything. */
  tr.cleanup-kept > td:not(.cleanup-keep) { opacity: 0.5; }

  /* Reserved height, message centred in it. The block is the whole preview
     until rows exist, so it holds the space the table will take — and that
     space is what the suggestion list above it drops into. The floor is the
     region's own: the message sits in the middle of the box it is standing
     in for, and opening the sheet does not resize it. */
  .cleanup-empty { display: flex; flex-direction: column; justify-content: center; min-height: var(--preview-floor); padding: 0; }
  .cleanup-pagination { display: flex; align-items: center; justify-content: space-between; gap: var(--s3); margin-top: var(--s3); }

  /* The failure of the one irreversible action, edge to edge: it belongs to
     the whole sheet, not to the section the rules happen to be in. */
  .cleanup-error {
    flex: none; display: flex; align-items: center; justify-content: space-between;
    gap: var(--s4); padding: var(--s3) var(--s6);
    background: var(--danger-wash); color: var(--love); font-size: var(--t-13);
  }
  .cleanup-error button { flex: none; }

  .cleanup-foot { flex: none; margin-top: 0; padding: var(--s4) var(--s6); border-top: 1px solid var(--line); flex-wrap: wrap; }

  @media (max-width: 600px) {
    .cleanup-dialog { width: calc(100vw - var(--s4)); max-height: calc(100dvh - var(--s4)); }
    .cleanup-head, .cleanup-error, .cleanup-foot { padding-inline: var(--s4); }
    .cleanup-body { padding-inline: var(--s4); }
    .cleanup-field { flex: 1 1 100%; }
  }
</style>
