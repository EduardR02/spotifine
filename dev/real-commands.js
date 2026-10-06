/**
 * The only Tauri commands the harness may ever forward to the owner's running
 * app. Shared by the bridge (dev/real-bridge.js), which enforces it, and by the
 * harness (dev/ui-harness-real.js), which uses it to decide what to ask for.
 *
 * Each entry was checked against src-tauri/src/commands.rs and the engine: none
 * of them touches playback, the queue, the Spotify account, or a setting. The
 * worst any of them does is refresh a local cache, noted per entry.
 *
 *   status                 makes the engine re-broadcast state to the real window.
 *   cancel_track_waveform  cancels engine work.
 * Personal grant configuration/authorization/library writes/output selection
 * are never forwarded. Real mode directs those actions to the native app.
 * Simulated local playback stays on the harness mock.
 */
export const READ_COMMANDS = new Set([
  // In-memory snapshot of playback, library and me_id. No I/O.
  "get_state",
  // Native application metadata; no I/O or mutations.
  "plugin:app|version",
  // In-memory membership lookup. No I/O.
  "get_track_playlists",
  // Engine: one window of the listening archive. No writes.
  "get_history",
  // Engine: the saved edit for a track. No writes.
  "get_track_edit",
  // Engine: fetches the audio and computes peaks; caches the result under
  // the engine's waveforms/ directory. Heavy, so it is never warmed.
  "get_track_waveform",
  // Engine: pathfinder searchDesktop. Does not touch recent searches.
  "search",
  // Engine fetch; the shell refreshes its bounded tracks cache and membership
  // index and emits display-only `playlist`/`playlist_summary` events.
  "browse_playlist",
  // Engine browse reads with no shell side effects. Canvas answers are
  // memoised in the engine's memory.
  "browse_radio",
  "browse_playlist_recommendations",
  "browse_track",
  "browse_album",
  "browse_artist",
  "browse_artist_songwriter",
  "browse_artist_catalogue",
  "browse_liked_songs",
  "browse_track_credits",
  "browse_canvas",
  "browse_followed_artists",
  // Engine: the collection service's saved podcasts, read-only.
  "browse_saved_shows",
  "browse_show",
  "browse_episode",
  "browse_profile",
  // Native-bound personal reads only, never prewarmed. Contains answers only
  // whether a profile's user is followed, once when the profile opens; the
  // UI asks for devices only when the device menu is opened.
  "personal_api_status",
  "personal_api_contains",
  "personal_api_devices",
  // Walks the cache directories, memoised for a minute.
  "get_cache_stats",
]);
