<script>
  import { untrack } from "svelte";
  import { route } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Menu from "./Menu.svelte";

  /**
   * The "…" that ends every page header's action row: a 40px round button
   * and the menu it opens. The row itself keeps only what is worth a glyph
   * (play, shuffle, add to queue) and at most one state pill; everything
   * secondary lives here, so a header never becomes a row of buttons again.
   *
   * `children` receives `close`, as Menu's items do.
   */
  let { label, children } = $props();

  let open = $state(false);
  let button = $state(null);

  /* A view is reused when one record's route replaces another's; a menu
     opened on the last one must not survive onto this one. */
  $effect(() => {
    route.name;
    route.id;
    untrack(() => (open = false));
  });
</script>

<button
  class="btn-round lg"
  bind:this={button}
  title={label}
  aria-label={label}
  aria-haspopup="menu"
  aria-expanded={open}
  onclick={() => (open = !open)}
>
  <Icon name="more" size={20} />
</button>
{#if open}
  <!-- A group menu: an item may hang a submenu off it (Move to folder). -->
  <Menu anchor={button} {label} group onclose={() => (open = false)} {children} />
{/if}
