import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * The window frame, per platform, as the shell configures it.
 *
 * Windows has no native frame (tauri.windows.conf.json): the app draws
 * minimize, maximize and close itself, and its empty top areas move the
 * window. macOS keeps its own traffic lights over the top of the content
 * (titleBarStyle "Overlay" in tauri.macos.conf.json), so nothing is drawn
 * there and the rail leaves them room (`:root[data-os="mac"]`, app.css).
 */
const agent = typeof navigator === "undefined" ? "" : navigator.userAgent;
export const os = /Macintosh/.test(agent) ? "mac" : /Windows/.test(agent) ? "windows" : "other";
export const drawsWindowControls = os === "windows";

export const frame = $state({ maximized: false, minimized: false });

let watching = false;

/** Native frame state, independent of WebView document visibility/focus.
    The shell emits it on window changes; no polling while at rest. */
export function watchFrame() {
  if (watching || !drawsWindowControls) return;
  watching = true;
  let revision = 0;
  listen("window-state", (event) => {
    revision++;
    frame.maximized = event.payload?.maximized === true;
    frame.minimized = event.payload?.minimized === true;
  }).then(() => {
    const ticket = revision;
    const window = getCurrentWindow();
    return Promise.all([window.isMaximized(), window.isMinimized()]).then(([maximized, minimized]) => {
      if (revision !== ticket) return;
      frame.maximized = maximized;
      frame.minimized = minimized;
    });
  }).catch(() => {});
}

export const minimizeWindow = () => getCurrentWindow().minimize().catch(() => {});
export const toggleMaximizeWindow = () => getCurrentWindow().toggleMaximize().catch(() => {});
export const closeWindow = () => getCurrentWindow().close().catch(() => {});
