<script>
  import {
    detail,
    api,
    navigate,
    navigateArtist,
    artistNameHint,
    route,
    ui,
    retryDetail,
    loadCataloguePage,
    coverUrl,
  } from "../lib/state.svelte.js";
  import { playAlbumById, playPlaylistById } from "../lib/play.js";
  import { spotifyLink } from "../lib/spotify-link.js";
  import { coverTone, cardTone } from "../lib/covertone.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import FollowPill from "../components/FollowPill.svelte";
  import HeaderMenu from "../components/HeaderMenu.svelte";
  import CopyLinkItem from "../components/CopyLinkItem.svelte";
  import Biography from "../components/Biography.svelte";
  import GalleryLightbox from "../components/GalleryLightbox.svelte";
  import { GROUPS, RELEASE_KEYS } from "../lib/discography.svelte.js";
  import { artistPlaylistCollections, playlistSubtitle } from "../lib/artist.js";

  /**
   * The artist page: who this is, what to play, and one shelf of records.
   *
   * The redesign folded the whole discography reader into this page, so an
   * artist page was a several-screen scroll through every track of every
   * release. That is a different job. This page INTRODUCES an artist — the
   * portrait, the songs people actually play, and one row of covers you can
   * recognise a record from — and hands the reading to `discography`.
   *
   * The shelf is ONE row, not four. The pre-redesign page stacked a separate
   * shelf for Albums, Singles, Compilations and Appears-on, each with its own
   * "Show all". The segmented toggle now covers only the three main catalogue
   * groups; Appears On lives in its own lazy shelf near the bottom. "See all"
   * carries a regular catalogue selection into the reader; ranked Popular
   * releases open the reader on "All" because that order only exists in this
   * overview payload. Every release on the main shelf comes out of the artist
   * payload. Regular groups carry per-type summaries; ranked Popular releases
   * come from the overview.
   */
  const artist = $derived(detail.artist);
  const top = $derived(artist?.top_tracks ?? []);
  const counts = $derived(artist?.release_counts ?? {});
  const overview = $derived(artist?.overview ?? null);
  const popularReleases = $derived(overview?.popular_releases ?? []);
  const relatedArtists = $derived(overview?.related_artists ?? []);
  const topCities = $derived(overview?.top_cities ?? []);
  const artistPick = $derived(overview?.artist_pick ?? null);
  const playlistShelves = $derived(artistPlaylistCollections(overview));
  const NUMBER_FORMAT = new Intl.NumberFormat();

  /* ---------------------------------------------------------------- about
     Every field on the overview is nullable and they fail independently, so
     each block below decides for itself whether it exists. Nothing here draws
     a labelled placeholder for a fact that did not arrive: an empty stat is
     worse than a shorter card.

     The release counts that used to sit in this list are gone. They said
     nothing you could not read off the Discography section forty pixels
     above, and four display numbers in a row turned the quietest part of the
     page into its loudest. */
  const rank = $derived(overview?.world_rank || null);
  const popularity = $derived(
    Number.isFinite(overview?.popularity) && overview.popularity > 0 ? overview.popularity : null,
  );
  /* The About picture is a GALLERY, not a picture. `visuals.gallery.items`
     carries every editorial photograph the artist has — eighteen of them for
     Taylor Swift — and this page was drawing item zero with no way to reach
     the rest. The header avatar stands in for artists whose gallery is empty,
     and one image is simply a gallery of one: the stepper below the figure
     appears only when there is somewhere to step to.

     Each entry is `{ url, width, height }`. The avatar is a bare url and is
     wrapped as an unmeasured entry, which costs it nothing — a gallery of one
     has no set to be sized against. */
  const aboutFigures = $derived.by(() => {
    const gallery = overview?.biography_images ?? [];
    if (gallery.length) return gallery;
    return overview?.header_image_url ? [{ url: overview.header_image_url }] : [];
  });
  let figureIndex = $state(0);
  /* Clamped rather than trusted: the artist can change under a stale index. */
  const figurePosition = $derived(Math.min(figureIndex, Math.max(0, aboutFigures.length - 1)));
  const aboutFigure = $derived(aboutFigures[figurePosition] ?? null);
  function stepFigure(delta) {
    const n = aboutFigures.length;
    if (n < 2) return;
    figureIndex = (figurePosition + delta + n) % n;
  }
  /* Warm both neighbours through the browser's ordinary image cache. */
  $effect(() => {
    const n = aboutFigures.length;
    if (n < 2) return;
    for (const offset of [1, -1]) {
      const url = aboutFigures[(figurePosition + offset + n) % n]?.url;
      if (!url) continue;
      new Image().src = coverUrl(url);
    }
  });
  /* ------------------------------------------------------- the About frame
     ONE shape for the whole gallery, decided before a single picture loads.

     The square this replaces was stable and nothing else. It was chosen
     because stepping at each picture's own proportions resized the figure by
     up to 250px, so the Next button walked out from under the cursor and every
     section below About moved with it. But a square buys that stability by
     discarding the shape of all eighteen photographs, and the shape is most of
     what an editorial picture is. The dimensions now travel with the urls, so
     the frame can be derived from the entire set at once — which is exactly as
     immovable while stepping, and is at least the gallery's own shape.

     MEDIAN rather than mean: one panorama among seventeen portraits must not
     flatten the frame for the other seventeen, and a median simply does not
     see it. CLAMPED because a gallery whose median genuinely is 3:1 would
     still turn this column into a letterbox slot with a page around it.
     Pictures the service did not measure count as square, so they neither skew
     the result nor fall out of it. */
  const FRAME_MIN_ASPECT = 0.75;
  const FRAME_MAX_ASPECT = 1.6;
  const frameAspect = $derived.by(() => {
    if (aboutFigures.length < 2) return 0;
    const ratios = aboutFigures
      .map((item) => {
        const w = Number(item?.width);
        const h = Number(item?.height);
        return w > 0 && h > 0 ? w / h : 1;
      })
      .sort((a, b) => a - b);
    const mid = ratios.length >> 1;
    const median = ratios.length % 2 ? ratios[mid] : (ratios[mid - 1] + ratios[mid]) / 2;
    return Math.min(FRAME_MAX_ASPECT, Math.max(FRAME_MIN_ASPECT, median));
  });
  /* The mat under the picture takes the PICTURE's colour, not the page's.
     Everywhere else on this page `--tone-*` is the artist's, which is right
     because the artist is what the page is about; inside this frame the
     subject changes under you every time you step, and a mat that stayed the
     header's colour would be the one surface disagreeing with the thing
     standing on it. `coverTone` is the same measurement the header wash is
     built from — hue from the image, lightness and chroma imposed in
     covertone.svelte.js — so a pale photograph cannot lift the mat and a neon
     one cannot make it glow. The artist id seeds the fallback, so a picture
     that never resolves lands on the colour the artist's own generated tile
     would have used. */
  const figureTone = $derived(coverTone(aboutFigure?.url ?? "", artist?.id ?? ""));
  /* The ceiling is the picture's own width, or 560, whichever is smaller.
     Blowing a 640px source up to 560 CSS px is already soft on a 1.5x display;
     past its own width it is a bigger blur and nothing else. The engine says
     what that width is, so the cap can be honest instead of a guess. */
  const FIGURE_MAX = 560;
  const figureMax = $derived.by(() => {
    const widest = Math.max(0, ...aboutFigures.map((item) => Number(item?.width) || 0));
    return widest ? Math.min(FIGURE_MAX, widest) : FIGURE_MAX;
  });
  /* The frame is deliberately not the whole picture at its own scale, so there
     has to be somewhere the whole picture IS. */
  let lightboxOpen = $state(false);
  let figureButton = $state(null);
  function closeLightbox() {
    lightboxOpen = false;
    /* Back to the control that opened it, and on the frame AFTER the state
       change: the overlay is still the modal until its own effect runs
       `close()`, and while it is, every other element in the document is inert
       and a focus() call on one is simply discarded. */
    requestAnimationFrame(() => figureButton?.focus());
  }
  /* The supporting pair. Monthly listeners also lead the header now, and that
     is deliberate rather than an oversight: up there the figure is the first
     thing you read about an artist, down here it is the number the follower
     count and the rank are being compared against. What it must not do is
     appear at display size in both places, so this pair is set small and the
     rank keeps the one big number on the card. */
  const aboutStats = $derived.by(() => {
    const facts = [];
    if (overview?.monthly_listeners) {
      facts.push({
        value: NUMBER_FORMAT.format(overview.monthly_listeners),
        label: "Monthly listeners",
      });
    }
    if (overview?.followers) {
      facts.push({ value: NUMBER_FORMAT.format(overview.followers), label: "Followers" });
    }
    return facts;
  });
  /* A city's share of the top city's audience, which is what turns a list into
     something with a shape. Cities whose listener count is missing get no bar
     rather than a zero-width one — an absent number is not a small number. */
  const cityLeader = $derived(Math.max(0, ...topCities.map((city) => city.listeners ?? 0)));
  /* Three columns need roughly 300 + 240 + 300 plus two gaps. Below that the
     figures and the cities stack into one column beside the picture rather
     than being squeezed into thirds that fit none of them. */
  const aboutWide = $derived((ui.paneWidth || 1200) >= 940);
  const hasAbout = $derived(
    !!(overview?.biography || aboutFigure || rank || popularity || aboutStats.length || topCities.length),
  );
  let shelfExtras = $state({});

  function mergeShelfReleases(primary, extras) {
    const seen = new Set();
    const merged = [];
    for (const release of [...(primary ?? []), ...(extras ?? [])]) {
      const id = release?.id;
      if (id && seen.has(id)) continue;
      if (id) seen.add(id);
      merged.push(release);
    }
    return merged;
  }

  const groupLists = $derived.by(() => {
    const base = artist?.releases ?? {};
    const artistId = artist?.id ?? "";
    return Object.fromEntries(
      RELEASE_KEYS.map((key) => [
        key,
        mergeShelfReleases(base[key], shelfExtras[`${artistId}:${key}`]),
      ]),
    );
  });

  const releaseTotal = $derived(RELEASE_KEYS.reduce((sum, k) => sum + (counts[k] ?? 0), 0));
  const hasReleases = $derived(releaseTotal > 0);

  /** Empty groups are not offered: a dead tab is a worse answer than no tab. */
  const segments = $derived(
    [
      ...(popularReleases.length ? [{ id: "popular", label: "Popular releases" }] : []),
      ...GROUPS,
    ]
      .filter(
        (g) =>
          g.id === "all" ||
          g.id === "popular" ||
          (counts[g.id] ?? 0) > 0,
      )
      .map((g) => ({
        ...g,
        count:
          g.id === "all"
            ? releaseTotal
            : g.id === "popular"
              ? popularReleases.length
              : (counts[g.id] ?? 0),
      })),
  );

  let group = $state("popular");

  const requestedShelfPages = new Set();
  let shelfRetry = $state(0);
  let shelfError = $state("");
  let shelfErrorKey = $state("");
  let artistGeneration = 0;
  let busy = $state("");
  let playError = $state("");

  /* The artist changed under us — a link from a track row, say. Start the new
     artist on its ranked releases when the overview provides them, otherwise
     fall back to the balanced "All" shelf. */
  let seenArtist = null;
  $effect(() => {
    if (artist?.id === seenArtist) return;
    seenArtist = artist?.id ?? null;
    artistGeneration += 1;
    group = popularReleases.length ? "popular" : "all";
    shelfExtras = {};
    requestedShelfPages.clear();
    shelfRetry = 0;
    shelfError = "";
    shelfErrorKey = "";
    appearsOnReady = false;
    appearsOnLoading = false;
    appearsOnError = "";
    appearsOnRetry = 0;
    busy = "";
    playError = "";
    popularExpanded = false;
    songwriterExpanded = false;
    shelfExpanded = false;
    figureIndex = 0;
    lightboxOpen = false;
  });

  /* An overview may be refreshed independently of the catalogue payload. Do
     not leave the selector on a segment whose server-ranked data disappeared. */
  $effect(() => {
    if (!popularReleases.length && group === "popular") group = "all";
  });

  /** The label for a release's own group, used as its subtext on the shelf. */
  const KIND_LABEL = {
    albums: "Album",
    singles: "Single or EP",
    compilations: "Compilation",
  };

  /**
   * What the shelf shows for the current segment.
   *
   * Named catalogue segments show the order returned by the engine. Ranked
   * Popular releases use the overview's server order. "All" is the exception:
   * it intersperses the most recent releases across the three main types,
   * which is what a highlight row on an artist page is for.
   */
  const shelf = $derived.by(() => {
    if (group === "popular") {
      return popularReleases.map((release) => ({
        release,
        kind: release.artist_names?.join(", ") || "Release",
      }));
    }
    if (group !== "all") {
      return (groupLists[group] ?? []).map((release) => ({ release, kind: KIND_LABEL[group] }));
    }
    return RELEASE_KEYS.flatMap((key) =>
      (groupLists[key] ?? []).map((release) => ({ release, kind: KIND_LABEL[key] })),
    ).sort((a, b) => (b.release.year ?? 0) - (a.release.year ?? 0));
  });

  /**
   * How many cards fit in exactly one row.
   *
   * A shelf that wraps is not a shelf, and one that clips a card mid-cover
   * reads as a rendering fault, so the count is computed from the pane rather
   * than left to `auto-fill`. 176px is the smallest tile at which a sleeve is
   * still recognisable at a glance; the cards then stretch to fill the row, so
   * the shelf is flush with the page at every width.
   */
  const CARD_MIN = 176;
  const CARD_GAP = 20; /* --s5 */
  const perRow = $derived.by(() => {
    const usable = (ui.paneWidth || 1200) - 2 * 24;
    const n = Math.floor((usable + CARD_GAP) / (CARD_MIN + CARD_GAP));
    return Math.max(2, Math.min(7, n));
  });
  /**
   * Popular releases is the one segment that is already complete.
   *
   * The overview payload carries all ten ranked releases (Taylor Swift:
   * `popularReleasesAlbums.items` is ten long), the tab counts them honestly
   * as ten, and the shelf then drew one row of five and offered "5 more" —
   * which opened the discography reader, a view that has no Popular grouping
   * at all, because that ranking exists only in this payload. The five under
   * the fold were unreachable by any route. So this segment expands in place
   * instead, exactly as Popular songs above it does; the named catalogue
   * segments keep the link, because for those there genuinely IS more behind
   * it and the reader genuinely does have it.
   */
  let shelfExpanded = $state(false);
  const shelfExpandable = $derived(group === "popular");
  const visibleShelf = $derived(
    shelf.slice(0, shelfExpandable && shelfExpanded ? shelf.length : perRow),
  );
  /**
   * How many of this segment are not on screen — the number the button says.
   *
   * Taken from the SEGMENT's own count, which is the number its tab is already
   * showing, and not from `shelf.length`. A named segment hydrates one bounded
   * page at a time, so `shelf.length` is a fact about the last request rather
   * than about the catalogue: Albums arrived with six loaded against a tab
   * reading 33, and the link offered "1 more" to a view that then listed all
   * 33. Reading the tab's own figure is what makes the two agree, and it is
   * one number in one place instead of two that can drift.
   */
  const shelfTotal = $derived(segments.find((segment) => segment.id === group)?.count ?? 0);
  const shelfRemaining = $derived(Math.max(0, shelfTotal - visibleShelf.length));
  const visibleRelatedArtists = $derived(relatedArtists.slice(0, perRow));
  const appearsOnCount = $derived(counts.appears_on ?? 0);
  const appearsOnReleases = $derived.by(() =>
    mergeShelfReleases(
      artist?.releases?.appears_on,
      shelfExtras[`${artist?.id ?? ""}:appears_on`],
    ),
  );
  const visibleAppearsOn = $derived(appearsOnReleases.slice(0, perRow));
  let appearsOnSentinel = $state(null);
  let appearsOnReady = $state(false);
  let appearsOnLoading = $state(false);
  let appearsOnError = $state("");
  let appearsOnRetry = $state(0);

  function retryAppearsOn() {
    if (!appearsOnError) return;
    appearsOnError = "";
    appearsOnRetry += 1;
    appearsOnReady = true;
  }

  /**
   * A named shelf starts with lightweight AlbumRefs. Hydrate one bounded page
   * per artist/group; narrower panes reuse and slice it rather than turning
   * every resize into another request.
   */
  const SHELF_PAGE_SIZE = 6;

  $effect(() => {
    const id = artist?.id;
    if (!id || group === "all" || group === "popular") return;

    const selected = GROUPS.find((candidate) => candidate.id === group);
    const target = Math.min(SHELF_PAGE_SIZE, counts[group] ?? 0);
    const loaded = (groupLists[group] ?? []).length;
    if (!selected || target <= 0 || loaded >= target) return;

    const requestKey = `${id}:${group}:${shelfRetry}`;
    if (requestedShelfPages.has(requestKey)) return;
    requestedShelfPages.add(requestKey);

    const viewKey = `${id}:${group}`;
    shelfError = "";
    shelfErrorKey = "";
    loadCataloguePage(id, selected.types, 0, SHELF_PAGE_SIZE)
      .then((page) => {
        if (artist?.id !== id) return;

        const existing = shelfExtras[viewKey] ?? [];
        const known = new Set(
          [...(artist?.releases?.[selected.id] ?? []), ...existing]
            .map((release) => release?.id)
            .filter(Boolean),
        );
        const additions = [];
        for (const release of page?.releases ?? []) {
          if (!release?.id || known.has(release.id)) continue;
          known.add(release.id);
          additions.push(release);
        }
        if (additions.length) shelfExtras[viewKey] = [...existing, ...additions];
      })
      .catch((reason) => {
        requestedShelfPages.delete(requestKey);
        if (artist?.id !== id || group !== selected.id) return;
        shelfErrorKey = viewKey;
        shelfError = String(reason || "Could not load more of this catalogue.");
      });
  });
  /* Appears On is intentionally not part of the main selector. Its first
     bounded page waits until the lower shelf approaches the viewport. */
  $effect(() => {
    const node = appearsOnSentinel;
    if (!node || !artist?.id || !appearsOnCount) return;
    const scroller = node.closest(".scroll");
    if (!scroller) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) appearsOnReady = true;
      },
      { root: scroller, rootMargin: "0px 0px 360px 0px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  });

  $effect(() => {
    const id = artist?.id;
    if (!id || !appearsOnReady || !appearsOnCount || appearsOnLoading || appearsOnError) return;
    const target = Math.min(SHELF_PAGE_SIZE, appearsOnCount);
    if (appearsOnReleases.length >= target) return;
    const requestKey = `${id}:appears_on:${appearsOnRetry}`;
    if (requestedShelfPages.has(requestKey)) return;
    requestedShelfPages.add(requestKey);
    appearsOnLoading = true;
    appearsOnError = "";
    loadCataloguePage(id, ["appears_on"], 0, SHELF_PAGE_SIZE)
      .then((page) => {
        if (artist?.id !== id) return;
        const key = `${id}:appears_on`;
        const existing = shelfExtras[key] ?? [];
        const known = new Set(
          [...(artist?.releases?.appears_on ?? []), ...existing]
            .map((release) => release?.id)
            .filter(Boolean),
        );
        const additions = [];
        for (const release of page?.releases ?? []) {
          if (!release?.id || known.has(release.id)) continue;
          known.add(release.id);
          additions.push(release);
        }
        if (additions.length) shelfExtras[key] = [...existing, ...additions];
      })
      .catch((reason) => {
        requestedShelfPages.delete(requestKey);
        if (artist?.id === id) appearsOnError = String(reason || "Could not load Appears On.");
      })
      .finally(() => {
        if (artist?.id === id) appearsOnLoading = false;
      });
  });

  const currentShelfError = $derived.by(() => {
    const key = `${artist?.id ?? ""}:${group}`;
    return shelfErrorKey === key ? shelfError : "";
  });

  function retryShelf() {
    if (!currentShelfError) return;
    shelfError = "";
    shelfErrorKey = "";
    shelfRetry += 1;
  }

  function openDiscography() {
    navigate("discography", artist.id, group === "popular" ? "all" : group);
  }

  /* Popular opens at five. Ten rows of a table before you reach the covers
     buries the thing the page is actually for; expanding reveals up to ten,
     never past it. Both views are prefix slices, so row indices stay aligned
     with `top` and direct play needs no remapping. */
  const POPULAR_PREVIEW = 5;
  const POPULAR_EXPANDED = 10;
  let popularExpanded = $state(false);
  const visibleTop = $derived(
    top.slice(0, popularExpanded ? POPULAR_EXPANDED : POPULAR_PREVIEW),
  );

  /* ----------------------------------------------------- in-place growth
     Every TrackList opts out of browser scroll anchoring (its rows declare
     overflow-anchor:none), so when Popular expands the shared `.scroll`
     often has no eligible anchor left above the growth inside the viewport
     band. The engine then grabs something BELOW the inserted rows — the
     expander, the next section — and holds THAT still, dragging the whole
     page above it down by exactly the added height. An expand here is
     deliberate in-place growth: rows appear only below, so the correct
     frame is "scrollTop does not change". Freeze the offset at click time
     and re-assert it inside the same paint the growth lands in; collapsed,
     symmetric. */
  let popularSectionEl = $state(null);
  let songwriterSectionEl = $state(null);
  let dxSectionEl = $state(null);

  function toggleExpand(scrollHost, mutate) {
    const sc = scrollHost?.closest(".scroll") ?? null;
    const frozen = sc ? sc.scrollTop : 0;
    mutate();
    if (!sc) return;
    requestAnimationFrame(() => {
      if (!sc.isConnected) return;
      const wanted = Math.min(frozen, sc.scrollHeight - sc.clientHeight);
      if (Math.abs(sc.scrollTop - wanted) >= 1) sc.scrollTop = wanted;
    });
  }

  function playTop(i) {
    if (top.length) api.playQueue(top, i, `artist:${artist?.id ?? ""}`).catch(() => {});
  }

  async function shuffleTop() {
    if (!top.length) return;
    const queue = top;
    const source = `artist:${artist?.id ?? ""}`;
    try {
      await api.setShuffle(true);
      await api.playQueue(queue, 0, source, { automaticStart: true });
    } catch {}
  }

  function openArtistRadio() {
    if (artist?.id) navigate("radio", `artist:${artist.id}`);
  }


  /* ------------------------------------------------------- written by
     The verified playlist is an optional enhancement, so it gets its own
     request, kept off the artist payload's critical path. The request
     identity is the route id plus the name recorded at navigation time
     (`navigateArtist`): a click that already knows the name starts this as
     soon as the route commits, before BrowseArtist answers, and the section
     stays hidden until data arrives. Only a navigation that knew no name
     falls back to the loaded artist — and hint precedence then keeps the
     overview's later arrival from re-issuing under the canonical name.

     The engine returns the playlist reference plus at most ten source-order
     tracks. A missing playlist or an empty track payload is intentionally
     indistinguishable from no section: there is nothing useful to render. */
  const songwriterRequest = $derived.by(() => {
    const id = String(route.id ?? "").trim();
    const hinted = id ? artistNameHint(id) : "";
    const name = hinted || artist?.name || "";
    return id && name ? { id, name } : null;
  });
  const songwriterArtistKey = $derived(
    songwriterRequest ? `${songwriterRequest.id}\u0000${songwriterRequest.name}` : "",
  );
  let songwriterPayload = $state(null);
  let songwriterLoading = $state(false);
  let songwriterForArtist = $state("");
  let songwriterRequestKey = "";
  let songwriterExpanded = $state(false);
  let songwriterRequestGeneration = 0;

  $effect(() => {
    const key = songwriterArtistKey;
    if (key === songwriterRequestKey) return;
    songwriterRequestKey = key;
    const generation = ++songwriterRequestGeneration;
    songwriterForArtist = key;
    songwriterPayload = null;
    songwriterExpanded = false;
    songwriterLoading = false;
    if (!key) return;

    songwriterLoading = true;
    api
      .browseArtistSongwriter(songwriterRequest.id, songwriterRequest.name)
      .then((value) => {
        if (generation !== songwriterRequestGeneration || songwriterArtistKey !== key) return;
        songwriterLoading = false;
        songwriterPayload = value ?? null;
      })
      .catch(() => {
        if (generation !== songwriterRequestGeneration || songwriterArtistKey !== key) return;
        songwriterLoading = false;
      });
  });

  const songwriter = $derived.by(() => {
    if (songwriterForArtist !== songwriterArtistKey) return null;
    const raw = songwriterPayload;
    const id = raw?.playlist?.id ?? "";
    if (!id || !Array.isArray(raw?.tracks) || !raw.tracks.length) return null;
    const total = Number(raw.playlist.tracks_total);
    return {
      id,
      tracks: raw.tracks.slice(0, POPULAR_EXPANDED),
      total: Number.isFinite(total) && total > 0 ? Math.round(total) : 0,
    };
  });
  const visibleSongwriter = $derived(
    songwriter
      ? songwriter.tracks.slice(0, songwriterExpanded ? POPULAR_EXPANDED : POPULAR_PREVIEW)
      : [],
  );
  const songwriterTotal = $derived(
    songwriter ? songwriter.total || songwriter.tracks.length : 0,
  );
  const songwriterHasMore = $derived(
    !!songwriter &&
      (songwriter.tracks.length > POPULAR_PREVIEW || songwriter.total > POPULAR_PREVIEW),
  );

  function playSongwriter(i) {
    if (!songwriter?.tracks.length) return;
    /* Direct row playback remains in the full playlist context, even though
       only the first ten source rows are painted on this artist page. */
    api.playQueue(songwriter.tracks, i, `playlist:${songwriter.id}`).catch(() => {});
  }

  /* A shelf card knows an id and nothing else, so playing it costs one browse
     first — the shared rule for every card in the app (see lib/play.js). */
  async function playRelease(id) {
    if (busy) return;
    const generation = artistGeneration;
    busy = id;
    playError = "";
    try {
      await playAlbumById(id);
    } catch (reason) {
      if (generation === artistGeneration) {
        playError = String(reason || "Could not play this release.");
      }
    } finally {
      if (generation === artistGeneration) busy = "";
    }
  }
  /* -------------------------------------------------------- artist pick
     One shape, three kinds. The pinned item arrives tagged (see the engine's
     `ArtistPickItem`) precisely so the card can branch here rather than
     guessing from which fields happen to be populated.

     `subtitle` is the line that tells you WHAT you are being offered without
     repeating the kind pill above it. */
  const pick = $derived.by(() => {
    const raw = artistPick?.item;
    if (!raw?.data) return null;
    const data = raw.data;
    const kind = raw.kind;
    const common = {
      comment: artistPick.comment || "",
      id: data.id,
      name: data.name,
      cover: data.cover_url || "",
      covers: data.cover_urls ?? [],
    };
    if (kind === "track") {
      return {
        ...common,
        kind,
        label: "Song",
        subtitle: (data.artist_names ?? []).join(", "),
        track: data,
      };
    }
    if (kind === "album") {
      return {
        ...common,
        kind,
        /* Spotify calls a one-to-three-track release a single, and the pinned
           item is very often exactly that — but the pinned payload carries no
           album type, so this says the honest thing rather than guessing. */
        label: "Release",
        subtitle: [(data.artist_names ?? []).join(", "), data.year || null]
          .filter(Boolean)
          .join(" · "),
      };
    }
    return {
      ...common,
      kind: "playlist",
      label: "Playlist",
      subtitle: playlistSubtitle(data),
    };
  });

  function openPick() {
    if (!pick) return;
    if (pick.kind === "album") navigate("album", pick.id);
    else if (pick.kind === "playlist") navigate("playlist", pick.id);
  }

  async function playPick() {
    if (!pick) return;
    /* A pinned TRACK is the one kind with nothing to open, so the frame itself
       plays it and there is no second control. Everything else keeps the rule
       the rest of the app is built on: the card opens, the button plays. */
    if (pick.kind === "track") {
      api.playQueue([pick.track], 0, `artist:${artist?.id ?? ""}`).catch(() => {});
      return;
    }
    if (pick.kind === "album") await playRelease(pick.id);
    else await playPlaylist(pick.id);
  }

  async function playPlaylist(id) {
    if (busy) return;
    const generation = artistGeneration;
    busy = id;
    playError = "";
    try {
      await playPlaylistById(id);
    } catch (reason) {
      if (generation === artistGeneration) {
        playError = String(reason || "Could not play this playlist.");
      }
    } finally {
      if (generation === artistGeneration) busy = "";
    }
  }
</script>
{#snippet playlistCard(pl)}
  <div class="card" use:cardTone={[pl.cover_url || pl.cover_urls, pl.id]}>
    <div class="card-art">
      <Cover src={pl.cover_url} srcs={pl.cover_urls ?? []} id={pl.id} name={pl.name} fill lg />
      <button
        class="card-open"
        aria-label={`Open ${pl.name}`}
        onclick={() => navigate("playlist", pl.id)}
      ></button>
      <button
        class="card-play"
        aria-label={`Play ${pl.name}`}
        title={`Play ${pl.name}`}
        disabled={!!busy}
        onclick={() => playPlaylist(pl.id)}
      >
        <Icon name={busy === pl.id ? "more" : "play"} size={15} />
      </button>
    </div>
    <button class="card-copy" onclick={() => navigate("playlist", pl.id)}>
      <span class="card-name">{pl.name}</span>
      <span class="card-sub">{playlistSubtitle(pl)}</span>
    </button>
  </div>
{/snippet}

<!--
  THE PICK CARD — one card, three kinds, and the affordance is the difference.

  This section used to render the square shelf card, which was only ever
  reachable for a pinned playlist because the browse contract dropped every
  other kind. A pin is not a shelf item: there is exactly one of it, it is the
  artist SAYING something, and it can carry their words. So it is a wide frame
  with the artwork at a fixed 76px and the copy given the room instead — which
  is also the shape that lets a pinned song sit beside a pinned album without
  either looking like a mistake.
-->
{#snippet pickCard(item)}
  <div
    class="pick glass-card"
    class:playable={item.kind === "track"}
    use:cardTone={[item.cover || item.covers, item.id, true]}
  >
    <div class="pick-art">
      <Cover
        src={item.cover}
        srcs={item.covers}
        id={item.id}
        name={item.name}
        fill
        lg
      />
    </div>
    <div class="pick-copy">
      <span class="kind">{item.label}</span>
      <span class="pick-name">{item.name}</span>
      {#if item.subtitle}<span class="pick-sub">{item.subtitle}</span>{/if}
      {#if item.comment}
        <!-- The artist's own words. Set as a quote, because that is what it
             is, and it is the only thing on this page written by the person
             the page is about. -->
        <p class="pick-note">{item.comment}</p>
      {/if}
    </div>

    {#if item.kind === "track"}
      <!-- Nothing to open, so the whole frame is the play control and there is
           no floating button to duplicate it. -->
      <button
        class="pick-hit"
        aria-label={`Play ${item.name}`}
        title={`Play ${item.name}`}
        onclick={playPick}
      ></button>
      <span class="pick-play" aria-hidden="true"><Icon name="play" size={16} /></span>
    {:else}
      <button
        class="pick-hit"
        aria-label={`Open ${item.name}`}
        onclick={openPick}
      ></button>
      <button
        class="pick-play as-button"
        aria-label={`Play ${item.name}`}
        title={`Play ${item.name}`}
        disabled={!!busy}
        onclick={playPick}
      >
        <Icon name={busy === item.id ? "more" : "play"} size={16} />
      </button>
    {/if}
  </div>
{/snippet}


<!-- `bleed`: the banner carries its own negative margins up under the topbar,
     so the page must not pad above it. -->
<section class="view page bleed">
  <!-- 260px, fixed. The portrait is a background inside it, never a box that
       the header is sized around, so a missing portrait costs the image and
       nothing else — the gradient and the name stay exactly where they were. -->
  <header class="banner">
    <div class="bg">
      {#if artist}
        <Cover
          src={overview?.header_image_url || artist.cover_url}
          id={artist.id}
          name={artist.name}
          fill
        />
      {:else}
        <!-- Something has to be behind the banner's darkening gradient while
             the portrait is on its way, or the top of the page is pure black
             and reads as a rendering failure rather than as a wait. It used to
             be a flat `--bg-2` slab, which is a grey answer to a question the
             app can already answer in colour: the identity tile is a hash of
             the artist id, so the route alone is enough to know what colour
             this artist is before a single byte of the payload has landed.
             Which also means the wait is the artist's OWN colour and not a
             stand-in — an artist with no portrait keeps exactly this ground
             when the payload arrives, because that is the same tile. The
             banner suppresses the monogram, so what shows is the colour and
             nothing else, and the name is left to the skeleton below. -->
        <Cover id={route.id} name={artistNameHint(route.id)} fill />
      {/if}
    </div>
    <span class="tag">Artist</span>
    {#if artist}
      <h1 class="name">{artist.name}</h1>
      <!-- Under the name, the way the official client does it: reach is the
           one fact about an artist you want before you have decided whether to
           press play. A fixed slot is not worth it here — the line either
           exists for this artist for the whole visit or it never does. -->
      {#if overview?.monthly_listeners}
        <p class="banner-listeners">
          <span class="tnum">{NUMBER_FORMAT.format(overview.monthly_listeners)}</span> monthly listeners
        </p>
      {/if}
    {:else}
      <span class="skeleton line lg" style="margin-top:var(--s3);height:52px;width:min(420px,62%)"></span>
    {/if}
  </header>

  {#if detail.error && !artist}
    <div class="empty failed" style="margin-top:var(--s6)">
      <p class="h">This artist could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="pill" onclick={retryDetail}>Try again</button>
        <button class="link-more" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if !artist}
    <!-- The frame the artist arrives into. Every block below is the size of
         the block that replaces it, so nothing on this page moves when the
         payload lands — which is the whole point of drawing it at all. -->
    <div aria-hidden="true">
      <div class="actions" style="margin-top:var(--s6)">
        <span class="skeleton" style="width:48px;height:48px;border-radius:var(--rf)"></span>
        <span class="skeleton" style="width:40px;height:40px;border-radius:var(--rf)"></span>
      </div>
      <div class="section">
        <div class="section-head"><span class="skeleton line" style="width:110px;height:22px"></span></div>
        {#each Array.from({ length: 5 }) as _, i (i)}
          <div class="sk-row" style="--cols:28px 36px minmax(0,1fr) 52px">
            <span class="sk" style="width:12px"></span>
            <span class="sk art"></span>
            <span class="sk-stack">
              <span class="sk a" style="width:{62 - ((i * 9) % 24)}%"></span>
              <span class="sk b" style="width:{30 - ((i * 5) % 11)}%"></span>
            </span>
            <span class="sk" style="width:28px;justify-self:end"></span>
          </div>
        {/each}
      </div>
      <div class="section">
        <div class="section-head"><span class="skeleton line" style="width:132px;height:22px"></span></div>
        <div class="shelf" style:--per-row={perRow}>
          {#each Array.from({ length: perRow }) as _, i (i)}
            <div class="card">
              <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
              <span class="card-copy">
                <span class="skeleton line" style="width:72%;height:12px;margin:0"></span>
                <span class="skeleton line sm" style="height:10px;margin:6px 0 0"></span>
              </span>
            </div>
          {/each}
        </div>
      </div>
    </div>
  {:else}
    <!-- No meta line here. "10 popular songs / 31 releases" counted the two
         sections immediately below it, both of which already carry their own
         count beside their own heading. -->
    <div class="actions" style="margin-top:var(--s6)">
      <button class="play-lg" title="Play popular songs" onclick={() => playTop(0)} disabled={!top.length}>
        <Icon name="play" size={22} />
      </button>
      <button class="btn-round lg" title="Shuffle" aria-label="Shuffle popular songs" onclick={shuffleTop} disabled={!top.length}>
        <Icon name="shuffle" size={20} />
      </button>
      <FollowPill {artist} />
      <HeaderMenu label="Artist actions">
        {#snippet children(close)}
          <button class="menu-item" role="menuitem" disabled={!artist?.id} onclick={() => { close(); openArtistRadio(); }}><Icon name="radio" size={16} />Go to artist radio</button>
          <CopyLinkItem link={spotifyLink("artist", artist?.id)} {close} />
        {/snippet}
      </HeaderMenu>
    </div>

    {#if top.length}
      <div class="section" bind:this={popularSectionEl}>
        <div class="section-head">
          <h2 class="section-title">Popular</h2>
        </div>
        <!-- Popular is a bounded section of a longer page, so it renders all
             rows and owns no shared-pane scroll/resize pipeline. -->
        <TrackList tracks={visibleTop} playFrom={playTop} showAlbum={false} showPlays disableWindowing queueContext={`artist:${artist?.id ?? ""}`} />
        {#if top.length > POPULAR_PREVIEW}
          <!-- The expander sits where the eye leaves row five, not across the
               page in the header; aria-expanded keeps it honest to ATs. -->
          <button
            class="link-more popular-more"
            aria-expanded={popularExpanded}
            onclick={() => toggleExpand(popularSectionEl, () => (popularExpanded = !popularExpanded))}
          >
            {popularExpanded ? "Show less" : "See more"}
          </button>
        {/if}
      </div>
    {/if}

    {#if songwriter}
      <!--
        WRITTEN BY — this arrives after the artist overview and is deliberately
        optional. It sits immediately below Popular so the late response adds
        one self-contained section without replacing any already-rendered page.
        The rows stay in playlist source order and direct playback carries the
        playlist context.
      -->
      <section class="section written-by" aria-labelledby="written-by-title" bind:this={songwriterSectionEl}>
        <div class="section-head">
          <h2 class="section-title" id="written-by-title">Written by</h2>
        </div>
        <TrackList
          tracks={visibleSongwriter}
          playFrom={playSongwriter}
          showAlbum
          disableWindowing
          queueContext={`playlist:${songwriter.id}`}
        />
        {#if songwriterExpanded}
          <!-- Expanded state has one action only: leave for the complete
               playlist. It never duplicates the collapsed action. -->
          <button class="link-more popular-more" onclick={() => navigate("playlist", songwriter.id)}>
            {songwriterTotal
              ? `Open full playlist · ${NUMBER_FORMAT.format(songwriterTotal)} songs`
              : "Open full playlist"}
          </button>
        {:else}
          <div class="written-actions">
            <button class="link-more written-open" onclick={() => navigate("playlist", songwriter.id)}>
              {songwriterTotal
                ? `Open full playlist · ${NUMBER_FORMAT.format(songwriterTotal)} songs`
                : "Open full playlist"}
            </button>
            {#if songwriterHasMore}
              <button
                class="link-more popular-more"
                aria-expanded={false}
                onclick={() => toggleExpand(songwriterSectionEl, () => (songwriterExpanded = true))}
              >
                See more
              </button>
            {/if}
          </div>
        {/if}
      </section>
    {/if}

    {#if hasReleases}
      <section class="section dx" aria-labelledby="dx-title" bind:this={dxSectionEl}>
        <div class="section-head">
          <h2 class="section-title" id="dx-title">
            Discography<span class="section-count tnum">{releaseTotal}</span>
          </h2>
          <button class="link-more" onclick={openDiscography}>
            See all<Icon name="fwd" size={12} />
          </button>
        </div>

        <div class="seg" role="tablist" aria-label="Release types">
          {#each segments as option (option.id)}
            <button
              class="seg-btn"
              class:on={group === option.id}
              role="tab"
              aria-selected={group === option.id}
              onclick={() => {
                group = option.id;
                shelfExpanded = false;
              }}
            >
              {option.label}<span class="seg-n tnum">{option.count}</span>
            </button>
          {/each}
        </div>


        {#if visibleShelf.length}
          <!-- Exactly one row. Same card object as the library and search
               grids, and the same non-negotiable rule: the artwork OPENS the
               record, only the floating button plays it. -->
          <div class="shelf" style:--per-row={perRow}>
            {#each visibleShelf as entry (entry.release.id)}
              {@const release = entry.release}
              <div class="card" use:cardTone={[release.cover_url, release.id]}>
                <div class="card-art">
                  <Cover src={release.cover_url} id={release.id} name={release.name} fill lg />
                  <button
                    class="card-open"
                    aria-label={`Open ${release.name}`}
                    onclick={() => navigate("album", release.id)}
                  ></button>
                  <button
                    class="card-play"
                    aria-label={`Play ${release.name}`}
                    title={`Play ${release.name}`}
                    disabled={!!busy}
                    onclick={() => playRelease(release.id)}
                  >
                    <Icon name={busy === release.id ? "more" : "play"} size={15} />
                  </button>
                </div>
                <button class="card-copy" onclick={() => navigate("album", release.id)}>
                  <span class="card-name">{release.name}</span>
                  <span class="card-sub"
                    >{[release.year || null, entry.kind].filter(Boolean).join(" · ")}</span
                  >
                </button>
              </div>
            {/each}
          </div>
          {#if shelfExpandable}
            {#if shelfRemaining > 0 || shelfExpanded}
              <!-- Ranked releases are all here already, so this grows the row
                   rather than leaving the page. Same count, same wording; it
                   now delivers what it names. -->
              <button
                class="shelf-more"
                aria-expanded={shelfExpanded}
                onclick={() => toggleExpand(dxSectionEl, () => (shelfExpanded = !shelfExpanded))}
              >
                {shelfExpanded ? "Show less" : `${shelfRemaining} more`}
                <Icon name={shelfExpanded ? "chevron-up" : "chevron-down"} size={12} />
              </button>
            {/if}
          {:else if shelfRemaining > 0}
            <!-- The count is the point: it says how much is behind the link,
                 so "See all" is an informed click rather than a guess. -->
            <button class="shelf-more" onclick={openDiscography}>
              {shelfRemaining} more<Icon name="fwd" size={12} />
            </button>
          {/if}
        {:else}
          <p class="shelf-empty">
            The catalogue lists {counts[group] ?? 0} of these, but the artist payload
            carried no summaries for them. Open the full discography to read them.
          </p>
        {/if}
        {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}
        {#if currentShelfError}
          <p class="inline-error" role="alert">{currentShelfError}</p>
          <button class="link-more" onclick={retryShelf}>Try again</button>
        {/if}
      </section>
    {/if}
    {#if pick}
      <section class="section artist-pick" aria-labelledby="pick-title">
        <div class="section-head">
          <h2 class="section-title" id="pick-title">Artist Pick</h2>
        </div>
        {@render pickCard(pick)}
      </section>
    {/if}

    {#if playlistShelves.artist.length}
      <section class="section" aria-labelledby="artist-playlists-title">
        <div class="section-head">
          <h2 class="section-title" id="artist-playlists-title">
            Artist playlists<span class="section-count tnum">{playlistShelves.artist.length}</span>
          </h2>
          {#if playlistShelves.artist.length > perRow}
            <button class="link-more" onclick={() => navigate("artist-playlists", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf playlist-shelf" style:--per-row={perRow}>
          {#each playlistShelves.artist.slice(0, perRow) as playlist (playlist.id)}
            {@render playlistCard(playlist)}
          {/each}
        </div>
      </section>
    {/if}

    {#if playlistShelves.discovered.length}
      <section class="section" aria-labelledby="discovered-title">
        <div class="section-head">
          <h2 class="section-title" id="discovered-title">
            Discovered on<span class="section-count tnum">{playlistShelves.discovered.length}</span>
          </h2>
          {#if playlistShelves.discovered.length > perRow}
            <button class="link-more" onclick={() => navigate("discovered-on", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf playlist-shelf" style:--per-row={perRow}>
          {#each playlistShelves.discovered.slice(0, perRow) as playlist (playlist.id)}
            {@render playlistCard(playlist)}
          {/each}
        </div>
      </section>
    {/if}


    <!--
      ABOUT — the liner-note layer of this page, and it is set as one.

      Gold is the hue this design system reserves for facts ABOUT a record or
      the people who made it, as opposed to things you can do to them; the
      credits sheet already owns it. This card is the same kind of object, so
      it takes the same lead: a gold-led rule opens it, the popularity meter
      fills in gold, and each city row is washed in gold in proportion to its
      share. No panel and no border — grouping in this app is space and a
      hairline, and a boxed card here would be the only one in the interface.
    -->
    {#if hasAbout}
      <section class="section about" aria-labelledby="about-title">
        <!-- "About", not "About {name}". This is a section label in the same
             family as the Settings group headings, and it is one screen below
             a 64px display setting of the same name — so repeating it here
             bought nothing and cost the heading its rule as soon as the name
             wrapped the label to two lines. -->
        <h2 class="about-head" id="about-title">About</h2>
        <div class="about-body" class:no-figure={!aboutFigure} class:wide={aboutWide}>
          {#if aboutFigure}
            <!-- The official editorial photograph, whole and never cropped. It
                 was once drawn into the same 148px tile the search results use,
                 then into a fixed portrait box that cut the sides off a
                 landscape source. It is now shown at the gallery's own shape,
                 and clicking it opens the picture at the size it was made at. -->
            <figure
              class="about-figure"
              class:gallery={aboutFigures.length > 1}
              style:--figure-max="{figureMax}px"
              style:--frame-aspect={frameAspect || null}
              style:--tone-wash={figureTone.wash}
              style:--tone-glow={figureTone.glow}
            >
              <button
                class="figure-open"
                bind:this={figureButton}
                title="View at full size"
                aria-label={`View ${artist.name} at full size`}
                onclick={() => (lightboxOpen = true)}
              >
                <Cover src={aboutFigure.url} id={artist.id} name={artist.name} natural lg raised />
              </button>
              {#if aboutFigures.length > 1}
                <!-- A caption, not a carousel. No dots for eighteen pictures,
                     no auto-advance, nothing that moves on its own — two
                     steppers and the position, set at caption size under the
                     picture they belong to. -->
                <figcaption class="figure-nav">
                  <button class="fig-step" title="Previous picture" aria-label="Previous picture" onclick={() => stepFigure(-1)}>
                    <Icon name="back" size={12} />
                  </button>
                  <span class="fig-n tnum">{figurePosition + 1} / {aboutFigures.length}</span>
                  <button class="fig-step" title="Next picture" aria-label="Next picture" onclick={() => stepFigure(1)}>
                    <Icon name="fwd" size={12} />
                  </button>
                </figcaption>
              {/if}
            </figure>
          {/if}

          <!-- picture | figures | cities across the top, prose full width under.
               `.about-side` exists ONLY for the narrow case, where there is not
               room for three columns and the figures and the cities stack into
               one. When there IS room it is dissolved with `display: contents`
               so its two children become grid items in their own right — which
               is how the same markup serves both shapes without duplicating it
               or doing row arithmetic that breaks the moment an artist has no
               cities. Either way both sit beside the photograph, never under
               it. -->
          <div class="about-side">
          <div class="about-lede">
            {#if rank}
              <!-- ONE display number, not four. The worldwide rank is the fact
                   here you cannot get anywhere else in the app. -->
              <p class="rank">
                <span class="rank-n tnum">#{NUMBER_FORMAT.format(rank)}</span>
                <span class="caps">Worldwide</span>
              </p>
            {/if}

            {#if aboutStats.length}
              <dl class="about-stats">
                {#each aboutStats as fact (fact.label)}
                  <div>
                    <dt class="tnum">{fact.value}</dt>
                    <dd class="caps">{fact.label}</dd>
                  </div>
                {/each}
              </dl>
            {/if}

            {#if popularity !== null}
              <!-- Popularity kept, and framed as the oddity it is: it is a
                   real 0-100 figure the official client never puts on screen.
                   A meter rather than "72/100" — a fraction reads as a mark
                   out of a test, a filled rail reads as an instrument, and the
                   app already uses this exact shape for its cache gauge. -->
              <div class="pop">
                <div class="pop-line">
                  <span class="caps">Popularity</span>
                  <span class="pop-n tnum">{popularity}</span>
                </div>
                <div
                  class="pop-rail"
                  role="meter"
                  aria-valuenow={popularity}
                  aria-valuemin="0"
                  aria-valuemax="100"
                  aria-label="Spotify popularity"
                >
                  <span class="pop-fill" style:--p={popularity / 100}></span>
                </div>
              </div>
            {/if}
          </div>

            {#if topCities.length}
              <div class="cities">
                <h3 class="caps">Top cities</h3>
                <ol>
                  {#each topCities.slice(0, 5) as city, i (`${city.city}:${city.region}:${city.country}`)}
                    {@const place = [city.region, city.country].filter(Boolean).join(", ")}
                    {@const share = cityLeader && city.listeners ? city.listeners / cityLeader : 0}
                    <!-- The bar IS the row: a left-anchored wash sized to the
                         city's share of the leader, which is the same device
                         the playing track row uses. A separate bar element
                         beside a list would be a chart, and this is a list
                         that happens to have a shape. -->
                    <li style:--share={share}>
                      <span class="city-rank tnum">{i + 1}</span>
                      <span class="city-name">{city.city}</span>
                      {#if place}<span class="city-place">{place}</span>{/if}
                      {#if city.listeners}
                        <span class="city-n tnum">{NUMBER_FORMAT.format(city.listeners)}</span>
                      {/if}
                    </li>
                  {/each}
                </ol>
              </div>
            {/if}
          </div>

          <!-- Prose runs the full width under everything, and only exists when
               there is prose: an empty grid item still costs a row gap. -->
          {#if overview?.biography}
            <div class="about-rest">
              <!-- The ONLY thing on this page that expands. See lib/bio.js for
                   what the string turns out to contain. -->
              <div class="about-bio">
                <Biography source={overview.biography} lines={5} />
              </div>
            </div>
          {/if}
        </div>
        {#if aboutFigure}
          <!-- Outside the grid, deliberately: a <dialog> is a grid item like
               anything else until it is open, and a track that exists only to
               hold an invisible overlay is a gap in the card. It shares
               `figureIndex` with the frame, so the overlay and the page are
               never on different pictures. -->
          <GalleryLightbox
            open={lightboxOpen}
            images={aboutFigures}
            index={figurePosition}
            name={artist.name}
            onStep={stepFigure}
            onClose={closeLightbox}
          />
        {/if}
      </section>
    {/if}

    {#if visibleRelatedArtists.length}
      <section class="section" aria-labelledby="related-title">
        <div class="section-head">
          <h2 class="section-title" id="related-title">Fans also like</h2>
          {#if relatedArtists.length > visibleRelatedArtists.length}
            <button class="link-more" onclick={() => navigate("fans-also-like", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf" style:--per-row={perRow}>
          {#each visibleRelatedArtists as related (related.id)}
            <button
              class="card"
              use:cardTone={[related.cover_url, related.id]}
              onclick={() => navigateArtist(related.id, related.name)}
            >
              <span class="card-art">
                <Cover
                  src={related.cover_url}
                  id={related.id}
                  name={related.name}
                  fill
                  circle
                />
              </span>
              <span class="card-copy">
                <span class="card-name">{related.name}</span>
                <span class="card-sub">Artist</span>
              </span>
            </button>
          {/each}
        </div>
      </section>
    {/if}

    {#if appearsOnCount}
      <section class="section appears" aria-labelledby="appears-title">
        <div class="section-head">
          <h2 class="section-title" id="appears-title">
            Appears On<span class="section-count tnum">{appearsOnCount}</span>
          </h2>
          {#if appearsOnCount > perRow}
            <button class="link-more" onclick={() => navigate("appears-on", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf appears-shelf" style:--per-row={perRow} bind:this={appearsOnSentinel}>
          {#if visibleAppearsOn.length}
            {#each visibleAppearsOn as release (release.id)}
              <div class="card" use:cardTone={[release.cover_url, release.id]}>
                <div class="card-art">
                  <Cover src={release.cover_url} id={release.id} name={release.name} fill lg />
                  <button
                    class="card-open"
                    aria-label={`Open ${release.name}`}
                    onclick={() => navigate("album", release.id)}
                  ></button>
                  <button
                    class="card-play"
                    aria-label={`Play ${release.name}`}
                    title={`Play ${release.name}`}
                    disabled={!!busy}
                    onclick={() => playRelease(release.id)}
                  >
                    <Icon name={busy === release.id ? "more" : "play"} size={15} />
                  </button>
                </div>
                <button class="card-copy" onclick={() => navigate("album", release.id)}>
                  <span class="card-name">{release.name}</span>
                  <span class="card-sub">{[release.year || null, "Appears on"].filter(Boolean).join(" · ")}</span>
                </button>
              </div>
            {/each}
          {:else if appearsOnLoading}
            {#each Array.from({ length: perRow }) as _, i (i)}
              <div class="card" aria-hidden="true">
                <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
                <span class="card-copy">
                  <span class="skeleton line" style="width:72%;height:12px;margin:0"></span>
                  <span class="skeleton line sm" style="height:10px;margin:6px 0 0"></span>
                </span>
              </div>
            {/each}
          {:else}
            <p class="shelf-empty">Spotify lists {appearsOnCount} Appears On releases, but none are available yet.</p>
          {/if}
        </div>
        {#if appearsOnError}<p class="inline-error" role="alert">{appearsOnError}</p>{/if}
        {#if appearsOnError}<button class="link-more" onclick={retryAppearsOn}>Try again</button>{/if}
      </section>
    {/if}



    {#if !top.length && !hasReleases && !appearsOnCount && !popularReleases.length && !relatedArtists.length && !pick && !playlistShelves.artist.length && !playlistShelves.discovered.length && !songwriter && !songwriterLoading}
      <div class="empty">
        <p class="h">Nothing to show for this artist.</p>
        <p class="sub">The engine returned no popular songs or releases.</p>
      </div>
    {/if}
  {/if}
</section>

<style>
  .dx { margin-top: var(--s8); }
  .dx .seg { margin-bottom: var(--s5); }
  .appears { margin-top: var(--s9); }


  /* One row, always. `repeat(auto-fill, …)` was not an option: it decides how
     many cards fit and then wraps the rest, and a shelf that wraps is a grid.
     The count comes from the pane instead (see `perRow`) and the tracks are
     equal fractions, so the row is flush with the page at every width and no
     card is ever half-drawn at the edge. */
  .shelf {
    display: grid; gap: var(--s5);
    grid-template-columns: repeat(var(--per-row, 5), minmax(0, 1fr));
  }
  .playlist-shelf { margin-top: var(--s1); }
  .written-actions {
    display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4);
    margin-top: var(--s4);
  }
  .written-actions .link-more { margin-top: 0; }
  .written-open { color: var(--fg); }
  .written-open:hover { color: var(--accent); }
  /* The overflow, named. It sits under the shelf rather than beside the
     heading because it is about the row you just looked at. */
  .shelf-more {
    display: inline-flex; align-items: center; gap: var(--s1);
    margin-top: var(--s4); color: var(--fg-2); font-size: var(--t-12);
    transition: color var(--d1) var(--ease);
  }
  .shelf-more:hover { color: var(--accent); }
  .shelf-empty { max-width: 56ch; color: var(--fg-2); font-size: var(--t-12); }

  /* ------------------------------------------------------------ the pick */
  /* A PANEL, unlike every other card on this page, and it is the same panel
     the search Top Result uses: one singled-out item, tinted with its own
     artwork's colour so it reads as a thing that was chosen rather than as the
     first cell of a shelf that is not there. The shelf card it used to borrow
     was a 240px square that looked like a stray album with a caption. */
  /* A card on the pane (.glass-card, in the markup), its colour pooled in
     the glass as the Top Result's is. Hover adds light rather than a filter:
     a filter on a glass host re-rasters its rim, and bleaches the sleeve. */
  .pick {
    --pick-tone: linear-gradient(150deg, color-mix(in srgb, var(--tone-wash) 72%, transparent), transparent 82%);
    position: relative;
    display: grid; grid-template-columns: 88px minmax(0, 1fr) auto;
    align-items: center; gap: var(--s4);
    width: min(560px, 100%); padding: var(--s4);
    border-radius: var(--r3);
    background: var(--glass-sheen), var(--pick-tone), var(--lift-card);
    transition: background-color var(--d1) var(--ease);
  }
  .pick:hover { background: var(--glass-sheen), var(--pick-tone), var(--raise-2); }
  .pick-art { position: relative; width: 88px; height: 88px; --tile: 88px; }
  /* The sleeve throws its own colour under itself, as it does in the inspector
     and the credits sheet — what stops a tile reading as a stamp on a panel. */
  .pick-art :global(.art) {
    box-shadow: 0 10px 24px -10px color-mix(in srgb, var(--tone-glow) 78%, transparent);
  }
  /* `padding-right`, because the column that follows is the play button and an
     ellipsis that stops flush against it reads as text still running rather
     than text that has ended. */
  .pick-copy {
    display: flex; flex-direction: column; align-items: flex-start;
    gap: 3px; min-width: 0; padding-right: var(--s2);
  }
  .pick-copy > .kind { margin-bottom: 2px; }
  .pick-name {
    max-width: 100%;
    font-family: var(--font-display); font-size: var(--t-15); font-weight: var(--w-semi);
    letter-spacing: -0.01em; color: var(--fg);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .pick-sub {
    max-width: 100%; color: var(--fg-2);
    font-family: var(--font-small); font-size: var(--t-12);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  /* The artist's note, marked as a quotation by a rule rather than by quote
     marks: a curly quote around a sentence that may already contain one is a
     typographic gamble the rest of this app does not take. */
  /* `padding-right` is not spacing, it is headroom for the italic.
     An italic glyph inks past the advance width its line box is measured from,
     and `overflow: hidden` clips at the padding edge — so with no padding the
     last character of a note lost its right-hand stroke ("Toy Story 5" ended
     mid-5). The clamp still has to be there for long notes, so the fix is to
     move the clip edge, not remove it. */
  .pick-note {
    max-width: 46ch; margin-top: var(--s2);
    padding-left: var(--s3); padding-right: 0.25em;
    border-left: 2px solid color-mix(in srgb, var(--gold) 52%, transparent);
    color: var(--fg-1); font-size: var(--t-12); font-style: italic; line-height: 1.5;
    display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; overflow: hidden;
  }
  /* Transparent, full-bleed, and BELOW the play control — the same two-real-
     controls arrangement `.card` uses, for the same reason: a button may not
     contain a button. */
  .pick-hit {
    position: absolute; inset: 0; z-index: 1;
    border-radius: var(--r3); background: none;
  }
  .pick-play {
    position: relative; z-index: 2; justify-self: end;
    display: grid; place-items: center; width: 40px; height: 40px;
    border-radius: var(--rf); background: var(--accent) var(--bead); color: var(--accent-ink);
    box-shadow: var(--bead-edge), 0 8px 20px -8px color-mix(in srgb, var(--foam) 55%, transparent);
    transition: background-color var(--d1) var(--ease), transform var(--d1) var(--ease);
  }
  .pick-play.as-button:hover:not(:disabled) { background-color: var(--accent-hi); }
  .pick-play.as-button:active:not(:disabled) { transform: scale(0.94); }
  /* A pinned song has no separate control, so the glyph is decoration for a
     frame that is itself the button — and it has to react to the frame. */
  .pick.playable:hover .pick-play { background-color: var(--accent-hi); }

  /* ---------------------------------------------------------------- about */
  /* A CONTAINER, so the two-column split below reacts to the width this
     section actually has rather than to the width of the window. Those are
     different numbers by up to 576px here: the rail and the now-playing
     inspector are both fixed, so a 1400px window with the inspector open gives
     the pane less room than a 940px one without it. A `@media` query cannot
     see that, which is the same class of mistake the track grid's `--cols`
     comment above describes. Contained on the inline axis only, so the
     section's height still comes from its content. */
  .about { margin-top: var(--s9); container-type: inline-size; }
  /* Same object as a settings group heading: the label and its separator are
     one mark, and the lead-in names the layer. Gold here rather than foam —
     this is the liner-note section, not structure. */
  .about-head {
    display: flex; align-items: center; gap: var(--s3);
    font-family: var(--font-small);
    font-size: var(--t-caps); font-weight: var(--w-semi); letter-spacing: var(--track-caps);
    text-transform: uppercase; color: var(--label-hi);
    padding-bottom: var(--s5);
  }
  .about-head::after {
    content: ""; flex: 1; height: 1px;
    background: linear-gradient(90deg,
      color-mix(in srgb, var(--gold) 58%, transparent), var(--line) 28%, var(--line));
  }
  /* Narrow: picture | (figures over cities).  Wide: picture | figures | cities.
     Prose spans the full width underneath in both. */
  /* The picture's track is ELASTIC, not a 300px cap. A fixed maximum meant the
     photograph stayed 300px wide while the card had room to spare beside it,
     and — because the track could still shrink — it was free to end up SMALLER
     than that when space got tight. Worst of both. A fractional track with a
     floor grows into whatever the card has and never collapses; the ceiling
     lives on the figure so an enormous window does not turn an editorial photo
     into a poster.

     The fraction itself is larger than it was, and the floor with it. These
     are editorial photographs, often 1600px at source, and the whole reason
     they are on the page is to be looked at; a third of the card put them on
     screen at about 300px, which is a thumbnail with a caption. The copy and
     the cities lose a little width and lose nothing else — the popularity
     meter is capped at 340px anyway, and a city row ellipsises its region
     before anything else gives. */
  .about-body {
    display: grid; grid-template-columns: minmax(240px, 1.15fr) minmax(0, 1fr);
    gap: var(--s6) var(--s7); align-items: start;
  }
  .about-side { display: flex; flex-direction: column; gap: var(--s6); min-width: 0; }
  .about-lede, .cities { min-width: 0; }
  .about-rest { grid-column: 1 / -1; min-width: 0; }
  /* Three columns once there is room for them, which is where `display:
     contents` earns its keep: the wrapper stops generating a box and its two
     children place themselves as grid items. */
  .about-body.wide {
    grid-template-columns: minmax(260px, 1.6fr) minmax(0, 0.95fr) minmax(0, 1.1fr);
  }
  .about-body.wide .about-side { display: contents; }
  /* No image, no column. An empty tile beside the copy would be a hole that
     reads as a failed load; the copy simply takes the width instead. */
  .about-body.no-figure { grid-template-columns: minmax(0, 1fr); }
  .about-body.no-figure.wide { grid-template-columns: minmax(0, 1fr) minmax(0, 1.15fr); }
  /* The picture is shown WHOLE, at its own proportions.
     Stretching it to the height of the copy beside it sounded tidy and was
     wrong: these editorial images are usually landscape (a common one is
     640x429), and a 300px-wide column stretched to 320-460px tall is a
     portrait box, so `cover` cut the sides off a wide photograph to fill a
     tall hole. Letting the height follow the width means no crop at all and
     no letterbox either — the frame is whatever the image's frame is. */
  /* The shape is the picture's own — see the `natural` mode on Cover, which is
     where width/height/aspect are settled. The ceiling comes in as
     `--figure-max`, computed from the gallery's own widest source: past that
     the picture is only being enlarged, not shown. */
  .about-figure {
    margin: 0; width: 100%; max-width: var(--figure-max, 560px);
    align-self: start; --tile: 300px;
  }
  /* A gallery is mounted; a lone photograph is free-standing.

     Natural proportions are exactly right for one editorial photograph and
     exactly wrong for eighteen of them. Measured in this column, the gallery
     runs from 279x119 to 279x402, so stepping resized the figure by up to
     250px: the Next button walked out from under the cursor between clicks,
     and every section below About moved with it. A picture browser whose
     browsing shoves the page is not a quiet one.

     So when there is a gallery the picture is mounted in one frame with the
     photograph contained whole inside it and the frame's own surface showing
     as the mat. Nothing is cropped and nothing moves. That frame used to be a
     square, which was stable because it ignored every picture in the set;
     `--frame-aspect` is the median of the set's own ratios, which is stable
     because it is computed from all of them at once and never changes as you
     step. `.art` already is the tile; these rules only decline the `natural`
     overrides and paint the mat. */
  /* The frame is GEOMETRY, not decoration, and that distinction is the whole
     of this block. Something has to reserve the tallest shape in the set or
     the page shoves on every step; nothing has to draw it. Painting it — as
     `--bg-2`, and later as a tinted mat of the picture's own light — put a
     rectangle on screen whose edges belong to the median of thirteen
     photographs rather than to the one you are looking at, so it read as a
     box the picture had failed to fill. So the box keeps its size and gives
     up its surface: no ground, no ring, no radius, and `overflow: visible`
     so the light below is not clipped back to those same edges.

     Centring is done by the BOX rather than by `object-fit`, which is what
     lets everything else here be honest. Contained inside a fixed element,
     an <img>'s border box is still the whole frame and only its painted
     pixels are inset — so a radius rounded the frame's corners and a shadow
     traced the frame's edges, and the picture got neither. Sized to fit and
     centred by flex, the element IS the photograph: the radius rounds the
     print, and a plain `box-shadow` follows it exactly, with none of the
     raster cost a `drop-shadow` filter would have charged for the same
     outline. */
  .about-figure.gallery :global(.art.natural) {
    aspect-ratio: var(--frame-aspect, 1); height: auto;
    display: flex; align-items: center; justify-content: center;
    background: none; box-shadow: none; overflow: visible;
  }
  /* The print lies on the page and lights it. Two shadows, one job each: a
     tight dark one for the millimetre of depth that says the photograph is
     on top of the page rather than punched into it, and a wide soft one in
     the picture's own hue — `coverTone` has already measured this url for
     the lightbox, same key, same cache — so the light around a print comes
     from the print. It is the gesture the sleeve already makes in the
     now-playing rail, at a fraction of the strength, because this one has to
     survive being looked at for as long as you read the biography. */
  .about-figure.gallery :global(.art.natural > img) {
    width: auto; height: auto; max-width: 100%; max-height: 100%;
    border-radius: var(--r3);
    box-shadow:
      0 16px 36px -14px rgba(0, 0, 0, 0.78),
      0 0 72px -10px color-mix(in srgb, var(--tone-glow) 46%, transparent);
  }
  /* The picture is a control, and it says so under the pointer only: a
     permanent badge on a photograph is chrome laid over the thing you came to
     look at. */
  .figure-open { display: block; width: 100%; border-radius: var(--r3); }
  .figure-open :global(.art) { transition: filter var(--d1) var(--ease); }
  .figure-open:hover :global(.art) { filter: brightness(1.1); }
  /* Left-aligned under the picture's edge, at the size the rest of the card's
     micro-text is set at. It is a caption; it should read as one. */
  .figure-nav {
    display: flex; align-items: center; gap: var(--s1);
    margin-top: var(--s2); color: var(--fg-2);
  }
  .fig-step {
    display: grid; place-items: center; width: 22px; height: 22px;
    border-radius: var(--rf); color: var(--fg-2);
    transition: color var(--d1) var(--ease), background-color var(--d1) var(--ease);
  }
  .fig-step:hover { color: var(--fg); background: var(--hover-2); }
  .fig-n {
    padding: 0 var(--s1);
    font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-2);
  }
  /* Measure belongs to the prose, not to the block that holds it: the cities
     want the width, a paragraph does not. */
  .about-bio :global(*) { max-width: 78ch; }

  .rank { display: flex; align-items: baseline; gap: var(--s3); }
  .rank-n {
    font-family: var(--font-display); font-size: var(--t-32); font-weight: var(--w-bold);
    letter-spacing: -0.03em; line-height: 1; color: var(--fg);
  }
  .about-stats { display: flex; flex-wrap: wrap; gap: var(--s3) var(--s7); margin: var(--s5) 0 0; }
  /* A step and a half below the rank, deliberately: these are supporting
     figures, and at display size they competed with the one number that is
     genuinely hard to find anywhere else. */
  .about-stats dt {
    font-family: var(--font-number); font-size: var(--t-15); font-weight: var(--w-med);
    line-height: 1.2; color: var(--fg);
  }
  .about-stats dd { margin: 2px 0 0; }

  .pop { margin-top: var(--s6); max-width: 340px; }
  .pop-line { display: flex; align-items: baseline; justify-content: space-between; gap: var(--s3); }
  .pop-n {
    font-family: var(--font-number); font-size: var(--t-13); font-weight: var(--w-med);
    color: color-mix(in srgb, var(--gold) 70%, var(--fg));
  }
  .pop-rail {
    position: relative; height: 4px; margin-top: var(--s2); overflow: hidden;
    border-radius: var(--rf); background: rgba(255, 255, 255, 0.07);
  }
  /* scaleX on a pre-painted gradient, like every other rail in the app. Gold
     rather than the foam-to-rose signature: the signature says "this is the
     app", and this is a fact about the artist. */
  .pop-fill {
    position: absolute; inset: 0; transform-origin: left; transform: scaleX(var(--p, 0));
    background: linear-gradient(90deg,
      color-mix(in srgb, var(--gold) 45%, transparent), var(--gold));
  }
  .about-bio {
    margin-top: var(--s6); padding-top: var(--s5); border-top: 1px solid var(--line);
  }

  /* The rule separates the cities from the figures ABOVE them, so it only
     belongs when something is actually above them. In the three-column layout
     the cities are their own column starting at the top of the card, and the
     rule was a hairline hanging over nothing. */
  .cities { margin-top: var(--s6); padding-top: var(--s5); border-top: 1px solid var(--line); }
  .about-body.wide .cities { margin-top: 0; padding-top: 0; border-top: none; }
  .cities h3 { margin: 0 0 var(--s3); }
  .cities ol { margin: 0; padding: 0; list-style: none; }
  /* [rank] [city] [region, country] [listeners] — one grid so four columns of
     five rows line up, instead of four independently-wrapping flex rows. */
  .cities li {
    position: relative;
    display: grid; grid-template-columns: 16px auto minmax(0, 1fr) auto;
    align-items: baseline; gap: var(--s3);
    padding: var(--s2) 0 9px; font-size: var(--t-12); color: var(--fg-1);
  }
  /* The comparison is a 2px RULE under each row, not a wash behind it.
     A washed row was the first thing tried and it was the mistake the palette
     block in app.css already documents: gold at 15% over near-black resolves
     to about #2a2318, so five rows of it read as five dim brown blocks rather
     than as one colour at five widths. Alpha destroys chroma; the fix is to
     spend LESS AREA at MORE strength. Two pixels of near-solid gold is
     unmistakably gold, and the width still carries the whole comparison. */
  .cities li::before {
    content: ""; position: absolute; inset: auto 0 0 0; height: 2px;
    border-radius: 1px; background: var(--line);
  }
  .cities li::after {
    content: ""; position: absolute; inset: auto auto 0 0; height: 2px;
    width: calc(var(--share, 0) * 100%);
    border-radius: 1px; background: var(--gold); opacity: 0.82;
  }
  .city-rank { color: var(--fg-3); font-size: var(--t-11); text-align: right; }
  .city-name { color: var(--fg); font-weight: var(--w-med); }
  .city-place {
    min-width: 0; color: var(--fg-3); font-family: var(--font-small); font-size: var(--t-11);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .city-n { flex: none; color: var(--fg-2); font-family: var(--font-number); font-size: var(--t-11); }

  /* Below this the copy column is narrower than the picture beside it, which
     is the point at which the picture has stopped being an illustration and
     started being an obstruction. It goes above the copy instead, at a size
     that still reads as a photograph. */
  @container (max-width: 680px) {
    .about-body { grid-template-columns: minmax(0, 1fr); gap: var(--s5); }
    .about-side { display: flex; }
    /* Width only. Forcing a ratio here would put the old portrait box back and
       reserve space the picture does not occupy. It is wider than it was for
       the same reason the tracks above are: stacked above the copy the picture
       has the whole card to work with, and 260px of it was timid. */
    .about-figure { width: min(420px, 80%); }
  }
</style>
