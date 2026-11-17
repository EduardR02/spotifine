/* The demo: the real app, mounted in a page with the demo's shell standing
   in for Tauri (demo/tauri.js, demo/bridge.js). It opens on a playlist with
   a song playing. Embedded in the download page it also behaves as part of
   that page: see EMBEDDED below. */
import "./storage.js";
import "../src/styles/app.css";
import { mount } from "svelte";
import App from "../src/App.svelte";
import Notice from "./Notice.svelte";
import { navigate, setNavigationGuard } from "../src/lib/state.svelte.js";
import { haze } from "../src/lib/ambient.svelte.js";
import { OPENING } from "./library.js";
import { say } from "./notice.svelte.js";

const EMBEDDED = window.parent !== window;

/* ---- What the demo covers ------------------------------------------- */

/** The pages the demo has, and a line for each one it has not. */
const COVERED = new Set(["library", "playlist", "liked", "queue"]);
const ARTIST = "Artist pages are in the app. This demo covers playlists.";
const ELSEWHERE = {
  search: "Search needs a Spotify account, so it's not in this demo.",
  "search-songs": "Search needs a Spotify account, so it's not in this demo.",
  album: "Album pages are in the app. This demo covers playlists.",
  artist: ARTIST,
  discography: ARTIST,
  "fans-also-like": ARTIST,
  "appears-on": ARTIST,
  "artist-playlists": ARTIST,
  "discovered-on": ARTIST,
  history: "The app keeps your listening history. This demo has none.",
  settings: "Settings are in the app. This demo has nothing to set up.",
  "track-editor": "Cutting and looping a song needs its audio. That's in the app.",
};

setNavigationGuard((entry) => {
  if (COVERED.has(entry.name)) return true;
  say(ELSEWHERE[entry.name] ?? "That's in the app. This demo covers playlists.");
  return false;
});

/* ---- Embedded ------------------------------------------------------- */

if (EMBEDDED) {
  /* Focus belongs to the page: the demo plays its Canvas and meter while it
     can be seen (demo/window-frame.svelte.js), not only after a click into
     the frame. Registered before the app's own listener, so it is first. */
  document.hasFocus = () => true;
  window.addEventListener("blur", (event) => event.stopImmediatePropagation(), true);

  /* A wheel over the frame scrolls the page until the visitor has clicked
     into the demo, and again once the pointer has left it, so the hero
     never catches the page's scroll. The page does the scrolling. */
  let engaged = false;
  const LINE = 16;
  window.addEventListener("pointerdown", () => (engaged = true), true);
  window.addEventListener("keydown", () => (engaged = true), true);
  document.documentElement.addEventListener("pointerleave", () => (engaged = false));
  window.addEventListener("wheel", (event) => {
    if (engaged || event.ctrlKey) return;
    event.preventDefault();
    const unit = event.deltaMode === 1 ? LINE : event.deltaMode === 2 ? window.innerHeight : 1;
    window.parent.postMessage({ type: "spotifine-demo:wheel", dx: event.deltaX * unit, dy: event.deltaY * unit }, "*");
  }, { capture: true, passive: false });
}

/* ---- Mount ---------------------------------------------------------- */

mount(App, { target: document.getElementById("app") });
mount(Notice, { target: document.body });
navigate("playlist", OPENING.summary.id);

/* Ready once the window is lit: the haze has drawn its first picture and
   faded it in (Ambient.svelte's first fade, 1.6s). Where there is no haze
   (no worker, transparency reduced) the demo is ready as soon as it is up. */
const FIRST_FADE_MS = 1700;
const NO_HAZE_MS = 5000;
const started = performance.now();
(function ready() {
  const lit = Boolean(haze.tone);
  if (!lit && performance.now() - started < NO_HAZE_MS) return setTimeout(ready, 100);
  setTimeout(() => {
    document.documentElement.dataset.demoReady = "true";
    if (EMBEDDED) window.parent.postMessage({ type: "spotifine-demo:ready" }, "*");
  }, lit ? FIRST_FADE_MS : 0);
})();
