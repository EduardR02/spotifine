import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { session, sessionEpoch, playback, lookupSavedIn, artistFollowChanged, appSettings } from "./state.svelte.js";

/*
 * The personal Spotify app: the owner's own developer app for the same
 * account, which can WRITE to the library — likes, follows — and route
 * Spotifine playback to a Spotify Connect device. It is never asked what is saved: Liked Songs is
 * answered by the shell's membership index (`get_track_playlists`) and the
 * followed artists by the engine's own list, both local. The one exception is
 * whether you follow a USER, which nothing local knows (`followsUser`).
 */
export const personal = $state({ status: null, loading: false, error: "" });
let request = null;
let watchedSession = null;
let listening = null;

export function refreshPersonal() {
  const key = `${sessionEpoch()}\u0000${session.username ?? ""}\u0000${playback.auth_state ?? ""}`;
  if (request?.key === key) return request.promise;
  const pending = { key, promise: null };
  personal.loading = true;
  pending.promise = invoke("personal_api_status").then((status) => {
    if (request === pending) {
      personal.status = status;
      personal.error = status?.error || "";
    }
    return status;
  }).catch((error) => {
    if (request === pending) personal.error = String(error);
    throw error;
  }).finally(() => {
    if (request === pending) {
      request = null;
      personal.loading = false;
    }
  });
  request = pending;
  return pending.promise;
}

export function watchPersonal() {
  if (!listening) {
    listening = listen("personal-api-changed", ({ payload }) => {
      request = null;
      personal.loading = false;
      personal.status = payload;
      personal.error = payload?.error || "";
    });
  }
  // watchPersonal is called from mounted effects. Reading identity/readiness
  // here refreshes the initially disconnected status when playback logs in.
  const key = `${sessionEpoch()}\u0000${session.username ?? ""}\u0000${playback.auth_state ?? ""}`;
  if (watchedSession !== key) {
    watchedSession = key;
    refreshPersonal().catch(() => {});
  }
}

export function personalConnected() {
  return !!personal.status?.connected && !!session.username &&
    personal.status.account_id === session.username;
}

export async function configurePersonal(clientId) {
  personal.status = await invoke("personal_api_configure", { clientId: clientId.trim() });
  personal.error = "";
}

export async function authorizePersonal(enableDevices = false) {
  const { url } = await invoke("personal_api_authorize", { enableDevices });
  await openUrl(url);
  await refreshPersonal();
}

export async function disconnectPersonal() {
  personal.status = await invoke("personal_api_disconnect");
  personal.error = "";
}

/** Device control is its own grant on the same app, asked for once. */
export function personalDevicesAuthorized() {
  return personalConnected() && !!personal.status?.devices_authorized;
}

/** Devices turned on in Settings. Off is local: the grant stays, so on again
    is instant, and while off nothing device-related runs at all. */
export function devicesEnabled() {
  return appSettings.devices_enabled !== false;
}

/** The player bar offers devices: granted, and turned on. */
export function personalDevicesActive() {
  return personalDevicesAuthorized() && devicesEnabled();
}


/**
 * Like or unlike tracks. The shell applies a landed write to its membership
 * index and says so (`memberships_changed`), which is what every saved mark
 * reads and what drops the cached Liked Songs page; the playing track's mark
 * is re-read at once rather than waiting on the event.
 */
export async function setLiked(uris, liked) {
  await invoke("personal_api_set_saved", { uris, saved: liked });
  if (uris.includes(playback.current_uri)) lookupSavedIn(playback.current_uri);
}

/** Follow or unfollow an artist. The followed list is the answer everywhere
    (the rail, the artist's Follow pill), so a landed write updates it in place. */
export async function setFollowingArtist(artist, following) {
  await invoke("personal_api_set_saved", { uris: [`spotify:artist:${artist.id}`], saved: following });
  artistFollowChanged(artist, following);
}

/** Whether you follow a user: asked once, when their profile opens. */
export async function followsUser(username) {
  const [value] = await invoke("personal_api_contains", { uris: [`spotify:user:${username}`] });
  if (typeof value !== "boolean") throw new Error("Spotify returned no follow state");
  return value;
}

export async function setFollowingUser(username, following) {
  await invoke("personal_api_set_saved", { uris: [`spotify:user:${username}`], saved: following });
}

export const personalApi = {
  devices: () => invoke("personal_api_devices"),
  savedShows: (offset = 0, limit = 50) => invoke("personal_api_saved_shows", { offset, limit }),
};
