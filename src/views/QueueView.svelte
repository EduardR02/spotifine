<script>
  import { tick, untrack } from "svelte";
  import { playback, api, navigate, togglePlay, ui, session } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ArtistLinks from "../components/ArtistLinks.svelte";
  import { formatTime, formatTotal } from "../lib/time.js";
  import { observeStuck } from "../lib/sticky.js";
  import { rowWindow } from "../lib/virtual.js";
  import { vuMeter } from "../lib/vu-meter.js";
  import { occurrenceKeys, occurrences } from "../lib/occurrence-keys.js";
  const queue = $derived(playback.queue);

  /* Two index spaces live in this file. `qi` is a queue index — the only thing
     any api call accepts. `k` is a display position in `rows`, which is what
     the reader sees and what the window math counts in. Never pass one where
     the other belongs.

     `rows` is the current track followed by the engine's own upcoming plan, so
     the list says what will actually play: shuffle's drawn order when shuffle
     is on, the sequential walk when it is off, tracks excluded from automatic
     playback already gone. Rows already played are deliberately absent — this
     view answers "what is next", and the past has its own History tab. */
  const rows = $derived(
    playback.current_index >= 0
      ? [playback.current_index, ...(playback.upcoming ?? [])]
      : (playback.upcoming ?? []),
  );
  const upNext = $derived(rows.slice(1));
  const upNextTotal = $derived(formatTotal(upNext.map((qi) => queue[qi])));

  /* The queue row is the one track row the shared TrackList does not render:
     its trailing cell is a reorder/remove cluster rather than a menu. Its five
     cells never change, so only the artwork drops — below a 430px pane the
     36px tile plus its gap is a sixth of the row and the title needs it more.
     Same threshold and same source of truth as TrackList (`ui.paneWidth`); a
     media query cannot see the inspector's 336px. */
  const cols = $derived(
    (ui.paneWidth || 1200) >= 430
      ? "28px 36px minmax(0, 1fr) 52px 96px"
      : "28px minmax(0, 1fr) 52px 96px",
  );
  const showArt = $derived((ui.paneWidth || 1200) >= 430);

  let headSentinel = $state(null);
  let headStuck = $state(false);
  $effect(() => observeStuck(headSentinel, (stuck) => (headStuck = stuck)));

  function playAt(qi) {
    if (queue[qi]?.unavailable) return;
    if (qi === playback.current_index) togglePlay();
    else api.playQueueIndex(qi).catch(() => {});
  }

  /* ---------------- Windowed rendering ----------------
     Same scheme as TrackList: a fixed-height body preserves the queue's full
     geometry while an occurrence-keyed window moves through it. One focused
     offscreen occurrence is retained without expanding the visible range.
     Sliding by one row retains every overlapping row/Cover subtree and only
     removes and adds the boundary rows. The scroller is an ancestor (.scroll
     in App.svelte), not this component, so the offset has to be read from it
     rather than from a local scrollTop.
     The queue differs from a playlist table in one way: the engine re-sends
     the whole queue (a fresh array) on every change, so identity changes on
     the queue's own edits too. Those keep the window — a move/remove/play-from
     here must not yank the scroll — while a queue that no longer contains the
     visible tracks is a genuinely new context and resets to the top.
     The rows are queue indexes, which mean nothing once the queue behind them
     is replaced, so the alignment check keeps the queue those indexes belonged
     to and resolves both sides to track identity. */
  const ROW_H = 48; // must track --row-h
  /* Sliding the window one row at a time measured better than batching it into
     blocks of 8 (92 vs 86 fps, worst frame 18ms vs 27ms): the batched version
     does the same total work in rarer, bigger bursts, and it is the burst that
     misses the frame. Keep the updates small and frequent. */
  const OVERSCAN = 6;

  let bodyEl = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(0);
  /* Plain mirrors of the two above: measure() must not *read* reactive state,
     or the wiring effect below would re-subscribe on every scroll frame. */
  let curFirst = 0;
  let curLast = 0;

  /* Tracks identity and length so the effect below fires before the new rows
     are patched in, while an identity change on its own is not enough: the
     engine replaces the array on every mutation, so the visible rows decide
     whether this is an edit of the same queue or a brand-new one. */
  let seenRows = null;
  let seenQueue = null;
  let seenLength = -1;

  const queueIdentity = $derived(occurrences(occurrenceKeys(queue)));
  const queueKeys = $derived(queueIdentity.keys);
  function keyMultiplicity(key) {
    return queueIdentity.multiplicity(key);
  }
  let focusedKey = $state(null);
  let focusedMultiplicity = $state(0);
  let focusedTrack = null;
  // Native queue snapshots do not carry occurrence IDs. If an exact duplicate
  // is replaced, discard its focus rather than trusting a regenerated ordinal.
  const focusedRow = $derived(
    focusedKey === null || keyMultiplicity(focusedKey) !== focusedMultiplicity
      ? -1 : rows.indexOf(queueIdentity.indexOf(focusedKey)),
  );
  const visible = $derived.by(() => {
    const positions = [];
    if (focusedRow >= 0 && focusedRow < firstRow) positions.push(focusedRow);
    for (let k = firstRow; k < lastRow; k++) positions.push(k);
    if (focusedRow >= lastRow) positions.push(focusedRow);
    return positions;
  });
  function retainRowFocus(event) {
    const row = event.target.closest("[data-queue-key]");
    if (row) {
      focusedKey = row.dataset.queueKey;
      focusedMultiplicity = keyMultiplicity(focusedKey);
      focusedTrack = queue[queueIdentity.indexOf(focusedKey)];
    }
  }
  function releaseRowFocus(event) {
    if (event.relatedTarget && !bodyEl?.contains(event.relatedTarget)) focusedKey = null;
  }
  let focusOwner = null;
  $effect.pre(() => {
    const owner = `${playback.context ?? ""}\u0000${playback.output_device_id ?? ""}\u0000${session.username ?? ""}`;
    if (owner === focusOwner) return;
    focusOwner = owner;
    untrack(() => {
      if (bodyEl?.contains(document.activeElement)) document.activeElement.blur();
      focusedKey = null;
    });
  });
  $effect.pre(() => {
    queueKeys;
    rows;
    const owner = focusOwner;
    const active = untrack(() => document.activeElement);
    if (!bodyEl?.contains(active)) return;
    if (untrack(() => focusedKey !== null && (
      focusedRow < 0 || (focusedMultiplicity > 1 && queue[rows[focusedRow]] !== focusedTrack)
    ))) {
      active.blur();
      focusedKey = null;
      return;
    }
    tick().then(() => {
      if (owner === focusOwner && active.isConnected && document.activeElement === document.body) active.focus({ preventScroll: true });
    });
  });

  function measure(scroller) {
    if (!bodyEl || !scroller) return;
    // The window arithmetic is shared with the track table and the history.
    const { first: f, last: l } = rowWindow(bodyEl, scroller, ROW_H, OVERSCAN, rows.length);
    if (f === curFirst && l === curLast) return;
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  function resetWindow(length, scroller) {
    if (scroller) scroller.scrollTop = 0;
    const initialLast = scroller
      ? Math.min(length, Math.ceil(scroller.clientHeight / ROW_H) + OVERSCAN)
      : length;
    curFirst = 0;
    curLast = initialLast;
    firstRow = 0;
    lastRow = initialLast;
  }

  function clampWindow(length) {
    const maxFirst = Math.max(0, length - 1);
    const f = Math.min(curFirst, maxFirst);
    const l = Math.min(Math.max(curLast, f), length);
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  /** True when the tracks currently in the window still exist in `list` —
   *  i.e. `list` is the same plan edited in place (moved/appended, or a
   *  remove that may have taken one visible row with it), not a whole new
   *  one. Window-sized, so it is cheap on the rare full states that replace
   *  the queue array. */
  function windowAligned(prevRows, prevQueue, list) {
    const f = curFirst;
    const l = Math.min(curLast, prevRows.length);
    let missing = 0;
    for (let k = f; k < l; k++) {
      const uri = prevQueue[prevRows[k]]?.uri;
      if (uri == null) continue;
      let found = false;
      for (let j = 0; j < list.length; j++) {
        if (queue[list[j]]?.uri === uri) {
          found = true;
          break;
        }
      }
      if (!found && ++missing > 1) return false;
    }
    return true;
  }

  /*
   * Runs before the new rows are patched into the DOM, so an edit cannot
   * briefly render a stale retained range. Edits of the same queue keep the
   * position (clamped to the new length); a queue that no longer shows the
   * visible tracks resets the shared pane scroll to the top.
   */
  $effect.pre(() => {
    const list = rows;
    const length = list.length;
    const body = bodyEl;
    if (!body) return;

    const identityChanged = list !== seenRows;
    const lengthChanged = length !== seenLength;
    if (!identityChanged && !lengthChanged) return;

    const prevRows = seenRows;
    const prevQueue = seenQueue;
    seenRows = list;
    seenQueue = queue;
    seenLength = length;
    const scroller = body.closest(".scroll");
    if (identityChanged && prevRows && !windowAligned(prevRows, prevQueue, list))
      resetWindow(length, scroller);
    else clampWindow(length);
  });

  $effect(() => {
    // Re-runs when the row plan's identity, its length, or the mounted body
    // changes; deliberately does not depend on firstRow/lastRow, which change
    // on every scroll frame.
    const list = rows;
    list.length;
    if (!bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) {
      // No scroll ancestor (embedded use): render everything, as before.
      curFirst = 0;
      curLast = list.length;
      firstRow = 0;
      lastRow = list.length;
      return;
    }
    let queued = false;
    const onScroll = () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(() => {
        queued = false;
        measure(scroller);
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(() => measure(scroller));
    ro.observe(scroller);
    measure(scroller);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      ro.disconnect();
    };
  });
</script>

<section class="view page">
  <div style="padding:var(--s4) 0 var(--s6)">
    <h1 class="page-title">Queue</h1>
    {#if rows.length}
      <p class="detail-meta" style="margin-top:var(--s3)">
        <span class="num">{upNext.length} {upNext.length === 1 ? "song" : "songs"} up next</span>
        <span class="sep">/</span><span class="num">{upNextTotal}</span>
        <span class="sep">/</span><span class="num">{queue.length} in the queue</span>
      </p>
      {#if playback.shuffle}
        <p class="shuffle-note">Shuffle is on, so this is the order it drew rather than the queue's.</p>
      {/if}
    {/if}
  </div>

  {#if rows.length}
    <!-- Same row system as every other track table; only the trailing cell
         differs, so a queue row and a playlist row line up exactly. -->
    <div class="tl queue" style="overflow-anchor: none" style:--cols={cols}>
      <div class="tl-head-sentinel" bind:this={headSentinel} aria-hidden="true"></div>
      <div class="tl-head" class:stuck={headStuck}>
        <span style="text-align:right">#</span>
        {#if showArt}<span></span>{/if}
        <span>Title</span>
        <span style="display:grid;justify-items:end"><Icon name="clock" size={13} /></span>
        <span></span>
      </div>

      <div
        bind:this={bodyEl}
        style="overflow-anchor: none; position: relative"
        style:height="{rows.length * ROW_H}px"
        onfocusin={retainRowFocus}
        onfocusout={releaseRowFocus}
      >
        <div
          style="position: absolute; inset: 0 0 auto"
        >
          <!-- Keep one focused occurrence outside the bounded visible range. -->
          {#each visible as k (queueKeys[rows[k]])}
            {@const qi = rows[k]}
            {@const track = queue[qi]}
            <div
              class="tl-row"
              data-queue-key={queueKeys[qi]}
              style="position: absolute; left: 0; right: 0"
              style:top="{k * ROW_H}px"
              class:current={k === 0}
              class:unavailable={track.unavailable}
              role="button"
              aria-disabled={track.unavailable ? "true" : undefined}
              title={track.unavailable ? (track.unavailable_reason || "Unavailable") : undefined}
              tabindex="-1"
              ondblclick={() => playAt(qi)}
            >
              <span class="c-idx">
                <span class="n">{k + 1}</span>
                {#if k === 0}
                  <canvas class="eq" use:vuMeter></canvas>
                {/if}
                <button
                  class="go"
                  title={track.unavailable ? (track.unavailable_reason || "Unavailable") : "Play from here"}
                  disabled={track.unavailable}
                  onclick={() => playAt(qi)}
                >
                  <Icon name={k === 0 && playback.playing ? "pause" : "play"} size={12} />
                </button>
              </span>

              {#if showArt}
                <Cover
                  src={track.cover_url}
                  id={track.album_id || track.uri}
                  name={track.album_name || track.name}
                  size={36}
                  class="c-art"
                />
              {/if}

              <span class="c-title">
                <span class="t-name">{track.name}</span>
                {#if track.uri?.startsWith("spotify:episode:")}
                  <button class="t-artists" disabled={!track.album_id} onclick={() => navigate("show", track.album_id)}>{track.album_name || track.artist_names?.[0]}</button>
                {:else}
                  <ArtistLinks
                    class="t-artists"
                    names={track.artist_names}
                    ids={track.artist_ids ?? []}
                    id={track.artist_id}
                  />
                {/if}
              </span>

              <span class="c-time">{formatTime(track.duration_ms)}</span>

              <span class="q-actions">
                <!-- Reordering is hidden under shuffle on purpose: the engine
                     rebuilds the whole bag on a move, so the button would
                     throw this entire drawn order away and replace it with a
                     new random one — visibly not what it says it does. The
                     current row has nothing to move past, so it is skipped too.
                     Targets are the DISPLAYED neighbours, not qi ± 1: a
                     remove-then-insert at the neighbour's index lands the row
                     exactly on its other side. -->
                {#if !playback.shuffle && k >= 1}
                  <button
                    title="Move up"
                    disabled={k === 1}
                    onclick={() => api.moveQueue(qi, rows[k - 1]).catch(() => {})}
                  >
                    <Icon name="chevron-up" size={15} />
                  </button>
                  <button
                    title="Move down"
                    disabled={k === rows.length - 1}
                    onclick={() => api.moveQueue(qi, rows[k + 1]).catch(() => {})}
                  >
                    <Icon name="chevron-down" size={15} />
                  </button>
                {/if}
                <button class="danger" title="Remove from queue" onclick={() => api.removeQueue(qi).catch(() => {})}>
                  <Icon name="x" size={14} />
                </button>
              </span>
            </div>
          {/each}
        </div>
      </div>
    </div>
  {:else if queue.length}
    <!-- A populated queue with nothing playable left: every remaining row is
         skipped, so there genuinely is no next track to name. -->
    <div class="empty">
      <p class="h">Nothing is up next.</p>
      <p class="sub">Every track still in the queue is set to be skipped in automatic playback.</p>
    </div>
  {:else}
    <div class="empty">
      <p class="h">The queue is empty.</p>
      <p class="sub">Play music or an audio podcast, or add songs from a track menu.</p>
      <div class="actions">
        <button class="pill" onclick={() => navigate("library")}>
          <Icon name="library" size={14} />Go to your library
        </button>
      </div>
    </div>
  {/if}
</section>

<style>
  /* One quiet line under the counts, not a banner: it explains an ordering the
     reader can already see, so it must not compete with the list itself. */
  .shuffle-note {
    margin-top: var(--s2);
    font-size: var(--t-12);
    color: var(--fg-3);
  }
</style>
