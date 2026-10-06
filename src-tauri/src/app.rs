//! Application state and on-disk caches.
//!
//! Playlist and cache files live under the platform's app-owned data directory.
//! Covers are raw image bytes keyed by `sha1(url)`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use renderer_engine::protocol::normalize_canonical_playlist_description;

use crate::types::{
    align_artist_ids, cover_urls_from_tracks, CacheStats, CacheUsage,
    LibraryNodeDetail, PlaybackState, Playlist, PlaylistDetail, Track,
};

/// Number of playlists requested from the engine's rootlist browse.
pub const LIBRARY_LENGTH: usize = 1000;

/// Most-recent-first cap for the playlist tracks cache.
const TRACKS_CACHE_MAX: usize = 25;

/// Default audio cache cap. Kept in MB because that is the user-facing unit
/// and the engine command line accepts the same value without rounding.
pub const DEFAULT_AUDIO_CACHE_LIMIT_MB: u64 = 1024;

/// Persistent preferences that affect process startup rather than live
/// playback state. Zero means an unlimited audio cache.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct AppSettings {
    pub audio_cache_limit_mb: u64,
    pub launch_at_login: bool,
    pub start_minimized: bool,
    pub animated_canvas: bool,
    /// Track-gain volume normalisation in the engine. Also handed to the live
    /// engine over the protocol on change, so only the *initial* value comes
    /// from here.
    pub normalisation: bool,
    /// User-owned Spotify developer app ID, never an app secret.
    pub personal_client_id: String,
    /// Spotify Connect devices from the player bar. Off is local only: the
    /// grant is kept, so on again needs no new authorisation; while off no
    /// device work runs at all.
    pub devices_enabled: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            audio_cache_limit_mb: DEFAULT_AUDIO_CACHE_LIMIT_MB,
            launch_at_login: false,
            start_minimized: false,
            animated_canvas: true,
            normalisation: false,
            personal_client_id: String::new(),
            devices_enabled: true,
        }
    }
}

/// Version 4 remembers the speed apart for songs and podcast episodes.
pub const PLAYBACK_STATE_VERSION: u32 = 4;

/// Version of the playhead sidecar. Versioned apart from the snapshot it
/// refines: the two files are written on different cadences and either can be
/// replaced without the other. Version 2 added `queue_identity`; a version-1
/// sidecar carries no token and is therefore never applied (the snapshot's own
/// playhead is used and the next drift rewrites the sidecar).
pub const PLAYBACK_PLAYHEAD_VERSION: u32 = 2;

/// App-owned durable playback state. Deliberately excludes `playing`: every
/// normal process start restores paused, while crash-only resume intent stays
/// in memory in `EngineClient`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PlaybackSnapshot {
    pub version: u32,
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    /// Position in the compiled transport timeline. Queue durations and edit
    /// ranges remain in original-source coordinates.
    pub position_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: String,
    pub track_speed: f32,
    pub episode_speed: f32,
}

impl PlaybackSnapshot {
    pub fn from_playback(state: &PlaybackState) -> Self {
        let mut queue = state.queue.to_vec();
        // Download marks are session-live truth, derived from the audio cache
        // by the heartbeat and by browses. A persisted snapshot must not carry
        // them: the cache prunes on its own schedule, so a stored mark is a
        // claim about the past — exactly the ghost-icon bug this avoids.
        for track in &mut queue {
            track.cached = false;
        }
        Self {
            version: PLAYBACK_STATE_VERSION,
            queue,
            current_index: state.current_index,
            position_ms: state.position_ms,
            volume: state.volume,
            shuffle: state.shuffle,
            repeat: state.repeat.clone(),
            track_speed: state.track_speed,
            episode_speed: state.episode_speed,
        }
    }

    fn is_valid(&self) -> bool {
        self.version == PLAYBACK_STATE_VERSION
            && self.volume <= 100
            && matches!(self.repeat.as_str(), "off" | "context" | "track")
            && [self.track_speed, self.episode_speed]
                .iter()
                .all(|speed| speed.is_finite() && (0.5..=4.0).contains(speed))
            && self
                .current_index
                .is_none_or(|index| index < self.queue.len())
            && (!self.queue.is_empty() || self.current_index.is_none())
            && self.queue.iter().all(|track| {
                (track.uri.starts_with("spotify:track:")
                    || track.uri.starts_with("spotify:episode:"))
                    && (track.duration_ms > 0 || track.unavailable)
            })
            && match self.current_index {
                Some(index) => self.position_ms <= self.queue[index].duration_ms,
                None => self.position_ms == 0,
            }
    }

    /// Identity of the queue this snapshot carries: the token a playhead
    /// sidecar is stamped with to say which snapshot it belongs to.
    ///
    /// FNV-1a over the row count and every row's uri, in order, so a queue
    /// edit (add, remove, reorder) changes it and two snapshots that agree on
    /// it describe the same rows. Written to a file that outlives the process,
    /// which is why the algorithm is spelled out here rather than taken from
    /// `DefaultHasher`: the next launch has to recompute the same number.
    ///
    /// Only the uris are folded in: they are what identifies a row, they
    /// round-trip through JSON verbatim, and the sidecar is never a restore
    /// source on its own — it can only move the playhead of a queue that is
    /// already known to be this one.
    pub fn queue_identity(&self) -> u64 {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        fn fold(mut hash: u64, bytes: &[u8]) -> u64 {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(PRIME);
            }
            hash
        }
        // The length is folded in first so that no two different row counts
        // can collide on a concatenation of uris, and each uri is followed by
        // a byte a uri cannot contain (the engine only ever sends
        // `spotify:track:...`), so `["ab", "c"]` and `["a", "bc"]` differ.
        let mut hash = fold(OFFSET, &(self.queue.len() as u64).to_le_bytes());
        for track in &self.queue {
            hash = fold(hash, track.uri.as_bytes());
            hash = fold(hash, &[0xff]);
        }
        hash
    }
}

/// The playhead half of the durable state: which queue row is playing and how
/// far into it.
///
/// Written on its own far more often than [`PlaybackSnapshot`], because a
/// fifteen-second position change must not rewrite and fsync the whole queue
/// to record one integer. It is only ever read as an overlay on the snapshot
/// it trails — a playhead with no queue says nothing about what to restore.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct PlayheadSnapshot {
    pub version: u32,
    /// [`PlaybackSnapshot::queue_identity`] of the snapshot this playhead was
    /// written for.
    ///
    /// File timestamps alone cannot say which snapshot a sidecar belongs to:
    /// two writes inside one filesystem tick tie, and a clock that steps back
    /// makes the newer file look older. A sidecar whose token disagrees with
    /// the snapshot beside it describes another queue — restoring it would
    /// silently seek the wrong row — so the snapshot's own playhead wins.
    pub queue_identity: u64,
    pub current_index: Option<usize>,
    pub position_ms: u32,
}

/// Managed application state. Only the contract fields are serialized; the
/// cache bookkeeping and the data dir are internal.
#[derive(Debug, Serialize)]
pub struct AppState {
    pub playback: PlaybackState,
    pub playlists: Vec<Playlist>,
    pub playlist_tree: Vec<LibraryNodeDetail>,
    pub me_id: String,
    #[serde(skip)]
    pub playlists_fetched_at: Option<i64>,
    /// This process received an authenticated rootlist, not just disk cache.
    /// The initial get_state pull can race the `library` event.
    #[serde(skip)]
    pub library_fresh: bool,
    /// Most-recently-opened first; limited to [`TRACKS_CACHE_MAX`] entries.
    #[serde(skip)]
    pub tracks_cache: Vec<PlaylistTracksEntry>,
    #[serde(skip)]
    pub data_dir: PathBuf,
    /// Playlist ids with a background detail refresh currently in flight.
    /// This is process-local bookkeeping and is deliberately not serialized.
    #[serde(skip)]
    playlist_refreshing: HashSet<String>,
    /// Set when an edit lands while another fetch for the same playlist is
    /// out; the running refresh re-runs once more instead of the edit being
    /// dropped (it would otherwise never be seen until the next open). A
    /// re-open while a fetch is out is not queued: that fetch answers it.
    #[serde(skip)]
    playlist_refresh_queued: HashSet<String>,
    /// Serializes playlist state mutation with its rebuildable cache write.
    /// Async fetches may finish out of order; holding this outside the
    /// AppState lock lets disk I/O proceed without blocking state readers while
    /// still making memory and disk observe one completion order.
    #[serde(skip)]
    pub(crate) playlist_persistence: Arc<Mutex<()>>,
    /// True while a background library refresh (with retries) is running.
    #[serde(skip)]
    pub library_fetching: bool,
    /// Set when a trigger arrives mid-chain; the running chain re-runs once
    /// more instead of the trigger being dropped.
    #[serde(skip)]
    pub library_refresh_queued: bool,

