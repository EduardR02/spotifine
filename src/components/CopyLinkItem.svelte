<script>
  import { writeClipboard } from "../lib/spotify-link.js";
  import Icon from "./Icon.svelte";

  /**
   * "Copy link", the one item every shareable thing's menu carries.
   *
   * The confirmation lives on the item, which is why the menu does not close
   * on click: a copy with no feedback is indistinguishable from a dead
   * control. A copy that landed turns its glyph into a foam check and closes
   * the menu shortly after, through the menu's own close, so the trigger gets
   * its focus back; a refused write stays up and says so — the item is also
   * the retry. The item dies with its menu, and so do its state and its timer.
   */
  let { link, close } = $props();

  let copied = $state("idle");
  let timer = 0;
  let generation = 0;
  $effect(() => {
    link;
    copied = "idle";
    return () => {
      generation += 1;
      clearTimeout(timer);
    };
  });

  async function copy() {
    const target = link;
    if (!target) return;
    const mine = generation;
    const ok = await writeClipboard(target);
    if (mine !== generation) return;
    copied = ok ? "copied" : "failed";
    clearTimeout(timer);
    if (ok) timer = setTimeout(() => {
      if (mine === generation) close?.(true);
    }, 900);
  }
</script>

<button
  class="menu-item"
  role="menuitem"
  class:done={copied === "copied"}
  class:failed={copied === "failed"}
  disabled={!link}
  onclick={copy}
>
  <Icon name={copied === "copied" ? "check" : "copy"} size={16} />
  {copied === "copied" ? "Link copied" : copied === "failed" ? "Copy failed" : "Copy link"}
</button>
