<script>
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { navigate } from "../lib/state.svelte.js";
  import { parseBiography } from "../lib/bio.js";

  /**
   * An artist biography, rendered from its markup rather than printed as
   * source or injected as HTML. See lib/bio.js for what the string actually
   * contains and why this is a parser and not `{@html}`.
   *
   * The expand/collapse belongs to THIS component and to nothing else on the
   * artist page. It clamps lines rather than slicing the string, so the words
   * are never cut mid-link and the markup is parsed exactly once regardless of
   * which state it is in.
   */
  let { source = "", lines = 4 } = $props();

  const nodes = $derived(parseBiography(source));
  let expanded = $state(false);
  $effect.pre(() => {
    source;
    expanded = false;
  });
  /* Whether there is anything to expand. Measured on the collapsed element —
     a clamp that has nothing to hide must not offer a "Read more" that does
     nothing when pressed, and the only honest test is the rendered height. */
  let body = $state(null);
  let clipped = $state(false);
  $effect(() => {
    const el = body;
    // Re-runs when the text or the clamp changes; both change the answer.
    nodes;
    lines;
    if (!el) return;
    const measure = () => {
      if (!body) return;
      clipped = expanded || body.scrollHeight - body.clientHeight > 2;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  });

  function openExternal(href) {
    openUrl(href).catch(() => {});
  }
</script>

{#snippet content(list)}
  {#each list as node, i (i)}
    {#if node.kind === "text"}{node.value}
    {:else if node.kind === "break"}<br />
    {:else if node.kind === "paragraph"}
      <span class="bio-p">{@render content(node.children)}</span>
    {:else if node.kind === "emphasis"}
      {#if node.strong}<strong>{@render content(node.children)}</strong>
      {:else}<em>{@render content(node.children)}</em>{/if}
    {:else if node.kind === "route"}
      <!--
        A SPAN with role="link", for the same reason ArtistLinks is one: an
        inline `<button>` is an atomic box, so a clamped paragraph could not
        break a line inside it and would slice the name through a glyph.
      -->
      <span
        class="bio-link"
        role="link"
        tabindex="0"
        onclick={() => navigate(node.route, node.id)}
        onkeydown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), navigate(node.route, node.id))}
      >{@render content(node.children)}</span>
    {:else if node.kind === "external"}
      <span
        class="bio-link out"
        role="link"
        tabindex="0"
        title={node.href}
        onclick={() => openExternal(node.href)}
        onkeydown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), openExternal(node.href))}
      >{@render content(node.children)}</span>
    {/if}
  {/each}
{/snippet}

{#if nodes.length}
  <div class="bio">
    <p
      class="bio-body"
      class:clamped={!expanded}
      style:--bio-lines={lines}
      bind:this={body}
    >{@render content(nodes)}</p>
    {#if clipped}
      <button class="bio-toggle" aria-expanded={expanded} onclick={() => (expanded = !expanded)}>
        {expanded ? "Show less" : "Read more"}
      </button>
    {/if}
  </div>
{/if}

<style>
  .bio-body {
    max-width: 68ch;
    color: var(--fg-1); font-size: var(--t-13); line-height: 1.62;
  }
  /* Line clamp, not a character count: a truncated string cuts through the
     middle of a linked artist name, and there is no length in characters that
     is the right one at every pane width. */
  .bio-body.clamped {
    display: -webkit-box; -webkit-box-orient: vertical;
    -webkit-line-clamp: var(--bio-lines, 4);
    overflow: hidden;
  }
  /* Paragraphs inside a clamped box have to stay inline-level or the clamp has
     nothing to count, so the break is drawn as space rather than as a block. */
  .bio-p { display: block; }
  .bio-p + .bio-p { margin-top: var(--s3); }
  .bio-body :global(strong) { font-weight: var(--w-med); color: var(--fg); }

  /* Links in a biography are almost always other artists, which makes them
     the same object as the artist line on a track row — so they get the same
     treatment, plus a resting underline. Inside a paragraph of running text a
     hover-only affordance is invisible: you cannot hover a whole paragraph to
     find out which four words are navigable. */
  .bio-link {
    color: var(--fg); cursor: pointer;
    text-decoration: underline;
    text-decoration-color: color-mix(in srgb, var(--foam) 42%, transparent);
    text-decoration-thickness: 1px;
    text-underline-offset: 2px;
    transition: color var(--d1) var(--ease), text-decoration-color var(--d1) var(--ease);
  }
  .bio-link:hover, .bio-link:focus-visible {
    color: var(--accent);
    text-decoration-color: var(--accent);
  }
  .bio-toggle {
    display: inline-block; margin-top: var(--s2);
    color: var(--fg-2); font-size: var(--t-12);
    transition: color var(--d1) var(--ease);
  }
  .bio-toggle:hover { color: var(--accent); }
</style>