    /// Monotonic process-local fence for in-flight library, detail, and
    /// membership requests. Successful deletes advance it; disk caches do not
    /// carry this ephemeral generation across launches.
    #[serde(skip)]
    pub library_generation: u64,
    /// Fences full Liked Songs walks against confirmed personal-API writes.
    #[serde(skip)]
    pub liked_generation: u64,
    /// Last computed cache sizes and the unix second they were computed at,
    /// so reopening Settings does not re-walk thousands of files. See
    /// [`CACHE_STATS_TTL_SECS`].
    #[serde(skip)]
    pub cache_stats: Option<(i64, CacheStats)>,
    /// Reverse lookup index answering "which of my containers hold this
    /// track": owned playlists plus the Liked Songs collection. Backs the
    /// player bar's saved mark without any network round trip at play time.
    #[serde(skip)]
    pub memberships: Vec<MembershipEntry>,
    /// True while a background membership reconciliation chain is running.
    #[serde(skip)]
    pub membership_fetching: bool,
    /// Set when a trigger arrives mid-chain, mirroring
    /// [`library_refresh_queued`](AppState::library_refresh_queued).
    #[serde(skip)]
    pub membership_refresh_queued: bool,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            playback: PlaybackState::default(),
            playlists: Vec::new(),
            playlist_tree: Vec::new(),
            me_id: String::new(),
            playlists_fetched_at: None,
            library_fresh: false,
            tracks_cache: Vec::new(),
            data_dir,
            playlist_refreshing: HashSet::new(),
            playlist_persistence: Arc::new(Mutex::new(())),
            playlist_refresh_queued: HashSet::new(),
            library_fetching: false,
            library_refresh_queued: false,
            library_generation: 0,
            liked_generation: 0,
            cache_stats: None,
            memberships: Vec::new(),
            membership_fetching: false,
            membership_refresh_queued: false,
        }
    }

    pub(crate) fn record_liked_write(&mut self, uris: &[String], saved: bool) -> bool {
        if !uris.iter().any(|uri| uri.starts_with("spotify:track:")) {
            return false;
        }
        self.liked_generation = self.liked_generation.wrapping_add(1);
        apply_liked_write(&mut self.memberships, uris, saved)
    }

    /// Returns no result for a read overtaken by a landed like/unlike, and
    /// coalesces a fresh walk into the running reconciliation chain.
    pub(crate) fn commit_liked_read(&mut self, generation: u64, uris: HashSet<String>) -> Option<bool> {
        if self.liked_generation != generation {
            self.membership_refresh_queued = true;
            return None;
        }
        Some(upsert_membership(&mut self.memberships, MembershipEntry {
            id: LIKED_MEMBERSHIP_ID.to_owned(),
            revision: String::new(),
            uris,
        }))
    }

    pub(crate) fn start_playlist_refresh(&mut self, id: &str, cause: RefreshCause) -> bool {
        if self.playlist_refreshing.insert(id.to_owned()) {
            return true;
        }
        // Another fetch is out for this playlist. Only an edit behind it
        // needs one more pass: it would otherwise be answered by a read that
        // predates it, and never seen until the next open. A re-open carries
        // no such news — the fetch in flight already answers it, and queuing
        // it would spend a second round trip only to drop the first payload.
        if cause == RefreshCause::Edit {
            self.playlist_refresh_queued.insert(id.to_owned());
        }
        false
    }

    pub(crate) fn finish_playlist_refresh(&mut self, id: &str) {
        self.playlist_refreshing.remove(id);
    }

    /// Takes and clears a mid-flight refresh trigger for `id`.
    pub(crate) fn take_playlist_refresh_queued(&mut self, id: &str) -> bool {
        self.playlist_refresh_queued.remove(id)
    }
}

/// Why a playlist refresh was triggered. An edit committed while a fetch is
/// out must not be spoken over by that older read, so it queues one more
/// pass; a plain re-open of the same playlist is already answered by the
/// fetch in flight, which is about to emit exactly the payload the second
/// pass would have fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshCause {
    Reopen,
    Edit,
}

/// One cached playlist tracks payload, matching the on-disk entry shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaylistTracksEntry {
    pub id: String,
    pub fetched_at: Option<i64>,
    /// Playlist4 revision hex; the Web API snapshot id.
    pub revision: String,
    /// Stored without their session-live fields; see [`StoredTrack`].
    #[serde(serialize_with = "store_tracks")]
    pub tracks: Vec<Track>,
    /// Playlist-local tracks skipped by automatic playback.
    ///
    /// Older cache files do not carry this field; missing data means no
    /// exclusions, while fresh engine browses replace it authoritatively.
    #[serde(default)]
    pub excluded_track_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Data directory
// ---------------------------------------------------------------------------

/// `%LOCALAPPDATA%\Spotifine` on Windows; macOS uses the user's Application
/// Support directory so the library and playback state survive restarts
/// instead of landing in the temporary-directory fallback.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from(std::env::var_os("HOME").expect("macOS home directory unavailable"))
            .join("Library/Application Support/Spotifine");
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("Spotifine");
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(profile)
                .join("AppData")
                .join("Local")
                .join("Spotifine");
        }
        std::env::temp_dir().join("Spotifine")
    }
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

static SETTINGS: LazyLock<Mutex<AppSettings>> = LazyLock::new(|| {
    let mut settings: AppSettings = std::fs::read(settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if !matches!(settings.audio_cache_limit_mb, 0 | 1024 | 2048 | 4096 | 8192) {
        settings.audio_cache_limit_mb = DEFAULT_AUDIO_CACHE_LIMIT_MB;
    }
    Mutex::new(settings)
});

pub fn load_app_settings() -> AppSettings {
    SETTINGS.lock().clone()
}

pub fn update_app_settings(change: impl FnOnce(&mut AppSettings)) -> Result<AppSettings, String> {
    let mut guard = SETTINGS.lock();
    let mut next = guard.clone();
    change(&mut next);
    if next == *guard { return Ok(next); }
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create settings directory: {error}"))?;
    }
    write_json_atomic_result(path, &next)?;
    *guard = next.clone();
    Ok(next)
}

fn playback_state_path(dir: &Path) -> PathBuf {
    dir.join("playback_state.json")
}

fn playback_playhead_path(dir: &Path) -> PathBuf {
    dir.join("playback_playhead.json")
}

/// Loads the durable snapshot with the fresher of the two playheads applied.
///
/// The sidecar carries no queue, so it is never a restore source on its own:
/// it only moves the playhead of the snapshot it belongs to, and only while it
/// is at least as new as that snapshot and names the same queue. A queue edit
/// (or a clean exit) rewrites the snapshot with the live position, and that
/// snapshot then outranks a sidecar the previous playhead left behind.
fn load_playback_snapshot_from(dir: &Path) -> Option<PlaybackSnapshot> {
    let mut snapshot: PlaybackSnapshot =
        serde_json::from_slice(&std::fs::read(playback_state_path(dir)).ok()?).ok()?;
    if !snapshot.is_valid() {
        return None;
    }
    if let Some(playhead) = load_playhead(dir, &snapshot) {
        snapshot.current_index = playhead.current_index;
        snapshot.position_ms = playhead.position_ms;
    }
    Some(snapshot)
}

/// Reads the sidecar when it names `snapshot`'s queue, is at least as new as
/// it, and its playhead resolves inside those rows.
fn load_playhead(dir: &Path, snapshot: &PlaybackSnapshot) -> Option<PlayheadSnapshot> {
    let path = playback_playhead_path(dir);
    let written = std::fs::metadata(&path).ok()?.modified().ok()?;
    let snapshot_written = std::fs::metadata(playback_state_path(dir))
        .ok()?
        .modified()
        .ok()?;
    if written < snapshot_written {
        return None;
    }
    let playhead: PlayheadSnapshot = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    (playhead.version == PLAYBACK_PLAYHEAD_VERSION
        // The token is what makes the sidecar belong to *this* snapshot: the
        // bounds checks below cannot tell one queue from another of the same
        // shape, and the timestamps above cannot tell two writes apart inside
        // one filesystem tick or across a clock that stepped back.
        && playhead.queue_identity == snapshot.queue_identity()
        && playhead_resolves(&playhead, snapshot))
    .then_some(playhead)
}

/// Whether a sidecar's playhead can be applied to a snapshot's queue. An
/// index that does not exist there means the sidecar belongs to another
/// queue, and a stray position inside a row that cannot hold it means the
/// same; both fall back to the snapshot's own playhead.
fn playhead_resolves(playhead: &PlayheadSnapshot, snapshot: &PlaybackSnapshot) -> bool {
    match playhead.current_index {
        Some(index) => snapshot
            .queue
            .get(index)
            .is_some_and(|track| playhead.position_ms <= track.duration_ms),
        None => playhead.position_ms == 0,
    }
}

pub fn load_playback_snapshot() -> Option<PlaybackSnapshot> {
    load_playback_snapshot_from(&data_dir())
}

fn save_playback_snapshot_to(dir: &Path, snapshot: &PlaybackSnapshot) -> Result<(), String> {
    if !snapshot.is_valid() {
        return Err("refusing to persist an invalid playback snapshot".to_owned());
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create playback state directory: {error}"))?;
    write_json_atomic_result(playback_state_path(dir), snapshot)
}

pub fn save_playback_snapshot(snapshot: &PlaybackSnapshot) -> Result<(), String> {
    save_playback_snapshot_to(&data_dir(), snapshot)
}

fn save_playhead_snapshot_to(
    dir: &Path,
    current_index: Option<usize>,
    position_ms: u32,
    queue_identity: u64,
) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create playback state directory: {error}"))?;
    write_json_atomic_result(
        playback_playhead_path(dir),
        &PlayheadSnapshot {
            version: PLAYBACK_PLAYHEAD_VERSION,
            queue_identity,
            current_index,
            position_ms,
        },
    )
}

/// Persists the playhead alone. The writer only reaches this when the queue,
/// the volume and every other persisted field already match the snapshot on
/// disk, so the two files together still describe one coherent state — and
/// `queue_identity` is that snapshot's own token, taken from the copy the
/// comparison ran against rather than recomputed here.
pub fn save_playhead_snapshot(
    current_index: Option<usize>,
    position_ms: u32,
    queue_identity: u64,
) -> Result<(), String> {
    save_playhead_snapshot_to(&data_dir(), current_index, position_ms, queue_identity)
}

pub fn clear_playback_snapshot() -> Result<(), String> {
    // The sidecar is cleared with the snapshot: a playhead left behind would
    // otherwise outlive the queue it belongs to and refine the next one.
    let dir = data_dir();
    for path in [playback_state_path(&dir), playback_playhead_path(&dir)] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not clear playback state: {error}")),
        }
    }
    Ok(())
}

/// Diagnostic logs under the app data directory: `spotifine.log` and the
/// engine's `playback_engine.log`.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Engine `--state-dir`: the app data directory's `engine` child, overridable
/// via `SPOTIFY_STATE_DIR`.
pub fn engine_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SPOTIFY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    data_dir().join("engine")
}

// ---------------------------------------------------------------------------
// playlist_list.json
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaylistListCache {
    pub version: u32,
    pub fetched_at: Option<i64>,
    pub me_id: String,
    pub playlists: Vec<Playlist>,
    #[serde(default)]
    pub playlist_tree: Vec<LibraryNodeDetail>,
}

