<script>
  import { untrack } from "svelte";

  /**
   * The one menu surface: overlay glass in the top layer.
   *
   * Every "…" in a page header, the rail's right-click menu and the player
   * bar's device list are this. It hangs off an ANCHOR (its trigger) or a
   * POINT (a context menu), is placed once when it opens — flipped to the
   * other side when the preferred one has no room, and held inside the
   * window — and it goes away the way a menu is expected to: Escape (focus
   * back to the trigger), a pointer down anywhere outside it, Tab, a scroll or
   * a resize. A menu that followed a scrolling anchor would be a layout read
   * per frame on the scroller; dismissing is both cheaper and what the
   * pointer is asking for.
   *
   * The top layer is outside every stacking context and clip on the page, so
   * nothing — the sticky topbar, a scroller, the player bar's own layer — can
   * cut it off. `manual`, because dismissal is handled here and two of these
   * may coexist.
   *
   * The host renders it inside an `{#if}` and is told to drop it through
   * `onclose`; the items get `close` from the snippet, so an item can close
   * the menu it is in (`close(true)` hands the focus back to the trigger).
   */
  let {
    anchor = null,
    at = null,
    returnTo = null,
    align = "start",
    side = "below",
    label = "",
    class: extra = "",
    placement = null,
    group = false,
    element = $bindable(null),
    onclose,
    children,
  } = $props();

  const el = $derived(element);
  let left = $state(0);
  let top = $state(0);

  function close(returnFocus = false) {
    const hadFocus = !!el?.contains(document.activeElement);
    const target = returnTo ?? anchor;
    // A virtualized host may drop its anchor when onclose clears the menu.
    // Manual popovers are not modal: hand focus back while that anchor is owned.
    if (returnFocus && hadFocus && target?.isConnected) target.focus({ preventScroll: true });
    onclose?.();
  }

  function place() {
    if (placement) return;
    const box = el.getBoundingClientRect();
    const w = box.width;
    const h = box.height;
    const gap = 6;
    const maxLeft = window.innerWidth - w - 8;
    const maxTop = window.innerHeight - h - 8;
    if (anchor) {
      const r = anchor.getBoundingClientRect();
      const x = align === "end" ? r.right - w : r.left;
      const below = r.bottom + gap;
      const above = r.top - gap - h;
      const y = side === "above"
        ? (above >= 8 ? above : below)
        : (below <= maxTop ? below : above >= 8 ? above : below);
      left = Math.max(8, Math.min(x, maxLeft));
      top = Math.max(8, Math.min(y, maxTop));
    } else if (at) {
      left = Math.max(8, Math.min(at.x, maxLeft));
      top = at.y + h > window.innerHeight - 8 ? Math.max(8, at.y - h) : at.y;
    }
  }

  function items() {
    return [...el.querySelectorAll('[role^="menuitem"]:not(:disabled)')]
      .filter((item) => item.closest('[role="menu"]') === el);
  }

  $effect(() => {
    const node = el;
    if (!node) return;
    for (const item of node.querySelectorAll(".menu-item:not([role])")) item.setAttribute("role", "menuitem");
    node.showPopover();
    untrack(place);
    queueMicrotask(() => items()[0]?.focus());
    /* The trigger is exempt, so that its own click can toggle the menu shut. */
    const trigger = untrack(() => anchor);
    const onDown = (event) => {
      if (node.contains(event.target) || trigger?.contains(event.target)) return;
      if (group && event.target.closest?.(".menu")) return;
      close();
    };
    const onScroll = (event) => {
      if (node.contains(event.target) || (group && event.target.closest?.(".menu"))) return;
      close();
    };
    const onResize = () => close();
    document.addEventListener("pointerdown", onDown, true);
    document.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    return () => {
      document.removeEventListener("pointerdown", onDown, true);
      document.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
      if (node.isConnected) node.hidePopover();
    };
  });

  function onKeyDown(event) {
    if (event.target.closest?.('[role="menu"]') !== el && event.key !== "Tab") return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
      return;
    }
    if (event.key === "Tab") {
      close();
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    const list = items();
    if (!list.length) return;
    const current = list.indexOf(document.activeElement);
    const next =
      event.key === "Home" ? 0
      : event.key === "End" ? list.length - 1
      : event.key === "ArrowDown" ? (current + 1) % list.length
      : (current - 1 + list.length) % list.length;
    list[next]?.focus();
  }
</script>

<div
  class="menu glass-overlay {extra}"
  popover="manual"
  role="menu"
  tabindex="-1"
  aria-label={label || undefined}
  bind:this={element}
  style:left="{placement ? placement.x : left}px"
  style:top={placement ? (placement.top === null ? null : `${placement.top}px`) : `${top}px`}
  style:bottom={placement?.bottom == null ? null : `${placement.bottom}px`}
  onkeydown={onKeyDown}
>
  {@render children?.(close)}
</div>

<style>
  /* Placed in window coordinates from the anchor's rect, once. */
  .menu {
    position: fixed;
    inset: auto;
    margin: 0;
    max-width: min(320px, calc(100vw - 16px));
  }
</style>
