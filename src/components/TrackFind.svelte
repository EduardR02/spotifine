<script>
  import Icon from "./Icon.svelte";

  /* =====================================================================
     FIND IN A LIST — a round button among a page's actions that opens into
     a field (THE FIELD) in the same place. The page owns the query and
     narrows its own list (lib/find.js); this is only the control.

     Escape, or the mark at the field's end, clears the query and folds the
     field back into its button; so does leaving an empty field. `status`
     is what the page has to say about the query — "12 of 340" — and sits
     at the field's end, where it does not move the text being typed.
     ===================================================================== */
  let { query = $bindable(""), label = "Find in this playlist", status = "", disabled = false } = $props();

  let open = $state(false);
  let input = $state(null);

  $effect(() => {
    if (open) input?.focus();
  });

  function close() {
    query = "";
    open = false;
  }
</script>

{#if open}
  <span class="field track-find">
    <Icon name="search" size={14} />
    <input
      bind:this={input}
      bind:value={query}
      placeholder={label}
      aria-label={label}
      spellcheck="false"
      onkeydown={(event) => {
        if (event.key !== "Escape") return;
        event.preventDefault();
        event.stopPropagation();
        close();
      }}
      onblur={() => {
        if (!query.trim()) close();
      }}
    />
    {#if status}<span class="track-find-status" role="status">{status}</span>{/if}
    <button type="button" class="field-btn" title="Clear" aria-label="Clear and close" onclick={close}>
      <Icon name="x" size={11} />
    </button>
  </span>
{:else}
  <button class="btn-round lg" title={label} aria-label={label} {disabled} onclick={() => (open = true)}>
    <Icon name="search" size={19} />
  </button>
{/if}

<style>
  /* It grows into what the row has left and never wraps the row to make room. */
  .track-find { flex: 1 1 0; min-width: 150px; max-width: 280px; margin-left: var(--s1); }
  .track-find-status {
    flex: none; color: var(--fg-3); white-space: nowrap;
    font-family: var(--font-number); font-size: var(--t-11); font-weight: var(--w-med);
    font-variant-numeric: tabular-nums;
  }
</style>