pub fn load_playlist_list(dir: &Path) -> Option<PlaylistListCache> {
    let bytes = std::fs::read(dir.join("playlist_list.json")).ok()?;
    let mut cache: PlaylistListCache = serde_json::from_slice(&bytes).ok()?;
    for playlist in &mut cache.playlists {
        playlist.description = normalize_canonical_playlist_description(&playlist.description);
    }
    Some(cache)
}

pub fn save_playlist_list(dir: &Path, cache: &PlaylistListCache) {
    write_json_cache(dir.join("playlist_list.json"), cache);
}

// ---------------------------------------------------------------------------
// playlist_tracks_cache.json
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaylistTracksCache {
    pub version: u32,
    pub saved_at: Option<i64>,
    /// Most-recently-opened first.
    pub playlists: Vec<PlaylistTracksEntry>,
}

#[derive(Serialize)]
struct PlaylistTracksCacheRef<'a> {
    version: u32,
    saved_at: Option<i64>,
    playlists: &'a [PlaylistTracksEntry],
}

/// A track as `playlist_tracks_cache.json` stores it: its durable fields only.
/// The download mark is about the audio cache, which this file knows nothing
/// about and which may be pruned or cleared at any time; queue contexts and
/// edit snapshots belong to one queue. Read back, they take their defaults.
#[derive(PartialEq, Serialize)]
struct StoredTrack<'a> {
    id: &'a str,
    uri: &'a str,
    name: &'a str,
    artist_names: &'a [String],
    artist_ids: &'a [String],
    artist_id: &'a str,
    album_id: &'a str,
    album_name: &'a str,
    cover_url: &'a str,
    duration_ms: u32,
    play_count: Option<u64>,
    added_at: Option<i64>,
    unavailable: bool,
    unavailable_reason: Option<&'a str>,
}

impl<'a> From<&'a Track> for StoredTrack<'a> {
    fn from(track: &'a Track) -> Self {
        // Exhaustive, so that a new field has to choose a side.
        let Track { id, uri, name, artist_names, artist_ids, artist_id, album_id, album_name, cover_url, duration_ms,
            play_count, added_at, unavailable, unavailable_reason, cached: _, context: _, effective_edit: _ } = track;
        Self { id, uri, name, artist_names, artist_ids, artist_id, album_id, album_name, cover_url, duration_ms: *duration_ms,
            play_count: *play_count, added_at: *added_at, unavailable: *unavailable, unavailable_reason: unavailable_reason.as_deref() }
    }
}

fn store_tracks<S: serde::Serializer>(tracks: &[Track], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(tracks.iter().map(StoredTrack::from))
}

pub fn load_tracks_cache(dir: &Path) -> Vec<PlaylistTracksEntry> {
    let bytes = match std::fs::read(dir.join("playlist_tracks_cache.json")) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<PlaylistTracksCache>(&bytes) {
        Ok(mut cache) => {
            // Entries written before `artist_ids` existed carry names but no
            // ids; align them on the way in so the two lists are always the
            // same length, whatever wrote the file.
            for entry in &mut cache.playlists {
                for track in &mut entry.tracks {
                    align_artist_ids(track);
                }
            }
            cache.playlists
        }
        Err(_) => Vec::new(),
    }
}

fn tracks_cache_ref(playlists: &[PlaylistTracksEntry]) -> PlaylistTracksCacheRef<'_> {
    PlaylistTracksCacheRef {
        version: 1,
        saved_at: Some(now_secs()),
        playlists,
    }
}

pub fn save_tracks_cache(dir: &Path, playlists: &[PlaylistTracksEntry]) {
    write_json_cache(dir.join("playlist_tracks_cache.json"),
    &tracks_cache_ref(playlists),);
}

/// The cache serialized to the exact bytes [`write_tracks_cache_bytes`]
/// writes. Split from the write because the caller holds the state lock while
/// the cache is borrowed — serializing into a buffer costs a fraction of the
/// clone that handing the rows to another thread would — and because this
/// cache is 1.5 MB of the app's disk churn. Failures are reported and skipped
/// exactly as [`save_tracks_cache`] treats them: best-effort cache IO.
pub fn tracks_cache_bytes(playlists: &[PlaylistTracksEntry]) -> Option<Vec<u8>> {
    match serde_json::to_vec(&tracks_cache_ref(playlists)) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            eprintln!("Spotifine: could not serialize the playlist tracks cache: {error}");
            None
        }
    }
}

/// Writes bytes from [`tracks_cache_bytes`], with the same failure reporting
/// as [`save_tracks_cache`]. A partial cache is discarded on its next read.
pub fn write_tracks_cache_bytes(dir: &Path, bytes: &[u8]) {
    let path = dir.join("playlist_tracks_cache.json");
    if let Err(error) = std::fs::write(&path, bytes) {
        eprintln!(
            "Spotifine: could not write cache {}: {error}",
            path.display()
        );
    }
}

/// Inserts (or refreshes) one playlist's tracks at the front of the cache,
/// dropping the oldest entries beyond the cap, and reports whether the stored
/// entry actually changed.
///
/// The comparison keeps an open from rewriting the whole rebuildable cache
/// to store what it already holds. `fetched_at` is deliberately
/// not part of it: nothing reads that field back, and counting it would make
/// every browse a change. Nor are the fields the file never stores, such as
/// download marks. Moving an unchanged entry to the front is not worth a
/// write either; the next real change persists the list in this order.
pub fn upsert_tracks_cache(
    entries: &mut Vec<PlaylistTracksEntry>,
    entry: PlaylistTracksEntry,
) -> bool {
    let changed = entries
        .iter()
        .find(|existing| existing.id == entry.id)
        .is_none_or(|existing| {
            existing.revision != entry.revision
                || !existing.tracks.iter().map(StoredTrack::from).eq(entry.tracks.iter().map(StoredTrack::from))
                || existing.excluded_track_ids != entry.excluded_track_ids
        });
    entries.retain(|existing| existing.id != entry.id);
    entries.insert(0, entry);
    entries.truncate(TRACKS_CACHE_MAX);
    changed
}

// ---------------------------------------------------------------------------
// playlist_membership.json
// ---------------------------------------------------------------------------

/// Membership sentinel for the user's Liked Songs collection: not a rootlist
/// playlist, but it must light the same saved mark the playlists do.
pub const LIKED_MEMBERSHIP_ID: &str = "liked";

/// One indexed container's track URIs. Playlists also carry their Playlist4
/// revision so reconciliation can skip an unchanged fetch; Liked Songs has no
/// revision and permanently carries an empty one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MembershipEntry {
    pub id: String,
    pub revision: String,
    /// Track URIs (`spotify:track:<id>`), deduplicated.
    pub uris: HashSet<String>,
}

impl MembershipEntry {
    pub fn contains(&self, uri: &str) -> bool {
        self.uris.contains(uri)
    }

}

/// On-disk shape of `playlist_membership.json`. The URI sets serialize as
/// plain arrays, so the file stays diffable and hand-editable.
#[derive(Serialize, Deserialize)]
struct MembershipCache {
    version: u32,
    saved_at: Option<i64>,
    entries: Vec<MembershipEntry>,
}

pub fn load_membership(dir: &Path) -> Vec<MembershipEntry> {
    let bytes = match std::fs::read(dir.join("playlist_membership.json")) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    serde_json::from_slice::<MembershipCache>(&bytes)
        .map(|cache| cache.entries)
        .unwrap_or_default()
}

pub fn save_membership(dir: &Path, entries: &[MembershipEntry]) {
    let cache = MembershipCache {
        version: 1,
        saved_at: Some(now_secs()),
        entries: entries.to_vec(),
    };
    write_json_cache(dir.join("playlist_membership.json"), &cache);
}

/// Inserts or replaces one container's membership. Returns whether anything
/// changed, so callers can skip pointless persistence and UI events when a
/// browse confirmed what the index already knew.
pub fn upsert_membership(entries: &mut Vec<MembershipEntry>, entry: MembershipEntry) -> bool {
    match entries.iter_mut().find(|existing| existing.id == entry.id) {
        Some(existing) => {
            let changed = existing.revision != entry.revision || existing.uris != entry.uris;
            if changed {
                *existing = entry;
            }
            changed
        }
        None => {
            entries.push(entry);
            true
        }
    }
}

/// A confirmed Liked Songs write, applied to the index at once. The index is
/// what answers "is this in Liked Songs" everywhere — the player bar's mark,
/// the track menu — so a like made here must not wait for the next reconcile
/// pass to show. Only track URIs belong to the collection. An index that has
/// never walked Liked Songs gains the entry on a like (absent already means
/// "not liked", so an unlike there changes nothing). Returns whether anything
/// changed.
pub fn apply_liked_write(entries: &mut Vec<MembershipEntry>, uris: &[String], saved: bool) -> bool {
    let tracks = uris.iter().filter(|uri| uri.starts_with("spotify:track:"));
    let liked = match entries.iter_mut().position(|entry| entry.id == LIKED_MEMBERSHIP_ID) {
        Some(index) => &mut entries[index],
        None if saved => {
            entries.push(MembershipEntry {
                id: LIKED_MEMBERSHIP_ID.to_owned(),
                revision: String::new(),
                uris: HashSet::new(),
            });
            entries.last_mut().expect("just pushed")
        }
        None => return false,
    };
    let mut changed = false;
    for uri in tracks {
        changed |= if saved { liked.uris.insert(uri.clone()) } else { liked.uris.remove(uri) };
    }
    changed
}

pub fn remove_membership(entries: &mut Vec<MembershipEntry>, id: &str) -> bool {
    let before = entries.len();
    entries.retain(|existing| existing.id != id);
    entries.len() != before
}

