<script>
  import { notice } from "./notice.svelte.js";

  /* The app's own popup glass, low over the pane, for a few seconds: long
     enough to read a line, and gone without being dismissed. */
  const SHOWN_MS = 3200;

  let shown = $state(false);

  $effect(() => {
    if (!notice.serial) return;
    shown = true;
    const timer = setTimeout(() => (shown = false), SHOWN_MS);
    return () => clearTimeout(timer);
  });
</script>

<div class="demo-notice glass-overlay" class:shown role="status" aria-live="polite">
  {notice.text}
</div>

<style>
  .demo-notice {
    position: fixed;
    z-index: 900;
    left: 50%;
    bottom: calc(var(--player-h) + 3 * var(--gutter) + var(--s3));
    max-width: min(520px, calc(100vw - 2 * var(--s6)));
    padding: var(--s2) var(--s4);
    border-radius: var(--rf);
    font-size: var(--t-13);
    line-height: 1.4;
    text-align: center;
    pointer-events: none;
    opacity: 0;
    visibility: hidden;
    transform: translate(-50%, 6px);
    transition: opacity var(--d2) var(--ease), transform var(--d2) var(--ease), visibility 0s linear var(--d2);
  }
  .demo-notice.shown {
    opacity: 1;
    visibility: visible;
    transform: translate(-50%, 0);
    transition: opacity var(--d2) var(--ease), transform var(--d2) var(--ease);
  }
  @media (prefers-reduced-motion: reduce) {
    .demo-notice, .demo-notice.shown { transform: translate(-50%, 0); }
  }
</style>
