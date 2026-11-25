<script>
  import { untrack } from "svelte";
  import { coverUrl } from "../lib/state.svelte.js";
  import { identityTone } from "../lib/covertone.svelte.js";

  /**
   * Artwork in four tiers, falling back in order:
   *
   *   1. the entity's own cover
   *   2. a 2x2 mosaic of four distinct album covers
   *   3. a single cover, full-bleed, when fewer than four are distinct
   *      (a 1x2 or L-shaped 3-up reads as broken, so we never draw one)
   *   4. a generated identity tile keyed off the entity id
   *
   * Tier 4 is deterministic and needs no network, so the common case for a
   * playlist — Spotify's rootlist carries no cover at all — paints instantly
   * and still looks designed rather than absent. Tiers 1–3 wait on the
   * network; while they wait the tile holds a quiet neutral ground, never a
   * borrowed letter — a monogram that vanishes a beat later reads as a
   * mistake, not a placeholder.
   */
  let {
    src = "",
    srcs = [],
    id = "",
    name = "",
    size = 48,
    /**
     * Stretch to the container instead of taking a pixel size. The tile still
     * takes explicit dimensions — 100% rather than none — so it can never fall
     * back to shrink-wrapping the image's natural 300px in a container that
     * does not happen to constrain it. The container supplies `--tile` for the
     * monogram scale.
     */
    fill = false,
    /**
     * Show the source at its OWN proportions: width leads, height follows.
     *
     * Every other mode puts the image in a box of a known shape and crops to
     * it, which is right for a tile in a row of tiles. It is wrong for a lone
     * editorial photograph, where the frame the photographer chose is part of
     * the picture. `fill` cannot do this: it writes `height: 100%` inline, so
     * the tile keeps a height the image no longer fills and `.art`'s own
     * `background` paints the difference as a grey slab under the picture.
     *
     * Only meaningful for a real image; the generated monogram tile stays
     * square, because a letter has no proportions to respect.
     */
    natural = false,
    lg = false,
    circle = false,
    raised = false,
    class: cls = "",
  } = $props();

  /** Distinct candidates for the mosaic, capped at the four cells. */
  const pool = $derived([...new Set(srcs.filter(Boolean))].slice(0, 4));

  /** Tier selection. `src` always wins: it is the entity's own artwork. */
  const tier = $derived(src ? "single" : pool.length >= 4 ? "mosaic" : pool.length ? "single" : "gen");
  const primary = $derived(src || pool[0] || "");

  const letter = $derived((name.trim()[0] ?? "?").toUpperCase());
  /* One hash, one ring of hues, shared with the header wash — so a coverless
     playlist's tile and its page open on the same colour. See covertone. */
  const seedTone = $derived(identityTone(id || name));
  /** Failed sources use the same generated fallback as missing artwork. */
  let failed = $state({});
  let shown = $state({});

  const activeUrls = $derived(tier === "mosaic" ? pool : primary ? [primary] : []);
  $effect.pre(() => {
    const keep = new Set(activeUrls);
    untrack(() => {
      for (const key of Object.keys(shown)) {
        if (!keep.has(key)) delete shown[key];
      }
      for (const key of Object.keys(failed)) {
        if (!keep.has(key)) delete failed[key];
      }
    });
  });

  /** Every image of the selected tier has painted. */
  const revealed = $derived(
    tier === "mosaic" ? pool.every((u) => shown[u]) : !!shown[primary],
  );
  /** The tier can no longer complete — its own cover or a mosaic cell
      failed — so the generated identity tile takes over, which is where a
      failed cover has always landed. */
  const lost = $derived(
    tier === "mosaic" ? pool.some((u) => failed[u]) : !!failed[primary],
  );

  /** Preserve the outgoing image while the browser decodes its replacement. */
  function bindArt(node, url) {
    let gate = null;
    const arm = (u) => {
      gate?.abort();
      gate = new AbortController();
      if (node.complete && node.naturalWidth > 0) {
        shown[u] = true;
        return;
      }
      node.addEventListener("load", () => (shown[u] = true), { once: true, signal: gate.signal });
      node.addEventListener("error", () => (failed[u] = true), { once: true, signal: gate.signal });
    };
    arm(url);
    return {
      update: arm,
      destroy: () => gate?.abort(),
    };
  }
</script>

{#if tier !== "gen" && !lost}
  <span
    class="art {cls}"
    class:lg
    class:circle
    class:raised
    class:mosaic={tier === "mosaic"}
    class:natural
    class:pending={!revealed}
    class:ready={revealed}
    style:width={fill || natural ? "100%" : `${size}px`}
    style:height={natural ? null : fill ? "100%" : `${size}px`}
  >
    <!-- width/height attributes as well as the CSS above: a load that fails
         still reserves the identical box, so nothing reflows around it. -->
    <!-- loading="lazy": the sidebar and queue render covers far below the
         fold; offscreen ones must not be fetched or decoded until scrolled
         to. decoding="async": the visible slice paints without waiting for
         the rest of the decode queue. Both are safe because every image has
         explicit dimensions — nothing can reflow around a late decode. -->
    {#if tier === "mosaic"}
      {#each pool as url (url)}
        <img use:bindArt={url} class:fresh={!shown[url]} src={coverUrl(url)} alt="" width={Math.round(size / 2)} height={Math.round(size / 2)} draggable="false" loading="lazy" decoding="async" />
      {/each}
    {:else}
      <img use:bindArt={primary} class:fresh={!shown[primary]} src={coverUrl(primary)} alt={name} width={size} height={size} draggable="false" loading="lazy" decoding="async" />
    {/if}
  </span>
{:else}
  <span
    class="art gen {cls}"
    class:lg
    class:circle
    class:raised
    style:width={fill ? "100%" : `${size}px`}
    style:height={fill ? "100%" : `${size}px`}
    style:--tone-a={seedTone.tileA}
    style:--tone-b={seedTone.tileB}
    style:--tile={fill ? null : `${size}px`}
    data-letter={letter}
    role="img"
    aria-label={name}
  ></span>
{/if}