/// Whether a library playlist counts for the saved mark. Deliberately strict:
/// only containers the user created themselves qualify — followed playlists
/// are someone else's curation, and editorial (Made For You, The DJ) rows are
/// Spotify's, so none of them belong in either the index or the hover list.
pub fn playlist_qualifies(playlist: &Playlist, me_id: &str) -> bool {
    !me_id.is_empty() && playlist.owner_id == me_id
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Combines cached tracks with the library playlist metadata into the full
/// detail payload the UI renders.
pub fn playlist_detail_from_cache(state: &AppState, entry: PlaylistTracksEntry) -> PlaylistDetail {
    let meta = state
        .playlists
        .iter()
        .find(|playlist| playlist.id == entry.id);
    let mut playlist = meta.cloned().unwrap_or_else(|| Playlist {
        id: entry.id.clone(),
        uri: format!("spotify:playlist:{}", entry.id),
        ..Playlist::default()
    });
    playlist.description = normalize_canonical_playlist_description(&playlist.description);
    playlist.tracks_total = entry.tracks.len() as u32;
    playlist.snapshot_id = entry.revision;
    // These tracks are what the detail page renders, so its candidates come
    // from them rather than from whatever revision the library entry was last
    // browsed at — candidates and revision always travel together.
    playlist.cover_urls = cover_urls_from_tracks(&entry.tracks);
    PlaylistDetail {
        playlist,
        tracks: entry.tracks,
        excluded_track_ids: entry.excluded_track_ids,
    }
}

/// Upserts a playlist into the in-memory library list.
///
/// A browsed playlist has no local activity timestamps and may have an empty
/// description when Spotify omits it; incoming empty values never overwrite
/// those fields from the existing library snapshot.
pub fn upsert_playlist(playlists: &mut Vec<Playlist>, mut playlist: Playlist) {
    playlist.description = normalize_canonical_playlist_description(&playlist.description);
    if let Some(existing) = playlists.iter_mut().find(|entry| entry.id == playlist.id) {
        playlist.last_played = playlist.last_played.or(existing.last_played);
        playlist.last_activity = playlist.last_activity.or(existing.last_activity);
        if playlist.description.is_empty() {
            playlist.description = normalize_canonical_playlist_description(&existing.description);
        }
        if playlist.cover_url.is_empty() {
            playlist.cover_url = existing.cover_url.clone();
        }
        *existing = playlist;
    } else {
        playlists.push(playlist);
    }
}

/// Reports whether a playlist belongs to the rootlist-backed library.
///
/// Browsing a public playlist may still populate the bounded tracks cache, but
/// only a playlist already present here may update the library snapshot.
pub fn is_followed_playlist(playlists: &[Playlist], id: &str) -> bool {
    playlists.iter().any(|playlist| playlist.id == id)
}

/// Copies sparse display metadata and local activity timestamps from the
/// previous library snapshot. A missing source revision remains unknown:
/// a previously browsed version is not evidence that today's rootlist is fresh.
///
/// The rootlist is intentionally sparse, so a plain replacement would blank
/// these fields on every library refresh. For the candidates that is not
/// cosmetic: they are derived from a browse of the playlist itself, so a wipe
/// would drop the sidebar and home-grid mosaics back to monogram tiles until
/// each playlist happens to be browsed again. The timestamps likewise must
/// survive refresh so the sidebar does not jump back to rootlist order and
/// Home does not lose its listening history.
///
/// The name is here for the refresh that follows a creation. The rootlist is
/// eventually consistent: it lists the brand-new playlist within the second,
/// but its attributes can still arrive empty, and `PlaylistRef::name`
/// defaults to `""` when they do. Letting that overwrite the name we just
/// posted is the whole "the name I typed didn't apply" bug — the row went
/// blank a moment after appearing. An empty name is never a real rename
/// either (`rename_playlist` rejects one, and so does Spotify), so an empty
/// fresh name always means "not supplied yet", never "cleared".
pub fn carry_local_fields(previous: &[Playlist], fresh: &mut [Playlist]) {
    let by_id: std::collections::HashMap<&str, &Playlist> =
        previous.iter().map(|playlist| (playlist.id.as_str(), playlist)).collect();
    for playlist in fresh.iter_mut() {
        playlist.description = normalize_canonical_playlist_description(&playlist.description);
        if let Some(old) = by_id.get(playlist.id.as_str()) {
            if playlist.name.is_empty() {
                playlist.name = old.name.clone();
            }
            // Rootlist is sparse: preserve a browsed cover when its row has no
            // picture, but let a newly supplied rootlist cover win.
            if playlist.cover_url.is_empty() {
                playlist.cover_url = old.cover_url.clone();
            }
            if playlist.cover_urls.is_empty() { playlist.cover_urls = old.cover_urls.clone(); }
            playlist.last_played = old.last_played;
            playlist.last_activity = old.last_activity;
            if playlist.description.is_empty() {
                playlist.description = normalize_canonical_playlist_description(&old.description);
            }
        }
    }
}

/// Sorts the library most-recently-used first.
///
/// Playlists with no local activity keep their rootlist order behind the
/// active ones: the sort is stable and their key is the minimum. This is the
/// order consumed by both the sidebar and Home's remaining-library grid.
pub fn order_by_last_activity(playlists: &mut [Playlist]) {
    playlists.sort_by_key(|playlist| std::cmp::Reverse(playlist.last_activity.unwrap_or(i64::MIN)));
}

/// Inserts a just-created playlist into the library and returns it stamped.
///
/// Creating a playlist IS library activity, so it gets the same `at` stamp an
/// add-to-playlist would. That is what puts it first, and it is deliberately
/// the only mechanism that does: [`order_by_last_activity`] is the single
/// ordering authority here, the frontend sorts by the same field, and the
/// rootlist ADD asks the server for the top as well — three places agreeing
/// on one answer rather than an unsorted `insert(0, …)` that the next
/// refresh would undo.
///
/// Re-creating an id already present (a refresh that raced us) updates that
/// row rather than duplicating it, and does so through [`upsert_playlist`]
/// rather than a merge of its own. Which fields survive a row being replaced
/// in place is shared policy — it has already had to grow once — and a second
/// hand-written copy here would be the one the next field is forgotten in. A
/// freshly created row carries no description or cover, so delegating also
/// means a raced refresh keeps the ones the library already knew instead of
/// blanking them.
pub fn insert_created_playlist(
    playlists: &mut Vec<Playlist>,
    mut playlist: Playlist,
    at: i64,
) -> Playlist {
    // Stamped before the upsert, not after: `upsert_playlist` folds the
    // incoming activity with `.or(existing)`, so ours is already the winner
    // and the created row does not inherit a staler stamp.
    playlist.last_activity = Some(at);
    let id = playlist.id.clone();
    upsert_playlist(playlists, playlist);
    order_by_last_activity(playlists);
    playlists
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or_default()
}

/// Stamps `id` as used in the library at `at` and re-sorts the list.
///
/// This is used for a successful add-to-playlist. It deliberately does not
/// touch `last_played`, so a library edit cannot create a fake listening-
/// history entry on Home.
pub fn touch_playlist_activity(playlists: &mut Vec<Playlist>, id: &str, at: i64) -> bool {
    let Some(playlist) = playlists.iter_mut().find(|entry| entry.id == id) else {
        return false;
    };
    playlist.last_activity = Some(at);
    order_by_last_activity(playlists);
    true
}

/// Stamps `id` as played from at `at`, updating both local recency fields.
///
/// Deliberately not driven by opening a playlist: browsing one is not using
/// it. Which playlist a play came from is also not derivable from a track URI —
/// the same track sits in many playlists — which is why the caller passes the
/// id explicitly.
pub fn touch_playlist_played(playlists: &mut Vec<Playlist>, id: &str, at: i64) -> bool {
    let Some(playlist) = playlists.iter_mut().find(|entry| entry.id == id) else {
        return false;
    };
    playlist.last_played = Some(at);
    playlist.last_activity = Some(at);
    order_by_last_activity(playlists);
    true
}

// ---------------------------------------------------------------------------
// Cache sizes
// ---------------------------------------------------------------------------

/// How long a computed [`CacheStats`] stays fresh. Reopening Settings inside
/// this window reuses the numbers instead of re-walking the caches; a minute
/// is far shorter than it takes a user to notice a size change, and the walk
/// only ever runs off the UI thread anyway.
pub const CACHE_STATS_TTL_SECS: i64 = 60;

/// Bookkeeping files that sit in a cache directory without being cached
/// content. `cache-version` is the audio cache's layout marker (written by
/// the engine's `version_audio_cache`); counting it would report one song too
/// many for an empty cache.
const CACHE_NON_CONTENT: &[&str] = &["cache-version"];

/// Walks one cache directory and totals the files it holds.
///
/// Recursive because librespot shards its audio cache by the first byte of the
/// file id (`audio/ab/cdef…`), while the cover cache is flat. Unreadable
/// entries are skipped rather than failing the whole figure — a stat is not
/// worth an error dialog — and directory symlinks are never followed, so a
/// junction inside the cache cannot send this into a loop.
pub fn directory_usage(root: &Path) -> CacheUsage {
    let mut usage = CacheUsage::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry.path());
                continue;
            }
            let name = entry.file_name();
            if CACHE_NON_CONTENT
                .iter()
                .any(|skipped| name.eq_ignore_ascii_case(skipped))
            {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                usage.files += 1;
                usage.bytes += metadata.len();
            }
        }
    }
    usage
}

/// Sizes of both on-disk caches. Blocking filesystem work: call it from a
/// blocking task, never on the UI thread (see `commands::get_cache_stats`).
pub fn compute_cache_stats() -> CacheStats {
    CacheStats {
        audio: directory_usage(&engine_state_dir().join("audio")),
        covers: directory_usage(&data_dir().join("covers")),
    }
}

