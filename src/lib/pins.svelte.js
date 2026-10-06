import { session } from "./state.svelte.js";

/*
 * The rail's own arrangement, per account, in localStorage: which rows are
 * pinned to the top, and which folders are shut. Both are preferences about
 * how this app lays out a library, not facts about the library, so they are
 * kept here rather than written back to Spotify.
 */
export const pins = $state({ account: null, ids: [], folders: [] });
const LIKED = "liked";

function read(key) {
  try {
    const value = localStorage.getItem(key);
    return value === null ? null : JSON.parse(value);
  } catch {
    return null;
  }
}

function write(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); }
  catch { /* Storage may be disabled; the arrangement stays session-local. */ }
}

export function loadPins(account = session.username) {
  if (pins.account === account) return;
  pins.account = account || null;
  pins.ids = [];
  pins.folders = [];
  if (!account) return;
  const stored = read(`sr.library-pins:${account}`);
  if (stored === null) pins.ids = [LIKED];
  else if (Array.isArray(stored)) {
    pins.ids = [...new Set(stored.filter((id) => typeof id === "string" && (id === LIKED || /^[A-Za-z0-9]{22}$/.test(id))))];
  }
  const folders = read(`sr.library-folders:${account}`);
  if (Array.isArray(folders)) {
    pins.folders = [...new Set(folders.filter((id) => typeof id === "string" && id.length > 0 && id.length <= 256))];
  }
}

export function isPinned(id) {
  return !!pins.account && pins.ids.includes(id);
}

export function togglePin(id) {
  loadPins();
  if (!pins.account || !id) return;
  pins.ids = isPinned(id) ? pins.ids.filter((item) => item !== id) : [...pins.ids, id];
  write(`sr.library-pins:${pins.account}`, pins.ids);
}

export function isCollapsed(id) {
  return !!pins.account && pins.folders.includes(id);
}

export function toggleFolder(id) {
  loadPins();
  if (!pins.account || !id) return;
  pins.folders = isCollapsed(id) ? pins.folders.filter((item) => item !== id) : [...pins.folders, id];
  write(`sr.library-folders:${pins.account}`, pins.folders);
}

/** Opens a folder if it is shut — it is where a new folder is being named —
    which also forgets a deleted folder's state, since open is the default. */
export function openFolder(id) {
  if (isCollapsed(id)) toggleFolder(id);
}
