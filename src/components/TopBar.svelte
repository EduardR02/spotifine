<script>
  import {
    route,
    detail,
    search,
    queueSearch,
    submitSearch,
    ui,
    api,
    playback,
    togglePlay,
    navigate,
    goBack,
    goForward,
    canGoBack,
    canGoForward,
    getTrackEditorTrack,
    pageTitle,
  } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import WindowControls from "./WindowControls.svelte";
  import { drawsWindowControls } from "../lib/window-frame.svelte.js";
  import { nowPlayingShown } from "../lib/panel.svelte.js";

  let field = $state(null);
  const framed = $derived(drawsWindowControls && !nowPlayingShown());

  // The sidebar's Search entry and Ctrl+F both focus this one field.
  $effect(() => {
    ui.searchFocusTick;
    if (route.name === "search") field?.focus();
  });

  /**
   * Whether the page has been scrolled past its header, which is what brings
   * the bar's hairline and its sticky title in.
   *
   * An IntersectionObserver on a zero-size sentinel 150px down the scroller,
   * and specifically NOT `animation-timeline: scroll()`, which is what this
   * used to be. A scroll timeline requires an animation update for every
   * change of scroll offset; when that update cannot be composited it lands on
   * the main thread and the frame cannot be presented until it finishes, which
   * is the repaint stall. This observer fires twice in the life of a page —
   * once crossing down, once crossing back — and nothing at all in between.
   *
   * A plain scroll listener would have been the obvious alternative and is
   * also wrong: it runs on every scroll event whether or not the answer has
   * changed.
   */
  let sentinel = $state(null);
  let scrolled = $state(false);
  /* The same device at the page's very top: once it has scrolled away,
     content is passing under the bar and the bar takes its glass. */
  let edge = $state(null);
  let covering = $state(false);

  $effect(() => {
    const node = edge;
    if (!node) return;
    const root = node.closest(".scroll");
    if (!root) return;
    const observer = new IntersectionObserver(([entry]) => {
      covering = !entry.isIntersecting;
    }, { root });
    observer.observe(node);
    return () => observer.disconnect();
  });

  $effect(() => {
    const node = sentinel;
    if (!node) return;
    const root = node.closest(".scroll");
    if (!root) return;
    const observer = new IntersectionObserver(([entry]) => {
      scrolled = !entry.isIntersecting;
    }, { root });
    observer.observe(node);
    return () => observer.disconnect();
  });

  /** Title shown in the bar once the detail header has scrolled away. */
  const title = $derived(
    route.name === "made-for-you"
      ? "Made for you"
      : route.name === "playlist"
        ? (detail.playlist?.name ?? "")
        : route.name === "radio"
          ? (detail.radio?.seed_kind === "artist"
            ? `${detail.radio.seed_artist?.name ?? "Artist"} Radio`
            : detail.radio?.seed?.name
              ? `${detail.radio.seed.name} Radio`
              : "")
          : route.name === "album"
            ? (detail.album?.name ?? "")
            : route.name === "artist"
              ? (detail.artist?.name ?? "")
              : route.name === "discography"
                ? (detail.artist ? `${detail.artist.name} / Discography` : "")
                : route.name === "fans-also-like"
                  ? (detail.artist ? `${detail.artist.name} / Fans also like` : "")
                  : route.name === "appears-on"
                    ? (detail.artist ? `${detail.artist.name} / Appears On` : "")
                    : route.name === "track-editor"
                      ? (getTrackEditorTrack(route.id, route.param)?.name ?? "Track edit")
                      : ["show", "episode", "profile"].includes(route.name) &&
                          pageTitle.route === `${route.name}\u0000${route.id ?? ""}`
                        ? pageTitle.text
                        : ""
  );

  /**
   * Search as you type. The official client feels instant largely because it
   * is already searching while you type, so results are waiting by the time
   * you stop; firing only on Enter made every query start from zero.
   */
  function onInput(e) {
    const field = e.currentTarget;
    search.query = field.value;
    if (route.name !== "search" && search.query.trim()) navigate("search");
    queueSearch(search.query);
    /* A pasted link that opens its own route empties the surface inside
       `queueSearch` — and it empties it back to the value this field started
       from, so the one-way binding above has nothing to write and the box
       would keep a link the route now owns. The field is the only thing that
       can disagree, so the field is written here. */
    if (field.value !== search.query) field.value = search.query;
  }

  /**
   * Clears the field and the results in one gesture, and puts the caret back
   * so the next query can just be typed — the button is only ever reached
   * mid-search, and dismissing the keyboard focus there would cost a click.
   */
  function clearSearch() {
    search.query = "";
    queueSearch("");
    field?.focus();
  }

  /** Enter skips the remaining debounce without duplicating an in-flight call. */
  function onSubmit(e) {
    e.preventDefault();
    if (!search.query.trim()) return;
    if (route.name !== "search") navigate("search");
    submitSearch(search.query);
  }
</script>

<!-- Lives in the scrolled content, not in the bar: the bar is sticky, so
     anything inside it stays put and would never cross anything. -->
<div class="topbar-sentinel" aria-hidden="true" bind:this={sentinel}></div>
<div class="topbar-sentinel topbar-edge" aria-hidden="true" bind:this={edge}></div>

<!-- The bar's empty parts move the window. With the now-playing panel
     closed, the window's controls end the bar; open, the panel has them. -->
<div class="topbar" class:scrolled class:glass-strip={covering} class:framed data-tauri-drag-region="deep">
  <div class="hist">
    <button class="round" title="Back (Alt ←)" disabled={!canGoBack()} onclick={goBack}>
      <Icon name="back" size={16} />
    </button>
    <button class="round" title="Forward (Alt →)" disabled={!canGoForward()} onclick={goForward}>
      <Icon name="forward" size={16} />
    </button>
  </div>

  <!-- Fades in once the sentinel above has scrolled out of the pane. Below a
       620px pane it is dropped rather than squeezed: it repeats a heading that
       is a few pixels up the page, and the search field beside it does not
       repeat anything. -->
  <div class="topbar-title" class:has={!!title}>
    {#if title && ui.paneWidth >= 620}
      <button class="topbar-play" title={playback.playing ? "Pause" : "Play"} onclick={togglePlay}>
        <Icon name={playback.playing ? "pause" : "play"} size={12} />
      </button>
      <span class="t">{title}</span>
    {/if}
  </div>

  <form class="field" onsubmit={onSubmit} data-tauri-drag-region="false">
    <Icon name="search" size={14} />
    <input
      bind:this={field}
      value={search.query}
      oninput={onInput}
      placeholder="Search or paste a Spotify link"
      aria-label="Search songs, albums, artists, playlists or paste a Spotify link"
      aria-invalid={!!search.link?.error}
      spellcheck="false"
      onfocus={() => route.name !== "search" && navigate("search")}
    />
    {#if search.query}
      <button type="button" class="field-btn" title="Clear search" onclick={clearSearch}>
        <Icon name="x" size={11} />
      </button>
    {/if}
  </form>

  {#if framed}<WindowControls />{/if}
</div>
