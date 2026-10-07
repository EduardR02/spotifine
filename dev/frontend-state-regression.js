import { tick } from "svelte";
import { api, applyPlayback, playback, playCatalogueContext, ui, setNowPlayingOpen } from "../src/lib/state.svelte.js";

const settle = async () => {
  for (let i = 0; i < 8; i++) { await Promise.resolve(); await tick(); }
};
const check = (condition, message) => { if (!condition) throw new Error(message); };

/** Exercise the real compiled App effect with the synthetic bridge only. */
export async function frontendStateRegressions() {
  if (!document.body.dataset.harnessReady || new URLSearchParams(location.search).has("real")) {
    throw new Error("These regressions require the synthetic UI harness.");
  }
  const oldInvoke = window.__TAURI_INTERNALS__.invoke;
  const oldPlayback = { ...playback };
  const oldOpen = ui.nowPlayingOpen;
  const artist = `deferred-catalogue-${crypto.randomUUID()}`;
  const track = (id) => ({ id, uri: `spotify:track:${id}`, name: id, duration_ms: 180_000 });
  const first = track("catalogue-first-tail");
  const next = track("catalogue-next-page");
  const calls = [];
  let resolvePlay;
  let pending;
  try {
    setNowPlayingOpen(false);
    await settle();
    window.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command === "play_queue") {
        // The state event publishes before the command's reply. App's effect
        // runs now, before startLazyQueue has installed its pagination source.
        applyPlayback({ queue: args.queue, queue_revision: 0, current_index: args.index,
          current_uri: first.uri, context: args.context, playing: false });
        return new Promise((resolve) => { resolvePlay = resolve; });
      }
      if (command === "browse_artist_catalogue" && args.id === artist) {
        calls.push(args.offset);
        return Promise.resolve({ releases: [{ id: "catalogue-release-next", tracks: [next] }], next_offset: null });
      }
      if (command === "add_queue_batch") {
        applyPlayback({ queue: [...playback.queue, ...args.tracks], queue_revision: 0 });
        return Promise.resolve();
      }
      return oldInvoke(command, args);
    };
    pending = playCatalogueContext([{ id: "catalogue-release-first", tracks: [first] }], artist, ["albums"], 4, 0);
    await settle();
    check(playback.current_index === 0 && playback.queue.length === 1, "first-page tail state reaches the compiled App");
    check(calls.length === 0, "no source exists before play_queue has succeeded");
    resolvePlay();
    await pending;
    await settle();
    check(calls.length === 1 && calls[0] === 4, "installing the source triggers exactly one low-water page without another index change");
    check(playback.current_index === 0 && playback.queue[1]?.uri === next.uri, "next catalogue page appends while the original tail remains current");
    return ["Catalogue low-water backfill after state-before-command-response ordering"];
  } finally {
    if (resolvePlay) resolvePlay();
    if (pending) await pending.catch(() => {});
    window.__TAURI_INTERNALS__.invoke = oldInvoke;
    // A regular queue command cancels any pagination source left by a failure.
    await api.playQueue(oldPlayback.queue, Math.max(0, oldPlayback.current_index), oldPlayback.context).catch(() => {});
    applyPlayback({ ...oldPlayback, queue_revision: 0, order_revision: 0, playing: false });
    setNowPlayingOpen(oldOpen);
    await settle();
  }
}
