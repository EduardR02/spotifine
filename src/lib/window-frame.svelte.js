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

export const frame = $state({ maximized: false });

let watching = false;

/** Maximized or not, from the shell's change events (lib.rs,
    report_maximized): nothing runs while the window is left alone. */
export function watchFrame() {
  if (watching || !drawsWindowControls) return;
  watching = true;
  listen("window-maximized", (event) => (frame.maximized = event.payload === true)).catch(() => {});
  getCurrentWindow().isMaximized().then((value) => (frame.maximized = value)).catch(() => {});
}

export const minimizeWindow = () => getCurrentWindow().minimize().catch(() => {});
export const toggleMaximizeWindow = () => getCurrentWindow().toggleMaximize().catch(() => {});
export const closeWindow = () => getCurrentWindow().close().catch(() => {});