/// Removes cached content below one exact app-owned directory while retaining
/// named bookkeeping files such as the audio cache's layout marker.
///
/// Callers resolve the root (`engine/audio` or `covers`) before entering this
/// helper; no user input becomes a path. A partial clear is reported instead
/// of pretending success when Windows still has a file open.
pub fn clear_cache_directory(root: &Path, keep: &[&str]) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|error| {
        format!(
            "could not create cache directory {}: {error}",
            root.display()
        )
    })?;
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("could not read cache directory {}: {error}", root.display()))?;
    let mut failures = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if keep.iter().any(|kept| name.eq_ignore_ascii_case(kept)) {
            continue;
        }
        let path = entry.path();
        let result = match entry.file_type() {
            Ok(kind) if kind.is_dir() && !kind.is_symlink() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
        if let Err(error) = result {
            failures.push(format!("{}: {error}", path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "could not remove {} cache item(s): {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Rebuildable caches are best-effort writes, not durable application state.
fn write_json_cache<T: Serialize>(path: PathBuf, value: &T) {
    let result = serde_json::to_vec(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))
        .and_then(|bytes| std::fs::write(&path, bytes).map_err(|error| error.to_string()));
    if let Err(error) = result {
        eprintln!(
            "Spotifine: could not write cache {}: {error}",
            path.display()
        );
    }
}

fn write_json_atomic_result<T: Serialize>(path: PathBuf, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))?;
    write_bytes_atomic_result(path, &bytes)
}

fn write_bytes_atomic_result(path: PathBuf, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;

    let temp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temp)
        .map_err(|error| format!("could not create {}: {error}", temp.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    file.sync_all().map_err(|error| format!("could not flush {}: {error}", temp.display()))?;
    drop(file);
    renderer_engine::atomic::replace_file_atomically(&temp, &path, true)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_rootlist_keeps_display_metadata_without_claiming_old_revision_is_current() {
        let old = Playlist { snapshot_id: "before".into(), cover_urls: vec!["old-cover".into()],
            ..playlist("p") };
        let mut fresh = vec![Playlist { snapshot_id: "after".into(), ..playlist("p") }];
        carry_local_fields(&[old.clone()], &mut fresh);
        assert_eq!(fresh[0].snapshot_id, "after");
        let mut sparse = vec![playlist("p")];
        carry_local_fields(&[old], &mut sparse);
        assert!(sparse[0].snapshot_id.is_empty());
        assert_eq!(sparse[0].cover_urls, ["old-cover"]);
    }

    #[test]
    fn landed_likes_and_unlikes_fence_older_full_collection_reads() {
        let mut state = AppState::new(PathBuf::new());
        let a = "spotify:track:a".to_owned();
        let b = "spotify:track:b".to_owned();
        state.commit_liked_read(0, HashSet::from([a.clone()]));
        let read_generation = state.liked_generation;
        assert!(state.record_liked_write(&[a.clone()], false));
        assert!(state.record_liked_write(&[b.clone()], true));
        assert_eq!(state.commit_liked_read(read_generation, HashSet::from([a])), None);
        assert!(state.membership_refresh_queued);
        assert_eq!(state.memberships[0].uris, HashSet::from([b.clone()]));
        assert_eq!(state.commit_liked_read(state.liked_generation, HashSet::from([b.clone()])), Some(false));
        assert_eq!(state.memberships[0].uris, HashSet::from([b]));
    }

    #[test]
    fn folder_tree_survives_cache_replacement_and_deleted_playlists_stay_deleted() {
        let dir = std::env::temp_dir().join(format!("renderer-tree-{}-{}", std::process::id(), now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        let folder = |children| LibraryNodeDetail::Folder { id: "folder".into(), name: "Music".into(), children };
        let node = |id: &str| LibraryNodeDetail::Playlist { id: id.into() };
        let first = PlaylistListCache { version: 1, fetched_at: Some(1), me_id: "me".into(),
            playlists: vec![playlist("a"), playlist("b")], playlist_tree: vec![folder(vec![node("a"), node("b")])] };
        save_playlist_list(&dir, &first);
        assert_eq!(load_playlist_list(&dir).unwrap().playlist_tree, first.playlist_tree);
        let refreshed = PlaylistListCache { version: 1, fetched_at: Some(2), me_id: "me".into(),
            playlists: vec![playlist("b")], playlist_tree: vec![folder(vec![node("b")])] };
        save_playlist_list(&dir, &refreshed);
        let loaded = load_playlist_list(&dir).unwrap();
        assert_eq!(loaded.playlists, refreshed.playlists);
        assert_eq!(loaded.playlist_tree, refreshed.playlist_tree);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn app_settings_missing_startup_fields_use_safe_defaults() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"audio_cache_limit_mb":2048}"#).unwrap();
        assert_eq!(settings.audio_cache_limit_mb, 2048);
        assert!(!settings.launch_at_login);
        assert!(!settings.start_minimized);
        assert!(!settings.normalisation, "normalisation defaults to off");
        assert!(settings.devices_enabled, "devices stay on until turned off");

        let mut saved = AppSettings::default();
        saved.launch_at_login = true;
        saved.start_minimized = true;
        saved.normalisation = true;
        saved.devices_enabled = false;
        let round_trip: AppSettings =
            serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
        assert!(round_trip.launch_at_login);
        assert!(round_trip.start_minimized);
        assert!(round_trip.normalisation);
        assert!(!round_trip.devices_enabled);
    }

    fn membership(id: &str, revision: &str, uris: &[&str]) -> MembershipEntry {
        MembershipEntry {
            id: id.to_owned(),
            revision: revision.to_owned(),
            uris: uris.iter().map(|uri| uri.to_string()).collect(),
        }
    }

    #[test]
    fn membership_upsert_reports_changes_and_skips_identical_writes() {
        let mut entries = Vec::new();
        assert!(upsert_membership(
            &mut entries,
            membership(
                "p1",
                "rev1",
                &["spotify:track:a", "spotify:track:a", "spotify:track:b"]
            ),
        ));
        // Same id, same content: no change to report.
        assert!(!upsert_membership(
            &mut entries,
            membership("p1", "rev1", &["spotify:track:b", "spotify:track:a"]),
        ));
        assert!(upsert_membership(
            &mut entries,
            membership("p1", "rev2", &["spotify:track:b"]),
        ));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, "rev2");
        assert!(entries[0].contains("spotify:track:b"));
        assert!(!entries[0].contains("spotify:track:a"));
    }

    #[test]
    fn liked_writes_update_the_index_and_ignore_non_tracks() {
        let mut entries = vec![membership("p1", "rev1", &["spotify:track:a"])];
        // Unliking with no Liked Songs entry: nothing to change.
        assert!(!apply_liked_write(&mut entries, &["spotify:track:a".into()], false));
        assert!(apply_liked_write(&mut entries, &["spotify:track:b".into(), "spotify:artist:x".into()], true));
        let liked = entries.iter().find(|entry| entry.id == LIKED_MEMBERSHIP_ID).unwrap();
        assert!(liked.contains("spotify:track:b"));
        assert!(!liked.contains("spotify:artist:x"));
        // The same like again is not a change; an unlike is.
        assert!(!apply_liked_write(&mut entries, &["spotify:track:b".into()], true));
        assert!(apply_liked_write(&mut entries, &["spotify:track:b".into()], false));
        let liked = entries.iter().find(|entry| entry.id == LIKED_MEMBERSHIP_ID).unwrap();
        assert!(!liked.contains("spotify:track:b"));
        // Playlist entries are never touched.
        assert!(entries.iter().find(|entry| entry.id == "p1").unwrap().contains("spotify:track:a"));
    }

    #[test]
    fn membership_round_trips_through_the_disk_format() {
        let dir = std::env::temp_dir().join(format!("renderer-membership-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        let entries = vec![
            membership("p1", "rev1", &["spotify:track:a", "spotify:track:b"]),
            membership(LIKED_MEMBERSHIP_ID, "", &["spotify:track:c"]),
        ];
        save_membership(&dir, &entries);
        let loaded = load_membership(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "p1");
        assert_eq!(loaded[0].revision, "rev1");
        assert_eq!(loaded[0].uris.len(), 2);
        assert!(loaded[1].contains("spotify:track:c"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_owned_playlists_qualify_for_the_saved_mark() {
        let mut mine = Playlist::default();
        mine.owner_id = "user-1".into();
        assert!(playlist_qualifies(&mine, "user-1"));
        assert!(
            !playlist_qualifies(&mine, ""),
            "no identity yet means nothing qualifies"
        );

        let mut followed = Playlist::default();
        followed.owner_id = "someone-else".into();
        assert!(!playlist_qualifies(&followed, "user-1"));
    }

    fn track(id: &str) -> Track {
        Track {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            ..Track::default()
        }
    }

    #[test]
    fn playback_snapshot_round_trips_and_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-playback-state-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let mut snapshot = PlaybackSnapshot {
            version: PLAYBACK_STATE_VERSION,
            queue: vec![Track {
                id: "0123456789ABCDEFGHIJKL".to_owned(),
                uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
                duration_ms: 240_000,
                ..Track::default()
            }],
            current_index: Some(0),
            position_ms: 42_000,
            volume: 37,
            shuffle: true,
            repeat: "context".to_owned(),
            track_speed: 1.25,
            episode_speed: 1.5,
        };
        save_playback_snapshot_to(&dir, &snapshot).unwrap();
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot.clone()));

        snapshot.position_ms = 84_000;
        save_playback_snapshot_to(&dir, &snapshot).unwrap();
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot));
        assert!(!playback_state_path(&dir)
            .with_extension("json.tmp")
            .exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Songs and episodes keep their own speed across a restart, and either
    /// one out of range rejects the file rather than restoring half of it.
    #[test]
    fn mixed_episode_queue_restores_each_kinds_speed() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-episode-playback-state-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let mut snapshot = PlaybackSnapshot {
            version: PLAYBACK_STATE_VERSION,
            queue: vec![
                Track { duration_ms: 240_000, ..track("music") },
                Track {
                    id: "episode".to_owned(),
                    uri: "spotify:episode:episode".to_owned(),
                    duration_ms: 1_800_000,
                    ..Track::default()
                },
            ],
            current_index: Some(1),
            position_ms: 42_000,
            volume: 37,
            shuffle: false,
            repeat: "off".to_owned(),
            track_speed: 1.0,
            episode_speed: 4.0,
        };
        save_playback_snapshot_to(&dir, &snapshot).unwrap();
        let loaded = load_playback_snapshot_from(&dir).expect("the snapshot restores");
        assert_eq!((loaded.track_speed, loaded.episode_speed), (1.0, 4.0));
        assert_eq!(loaded, snapshot);
        for speed in [0.5, 1.0, 3.0, 4.0] {
            snapshot.track_speed = speed;
            snapshot.episode_speed = speed;
            assert!(snapshot.is_valid(), "valid speed {speed} must restore");
        }
        for speed in [0.49, 4.01, f32::NAN, f32::INFINITY] {
            snapshot.track_speed = speed;
            snapshot.episode_speed = 1.0;
            assert!(!snapshot.is_valid(), "invalid song speed {speed} must be rejected");
            snapshot.track_speed = 1.0;
            snapshot.episode_speed = speed;
            assert!(!snapshot.is_valid(), "invalid episode speed {speed} must be rejected");
        }
        snapshot.episode_speed = 1.0;
        snapshot.queue[1].uri = "spotify:show:show".to_owned();
        assert!(!snapshot.is_valid(), "a show is not a playable queue item");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sets a file's LastWriteTime, so a test can order the two durable files
    /// without sleeping through filesystem timestamp granularity.
    fn stamp_modified(path: &Path, when: SystemTime) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    #[test]
    fn playhead_sidecar_refines_the_snapshot_it_is_not_older_than() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-playhead-state-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let snapshot = PlaybackSnapshot {
            version: PLAYBACK_STATE_VERSION,
            queue: vec![
                Track {
                    duration_ms: 240_000,
                    ..track("a")
                },
                Track {
                    duration_ms: 180_000,
                    ..track("b")
                },
            ],
            current_index: Some(0),
            position_ms: 30_000,
            volume: 50,
            shuffle: false,
            repeat: "off".to_owned(),
            track_speed: 1.0,
            episode_speed: 1.0,
        };
        save_playback_snapshot_to(&dir, &snapshot).unwrap();

        // With no sidecar the snapshot is its own playhead.
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot.clone()));

        // A sidecar written after it carries what playback has moved on to,
        // including a track change the snapshot never saw.
        save_playhead_snapshot_to(&dir, Some(1), 12_000, snapshot.queue_identity()).unwrap();
        assert_eq!(
            load_playback_snapshot_from(&dir),
            Some(PlaybackSnapshot {
                current_index: Some(1),
                position_ms: 12_000,
                ..snapshot.clone()
            })
        );

        // A snapshot rewritten after the sidecar — a queue edit, or the exit
        // flush — carries the live playhead itself and outranks the sidecar.
        let older = std::fs::metadata(playback_state_path(&dir))
            .unwrap()
            .modified()
            .unwrap()
            - std::time::Duration::from_secs(60);
        stamp_modified(&playback_playhead_path(&dir), older);
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot.clone()));

        // Written in the same filesystem tick counts as no older, and this
        // sidecar names this snapshot's own queue, so the tie still goes to
        // the sidecar. A tie alone is not enough: see
        // `a_sidecar_from_another_queue_never_moves_this_snapshots_playhead`.
        let together = std::fs::metadata(playback_state_path(&dir))
            .unwrap()
            .modified()
            .unwrap();
        stamp_modified(&playback_playhead_path(&dir), together);
        assert_eq!(
            load_playback_snapshot_from(&dir),
            Some(PlaybackSnapshot {
                current_index: Some(1),
                position_ms: 12_000,
                ..snapshot.clone()
            })
        );

        // A playhead that cannot belong to this queue falls back to the
        // snapshot's own, however fresh it is.
        save_playhead_snapshot_to(&dir, Some(7), 1_000, snapshot.queue_identity()).unwrap();
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot.clone()));
        save_playhead_snapshot_to(&dir, Some(0), 240_001, snapshot.queue_identity()).unwrap();
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot.clone()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A sidecar belongs to one snapshot, and the file timestamps cannot say
    /// which: two writes inside one filesystem tick tie, and a clock that steps
    /// back makes the newer file look older. Without the token, a playhead
    /// written for the queue the user left behind is applied to the queue that
    /// replaced it whenever its index and position happen to fit — silently
    /// restoring the wrong row and position.
    #[test]
    fn a_sidecar_from_another_queue_never_moves_this_snapshots_playhead() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-playhead-identity-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let first = PlaybackSnapshot {
            version: PLAYBACK_STATE_VERSION,
            queue: vec![
                Track {
                    duration_ms: 240_000,
                    ..track("a")
                },
                Track {
                    duration_ms: 180_000,
                    ..track("b")
                },
            ],
            current_index: Some(0),
            position_ms: 30_000,
            volume: 50,
            shuffle: false,
            repeat: "off".to_owned(),
            track_speed: 1.0,
            episode_speed: 1.0,
        };
        save_playback_snapshot_to(&dir, &first).unwrap();
        save_playhead_snapshot_to(&dir, Some(1), 12_000, first.queue_identity()).unwrap();

        // The user opens another playlist: the snapshot now holds rows the
        // sidecar knows nothing about. Both of them are long enough for index
        // 1 and 12s to fit, which is what used to make this silent.
        let second = PlaybackSnapshot {
            queue: vec![
                Track {
                    duration_ms: 200_000,
                    ..track("c")
                },
                Track {
                    duration_ms: 210_000,
                    ..track("d")
                },
            ],
            ..first.clone()
        };
        save_playback_snapshot_to(&dir, &second).unwrap();

        // Even with the sidecar's timestamp made equal to the snapshot's — the
        // tie that used to hand it the snapshot's playhead — the token rejects
        // it: the snapshot's own playhead stands.
        let together = std::fs::metadata(playback_state_path(&dir))
            .unwrap()
            .modified()
            .unwrap();
        stamp_modified(&playback_playhead_path(&dir), together);
        assert_eq!(
            load_playback_snapshot_from(&dir),
            Some(second),
            "a sidecar for another queue must not seek inside this one"
        );

        // And it is still the sidecar for the queue it *does* name: the guard
        // is the identity, not the freshness.
        save_playback_snapshot_to(&dir, &first).unwrap();
        let together = std::fs::metadata(playback_state_path(&dir))
            .unwrap()
            .modified()
            .unwrap();
        stamp_modified(&playback_playhead_path(&dir), together);
        assert_eq!(
            load_playback_snapshot_from(&dir),
            Some(PlaybackSnapshot {
                current_index: Some(1),
                position_ms: 12_000,
                ..first.clone()
            })
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sidecar's token is the snapshot's own: the writer stamps what it
    /// compared against, and the loader recomputes it from the rows it read,
    /// so the same queue always yields the same number — across a JSON
    /// round-trip, and regardless of which row is current.
    #[test]
    fn the_queue_identity_survives_a_round_trip_and_ignores_the_playhead() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-playhead-token-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let snapshot = PlaybackSnapshot {
            version: PLAYBACK_STATE_VERSION,
            queue: vec![
                Track {
                    duration_ms: 240_000,
                    ..track("a")
                },
                Track {
                    duration_ms: 180_000,
                    ..track("b")
                },
            ],
            current_index: Some(0),
            position_ms: 30_000,
            volume: 50,
            shuffle: false,
            repeat: "off".to_owned(),
            track_speed: 1.0,
            episode_speed: 1.0,
        };
        save_playback_snapshot_to(&dir, &snapshot).unwrap();
        let loaded = load_playback_snapshot_from(&dir).expect("snapshot reads back");
        assert_eq!(loaded.queue_identity(), snapshot.queue_identity());

        // The playhead is not part of the identity: it moves constantly, and a
        // token that moved with it would reject the sidecar it belongs to.
        let mut moved = snapshot.clone();
        moved.current_index = Some(1);
        moved.position_ms = 90_000;
        assert_eq!(moved.queue_identity(), snapshot.queue_identity());

        // A different queue is a different identity, and so is a reorder of
        // the same rows: the token names the rows this snapshot describes.
        let mut reordered = snapshot.clone();
        reordered.queue.swap(0, 1);
        assert_ne!(reordered.queue_identity(), snapshot.queue_identity());
        let mut shorter = snapshot.clone();
        shorter.queue.pop();
        shorter.current_index = Some(0);
        assert_ne!(shorter.queue_identity(), snapshot.queue_identity());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_or_unknown_playback_snapshots_are_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-bad-playback-state-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(playback_state_path(&dir), b"{not-json").unwrap();
        assert!(load_playback_snapshot_from(&dir).is_none());
        std::fs::write(
            playback_state_path(&dir),
            br#"{"version":99,"queue":[],"current_index":null,"position_ms":0,"volume":50,"shuffle":false,"repeat":"off"}"#,
        )
        .unwrap();
        assert!(load_playback_snapshot_from(&dir).is_none());
        std::fs::write(
            playback_state_path(&dir),
            br#"{"version":2,"queue":[{"id":"0123456789ABCDEFGHIJKL","uri":"spotify:track:0123456789ABCDEFGHIJKL","duration_ms":240000}],"current_index":0,"position_ms":42000,"volume":50,"shuffle":false,"repeat":"off","playback_speed":1.0}"#,
        )
        .unwrap();
        assert!(
            load_playback_snapshot_from(&dir).is_none(),
            "source-coordinate snapshots must not be restored as compiled positions"
        );
        std::fs::write(
            playback_state_path(&dir),
            br#"{"version":3,"queue":[{"id":"0123456789ABCDEFGHIJKL","uri":"spotify:track:0123456789ABCDEFGHIJKL","duration_ms":240000}],"current_index":0,"position_ms":42000,"volume":50,"shuffle":false,"repeat":"off","playback_speed":1.5}"#,
        )
        .unwrap();
        assert!(
            load_playback_snapshot_from(&dir).is_none(),
            "a single speed cannot say which kind of item it was for"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn track_with_cover(id: &str, cover_url: &str) -> Track {
        Track {
            cover_url: cover_url.to_owned(),
            ..track(id)
        }
    }

    fn playlist(id: &str) -> Playlist {
        Playlist {
            id: id.to_owned(),
            uri: format!("spotify:playlist:{id}"),
            ..Playlist::default()
        }
    }

    #[test]
    fn playlist_refresh_state_coalesces_same_id_but_allows_other_ids() {
        let mut state = AppState::new(PathBuf::from("unused"));

        assert!(state.start_playlist_refresh("p1", RefreshCause::Reopen));
        // An edit landing mid-fetch is refused but queued for a single
        // re-run: the read in flight may predate it and must not speak for it.
        assert!(!state.start_playlist_refresh("p1", RefreshCause::Edit));
        // A re-open is refused too, but queues nothing: the fetch in flight
        // already answers it, and a second round trip would only discard the
        // payload that fetch is about to emit.
        assert!(!state.start_playlist_refresh("p1", RefreshCause::Reopen));
        assert!(state.start_playlist_refresh("p2", RefreshCause::Reopen));

        state.finish_playlist_refresh("p1");
        assert!(state.take_playlist_refresh_queued("p1"));
        assert!(!state.take_playlist_refresh_queued("p1"));

        assert!(state.start_playlist_refresh("p1", RefreshCause::Reopen));
        assert!(!state.start_playlist_refresh("p1", RefreshCause::Reopen));
        state.finish_playlist_refresh("p1");
        assert!(
            !state.take_playlist_refresh_queued("p1"),
            "a re-open behind a fetch leaves no second pass behind it"
        );
        state.finish_playlist_refresh("p2");
    }

    #[test]
    fn followed_browse_updates_library_but_public_browse_does_not_insert() {
        let mut followed = vec![playlist("followed")];
        assert!(is_followed_playlist(&followed, "followed"));
        if is_followed_playlist(&followed, "followed") {
            upsert_playlist(
                &mut followed,
                Playlist {
                    snapshot_id: "rev-new".into(),
                    ..playlist("followed")
                },
            );
        }
        assert_eq!(followed[0].snapshot_id, "rev-new");

        let mut public = Vec::new();
        assert!(!is_followed_playlist(&public, "public"));
        if is_followed_playlist(&public, "public") {
            upsert_playlist(&mut public, playlist("public"));
        }
        assert!(public.is_empty());
    }

    #[test]
    fn tracks_cache_keeps_most_recent_first_and_caps_at_25() {
        let mut cache = Vec::new();
        for id in 0..30 {
            upsert_tracks_cache(
                &mut cache,
                PlaylistTracksEntry {
                    id: format!("p{id}"),
                    fetched_at: Some(id),
                    revision: String::new(),
                    tracks: vec![track("t")],
                    excluded_track_ids: Vec::new(),
                },
            );
        }
        assert_eq!(cache.len(), 25);
        assert_eq!(cache[0].id, "p29");
        assert_eq!(cache[24].id, "p5");
    }

    #[test]
    fn upsert_refreshes_an_existing_entry_in_place() {
        let mut cache = vec![PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(1),
            revision: "old".into(),
            tracks: vec![track("a")],
            excluded_track_ids: Vec::new(),
        }];
        upsert_tracks_cache(
            &mut cache,
            PlaylistTracksEntry {
                id: "p1".into(),
                fetched_at: Some(2),
                revision: "new".into(),
                tracks: vec![track("b")],
                excluded_track_ids: vec!["b".into()],
            },
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].revision, "new");
        assert_eq!(cache[0].tracks[0].id, "b");
    }

    #[test]
    fn a_browse_that_confirms_the_cached_entry_is_not_a_change() {
        let mut cache = Vec::new();
        let entry = |revision: &str, track_id: &str| PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(1),
            revision: revision.into(),
            tracks: vec![track(track_id)],
            excluded_track_ids: Vec::new(),
        };

        assert!(upsert_tracks_cache(&mut cache, entry("rev1", "a")));
        assert!(
            !upsert_tracks_cache(&mut cache, entry("rev1", "a")),
            "a re-browse only refreshes fetched_at, which is never read back"
        );

        assert!(
            upsert_tracks_cache(&mut cache, entry("rev2", "a")),
            "a new revision is a change"
        );
        assert!(
            upsert_tracks_cache(&mut cache, entry("rev2", "b")),
            "a replaced row is a change"
        );
        assert!(
            !upsert_tracks_cache(&mut cache, entry("rev2", "b")),
            "the same revision and rows are not a change"
        );

        let mut excluded = entry("rev2", "b");
        excluded.excluded_track_ids = vec!["b".into()];
        assert!(
            upsert_tracks_cache(&mut cache, excluded),
            "an exclusion edit is a change"
        );
    }

    #[test]
    fn detail_from_cache_falls_back_to_blank_metadata() {
        let state = AppState::new(PathBuf::from("unused"));
        let entry = PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(1),
            revision: "rev".into(),
            tracks: vec![track("t")],
            excluded_track_ids: vec!["t".into()],
        };
        let detail = playlist_detail_from_cache(&state, entry);
        assert_eq!(detail.playlist.id, "p1");
        assert_eq!(detail.playlist.uri, "spotify:playlist:p1");
        assert_eq!(detail.playlist.snapshot_id, "rev");
        assert_eq!(detail.playlist.tracks_total, 1);
        assert_eq!(detail.excluded_track_ids, vec!["t"]);
        assert_eq!(detail.tracks[0].id, "t");
    }

    #[test]
    fn detail_from_cache_derives_candidates_from_the_cached_tracks() {
        let state = AppState::new(PathBuf::from("unused"));
        let entry = PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(1),
            revision: "rev".into(),
            tracks: vec![
                track_with_cover("a", "cover-a"),
                track_with_cover("b", "cover-a"),
                track_with_cover("c", "cover-b"),
            ],
            excluded_track_ids: Vec::new(),
        };
        let detail = playlist_detail_from_cache(&state, entry);
        assert_eq!(detail.playlist.cover_urls, vec!["cover-a", "cover-b"]);
    }

    #[test]
    fn a_library_refresh_carries_local_fields_forward() {
        // The rootlist supplies none of them, and losing the candidates would
        // drop every browsed playlist's mosaic back to a monogram tile until
        // it is browsed again. Both timestamps are local-only as well.
        let previous = vec![Playlist {
            description: "<p>Road&nbsp;music</p>".into(),
            cover_urls: vec!["cover-a".into(), "cover-b".into()],
            last_played: Some(1_000),
            last_activity: Some(2_000),
            ..playlist("p1")
        }];
        let mut fresh = vec![playlist("p1"), playlist("p2")];
        carry_local_fields(&previous, &mut fresh);
        assert_eq!(fresh[0].description, "Road music");
        assert_eq!(fresh[0].cover_urls, vec!["cover-a", "cover-b"]);
        assert_eq!(fresh[0].last_played, Some(1_000));
        assert_eq!(fresh[0].last_activity, Some(2_000));
        // A playlist the previous snapshot never had stays empty — it has
        // never been browsed or used — and keeps its rootlist place.
        assert!(fresh[1].cover_urls.is_empty());
        assert_eq!(fresh[1].last_played, None);
        assert_eq!(fresh[1].last_activity, None);
    }

    #[test]
    fn a_sparse_rootlist_row_does_not_erase_a_browsed_cover() {
        let previous = vec![Playlist {
            cover_url: "https://i.scdn.co/image/full".into(),
            ..playlist("p1")
        }];
        let mut fresh = vec![
            playlist("p1"),
            Playlist {
                cover_url: "https://i.scdn.co/image/new".into(),
                ..playlist("p1")
            },
        ];
        carry_local_fields(&previous, &mut fresh);
        assert_eq!(fresh[0].cover_url, "https://i.scdn.co/image/full");
        assert_eq!(fresh[1].cover_url, "https://i.scdn.co/image/new");
    }

    /// The rootlist lists a brand-new playlist before its attributes are
    /// readable, so the refresh that follows a creation can carry `name: ""`.
    /// Letting that land is what blanked the row the user had just named.
    #[test]
    fn a_rootlist_row_with_no_name_yet_keeps_the_name_we_already_know() {
        let previous = vec![Playlist {
            name: "Road Trip".into(),
            ..playlist("p1")
        }];
        let mut fresh = vec![
            playlist("p1"),
            Playlist {
                name: "Renamed".into(),
                ..playlist("p1")
            },
        ];
        carry_local_fields(&previous, &mut fresh);
        assert_eq!(fresh[0].name, "Road Trip");
        // A name the rootlist DOES supply still wins: this is a fallback for
        // missing data, not a local override of the server's answer.
        assert_eq!(fresh[1].name, "Renamed");
    }

    #[test]
    fn a_created_playlist_takes_the_top_of_the_library() {
        let mut playlists = vec![
            Playlist {
                last_activity: Some(500),
                ..playlist("busy")
            },
            playlist("never-used"),
        ];
        let created = insert_created_playlist(
            &mut playlists,
            Playlist {
                name: "Road Trip".into(),
                ..playlist("new")
            },
            900,
        );
        // The returned row is what the frontend inserts optimistically, so it
        // must carry the same stamp the library was sorted by.
        assert_eq!(created.id, "new");
        assert_eq!(created.name, "Road Trip");
        assert_eq!(created.last_activity, Some(900));
        // Creating is library activity, not listening history.
        assert_eq!(created.last_played, None);
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "busy", "never-used"]);
        assert_eq!(playlists[0].last_activity, Some(900));
    }

    #[test]
    fn re_inserting_a_known_id_updates_that_row_instead_of_duplicating_it() {
        // A library refresh can land between the engine's answer and this
        // insert, so the id may already be present.
        let mut playlists = vec![
            Playlist {
                last_played: Some(50),
                last_activity: Some(100),
                ..playlist("new")
            },
            Playlist {
                last_activity: Some(700),
                ..playlist("busy")
            },
        ];
        let created = insert_created_playlist(
            &mut playlists,
            Playlist {
                name: "Road Trip".into(),
                ..playlist("new")
            },
            900,
        );
        assert_eq!(playlists.len(), 2);
        assert_eq!(playlists[0].id, "new");
        assert_eq!(playlists[0].name, "Road Trip");
        assert_eq!(playlists[0].last_activity, Some(900));
        // An existing listening-history stamp is not erased by the re-insert.
        assert_eq!(playlists[0].last_played, Some(50));
        assert_eq!(created.last_played, Some(50));
    }

    #[test]
    fn the_library_is_ordered_most_recently_active_first() {
        let mut playlists = vec![
            playlist("never-a"),
            Playlist {
                last_played: Some(900),
                last_activity: Some(100),
                ..playlist("old")
            },
            playlist("never-b"),
            Playlist {
                last_played: Some(100),
                last_activity: Some(300),
                ..playlist("newest")
            },
            Playlist {
                last_played: Some(500),
                last_activity: Some(200),
                ..playlist("middle")
            },
        ];
        order_by_last_activity(&mut playlists);
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        // Library activity, not listening history, controls this order.
        assert_eq!(ids, vec!["newest", "middle", "old", "never-a", "never-b"]);
    }

    #[test]
    fn playing_from_a_playlist_stamps_played_and_activity_and_moves_it_to_the_front() {
        let mut playlists = vec![playlist("p1"), playlist("p2"), playlist("p3")];
        assert!(touch_playlist_played(&mut playlists, "p3", 500));
        assert_eq!(playlists[0].id, "p3");
        assert_eq!(playlists[0].last_played, Some(500));
        assert_eq!(playlists[0].last_activity, Some(500));
        // The untouched ones keep rootlist order behind it.
        assert_eq!(playlists[1].id, "p1");
        assert_eq!(playlists[2].id, "p2");

        // A later play from a different playlist takes the lead in turn.
        assert!(touch_playlist_played(&mut playlists, "p1", 600));
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p3", "p2"]);

        // Playback from outside the library (an album, an artist page) is a no-op.
        assert!(!touch_playlist_played(&mut playlists, "not-followed", 700));
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p3", "p2"]);
    }

    #[test]
    fn adding_to_a_playlist_stamps_activity_without_creating_listening_history() {
        let mut playlists = vec![
            Playlist {
                last_played: Some(100),
                last_activity: Some(100),
                ..playlist("played")
            },
            playlist("added"),
        ];
        assert!(touch_playlist_activity(&mut playlists, "added", 200));
        assert_eq!(playlists[0].id, "added");
        assert_eq!(playlists[0].last_activity, Some(200));
        assert_eq!(playlists[0].last_played, None);
        assert_eq!(playlists[1].last_played, Some(100));
        assert_eq!(playlists[1].last_activity, Some(100));

        // A missing playlist cannot create either kind of stamp.
        assert!(!touch_playlist_activity(
            &mut playlists,
            "not-followed",
            300
        ));
    }

    #[test]
    fn a_background_refresh_cannot_erase_activity_stamps() {
        // fetch_playlist upserts the browsed playlist, which carries no
        // timestamp; without the guard in upsert_playlist that would undo the
        // stamps written moments earlier by touch commands.
        let mut playlists = vec![Playlist {
            description: "<p>Saved before browse</p>".into(),
            last_played: Some(900),
            last_activity: Some(950),
            ..playlist("p1")
        }];
        upsert_playlist(
            &mut playlists,
            Playlist {
                snapshot_id: "rev-b".into(),
                ..playlist("p1")
            },
        );
        assert_eq!(playlists[0].last_played, Some(900));
        assert_eq!(playlists[0].last_activity, Some(950));
        assert_eq!(playlists[0].snapshot_id, "rev-b");
        assert_eq!(playlists[0].description, "Saved before browse");

        // Explicit newer timestamps still win independently.
        upsert_playlist(
            &mut playlists,
            Playlist {
                last_played: Some(1_000),
                last_activity: Some(1_100),
                ..playlist("p1")
            },
        );
        assert_eq!(playlists[0].last_played, Some(1_000));
        assert_eq!(playlists[0].last_activity, Some(1_100));
    }

    #[test]
    fn a_library_cache_written_before_cover_urls_existed_still_loads() {
        let dir = std::env::temp_dir().join(format!("renderer-old-cache-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("playlist_list.json"),
            r#"{"version":1,"fetched_at":42,"me_id":"me","playlists":[
                {"id":"p1","uri":"spotify:playlist:p1","name":"Mixtape","owner":"me",
                 "owner_id":"me","cover_url":"","collaborative":false,"tracks_total":7,
                 "snapshot_id":"rev-a"}]}"#,
        )
        .unwrap();
        let cache = load_playlist_list(&dir).expect("an old cache file must still deserialize");
        assert_eq!(cache.playlists.len(), 1);
        assert_eq!(cache.playlists[0].snapshot_id, "rev-a");
        assert!(cache.playlists[0].cover_urls.is_empty());
        // Written before either local timestamp existed: it reads as never
        // used, so the playlist keeps its rootlist position and does not
        // invent a Home listening-history entry.
        assert_eq!(cache.playlists[0].last_played, None);
        assert_eq!(cache.playlists[0].last_activity, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn local_activity_stamps_survive_a_save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("renderer-lru-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        save_playlist_list(
            &dir,
            &PlaylistListCache {
                version: 1,
                fetched_at: Some(42),
                me_id: "me".to_owned(),
                playlist_tree: Vec::new(),
                playlists: vec![
                    Playlist {
                        last_played: Some(1_700),
                        last_activity: Some(1_800),
                        ..playlist("opened")
                    },
                    playlist("never"),
                ],
            },
        );
        let cache = load_playlist_list(&dir).expect("the cache round-trips");
        assert_eq!(cache.playlists[0].last_played, Some(1_700));
        assert_eq!(cache.playlists[0].last_activity, Some(1_800));
        assert_eq!(cache.playlists[1].last_played, None);
        assert_eq!(cache.playlists[1].last_activity, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn playlist_cache_save_replaces_an_existing_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-cache-replace-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["first", "second"] {
            save_playlist_list(
                &dir,
                &PlaylistListCache {
                    version: 1,
                    fetched_at: Some(42),
                    me_id: "me".to_owned(),
                    playlist_tree: Vec::new(),
                    playlists: vec![Playlist {
                        name: name.to_owned(),
                        ..playlist("p1")
                    }],
                },
            );
        }
        assert_eq!(
            load_playlist_list(&dir).unwrap().playlists[0].name,
            "second"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_usage_totals_files_recursively_and_skips_bookkeeping() {
        let dir = std::env::temp_dir().join(format!("renderer-usage-{}", now_secs()));
        let shard = dir.join("ab");
        std::fs::create_dir_all(&shard).unwrap();
        // The audio cache shape: a layout marker beside sharded song files.
        std::fs::write(dir.join("cache-version"), "2\n").unwrap();
        std::fs::write(shard.join("song-one"), vec![0u8; 1_000]).unwrap();
        std::fs::write(shard.join("song-two"), vec![0u8; 2_500]).unwrap();

        let usage = directory_usage(&dir);
        assert_eq!(usage.files, 2, "the version marker is not a cached song");
        assert_eq!(usage.bytes, 3_500);

        // A directory that was never created reads as empty, not as an error.
        assert_eq!(directory_usage(&dir.join("missing")), CacheUsage::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_a_cache_keeps_only_explicit_bookkeeping() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-clear-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let shard = dir.join("ab");
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(dir.join("cache-version"), "2\n").unwrap();
        std::fs::write(dir.join("loose-cover"), b"cover").unwrap();
        std::fs::write(shard.join("song"), b"audio").unwrap();

        clear_cache_directory(&dir, &["cache-version"]).unwrap();
        assert!(dir.join("cache-version").exists());
        assert!(!dir.join("loose-cover").exists());
        assert!(!shard.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tracks_cache_written_before_artist_ids_loads_with_aligned_lists() {
        let dir = std::env::temp_dir().join(format!("renderer-old-tracks-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("playlist_tracks_cache.json"),
            r#"{"version":1,"saved_at":42,"playlists":[{"id":"p1","fetched_at":1,"revision":"rev",
                "tracks":[{"id":"t1","uri":"spotify:track:t1","name":"Track",
                           "artist_names":["A","B","C"],"artist_id":"a1",
                           "album_id":"al","album_name":"Album","cover_url":"","duration_ms":1000}]}]}"#,
        )
        .unwrap();
        let loaded = load_tracks_cache(&dir);
        let track = &loaded[0].tracks[0];
        assert_eq!(track.artist_names.len(), 3);
        assert_eq!(
            track.artist_ids.len(),
            3,
            "an upgraded cache must still zip index-for-index"
        );
        assert!(track.artist_ids.iter().all(|id| id.is_empty()));
        assert_eq!(track.artist_id, "a1");
        assert!(
            loaded[0].excluded_track_ids.is_empty(),
            "legacy detail caches default to no exclusions"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The audio cache can be cleared or pruned between two runs of the app,
    /// and this file would not know. A download mark on disk would be a claim
    /// about a directory this cache does not own, so it is never written, and
    /// a browse that only changed marks owes the file nothing.
    #[test]
    fn a_tracks_cache_never_stores_a_download_mark() {
        let dir = std::env::temp_dir().join(format!("renderer-cached-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = |cached| PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(42),
            revision: "rev".into(),
            tracks: vec![Track { cached, context: "playlist:p1".into(), ..track("t") }],
            excluded_track_ids: Vec::new(),
        };
        let mut entries = vec![entry(true)];
        save_tracks_cache(&dir, &entries);
        let bytes = std::fs::read(dir.join("playlist_tracks_cache.json")).unwrap();
        let stored: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let track = &stored["playlists"][0]["tracks"][0];
        assert_eq!(track["id"], "t");
        assert!(track.get("cached").is_none() && track.get("context").is_none(), "{track}");
        assert!(!load_tracks_cache(&dir)[0].tracks[0].cached, "only a live browse may claim a track is on disk");
        assert!(!upsert_tracks_cache(&mut entries, entry(false)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracks_cache_round_trips_through_the_disk_format() {
        let dir = std::env::temp_dir().join(format!("renderer-test-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        let entries = vec![PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(42),
            revision: "rev".into(),
            tracks: vec![track("t")],
            excluded_track_ids: vec!["t".into()],
        }];
        save_tracks_cache(&dir, &entries);
        let loaded = load_tracks_cache(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "p1");
        assert_eq!(loaded[0].revision, "rev");
        assert_eq!(loaded[0].excluded_track_ids, vec!["t"]);
        assert_eq!(loaded[0].tracks[0].id, "t");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
