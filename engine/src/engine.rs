use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use librespot_core::SpotifyUri;
use librespot_core::cache::Cache;
use librespot_metadata::Metadata;
use librespot_playback::mixer::{Mixer, softmixer::SoftMixer};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc;

use crate::audio::{self, AudioSignal};
use crate::auth::{
    AUDIO_START_TIMEOUT, AuthFailure, ConnectedSession, OauthListener, PendingAuth, PlaybackError,
    PlaybackHandles, bind_oauth_listener, complete_oauth, connect_cached, create_playback,
    percent_to_volume, prepare_oauth, stored_volume_percent,
};
use crate::customization::{EditTimeline, TrackEditStore, validate_definition};
use crate::history::ListeningHistory;
use crate::io::ProtocolWriter;
use renderer_engine::protocol::{
    AuthState, Command, LoopRange, PositionEvent,
    RepeatMode, Response, StateEvent, TimeRange, TrackEdit, TrackEditDefinition, TrackEditStatus,
    TrackRef, VolumeEvent,
};

pub struct CacheSignal {
    id: String,
    generation: u64,
    lease: Arc<Mutex<bool>>,
}
/// Pressing previous within this many milliseconds of a track start restarts
/// the current track instead of switching tracks. Mirrors the UI's optimistic
/// restart window (OnPrevious in app.cpp) so both sides agree.
const PREVIOUS_RESTART_THRESHOLD_MS: u32 = 3_000;
/// Minimum spacing between command-driven track changes (queue replacement,
/// queue index changes, Next, and Previous). Loading an uncached track fetches
/// its audio key from Spotify's key service, which rate-limits bursts ("Unable to load
/// key, continuing without decryption") and playback dies with decoder
/// errors. 250 ms bounds key requests to roughly 4-8/s while keeping rapid
/// next/prev responsive; presses are delayed, never dropped. Natural
/// end-of-track advances are not paced.
const TRACK_CHANGE_MIN_INTERVAL: Duration = Duration::from_millis(250);
/// Failures from a rapid load burst are commonly key-service or network
/// transient errors, not evidence that every involved cache entry is bad.
/// Keep the burst window long enough to cover several paced changes.
const TRACK_CHANGE_BURST_WINDOW: Duration = Duration::from_secs(2);
const TRACK_CHANGE_BURST_MINIMUM: usize = 2;
/// A second current-track load failure within this window is treated as part
/// of the same transient burst. The first failure in a quiet period remains
/// eligible for cache cleanup.
///
/// This is sized against how far apart *failures* land, not how far apart
/// clicks land, and the two are nothing alike. A failing load takes seconds to
/// give up: the audio key times out, librespot falls back to downloading, and
/// the decoder waits out its own deadline. Measured across a real dead-session
/// episode, consecutive `Unavailable` events arrived 6.5 s and 8.9 s apart —
/// so the 2 s window this started at pruned itself empty between every pair,
/// classified each failure as isolated, and evicted the cache for every track
/// it touched. The window has to outlast the failure, not the gesture.
const UNAVAILABLE_BURST_WINDOW: Duration = Duration::from_secs(30);
/// How many current-track load failures inside [`UNAVAILABLE_BURST_WINDOW`]
/// stop playback outright instead of skipping onward.
///
/// One failure is a track: skip it, which is what the engine has always done
/// and what a genuinely dead row still deserves. A run of them is not a queue
/// full of dead rows, it is the service refusing to serve this client at all —
/// Spotify's key service answers `error audio key 0 2`, librespot warns that it
/// is "continuing without decryption", and the decoder times out on the
/// resulting garbage about three seconds later. Skipping through that empties
/// the whole queue in silence at a key request per track, feeding the very
/// throttle that caused it, and it outlives the process because the throttle
/// is server-side. Three is the smallest limit that still lets two genuinely
/// dead rows sit next to each other in an otherwise working queue.
const UNAVAILABLE_STOP_LIMIT: usize = 3;

/// How long a failed row waits before the engine loads it one more time.
///
/// The refusal this recovers from is transient by nature. Across five separate
/// episodes in the diagnostic log the key service answered `error audio key
/// 0 2` in tight bursts of a few minutes and then went back to working
/// perfectly; no track is individually cursed, and the same track that failed
/// plays on the next attempt once the bucket refills. Skipping straight past a
/// row on its first failure therefore throws away a track that was never
/// broken, and does it at the cost of another key request for the row after.
///
/// One retry, not a ladder of them. A longer backoff would ride out more of a
/// burst, but a queue that sits visibly dead for half a minute and then starts
/// is worse than one that says so: the honest answer to a burst is
/// [`UNAVAILABLE_STOP_LIMIT`], and the retry's job is only to separate a
/// momentary refusal from one. It also makes the breaker reachable without
/// spending the queue — the first row's two failures plus the next row's one
/// reach the limit having skipped a single track.
const LOAD_RETRY_BACKOFF: Duration = Duration::from_secs(3);

/// How much of a track may be left unplayed for its end to still be the end.
///
/// librespot reports `EndOfTrack` for a track that finished and for a track
/// whose decoder collapsed mid-stream (`player.rs`: "Skipping to next track,
/// unable to decode samples" and "...unable to get next packet" both send
/// `EndOfTrack`), and nothing in the event tells them apart. Position does:
/// a track that ended is at its end. The allowance covers the output buffer
/// the engine's projected playhead trails the decoder by, which is
/// [`crate::audio`]'s write-ahead plus the device's own queue — milliseconds,
/// not seconds — and is set an order of magnitude above that because the
/// failure it has to separate from leaves minutes unplayed, not seconds.
const ABNORMAL_END_REMAINDER_MS: u32 = 5_000;

/// How little of the current track may remain before its successor is worth
/// an audio key.
///
/// A preload costs exactly one key request, the same as a play, and it is the
/// one request the user did not ask for. Issuing it on every track change —
/// which is what this engine used to do, from seven call sites — doubled the
/// draw on Spotify's key service, and doubled it hardest during the behaviour
/// that provokes the service in the first place. The diagnostic log shows the
/// shape plainly: seven track changes in 57 seconds costing fourteen loads,
/// with the first `error audio key 0 2` arriving on the fourteenth, and every
/// one of those five key-error episodes preceded by minutes of load rates ten
/// to twenty times the 1-2/min baseline. Each click also threw away the
/// preload the previous click had just paid for.
///
/// A watermark makes churn free without measuring it. A gapless preload exists
/// to smooth a track that is about to *finish*; a track the user started two
/// seconds ago is not about to finish, and if they are skipping it never will,
/// so the watermark is simply never reached and no request is made. Nothing is
/// rate-estimated, debounced, or reverse-engineered from the burst data — the
/// data does not support a threshold, and none is needed.
///
/// Thirty seconds is librespot's own answer (`player.rs`:
/// `PRELOAD_NEXT_TRACK_BEFORE_END_DURATION_MS`), which is what its preload
/// path is built to work with: resolve the file, fetch the key, download and
/// parse enough to construct a decoder, then hold it. Two local reasons argue
/// for that end of "tens of seconds" rather than a tighter one. The playhead
/// this is measured against is a projection that trails the decoder by the
/// output buffer, and playback speed rescales the whole thing — at 2x, thirty
/// seconds of track is fifteen seconds of wall clock to get the work done in.
const PRELOAD_WATERMARK_MS: u32 = 30_000;

/// How long to wait before the first automatic reconnect after the session
/// dies, and the ceiling the wait doubles up to while reconnects keep failing.
///
/// librespot invalidates its own `Session` when the access-point connection
/// drops (`session.rs`: the sender task's error arm calls `shutdown()`), which
/// closes the channel manager every audio-key request travels over. Metadata
/// does not go that way — `spclient` is plain HTTPS with its own pool — so the
/// library, search and browse all keep working while playback is dead, and
/// nothing surfaces the problem except tracks refusing to start. Nothing here
/// used to notice, so the engine held the corpse until it was restarted.
///
/// The first reconnect runs as soon as the dead session is noticed, because
/// the common cause is a transient drop the reconnect will simply fix; these
/// space out the attempts after a failed one. The ceiling exists so a genuine
/// outage is not hammered.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(2);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Wait before trying a present output device again after a failed open, and
/// the ceiling that wait doubles up to.
///
/// These are short because this wait is the delay between a device that
/// refused its first open and hearing music. A machine with no device at all
/// waits on no clock: the OS reports the default output appearing, and that
/// notification is what probes it (see [`Engine::tick_audio_device`]).
const AUDIO_PROBE_BACKOFF_MIN: Duration = Duration::from_secs(2);
const AUDIO_PROBE_BACKOFF_MAX: Duration = Duration::from_secs(10);
/// The longest wait a probe may reach after an open that never returned. See
/// [`PlayerFailure::Blocked`].
const AUDIO_BLOCKED_PROBE_BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Slack added to the output's own account of its queued tail before the
/// natural end of a queue stops the player: the device holds a buffer of its
/// own past the ring (a wireless one, a long one), and stopping late only
/// costs a few silent callbacks once per finished queue.
const OUTPUT_DRAIN_MARGIN: Duration = Duration::from_millis(500);

/// How long a changed volume has to sit still before [`Engine::tick_volume_persist`]
/// writes it to the librespot cache.
///
/// The write is a file create, and the UI paces `set_volume` at 50 ms while a
/// slider is dragged. A one-second quiet deadline coalesces the gesture into
/// one write without needing a timer while no change is pending.
const VOLUME_PERSIST_QUIET: Duration = Duration::from_secs(1);

pub struct Engine {
    writer: ProtocolWriter,
    cache: Cache,
    temporary_directory: std::path::PathBuf,
    /// The app-owned `credentials.json` inside the cache, removed by
    /// `logout`. Kept separately because librespot's `Cache` offers no
    /// credential-removal API.
    credentials_file: std::path::PathBuf,
    track_edits: TrackEditStore,
    state: PlaybackState,
    /// The generation of the queue rows themselves.
    ///
    /// Bumped through [`Engine::rows_changed`] by every writer of the rows:
    /// an install, an append, a removal, a move. [`Engine::emit_state`] sends
    /// the rows only when this is newer than what the last state carried, and
    /// the shell and the webview keep the rows they hold otherwise — so a
    /// writer that forgets it leaves both rendering rows the engine no longer
    /// has.
    rows_revision: u64,
    /// The generation [`Engine::emit_state`] last published rows under. `None`
    /// until the first state of this process (and after `status`, which asks
    /// for everything again).
    emitted_rows_revision: Option<u64>,
    /// The generation of the play order derived from the rows: the current
    /// row, the shuffle bag, the shuffle and repeat modes, and what counts as
    /// eligible. Bumped through [`Engine::order_changed`] — and by every rows
    /// change, because the order indexes the rows. A track change moves only
    /// this, so it costs the `upcoming` list and never the rows.
    order_revision: u64,
    emitted_order_revision: Option<u64>,
    /// Queue track ids found in the audio cache since the rows were last
    /// published. The rows held here are marked too; this list is how the
    /// mark reaches holders of the published rows without re-sending them.
    cached_marks: Vec<String>,
    /// How much of [`Self::cached_marks`] the last state carried.
    emitted_cached_marks: usize,
    uncached_marks: Vec<String>,
    invalidated_cache_ids: HashSet<String>,
    cache_evictions: HashMap<String, Arc<Mutex<bool>>>,
    cache_sender: Option<mpsc::UnboundedSender<CacheSignal>>,
    /// The track the latest load was for. Its audio is on disk by the time it
    /// is left, so the next track change asks the cache about it.
    loaded_track_id: Option<String>,
    /// The natural end of the queue left a tail draining through the output.
    /// Due, the player is stopped so the device stops calling back.
    drain_stop_at: Option<Instant>,
    player: Option<Arc<Player>>,
    mixer: Option<Arc<SoftMixer>>,
    session: Option<librespot_core::Session>,
    /// Construction in flight for this generation; deadlines do not authorize
    /// overlapping probes while its bounded worker result is still pending.
    audio_probe_generation: Option<u64>,
    play_request_id: Option<u64>,
    /// Set when librespot reports `Unavailable` for the current load. The
    /// player remains in librespot's failed `Loading` state unless it is
    /// explicitly stopped; this flag makes the next Play command issue a
    /// fresh load instead of sending `play` to a dead loader.
    loading_failed: bool,
    /// The queue/playhead is installed but no librespot load exists. This is
    /// set by restore and by session teardown, and consumed by the next Play.
    current_needs_load: bool,
    /// Whether the installed queue is a draft editor preview. Preview player
    /// events still update transport state, but never create listening-history
    /// rows.
    preview_mode: bool,
    /// Process-local owner of the installed editor preview. Zero means that
    /// no preview lease is active.
    preview_lease_id: u64,
    position_anchor: Option<(u32, Instant)>,
    /// Actual queued rate can lag the requested setting by the write-ahead.
    audible_speed: Option<f32>,
    audio_start_position_ms: u32,
    /// When the last command-driven track change (PlayQueue/Next/Previous)
    /// was dispatched, for pacing rapid presses (see
    /// [`TRACK_CHANGE_MIN_INTERVAL`]). `None` until the first change.
    last_track_change: Option<Instant>,
    /// Actual current-track load starts seen recently. This complements the
    /// 250 ms command pacing: when a failure arrives after a rapid sequence
    /// of loads, cache eviction is unsafe because the failure may be a
    /// clustered key/network rejection.
    recent_track_changes: VecDeque<Instant>,
    /// Current-track load failures seen recently. A later failure in the same
    /// window is considered transient even when the user did not change
    /// tracks between the failures. Its length is also the circuit breaker's
    /// count (see [`UNAVAILABLE_STOP_LIMIT`]) and, while it is non-empty, the
    /// reason next-track preloading is held back.
    recent_unavailable: VecDeque<Instant>,
    /// Whether the current load has been heard from — see
    /// [`AudioSignal::Output`]. Cleared by every load, set by the first packet
    /// the decoder produces for it. This is the engine's only truthful answer
    /// to "is this track actually playing", and the difference between a track
    /// that ended and a track that never started.
    current_load_produced_audio: bool,
    /// When the current row should be loaded once more after failing, and
    /// whether the load now in flight already is that retry. See
    /// [`LOAD_RETRY_BACKOFF`]. Both are cleared by any load, so a user action
    /// that moves the playhead cancels a pending retry by construction.
    retry_current_at: Option<Instant>,
    current_load_is_retry: bool,
    /// Whether librespot has said the current track is close enough to its end
    /// for the next one to be worth fetching. See [`Engine::preload_next`].
    preload_armed: bool,
    /// Earliest time an automatic reconnect may be attempted, and how long to
    /// wait after the next failure. `None` means "no reconnect is pending":
    /// the session is healthy, or one is already running. See
    /// [`Engine::tick_session_health`].
    next_reconnect: Option<Instant>,
    reconnect_backoff: Duration,
    /// Set when a cached-auth attempt failed on transport rather than refusal.
    /// It shuts its own session down, so there is no dead session for the
    /// health tick to find, and "no session" already means the pre-auth state.
    awaiting_transport_retry: bool,
    shuffle_pool: Vec<usize>,
    /// Queue navigation history used by Previous; local listening rows live
    /// in `listening_history`.
    history: Vec<usize>,
    listening_history: ListeningHistory,
    random_state: u64,
    generation: u64,
    /// The generation the installed player's events travel under. A
    /// preserved-playback reconnect moves `generation` on while keeping that
    /// player and commanding it, so its events, its end of track above all,
    /// must still be heard. `None` once it is detached.
    player_generation: Option<u64>,
    /// The customization revision expected by loop-boundary signals. Markers
    /// can remain queued in the audio callback after a track/config change, so
    /// delivery must be tied to the pipeline that produced them.
    audio_revision: u64,
    /// The decoder reached physical EOF while a loop boundary was still
    /// draining through the output queue. `Player::seek` is invalid in
    /// librespot's EndOfTrack state, so the audible marker reloads the same
    /// track instead.
    loop_decoder_eof: bool,
    /// The audible marker has requested a loop jump but librespot has not yet
    /// confirmed the new load/position. If EOF wins that race, reload instead
    /// of waiting for a marker that was already consumed.
    loop_jump_pending: bool,
    /// One-based audible pass through the current finite loop. Fresh loads
    /// and user seeks derive it from their source position: pass one before
    /// the loop end, and the final pass at or after it. Internal loop jumps
    /// preserve and increment this value.
    loop_pass: u32,
    /// An authentication attempt is in flight; no second one may start, and
    /// nothing may act on the session until it answers.
    auth_running: bool,
    /// The generation [`Engine::auth_running`] belongs to: the attempt's own
    /// generation, captured when it started. It is what lets a stale
    /// `Complete` tell "the attempt I was abandoned for" from "the attempt the
    /// engine is waiting on now" (see
    /// [`Engine::abandon_cached_authentication`]). Meaningless while
    /// `auth_running` is false.
    auth_generation: u64,
    /// Track-gain volume normalisation (attenuation-only, see
    /// `auth::player_config`). Shared with in-flight authentication so it can
    /// build the latest preference without reconnecting the session.
    normalisation: Arc<AtomicBool>,
    /// The prepared OAuth attempt whose authorize URL is published in
    /// `needs_login` state; `login` consumes it so the UI opens exactly the
    /// URL the flow listens for. Regenerated per attempt.
    pending_auth: Option<PendingAuth>,
    /// The output device every player this engine builds opens. Held rather
    /// than reached for, so a machine with no output device is a state a test
    /// can put the engine in (see [`audio::SinkOpener`]) — real hardware
    /// cannot be asked to boot without one.
    audio_device: audio::SinkOpener,
    /// Whether the machine has a default output device at all. Held for the
    /// same reason as [`Self::audio_device`], and asked before it: a machine
    /// with nothing plugged in must not build a mixer, a player thread and a
    /// failing open to learn what the device list already says.
    audio_device_present: audio::DevicePresence,
    /// Set while the machine has no output device to open. See
    /// [`AudioUnavailable`].
    audio_unavailable: Option<AudioUnavailable>,
    /// How long the next device probe waits after a failed open, doubling per
    /// failure up to the ceiling of the failure the last probe answered with
    /// (see [`PlayerFailure`]). Reset only once native output runs.
    audio_probe_backoff: Duration,
    /// Whether the OS reports default-output changes to this engine. Without
    /// them a machine with no device has to be asked again on a clock.
    device_notifications: bool,
    /// The transport volume the librespot cache does not hold yet. See
    /// [`VOLUME_PERSIST_QUIET`] for why the write is not paid per change.
    pending_volume: Option<PendingVolume>,
    /// Armed only while playback needs position/cache/session observation.
    next_active_tick: Option<Instant>,
}

/// A volume change that is not in the librespot cache yet, in librespot's u16
/// scale.
struct PendingVolume {
    /// The newest value. A drag overwrites this faster than any single write
    /// could land, and only the value it stops at is worth writing.
    volume: u16,
    /// When it last changed, which is what [`VOLUME_PERSIST_QUIET`] measures.
    changed_at: Instant,
}

/// The engine has no player, and the probe is what brings one back.
///
/// This is deliberately not an error *state*: `state.ready` stays true when
/// the session is live, and the only thing missing is the player. Whether the
/// machine has no output device, a device whose open failed, or a
/// reconstruction that failed outright, the answer is the same — the device is
/// a property of the machine that the user changes, so the engine asks again
/// when the OS reports a new default output, or on the backoff ladder while a
/// present device keeps refusing.
struct AudioUnavailable {
    /// What the user is shown: the cause and the way out.
    message: String,
    /// The next moment a probe may build a player, or `None` while there is
    /// no device and the OS notification is what will ask again. A probe
    /// already running holds this at the instant its own open must have
    /// finished by ([`AUDIO_START_TIMEOUT`]), so one runs at a time — and a
    /// probe whose answer is dropped, because the session was replaced while
    /// it ran, is still replaced by another instead of leaving the device
    /// untried for good.
    retry_at: Option<Instant>,
}

/// Why the engine has no player, which is what its retry ladder is sized
/// against.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlayerFailure {
    /// An ordinary failure to get a player: no device is there, one refused
    /// the open, or the construction itself failed. A short retry gets past
    /// this — a dongle Windows recognised late, a device another application
    /// is holding between tracks, a player thread that died on the way up.
    Ordinary,
    /// A device is there and opening it did not return within
    /// [`AUDIO_START_TIMEOUT`]: the driver is wedged, not the machine's device
    /// list. That open is still running on a librespot player thread, parked
    /// inside cpal for the rest of the process, so every further probe leaks
    /// one — and this answer is reached by *waiting* ten seconds, so retrying
    /// it on the ordinary ten-second ceiling would park a thread per probe
    /// forever, thousands a day, all of them waiting on the same driver.
    /// Backing off on a much longer ladder bounds the accumulation while still
    /// recovering without a restart if the driver wakes up or is replaced,
    /// which is the only way this condition ends.
    Blocked,
}

impl PlayerFailure {
    /// The longest wait this failure lets the ladder reach.
    fn ceiling(self) -> Duration {
        match self {
            Self::Ordinary => AUDIO_PROBE_BACKOFF_MAX,
            Self::Blocked => AUDIO_BLOCKED_PROBE_BACKOFF_MAX,
        }
    }
}

struct PlaybackState {
    ready: bool,
    auth_state: AuthState,
    /// OAuth authorize URL for the current/next login attempt; present in
    /// `needs_login` (and `authenticating`) state events. See
    /// [`StateEvent::auth_url`].
    auth_url: Option<String>,
    playing: bool,
    position_ms: u32,
    duration_ms: u32,
    volume: u8,
    shuffle: bool,
    repeat: RepeatMode,
    speeds: PlaybackSpeeds,
    current_index: Option<usize>,
    queue: Vec<TrackRef>,
    error: Option<String>,
}

impl PlaybackState {
    fn current_uri(&self) -> Option<&str> {
        self.current_index
            .and_then(|index| self.queue.get(index))
            .map(|track| track.uri.as_str())
    }

    /// The speed the current item plays at: its kind's. With no current item
    /// it is the songs' speed, which is what a speed change then edits.
    fn playback_speed(&self) -> f32 {
        self.speeds.of(self.current_uri())
    }
}

/// Playback speed, remembered apart for songs and for podcast episodes: a
/// talk show at 1.5× and music at 1× are both "the speed", and which one
/// applies is the engine's to decide, because only the engine sees the item
/// change — at a natural boundary in the queue as much as on a click. Every
/// load applies the speed of the item it loads (`configure_current_audio_*`),
/// and a speed change edits the current item's kind. The shell persists both.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlaybackSpeeds {
    track: f32,
    episode: f32,
}

impl PlaybackSpeeds {
    const NORMAL: Self = Self { track: 1.0, episode: 1.0 };

    fn of(self, uri: Option<&str>) -> f32 {
        if uri.is_some_and(|uri| uri.starts_with("spotify:episode:")) {
            self.episode
        } else {
            self.track
        }
    }
}

#[derive(Clone, Copy)]
enum PositionSpace {
    Source,
    Transport,
}

/// What one current-track load failure says about the run it belongs to. The
/// two answers are kept apart because they protect different things and
/// disagree on purpose: the second failure in a burst already suppresses cache
/// eviction, long before enough of them have piled up to stop playback.
struct UnavailableBurst {
    /// Whether cached audio for the failed track must be preserved: a
    /// clustered failure is evidence about the service, not about the file.
    clustered: bool,
    /// Failures in the current run, this one included. Reset by the first
    /// load that succeeds (see [`Engine::clear_unavailable_burst`]) and by the
    /// burst window expiring.
    consecutive: usize,
}

pub enum AuthSignal {
    Complete {
        generation: u64,
        result: Result<ConnectedSession, AuthFailure>,
    },
    PlayerRebuilt {
        generation: u64,
        normalisation: bool,
        result: Result<PlaybackHandles, PlaybackError>,
    },
}

pub enum PlayerSignal {
    Event { generation: u64, event: PlayerEvent },
    Closed { generation: u64 },
}

impl Engine {
    pub fn new(
        writer: ProtocolWriter,
        cache: Cache,
        temporary_directory: std::path::PathBuf,
        credentials_file: std::path::PathBuf,
        state_directory: std::path::PathBuf,
        normalisation: bool,
        audio_device: audio::SinkOpener,
        audio_device_present: audio::DevicePresence,
    ) -> Self {
        let history_root = state_directory.clone();
        let track_edits = TrackEditStore::load_or_empty(&state_directory);
        let random_state = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15)
            ^ u64::from(std::process::id());
        let volume = stored_volume_percent(&cache);
        Self {
            writer,
            cache,
            temporary_directory,
            credentials_file,
            track_edits,
            state: PlaybackState {
                ready: false,
                auth_state: AuthState::Authenticating,
                auth_url: None,
                playing: false,
                position_ms: 0,
                duration_ms: 0,
                volume,
                shuffle: false,
                repeat: RepeatMode::Off,
                speeds: PlaybackSpeeds::NORMAL,
                current_index: None,
                queue: Vec::new(),
                error: None,
            },
            player: None,
            mixer: None,
            session: None,
            play_request_id: None,
            rows_revision: initial_revision(),
            emitted_rows_revision: None,
            order_revision: initial_revision(),
            emitted_order_revision: None,
            cached_marks: Vec::new(),
            emitted_cached_marks: 0,
            uncached_marks: Vec::new(),
            invalidated_cache_ids: HashSet::new(),
            cache_evictions: HashMap::new(),
            cache_sender: None,
            loaded_track_id: None,
            drain_stop_at: None,
            audio_probe_generation: None,
            loading_failed: false,
            current_needs_load: false,
            preview_mode: false,
            preview_lease_id: 0,
            position_anchor: None,
            audible_speed: None,
            audio_start_position_ms: 0,
            last_track_change: None,
            recent_track_changes: VecDeque::new(),
            recent_unavailable: VecDeque::new(),
            current_load_produced_audio: false,
            retry_current_at: None,
            current_load_is_retry: false,
            preload_armed: false,
            next_reconnect: None,
            reconnect_backoff: RECONNECT_BACKOFF_MIN,
            awaiting_transport_retry: false,
            shuffle_pool: Vec::new(),
            history: Vec::new(),
            listening_history: ListeningHistory::new(history_root),
            random_state,
            generation: 0,
            player_generation: None,
            // No marker produced before this engine was constructed belongs
            // to its first queue, even when the process-wide audio revision
            // was already advanced by an earlier player instance.
            audio_revision: crate::audio::customization_revision().wrapping_add(1),
            loop_decoder_eof: false,
            loop_jump_pending: false,
            loop_pass: 1,
            auth_running: false,
            auth_generation: 0,
            normalisation: Arc::new(AtomicBool::new(normalisation)),
            pending_auth: None,
            audio_device,
            audio_device_present,
            audio_unavailable: None,
            audio_probe_backoff: AUDIO_PROBE_BACKOFF_MIN,
            device_notifications: false,
            pending_volume: None,
            next_active_tick: None,
        }
    }

    /// Records that the OS will report default-output changes, so a machine
    /// with no output device is not asked again on a clock.
    pub fn set_device_notifications(&mut self, available: bool) {
        self.device_notifications = available;
    }

    /// A handle that pages the listening archive off the command loop.
    pub fn history_reader(&self) -> crate::history::HistoryReader {
        self.listening_history.reader()
    }

    pub fn clear_history(&mut self) -> Result<bool, String> {
        self.listening_history.clear()?;
        Ok(true)
    }
    pub fn writer(&self) -> &ProtocolWriter {
        &self.writer
    }
    pub fn track_edit_status(&self, track_id: &str, playlist_id: Option<&str>) -> TrackEditStatus {
        self.track_edits.status(track_id, playlist_id)
    }

    pub fn save_track_edit(
        &mut self,
        track_id: String,
        duration_ms: u32,
        cuts: Vec<TimeRange>,
        loop_range: Option<LoopRange>,
    ) -> Result<TrackEditDefinition, String> {
        self.track_edits
            .save_definition(track_id, duration_ms, cuts, loop_range)
    }

    pub fn delete_track_edit(&mut self, track_id: &str) -> Result<(), String> {
        self.track_edits.delete_definition(track_id)
    }

    pub fn set_playlist_track_edit_enabled(
        &mut self,
        playlist_id: &str,
        track_id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.track_edits.set_enabled(playlist_id, track_id, enabled)
    }
    pub fn playlist_excluded_track_ids(&self, playlist_id: &str) -> Result<Vec<String>, String> {
        self.track_edits.list_excluded_track_ids(playlist_id)
    }

    pub fn queue_metadata(&self, context: Option<&str>) -> Result<renderer_engine::protocol::QueueMetadata, String> {
        let context = context.unwrap_or_else(|| self.current_context());
        let excluded_track_ids = match context.strip_prefix("playlist:") {
            Some(id) => self.playlist_excluded_track_ids(id)?,
            None => Vec::new(),
        };
        Ok(renderer_engine::protocol::QueueMetadata {
            excluded_track_ids,
            upcoming: self.upcoming_indices(),
        })
    }

    fn current_context(&self) -> &str {
        self.state.current_index.and_then(|index| self.state.queue.get(index))
            .map_or("", |track| track.context.as_str())
    }

    pub fn set_playlist_track_excluded(
        &mut self,
        playlist_id: &str,
        track_id: &str,
        excluded: bool,
    ) -> Result<(), String> {
        self.track_edits
            .set_excluded(playlist_id, track_id, excluded)?;
        // Exclusion changes never stop or reload the current track. They do
        // change every future choice, including a pending shuffle/preload
        // decision, so fix up those cheap plans immediately.
        self.repair_shuffle_pool_for_eligibility();
        self.preload_next();
        Ok(())
    }

    /// Marks the queue rows as moved: an install, an append, a removal or a
    /// move. The play order indexes the rows, so it moves with them.
    fn rows_changed(&mut self) {
        self.rows_revision = self.rows_revision.wrapping_add(1);
        self.order_changed();
    }

    /// Marks the play order as moved without touching the rows: the current
    /// row, the shuffle bag, the shuffle/repeat modes, or an exclusion that
    /// automatic playback respects.
    fn order_changed(&mut self) {
        self.order_revision = self.order_revision.wrapping_add(1);
    }

    /// Makes the next state carry the rows, the order and the download marks
    /// whatever was published before — what a `status` request asks for.
    fn republish_queue(&mut self) {
        self.emitted_rows_revision = None;
        self.emitted_order_revision = None;
        self.emitted_cached_marks = 0;
    }

    /// Emits the full state. The rows travel only under a rows revision this
    /// process has not handed over, the `upcoming` order only under a new
    /// order revision, and the download marks only when they grew.
    ///
    /// The shell and the webview keep the rows they hold across the several
    /// states of one track change (that is what preserves row identity, and
    /// with it every consumer keyed on a row object), so re-sending identical
    /// rows would be serializing a megabyte of queue to say nothing. A track
    /// change moves the order alone, which costs a list of indexes. What the
    /// revisions cannot express — the playhead, the modes, the current row,
    /// the track — still travels in every state.
    pub fn emit_state(&mut self) -> Result<(), String> {
        let current_uri = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .map(|track| track.uri.as_str());
        let (position_ms, duration_ms) = self.transport_position_and_duration();
        let queue_revision = self.rows_revision;
        let order_revision = self.order_revision;
        let sending_rows = self.emitted_rows_revision != Some(queue_revision);
        let sending_order = sending_rows || self.emitted_order_revision != Some(order_revision);
        let sending_marks = self.cached_marks.len() != self.emitted_cached_marks
            || (sending_rows && !self.cached_marks.is_empty());
        let queue = sending_rows.then_some(self.state.queue.as_slice());
        let upcoming = sending_order.then(|| self.upcoming_indices());
        let cached_ids = sending_marks.then_some(self.cached_marks.as_slice());
        let uncached_ids = (!self.uncached_marks.is_empty()).then_some(self.uncached_marks.as_slice());
        let preload_uri = self.preload_row().map(|row| row.uri.as_str());
        self.writer.send(&StateEvent {
            kind: "state",
            ready: self.state.ready,
            auth_state: self.state.auth_state,
            auth_url: self.state.auth_url.as_deref(),
            playing: self.state.playing,
            // Intent is playing but no packet has come out of the decoder for
            // this load yet, or the load has failed and the row is being
            // retried. Both are the same thing to the listener: the playhead in
            // this message is where playback will start, not a position
            // anything has reached, and the UI must therefore hold its local
            // projection still instead of counting on from it. A paused row is
            // not buffering — its position is simply frozen, which the UI
            // already handles from `playing`.
            buffering: self.state.playing
                && (self.loading_failed || !self.current_load_produced_audio),
            preview: self.preview_mode,
            username: self.session.as_ref().map(|session| session.username()),
            position_ms,
            duration_ms,
            volume: self.state.volume,
            shuffle: self.state.shuffle,
            repeat: self.state.repeat,
            playback_speed: self.state.playback_speed(),
            track_speed: self.state.speeds.track,
            episode_speed: self.state.speeds.episode,
            audible_playback_speed: self.audible_speed.unwrap_or(self.state.playback_speed()),
            current_index: self.state.current_index,
            current_uri,
            context: self.current_context(),
            queue,
            queue_revision,
            order_revision,
            upcoming,
            preload_uri,
            cached_ids,
            uncached_ids,
            error: self.state.error.as_deref(),
        })?;
        self.emitted_rows_revision = Some(queue_revision);
        self.emitted_order_revision = Some(order_revision);
        if sending_rows {
            // The rows just sent carry every mark this list held.
            self.cached_marks.clear();
        }
        self.emitted_cached_marks = self.cached_marks.len();
        self.uncached_marks.clear();
        Ok(())
    }

    /// Serializes only the playhead scalars the frontend projects and clamps
    /// against — never the queue — for the 2-second position heartbeat.
    /// [`Engine::emit_state`] stays reserved for real changes (track, queue,
    /// shuffle, repeat, duration, play/pause), and volume has a scalar lane of
    /// its own ([`Engine::emit_volume`]), so the steady-state heartbeat cost is
    /// O(1) in queue length. The heartbeat is emitted only while playing:
    /// paused positions are static and project from the last full state.
    pub fn emit_position(&self) -> Result<(), String> {
        let (position_ms, duration_ms) = self.transport_position_and_duration();
        self.writer.send(&PositionEvent {
            kind: "position",
            position_ms,
            duration_ms,
        })
    }

    /// Serializes the one scalar a volume change moved — never the queue —
    /// and is what makes the command answer `false` instead of asking the loop
    /// for a full state.
    ///
    /// Volume travels on its own lane for the same reason the playhead does:
    /// the UI paces `set_volume` at one command per 50 ms while a slider is
    /// dragged, and a full state per step would serialize the queue and its
    /// computed upcoming set twenty times a second to say one number.
    pub fn emit_volume(&self) -> Result<(), String> {
        self.writer.send(&VolumeEvent {
            kind: "volume",
            volume: self.state.volume,
        })
    }

    /// Writes a volume that has stopped changing to the librespot cache.
    ///
    /// The next-due timer pays a drag's cache write once, a second after
    /// the value stops moving. Player construction and shutdown also flush it.
    pub fn tick_volume_persist(&mut self) {
        let Some(pending) = self.pending_volume.as_ref() else {
            return;
        };
        if pending.changed_at.elapsed() >= VOLUME_PERSIST_QUIET {
            self.flush_pending_volume();
        }
    }

    /// Hands the volume the cache is missing to librespot, which reports its
    /// own write failures (`Cache::save_volume` returns nothing else). Called
    /// by the work timer and by [`Engine::shutdown`].
    fn flush_pending_volume(&mut self) {
        if let Some(pending) = self.pending_volume.take() {
            self.cache.save_volume(pending.volume);
        }
    }

    /// The command loop owns one sleep for real work, not an idle interval.
    /// A player meant to be playing still observes silent session failures and
    /// the playhead every two seconds. Play intended over nothing audible has
    /// nothing to observe: with no output device the probe deadline or the OS
    /// notification wakes the loop, and a reconnect answers on its own
    /// deadline. A preserved player still sounding through one keeps its lane.
    pub fn next_work_deadline(&mut self) -> Option<Instant> {
        let reconnecting = self.auth_running || self.awaiting_transport_retry;
        let active = self.state.playing && self.session.is_some() && self.player.is_some()
            && (!reconnecting || self.current_load_produced_audio && !self.loading_failed);
        if active {
            self.next_active_tick.get_or_insert_with(|| Instant::now() + Duration::from_secs(2));
        } else {
            self.next_active_tick = None;
        }
        let volume = self.pending_volume.as_ref()
            .map(|pending| pending.changed_at + VOLUME_PERSIST_QUIET);
        let reconnect = (!self.auth_running).then_some(self.next_reconnect).flatten();
        let audio = self.session.as_ref().and(self.audio_unavailable.as_ref())
            .filter(|_| self.player.is_none())
            .filter(|_| self.audio_probe_generation != Some(self.generation))
            .and_then(|unavailable| unavailable.retry_at);
        let retry = self.state.playing.then_some(self.retry_current_at).flatten();
        let preload = if active && self.current_load_produced_audio && !self.loading_failed
            && !self.preload_armed
        {
            self.tick_position();
            let (position, duration) = self.transport_position_and_duration();
            (duration > 0).then(|| {
                let remaining = duration.saturating_sub(position)
                    .saturating_sub(PRELOAD_WATERMARK_MS.min(duration / 2));
                Instant::now() + Duration::from_secs_f64(
                    f64::from(remaining) / (1_000.0 * f64::from(self.audible_speed.unwrap_or(self.state.playback_speed())))
                )
            })
        } else {
            None
        };
        [self.next_active_tick, volume, reconnect, audio, retry, preload, self.drain_stop_at]
            .into_iter().flatten().min()
    }

    /// Stops the player once the tail the natural end of the queue left in the
    /// output has been heard. librespot leaves a finished queue in its
    /// end-of-track state with the sink running, so without this the device
    /// would keep calling back over an empty ring for as long as the app ran.
    ///
    /// The deadline is taken once, when the decoder reaches the end: nothing
    /// can be queued behind the tail after that, and a device that stopped
    /// consuming must not keep this re-arming at rest.
    pub fn tick_output_drain(&mut self) {
        if self.drain_stop_at.is_none_or(|due| Instant::now() < due) {
            return;
        }
        self.drain_stop_at = None;
        if let Some(player) = &self.player {
            player.stop();
        }
        self.invalidate_audio_signals();
    }

    pub fn take_active_tick(&mut self) -> bool {
        if self.next_active_tick.is_some_and(|due| Instant::now() >= due) {
            self.next_active_tick = None;
            true
        } else {
            false
        }
    }

    /// The cache a player construction about to be started will read the
    /// transport volume from, with any pending write flushed first.
    ///
    /// The deferred write is at most a couple of seconds behind (see
    /// [`VOLUME_PERSIST_QUIET`]), and a construction that read the older value
    /// would adopt it: [`Engine::on_auth_signal`] takes a fresh player's
    /// `volume_percent` into `state.volume` for a session with no queue, and
    /// the rebuild paths adopt `state.volume` as it stands. Flushing here
    /// means a new player always starts from the volume the user had set when
    /// the construction began, however recently they set it.
    fn construction_cache(&mut self) -> Cache {
        self.flush_pending_volume();
        self.cache.clone()
    }

    fn current_timeline(&self) -> EditTimeline<'_> {
        let cuts = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.as_ref())
            .map(|edit| edit.cuts.as_slice())
            .unwrap_or_default();
        EditTimeline::new(self.state.duration_ms, cuts)
    }

    fn transport_position_and_duration(&self) -> (u32, u32) {
        let timeline = self.current_timeline();
        (
            timeline.source_to_compiled(self.state.position_ms),
            timeline.compiled_duration_ms(),
        )
    }

    fn transport_to_source(&self, position_ms: u32) -> u32 {
        self.current_timeline().compiled_to_source(position_ms)
    }

    fn update_transport_position(&mut self, position_ms: u32) {
        let source_position_ms = self.transport_to_source(position_ms);
        self.update_position(source_position_ms);
    }

    /// Records an authoritative playback position and re-anchors the drift
    /// projection at the current wall clock. Called on every player event and
    /// command that establishes a position (track change, play/pause, seek,
    /// position correction) so `tick_position` projects from the newest truth.
    fn update_position(&mut self, position_ms: u32) {
        self.state.position_ms = position_ms.min(self.state.duration_ms);
        self.position_anchor = Some((self.state.position_ms, Instant::now()));
    }

    fn audible_position(&self) -> Option<u32> {
        let elapsed = crate::audio::audible_elapsed_ms(self.audio_revision)?;
        Some(self.source_position_after_audio(elapsed))
    }

    fn source_position_after_audio(&self, elapsed: f64) -> u32 {
        let timeline = self.current_timeline();
        let start = timeline.source_to_compiled(self.audio_start_position_ms);
        timeline.compiled_to_source(
            start.saturating_add(elapsed.round().min(f64::from(u32::MAX)) as u32)
                .min(timeline.compiled_duration_ms()),
        )
    }

    fn update_player_position(&mut self, position_ms: u32) {
        // librespot reports the decoder head, up to a ring plus WSOLA
        // lookahead ahead of the speaker. Corrections must use the consumer's
        // clock when it exists, not pull audible playback toward decode-ahead.
        self.update_position(self.audible_position().unwrap_or(position_ms));
    }

    fn prepare_audio_resume(&mut self) -> Option<librespot_playback::player::DecoderTransition> {
        if !self.state.playing && self.current_load_produced_audio
            && self.audible_position().is_none() && !crate::audio::customization_pending(self.audio_revision)
        {
            // An unsolicited stop/device loss may have discarded output. Its
            // decoder-paused offset is the next real packet's origin. Normal
            // user pauses retain a consumer clock and never rebuild here.
            self.configure_current_audio_at_loop_pass(self.state.position_ms, self.loop_pass);
            return Some(crate::audio::decoder_transition(self.audio_revision, true));
        }
        None
    }

    /// Advances the reported position from the latest anchor and reports
    /// whether a position heartbeat should be emitted (at most once per
    /// call). While paused the position is static and no heartbeat is
    /// produced: the frontend projects the frozen position from the last
    /// full state.
    ///
    /// A load that has produced no audio yet is the same case for the same
    /// reason, and it is the one that matters most: nothing is playing until
    /// the decoder has handed the pipeline its first packet, so nothing may
    /// advance the playhead. The projection is what the retry, a later Play,
    /// and every reported position read, and it clamps at the row's own
    /// duration — so a load that fails silently (a refused audio key, a
    /// truncated fetch) had already been walked to the end of a short row by
    /// the time the failure arrived. The engine then called that a finished
    /// track, or retried the row by asking librespot to load it at its own
    /// end: no audio either time, a second failure the row never earned, and a
    /// healthy one-second track skipped as unavailable. A run of them is
    /// exactly the queue that appeared to march through itself in silence.
    /// Freezing until the first packet keeps the projection on the last thing
    /// that was actually true — the position the load was asked for — and
    /// [`Engine::on_audio_signal`] re-anchors it there when audio really
    /// starts.
    ///
    /// A load that *failed* stays frozen on the same principle: the row is not
    /// playing, and for a decoder that died mid-track the frozen position is
    /// the point it died at, which is where the retry should resume.
    pub fn tick_position(&mut self) -> bool {
        self.tick_position_at(Instant::now())
    }

    fn tick_position_at(&mut self, at: Instant) -> bool {
        if !self.state.playing || self.loading_failed || !self.current_load_produced_audio {
            return false;
        }
        if let Some(position) = self.audible_position() {
            self.state.position_ms = position;
            return true;
        }
        let Some((anchor_position_ms, anchor_time)) = self.position_anchor else {
            return false;
        };
        let elapsed_ms =
            (at.saturating_duration_since(anchor_time).as_secs_f64() * 1_000.0
                * f64::from(self.audible_speed.unwrap_or(self.state.playback_speed())))
                .round()
                .min(f64::from(u32::MAX)) as u32;
        let source_position_ms = {
            let timeline = self.current_timeline();
            let compiled_anchor_ms = timeline.source_to_compiled(anchor_position_ms);
            let compiled_position_ms = compiled_anchor_ms
                .saturating_add(elapsed_ms)
                .min(timeline.compiled_duration_ms());
            timeline.compiled_to_source(compiled_position_ms)
        };
        self.state.position_ms = source_position_ms;
        true
    }

    /// Asks the audio cache about the tracks a track change can have finished
    /// downloading: the one being left (librespot stores a track once its
    /// download completes, which a track played for any length has) and the
    /// one starting (a gapless preload may already have stored it).
    ///
    /// Two lookups per track change, each a path join and one file-attribute
    /// call — never on a clock, never a walk of the queue's files. A new mark
    /// flips the rows held here and joins [`Engine::cached_marks`], which the
    /// next state carries without re-sending the rows.
    fn mark_cached_tracks(&mut self, outgoing: Option<String>) {
        let current = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .filter(|track| track.uri.starts_with("spotify:track:"))
            .map(|track| track.id.clone());
        let candidates: Vec<String> = outgoing
            .into_iter()
            .chain(current)
            .filter(|id| !self.cached_marks.contains(id))
            .filter(|id| !self.invalidated_cache_ids.contains(id))
            .filter(|id| !self.state.queue.iter().any(|track| track.id == *id && track.cached))
            .collect();
        if candidates.is_empty() {
            return;
        }
        for id in crate::browse::cached_track_ids(&candidates, Some(&self.cache)) {
            for track in &mut self.state.queue {
                if track.id == id && track.uri.starts_with("spotify:track:") {
                    track.cached = true;
                }
            }
            if !self.cached_marks.contains(&id) {
                self.cached_marks.push(id);
            }
        }
    }

    /// Notices a session librespot has invalidated underneath us and rebuilds
    /// it. Asked before every command, and by the work timer while playing;
    /// the check is one `RwLock` read, and nothing polls it at rest.
    ///
    /// Returns whether the engine's state changed and should be emitted.
    ///
    /// The failure this recovers from is silent by construction — see
    /// [`RECONNECT_BACKOFF_MIN`]. The first reconnect starts the moment the
    /// corpse is seen, because whoever saw it is usually a command waiting on
    /// it: a session that died during sleep is found by the first Play after
    /// waking, and that Play is recorded as the reconnect's resume intent (see
    /// [`Engine::runs_during_reconnect`]) instead of being refused. Later
    /// attempts after a failed one wait out the backoff. An active player
    /// remains logically playing during the reconnect so its projected
    /// playhead and listening row reach the handover without an artificial
    /// gap.
    fn prepare_session_reconnect(&mut self) -> Option<bool> {
        if self.auth_running {
            // An attempt is in flight; it clears this when it answers. An
            // answer that arrives stale hands the flag back through
            // [`Engine::abandon_cached_authentication`], so this guard can
            // never be the reason reconnecting stops happening.
            return None;
        }
        let dead = self
            .session
            .as_ref()
            .is_some_and(librespot_core::Session::is_invalid);
        // A cached-auth attempt that failed on transport shut its session down
        // and left only the armed retry, so there is no corpse to notice. That
        // is the same outage as a dead session and recovers the same way.
        let retry_armed = self.awaiting_transport_retry && self.next_reconnect.is_some();
        if !dead && !retry_armed {
            // A healthy session ends any backoff a previous outage built up.
            self.next_reconnect = None;
            self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
            return None;
        }

        let now = Instant::now();
        if let Some(due) = self.next_reconnect {
            if now < due {
                return None;
            }
            self.reconnect_backoff = (self.reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
        } else {
            eprintln!("Spotify session went invalid; reconnecting");
        }
        self.next_reconnect = Some(now + self.reconnect_backoff);
        // A playing queue keeps its old player until replacement handles
        // exist. The latest command intent remains in state.playing.
        Some(dead)
    }

    pub fn tick_session_health(&mut self, sender: &mpsc::UnboundedSender<AuthSignal>) -> bool {
        let Some(dead) = self.prepare_session_reconnect() else {
            return false;
        };
        let preserve = self.state.playing && self.state.current_index.is_some();
        self.start_cached_authentication(sender.clone(), preserve);
        if dead && self.auth_running {
            self.state.error = Some("the Spotify connection dropped; reconnecting".to_owned());
        }
        true
    }

    /// Cached reconnect accepts transport intent immediately. Queue selection
    /// installs the latest target, while loading waits for the live session.
    fn runs_during_reconnect(&self, command: &Command) -> bool {
        (self.auth_running || self.awaiting_transport_retry)
            && matches!(command, Command::Play | Command::Pause
                | Command::PlayQueue { .. } | Command::PlayQueueIndex { .. }
                | Command::RestoreQueue { .. } | Command::SetQueueCursor { .. })
    }

    /// The system changed its default render endpoint while a player was
    /// installed. Stop the old stream and make its in-flight events/rebuilds
    /// stale, then let the existing device probe open the new default.
    ///
    /// Preserve the projected playhead, queue and play/pause intent. The caller
    /// probes immediately; a failed open of the replacement keeps the usual
    /// backoff. A second change while that open is in flight invalidates its
    /// generation and starts a probe for the newer default.
    pub fn on_default_output_changed(&mut self) -> bool {
        if !self.state.ready || self.session.is_none() {
            return false;
        }
        // detach_player captures the clock before stopping the old pipeline.
        self.enter_audio_unavailable(
            "the default audio output changed; switching playback".to_owned(),
            PlayerFailure::Ordinary,
        );
        if let Some(unavailable) = self.audio_unavailable.as_mut() {
            unavailable.retry_at = Some(Instant::now());
        }
        true
    }

    /// Probes for an output device while the engine has no player, once the
    /// probe's deadline is due.
    ///
    /// Returns whether the engine's state changed and should be emitted.
    ///
    /// A probe starts with the cheap question — is there a default output
    /// device at all — because the construction behind it is not cheap: a
    /// mixer, a librespot player thread and a cpal open. With nothing plugged
    /// in the device list already answers no, so the construction is skipped
    /// entirely and no clock is armed: the OS reports the default output
    /// appearing ([`Engine::on_default_output_changed`]), which is what asks
    /// again. Only an engine without those notifications keeps asking on the
    /// ladder. The full construction runs once, when a device is really there.
    /// The rebuild it starts is the same one a normalisation change performs,
    /// and the only unusual thing about it is that it runs while a session is
    /// already up. Its own answer re-arms the clock — success clears the
    /// state, failure reschedules with the next backoff.
    pub fn tick_audio_device(&mut self, sender: &mpsc::UnboundedSender<AuthSignal>) -> bool {
        if self.audio_probe_generation == Some(self.generation) {
            return false;
        }
        if self.player.is_some() {
            // A lazy recovery player owns the next attempt at actual playback.
            return false;
        }
        let Some(unavailable) = self.audio_unavailable.as_ref() else {
            return false;
        };
        if unavailable.retry_at.is_none_or(|due| Instant::now() < due) {
            return false;
        }
        let Some(session) = self.session.clone() else {
            // Nothing to build a player against; the session's own retry
            // decides when that changes.
            return false;
        };
        // One enumeration of the device list, and no construction behind it
        // when it says nothing is there.
        if !(self.audio_device_present)() {
            let retry_at = (!self.device_notifications)
                .then(|| self.next_device_probe(PlayerFailure::Ordinary));
            if let Some(unavailable) = self.audio_unavailable.as_mut() {
                unavailable.retry_at = retry_at;
            }
            return false;
        }
        // The probe now owns the clock for as long as its open may take, which
        // is what keeps a second one from starting beside it.
        if let Some(unavailable) = self.audio_unavailable.as_mut() {
            unavailable.retry_at = Some(Instant::now() + AUDIO_START_TIMEOUT);
        }
        let generation = self.generation;
        self.audio_probe_generation = Some(generation);
        let cache = self.construction_cache();
        let normalisation = Arc::clone(&self.normalisation);
        let audio = Arc::clone(&self.audio_device);
        let sender = sender.clone();
        tokio::spawn(async move {
            let enabled = normalisation.load(Ordering::Acquire);
            let result = create_playback(session, cache, enabled, audio).await;
            let _ = sender.send(AuthSignal::PlayerRebuilt {
                generation,
                normalisation: enabled,
                result,
            });
        });
        false
    }

    /// Arms the next probe at the current backoff, then lengthens the ladder.
    ///
    /// A miss from the cheap device check and a construction that came back
    /// without a player are the same news to this clock — the machine has no
    /// audio *yet* — and both return the deadline the caller stores. What
    /// separates them is the ceiling: an ordinary failure may try again in ten
    /// seconds, an open that never returned may not (see
    /// [`PlayerFailure::Blocked`]). The ladder is clamped to that ceiling
    /// *before* it is used, not only before it is stored: a wedged driver walks
    /// the backoff up to five minutes, and the device that turns up missing or
    /// refusing afterwards must not inherit that wait — otherwise the ordinary
    /// failure that follows a blocked episode retries on the blocked clock,
    /// which is the one thing the separate ceilings exist to prevent.
    fn next_device_probe(&mut self, failure: PlayerFailure) -> Instant {
        let ceiling = failure.ceiling();
        let wait = self.audio_probe_backoff.min(ceiling);
        self.audio_probe_backoff = (wait * 2).min(ceiling);
        Instant::now() + wait
    }

    /// Enters the recoverable no-player state: no player, the session
    /// untouched, and the queue and playhead exactly as they were.
    ///
    /// Playback intent survives — `state.playing` is deliberately not touched —
    /// so a track that was playing when the player went away plays again when
    /// one comes back, which is the whole point of not treating this as a
    /// failure of the session.
    ///
    /// `failure` is why the player could not be had (see [`PlayerFailure`]), and
    /// it sizes the retry ladder the next probe runs on.
    fn enter_audio_unavailable(&mut self, message: String, failure: PlayerFailure) {
        self.detach_player();
        // Detaching already retired the player's own events. A rebuild or
        // probe answer still in flight was started for the player that just
        // failed, so the generation moves on to retire those too.
        self.generation = self.generation.wrapping_add(1);
        self.pause_listening();
        // Only the transition is log news. Every failed probe lands here
        // again, and a machine that has been without audio since boot used to
        // write one line per probe — thousands a day at the ceiling, into a log
        // that rotates at 4 MiB — eating the history that would explain the
        // outages worth reading. The message itself is not lost: it is
        // `state.error`, which every state event carries and the UI shows.
        if audio_unavailable_is_news(
            self.audio_unavailable.as_ref().map(|previous| previous.message.as_str()),
            &message,
        ) {
            eprintln!("audio: {message}");
        }
        self.state.error = Some(message.clone());
        let retry_at = Some(self.next_device_probe(failure));
        self.audio_unavailable = Some(AudioUnavailable { message, retry_at });
    }

    /// Releases the outage and its message once a replacement player is
    /// installed. The probe ladder is reset separately, only when a callback
    /// proves the device answers (`OutputReady`): a lazy player whose open
    /// then fails must not retry from the shortest rung.
    fn clear_audio_unavailable(&mut self) {
        if self.audio_unavailable.take().is_some() {
            self.state.error = None;
        }
    }

    /// Whether `command` can be applied while the machine has no output device.
    ///
    /// The queue, the volume, the shuffle and repeat modes, the speeds and the
    /// play/pause intent are engine state that a player is built *from*;
    /// everything else needs a player to act on and is answered with the
    /// device's own message instead.
    /// Refusing these too would leave a machine that booted without audio with
    /// a player bar that cannot be filled and a startup restore the shell
    /// reports as failed — for a device that may appear a second later.
    ///
    /// `state.ready` is part of the question rather than an oversight: the
    /// exemption is from the *device* check, not from the session's. An engine
    /// without a session has nothing to fill in and answers with the reason it
    /// has no session.
    fn runs_without_output_device(&self, command: &Command) -> bool {
        self.state.ready
            && self.audio_unavailable.is_some()
            && matches!(
                command,
                Command::Play
                    | Command::Pause
                    | Command::SetVolume { .. }
                    | Command::SetShuffle { .. }
                    | Command::SetRepeat { .. }
                    | Command::SetPlaybackSpeed { .. }
                    | Command::SetPlaybackSpeeds { .. }
                    | Command::RestoreQueue { .. }
                    | Command::SetQueueCursor { .. }
            )
    }

    /// Clears the last error, unless the machine has no output device: nothing
    /// a command does brings the audio back, so a command that succeeds while
    /// it is in force has not made the silence go away. The message is released
    /// where the condition is — by the player that replaces the missing one, in
    /// [`Engine::clear_audio_unavailable`].
    fn clear_error(&mut self) {
        if self.state.ready && self.audio_unavailable.is_none() {
            self.state.error = None;
        }
    }

    /// Enables or disables track-gain volume normalisation.
    ///
    /// A live change constructs only a new player against the existing
    /// authenticated session. While playback is unavailable, the atomic
    /// preference is consumed by the authentication already in flight.
    pub fn set_normalisation(
        &mut self,
        enabled: bool,
        sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> bool {
        if self.normalisation.swap(enabled, Ordering::AcqRel) == enabled {
            return false;
        }
        if !self.state.ready || self.player.is_none() {
            return true;
        }
        let Some(session) = self.session.clone() else {
            return true;
        };
        let cache = self.construction_cache();
        let sender = sender.clone();
        let generation = self.generation;
        let audio = Arc::clone(&self.audio_device);
        tokio::spawn(async move {
            let result = create_playback(session, cache, enabled, audio).await;
            let _ = sender.send(AuthSignal::PlayerRebuilt {
                generation,
                normalisation: enabled,
                result,
            });
        });
        true
    }

    pub fn start_authentication(&mut self, sender: mpsc::UnboundedSender<AuthSignal>) {
        self.start_cached_authentication(sender, false);
    }

    fn start_cached_authentication(
        &mut self,
        sender: mpsc::UnboundedSender<AuthSignal>,
        preserve_active_playback: bool,
    ) {
        if self.auth_running {
            return;
        }
        let generation = self.begin_cached_authentication(preserve_active_playback);
        if self.cache.credentials().is_none() {
            // No cached credentials: wait for an explicit login command so the
            // UI can present the authorize URL (a browser flow must never be
            // started behind the user's back or left waiting unattended).
            self.auth_running = false;
            self.enter_needs_login();
            return;
        }
        let cache = self.construction_cache();
        let temporary_directory = self.temporary_directory.clone();
        let normalisation = Arc::clone(&self.normalisation);
        let audio = Arc::clone(&self.audio_device);
        tokio::spawn(async move {
            let result = connect_cached(cache, temporary_directory, normalisation, audio).await;
            let _ = sender.send(AuthSignal::Complete { generation, result });
        });
    }

    /// Applies the synchronous half of cached authentication. Split from the
    /// task spawn so the two teardown modes remain an explicit state
    /// transition and can be tested without connecting to Spotify.
    fn begin_cached_authentication(&mut self, preserve_active_playback: bool) -> u64 {
        self.cancel_cache_evictions();
        if !preserve_active_playback {
            self.shutdown_playback();
        }
        self.auth_running = true;
        self.generation = self.generation.wrapping_add(1);
        // The attempt answers under the generation it started with, and that
        // is what identifies it to a stale-answer check later.
        self.auth_generation = self.generation;
        self.state.ready = false;
        self.state.auth_state = AuthState::Authenticating;
        self.state.error = None;
        self.generation
    }

    /// Transitions to `NeedsLogin`: tears down playback, clears the playback
    /// state, and prepares a fresh OAuth attempt whose authorize URL is
    /// published in state events for the UI's Log in button. The URL is
    /// regenerated every time the engine enters this state.
    fn enter_needs_login(&mut self) {
        self.shutdown_playback();
        self.state.ready = false;
        self.state.auth_state = AuthState::NeedsLogin;
        self.state.playing = false;
        self.state.position_ms = 0;
        self.state.duration_ms = 0;
        self.state.current_index = None;
        self.state.queue.clear();
        self.rows_changed();
        self.current_needs_load = false;
        self.preview_mode = false;
        self.preview_lease_id = 0;
        self.next_reconnect = None;
        self.awaiting_transport_retry = false;
        // The device state belongs to a live session: a machine that is being
        // asked to log in again is not also missing its output device as far as
        // the user or the next connection is concerned.
        self.audio_unavailable = None;
        self.audio_probe_backoff = AUDIO_PROBE_BACKOFF_MIN;
        self.shuffle_pool.clear();
        self.history.clear();
        self.state.error = None;
        match prepare_oauth() {
            Ok(pending) => {
                self.state.auth_url = Some(pending.auth_url.clone());
                self.pending_auth = Some(pending);
            }
            Err(error) => {
                self.state.auth_url = None;
                self.state.error = Some(error);
            }
        }
    }

    /// Starts the OAuth flow on demand, consuming the prepared attempt so the
    /// flow listens for exactly the authorize URL the UI opened. No-op while a
    /// session is live (`Ready`) or a flow is already running.
    ///
    /// Returning `Ok` is a promise the caller relies on: the loopback callback
    /// port is bound by the time this returns, so the UI may open the browser
    /// the moment the command answers. Nothing about that ordering is left to
    /// the spawned task — it is handed a listener that is already listening.
    pub fn login(
        &mut self,
        auth_sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> Result<bool, String> {
        if self.state.auth_state == AuthState::Ready {
            return Ok(true);
        }
        if self.auth_running {
            return Ok(true);
        }
        let (pending, listener) = self.begin_login_flow()?;
        let generation = self.generation;
        let auth_sender = auth_sender.clone();
        let cache = self.construction_cache();
        let temporary_directory = self.temporary_directory.clone();
        let normalisation = Arc::clone(&self.normalisation);
        let audio = Arc::clone(&self.audio_device);
        tokio::spawn(async move {
            let result = complete_oauth(
                cache,
                temporary_directory,
                pending,
                listener,
                normalisation,
                audio,
            )
            .await;
            let _ = auth_sender.send(AuthSignal::Complete {
                generation,
                result: result.map_err(AuthFailure::Rejected),
            });
        });
        Ok(true)
    }

    /// Synchronous half of [`Engine::login`]: binds the callback port, then
    /// consumes (or prepares) the OAuth attempt and marks the engine
    /// `Authenticating`. Split out so the transition is unit-testable without
    /// spawning a live flow.
    ///
    /// The bind comes first and its failure returns before anything is
    /// mutated. A port this application cannot have means the attempt is
    /// impossible, and the user must be told while still looking at the Log in
    /// button — not left in `Authenticating` waiting on a callback that has
    /// nowhere to land. Failing here also preserves the prepared attempt and
    /// the published authorize URL, so the next click is a clean retry.
    fn begin_login_flow(&mut self) -> Result<(PendingAuth, OauthListener), String> {
        let listener = bind_oauth_listener()?;
        let pending = match self.pending_auth.take() {
            Some(pending) => pending,
            None => prepare_oauth()?,
        };
        self.shutdown_playback();
        self.auth_running = true;
        self.generation = self.generation.wrapping_add(1);
        self.auth_generation = self.generation;
        self.state.ready = false;
        self.state.auth_state = AuthState::Authenticating;
        self.state.playing = false;
        self.state.error = None;
        self.state.auth_url = Some(pending.auth_url.clone());
        Ok((pending, listener))
    }

    /// Clears the cached credentials and tears the session down; the state
    /// flips to `NeedsLogin` with a fresh authorize URL so re-login works
    /// without a restart. Idempotent: safe when no session or credentials
    /// exist.
    pub fn logout(&mut self) -> Result<bool, String> {
        // Invalidate any in-flight authentication attempt: its completion
        // signal must not resurrect a session after an explicit logout.
        self.auth_running = false;
        self.generation = self.generation.wrapping_add(1);
        crate::browse::clear_canvas_cache();
        self.enter_needs_login();
        if let Err(error) = self.clear_cached_credentials() {
            eprintln!("could not clear cached Spotify credentials: {error}");
            self.state.error = Some(format!("could not clear cached credentials: {error}"));
        }
        Ok(true)
    }

    /// Removes the app-owned `credentials.json`; a missing file is already
    /// logged out. librespot's `Cache` has no removal API, so the file is
    /// removed directly.
    fn clear_cached_credentials(&self) -> Result<(), String> {
        let path = &self.credentials_file;
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    pub fn on_auth_signal(
        &mut self,
        signal: AuthSignal,
        player_sender: mpsc::UnboundedSender<PlayerSignal>,
    ) -> bool {
        match signal {
            AuthSignal::Complete { generation, result } => {
                if generation != self.generation {
                    // The attempt this answer belongs to no longer exists: the
                    // generation moved on while `connect_cached` was in flight.
                    // Discarding the answer is right; discarding the
                    // bookkeeping is not — see
                    // [`Engine::abandon_cached_authentication`].
                    self.abandon_cached_authentication(generation);
                    return false;
                }
                self.auth_running = false;
                match result {
                    Ok(connected) => {
                        let had_queue = self.state.current_index.is_some();
                        let restored_volume = self.state.volume;
                        let resume = had_queue && self.state.playing;
                        // Whatever the previous generation still owns goes away
                        // before the replacement is installed. Assigning over a
                        // live player would leave it feeding the audio device
                        // while every transport command reached its
                        // replacement: audible playback that pause, seek and
                        // next no longer touch, until the app is restarted.
                        if resume {
                            self.stop_playback_handles();
                        } else {
                            self.shutdown_playback();
                        }
                        self.state.ready = true;
                        self.state.auth_state = AuthState::Ready;
                        self.state.auth_url = None;
                        self.pending_auth = None;
                        // The session is live in both arms below, and stays
                        // live: a machine with no output device can browse,
                        // search and edit playlists, and the player is the only
                        // thing it cannot have yet.
                        self.session = Some(connected.session);
                        self.next_reconnect = None;
                        self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
                        self.awaiting_transport_retry = false;
                        match connected.playback {
                            Ok(handles) => {
                                self.state.volume = if had_queue {
                                    restored_volume
                                } else {
                                    handles.volume_percent
                                };
                                self.clear_audio_unavailable();
                                self.clear_error();
                                self.player = Some(handles.player);
                                self.mixer = Some(handles.mixer);
                                if had_queue {
                                    let volume = percent_to_volume(restored_volume);
                                    if let Some(mixer) = &self.mixer {
                                        mixer.set_volume(volume);
                                    }
                                    crate::audio::set_sink_volume(volume);
                                    self.current_needs_load = true;
                                    self.state.playing = resume;
                                    if resume {
                                        if let Err(error) = self.load_current_at_loop_pass(true) {
                                            self.state.playing = false;
                                            self.state.error = Some(error);
                                        }
                                    }
                                }
                                self.forward_player_events(
                                    handles.events,
                                    generation,
                                    player_sender,
                                );
                            }
                            Err(message) => {
                                // No output device. Everything a session is
                                // for still happened — the queue, the volume,
                                // the auth state — and the device probe builds
                                // the player once the machine has one.
                                //
                                // `playback` crosses this boundary as the
                                // message alone, so a boot whose open timed out
                                // is classified as a missing device here; the
                                // probe re-derives the blocked condition from
                                // its own typed answer, seconds later.
                                self.enter_audio_unavailable(message, PlayerFailure::Ordinary);
                            }
                        }
                        true
                    }
                    Err(failure) => {
                        eprintln!("Spotify playback authentication failed: {failure}");
                        match failure {
                            AuthFailure::Rejected(message) => {
                                self.enter_needs_login();
                                self.state.error = Some(message);
                            }
                            AuthFailure::Unreachable(message) => {
                                // The credentials are fine, the network is not.
                                // Keep both them and the queue and arm another
                                // attempt: resuming from sleep regularly beats
                                // Windows' resolver to the first connect, and
                                // answering that with a login prompt discards a
                                // session that was never refused.
                                self.state.ready = false;
                                self.state.auth_state = AuthState::Authenticating;
                                self.state.error = Some(message);
                                self.reconnect_backoff =
                                    (self.reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                                self.next_reconnect = Some(Instant::now() + self.reconnect_backoff);
                                self.awaiting_transport_retry = true;
                            }
                        }
                        true
                    }
                }
            }
            AuthSignal::PlayerRebuilt {
                generation,
                normalisation,
                result,
            } => {
                if self.audio_probe_generation == Some(generation) {
                    self.audio_probe_generation = None;
                }
                if !self.player_rebuild_is_current(generation, normalisation) {
                    return false;
                }
                let handles = match result {
                    Ok(handles) => handles,
                    // This is the device probe's answer, or a normalisation
                    // rebuild that ran into the same machine: either way the
                    // session is fine and only the device is missing, so the
                    // engine stays exactly where it was and asks again later.
                    Err(PlaybackError::NoOutputDevice(message)) => {
                        self.enter_audio_unavailable(message, PlayerFailure::Ordinary);
                        return true;
                    }
                    // A device that exists but did not answer: the driver is
                    // the problem, and the open that hung is a player thread
                    // this process will never get back. Retry it on the long
                    // ladder so those do not accumulate.
                    Err(PlaybackError::DeviceOpenBlocked(message)) => {
                        self.enter_audio_unavailable(message, PlayerFailure::Blocked);
                        return true;
                    }
                    Err(error) => {
                        // The rebuild itself failed — the software mixer, the
                        // player thread — rather than a device refusing an open.
                        // The preference it was rebuilding for is already
                        // swapped into the shared atomic every later rebuild
                        // reads, so keeping the incumbent would leave the engine
                        // playing a configuration it has just reported as not
                        // applied; and the message saying so would be released
                        // by the next command (`clear_error`) because no device
                        // failure was in force, leaving a silent mismatch over
                        // audio nobody described. So the player goes the way a
                        // refused device's does: the reason stays in
                        // `state.error`, and the probe re-arms to build one.
                        self.enter_audio_unavailable(
                            format!("could not rebuild the audio player: {}", error.message()),
                            PlayerFailure::Ordinary,
                        );
                        return true;
                    }
                };

                let was_playing = self.state.playing;
                self.detach_player();
                self.generation = self.generation.wrapping_add(1);
                let generation = self.generation;
                let volume = percent_to_volume(self.state.volume);
                handles.mixer.set_volume(volume);
                crate::audio::set_sink_volume(volume);
                self.player = Some(handles.player);
                self.mixer = Some(handles.mixer);
                // A paused replacement may not open output until the next
                // Play, and its banner must not wait for that.
                self.clear_audio_unavailable();
                self.current_needs_load = self.state.current_index.is_some();
                self.play_request_id = None;
                if was_playing {
                    if let Err(error) = self.load_current_at_loop_pass(true) {
                        self.state.playing = false;
                        self.state.error = Some(error);
                    }
                }
                self.forward_player_events(handles.events, generation, player_sender);
                true
            }
        }
    }
    /// Hands the engine back its retry after an authentication attempt that will
    /// never be answered.
    ///
    /// `connect_cached` is spawned with the generation it started under, and any
    /// generation bump while it is in flight — a device failure detaching the
    /// player, a rebuild installing one — makes its `Complete` arrive stale. The
    /// answer is dropped; the flag it set must not be. `auth_running` gates
    /// every later start: [`Engine::tick_session_health`] returns at its first
    /// line, [`Engine::start_cached_authentication`] no-ops, [`Engine::login`]
    /// answers `Ok` without binding the callback port, and even the device probe
    /// is refused (`player_rebuild_is_current` needs `state.ready`). The app
    /// would show "reconnecting" until the process was restarted.
    ///
    /// `attempt` is the generation the stale answer carries, and
    /// [`Engine::auth_generation`] says whether that attempt is the one the
    /// engine is still waiting on. It will not always be: an explicit logout
    /// clears the flag and bumps the generation while a cached attempt is still
    /// in flight, and the user may start a fresh OAuth flow before the old
    /// answer lands. Clearing the flag then would take it from the live flow —
    /// whose next click would fail on the callback port — so an answer that
    /// belongs to some earlier attempt only clears its own.
    fn abandon_cached_authentication(&mut self, attempt: u64) {
        if !self.auth_running || self.auth_generation != attempt {
            return;
        }
        self.auth_running = false;
        // A reconnect attempt armed its own due time before starting, so this
        // is a safety net rather than what makes the retry happen: it covers an
        // attempt started with no due time armed at all (a first connect), so
        // that a future caller of [`Engine::start_authentication`] cannot
        // reintroduce the wedge this helper exists to undo.
        if self.next_reconnect.is_none() {
            self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
            self.next_reconnect = Some(Instant::now() + self.reconnect_backoff);
        }
    }

    fn player_rebuild_is_current(&self, generation: u64, normalisation: bool) -> bool {
        self.state.ready
            && generation == self.generation
            && self.normalisation.load(Ordering::Acquire) == normalisation
    }

    fn forward_player_events(
        &mut self,
        mut events: librespot_playback::player::PlayerEventChannel,
        generation: u64,
        player_sender: mpsc::UnboundedSender<PlayerSignal>,
    ) {
        self.player_generation = Some(generation);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if player_sender
                    .send(PlayerSignal::Event { generation, event })
                    .is_err()
                {
                    return;
                }
            }
            let _ = player_sender.send(PlayerSignal::Closed { generation });
        });
    }

    /// Removes timestamps older than a burst window without treating a clock
    /// adjustment backwards as an expiration.
    ///
    /// The actual pacing contract is documented on [`Self::pace_track_change`].
    fn prune_recent_times(times: &mut VecDeque<Instant>, now: Instant, window: Duration) {
        loop {
            let expired = match times.front() {
                Some(when) => now
                    .checked_duration_since(*when)
                    .is_some_and(|elapsed| elapsed > window),
                None => false,
            };
            if !expired {
                break;
            }
            times.pop_front();
        }
    }

    /// Records a real player load rather than merely a button press. Keeping
    /// this history lets the first failure in an already-observable rapid
    /// load burst avoid destructive cache cleanup.
    fn note_track_change(&mut self, now: Instant) {
        Self::prune_recent_times(
            &mut self.recent_track_changes,
            now,
            TRACK_CHANGE_BURST_WINDOW,
        );
        self.recent_track_changes.push_back(now);
    }

    /// Records a current-track load failure and reports the two independent
    /// things the engine decides with. A second failure within the failure
    /// window is clustered even when the same track was retried; two or more
    /// recent load starts also protect the first failure observed after rapid
    /// clicks.
    fn record_unavailable(&mut self, now: Instant) -> UnavailableBurst {
        Self::prune_recent_times(
            &mut self.recent_track_changes,
            now,
            TRACK_CHANGE_BURST_WINDOW,
        );
        Self::prune_recent_times(&mut self.recent_unavailable, now, UNAVAILABLE_BURST_WINDOW);
        let clustered = self.recent_unavailable.len() >= 1
            || self.recent_track_changes.len() >= TRACK_CHANGE_BURST_MINIMUM;
        self.recent_unavailable.push_back(now);
        UnavailableBurst {
            clustered,
            consecutive: self.recent_unavailable.len(),
        }
    }

    /// A load that succeeded ends the current failure burst: a later isolated
    /// failure is again eligible for corrupt-cache cleanup, the
    /// [`UNAVAILABLE_STOP_LIMIT`] count starts over, and preloading resumes.
    ///
    /// "Succeeded" means audio came out, and nothing weaker. librespot's
    /// `Playing` used to be taken as the proof, and it is not one: a track
    /// loaded without its audio key reaches `Playing` exactly like a working
    /// track and is silent for the three seconds it survives. Resetting the
    /// count on that made the breaker count to one, forever, while the queue
    /// emptied itself.
    fn clear_unavailable_burst(&mut self) {
        self.recent_unavailable.clear();
    }

    /// Whether the `EndOfTrack` librespot just reported is a track that
    /// finished rather than one that broke.
    ///
    /// librespot sends the same event for both (`player.rs`: the decode-error
    /// and packet-error arms send `EndOfTrack`, as does real EOF), so the
    /// answer has to come from what the engine itself observed. Two things
    /// separate them, and a track has to pass both:
    ///
    /// - it produced audio at all, which a keyless track never does; and
    /// - it is at its end, which a decoder that collapsed mid-stream is not.
    ///
    /// The second test runs in the compiled timeline, so an edit that cuts the
    /// tail still ends where the listener hears it end, and is skipped
    /// entirely while a loop is active — a loop rewinds the source playhead on
    /// purpose, so its position at decoder EOF means nothing.
    fn end_of_track_is_genuine(&mut self) -> bool {
        if !self.current_load_produced_audio {
            return false;
        }
        if self.current_loop().is_some() {
            return true;
        }
        self.tick_position();
        let timeline = self.current_timeline();
        let played_ms = timeline.source_to_compiled(self.state.position_ms);
        timeline.compiled_duration_ms().saturating_sub(played_ms) <= ABNORMAL_END_REMAINDER_MS
    }

    /// Waits out any remaining [`TRACK_CHANGE_MIN_INTERVAL`] since the last
    /// command-driven track change, then re-arms the interval. Called before
    /// dispatching PlayQueue/Next/Previous so rapid presses advance at a
    /// bounded rate instead of bursting audio-key requests at Spotify's key
    /// service. Delays the command loop briefly; presses are never dropped.
    async fn pace_track_change(&mut self) {
        let wait = track_change_wait(
            self.last_track_change,
            Instant::now(),
            TRACK_CHANGE_MIN_INTERVAL,
        );
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        self.last_track_change = Some(Instant::now());
    }

    pub async fn process_command(
        &mut self,
        command: Command,
        auth_sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> Result<bool, String> {
        if matches!(&command, Command::Status) {
            if self.state.auth_state == AuthState::Error {
                self.start_authentication(auth_sender.clone());
            }
            // `status` is the shell's re-sync request: its answer carries
            // everything, including what an earlier state already handed over.
            self.republish_queue();
            return Ok(true);
        }
        if matches!(&command, Command::Login) {
            return self.login(auth_sender);
        }
        if matches!(&command, Command::Logout) {
            return self.logout();
        }
        if let Command::SetNormalisation { enabled } = command {
            return Ok(self.set_normalisation(enabled, auth_sender));
        }
        // A guarded editor teardown is allowed to lose a race with a real
        // queue command even while the player is unavailable. Treat that
        // stale request as an unchanged no-op before readiness checks. The
        // lease must be nonzero and still own the installed preview.
        if let Command::RestoreQueue {
            only_if_preview: true,
            preview_lease_id,
            ..
        } = &command
        {
            if !self.preview_mode
                || *preview_lease_id == 0
                || *preview_lease_id != self.preview_lease_id
            {
                return Ok(false);
            }
        }
        // A machine with no output device, or a session being rebuilt, can
        // still be *told* what to do: see [`Engine::runs_without_output_device`]
        // and [`Engine::runs_during_reconnect`]. Everything else needs a player
        // to act on — and gets the device's own message, not a generic "player
        // unavailable", when there is none.
        if !self.runs_without_output_device(&command) && !self.runs_during_reconnect(&command) {
            self.ensure_ready()?;
        }
        // Pace command-driven track changes so rapid next/prev spam cannot
        // burst audio-key requests (each load of an uncached track fetches
        // its decryption key; the key service rate-limits bursts and
        // playback dies). Only PlayQueue/Next/Previous are paced; other
        // commands may wait a tick — no press is dropped.
        if matches!(
            &command,
            Command::PlayQueue { .. }
                | Command::PreviewTrackEdit { .. }
                | Command::PlayQueueIndex { .. }
                | Command::Next
                | Command::Previous
        ) && self.state.ready {
            self.pace_track_change().await;
        }
        match command {
            Command::Status
            | Command::GetHistory { .. }
            | Command::ClearHistory
            | Command::Shutdown
            | Command::Login
            | Command::Logout
            | Command::SetNormalisation { .. }
            | Command::BrowsePlaylist { .. }
            | Command::BrowsePlaylistMembership { .. }
            | Command::QueueMetadata { .. }
            | Command::BrowsePlaylistCovers { .. }
            | Command::BrowsePlaylistTree { .. }
            | Command::BrowseRadio { .. }
            | Command::BrowsePlaylistRecommendations { .. }
            | Command::BrowseTrack { .. }
            | Command::BrowseEpisode { .. }
            | Command::BrowseShow { .. }
            | Command::BrowseProfile { .. }
            | Command::BrowseAlbum { .. }
            | Command::BrowseArtist { .. }
            | Command::BrowseArtistSongwriter { .. }
            | Command::BrowseArtistCatalogue { .. }
            | Command::BrowseLikedSongs { .. }
            | Command::BrowseLikedUris { .. }
            | Command::BrowseSearch { .. }
            | Command::BrowseTrackCredits { .. }
            | Command::BrowseCanvas { .. }
            | Command::EnableAccountCanvas
            | Command::BrowseFollowedArtists
            | Command::BrowseSavedShows
            | Command::GetTrackWaveform { .. }
            | Command::CancelTrackWaveform { .. }
            | Command::GetTrackEdit { .. }
            | Command::SaveTrackEdit { .. }
            | Command::DeleteTrackEdit { .. }
            | Command::SetPlaylistTrackEditEnabled { .. }
            | Command::SetPlaylistTrackExcluded { .. }
            | Command::EditCreatePlaylist { .. }
            | Command::EditRenamePlaylist { .. }
            | Command::EditDeletePlaylist { .. }
            | Command::EditFollowPlaylist { .. }
            | Command::EditCreateFolder { .. }
            | Command::EditRenameFolder { .. }
            | Command::EditDeleteFolder { .. }
            | Command::EditMoveLibraryItem { .. }
            | Command::EditAddPlaylistTracks { .. }
            | Command::EditRemovePlaylistTracks { .. }
            | Command::EditReorderPlaylistTracks { .. } => unreachable!(),
            Command::PlayQueue {
                queue,
                index,
                position_ms,
                context,
                automatic_start,
            } => self.play_queue_with_automatic_start(
                queue,
                index,
                position_ms,
                context,
                automatic_start,
            ),
            Command::RestoreQueue {
                queue,
                index,
                position_ms,
                context,
                preview_lease_id,
                only_if_preview,
                resume_playing,
                automatic_start,
            } => self.restore_queue(
                queue,
                index,
                position_ms,
                context,
                preview_lease_id,
                only_if_preview,
                resume_playing,
                automatic_start,
            ),
            Command::PreviewTrackEdit {
                track,
                cuts,
                loop_range,
                position_ms,
                preview_lease_id,
            } => self.preview_track_edit(track, cuts, loop_range, position_ms, preview_lease_id),
            Command::PlayQueueIndex { index } => self.play_queue_index(index),
            Command::SetQueueCursor { index, position_ms } => self.set_queue_cursor(index, position_ms),
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::Next => self.advance_with_current_skip(false, false, true),
            Command::Previous => self.previous(),
            Command::Seek { position_ms } => self.seek_transport(position_ms),
            Command::SetVolume { percent } => self.set_volume(percent),
            Command::SetShuffle { enabled } => self.set_shuffle(enabled),
            Command::SetRepeat { mode } => self.set_repeat(mode),
            Command::SetPlaybackSpeed { speed } => self.set_playback_speed(speed),
            Command::SetPlaybackSpeeds { track_speed, episode_speed } => {
                self.set_playback_speeds(PlaybackSpeeds { track: track_speed, episode: episode_speed })
            }
            Command::AddQueue { track, context } => self.add_queue(track, context),
            Command::AddQueueBatch { tracks, context } => self.add_queue_batch(tracks, context),
            Command::RemoveQueue { index } => self.remove_queue(index),
            Command::MoveQueue { from, to } => self.move_queue(from, to),
        }
    }

    pub fn send_response(
        &self,
        request_id: &str,
        result: &Result<bool, String>,
    ) -> Result<(), String> {
        self.writer.send(&Response {
            kind: "response",
            request_id,
            ok: result.is_ok(),
            error: result.as_ref().err().map(String::as_str),
        })
    }

    /// An owned clone of the live session for browse/edit work: browsing
    /// only needs the authenticated session (unlike playback commands, no
    /// player yet). The clone is handed to spawned browse tasks so the
    /// command loop never blocks on network resolution.
    pub fn browse_session_clone(&self) -> Result<librespot_core::Session, String> {
        self.session
            .clone()
            .ok_or_else(|| match self.state.auth_state {
                AuthState::Authenticating => {
                    "Spotify authentication is still in progress".to_owned()
                }
                AuthState::NeedsLogin => {
                    "Spotify login is required; use the Log in button in Settings".to_owned()
                }
                AuthState::Error => self
                    .state
                    .error
                    .clone()
                    .unwrap_or_else(|| "Spotify authentication failed".to_owned()),
                AuthState::Ready => "the Spotify session is unavailable".to_owned(),
            })
    }

    pub fn set_cache_sender(&mut self, sender: mpsc::UnboundedSender<CacheSignal>) {
        self.cache_sender = Some(sender);
    }

    fn invalidate_cache_mark(&mut self, id: &str) {
        self.invalidated_cache_ids.insert(id.to_owned());
        self.cached_marks.retain(|held| held != id);
        self.emitted_cached_marks = 0;
        for track in &mut self.state.queue {
            if track.id == id { track.cached = false; }
        }
        if !self.uncached_marks.iter().any(|held| held == id) {
            self.uncached_marks.push(id.to_owned());
        }
    }

    /// Cancel deletion before a new load can write the same track. Deletion
    /// holds this short lease only around cache file removal, never network I/O.
    fn prepare_cache_reload(&self, uri: &SpotifyUri) {
        if let Ok(id) = uri.to_id() {
            if let Some(lease) = self.cache_evictions.get(&id) {
                *lease.lock() = false;
            }
        }
    }

    fn cancel_cache_evictions(&mut self) {
        for (_, lease) in self.cache_evictions.drain() { *lease.lock() = false; }
    }

    pub fn on_cache_signal(&mut self, signal: CacheSignal) -> bool {
        let matching = self.cache_evictions.get(&signal.id)
            .is_some_and(|lease| Arc::ptr_eq(lease, &signal.lease));
        if !matching { return false; }
        self.cache_evictions.remove(&signal.id);
        if signal.generation != self.generation || !*signal.lease.lock() { return false; }
        // Failed removals remain untrusted too: a fresh, audible load must
        // vouch for the cache before an old file can regain its badge.
        self.invalidate_cache_mark(&signal.id);
        true
    }

    fn evict_track_audio_cache(&mut self, track_uri: SpotifyUri) {
        if !matches!(track_uri, SpotifyUri::Track { .. }) { return; }
        let Ok(id) = track_uri.to_id() else { return; };
        self.invalidate_cache_mark(&id);
        let (Some(session), Some(sender)) = (self.session.clone(), self.cache_sender.clone()) else { return; };
        let Some(cache) = session.cache().cloned() else { return; };
        let lease = Arc::new(Mutex::new(true));
        if let Some(previous) = self.cache_evictions.insert(id.clone(), Arc::clone(&lease)) {
            *previous.lock() = false;
        }
        let generation = self.generation;
        tokio::spawn(async move {
            if let Ok(track) = librespot_metadata::Track::get(&session, &track_uri).await {
                // A session replacement or new load cancels the lease before
                // it can create a new download this old job would remove.
                if !session.is_invalid() {
                    let active = lease.lock();
                    if *active {
                        for file_id in track.files.values() {
                            if let Err(error) = cache.remove_file(*file_id) {
                                eprintln!("could not evict cached audio file {file_id}: {error}");
                            }
                        }
                    }
                }
            }
            let _ = sender.send(CacheSignal { id, generation, lease });
        });
    }

    pub fn on_player_signal(&mut self, signal: PlayerSignal) -> bool {
        match signal {
            PlayerSignal::Event { generation, event } if self.player_generation == Some(generation) => {
                self.on_player_event(event)
            }
            PlayerSignal::Closed { generation } if self.player_generation == Some(generation) => {
                if self.state.ready {
                    self.state.ready = false;
                    self.state.auth_state = AuthState::Error;
                    self.state.playing = false;
                    self.finalize_listening(false);
                    self.state.error = Some(
                        "the local audio player stopped unexpectedly; request status to retry"
                            .to_owned(),
                    );
                    self.play_request_id = None;
                    self.loading_failed = false;
                    self.last_track_change = None;
                    self.recent_track_changes.clear();
                    self.recent_unavailable.clear();
                    self.player = None;
                    self.player_generation = None;
                    self.mixer = None;
                    self.invalidate_audio_signals();
                    if let Some(session) = self.session.take() {
                        session.shutdown();
                    }
                    true
                } else {
                    false
                }
            }
            PlayerSignal::Event { .. } | PlayerSignal::Closed { .. } => false,
        }
    }

    pub fn on_audio_signal(&mut self, signal: AudioSignal) -> bool {
        match signal {
            // Construction alone proves nothing for lazy output. A callback
            // for this load does, even if the listener has since paused it,
            // and only that proof resets the probe ladder. The banner already
            // went when the player was installed, so nothing visible changes.
            AudioSignal::OutputReady { revision } => {
                if revision == self.audio_revision && self.player.is_some() {
                    self.audio_probe_backoff = AUDIO_PROBE_BACKOFF_MIN;
                }
                false
            }
            AudioSignal::OutputFailed { revision, blocked } => {
                if revision != self.audio_revision || self.player.is_none() {
                    return false;
                }
                // Native errors may arrive after pause or at EOF without a
                // subsequent decoder write. Keep the user's current intent.
                self.enter_audio_unavailable(audio_stalled_message(),
                    if blocked { PlayerFailure::Blocked } else { PlayerFailure::Ordinary });
                true
            }
            AudioSignal::SpeedBoundary { speed, revision, at } => {
                if revision != self.audio_revision {
                    return false;
                }
                if let Some(position) = self.audible_position() {
                    self.update_position(position);
                } else {
                    self.tick_position_at(at);
                    self.position_anchor = Some((self.state.position_ms, at));
                }
                // Every load and seek opens with a boundary at the rate it was
                // configured with; only a rate the listener can see change is
                // worth a state. The re-anchored playhead rides the position
                // lane.
                let reported = self.audible_speed.unwrap_or(self.state.playback_speed());
                self.audible_speed = Some(speed);
                reported != speed
            }
            // The one fact librespot's transport events cannot supply: this
            // load is real. Everything that has to tell a playing track from a
            // silent one hangs off it. Revision-gated like every audio signal,
            // so a packet from the pipeline a previous load left behind cannot
            // vouch for this one.
            AudioSignal::Output { revision } => {
                if revision != self.audio_revision {
                    return false;
                }
                if self.current_load_produced_audio {
                    return false;
                }
                self.current_load_produced_audio = true;
                if let Some(id) = self.loaded_track_id.clone() {
                    if self.invalidated_cache_ids.remove(&id) {
                        for track in &mut self.state.queue {
                            if track.id == id { track.cached = false; }
                        }
                    }
                    self.mark_cached_tracks(None);
                }
                // Audio starts here, so this is where the playhead starts
                // moving from: the position it holds is the offset the load was
                // asked for (or the last position an event reported for it),
                // because nothing has been allowed to project past it while
                // the load was silent. Re-anchoring is what makes that offset
                // the base of the drift rather than an instant that has already
                // expired, and the state change is what tells the UI its
                // buffering hold is over — the local projection may resume from
                // exactly this position.
                self.update_position(self.state.position_ms);
                true
            }
            // Deliberately not gated on `playing`: the boundary was reached,
            // and the jump is what re-arms the pipeline to emit the next one.
            AudioSignal::LoopBoundary {
                position_ms,
                revision,
            } => {
                if revision != self.audio_revision {
                    return false;
                }
                let Some(loop_range) = self.current_loop() else {
                    return false;
                };
                if loop_range.start_ms != position_ms || self.loop_pass >= loop_range.play_count {
                    return false;
                }
                self.loop_pass += 1;
                self.loop_jump_pending = true;
                let result = if self.loop_decoder_eof {
                    let start_playing = self.state.playing;
                    self.update_position(position_ms);
                    self.load_current_at_loop_pass(start_playing)
                } else {
                    self.seek_source_at_loop_pass(position_ms).map(|_| ())
                };
                if let Err(error) = result {
                    self.state.playing = false;
                    self.state.error = Some(error);
                }
                true
            }
            // The output stopped draining mid-write: nothing has consumed the
            // ring for the whole write-drain timeout, which is what an
            // unplugged device looks like from inside the player thread.
            // librespot turns the resulting write error into a pause and says
            // nothing about the device, so this is the engine's only chance to
            // put the transport somewhere it can recover from.
            AudioSignal::OutputStalled { revision } => {
                if revision != self.audio_revision {
                    // A player this engine has already let go, reporting two
                    // seconds after the fact. Acting on it would tear down the
                    // player that replaced it.
                    return false;
                }
                if self.player.is_none() {
                    return false;
                }
                // Transport commands own intent. Audio already produced by
                // this load says nothing about a later user Pause.
                self.enter_audio_unavailable(audio_stalled_message(), PlayerFailure::Ordinary);
                true
            }
            // librespot paused itself on the refused packet and reports that
            // as an ordinary pause, which the engine does not act on. Left
            // there, the engine would call itself playing over silence; the
            // row's failure gets the retry, cache eviction and skip any other
            // failed load gets.
            AudioSignal::DecodeFailed { revision } => {
                if revision != self.audio_revision {
                    return false;
                }
                let Some(track_id) = self.state.current_index
                    .and_then(|index| self.state.queue.get(index))
                    .and_then(|track| SpotifyUri::from_uri(&track.uri).ok())
                else {
                    return false;
                };
                eprintln!(
                    "transport: {track_id} produced audio the sink cannot play; treating it as a failed load"
                );
                self.fail_current_load(track_id)
            }
        }
    }
    pub fn shutdown(&mut self) {
        self.auth_running = false;
        self.generation = self.generation.wrapping_add(1);
        self.shutdown_playback();
        // A clean exit must not lose the volume the deferred write has not stored
        // yet. The write it was waiting for is never going to come, and the
        // cache file is the engine's only copy — the shell's snapshot is a
        // different file with a different clock.
        self.flush_pending_volume();
        self.state.ready = false;
        self.state.playing = false;
    }

    fn resolve_queue_edits(&self, queue: &mut [TrackRef]) {
        for track in queue {
            if track.uri.starts_with("spotify:episode:") {
                // A restored snapshot may still carry an old Downloaded mark.
                track.cached = false;
            }
            track.effective_edit = if track.uri.starts_with("spotify:track:") {
                self.track_edits
                    .resolve(&track.id, track.duration_ms, &track.context)
            } else {
                None
            };
        }
    }
    /// Eligibility is intentionally derived from the live row context and
    /// store on every transition. Queue snapshots carry no exclusion bit, so
    /// changing a preference immediately affects future choices without
    /// rewriting or reloading the queue.
    fn automatic_track_eligible(&self, index: usize) -> bool {
        self.state.queue.get(index).is_some_and(|track| {
            // Editor previews are a separate direct playback surface. Their
            // source row may retain a playlist context, but playlist
            // exclusions must never interfere with preview transport.
            if self.preview_mode {
                !track.unavailable
            } else {
                automatic_track_eligible(&self.track_edits, track)
            }
        })
    }

    fn configure_current_audio_at_loop_pass(&mut self, position_ms: u32, loop_pass: u32) {
        self.audio_start_position_ms = position_ms;
        self.audible_speed = None;
        let edit = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.clone());
        self.audio_revision = crate::audio::configure_customization_at_loop_pass(
            edit,
            self.state.playback_speed(),
            position_ms,
            loop_pass,
        );
    }

    fn configure_current_audio_after_natural_boundary(&mut self, position_ms: u32) {
        self.audio_start_position_ms = position_ms;
        self.audible_speed = None;
        let edit = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.clone());
        self.audio_revision = crate::audio::configure_customization_after_natural_boundary(
            edit,
            self.state.playback_speed(),
            position_ms,
        );
    }
    fn current_loop(&self) -> Option<LoopRange> {
        self.state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.as_ref())
            .and_then(|edit| edit.loop_range)
    }

    fn invalidate_audio_signals(&mut self) {
        self.audio_revision = self.audio_revision.wrapping_add(1);
    }

    fn loop_pass_for_position(&self, position_ms: u32) -> u32 {
        self.current_loop()
            .filter(|loop_range| position_ms >= loop_range.end_ms)
            .map_or(1, |loop_range| loop_range.play_count.max(1))
    }

    fn reset_loop_pass_for_position(&mut self, position_ms: u32) {
        self.loop_pass = self.loop_pass_for_position(position_ms);
    }

    /// A speed change rebuilds the customization at the current source
    /// position, but it is not a user seek: an internal repeated pass must
    /// survive that rebuild. A position at or beyond the loop end is always
    /// the completed pass so a missing marker cannot re-enter the loop.
    fn preserve_loop_pass_for_position(&mut self, position_ms: u32) {
        if self
            .current_loop()
            .is_some_and(|loop_range| position_ms >= loop_range.end_ms)
        {
            self.reset_loop_pass_for_position(position_ms);
        } else {
            self.loop_pass = self.loop_pass.max(1);
        }
    }

    fn current_loop_start(&self) -> Option<u32> {
        self.current_loop().map(|range| range.start_ms)
    }

    fn ensure_ready(&self) -> Result<(), String> {
        if !self.state.ready {
            return Err(match self.state.auth_state {
                AuthState::Authenticating => {
                    "Spotify authentication is still in progress".to_owned()
                }
                AuthState::NeedsLogin => {
                    "Spotify login is required; use the Log in button in Settings".to_owned()
                }
                AuthState::Error => self
                    .state
                    .error
                    .clone()
                    .unwrap_or_else(|| "Spotify authentication failed".to_owned()),
                AuthState::Ready => "the local audio player is unavailable".to_owned(),
            });
        }
        if self.player.is_some() {
            return Ok(());
        }
        // Ready with no player has exactly one cause now, and the user has to
        // act on it: say what it is and what fixes it.
        Err(self.audio_unavailable.as_ref().map_or_else(
            || "the local audio player is unavailable".to_owned(),
            |unavailable| unavailable.message.clone(),
        ))
    }

    fn player(&self) -> Result<&Arc<Player>, String> {
        self.player.as_ref().ok_or_else(|| {
            self.audio_unavailable.as_ref().map_or_else(
                || "the local audio player is unavailable".to_owned(),
                |unavailable| unavailable.message.clone(),
            )
        })
    }

    fn finalize_listening(&mut self, completed: bool) {
        if self.preview_mode {
            return;
        }
        let _ = self.listening_history.finalize(completed);
    }

    fn pause_listening(&mut self) {
        if !self.preview_mode {
            self.listening_history.pause();
        }
    }

    fn start_listening(&mut self, track: &TrackRef) {
        if !self.preview_mode {
            self.listening_history.start_or_resume(track);
        }
    }
    fn enter_preview_mode(&mut self) {
        if !self.preview_mode {
            self.finalize_listening(false);
        }
        self.preview_mode = true;
    }

    fn leave_preview_mode(&mut self) {
        self.preview_mode = false;
        self.preview_lease_id = 0;
        self.finalize_listening(false);
    }
    fn validate_queue(queue: &[TrackRef], index: usize) -> Result<(), String> {
        if queue.is_empty() {
            return (index == 0)
                .then_some(())
                .ok_or_else(|| "index must be zero for an empty queue".to_owned());
        }
        if index >= queue.len() {
            return Err(format!("queue index {index} is out of range"));
        }
        for track in queue {
            let uri = parse_track_uri(track)?;
            if let Some(edit) = &track.effective_edit {
                if !matches!(uri, SpotifyUri::Track { .. }) {
                    return Err("podcast episodes cannot have song edits".to_owned());
                }
                validate_definition(&track.id, track.duration_ms, &edit.cuts, edit.loop_range)?;
            }
        }
        Ok(())
    }

    fn install_empty_or_unavailable_queue(&mut self, queue: Vec<TrackRef>) -> Result<bool, String> {
        self.finalize_listening(false);
        crate::audio::pause_output();
        if let Some(player) = &self.player {
            player.stop();
        } else if !(self.auth_running || self.awaiting_transport_retry) {
            self.player()?;
        }
        self.invalidate_audio_signals();
        self.state.queue = queue;
        self.rows_changed();
        self.state.current_index = None;
        self.state.position_ms = 0;
        self.state.duration_ms = 0;
        self.state.playing = false;
        self.history.clear();
        self.shuffle_pool.clear();
        self.clear_error();
        self.loading_failed = false;
        self.loop_pass = 1;
        self.current_needs_load = false;
        self.recent_track_changes.clear();
        self.recent_unavailable.clear();
        Ok(true)
    }

    #[cfg(test)]
    fn play_queue(
        &mut self,
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        context: String,
    ) -> Result<bool, String> {
        self.play_queue_with_automatic_start(queue, index, position_ms, context, false)
    }

    fn play_queue_with_automatic_start(
        &mut self,
        mut queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        context: String,
        automatic_start: bool,
    ) -> Result<bool, String> {
        fill_queue_context(&mut queue, &context);
        self.resolve_queue_edits(&mut queue);
        self.play_resolved_queue(
            queue,
            index,
            position_ms,
            PositionSpace::Transport,
            false,
            0,
            automatic_start,
        )
    }

    fn preview_track_edit(
        &mut self,
        track: TrackRef,
        cuts: Vec<TimeRange>,
        loop_range: Option<LoopRange>,
        position_ms: u32,
        preview_lease_id: u64,
    ) -> Result<bool, String> {
        if preview_lease_id == 0 {
            return Err("preview lease ID must be nonzero".to_owned());
        }
        // A replacement transfers ownership before draft validation. If this
        // attempt is rejected, the old preview remains restorable by the new
        // editor context rather than by the stale one.
        if self.preview_mode {
            self.preview_lease_id = preview_lease_id;
        }
        let track = with_preview_edit(track, cuts, loop_range)?;
        self.play_resolved_queue(
            vec![track],
            0,
            position_ms,
            PositionSpace::Source,
            true,
            preview_lease_id,
            false,
        )
    }

    /// Automatic shuffle starts draw their current row from the same filtered
    /// Fisher–Yates bag as the rest of playback, rather than fixing it first.
    fn install_queue_start(
        &mut self,
        queue: Vec<TrackRef>,
        current: Option<usize>,
        automatic_start: bool,
    ) -> Option<usize> {
        self.state.queue = queue;
        self.rows_changed();
        let draw_first = automatic_start && self.state.shuffle;
        self.state.current_index = if draw_first { None } else { current };
        self.rebuild_shuffle_pool();
        if draw_first {
            self.state.current_index = self.shuffle_pool.pop();
        }
        self.state.current_index
    }

    /// Installs queue rows whose `effective_edit` values are already final.
    /// Preview uses this path so its draft is never resolved against the
    /// persisted edit store.
    fn play_resolved_queue(
        &mut self,
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        position_space: PositionSpace,
        preview: bool,
        preview_lease_id: u64,
        automatic_start: bool,
    ) -> Result<bool, String> {
        Self::validate_queue(&queue, index)?;
        let candidate = if automatic_start {
            first_automatic_wrapping(&queue, index, &self.track_edits)
        } else {
            first_available_from(&queue, index)
        };
        let Some(playable_index) = candidate else {
            // Do not tear down an installed preview queue when a replacement
            // cannot stop the current player.
            if (preview || self.preview_mode) && !(self.auth_running || self.awaiting_transport_retry) {
                self.player()?;
            }
            if preview {
                self.preview_lease_id = preview_lease_id;
                self.enter_preview_mode();
            } else {
                self.leave_preview_mode();
            }
            let result = self.install_empty_or_unavailable_queue(queue);
            if automatic_start && result.is_ok() {
                self.state.error =
                    Some("no eligible tracks remain for automatic playback".to_owned());
            }
            return result;
        };
        if preview {
            self.preview_lease_id = preview_lease_id;
            self.enter_preview_mode();
        } else {
            self.leave_preview_mode();
        }

        let playable_index = self
            .install_queue_start(queue, Some(playable_index), automatic_start)
            .expect("validated queue has an eligible starting row");
        self.state.duration_ms = self.state.queue[playable_index].duration_ms;
        let position_ms = if playable_index == index {
            position_ms
        } else {
            0
        };
        match position_space {
            PositionSpace::Source => self.update_position(position_ms),
            PositionSpace::Transport => self.update_transport_position(position_ms),
        }
        self.clear_error();
        self.history.clear();
        // The first load starts a fresh pacing window for subsequent
        // command-driven changes.
        self.last_track_change = Some(Instant::now());
        self.load_current(true)?;
        Ok(true)
    }

    /// Installs a validated queue and frozen playhead without touching the
    /// audio loader. This is the only startup path: there is no paused load
    /// whose decoder/output setup could produce an audible blip.
    fn restore_queue(
        &mut self,
        mut queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        context: String,
        preview_lease_id: u64,
        only_if_preview: bool,
        resume_playing: bool,
        automatic_start: bool,
    ) -> Result<bool, String> {
        // A preview teardown can race with a real queue command. Once the
        // latter wins, this stale restore must not even validate or resolve
        // its snapshot against the live engine state.
        if only_if_preview
            && (!self.preview_mode
                || preview_lease_id == 0
                || self.preview_lease_id != preview_lease_id)
        {
            return Ok(false);
        }

        fill_queue_context(&mut queue, &context);
        // The snapshot carries whatever edits were resolved when it was
        // written, which may since have been deleted or disabled. Every other
        // queue install re-resolves against the store; this one must too, or a
        // restart replays an edit the store no longer holds.
        self.resolve_queue_edits(&mut queue);
        // Validate every input before stopping the current player or changing
        // any engine-owned queue/playhead state. A malformed stale snapshot
        // therefore cannot damage a live preview either.
        Self::validate_queue(&queue, index)?;

        self.leave_preview_mode();
        if let Some(player) = &self.player {
            player.stop();
        }
        self.invalidate_audio_signals();
        let playable_index = if queue.is_empty() {
            None
        } else if automatic_start {
            first_automatic_wrapping(&queue, index, &self.track_edits)
        } else {
            first_available_wrapping(&queue, index)
        };
        let playable_index = self.install_queue_start(queue, playable_index, automatic_start);
        self.state.duration_ms = playable_index
            .map(|current| self.state.queue[current].duration_ms)
            .unwrap_or(0);
        if playable_index == Some(index) {
            self.update_transport_position(position_ms);
        } else {
            self.update_transport_position(0);
        }
        self.state.playing = false;
        // Not `state.error = None` directly: a startup restore on a machine
        // with no output device installs the queue perfectly well, and must not
        // take the message that says why there is no sound with it.
        self.clear_error();
        self.history.clear();
        self.play_request_id = None;
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        self.reset_loop_pass_for_position(self.state.position_ms);
        self.loading_failed = false;
        self.current_needs_load = playable_index.is_some();
        self.recent_track_changes.clear();
        self.recent_unavailable.clear();

        if resume_playing {
            // Keep restore and resume inside one command. The command loop
            // will therefore emit one authoritative restored-playing state,
            // rather than publishing an intermediate paused restore first.
            if let Err(error) = self.play() {
                // Queue installation already left preview mode. Do not put a
                // failed resume back into the draft preview; expose the
                // installed real queue as paused with its failure visible.
                self.preview_mode = false;
                self.preview_lease_id = 0;
                self.state.playing = false;
                self.state.error = Some(error);
            }
        }
        Ok(true)
    }

    fn play_queue_index(&mut self, index: usize) -> Result<bool, String> {
        let (duration_ms, unavailable_reason) = {
            let track = self
                .state
                .queue
                .get(index)
                .ok_or_else(|| format!("queue index {index} is out of range"))?;
            (
                track.duration_ms,
                track.unavailable.then(|| {
                    track
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "this track is permanently unavailable".to_owned())
                }),
            )
        };
        if let Some(reason) = unavailable_reason {
            return Err(reason);
        }
        self.leave_preview_mode();
        if let Some(current) = self.state.current_index {
            if current != index {
                self.history.push(current);
            }
        }
        self.state.current_index = Some(index);
        self.order_changed();
        self.state.duration_ms = duration_ms;
        self.update_transport_position(0);
        self.clear_error();
        // Clicking a row asks to hear that row, not to be handed a different
        // random ordering of everything behind it, so the drawn plan survives
        // minus the row that just became current. The row being left is not put
        // back: `previous` owns that direction and pushes it there itself.
        self.repair_shuffle_pool(|pooled| Some(pooled));
        self.last_track_change = Some(Instant::now());
        self.load_current(true)?;
        Ok(true)
    }

    fn set_queue_cursor(&mut self, index: usize, position_ms: u32) -> Result<bool, String> {
        let track = self.state.queue.get(index)
            .ok_or_else(|| format!("queue index {index} is out of range"))?;
        playable_track_uri(track)?;
        let duration_ms = track.duration_ms;
        if self.state.playing {
            return Err("remote queue cursor requires paused local playback".to_owned());
        }
        let current = self.state.current_index;
        if current != Some(index) {
            let upcoming = if current.is_some() { self.upcoming_indices() } else { Vec::new() };
            if let Some(offset) = upcoming.iter().position(|candidate| *candidate == index) {
                // Commit each skipped draw, not a new shuffle. The shell may
                // observe multiple phone Next actions in one poll.
                for _ in 0..=offset {
                    if let Some(previous) = self.state.current_index {
                        self.history.push(previous);
                    }
                    let next = self.take_next_index_with_skip(false, true)
                        .ok_or_else(|| "canonical queue plan was exhausted".to_owned())?;
                    self.state.current_index = Some(next);
                }
            } else if let Some(offset) = self.history.iter().rposition(|candidate| *candidate == index) {
                // Returning through history makes the departed rows upcoming
                // again, in the same order as local Previous.
                while self.history.len() > offset {
                    let previous = self.history.pop().expect("history offset exists");
                    if let Some(departed) = self.state.current_index {
                        if self.state.shuffle && departed != previous
                            && self.automatic_track_eligible(departed)
                            && !self.shuffle_pool.contains(&departed)
                        {
                            self.shuffle_pool.push(departed);
                        }
                    }
                    self.shuffle_pool.retain(|candidate| *candidate != previous);
                    self.state.current_index = Some(previous);
                }
            } else {
                if let Some(previous) = current {
                    self.history.push(previous);
                }
                self.state.current_index = Some(index);
                self.repair_shuffle_pool(|candidate| Some(candidate));
            }
            if self.state.shuffle && self.shuffle_pool.is_empty()
                && self.state.repeat == RepeatMode::Context
            {
                self.rebuild_shuffle_pool();
            }
            self.order_changed();
        }
        self.finalize_listening(false);
        if let Some(player) = &self.player {
            player.stop();
        }
        self.drain_stop_at = None;
        self.play_request_id = None;
        self.invalidate_audio_signals();
        self.current_needs_load = true;
        self.current_load_produced_audio = false;
        self.loading_failed = false;
        self.retry_current_at = None;
        self.preload_armed = false;
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        self.state.duration_ms = duration_ms;
        self.update_transport_position(position_ms);
        // The cursor names a fresh position, possibly on another row; the
        // pass the departed position had reached must not carry over to it.
        self.reset_loop_pass_for_position(self.state.position_ms);
        Ok(true)
    }

    fn play(&mut self) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        if self.player.is_none() && (self.audio_unavailable.is_some()
            || self.auth_running || self.awaiting_transport_retry)
        {
            // There is no player to send this to, but the request itself is
            // worth keeping: `state.playing` is what the device probe starts
            // from when it finds a device, and what a reconnect resumes when
            // its session arrives — so pressing play into a mute machine, or
            // into a session that died while the machine slept, is answered by
            // music as soon as there can be any. The row is marked as not
            // loaded for the same reason a teardown marks it.
            self.state.playing = true;
            self.current_needs_load = true;
            eprintln!(
                "transport: play at {} ms with no player yet; it starts when one exists",
                self.state.position_ms,
            );
            return Ok(true);
        }
        let was_playing = self.state.playing;
        // There is no live load to resume when one is pending, has failed, or
        // has run off the end of the track, so those all start a fresh one —
        // from the top when the track had ended, where it stands otherwise.
        let at_track_end =
            self.state.duration_ms > 0 && self.state.position_ms >= self.state.duration_ms;
        let reload = self.loop_decoder_eof
            || self.current_needs_load
            || self.loading_failed
            || at_track_end;
        if reload {
            if at_track_end || !(self.loading_failed || self.current_needs_load) {
                self.update_transport_position(0);
                self.load_current(true)?;
            } else {
                self.load_current_at_loop_pass(true)?;
            }
        } else {
            let player = Arc::clone(self.player()?);
            let transition = self.prepare_audio_resume();
            player.play(transition);
        }
        self.state.playing = true;
        self.update_position(self.state.position_ms);
        self.clear_error();
        // Transport commands are otherwise invisible in the log: a resume
        // prints nothing, and neither does a load librespot satisfies from the
        // track it already holds. A session where the engine and librespot
        // disagreed about `playing` could not be told apart afterwards from one
        // where no command ever arrived, because neither left a record.
        eprintln!(
            "transport: play at {} ms (engine was {}); librespot told to {}",
            self.state.position_ms,
            if was_playing { "playing" } else { "paused" },
            if reload { "load" } else { "resume" },
        );
        Ok(true)
    }

    fn pause(&mut self) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        if self.player.is_none() && (self.audio_unavailable.is_some()
            || self.auth_running || self.awaiting_transport_retry)
        {
            // Nothing is audible, so there is nothing to stop — but the intent
            // still has to be cleared here, or the device probe (or the
            // reconnect) would start playing something the user asked to stop.
            let was_playing = self.state.playing;
            self.state.playing = false;
            self.pause_listening();
            self.update_position(self.state.position_ms);
            eprintln!(
                "transport: pause at {} ms with no player (engine was {})",
                self.state.position_ms,
                if was_playing { "playing" } else { "paused" },
            );
            return Ok(true);
        }
        let was_playing = self.state.playing;
        // The engine may not call itself paused before librespot has been told
        // to stop, so the command goes out first and a failure to reach the
        // player aborts the transition rather than declaring a pause that never
        // happened. This used to be skipped whenever a load was pending or had
        // failed, on the theory that pause is invalid in the state librespot is
        // then in — but `current_needs_load` records an intent to load, not
        // that librespot stopped, and the two come apart the instant anything
        // sets the flag while audio is still running. librespot makes the
        // unconditional call safe from every direction: it clears the pending
        // start of a Loading player (precisely what a pause during a load
        // should do), is idempotent and silent on an already paused one, and
        // costs one log line and no state change on a stopped or finished one.
        // Skipping it costs audio that keeps playing with no transport control
        // left that can reach it.
        let player = Arc::clone(self.player()?);
        crate::audio::pause_output();
        self.tick_position();
        if crate::audio::output_is_ready(self.audio_revision) {
            self.on_audio_signal(AudioSignal::OutputReady { revision: self.audio_revision });
        }
        player.pause();
        // The clock was captured above, before changing transport intent.
        self.state.playing = false;
        self.update_position(self.state.position_ms);
        self.pause_listening();
        self.clear_error();
        // See the note in `play`. A pause arriving at an already paused engine
        // is the specific signature of a stale UI, and it used to leave no
        // trace at all.
        eprintln!(
            "transport: pause at {} ms (engine was {})",
            self.state.position_ms,
            if was_playing { "playing" } else { "paused" },
        );
        Ok(true)
    }

    fn seek_transport(&mut self, position_ms: u32) -> Result<bool, String> {
        let source_position_ms = self.transport_to_source(position_ms);
        self.seek_source(source_position_ms)
    }

    fn seek_source(&mut self, position_ms: u32) -> Result<bool, String> {
        let position = position_ms.min(self.state.duration_ms);
        self.reset_loop_pass_for_position(position);
        self.loop_jump_pending = false;
        self.seek_source_at_loop_pass(position)
    }

    fn seek_source_at_loop_pass(&mut self, position_ms: u32) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        let position = position_ms.min(self.state.duration_ms);
        if self.loop_decoder_eof {
            let start_playing = self.state.playing;
            self.update_position(position);
            self.load_current_at_loop_pass(start_playing)?;
            self.clear_error();
            return Ok(true);
        }
        self.configure_current_audio_at_loop_pass(position, self.loop_pass);
        if self.current_needs_load {
            self.update_position(position);
            self.clear_error();
            return Ok(true);
        }
        if self.loading_failed {
            // The failed loader was stopped by the Unavailable handler. A
            // seek is also a valid recovery request: load at the new offset
            // while preserving the current play/pause intent.
            let start_playing = self.state.playing;
            self.update_position(position);
            self.load_current_at_loop_pass(start_playing)?;
            self.clear_error();
            return Ok(true);
        }
        // One owning command pauses, seeks, commits the sample origin and
        // resumes only the captured intent. Old in-flight reads cannot adopt
        // this revision between separately queued pause/seek/play commands.
        let was_playing = self.state.playing;
        self.player()?.seek(position, was_playing,
            crate::audio::decoder_transition(self.audio_revision, true));
        self.update_position(position);
        self.clear_error();
        Ok(true)
    }

    fn previous(&mut self) -> Result<bool, String> {
        let current = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        if let Some(index) = self.previous_index() {
            self.leave_preview_mode();
            if self.state.shuffle {
                // A repeated visit can already be scheduled in the bag. Once
                // it becomes current, it must leave that future plan.
                if let Some(position) = self.shuffle_pool.iter().position(|pooled| *pooled == index)
                {
                    self.shuffle_pool.remove(position);
                }
                if self.automatic_track_eligible(current)
                    && current != index
                    && !self.shuffle_pool.contains(&current)
                {
                    self.shuffle_pool.push(current);
                }
            }
            self.state.current_index = Some(index);
            self.order_changed();
            self.state.duration_ms = self.state.queue[index].duration_ms;
            self.update_transport_position(0);
            self.clear_error();
            // Symmetric with Next: the press is the request to hear it.
            self.load_current(true)?;
            Ok(true)
        } else {
            // No earlier track (first track, or already restarting): seek the
            // current track back to its beginning instead of erroring.
            self.seek_transport(0)
        }
    }

    /// Decides which track `previous` should switch to. `Some(index)` names a
    /// valid queue index; `None` means "restart the current track". The
    /// position restart threshold matches the UI's 3-second restart window, so
    /// the optimistic flip and the engine agree. History entries that no longer
    /// index the queue (stale after a mutation) are dropped instead of
    /// panicking.
    fn previous_index(&mut self) -> Option<usize> {
        if self
            .current_timeline()
            .source_to_compiled(self.state.position_ms)
            > PREVIOUS_RESTART_THRESHOLD_MS
        {
            return None;
        }
        while let Some(index) = self.history.pop() {
            if self.automatic_track_eligible(index) {
                return Some(index);
            }
        }
        let current = self.state.current_index?;
        if !self.state.shuffle {
            return (0..current)
                .rev()
                .find(|index| self.automatic_track_eligible(*index));
        }
        None
    }

    fn advance(&mut self, at_end: bool) -> Result<bool, String> {
        self.advance_with_current_skip(at_end, false, false)
    }

    /// `resume` is what separates a press of Next from the two advances the
    /// engine makes on its own. Pressing Next asks to *hear* the next track, so
    /// it starts a paused queue playing; a natural boundary and an unavailable
    /// track carry the transport intent across instead, because neither is a
    /// request to start playback.
    fn advance_with_current_skip(
        &mut self,
        at_end: bool,
        skip_current_for_repeat: bool,
        resume: bool,
    ) -> Result<bool, String> {
        // Every path out of here moves the published plan: the row that plays,
        // the row the bag popped, or the end of the queue itself.
        self.order_changed();
        if at_end {
            // Flush delayed speed/cut output before changing configuration.
            // The queue itself remains live so the tail can drain audibly.
            if let Err(error) = crate::audio::finish_natural_boundary() {
                // A bounded flush timeout is output failure, not a bad row.
                // Retire the stalled native stream and retain recovery intent.
                self.enter_audio_unavailable(error, PlayerFailure::Ordinary);
                return Ok(true);
            }
            self.last_track_change = Some(Instant::now());
        }
        let current = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        let next = self.take_next_index_with_skip(at_end, skip_current_for_repeat);
        if self.preview_mode && next.is_some_and(|index| index != current) {
            self.leave_preview_mode();
        }
        self.finalize_listening(at_end);
        match next {
            Some(index) => {
                if index != current {
                    self.history.push(current);
                }
                self.state.current_index = Some(index);
                self.state.duration_ms = self.state.queue[index].duration_ms;
                self.update_transport_position(0);
                self.clear_error();
                // Either way the load and `state.playing` agree, which is the
                // part that matters: loading with a fixed `true` used to leave
                // the engine calling itself paused while librespot played the
                // new track, to be reconciled by whichever event arrived next.
                let start_playing = resume || self.state.playing;
                if at_end {
                    self.load_current_after_natural_boundary(start_playing)?;
                } else {
                    self.load_current(start_playing)?;
                }
            }
            None => {
                // At natural EOF librespot is already in EndOfTrack. Calling
                // stop here would clear correctly queued audio before the
                // device drains it, so the stop waits for the tail; explicit
                // queue exhaustion stops at once.
                if at_end {
                    match audio::finish_output_tail() {
                        Ok(()) => {
                            let tail = audio::output_tail_remaining().unwrap_or_default();
                            self.drain_stop_at = Some(Instant::now() + tail + OUTPUT_DRAIN_MARGIN);
                        }
                        Err(error) => self.enter_audio_unavailable(error, PlayerFailure::Ordinary),
                    }
                } else {
                    self.player()?.stop();
                    self.invalidate_audio_signals();
                }
                // librespot holds nothing it can resume either way: Play loads
                // again, and a seek only moves the playhead it will load at.
                self.current_needs_load = true;
                self.state.playing = false;
                self.update_position(self.state.duration_ms);
                let finished = self.loaded_track_id.take();
                self.mark_cached_tracks(finished);
                eprintln!("transport: no eligible track follows queue index {current}; stopping");
            }
        }
        Ok(true)
    }

    /// Sets the speed of the current item's kind, which is the one the player
    /// bar shows.
    fn set_playback_speed(&mut self, speed: f32) -> Result<bool, String> {
        let mut speeds = self.state.speeds;
        if self.state.current_uri().is_some_and(|uri| uri.starts_with("spotify:episode:")) {
            speeds.episode = speed;
        } else {
            speeds.track = speed;
        }
        self.set_playback_speeds(speeds)
    }

    /// Installs both remembered speeds (the shell's restore), or one of them
    /// (a speed change). Only the current item's kind is audible; the other
    /// waits for the next item of its kind to load.
    fn set_playback_speeds(&mut self, speeds: PlaybackSpeeds) -> Result<bool, String> {
        if [speeds.track, speeds.episode]
            .iter()
            .any(|speed| !speed.is_finite() || !(0.5..=4.0).contains(speed))
        {
            return Err("playback speed must be between 0.5 and 4.0".to_owned());
        }
        if self.state.speeds == speeds {
            return Ok(false);
        }
        let speed = speeds.of(self.state.current_uri());
        if speed == self.state.playback_speed() {
            self.state.speeds = speeds;
            return Ok(true);
        }
        // Settle elapsed old-rate time before publishing the request. No
        // decoder command, queue clear, edit rebuild or listening-history
        // transition belongs to a speed change.
        self.tick_position();
        self.position_anchor = Some((self.state.position_ms, Instant::now()));
        if let Some(speed) = crate::audio::audible_speed(self.audio_revision) {
            self.audible_speed = Some(speed);
        } else if self.state.playing && self.current_load_produced_audio {
            self.audible_speed = Some(self.audible_speed.unwrap_or(self.state.playback_speed()));
        } else {
            self.audible_speed = None;
        }
        self.state.speeds = speeds;
        if self.current_needs_load || self.state.current_index.is_none() {
            let position = self.state.position_ms;
            self.preserve_loop_pass_for_position(position);
            self.configure_current_audio_at_loop_pass(position, self.loop_pass);
        } else {
            crate::audio::set_customization_speed(speed);
        }
        Ok(true)
    }

    fn set_volume(&mut self, percent: u8) -> Result<bool, String> {
        if percent > 100 {
            return Err("volume percent must be between 0 and 100".to_owned());
        }
        if self.state.volume == percent {
            return Ok(false);
        }
        let volume = percent_to_volume(percent);
        // A machine with no output device has no mixer and no sink to apply
        // this to, but the volume is still a real setting: it is persisted
        // here, reported in the state the UI reads, and applied by the player
        // the device probe builds.
        if let Some(mixer) = &self.mixer {
            mixer.set_volume(volume);
        }
        // The cache write is deferred rather than paid here. A slider drag
        // paces this command at 50 ms and `Cache::save_volume` creates and
        // rewrites a file per call, so writing per step would mean twenty file
        // creations a second for a number that has already moved on. The
        // work timer writes the value the drag stops at
        // ([`Engine::tick_volume_persist`]), and a clean exit writes whatever
        // is still pending ([`Engine::shutdown`]).
        self.pending_volume = Some(PendingVolume {
            volume,
            changed_at: Instant::now(),
        });
        // The audible volume lives on the output callback (per-packet attenuation
        // is disabled); apply it there so the change is heard immediately.
        crate::audio::set_sink_volume(volume);
        self.state.volume = percent;
        self.clear_error();
        // The change travels on the scalar lane, and the `Ok(false)` below
        // tells the command loop not to serialize the whole state for it.
        self.emit_volume()?;
        Ok(false)
    }

    fn set_shuffle(&mut self, enabled: bool) -> Result<bool, String> {
        if self.state.shuffle == enabled {
            return Ok(false);
        }
        self.state.shuffle = enabled;
        self.order_changed();
        self.history.clear();
        // Switching shuffle on is the one place a full draw is the point: there
        // is no earlier plan to preserve. Switching it off empties the bag by
        // the same call.
        self.rebuild_shuffle_pool();
        self.preload_next();
        Ok(true)
    }

    fn set_repeat(&mut self, mode: RepeatMode) -> Result<bool, String> {
        if self.state.repeat == mode {
            return Ok(false);
        }
        self.state.repeat = mode;
        self.order_changed();
        self.preload_next();
        Ok(true)
    }
    fn add_queue(&mut self, mut track: TrackRef, context: String) -> Result<bool, String> {
        fill_queue_context(std::slice::from_mut(&mut track), &context);
        self.resolve_queue_edits(std::slice::from_mut(&mut track));
        parse_track_uri(&track)?;
        self.state.queue.push(track);
        self.rows_changed();
        self.splice_new_rows_into_shuffle_pool(self.state.queue.len() - 1);
        self.preload_next();
        Ok(true)
    }

    fn add_queue_batch(
        &mut self,
        mut tracks: Vec<TrackRef>,
        context: String,
    ) -> Result<bool, String> {
        fill_queue_context(&mut tracks, &context);
        self.resolve_queue_edits(&mut tracks);
        for track in &tracks {
            parse_track_uri(track)?;
        }
        if tracks.is_empty() {
            return Ok(true);
        }
        let first_new = self.state.queue.len();
        self.state.queue.extend(tracks);
        self.rows_changed();
        self.splice_new_rows_into_shuffle_pool(first_new);
        self.preload_next();
        Ok(true)
    }

    fn remove_queue(&mut self, index: usize) -> Result<bool, String> {
        if index >= self.state.queue.len() {
            return Err(format!("queue index {index} is out of range"));
        }
        let current = self.state.current_index;
        let was_playing = self.state.playing;
        if self.state.current_index == Some(index) {
            self.leave_preview_mode();
        }
        self.state.queue.remove(index);
        self.rows_changed();
        self.history.retain_mut(|visited| {
            if *visited == index {
                false
            } else {
                if *visited > index {
                    *visited -= 1;
                }
                true
            }
        });
        let mut reload = false;

        match current {
            None => {}
            Some(_) if self.state.queue.is_empty() => {
                self.player()?.stop();
                self.invalidate_audio_signals();
                self.state.current_index = None;
                self.state.duration_ms = 0;
                self.state.playing = false;
                self.update_position(0);
                self.play_request_id = None;
            }
            Some(current) if index == current => {
                let start = current.min(self.state.queue.len() - 1);
                let replacement =
                    first_automatic_wrapping(&self.state.queue, start, &self.track_edits);
                self.state.current_index = replacement;
                self.state.duration_ms = replacement
                    .map(|replacement| self.state.queue[replacement].duration_ms)
                    .unwrap_or(0);
                self.update_transport_position(0);
                if replacement.is_some() {
                    reload = true;
                } else {
                    self.player()?.stop();
                    self.state.playing = false;
                    self.invalidate_audio_signals();
                    self.play_request_id = None;
                    self.current_needs_load = false;
                    self.state.error =
                        Some("no eligible tracks remain for automatic playback".to_owned());
                }
            }
            Some(current) if index < current => self.state.current_index = Some(current - 1),
            Some(_) => {}
        }
        // The removed row leaves the bag and everything above it slides down
        // one. Removing the current row also promotes a replacement that the
        // bag may already hold; `repair_shuffle_pool` drops whatever is current
        // by the time it runs, which is why this is called after the match.
        self.repair_shuffle_pool(|pooled| {
            if pooled == index {
                None
            } else if pooled > index {
                Some(pooled - 1)
            } else {
                Some(pooled)
            }
        });
        if reload {
            self.load_current(was_playing)?;
        } else {
            self.preload_next();
        }
        Ok(true)
    }

    fn move_queue(&mut self, from: usize, to: usize) -> Result<bool, String> {
        let length = self.state.queue.len();
        if from >= length || to >= length {
            return Err(format!("queue move {from} to {to} is out of range"));
        }
        if from == to {
            return Ok(true);
        }
        let track = self.state.queue.remove(from);
        self.state.queue.insert(to, track);
        self.rows_changed();
        if let Some(current) = self.state.current_index {
            self.state.current_index = Some(remap_current_index_after_move(current, from, to));
        }
        for visited in &mut self.history {
            *visited = remap_current_index_after_move(*visited, from, to);
        }
        // A move is a permutation of the queue, so the bag is remapped through
        // the very function that just moved `current_index`. Writing the
        // arithmetic out a second time here would be an opportunity for the two
        // to disagree, and a bag that disagrees about where the current row
        // went will happily hand it back as the next track.
        self.repair_shuffle_pool(|pooled| Some(remap_current_index_after_move(pooled, from, to)));
        self.preload_next();
        Ok(true)
    }

    fn load_current(&mut self, start_playing: bool) -> Result<(), String> {
        self.reset_loop_pass_for_position(self.state.position_ms);
        self.load_current_at_loop_pass(start_playing)
    }

    fn load_current_at_loop_pass(&mut self, start_playing: bool) -> Result<(), String> {
        self.load_current_with_boundary(start_playing, false)
    }

    fn load_current_after_natural_boundary(&mut self, start_playing: bool) -> Result<(), String> {
        self.loop_pass = 1;
        self.load_current_with_boundary(start_playing, true)
    }

    fn load_current_with_boundary(
        &mut self,
        start_playing: bool,
        natural_boundary: bool,
    ) -> Result<(), String> {
        let index = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        let track = self
            .state
            .queue
            .get(index)
            .ok_or_else(|| "the queue has no current track (index out of range)".to_owned())?;
        let uri = playable_track_uri(track)?;
        let incoming = track.uri.starts_with("spotify:track:").then(|| track.id.clone());
        let position_ms = self.state.position_ms;
        if natural_boundary {
            self.configure_current_audio_after_natural_boundary(position_ms);
        } else {
            self.configure_current_audio_at_loop_pass(position_ms, self.loop_pass);
        }
        self.play_request_id = None;
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        let reconnecting = !self.state.ready && (self.auth_running || self.awaiting_transport_retry);
        let player = if reconnecting {
            // No load may reach the invalid session. Stop an incumbent before
            // publishing the replacement selection, then resume that selection
            // once authentication installs live handles.
            crate::audio::pause_output();
            if let Some(player) = &self.player {
                player.stop();
            }
            None
        } else {
            Some(Arc::clone(self.player()?))
        };
        self.loading_failed = false;
        self.current_needs_load = reconnecting;
        // This load is the newest thing the engine asked for, so it owns every
        // judgement the previous one left behind: nothing has been heard from
        // it yet, no preload is due for a track that just started, and any
        // retry armed for the row being replaced is moot. Clearing them here
        // rather than at each caller is what makes a user action — a click, a
        // seek, a press of Next — cancel a pending retry without anyone having
        // to remember to.
        self.current_load_produced_audio = false;
        self.preload_armed = false;
        self.retry_current_at = None;
        self.current_load_is_retry = false;
        // What the engine reports and what it asks librespot for are one
        // statement, made once, here. Callers used to set `playing` beside the
        // call, which let the two say different things — a track change loading
        // with a hardcoded `true` while the engine still called itself paused,
        // with nothing but the next incoming event to settle the argument.
        self.state.playing = start_playing;
        // The load starts playing at the offset it was asked for, so that
        // offset is the playhead's truth from this instant. Every drift
        // projection measures from the newest authoritative position, and
        // without this one it would keep measuring from the previous load's
        // anchor — seconds old when a retry follows a failure, which is
        // precisely the case where the two positions differ. Callers that set a
        // position do so because they are changing the answer, not because the
        // load handler demands one, so this cannot disagree with them.
        self.update_position(position_ms);
        self.note_track_change(Instant::now());
        self.drain_stop_at = None;
        let outgoing = std::mem::replace(&mut self.loaded_track_id, incoming)
            .filter(|outgoing| self.loaded_track_id.as_ref() != Some(outgoing));
        if let Some(player) = player {
            self.prepare_cache_reload(&uri);
            player.load(uri, start_playing, position_ms,
                crate::audio::decoder_transition(self.audio_revision, !natural_boundary));
        }
        self.mark_cached_tracks(outgoing);
        Ok(())
    }

    /// Asks librespot to fetch the next track, if now is a moment at which
    /// that is worth an audio key.
    ///
    /// Two independent gates, each closing on a different reason not to spend
    /// the request. [`PRELOAD_WATERMARK_MS`] holds it back until the current
    /// track is nearly over, which is what makes a user clicking through the
    /// queue generate no preloads at all. The failure burst holds it back
    /// while the key service is refusing this client, which is precisely when
    /// a second request per load is the one not to make; it lifts the moment a
    /// load is heard producing audio.
    ///
    /// Every queue mutation still calls this, so an edit made during the last
    /// thirty seconds of a track re-points the preload at whatever now plays
    /// next. Outside that window the call is free.
    fn preload_next(&self) {
        let (Some(player), Some(uri)) = (&self.player, self.preload_target()) else {
            return;
        };
        self.prepare_cache_reload(&uri);
        player.preload(uri);
    }

    /// The track a preload would ask for right now, and `None` when it should
    /// not ask at all. Split out from [`Engine::preload_next`] so both gates
    /// are answerable without a live player.
    fn preload_target(&self) -> Option<SpotifyUri> {
        playable_track_uri(self.preload_row()?).ok()
    }

    /// The row [`Engine::preload_target`] names. Every state carries its uri
    /// (`preload_uri`), so the webview's one piece of ahead-of-time work, the
    /// next Canvas, starts and ends with the audio's.
    fn preload_row(&self) -> Option<&TrackRef> {
        // A preserved player sounding through a reconnect sits on the session
        // being replaced; no load may reach it, a preload included.
        if !self.state.ready || !self.preload_armed || !self.recent_unavailable.is_empty() {
            return None;
        }
        let row = self.state.queue.get(self.peek_next_index()?)?;
        // `playable_track_uri`, not the bare parse: a row already known to be
        // permanently unavailable is not worth an audio key, and this is the
        // path that used to be two — one that checked and one that did not.
        playable_track_uri(row).is_ok().then_some(row)
    }

    /// Whether the current track is close enough to its end to arm the
    /// preload. Answered in the compiled timeline, because what decides when
    /// the next track has to be ready is when the listener reaches the end of
    /// this one, not where the source file ends: an edit that cuts the tail
    /// brings that moment forward, and playback speed moves it either way.
    ///
    /// The floor is capped at half the track so no length is unpreloadable —
    /// a twenty-second interlude arms at ten seconds in. This mirrors
    /// `play_qualifies` in [`crate::history`], which caps its own threshold
    /// the same way and for the same reason.
    fn at_preload_watermark(&self) -> bool {
        let (position_ms, duration_ms) = self.transport_position_and_duration();
        if duration_ms == 0 {
            return false;
        }
        duration_ms.saturating_sub(position_ms) <= PRELOAD_WATERMARK_MS.min(duration_ms / 2)
    }

    /// The current row's load did not produce a playable track, however
    /// librespot chose to say so. Decides between retrying it, holding it,
    /// skipping it, and stopping, and reports whether state changed.
    ///
    /// Every caller reaches here with a different event — `Unavailable` for a
    /// load that gave up, `EndOfTrack` for one that lied about starting — and
    /// the same situation. Keeping the decision in one place is what stops the
    /// second kind from being handled, as it was, as a track that finished.
    fn fail_current_load(&mut self, track_id: SpotifyUri) -> bool {
        let burst = self.record_unavailable(Instant::now());
        self.loading_failed = true;
        self.finalize_listening(false);
        // `state.playing` is left alone deliberately: it is the intent the
        // retry or skip below has to carry onto whatever loads next. Every
        // branch settles it — a successor loads with this intent, a retry
        // reloads with it, and an exhausted queue stops.
        self.state.error = Some(format!("Spotify track is unavailable: {track_id}"));

        // librespot leaves the failed loader in PlayerState::Loading after
        // sending Unavailable. Stop it explicitly so a later Play can submit a
        // fresh Load command instead of toggling start_playback on a
        // terminated future.
        if let Some(player) = &self.player {
            player.stop();
        }
        // That stop has a `Stopped` event behind it, and the engine must not
        // still be listening for one: the failed play request is dead to every
        // branch below — each either reloads or waits for the owner — and the
        // arm that handles `Stopped` would otherwise clear `playing` moments
        // after this, quietly disarming the retry the engine just promised.
        self.play_request_id = None;
        self.invalidate_audio_signals();

        // An isolated failure can be a corrupt/truncated cache entry;
        // librespot's decoder retry handles one cached format and this removes
        // every format before the next user retry. Once failures cluster,
        // preserve all cache files: key-service or network failures are not
        // evidence of corruption.
        if !burst.clustered {
            self.evict_track_audio_cache(track_id.clone());
        }

        // Pause has to win here, and it used to lose. librespot runs a load to
        // completion whether or not it was told to start playing, so the
        // failure of a paused load arrived exactly like the failure of a
        // playing one and skipped onward all the same: the queue walked itself
        // silently, track after track, with the pause button visibly doing
        // nothing. A paused engine has no continuity to protect, so hold the
        // failed row and let the owner choose — Play retries it, Next moves
        // past it. It is also what disarms the retry below, since the tick
        // only fires one for a queue that is playing.
        if !self.state.playing {
            eprintln!("transport: load failed for {track_id} while paused; holding this row");
            return true;
        }

        // A run of failures is the service refusing this client, not a run of
        // bad rows, so stop and say so rather than spending the rest of the
        // queue finding that out one track at a time.
        if burst.consecutive >= UNAVAILABLE_STOP_LIMIT {
            self.state.playing = false;
            // The banner in App.svelte ellipsises a long error, so the part
            // that says what happened comes first.
            self.state.error = Some(format!(
                "Spotify refused audio for {} tracks in a row; playback stopped",
                burst.consecutive
            ));
            eprintln!(
                "transport: {} load failures within {} s; stopping instead of skipping past {track_id}",
                burst.consecutive,
                UNAVAILABLE_BURST_WINDOW.as_secs(),
            );
            return true;
        }

        // The row gets one more attempt before it is given up on. See
        // [`LOAD_RETRY_BACKOFF`]: the refusal is transient, the track is
        // usually fine, and skipping on the first failure both discards a
        // working track and pays another key request to discover the next one
        // is refused too.
        if !self.current_load_is_retry {
            self.retry_current_at = Some(Instant::now() + LOAD_RETRY_BACKOFF);
            // What the banner says has to be what is happening, and for the
            // next three seconds what is happening is a retry.
            self.state.error = Some("Spotify refused audio for this track; retrying".to_owned());
            eprintln!(
                "transport: load failed for {track_id}; retrying this row in {} s",
                LOAD_RETRY_BACKOFF.as_secs(),
            );
            return true;
        }

        // A runtime failure is an automatic progression opportunity: continue
        // with the next eligible row when one exists. The
        // `skip_current_for_repeat` guard prevents repeat-one from retrying the
        // same failed loader forever. With no candidate, the branch below
        // simply leaves this failed row stopped.
        if let Err(error) = self.advance_with_current_skip(false, true, false) {
            self.state.playing = false;
            self.state.error = Some(error);
        }
        true
    }

    /// Drives the two things a failing or recovering load needs a clock for.
    /// Called from the work timer when its preload or retry deadline is due, so
    /// no extra timer exists; both checks are a comparison on engine-local state.
    ///
    /// Returns whether the engine's state changed and should be emitted: a
    /// fired retry, or an armed preload, which every state names (`preload_uri`).
    pub fn tick_playback_health(&mut self) -> bool {
        // Audio is out: whatever the run of failures was, it is over.
        // Preloading and corrupt-cache cleanup come back with it.
        if self.current_load_produced_audio && !self.recent_unavailable.is_empty() {
            self.clear_unavailable_burst();
        }

        // The playhead this reads is a projection, so project it first rather
        // than depend on the caller having done so.
        self.tick_position();
        // Edge-triggered, and only for a track that is running: a paused track
        // is not approaching its end, and scrubbing back and forth across the
        // watermark must not buy the same key twice. A load is what re-arms
        // it, so resuming a track paused past the watermark reaches this on
        // the next tick.
        let armed = !self.preload_armed && self.state.playing && self.at_preload_watermark();
        if armed {
            self.preload_armed = true;
            self.preload_next();
        }

        // A retry belongs to a queue that is still playing. Pause clears
        // `state.playing` and that is the whole cancellation: the owner's
        // complaint last time was a pause button that could not stop the
        // cascade, and an armed timer that outlived it would be the same bug
        // wearing a different hat.
        let due = self
            .retry_current_at
            .is_some_and(|at| Instant::now() >= at && self.state.playing);
        if !due {
            return armed;
        }
        // `load_current` clears both fields, so the flag is set afterwards:
        // it marks the load now in flight as the one that has already had its
        // second chance.
        if let Err(error) = self.load_current(true) {
            self.retry_current_at = None;
            self.state.playing = false;
            self.state.error = Some(error);
            return true;
        }
        self.current_load_is_retry = true;
        true
    }

    fn take_next_index_with_skip(
        &mut self,
        at_end: bool,
        skip_current_for_repeat: bool,
    ) -> Option<usize> {
        let current = self.state.current_index?;
        if at_end
            && !skip_current_for_repeat
            && self.state.repeat == RepeatMode::Track
            && self
                .state
                .queue
                .get(current)
                .is_some_and(|track| !track.unavailable)
        {
            // Repeat-one is a deliberate direct continuation: an exclusion
            // toggled while this row is playing must not interrupt it.
            return Some(current);
        }
        if self.state.shuffle {
            if self.shuffle_pool.is_empty() && self.state.repeat == RepeatMode::Context {
                self.rebuild_shuffle_pool();
            }
            while let Some(index) = self.shuffle_pool.pop() {
                if self.automatic_track_eligible(index) {
                    return Some(index);
                }
            }
            return None;
        }
        sequential_automatic_index(
            &self.state.queue,
            current,
            self.state.repeat,
            &self.track_edits,
        )
    }

    fn peek_next_index(&self) -> Option<usize> {
        let current = self.state.current_index?;
        if self.state.repeat == RepeatMode::Track
            && self
                .state
                .queue
                .get(current)
                .is_some_and(|track| !track.unavailable)
        {
            // See `take_next_index_with_skip`: preload for repeat-one is
            // allowed to point at the excluded current row.
            return Some(current);
        }
        if self.state.shuffle {
            return self.shuffle_pool.iter().rev().copied().find(|index| {
                self.state
                    .queue
                    .get(*index)
                    .is_some_and(|_| self.automatic_track_eligible(*index))
            });
        }
        sequential_automatic_index(
            &self.state.queue,
            current,
            self.state.repeat,
            &self.track_edits,
        )
    }

    /// The order the queue will actually play from here: shuffle's own plan
    /// when shuffle is on, the sequential walk when it is off, with exclusions
    /// and unavailable rows dropped either way. The current row is not part of
    /// it; the UI puts that at the head itself.
    ///
    /// Repeat-one is deliberately ignored. It loops one row for as long as it
    /// is set, which is a transport detail of the current track rather than a
    /// different plan for the queue behind it.
    fn upcoming_indices(&self) -> Vec<usize> {
        let Some(current) = self.state.current_index else {
            // Removals can leave a populated queue with no current row. Listing
            // every eligible row keeps the view honest instead of blank.
            return (0..self.state.queue.len())
                .filter(|index| self.automatic_track_eligible(*index))
                .collect();
        };
        if self.state.shuffle {
            // The pool is popped from the back, so reversed is play order. When
            // it empties under repeat-context the engine reshuffles into a
            // brand-new random order (see `take_next_index_with_skip`), which
            // nothing can know in advance — so the list simply stops there
            // rather than inventing a continuation.
            return self
                .shuffle_pool
                .iter()
                .rev()
                .copied()
                .filter(|index| self.automatic_track_eligible(*index))
                .collect();
        }
        let mut upcoming: Vec<usize> = (current.saturating_add(1)..self.state.queue.len())
            .filter(|index| self.automatic_track_eligible(*index))
            .collect();
        if self.state.repeat == RepeatMode::Context {
            // Inclusive of `current`, matching `sequential_automatic_index`'s
            // wrap exactly: when nothing later is eligible the walk restarts at
            // zero and may legitimately land back on the row playing now.
            upcoming.extend(
                (0..=current.min(self.state.queue.len().saturating_sub(1)))
                    .filter(|index| self.automatic_track_eligible(*index)),
            );
        }
        upcoming
    }

    /// Draws a brand-new random play order. This is the right answer only
    /// where the previous order has genuinely stopped existing: shuffle being
    /// switched on (there was no plan), a whole different queue being
    /// installed, and the repeat-context refill that starts a new lap. Every
    /// other mutation repairs the bag instead — see [`Engine::repair_shuffle_pool`].
    ///
    /// The bag it produces defines the four invariants every repair must also
    /// hold: it never contains `current_index`, never contains an index outside
    /// `state.queue`, never repeats an index, and is empty whenever shuffle is
    /// off.
    fn rebuild_shuffle_pool(&mut self) {
        // A redraw replaces the published plan wholesale.
        self.order_changed();
        self.shuffle_pool.clear();
        if !self.state.shuffle {
            return;
        }
        let current = self.state.current_index;
        let queue = &self.state.queue;
        let track_edits = &self.track_edits;
        let preview_mode = self.preview_mode;
        for (index, track) in queue.iter().enumerate() {
            if Some(index) == current {
                continue;
            }
            let eligible = if preview_mode {
                !track.unavailable
            } else {
                automatic_track_eligible(track_edits, track)
            };
            if eligible {
                self.shuffle_pool.push(index);
            }
        }
        for index in (1..self.shuffle_pool.len()).rev() {
            let swap = (self.next_random() as usize) % (index + 1);
            self.shuffle_pool.swap(index, swap);
        }
    }

    /// Carries the drawn order across a change in the queue's shape by mapping
    /// every entry through `remap` and dropping the ones that map to `None`.
    /// Survivors keep their relative order, which is the whole point: the pool
    /// is published as "up next", and redrawing it — which is what every queue
    /// mutation used to do — rescrambles the list the owner is looking at just
    /// because one unrelated row was added, removed, or dragged.
    ///
    /// `remap` must be injective over the bag, since nothing here can tell a
    /// collision apart from an honest duplicate; the range and current-row
    /// invariants are enforced below rather than trusted from each caller's
    /// arithmetic, because a bag entry that no longer indexes the queue is a
    /// panic waiting in `take_next_index_with_skip`.
    fn repair_shuffle_pool(&mut self, remap: impl Fn(usize) -> Option<usize>) {
        // The bag *is* the published plan: any repair moves rows in it.
        self.order_changed();
        if !self.state.shuffle {
            // `rebuild_shuffle_pool` empties the bag when shuffle goes off, and
            // a repair must never be the thing that resurrects entries into it.
            self.shuffle_pool.clear();
            return;
        }
        let length = self.state.queue.len();
        let current = self.state.current_index;
        self.shuffle_pool = std::mem::take(&mut self.shuffle_pool)
            .into_iter()
            .filter_map(remap)
            .filter(|index| *index < length && Some(*index) != current)
            .collect();
    }

    /// Puts a newly eligible row into the drawn order at a uniformly random
    /// position, drawn from the same PRNG that shuffled the bag in the first
    /// place. Pushing it onto one end would be simpler, but the bag is popped
    /// from the back, so that would make a queued track either always play next
    /// or always play last — neither of which is what shuffle means.
    fn splice_into_shuffle_pool(&mut self, index: usize) {
        let position = (self.next_random() as usize) % (self.shuffle_pool.len() + 1);
        self.shuffle_pool.insert(position, index);
    }

    /// Folds rows appended at `first_new` and beyond into the existing drawn
    /// order. Only the tail of the queue is walked, because that is the only
    /// part an append can have changed — the indices already in the bag still
    /// name the same tracks.
    fn splice_new_rows_into_shuffle_pool(&mut self, first_new: usize) {
        // Appended rows enter the published plan, so the revision moves even
        // when the bag is empty because shuffle is off.
        self.order_changed();
        if !self.state.shuffle {
            self.shuffle_pool.clear();
            return;
        }
        for index in first_new..self.state.queue.len() {
            if self.automatic_track_eligible(index) && self.state.current_index != Some(index) {
                self.splice_into_shuffle_pool(index);
            }
        }
    }

    /// Repairs the bag after a change to what counts as eligible rather than to
    /// the queue's shape: rows that just became ineligible leave, rows that just
    /// became eligible are spliced in, and every survivor keeps its drawn
    /// position. It walks the queue rather than a single index because one
    /// track id can occupy several queue rows.
    fn repair_shuffle_pool_for_eligibility(&mut self) {
        // Eligibility is part of the published plan even with shuffle off — a
        // row that just became excluded leaves the sequential walk too — so the
        // bump happens before the bag's own early return.
        self.order_changed();
        if !self.state.shuffle {
            self.shuffle_pool.clear();
            return;
        }
        let eligible: Vec<bool> = (0..self.state.queue.len())
            .map(|index| self.automatic_track_eligible(index))
            .collect();
        // Membership is answered from a set built once. Asking the bag itself
        // with `Vec::contains` per queue row would be quadratic, and a playlist
        // queue holds thousands of rows for the sake of this one toggle.
        let pooled: HashSet<usize> = self.shuffle_pool.iter().copied().collect();
        self.shuffle_pool
            .retain(|index| eligible.get(*index).copied().unwrap_or(false));
        let current = self.state.current_index;
        for index in 0..eligible.len() {
            if eligible[index] && Some(index) != current && !pooled.contains(&index) {
                self.splice_into_shuffle_pool(index);
            }
        }
    }

    fn next_random(&mut self) -> u64 {
        let mut value = self.random_state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.random_state = value;
        value
    }

    /// Decoder metadata can correct a restored row. Publish every occurrence
    /// together rather than leaving duplicate rows or shell snapshots stale.
    fn update_loaded_metadata(&mut self, uri: &str, duration_ms: u32, name: &str) -> bool {
        let mut changed = false;
        for track in &mut self.state.queue {
            if track.uri == uri {
                if track.duration_ms != duration_ms {
                    track.duration_ms = duration_ms;
                    changed = true;
                }
                if track.name != name {
                    track.name = name.to_owned();
                    changed = true;
                }
            }
        }
        if !changed {
            return false;
        }
        self.state.duration_ms = duration_ms;
        self.state.position_ms = self.state.position_ms.min(duration_ms);
        self.rows_changed();
        true
    }
    fn on_player_event(&mut self, event: PlayerEvent) -> bool {
        match event {
            PlayerEvent::PlayRequestIdChanged { play_request_id } => {
                self.play_request_id = Some(play_request_id);
                false
            }
            PlayerEvent::TrackChanged { audio_item } => {
                let Some(index) = self.state.current_index else { return false; };
                if audio_item.uri != self.state.queue[index].uri {
                    return false;
                }
                self.update_loaded_metadata(&audio_item.uri, audio_item.duration_ms, &audio_item.name)
            }
            PlayerEvent::Loading {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                // A fresh load (including a retry after Unavailable) puts the
                // engine back into an in-progress state. Keep the requested
                // play/pause intent already held in `state.playing`. The load
                // command already reported all of this; only a position or an
                // error the report did not carry is worth a state.
                let previous_position = self.state.position_ms;
                let had_error = self.state.error.is_some();
                self.loading_failed = false;
                self.update_position(position_ms);
                self.loop_jump_pending = false;
                self.clear_error();
                self.state.position_ms != previous_position
                    || had_error != self.state.error.is_some()
            }

            PlayerEvent::Playing {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                // A delayed answer to an older Play cannot undo a newer Pause.
                if !self.state.playing {
                    return false;
                }
                self.loading_failed = false;
                self.loop_jump_pending = false;
                // Deliberately not where the failure burst is cleared. This
                // event means librespot has a decoder and intends to use it,
                // which is not the same as audio existing: a track loaded
                // without its key reaches exactly here, with a playhead about
                // to start moving over silence. Clearing the run on this would
                // reset the breaker once per phantom track and guarantee it
                // never counts to three. `AudioSignal::Output` is the event
                // that means playback.
                if let Some(index) = self.state.current_index {
                    if let Some(track) = self.state.queue.get(index).cloned() {
                        self.start_listening(&track);
                    }
                }
                // librespot answers every play it is sent, including the ones
                // the engine has already reported as its own command's result,
                // and `playing` is the engine's intent, never this event's. So
                // only an error this clears is a change worth a state; the
                // playhead it carries rides the position lane.
                let was_failed = self.state.error.is_some();
                self.update_player_position(position_ms);
                self.clear_error();
                was_failed
            }
            // Pause captures the consumer clock synchronously. Decoder echoes
            // can belong to an older pause, seek, or failed write, so neither
            // their state nor decode-ahead position may overwrite that clock.
            PlayerEvent::Paused {
                play_request_id,
                track_id,
                ..
            } if self.is_current_event(play_request_id, &track_id) => false,
            PlayerEvent::PositionChanged {
                play_request_id,
                track_id,
                position_ms,
            }
            | PlayerEvent::PositionCorrection {
                play_request_id,
                track_id,
                position_ms,
            }
            | PlayerEvent::Seeked {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                // A position librespot reports that is the one the engine
                // already holds is not news: the engine anchors its projection
                // from its own commands, and the 2-second position lane is what
                // carries a playhead the UI has not seen. A genuinely different
                // position is a change the state event reports, so it still
                // emits one.
                let previous_position = self.state.position_ms;
                self.update_player_position(position_ms);
                self.state.position_ms != previous_position
            }
            PlayerEvent::EndOfTrack {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                // The failure this catches is the one the owner reported
                // twice: a track that never produced a sample, whose playhead
                // ran for three seconds over silence, and which then "ended".
                // Advancing on that is what walked the queue. It is a load
                // failure wearing the end of a track, so it is handled as one.
                if !self.end_of_track_is_genuine() {
                    eprintln!(
                        "transport: {track_id} ended at {} ms of {} ms having produced {}; treating it as a failed load",
                        self.state.position_ms,
                        self.state.duration_ms,
                        if self.current_load_produced_audio {
                            "audio"
                        } else {
                            "no audio at all"
                        },
                    );
                    return self.fail_current_load(track_id);
                }
                if self
                    .current_loop()
                    .is_some_and(|loop_range| self.loop_pass < loop_range.play_count)
                {
                    if self.loop_jump_pending {
                        let position_ms = self.current_loop_start().unwrap_or(0);
                        let start_playing = self.state.playing;
                        self.update_position(position_ms);
                        if let Err(error) = self.load_current_at_loop_pass(start_playing) {
                            self.state.playing = false;
                            self.state.error = Some(error);
                        }
                        return true;
                    }
                    // The audible loop marker owns continuation. If it has not
                    // drained yet, remember that seek is no longer legal and
                    // reload the same track when the marker arrives.
                    self.loop_decoder_eof = true;
                    return false;
                }
                if let Err(error) = self.advance(true) {
                    self.state.playing = false;
                    self.state.error = Some(error);
                }
                true
            }
            // librespot's own `TimeToPreloadNextTrack` is deliberately not
            // used. It answers nearly the same question — it fires at the same
            // 30 s — but it measures against the source file rather than the
            // compiled timeline the listener hears, it fires in the Paused
            // state as readily as the Playing one, and it withholds itself
            // entirely until the current track has finished downloading, which
            // on a slow link means the preload silently never happens. The
            // engine's own watermark answers all three, so having both would
            // only be two triggers disagreeing about one decision.
            PlayerEvent::Unavailable {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                self.fail_current_load(track_id)
            }
            // A failed *preload*. librespot stamps this with the play request
            // of the track currently playing but the track id of the one it
            // was fetching ahead (`player.rs`, the `PlayerPreload::Loading`
            // error arm), so it matched neither the current row nor the
            // ignored-event log's expectations, and fell silently through both
            // — which is how a whole class of key refusals stayed invisible to
            // the breaker that exists to count them. It is still evidence
            // about the service, so it counts; it says nothing about the track
            // that is playing fine, so nothing else here moves.
            PlayerEvent::Unavailable {
                play_request_id,
                track_id,
            } if self.play_request_id == Some(play_request_id) => {
                let burst = self.record_unavailable(Instant::now());
                eprintln!(
                    "transport: preload failed for {track_id} ({} in this run); holding further preloads back",
                    burst.consecutive,
                );
                false
            }
            PlayerEvent::Stopped {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                // The engine's own stops (an exhausted queue, a drained one)
                // have already said so; only a stop that ends playback is news.
                self.finalize_listening(false);
                std::mem::replace(&mut self.state.playing, false)
            }
            // Every arm above is gated on `is_current_event`, so a transport
            // event landing here is one the engine refused: its play request or
            // its current row no longer describes what librespot is doing. That
            // is the difference between "the engine went deaf to its player"
            // and "no command ever arrived", which the log could not previously
            // tell apart — both look like silence.
            event @ (PlayerEvent::Playing { .. }
            | PlayerEvent::Paused { .. }
            | PlayerEvent::EndOfTrack { .. }
            | PlayerEvent::Stopped { .. }
            | PlayerEvent::Unavailable { .. }) => {
                eprintln!(
                    "transport: ignored {event:?}; engine holds play request {:?}",
                    self.play_request_id
                );
                false
            }
            _ => false,
        }
    }

    fn is_current_event(&self, play_request_id: u64, uri: &SpotifyUri) -> bool {
        if self.play_request_id != Some(play_request_id) {
            return false;
        }
        let Some(index) = self.state.current_index else {
            return false;
        };
        let Some(track) = self.state.queue.get(index) else {
            return false;
        };
        uri.to_uri().is_ok_and(|value| value == track.uri)
    }

    fn shutdown_playback(&mut self) {
        self.cancel_cache_evictions();
        self.finalize_listening(false);
        self.stop_playback_handles();
    }

    fn stop_playback_handles(&mut self) {
        self.detach_player();
        self.recent_unavailable.clear();
        if let Some(session) = self.session.take() {
            session.shutdown();
        }
    }

    /// Drops the installed player and everything that only made sense with it,
    /// keeping the session: the queue, the playhead and browsing do not depend
    /// on the audio device, and a player attached to a stream that can never
    /// drain again would do nothing for a transport command but lie about it.
    ///
    /// Events still in flight from this player are discarded from here on.
    /// The caller owns any generation bump that must also retire answers in
    /// flight for it — see [`Engine::enter_audio_unavailable`].
    fn detach_player(&mut self) {
        // Capture the consumer clock (or current projection) while the old
        // pipeline still exists. Replacement loads preserve this loop pass.
        self.tick_position();
        self.audio_probe_generation = None;
        // Cancel a pending native open before dropping Player (which joins its
        // thread). The native worker is detached and never owns that join.
        crate::audio::pause_output();
        if let Some(player) = self.player.take() {
            player.stop();
        }
        // The pause its own failure caused and the close that follows its
        // thread's end are about a player this engine has already let go.
        self.player_generation = None;
        self.invalidate_audio_signals();
        self.play_request_id = None;
        self.loading_failed = false;
        self.current_needs_load = self.state.current_index.is_some();
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        self.recent_track_changes.clear();
        self.current_load_produced_audio = false;
        self.retry_current_at = None;
        self.current_load_is_retry = false;
        self.preload_armed = false;
        self.drain_stop_at = None;
        self.mixer = None;
    }
}

/// The first rows or order revision of a process.
///
/// A revision is the shell's and the webview's only way to tell an omitted
/// queue (or order) from an empty one, so it must name one generation and
/// nothing else — including across a respawn. A counter starting at one would
/// let a fresh engine's first, empty queue be mistaken for rows a previous
/// engine had already published under the same number; the wall clock costs
/// one call at construction and cannot repeat that way within any plausible
/// uptime.
///
/// Milliseconds, not nanoseconds, because the webview reads these fields as
/// JSON numbers — doubles, which hold integers exactly only to 2^53. An epoch
/// in nanoseconds is ~1.79e18, where the numbers that are representable are 256
/// apart: two generations one bump apart arrive as the same number, and the
/// webview keeps the rows of the first. Milliseconds are ~1.75e12, exact for
/// every value, and a bump of one stays exact through any uptime — so a
/// revision in flight always names one generation and no other.
fn initial_revision() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis() as u64)
        .unwrap_or(1)
        .max(1)
}

fn fill_queue_context(queue: &mut [TrackRef], fallback: &str) {
    let fallback = fallback.trim();
    if fallback.is_empty() {
        return;
    }
    for track in queue {
        if track.context.trim().is_empty() {
            track.context = fallback.to_owned();
        }
    }
}

fn with_preview_edit(
    mut track: TrackRef,
    cuts: Vec<TimeRange>,
    loop_range: Option<LoopRange>,
) -> Result<TrackRef, String> {
    if !matches!(parse_track_uri(&track)?, SpotifyUri::Track { .. }) {
        return Err("podcast episodes cannot have song edits".to_owned());
    }
    let edit = TrackEdit { cuts, loop_range };
    validate_definition(&track.id, track.duration_ms, &edit.cuts, edit.loop_range)?;
    track.effective_edit = Some(edit);
    Ok(track)
}

fn parse_track_uri(track: &TrackRef) -> Result<SpotifyUri, String> {
    let uri = SpotifyUri::from_uri(&track.uri)
        .map_err(|error| format!("invalid Spotify audio URI '{}': {error}", track.uri))?;
    if !matches!(&uri, SpotifyUri::Track { .. } | SpotifyUri::Episode { .. }) {
        return Err(format!(
            "queue item is not a Spotify audio URI: {}",
            track.uri
        ));
    }
    Ok(uri)
}

fn playable_track_uri(track: &TrackRef) -> Result<SpotifyUri, String> {
    if track.unavailable {
        return Err(track
            .unavailable_reason
            .clone()
            .unwrap_or_else(|| "this track is permanently unavailable".to_owned()));
    }
    parse_track_uri(track)
}

fn automatic_playlist_id(context: &str) -> Option<&str> {
    let playlist_id = context.strip_prefix("playlist:")?;
    (!playlist_id.trim().is_empty()).then_some(playlist_id)
}

fn automatic_track_eligible(store: &TrackEditStore, track: &TrackRef) -> bool {
    if track.unavailable {
        return false;
    }
    let Some(playlist_id) = automatic_playlist_id(&track.context) else {
        return true;
    };
    !store
        .is_excluded(playlist_id, &track.id)
        .is_ok_and(|excluded| excluded)
}

fn first_automatic_from(queue: &[TrackRef], start: usize, store: &TrackEditStore) -> Option<usize> {
    (start..queue.len()).find(|index| automatic_track_eligible(store, &queue[*index]))
}

fn first_automatic_wrapping(
    queue: &[TrackRef],
    start: usize,
    store: &TrackEditStore,
) -> Option<usize> {
    first_automatic_from(queue, start, store).or_else(|| {
        (0..start.min(queue.len())).find(|index| automatic_track_eligible(store, &queue[*index]))
    })
}

fn sequential_automatic_index(
    queue: &[TrackRef],
    current: usize,
    repeat: RepeatMode,
    store: &TrackEditStore,
) -> Option<usize> {
    first_automatic_from(queue, current.saturating_add(1), store).or_else(|| {
        (repeat == RepeatMode::Context)
            .then(|| {
                (0..=current.min(queue.len().saturating_sub(1)))
                    .find(|index| automatic_track_eligible(store, &queue[*index]))
            })
            .flatten()
    })
}

fn first_available_from(queue: &[TrackRef], start: usize) -> Option<usize> {
    (start..queue.len()).find(|index| !queue[*index].unavailable)
}

fn first_available_wrapping(queue: &[TrackRef], start: usize) -> Option<usize> {
    first_available_from(queue, start)
        .or_else(|| (0..start.min(queue.len())).find(|index| !queue[*index].unavailable))
}

/// What the user is told when the output stopped draining mid-track.
///
/// Deliberately not the "no output device" wording yet: all the engine knows at
/// this point is that nothing has consumed the audio ring for a couple of
/// seconds, and the device may still be there — a driver restart, a USB
/// re-enumeration, a display that went to sleep. The probe that follows asks
/// the device list before it builds anything: with nothing there it fixes
/// nothing and says nothing, and with a device there the construction it starts
/// either clears this message or replaces it with the reason the open failed.
fn audio_stalled_message() -> String {
    "the audio output stopped responding; playback resumes by itself when an output device is \
     available"
        .to_owned()
}

/// Whether entering the no-output-device state is worth a log line.
///
/// Only the transition is news. Every failed probe re-enters this state, so a
/// machine that has been without audio since boot used to write one line per
/// probe — thousands a day at the ceiling, into a log that rotates at 4 MiB —
/// and ate the history that would explain the outages worth reading. The
/// steady-state retries stay silent; the message itself is not lost, because it
/// lives in `state.error`, which every state event carries and the UI shows.
/// A *different* message is a different condition worth a line: `no audio
/// output device` after `the audio output stopped responding` says the driver
/// finally answered, or that a plugged-back-in device refused the open.
fn audio_unavailable_is_news(previous: Option<&str>, message: &str) -> bool {
    previous != Some(message)
}

#[cfg(test)]
fn sequential_available_index(
    queue: &[TrackRef],
    current: usize,
    repeat: RepeatMode,
) -> Option<usize> {
    first_available_from(queue, current.saturating_add(1)).or_else(|| {
        (repeat == RepeatMode::Context)
            .then(|| {
                (0..=current.min(queue.len().saturating_sub(1)))
                    .find(|index| !queue[*index].unavailable)
            })
            .flatten()
    })
}

#[cfg(test)]
fn sequential_next_index(current: usize, queue_len: usize, repeat: RepeatMode) -> Option<usize> {
    if current + 1 < queue_len {
        Some(current + 1)
    } else if repeat == RepeatMode::Context && queue_len > 0 {
        Some(0)
    } else {
        None
    }
}

fn remap_current_index_after_move(current: usize, from: usize, to: usize) -> usize {
    if current == from {
        to
    } else if from < current && to >= current {
        current - 1
    } else if from > current && to <= current {
        current + 1
    } else {
        current
    }
}

/// The time to wait before the next command-driven track change may start:
/// zero when none is needed (no prior change, or the interval has already
/// elapsed). Pure so pacing is unit-testable without a real timer.
fn track_change_wait(last: Option<Instant>, now: Instant, interval: Duration) -> Duration {
    let Some(t) = last else {
        return Duration::ZERO;
    };
    let elapsed = now.checked_duration_since(t).unwrap_or_default();
    interval.saturating_sub(elapsed)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    use super::{
        AUDIO_BLOCKED_PROBE_BACKOFF_MAX, AUDIO_PROBE_BACKOFF_MAX, AUDIO_PROBE_BACKOFF_MIN,
        AudioSignal, AuthFailure, AuthSignal, ConnectedSession, Engine, LOAD_RETRY_BACKOFF,
        PRELOAD_WATERMARK_MS, PlaybackHandles, PlaybackSpeeds, PlaybackState, PlayerSignal,
        RECONNECT_BACKOFF_MAX,
        RECONNECT_BACKOFF_MIN, TRACK_CHANGE_BURST_WINDOW, TRACK_CHANGE_MIN_INTERVAL,
        UNAVAILABLE_BURST_WINDOW, UNAVAILABLE_STOP_LIMIT, VOLUME_PERSIST_QUIET,
        audio_unavailable_is_news, automatic_track_eligible, first_automatic_from,
        first_automatic_wrapping, first_available_from, first_available_wrapping,
        remap_current_index_after_move, sequential_automatic_index, sequential_available_index,
        sequential_next_index, track_change_wait, with_preview_edit,
    };
    use crate::audio::{self, OutputError};
    use crate::auth::{PlaybackError, create_playback, percent_to_volume};
    use crate::customization::TrackEditStore;
    use crate::io::ProtocolWriter;
    use librespot_core::SpotifyUri;
    use librespot_playback::audio_backend::{Sink, SinkResult};
    use librespot_playback::config::PlayerConfig;
    use librespot_playback::convert::Converter;
    use librespot_playback::decoder::AudioPacket;
    use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume, softmixer::SoftMixer};
    use librespot_playback::player::{Player, PlayerEvent};
    use renderer_engine::protocol::{AuthState, Command, LoopRange, RepeatMode, TimeRange, TrackEdit, TrackRef};


    #[test]
    fn eviction_invalidates_marks_even_on_failure_and_new_load_cancels_late_completion() {
        let (mut engine, _) = test_engine();
        let id = "0123456789ABCDEFGHIJKL";
        let uri = SpotifyUri::from_uri(&format!("spotify:track:{id}")).unwrap();
        engine.state.queue = vec![TrackRef { id: id.into(), uri: format!("spotify:track:{id}"),
            cached: true, ..Default::default() }];
        engine.cached_marks.push(id.into());
        engine.invalidate_cache_mark(id);
        assert!(!engine.state.queue[0].cached);
        assert!(engine.cached_marks.is_empty());
        assert_eq!(engine.uncached_marks, [id]);
        let lease = Arc::new(parking_lot::Mutex::new(true));
        engine.cache_evictions.insert(id.into(), Arc::clone(&lease));
        // No usable file after a failed eviction may regain an old positive.
        let signal = super::CacheSignal { id: id.into(), generation: engine.generation, lease: Arc::clone(&lease) };
        assert!(engine.on_cache_signal(signal));
        assert!(engine.invalidated_cache_ids.contains(id));
        engine.cache_evictions.insert(id.into(), Arc::clone(&lease));
        engine.prepare_cache_reload(&uri);
        assert!(!*lease.lock());
        engine.invalidated_cache_ids.remove(id);
        engine.state.queue[0].cached = true;
        let late = super::CacheSignal { id: id.into(), generation: engine.generation, lease };
        assert!(!engine.on_cache_signal(late));
        assert!(engine.state.queue[0].cached);
    }

    #[test]
    fn eviction_from_previous_generation_cannot_invalidate_new_session_rows() {
        let (mut engine, _) = test_engine();
        let lease = Arc::new(parking_lot::Mutex::new(true));
        engine.cache_evictions.insert("a".into(), Arc::clone(&lease));
        engine.state.queue = vec![TrackRef { id: "a".into(), cached: true, ..Default::default() }];
        let generation = engine.generation;
        engine.generation = generation.wrapping_add(1);
        assert!(!engine.on_cache_signal(super::CacheSignal { id: "a".into(), generation, lease }));
        assert!(engine.state.queue[0].cached);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn idle_and_paused_engines_have_no_work_timer() {
        let (mut engine, _) = test_engine();
        assert_eq!(engine.next_work_deadline(), None);
        let (mut engine, _probe) = engine_with_player();
        assert_eq!(engine.next_work_deadline(), None);
        engine.state.playing = true;
        let deadline = engine.next_work_deadline().expect("active session observation");
        assert_eq!(engine.next_work_deadline(), Some(deadline), "commands do not postpone observation");
        engine.state.playing = false;
        assert_eq!(engine.next_work_deadline(), None);

        // Play intended over nothing audible: an armed reconnect answers on
        // its own deadline...
        engine.state.playing = true;
        engine.awaiting_transport_retry = true;
        let reconnect = Instant::now() + RECONNECT_BACKOFF_MAX;
        engine.next_reconnect = Some(reconnect);
        assert_eq!(engine.next_work_deadline(), Some(reconnect));
        note_audio(&mut engine);
        assert!(engine.next_work_deadline().is_some_and(|due| due < reconnect),
            "a preserved player still sounding keeps its playhead lane");
        // ...and a machine without an output device waits for its probe.
        engine.awaiting_transport_retry = false;
        engine.next_reconnect = None;
        engine.player = None;
        assert_eq!(engine.next_work_deadline(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn paused_audio_recovery_keeps_a_deadline_only_with_a_live_session() {
        let (mut engine, _) = test_engine();
        engine.session = Some(librespot_core::Session::new(librespot_core::SessionConfig::default(), None));
        let retry_at = Instant::now() + AUDIO_PROBE_BACKOFF_MIN;
        engine.audio_unavailable = Some(super::AudioUnavailable {
            message: "recovering output".to_owned(),
            retry_at: Some(retry_at),
        });
        assert_eq!(engine.next_work_deadline(), Some(retry_at));
        engine.session = None;
        assert_eq!(engine.next_work_deadline(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_inflight_device_probe_does_not_rearm_on_an_expired_deadline() {
        let (mut engine, _) = test_engine();
        engine.session = Some(librespot_core::Session::new(librespot_core::SessionConfig::default(), None));
        engine.state = playback_state(240_000);
        engine.audio_unavailable = Some(super::AudioUnavailable {
            message: "opening".to_owned(),
            retry_at: Some(Instant::now() - Duration::from_millis(1)),
        });
        engine.audio_probe_generation = Some(engine.generation);
        assert_eq!(engine.next_work_deadline(), None);
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(!engine.tick_audio_device(&sender));
        let enabled = !engine.normalisation.load(Ordering::Acquire);
        assert!(engine.set_normalisation(enabled, &sender));
        assert_eq!(engine.audio_probe_generation, Some(engine.generation));
        assert!(engine.on_default_output_changed());
        assert_eq!(engine.audio_probe_generation, None);
        assert!(engine.next_work_deadline().is_some());
    }

    #[test]
    fn volume_and_future_reconnects_keep_only_their_actual_deadlines() {
        let (mut engine, _) = test_engine();
        engine.set_volume(65).unwrap();
        let volume_due = engine.pending_volume.as_ref().unwrap().changed_at + VOLUME_PERSIST_QUIET;
        assert_eq!(engine.next_work_deadline(), Some(volume_due));
        settle_volume(&mut engine);
        engine.tick_volume_persist();
        assert_eq!(engine.next_work_deadline(), None);
        let reconnect = Instant::now() + RECONNECT_BACKOFF_MIN;
        engine.awaiting_transport_retry = true;
        engine.next_reconnect = Some(reconnect);
        assert_eq!(engine.next_work_deadline(), Some(reconnect));
        engine.auth_running = true;
        assert_eq!(engine.next_work_deadline(), None, "in-flight auth is event-driven");
    }

    #[test]
    fn pausing_cancels_a_load_retry_timer_without_idle_polling() {
        let (mut engine, _) = test_engine();
        let retry = Instant::now() + LOAD_RETRY_BACKOFF;
        engine.retry_current_at = Some(retry);
        engine.state.playing = true;
        assert_eq!(engine.next_work_deadline(), Some(retry));
        engine.state.playing = false;
        assert_eq!(engine.next_work_deadline(), None);
    }

    #[test]
    fn queue_metadata_uses_requested_exclusion_context_and_the_live_plan() {
        let excluded = "0abcdefghijklmnopqrstu";
        let eligible = "1abcdefghijklmnopqrstu";
        let (directory, store) = store_with_exclusions(&[("one", excluded)]);
        let (mut engine, _) = test_engine();
        engine.track_edits = store;
        engine.state.queue = vec![
            contextual_track(eligible, "playlist:one"),
            contextual_track(excluded, "playlist:one"),
            contextual_track(eligible, "playlist:one"),
        ];
        engine.state.current_index = Some(0);
        let current = engine.queue_metadata(None).unwrap();
        assert_eq!(current.excluded_track_ids, [excluded]);
        assert_eq!(current.upcoming, [2]);
        let other = engine.queue_metadata(Some("playlist:two")).unwrap();
        assert!(other.excluded_track_ids.is_empty());
        assert_eq!(other.upcoming, [2]);
        drop(directory);
    }

    #[test]
    fn audio_queue_accepts_episodes_but_refuses_song_edits_for_them() {
        let episode = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:episode:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 12_345,
            ..TrackRef::default()
        };
        assert!(matches!(super::parse_track_uri(&episode), Ok(SpotifyUri::Episode { .. })));
        assert!(Engine::validate_queue(std::slice::from_ref(&episode), 0).is_ok());
        assert!(with_preview_edit(episode.clone(), Vec::new(), None).is_err());
        let mut with_edit = episode.clone();
        with_edit.effective_edit = Some(TrackEdit::default());
        assert!(Engine::validate_queue(&[with_edit], 0).is_err());
        let not_audio = TrackRef {
            uri: "spotify:show:0123456789ABCDEFGHIJKL".to_owned(),
            ..episode
        };
        assert!(super::parse_track_uri(&not_audio).is_err());
    }

    #[test]
    fn restoring_mixed_queue_clears_only_episode_download_marks() {
        let (mut engine, _) = test_engine();
        let song = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 12_345,
            cached: true,
            ..TrackRef::default()
        };
        let episode = TrackRef {
            id: "1123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:episode:1123456789ABCDEFGHIJKL".to_owned(),
            cached: true,
            ..song.clone()
        };

        engine
            .restore_queue(vec![song, episode], 0, 0, String::new(), 0, false, false, false)
            .expect("restore mixed queue");
        assert!(engine.state.queue[0].cached);
        assert!(!engine.state.queue[1].cached);
    }

    fn test_engine() -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        test_engine_in(PathBuf::new())
    }

    fn test_engine_in(state_directory: PathBuf) -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        test_engine_with_audio(state_directory, device_present(), device_always_present())
    }

    /// A machine that always has an output device, behind a sink that accepts
    /// everything: librespot needs a `Sink` and nothing in these tests listens
    /// to what it is handed. Tests that are *about* the device replace this
    /// with one they can take away (see [`TestAudioDevice`]).
    fn device_present() -> audio::SinkOpener {
        Arc::new(|_| Ok(Box::new(TestSink)))
    }

    /// The presence answer that goes with [`device_present`]: this machine has
    /// a default output device, every time it is asked.
    fn device_always_present() -> audio::DevicePresence {
        Arc::new(|| true)
    }

    fn test_engine_with_audio(
        state_directory: PathBuf,
        audio_device: audio::SinkOpener,
        audio_device_present: audio::DevicePresence,
    ) -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        test_engine_with_cache(
            state_directory,
            no_path_cache(),
            audio_device,
            audio_device_present,
        )
    }

    fn test_engine_with_cache(
        state_directory: PathBuf,
        cache: librespot_core::cache::Cache,
        audio_device: audio::SinkOpener,
        audio_device_present: audio::DevicePresence,
    ) -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        let (writer, buffer) = ProtocolWriter::capture();
        (
            Engine::new(
                writer,
                cache,
                PathBuf::new(),
                PathBuf::new(),
                state_directory,
                false,
                audio_device,
                audio_device_present,
            ),
            buffer,
        )
    }

    /// Stands in for a machine's audio output: librespot needs a sink and
    /// nothing here listens to what it is handed.
    struct TestSink;

    impl Sink for TestSink {
        fn write(&mut self, _position: librespot_playback::decoder::AudioPacketPosition, _packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
            Ok(())
        }
    }

    /// A machine whose output device can be unplugged and plugged back in, and
    /// which counts what the engine asked of it. Both directions of the device
    /// bug turn on exactly these facts, and none of them is something real
    /// hardware can be asked to provide on cue.
    #[derive(Default)]
    struct TestAudioDevice {
        present: AtomicBool,
        /// Every ask for a sink, whether or not one was handed back. This is
        /// the observable form of "a player construction reached the device
        /// step" — the engine only gets here through `auth::create_playback`.
        asked: AtomicUsize,
        opened: AtomicUsize,
    }

    impl TestAudioDevice {
        /// A machine whose output device is not there.
        fn absent() -> Arc<Self> {
            Arc::new(Self::default())
        }

        /// A machine with a working output device.
        fn present() -> Arc<Self> {
            let device = Arc::new(Self::default());
            device.plug_in();
            device
        }

        fn plug_in(&self) {
            self.present.store(true, Ordering::Release);
        }

        fn unplug(&self) {
            self.present.store(false, Ordering::Release);
        }

        /// How many sinks were handed back.
        fn opened(&self) -> usize {
            self.opened.load(Ordering::Acquire)
        }

        /// How many times a player construction reached the device step.
        fn asked(&self) -> usize {
            self.asked.load(Ordering::Acquire)
        }

        fn opener(self: &Arc<Self>) -> audio::SinkOpener {
            let device = Arc::clone(self);
            Arc::new(move |_| {
                device.asked.fetch_add(1, Ordering::AcqRel);
                if !device.present.load(Ordering::Acquire) {
                    return Err(OutputError::NoDeviceAvailable);
                }
                device.opened.fetch_add(1, Ordering::AcqRel);
                Ok(Box::new(TestSink))
            })
        }

        /// The cheap answer that goes with [`Self::opener`]: whether cpal would
        /// find a default output device on this machine, asked without
        /// building anything.
        fn presence(self: &Arc<Self>) -> audio::DevicePresence {
            let device = Arc::clone(self);
            Arc::new(move || device.present.load(Ordering::Acquire))
        }
    }

    /// Watches one librespot player from the outside. The sink is destroyed
    /// with the player thread, so its liveness is the observable form of "that
    /// player is really gone" rather than "the engine no longer names it".
    struct SinkProbe {
        alive: Arc<AtomicBool>,
    }

    struct ProbeSink {
        alive: Arc<AtomicBool>,
    }

    impl SinkProbe {
        fn new() -> Self {
            Self {
                alive: Arc::new(AtomicBool::new(true)),
            }
        }

        fn sink(&self) -> ProbeSink {
            ProbeSink {
                alive: Arc::clone(&self.alive),
            }
        }

        fn is_alive(&self) -> bool {
            self.alive.load(Ordering::Acquire)
        }
    }

    impl Sink for ProbeSink {
        fn start(&mut self) -> SinkResult<()> {
            Ok(())
        }

        fn stop(&mut self) -> SinkResult<()> {
            Ok(())
        }

        fn write(&mut self, _position: librespot_playback::decoder::AudioPacketPosition, _packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
            Ok(())
        }
    }

    impl Drop for ProbeSink {
        fn drop(&mut self) {
            self.alive.store(false, Ordering::Release);
        }
    }

    /// A real librespot player with no audio device behind it, so the lifetime
    /// under test is librespot's own: the thread ends when the last `Arc` goes,
    /// and `Drop for Player` joins it before returning.
    fn probe_player(probe: &SinkProbe) -> (Arc<Player>, librespot_core::Session) {
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        let sink = probe.sink();
        let player = Player::new(
            PlayerConfig::default(),
            session.clone(),
            Box::new(NoOpVolume),
            move || Box::new(sink),
        );
        (player, session)
    }

    fn playback_handles(player: Arc<Player>) -> PlaybackHandles {
        let events = player.get_player_event_channel();
        PlaybackHandles {
            player,
            events,
            mixer: Arc::new(SoftMixer::open(MixerConfig::default()).expect("software mixer")),
            volume_percent: 50,
        }
    }

    /// The signal a successful cached connection produces: a live session with
    /// a player built against it.
    fn connected_handles(
        player: Arc<Player>,
        session: librespot_core::Session,
    ) -> ConnectedSession {
        ConnectedSession {
            session,
            playback: Ok(playback_handles(player)),
        }
    }

    fn track_ref() -> TrackRef {
        TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            ..TrackRef::default()
        }
    }

    fn track_uri() -> SpotifyUri {
        SpotifyUri::from_uri("spotify:track:0123456789ABCDEFGHIJKL").expect("valid track uri")
    }

    fn playback_state(duration_ms: u32) -> PlaybackState {
        PlaybackState {
            ready: true,
            auth_state: renderer_engine::protocol::AuthState::Ready,
            auth_url: None,
            playing: false,
            position_ms: 0,
            duration_ms,
            volume: 50,
            shuffle: false,
            repeat: RepeatMode::Off,
            speeds: PlaybackSpeeds::NORMAL,
            current_index: Some(0),
            queue: vec![TrackRef {
                duration_ms,
                ..track_ref()
            }],
            error: None,
        }
    }

    fn two_track_state() -> PlaybackState {
        let mut state = playback_state(240_000);
        state.queue.push(TrackRef {
            id: "0123456789ABCDEFGHIJKM".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKM".to_owned(),
            duration_ms: 180_000,
            ..TrackRef::default()
        });
        state
    }

    /// Long enough that a run of failures has somewhere to march to, which is
    /// what the circuit-breaker tests have to be able to observe it not doing.
    fn five_track_state() -> PlaybackState {
        let mut state = playback_state(240_000);
        for suffix in ['M', 'N', 'P', 'Q'] {
            let id = format!("0123456789ABCDEFGHIJK{suffix}");
            state.queue.push(TrackRef {
                uri: format!("spotify:track:{id}"),
                id,
                duration_ms: 180_000,
                ..TrackRef::default()
            });
        }
        state
    }

    fn edited_playback_state(
        duration_ms: u32,
        cuts: Vec<TimeRange>,
        loop_range: Option<LoopRange>,
    ) -> PlaybackState {
        let mut state = playback_state(duration_ms);
        state.queue[0].id = "0123456789ABCDEFGHIJKL".to_owned();
        state.queue[0].duration_ms = duration_ms;
        state.queue[0].effective_edit = Some(TrackEdit { cuts, loop_range });
        state
    }

    fn range(start_ms: u32, end_ms: u32) -> TimeRange {
        TimeRange { start_ms, end_ms }
    }

    fn loop_range(start_ms: u32, end_ms: u32, play_count: u32) -> LoopRange {
        LoopRange {
            start_ms,
            end_ms,
            play_count,
        }
    }

    fn playing_engine() -> Engine {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.play_request_id = Some(7);
        engine
    }

    /// A playing engine holding a real librespot player. Transport commands
    /// then reach a live command channel instead of failing on a missing
    /// player, which is what makes the pause and seek paths reachable at all.
    /// The probe is returned so the caller keeps the sink alive.
    fn engine_with_player() -> (Engine, SinkProbe) {
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        let mut engine = playing_engine();
        engine.player = Some(player);
        engine.session = Some(session);
        (engine, probe)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_reconnect_accepts_the_first_selection_and_only_loads_the_latest_target() {
        for (new_queue, supersede) in [(false, false), (true, false), (false, true), (true, true)] {
            let (mut engine, _probe) = engine_with_player();
            engine.state = two_track_state();
            engine.session.as_ref().unwrap().shutdown();
            assert_eq!(engine.prepare_session_reconnect(), Some(true));
            let generation = engine.begin_cached_authentication(false);
            let (auth_sender, _) = tokio::sync::mpsc::unbounded_channel();
            let command = if new_queue {
                Command::PlayQueue {
                    queue: engine.state.queue.clone(), index: 1, position_ms: 0,
                    context: String::new(), automatic_start: false,
                }
            } else {
                Command::PlayQueueIndex { index: 1 }
            };
            assert_eq!(engine.process_command(command, &auth_sender).await, Ok(true));
            assert_eq!(engine.state.current_index, Some(1));
            assert!(engine.state.playing && engine.current_needs_load);
            assert!(engine.player.is_none(), "acceptance must not load on the dead session");
            assert!(engine.process_command(Command::PlayQueueIndex { index: 99 }, &auth_sender)
                .await.is_err());
            assert_eq!(engine.state.current_index, Some(1), "invalid selection cannot replace intent");
            if supersede {
                assert_eq!(engine.process_command(Command::PlayQueueIndex { index: 0 }, &auth_sender)
                    .await, Ok(true));
            }
            let expected_index = if supersede { 0 } else { 1 };
            let expected_track = SpotifyUri::from_uri(&engine.state.queue[expected_index].uri).unwrap();

            let replacement = SinkProbe::new();
            let (player, session) = probe_player(&replacement);
            let (player_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
            assert!(engine.on_auth_signal(AuthSignal::Complete {
                generation, result: Ok(connected_handles(player, session)),
            }, player_sender));
            assert_eq!(engine.state.current_index, Some(expected_index));
            assert!(engine.state.playing && !engine.current_needs_load);
            let loaded = tokio::time::timeout(Duration::from_secs(10), async {
                while let Some(signal) = events.recv().await {
                    if let PlayerSignal::Event {
                        event: PlayerEvent::Loading { track_id, position_ms, .. }, ..
                    } = signal {
                        return (track_id, position_ms);
                    }
                }
                panic!("replacement did not report its load");
            }).await.unwrap();
            assert_eq!(loaded, (expected_track, 0));
            engine.shutdown();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reconnect_pause_cancels_selection_and_transport_retry_retains_it() {
        for pause in [false, true] {
            let (mut engine, _probe) = engine_with_player();
            engine.state = two_track_state();
            let generation = engine.begin_cached_authentication(false);
            let (auth_sender, _) = tokio::sync::mpsc::unbounded_channel();
            assert_eq!(engine.process_command(Command::PlayQueueIndex { index: 1 }, &auth_sender)
                .await, Ok(true));
            let (player_sender, _) = tokio::sync::mpsc::unbounded_channel();
            assert!(engine.on_auth_signal(AuthSignal::Complete {
                generation, result: Err(AuthFailure::Unreachable("network offline".to_owned())),
            }, player_sender.clone()));
            assert!(engine.state.playing && engine.current_needs_load);
            assert_eq!(engine.state.error.as_deref(), Some("network offline"));
            if pause {
                assert_eq!(engine.process_command(Command::Pause, &auth_sender).await, Ok(true));
                assert!(!engine.state.playing);
                assert_eq!(engine.state.error.as_deref(), Some("network offline"));
            }
            let generation = engine.begin_cached_authentication(false);
            assert_eq!(engine.state.playing, !pause, "retry teardown must retain current intent");
            let replacement = SinkProbe::new();
            let (player, session) = probe_player(&replacement);
            assert!(engine.on_auth_signal(AuthSignal::Complete {
                generation, result: Ok(connected_handles(player, session)),
            }, player_sender));
            assert_eq!(engine.state.current_index, Some(1));
            assert_eq!(engine.state.playing, !pause);
            assert_eq!(engine.current_needs_load, pause);
            engine.shutdown();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejected_authentication_cancels_accepted_selection() {
        let (mut engine, _probe) = engine_with_player();
        let generation = engine.begin_cached_authentication(false);
        let (auth_sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(engine.process_command(Command::PlayQueueIndex { index: 0 }, &auth_sender)
            .await, Ok(true));
        let (player_sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(AuthSignal::Complete {
            generation, result: Err(AuthFailure::Rejected("grant rejected".to_owned())),
        }, player_sender));
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.state.queue.is_empty());
        assert_eq!(engine.state.auth_state, AuthState::NeedsLogin);
        assert_eq!(engine.state.error.as_deref(), Some("grant rejected"));
        assert_eq!(engine.next_work_deadline(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn newer_pause_survives_old_player_echoes_and_stalled_output_recovery() {
        let (mut engine, _probe) = engine_with_player();
        engine.state.playing = true;
        engine.update_position(42_000);
        note_audio(&mut engine);
        assert_eq!(engine.pause(), Ok(true));
        let position = engine.state.position_ms;
        for event in [
            PlayerEvent::Playing { play_request_id: 7, track_id: track_uri(), position_ms: 1 },
            PlayerEvent::Paused { play_request_id: 7, track_id: track_uri(), position_ms: 1 },
        ] {
            engine.on_player_event(event);
            assert!(!engine.state.playing);
            assert_eq!(engine.state.position_ms, position);
        }
        assert!(engine.on_audio_signal(AudioSignal::OutputStalled {
            revision: engine.audio_revision,
        }));
        assert!(!engine.state.playing);
        assert!(engine.audio_unavailable.is_some() && engine.current_needs_load);
        let replacement = SinkProbe::new();
        let (player, _) = probe_player(&replacement);
        let (player_sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(AuthSignal::PlayerRebuilt {
            generation: engine.generation, normalisation: false,
            result: Ok(playback_handles(player)),
        }, player_sender));
        assert!(!engine.state.playing);
        assert_eq!(engine.state.position_ms, position);
        assert!(engine.current_needs_load, "recovery must not start paused audio");
        engine.shutdown();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn output_and_normalisation_handover_capture_current_playhead_and_loop_pass() {
        for (output_loss, paused) in [(false, false), (true, false), (false, true), (true, true)] {
            let (mut engine, _probe) = engine_with_player();
            engine.state = edited_playback_state(100_000, Vec::new(),
                Some(loop_range(20_000, 40_000, 3)));
            engine.state.playing = !paused;
            engine.loop_pass = 2;
            engine.current_load_produced_audio = true;
            engine.audio_revision = u64::MAX;
            engine.update_position(25_000);
            engine.position_anchor = Some((25_000, Instant::now() - Duration::from_secs(2)));
            if output_loss {
                assert!(engine.on_audio_signal(AudioSignal::OutputStalled {
                    revision: engine.audio_revision,
                }));
            } else {
                engine.normalisation.store(true, Ordering::Release);
            }
            let replacement = SinkProbe::new();
            let (player, _) = probe_player(&replacement);
            let (player_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
            assert!(engine.on_auth_signal(AuthSignal::PlayerRebuilt {
                generation: engine.generation, normalisation: !output_loss,
                result: Ok(playback_handles(player)),
            }, player_sender));
            if paused {
                assert_eq!(engine.state.position_ms, 25_000);
                assert!(!engine.state.playing && engine.current_needs_load);
                assert_eq!(engine.play(), Ok(true));
            } else {
                assert!((27_000..28_000).contains(&engine.state.position_ms),
                    "handover must sample the replacement instant, not stale heartbeat");
            }
            assert_eq!(engine.loop_pass, 2);
            receive_load(&mut events).await;
            // Only one repeat remains. Resetting pass one would permit an
            // extra audible pass at the next marker.
            assert!(engine.on_audio_signal(AudioSignal::LoopBoundary {
                revision: engine.audio_revision, position_ms: 20_000,
            }));
            assert_eq!(engine.loop_pass, 3);
            assert!(!engine.on_audio_signal(AudioSignal::LoopBoundary {
                revision: engine.audio_revision, position_ms: 20_000,
            }));
            engine.shutdown();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn late_natural_eof_advances_a_paused_queue_without_starting_it() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = two_track_state();
        engine.state.playing = true;
        note_audio(&mut engine);
        engine.update_position(engine.state.duration_ms);
        assert_eq!(engine.pause(), Ok(true));
        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7, track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(1));
        assert!(!engine.state.playing);
        engine.shutdown();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_idle_invalid_session_reconnects_before_play_and_keeps_resume_intent() {
        let (mut engine, _probe) = engine_with_player();
        engine.state.playing = false;
        engine.session.as_ref().unwrap().shutdown();
        assert_eq!(engine.next_work_deadline(), None);
        // This is the same on-demand check the actual command loop runs.
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(engine.prepare_session_reconnect(), Some(true));
        // Drive the real authentication transition without spawning a network
        // task. No credentials or Spotify requests are needed for this proof.
        engine.begin_cached_authentication(false);
        assert!(engine.auth_running);
        assert!(engine.process_command(Command::Play, &sender).await.unwrap());
        assert!(engine.state.playing);
        engine.shutdown();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_callback_proof_resets_the_probe_ladder_even_after_pause() {
        let (mut engine, _probe) = engine_with_player();
        engine.state.playing = false;
        engine.audio_probe_backoff = AUDIO_PROBE_BACKOFF_MAX;
        assert!(!engine.on_audio_signal(AudioSignal::OutputReady { revision: engine.audio_revision.wrapping_sub(1) }));
        assert_eq!(engine.audio_probe_backoff, AUDIO_PROBE_BACKOFF_MAX, "a retired pipeline proves nothing");
        assert!(!engine.on_audio_signal(AudioSignal::OutputReady { revision: engine.audio_revision }),
            "the proof changes nothing the listener can see");
        assert_eq!(engine.audio_probe_backoff, AUDIO_PROBE_BACKOFF_MIN);
        assert!(!engine.state.playing);
    }

    /// librespot pauses itself on a refused write and reports an ordinary
    /// pause; the sink's signal is what keeps that from reading as playback.
    #[test]
    fn an_unplayable_packet_fails_the_row_instead_of_playing_silence() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        note_audio(&mut engine);
        assert!(!engine.on_audio_signal(AudioSignal::DecodeFailed {
            revision: engine.audio_revision.wrapping_sub(1),
        }));
        assert!(!engine.loading_failed, "a retired pipeline's packet says nothing");

        assert!(engine.on_audio_signal(AudioSignal::DecodeFailed { revision: engine.audio_revision }));
        assert!(engine.loading_failed, "the row failed, so its playhead freezes");
        assert!(engine.state.playing, "the retry carries the play intent");
        assert!(engine.retry_current_at.is_some(), "the row gets its one retry");
        assert!(engine.audio_unavailable.is_none(), "the output is not blamed");
    }

    #[test]
    fn canonical_remote_cursor_consumes_draws_and_retains_rows_and_history() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);
        engine.state.playing = false;
        let rows = engine.rows_revision;
        engine.set_queue_cursor(2, 12_000).unwrap();
        assert_eq!(engine.state.current_index, Some(2));
        assert_eq!(engine.history, vec![0, 1]);
        assert_eq!(engine.upcoming_indices(), vec![3]);
        assert_eq!(engine.rows_revision, rows);
        assert_eq!(engine.state.position_ms, 12_000);
        assert!(engine.current_needs_load);
        engine.set_queue_cursor(1, 1_000).unwrap();
        assert_eq!(engine.history, vec![0]);
        assert_eq!(engine.upcoming_indices(), vec![2, 3]);
        engine.state.repeat = RepeatMode::Context;
        engine.set_queue_cursor(3, 0).unwrap();
        assert_eq!(engine.history, vec![0, 1, 2]);
        assert_eq!(engine.upcoming_indices().len(), 3);
        assert!(!engine.upcoming_indices().contains(&3));
        let current = engine.state.current_index;
        assert!(engine.set_queue_cursor(99, 0).is_err());
        assert_eq!(engine.state.current_index, current);
        drop(directory);
    }

    /// The cursor is a fresh position: the pass a departed loop had reached
    /// must not make Play (or a speed change before it) skip the next loop.
    #[test]
    fn remote_cursor_starts_the_loop_pass_of_its_own_position() {
        let (mut engine, _) = test_engine();
        engine.state = two_track_state();
        for track in &mut engine.state.queue {
            track.effective_edit = Some(TrackEdit {
                cuts: Vec::new(),
                loop_range: Some(loop_range(20_000, 40_000, 3)),
            });
        }
        engine.loop_pass = 3;

        engine.set_queue_cursor(1, 0).unwrap();
        assert_eq!(engine.loop_pass, 1, "the next row's loop starts at its first pass");
        engine.set_playback_speed(1.5).unwrap();
        assert_eq!(engine.loop_pass, 1, "a speed change keeps the cursor's pass");
        assert!(engine.play().is_err(), "the capture engine has no player");
        assert_eq!(engine.loop_pass, 1, "Play loads the row at the cursor's pass");

        engine.set_queue_cursor(1, 50_000).unwrap();
        assert_eq!(engine.loop_pass, 3, "a cursor past the loop end is the completed pass");
    }

    #[test]
    fn queue_metadata_corrections_publish_once_and_empty_orders_clear_the_plan() {
        let (mut engine, buffer) = test_engine();
        engine.state = two_track_state();
        engine.state.queue.push(engine.state.queue[0].clone());
        engine.emit_state().unwrap();
        let uri = engine.state.queue[0].uri.clone();
        assert!(engine.update_loaded_metadata(&uri, 180_000, "corrected"));
        engine.emit_state().unwrap();
        assert!(!engine.update_loaded_metadata(&uri, 180_000, "corrected"));
        engine.emit_state().unwrap();
        engine.set_queue_cursor(2, 0).unwrap();
        engine.emit_state().unwrap();
        let values = captured_messages(&buffer);
        assert_eq!(values[1]["queue"][0]["name"], "corrected");
        assert_eq!(values[1]["queue"][2]["duration_ms"], 180_000);
        assert_ne!(values[0]["queue_revision"], values[1]["queue_revision"]);
        assert!(values[2].get("queue").is_none());
        assert!(values[2].get("upcoming").is_none());
        assert!(values[3].get("queue").is_none());
        assert_eq!(values[3]["upcoming"], serde_json::json!([]));
        assert_ne!(values[2]["order_revision"], values[3]["order_revision"]);
    }
    /// Fails the load of whatever row is current, the way librespot does:
    /// with the play request the engine is holding and the row's own uri.
    fn fail_current_row(engine: &mut Engine, play_request_id: u64) -> bool {
        let index = engine.state.current_index.expect("a current row");
        let track_id =
            SpotifyUri::from_uri(&engine.state.queue[index].uri).expect("queue uris are valid");
        engine.play_request_id = Some(play_request_id);
        engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id,
            track_id,
        })
    }

    /// Brings an armed retry forward to now and runs the tick that fires it.
    fn fire_due_retry(engine: &mut Engine) {
        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_playback_health(), "the armed retry fires");
    }

    /// What "this track is really playing" looks like: the sink reports the
    /// decoder produced a packet for the pipeline this load configured.
    /// Anything simulating audible playback owes the engine this, because it
    /// is the only evidence the engine accepts that a track is not silent.
    fn note_audio(engine: &mut Engine) {
        let revision = engine.audio_revision;
        engine.on_audio_signal(AudioSignal::Output { revision });
    }

    fn hear_audio(engine: &mut Engine) {
        note_audio(engine);
        engine.tick_playback_health();
    }

    fn preview_history_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "renderer-engine-preview-history-{}-{ordinal}",
            std::process::id()
        ))
    }

    fn history_playing_engine() -> (Engine, PathBuf) {
        let root = preview_history_root();
        let (mut engine, _) = test_engine_in(root.clone());
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.state.queue[0].id = "0123456789ABCDEFGHIJKL".to_owned();
        engine.state.queue[0].duration_ms = 240_000;
        engine.play_request_id = Some(7);
        (engine, root)
    }

    /// The archive keeps plays that were listened to, so a test that wants a
    /// real row has to supply real listening rather than a touch.
    fn listened(engine: &mut Engine) {
        engine.listening_history.pretend_listened();
    }

    fn history_rows(engine: &Engine) -> Vec<renderer_engine::protocol::HistoryItem> {
        engine
            .history_reader()
            .page(&renderer_engine::protocol::HistoryQuery {
                limit: usize::MAX,
                ..renderer_engine::protocol::HistoryQuery::default()
            })
            .unwrap()
            .items
    }

    fn unavailable_track(id: &str) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            duration_ms: 180_000,
            unavailable: true,
            unavailable_reason: Some("not available in your country".to_owned()),
            ..TrackRef::default()
        }
    }

    struct TempStateDirectory(PathBuf);

    impl TempStateDirectory {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "renderer-engine-exclusion-{}-{ordinal}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempStateDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store_with_exclusions(exclusions: &[(&str, &str)]) -> (TempStateDirectory, TrackEditStore) {
        let directory = TempStateDirectory::new();
        let mut store = TrackEditStore::load(directory.path()).expect("empty edit store");
        for &(playlist_id, track_id) in exclusions {
            store
                .set_excluded(playlist_id, track_id, true)
                .expect("persist exclusion");
        }
        (directory, store)
    }

    fn contextual_track(id: &str, context: &str) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            duration_ms: 180_000,
            context: context.to_owned(),
            ..TrackRef::default()
        }
    }

    fn engine_with_queue(store: TrackEditStore, queue: Vec<TrackRef>, current: usize) -> Engine {
        let (mut engine, _) = test_engine();
        let duration_ms = queue[current].duration_ms;
        engine.track_edits = store;
        engine.state.queue = queue;
        engine.state.current_index = Some(current);
        engine.state.duration_ms = duration_ms;
        engine.state.position_ms = 0;
        engine.state.playing = true;
        engine
    }

    #[test]
    fn restore_installs_a_paused_unloaded_playhead_and_skips_a_blocked_index() {
        let (mut engine, _) = test_engine();
        engine.state.ready = true;
        let playable = TrackRef {
            uri: "spotify:track:1abcdefghijklmnopqrstu".to_owned(),
            duration_ms: 240_000,
            ..TrackRef::default()
        };
        let queue = vec![
            unavailable_track("0abcdefghijklmnopqrstu"),
            playable.clone(),
        ];

        assert_eq!(
            engine.restore_queue(queue, 0, 42_000, String::new(), 0, false, false, false),
            Ok(true)
        );
        assert_eq!(engine.state.current_index, Some(1));
        assert_eq!(
            engine.state.position_ms, 0,
            "a skipped seed cannot keep its seek"
        );
        assert_eq!(engine.state.duration_ms, playable.duration_ms);
        assert!(!engine.state.playing);
        assert!(engine.current_needs_load);
        assert!(engine.play_request_id.is_none());
    }

    #[test]
    fn guarded_restore_is_a_noop_after_real_queue_wins() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        let original_queue = engine.state.queue.clone();

        // The stale snapshot is intentionally invalid: a guarded restore
        // that has already lost the preview race must not even validate it.
        let result = engine.restore_queue(
            vec![TrackRef::default()],
            4,
            12_000,
            String::new(),
            1,
            true,
            true,
            false,
        );

        assert_eq!(result, Ok(false));
        assert_eq!(engine.state.queue.len(), original_queue.len());
        assert_eq!(engine.state.queue[0].uri, original_queue[0].uri);
        assert!(engine.state.playing);
        assert!(!engine.preview_mode);
    }

    #[test]
    fn stale_preview_restore_cannot_overwrite_new_preview() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 22;
        let original_uri = engine.state.queue[0].uri.clone();

        let result = engine.restore_queue(
            vec![TrackRef::default()],
            4,
            12_000,
            String::new(),
            11,
            true,
            false,
            false,
        );

        assert_eq!(result, Ok(false));
        assert!(engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 22);
        assert_eq!(engine.state.queue[0].uri, original_uri);
        assert!(engine.state.playing);
    }

    #[test]
    fn failed_preview_replacement_transfers_restore_ownership() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 1;
        let original_track = engine.state.queue[0].clone();

        let error = engine
            .preview_track_edit(
                original_track.clone(),
                vec![range(10_000, 20_000), range(15_000, 25_000)],
                None,
                5_000,
                2,
            )
            .expect_err("overlapping replacement draft must be rejected");
        assert!(error.contains("overlap") || error.contains("sorted"));
        assert!(engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 2);
        assert_eq!(engine.state.queue[0].uri, original_track.uri);

        assert_eq!(
            engine.restore_queue(
                vec![original_track],
                0,
                42_000,
                String::new(),
                2,
                true,
                false,
                false,
            ),
            Ok(true)
        );
        assert!(!engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 0);
    }

    #[test]
    fn failed_first_preview_does_not_enter_preview_or_claim_lease() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        let original_uri = engine.state.queue[0].uri.clone();
        let track = engine.state.queue[0].clone();

        let error = engine
            .preview_track_edit(track, vec![range(20_000, 10_000)], None, 5_000, 3)
            .expect_err("invalid first draft must be rejected");

        assert!(!error.is_empty());
        assert!(!engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 0);
        assert_eq!(engine.state.queue[0].uri, original_uri);
    }

    #[test]
    fn invalid_restore_does_not_leave_preview_or_stop_the_current_queue() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 1;
        let original_uri = engine.state.queue[0].uri.clone();

        let error = engine
            .restore_queue(
                vec![TrackRef::default()],
                4,
                12_000,
                String::new(),
                1,
                true,
                false,
                false,
            )
            .expect_err("out-of-range restore index must be rejected");

        assert!(error.contains("out of range"));
        assert!(engine.preview_mode);
        assert!(engine.state.playing);
        assert_eq!(engine.state.queue[0].uri, original_uri);
    }

    #[test]
    fn failed_preview_resume_leaves_the_restored_queue_real_paused_and_in_error() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 1;
        let track = engine.state.queue[0].clone();

        // The capture engine has no player. Restore still installs the real
        // queue and reports the failed resume in state instead of reviving
        // the editor preview.
        assert_eq!(
            engine.restore_queue(vec![track], 0, 42_000, String::new(), 1, true, true, false),
            Ok(true)
        );
        assert!(!engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 0);
        assert!(!engine.state.playing);
        assert!(engine.state.error.is_some());
        assert_eq!(engine.state.current_index, Some(0));
        assert_eq!(engine.state.position_ms, 42_000);
    }

    #[test]
    fn preview_draft_is_validated_frozen_on_the_row_and_visible_in_state() {
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..TrackRef::default()
        };
        let cuts = vec![
            TimeRange {
                start_ms: 1_000,
                end_ms: 2_500,
            },
            TimeRange {
                start_ms: 10_000,
                end_ms: 12_000,
            },
        ];
        let loop_range = Some(loop_range(20_000, 25_000, 2));
        let preview =
            with_preview_edit(track.clone(), cuts.clone(), loop_range).expect("valid draft");
        assert_eq!(
            preview.effective_edit,
            Some(renderer_engine::protocol::TrackEdit {
                cuts: cuts.clone(),
                loop_range,
            })
        );

        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(track.duration_ms);
        engine.state.queue = vec![preview];
        engine.emit_state().expect("preview state emits");
        let line: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("valid state JSON")
        };
        assert_eq!(
            line["queue"][0]["effective_edit"],
            serde_json::json!({"cuts": cuts, "loop_range": loop_range})
        );

        assert!(
            with_preview_edit(
                track.clone(),
                vec![
                    TimeRange {
                        start_ms: 10_000,
                        end_ms: 12_000,
                    },
                    TimeRange {
                        start_ms: 11_000,
                        end_ms: 13_000,
                    },
                ],
                None,
            )
            .is_err(),
            "preview must use the same sorted/non-overlapping validation as persistence"
        );
    }

    #[test]
    fn normal_playback_is_finalized_before_preview_and_preview_events_are_ignored() {
        let (mut engine, root) = history_playing_engine();
        let uri = track_uri();
        engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: 5_000,
        });
        listened(&mut engine);
        assert_eq!(history_rows(&engine).len(), 1);

        let track = engine.state.queue[0].clone();
        assert!(
            engine
                .preview_track_edit(track, vec![range(1_000, 2_000)], None, 5_000, 1)
                .is_err(),
            "the capture engine has no player"
        );
        assert!(engine.preview_mode);
        let rows = history_rows(&engine);
        assert_eq!(rows.len(), 1, "the real row is finalized, not discarded");
        assert_eq!(rows[0].row.track_id, "0123456789ABCDEFGHIJKL");
        assert!(!rows[0].row.completed);

        engine.play_request_id = Some(8);
        // The preview install already reported this row as playing, so its
        // Playing event is an echo: it cannot append a draft row, and it is
        // not a change either.
        assert!(!engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: uri,
            position_ms: 5_000,
        }));
        assert_eq!(
            history_rows(&engine).len(),
            1,
            "a preview Playing event cannot append a draft row"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn repeated_preview_updates_keep_history_empty_through_pause_and_eof() {
        let (mut engine, root) = history_playing_engine();
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        engine.player = Some(player);
        engine.session = Some(session);
        let track = engine.state.queue[0].clone();
        assert!(
            engine
                .preview_track_edit(track.clone(), vec![range(1_000, 2_000)], None, 5_000, 1)
                .is_ok()
        );
        assert!(engine.preview_mode);

        engine.play_request_id = Some(8);
        assert!(!engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: track_uri(),
            position_ms: 5_000,
        }));
        assert_eq!(engine.pause(), Ok(true));
        engine.play_request_id = Some(8);
        engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: track_uri(),
            position_ms: 6_000,
        });
        assert!(!engine.state.playing, "a delayed preview echo cannot undo Pause");

        assert!(
            engine
                .preview_track_edit(track, vec![range(3_000, 4_000)], None, 7_000, 2)
                .is_ok()
        );
        assert!(engine.preview_mode);
        engine.play_request_id = Some(9);
        assert!(!engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 9,
            track_id: track_uri(),
            position_ms: 7_000,
        }));
        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 9,
            track_id: track_uri(),
        }));
        assert!(history_rows(&engine).is_empty());
        engine.shutdown();
        assert!(history_rows(&engine).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn normal_queue_replacement_leaves_preview_without_persisting_it() {
        let (mut engine, root) = history_playing_engine();
        let preview_track = engine.state.queue[0].clone();
        assert!(
            engine
                .preview_track_edit(preview_track, vec![range(1_000, 2_000)], None, 5_000, 1)
                .is_err()
        );
        assert!(engine.preview_mode);

        let normal_track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..TrackRef::default()
        };
        assert!(
            engine
                .play_queue(vec![normal_track], 0, 0, "library".to_owned())
                .is_err(),
            "the capture engine has no player"
        );
        assert!(!engine.preview_mode);
        engine.play_request_id = Some(10);
        engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 10,
            track_id: track_uri(),
            position_ms: 0,
        });
        listened(&mut engine);
        let rows = history_rows(&engine);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row.track_id, "0123456789ABCDEFGHIJKL");
        assert_eq!(rows[0].row.context, "library");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_output_stops_listening_history_wall_clock() {
        let (mut engine, root) = history_playing_engine();
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        engine.player = Some(player);
        engine.session = Some(session);
        engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7, track_id: track_uri(), position_ms: 5_000,
        });
        listened(&mut engine);
        note_audio(&mut engine);
        assert!(engine.on_audio_signal(AudioSignal::OutputStalled {
            revision: engine.audio_revision,
        }));
        let paused_ms = history_rows(&engine)[0].row.ms_played;
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            history_rows(&engine)[0].row.ms_played,
            paused_ms,
            "audio-device or player pauses must not accrue silent wall time"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unexpected_player_close_finalizes_the_active_history_row() {
        let (mut engine, root) = history_playing_engine();
        engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: track_uri(),
            position_ms: 5_000,
        });
        listened(&mut engine);
        // The installed player's event stream is the one that closes.
        engine.player_generation = Some(0);
        assert!(engine.on_player_signal(PlayerSignal::Closed { generation: 0 }));

        // The play is in the journal and the in-progress sidecar is gone.
        let journal = std::fs::read_to_string(root.join("listening_history.jsonl")).unwrap();
        assert_eq!(journal.lines().filter(|line| !line.is_empty()).count(), 1);
        assert!(!root.join("listening_history_active.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_progression_strategy_skips_permanent_unavailability() {
        let playable = TrackRef {
            uri: "spotify:track:1abcdefghijklmnopqrstu".to_owned(),
            ..TrackRef::default()
        };
        let queue = vec![
            playable.clone(),
            unavailable_track("2abcdefghijklmnopqrstu"),
            unavailable_track("3abcdefghijklmnopqrstu"),
            playable,
        ];
        assert_eq!(first_available_from(&queue, 1), Some(3));
        assert_eq!(first_available_wrapping(&queue, 2), Some(3));
        assert_eq!(
            sequential_available_index(&queue, 0, RepeatMode::Off),
            Some(3)
        );
        assert_eq!(
            sequential_available_index(&queue, 3, RepeatMode::Context),
            Some(0)
        );

        let blocked = vec![
            unavailable_track("4abcdefghijklmnopqrstu"),
            unavailable_track("5abcdefghijklmnopqrstu"),
        ];
        assert_eq!(first_available_from(&blocked, 0), None);
        assert_eq!(
            sequential_available_index(&blocked, 0, RepeatMode::Context),
            None
        );
    }

    #[test]
    fn automatic_shuffle_start_draws_from_all_eligible_rows() {
        let queue = vec![
            contextual_track(QUEUE_IDS[0], "playlist:playlist"),
            contextual_track(QUEUE_IDS[1], "playlist:playlist"),
            unavailable_track(QUEUE_IDS[2]),
            contextual_track(QUEUE_IDS[3], "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[1])]);
        drop(store);

        for (seed, expected) in [(1, 3), (2, 0)] {
            let (mut engine, _) = test_engine_in(directory.path().to_path_buf());
            engine.state.shuffle = true;
            engine.random_state = seed;
            assert!(
                engine
                    .play_queue_with_automatic_start(
                        queue.clone(), 0, 0, "playlist:playlist".to_owned(), true,
                    )
                    .is_err(),
                "the capture engine has no player"
            );
            assert_eq!(engine.state.current_index, Some(expected));
            assert_eq!(engine.upcoming_indices(), vec![3 - expected]);
            assert_shuffle_pool_invariants(&engine);

            let (mut paused, _) = test_engine_in(directory.path().to_path_buf());
            paused.state.shuffle = true;
            paused.random_state = seed;
            paused
                .restore_queue(
                    queue.clone(), 0, 0, "playlist:playlist".to_owned(),
                    0, false, false, true,
                )
                .expect("remote queue installs without loading local audio");
            assert_eq!(paused.state.current_index, Some(expected));
            assert_eq!(paused.upcoming_indices(), vec![3 - expected]);
            assert!(!paused.state.playing);
            assert_shuffle_pool_invariants(&paused);

            paused
                .restore_queue(
                    queue.clone(), 1, 12_000, "playlist:playlist".to_owned(),
                    0, false, false, false,
                )
                .expect("snapshot restore preserves its explicit row");
            assert_eq!(paused.state.current_index, Some(1));
            assert_eq!(paused.state.position_ms, 12_000);
        }
    }

    #[test]
    fn automatic_shuffle_start_handles_zero_or_one_eligible_row() {
        let (directory, store) =
            store_with_exclusions(&[("playlist", QUEUE_IDS[0])]);
        drop(store);
        for has_eligible in [false, true] {
            let mut queue = vec![
                contextual_track(QUEUE_IDS[0], "playlist:playlist"),
                unavailable_track(QUEUE_IDS[1]),
            ];
            if has_eligible {
                queue.push(contextual_track(QUEUE_IDS[2], "playlist:playlist"));
            }
            let (mut engine, _) = test_engine_in(directory.path().to_path_buf());
            engine.state.shuffle = true;
            engine
                .restore_queue(
                    queue, 0, 0, "playlist:playlist".to_owned(),
                    0, false, false, true,
                )
                .expect("automatic paused start handles an exhausted queue");
            assert_eq!(engine.state.current_index, has_eligible.then_some(2));
            assert!(engine.upcoming_indices().is_empty());
            assert!(!engine.state.playing);
        }
    }

    #[test]
    fn automatic_start_skips_excluded_but_direct_play_and_index_accept_it() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        drop(store);

        let (mut automatic_engine, _) = test_engine_in(directory.path().to_path_buf());
        assert!(
            automatic_engine
                .play_queue_with_automatic_start(
                    queue.clone(),
                    0,
                    12_000,
                    "playlist:playlist".to_owned(),
                    true,
                )
                .is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            automatic_engine.state.current_index,
            Some(1),
            "automatic starts skip the excluded requested row"
        );

        let (mut direct_engine, _) = test_engine_in(directory.path().to_path_buf());
        direct_engine.state.shuffle = true;
        assert!(
            direct_engine
                .play_queue(queue.clone(), 0, 12_000, "playlist:playlist".to_owned())
                .is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            direct_engine.state.current_index,
            Some(0),
            "direct queue playback accepts the excluded requested row"
        );

        let (mut index_engine, _) = test_engine_in(directory.path().to_path_buf());
        index_engine.state.queue = queue;
        index_engine.state.current_index = Some(1);
        index_engine.state.duration_ms = 180_000;
        assert!(
            index_engine.play_queue_index(0).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            index_engine.state.current_index,
            Some(0),
            "direct index playback accepts the excluded row"
        );
    }

    #[test]
    fn ordered_next_and_eof_skip_excluded_rows() {
        let excluded_id = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("0abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 0);
        assert_eq!(
            sequential_automatic_index(
                &engine.state.queue,
                0,
                RepeatMode::Off,
                &engine.track_edits,
            ),
            Some(2)
        );

        assert!(
            engine.advance(false).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            engine.state.current_index,
            Some(2),
            "Next skips excluded rows"
        );

        engine.state.current_index = Some(0);
        engine.state.duration_ms = 180_000;
        engine.state.position_ms = 0;
        engine.state.playing = true;
        engine.history.clear();
        assert!(
            engine.advance(true).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            engine.state.current_index,
            Some(2),
            "natural EOF skips excluded rows"
        );
        drop(directory);
    }

    #[test]
    fn repeat_context_wraps_only_to_eligible_rows() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 2);
        engine.state.repeat = RepeatMode::Context;

        assert_eq!(
            sequential_automatic_index(
                &engine.state.queue,
                2,
                RepeatMode::Context,
                &engine.track_edits,
            ),
            Some(1)
        );
        assert_eq!(engine.take_next_index_with_skip(false, false), Some(1));
        drop(directory);
    }

    #[test]
    fn shuffle_pool_excludes_playlist_preferences() {
        let excluded_id = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("0abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.shuffle = true;
        engine.rebuild_shuffle_pool();

        assert_eq!(
            engine.shuffle_pool,
            vec![2],
            "shuffle only contains non-current eligible rows"
        );
        drop(directory);
    }

    fn playlist_queue(ids: &[&str]) -> Vec<TrackRef> {
        ids.iter()
            .map(|id| contextual_track(id, "playlist:playlist"))
            .collect()
    }

    const QUEUE_IDS: [&str; 4] = [
        "0abcdefghijklmnopqrstu",
        "1abcdefghijklmnopqrstu",
        "2abcdefghijklmnopqrstu",
        "3abcdefghijklmnopqrstu",
    ];

    #[test]
    fn upcoming_is_the_shuffle_pool_in_pop_order() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[2])]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.shuffle = true;
        engine.rebuild_shuffle_pool();

        let popped: Vec<usize> = engine.shuffle_pool.iter().rev().copied().collect();
        assert_eq!(
            engine.upcoming_indices(),
            popped,
            "upcoming is exactly the order the pool will be popped in"
        );

        // A pool built before an exclusion still carries the row, and the
        // transport skips it on the way past; upcoming must skip it too.
        engine.shuffle_pool = vec![3, 2, 1];
        assert_eq!(engine.upcoming_indices(), vec![1, 3]);
        drop(directory);
    }

    #[test]
    fn upcoming_walks_sequentially_past_an_excluded_row() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[2])]);
        let engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);

        assert_eq!(engine.upcoming_indices(), vec![1, 3]);
        drop(directory);
    }

    #[test]
    fn upcoming_wraps_through_the_current_row_under_repeat_context() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[1])]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 2);
        engine.state.repeat = RepeatMode::Context;

        assert_eq!(
            engine.upcoming_indices(),
            vec![3, 0, 2],
            "the wrap is inclusive of the current row, like sequential_automatic_index"
        );
        drop(directory);
    }

    #[test]
    fn upcoming_ignores_repeat_one() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.repeat = RepeatMode::Track;

        assert_eq!(
            engine.upcoming_indices(),
            vec![1, 2, 3],
            "repeat-one loops the current row, it does not replan the queue"
        );
        drop(directory);
    }

    #[test]
    fn upcoming_lists_every_eligible_row_without_a_current() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[1])]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.current_index = None;

        assert_eq!(engine.upcoming_indices(), vec![0, 2, 3]);
        drop(directory);
    }

    /// A shuffling engine over `QUEUE_IDS` with a hand-written drawn order, so
    /// a repair's effect on that order is exactly readable. The seed is fixed
    /// because splices consume the same PRNG the bag was drawn with.
    fn shuffling_engine(store: TrackEditStore, current: usize, pool: &[usize]) -> Engine {
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), current);
        engine.state.shuffle = true;
        engine.shuffle_pool = pool.to_vec();
        engine.random_state = 0x2545_f491_4f6c_dd1d;
        engine
    }

    /// Every invariant the bag carries, checked as a set rather than one at a
    /// time, because a repair that breaks one usually breaks another.
    fn assert_shuffle_pool_invariants(engine: &Engine) {
        if !engine.state.shuffle {
            assert!(
                engine.shuffle_pool.is_empty(),
                "the bag is empty whenever shuffle is off"
            );
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for index in &engine.shuffle_pool {
            assert!(
                *index < engine.state.queue.len(),
                "bag entry {index} does not index the queue"
            );
            assert!(
                Some(*index) != engine.state.current_index,
                "the bag holds the current row {index}"
            );
            assert!(seen.insert(*index), "the bag holds {index} twice");
        }
    }

    #[test]
    fn removing_a_row_shifts_the_bag_instead_of_redrawing_it() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert_eq!(engine.remove_queue(2), Ok(true));
        assert_eq!(
            engine.shuffle_pool,
            vec![2, 1],
            "the removed row leaves and the rows above it slide down one"
        );
        assert_eq!(engine.upcoming_indices(), vec![1, 2]);
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn removing_the_current_row_drops_its_replacement_from_the_bag() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 1, &[3, 2, 0]);

        assert!(
            engine.remove_queue(1).is_err(),
            "the capture engine has no player to reload the replacement on"
        );
        assert_eq!(engine.state.current_index, Some(1));
        assert_eq!(
            engine.shuffle_pool,
            vec![2, 0],
            "the promoted replacement leaves the bag, the rest keep their order"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn moving_a_row_remaps_the_bag_the_same_way_as_the_current_row() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 1, &[3, 2, 0]);

        assert_eq!(engine.move_queue(0, 3), Ok(true));
        assert_eq!(engine.state.current_index, Some(0));
        assert_eq!(
            engine.shuffle_pool,
            vec![
                remap_current_index_after_move(3, 0, 3),
                remap_current_index_after_move(2, 0, 3),
                remap_current_index_after_move(0, 0, 3),
            ],
            "one permutation moves the queue, the current row and the bag"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn moving_a_row_onto_the_current_row_keeps_it_out_of_the_bag() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 2, &[3, 1, 0]);

        assert_eq!(engine.move_queue(0, 2), Ok(true));
        assert_eq!(engine.state.current_index, Some(1));
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn moving_rows_preserves_repeat_visits_for_shuffle_previous() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 3, &[1]);
        engine.history = vec![1, 0, 1];

        assert_eq!(engine.move_queue(0, 2), Ok(true));
        assert_eq!(engine.history, vec![0, 2, 0]);
        assert_eq!(engine.state.current_index, Some(3));
        assert_shuffle_pool_invariants(&engine);

        engine.state.position_ms = 5_000;
        assert_eq!(engine.previous_index(), None, "a late press restarts");
        assert_eq!(engine.history, vec![0, 2, 0]);
        engine.state.position_ms = 0;
        for (target, track_id) in [(0, QUEUE_IDS[1]), (2, QUEUE_IDS[0]), (0, QUEUE_IDS[1])] {
            assert!(
                engine.previous().is_err(),
                "the capture engine has no player"
            );
            assert_eq!(engine.state.current_index, Some(target));
            assert_eq!(engine.state.queue[target].id, track_id);
            assert_shuffle_pool_invariants(&engine);
        }
        assert!(engine.history.is_empty());
        assert_eq!(
            engine.previous_index(),
            None,
            "shuffle has no ordered fallback"
        );
        drop(directory);
    }

    #[test]
    fn removing_a_row_drops_its_visits_and_shifts_later_visits() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 3, &[2, 1, 0]);
        engine.history = vec![0, 1, 2, 1, 3, 2];

        assert_eq!(engine.remove_queue(1), Ok(true));
        assert_eq!(engine.history, vec![0, 1, 2, 1]);
        assert_eq!(engine.state.current_index, Some(2));
        assert_shuffle_pool_invariants(&engine);
        assert!(
            engine.previous().is_err(),
            "the capture engine has no player"
        );
        assert_eq!(engine.state.current_index, Some(1));
        assert_eq!(engine.state.queue[1].id, QUEUE_IDS[2]);
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn appending_one_or_a_lazy_batch_keeps_previous_visits() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 2, &[3]);
        engine.history = vec![0, 1];

        assert_eq!(
            engine.add_queue(
                contextual_track("4abcdefghijklmnopqrstu", "playlist:playlist"),
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert_eq!(engine.history, vec![0, 1]);
        assert_eq!(
            engine.add_queue_batch(
                vec![
                    contextual_track("5abcdefghijklmnopqrstu", "playlist:playlist"),
                    contextual_track("6abcdefghijklmnopqrstu", "playlist:playlist"),
                ],
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert_eq!(engine.history, vec![0, 1]);
        assert_eq!(engine.state.current_index, Some(2));
        assert_shuffle_pool_invariants(&engine);
        for (target, track_id) in [(1, QUEUE_IDS[1]), (0, QUEUE_IDS[0])] {
            assert!(
                engine.previous().is_err(),
                "the capture engine has no player"
            );
            assert_eq!(engine.state.current_index, Some(target));
            assert_eq!(engine.state.queue[target].id, track_id);
            assert_shuffle_pool_invariants(&engine);
        }
        drop(directory);
    }

    #[test]
    fn adding_a_row_splices_it_without_reordering_the_survivors() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[]);
        // Seven survivors, not three: a redraw would have to reproduce one
        // ordering out of five thousand to sneak past this assertion, which a
        // four-row queue leaves well within coincidence.
        for ordinal in 4..8u32 {
            engine.state.queue.push(contextual_track(
                &format!("0abcdefghijklmnopqrst{ordinal}"),
                "playlist:playlist",
            ));
        }
        engine.shuffle_pool = vec![2, 7, 4, 1, 6, 3, 5];

        assert_eq!(
            engine.add_queue(
                contextual_track("0abcdefghijklmnopqrst9", "playlist:playlist"),
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert!(engine.shuffle_pool.contains(&8));
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index != 8)
                .collect::<Vec<usize>>(),
            vec![2, 7, 4, 1, 6, 3, 5],
            "queueing a track must not redraw the plan already on screen"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn a_batch_add_splices_every_new_row_and_keeps_the_survivors_ordered() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert_eq!(
            engine.add_queue_batch(
                vec![
                    contextual_track("4abcdefghijklmnopqrstu", "playlist:playlist"),
                    contextual_track("5abcdefghijklmnopqrstu", "playlist:playlist"),
                ],
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index < 4)
                .collect::<Vec<usize>>(),
            vec![3, 2, 1]
        );
        assert!(engine.shuffle_pool.contains(&4) && engine.shuffle_pool.contains(&5));
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn queued_rows_land_somewhere_other_than_one_end_of_the_bag() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        let mut interior = false;
        for ordinal in 4..24u32 {
            let id = format!("{ordinal:02}bcdefghijklmnopqrstu");
            let added = engine.state.queue.len();
            assert_eq!(
                engine.add_queue(
                    contextual_track(&id, "playlist:playlist"),
                    "playlist:playlist".to_owned(),
                ),
                Ok(true)
            );
            let at = engine
                .shuffle_pool
                .iter()
                .position(|index| *index == added)
                .expect("the new row joined the bag");
            interior |= at > 0 && at + 1 < engine.shuffle_pool.len();
        }
        assert!(
            interior,
            "appending to an end would make queued tracks always play next or always last"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn play_queue_index_drops_only_the_row_it_jumps_to() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert!(
            engine.play_queue_index(2).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(engine.state.current_index, Some(2));
        assert_eq!(
            engine.shuffle_pool,
            vec![3, 1],
            "the row jumped to leaves; the row left behind is `previous`'s business"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn exclusion_toggles_repair_the_bag_in_both_directions() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", QUEUE_IDS[2], true),
            Ok(())
        );
        assert_eq!(
            engine.shuffle_pool,
            vec![3, 1],
            "an excluded row leaves without disturbing the others"
        );
        assert_shuffle_pool_invariants(&engine);

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", QUEUE_IDS[2], false),
            Ok(())
        );
        assert!(engine.shuffle_pool.contains(&2));
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index != 2)
                .collect::<Vec<usize>>(),
            vec![3, 1],
            "a re-included row is spliced in, it does not trigger a redraw"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn an_exclusion_toggle_reaches_every_queue_row_holding_that_track() {
        let repeated = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(repeated, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(repeated, "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.shuffle = true;
        engine.shuffle_pool = vec![3, 2, 1];
        engine.random_state = 0x2545_f491_4f6c_dd1d;

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", repeated, true),
            Ok(())
        );
        assert_eq!(
            engine.shuffle_pool,
            vec![2],
            "both rows carrying the excluded id leave the bag"
        );

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", repeated, false),
            Ok(())
        );
        assert_eq!(engine.shuffle_pool.len(), 3);
        assert!(engine.shuffle_pool.contains(&1) && engine.shuffle_pool.contains(&3));
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn preview_mode_eligibility_governs_a_repair_like_it_governs_a_redraw() {
        let excluded_id = QUEUE_IDS[2];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = shuffling_engine(store, 0, &[3, 1]);
        engine.preview_mode = true;

        // Preview ignores playlist exclusions, so the excluded row is eligible
        // here and a repair must splice it in rather than leave it out.
        engine.repair_shuffle_pool_for_eligibility();
        assert!(engine.shuffle_pool.contains(&2));
        assert_shuffle_pool_invariants(&engine);

        engine.preview_mode = false;
        engine.repair_shuffle_pool_for_eligibility();
        assert_eq!(
            engine.shuffle_pool,
            vec![3, 1],
            "leaving preview puts the exclusion back in force"
        );
        drop(directory);
    }

    #[test]
    fn a_mutation_leaves_the_drawn_order_of_untouched_rows_alone() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.shuffle = true;
        engine.random_state = 0x9e37_79b9_7f4a_7c15;
        engine.rebuild_shuffle_pool();
        let drawn = engine.shuffle_pool.clone();

        assert_eq!(
            engine.add_queue(
                contextual_track("4abcdefghijklmnopqrstu", "playlist:playlist"),
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index != 4)
                .collect::<Vec<usize>>(),
            drawn,
            "this is the regression: a queue mutation used to redraw the whole plan"
        );
        drop(directory);
    }

    #[test]
    fn repairs_never_resurrect_the_bag_while_shuffle_is_off() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);

        for seed in 0..3 {
            engine.shuffle_pool = vec![3, 2, 1];
            match seed {
                0 => assert_eq!(engine.move_queue(1, 2), Ok(true)),
                1 => assert_eq!(engine.remove_queue(3), Ok(true)),
                _ => assert_eq!(
                    engine.add_queue(
                        contextual_track("4abcdefghijklmnopqrstu", "playlist:playlist"),
                        "playlist:playlist".to_owned(),
                    ),
                    Ok(true)
                ),
            }
            assert!(
                engine.shuffle_pool.is_empty(),
                "an ordered queue has no shuffle plan to repair"
            );
        }
        engine.shuffle_pool = vec![3, 2, 1];
        assert_eq!(
            engine.set_playlist_track_excluded("playlist", QUEUE_IDS[1], true),
            Ok(())
        );
        assert!(engine.shuffle_pool.is_empty());
        drop(directory);
    }

    #[test]
    fn the_repeat_context_refill_still_draws_a_fresh_lap() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[]);
        engine.state.repeat = RepeatMode::Context;

        let next = engine
            .take_next_index_with_skip(false, false)
            .expect("an emptied bag refills under repeat-context");
        assert_ne!(next, 0, "the refill never offers the current row");
        assert_eq!(
            engine.shuffle_pool.len(),
            2,
            "a new lap is a new draw of every other eligible row"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn previous_history_and_ordered_fallback_skip_excluded_rows() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 2);

        // History is newest-first in the vector's pop order here: it first
        // offers the excluded row, then the eligible earlier row.
        engine.history = vec![1, 0];
        assert_eq!(engine.previous_index(), Some(1));
        engine.history.clear();
        assert_eq!(
            engine.previous_index(),
            Some(1),
            "ordered fallback also skips excluded rows"
        );
        drop(directory);
    }

    #[test]
    fn preload_peek_skips_excluded_rows() {
        let excluded_id = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("0abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let engine = engine_with_queue(store, queue, 0);

        assert_eq!(
            engine.peek_next_index(),
            Some(2),
            "peek selects the same eligible target used by preload"
        );
        engine.preload_next();
        drop(directory);
    }

    #[test]
    fn repeat_one_continues_a_manually_started_excluded_current() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![contextual_track(excluded_id, "playlist:playlist")];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.repeat = RepeatMode::Track;

        assert_eq!(
            engine.take_next_index_with_skip(true, false),
            Some(0),
            "repeat-one preserves a manually started excluded current"
        );
        assert_eq!(engine.peek_next_index(), Some(0));
        drop(directory);
    }

    #[test]
    fn toggling_current_exclusion_does_not_stop_playback() {
        let track_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(track_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.position_ms = 7_500;

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", track_id, true),
            Ok(())
        );
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.state.playing, "toggling a preference must not pause");
        assert_eq!(engine.state.position_ms, 7_500);
        assert!(
            engine
                .track_edit_status(track_id, Some("playlist"))
                .excluded_from_automatic_playback
        );
        drop(directory);
    }

    #[test]
    fn all_excluded_automatic_queues_have_no_target() {
        let first = "0abcdefghijklmnopqrstu";
        let second = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(first, "playlist:playlist"),
            contextual_track(second, "playlist:playlist"),
        ];
        let (directory, store) =
            store_with_exclusions(&[("playlist", first), ("playlist", second)]);
        assert_eq!(first_automatic_from(&queue, 0, &store), None);
        assert_eq!(first_automatic_wrapping(&queue, 1, &store), None);
        assert_eq!(
            sequential_automatic_index(&queue, 0, RepeatMode::Context, &store),
            None
        );
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.repeat = RepeatMode::Context;
        assert_eq!(
            engine.take_next_index_with_skip(false, false),
            None,
            "automatic progression has no target when every row is excluded"
        );
        drop(directory);
    }

    #[test]
    fn playlist_exclusions_are_isolated_from_album_and_other_playlists() {
        let track_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(track_id, "playlist:one"),
            contextual_track(track_id, "album:one"),
            contextual_track(track_id, "playlist:two"),
        ];
        let (directory, store) = store_with_exclusions(&[("one", track_id)]);

        assert!(!automatic_track_eligible(&store, &queue[0]));
        assert!(automatic_track_eligible(&store, &queue[1]));
        assert!(automatic_track_eligible(&store, &queue[2]));
        assert_eq!(first_automatic_from(&queue, 0, &store), Some(1));
        drop(directory);
    }

    #[test]
    fn restore_preserves_an_excluded_requested_row() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        drop(store);
        let (mut engine, _) = test_engine_in(directory.path().to_path_buf());

        assert_eq!(
            engine.restore_queue(queue, 0, 42_000, String::new(), 0, false, false, false),
            Ok(true)
        );
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "restore is not automatic playback and keeps the requested row"
        );
        assert_eq!(engine.state.position_ms, 42_000);
        assert!(engine.current_needs_load);
        drop(directory);
    }

    #[test]
    fn same_value_setters_are_no_ops() {
        let (mut engine, _) = test_engine();
        engine.history.push(3);
        engine.shuffle_pool.push(4);

        assert_eq!(engine.set_volume(50), Ok(false));
        assert_eq!(engine.set_shuffle(false), Ok(false));
        assert_eq!(engine.set_repeat(RepeatMode::Off), Ok(false));
        assert_eq!(engine.history, vec![3]);
        assert_eq!(engine.shuffle_pool, vec![4]);
    }

    /// Reads one protocol line and parses it, leaving the buffer empty for the
    /// next emission.
    fn take_line(buffer: &Arc<std::sync::Mutex<Vec<u8>>>) -> serde_json::Value {
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        assert!(!line.is_empty(), "nothing was emitted");
        serde_json::from_slice(&line).expect("protocol line is json")
    }

    /// Ages the pending volume past its quiet period, the way the persist
    /// deadline that follows a drag finds it.
    fn settle_volume(engine: &mut Engine) {
        let pending = engine
            .pending_volume
            .as_mut()
            .expect("a volume change is pending");
        pending.changed_at = Instant::now() - VOLUME_PERSIST_QUIET - Duration::from_millis(1);
    }

    /// A temp cache directory whose volume file a test can read back, so
    /// "persisted" means the bytes librespot would read on the next start.
    struct VolumeCacheFixture {
        directory: PathBuf,
        cache: librespot_core::cache::Cache,
    }

    impl VolumeCacheFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "renderer-engine-volume-cache-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            let _ = std::fs::remove_dir_all(&directory);
            let cache = librespot_core::cache::Cache::new(
                None::<PathBuf>,
                Some(directory.clone()),
                None::<PathBuf>,
                None,
            )
            .expect("cache with a volume path");
            Self { directory, cache }
        }

        /// What a player built right now would start from.
        fn stored(&self) -> Option<u16> {
            self.cache.volume()
        }
    }

    impl Drop for VolumeCacheFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// A cache with no paths at all: nothing is ever read from or written to
    /// disk, which is what most tests want.
    fn no_path_cache() -> librespot_core::cache::Cache {
        librespot_core::cache::Cache::new(
            None::<PathBuf>,
            None::<PathBuf>,
            None::<PathBuf>,
            None,
        )
        .expect("cache with no paths")
    }

    /// Ready with a real player, plus the protocol buffer the caller reads:
    /// the shape a running app is in when the user drags the volume. `cache`
    /// is separate so a persistence test can hand in one it can read back.
    fn dragging_engine(
        cache: librespot_core::cache::Cache,
    ) -> (Engine, SinkProbe, Arc<std::sync::Mutex<Vec<u8>>>) {
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        let (mut engine, buffer) = test_engine_with_cache(
            PathBuf::new(),
            cache,
            device_present(),
            device_always_present(),
        );
        engine.state = playback_state(240_000);
        engine.play_request_id = Some(7);
        engine.player = Some(player);
        engine.session = Some(session);
        (engine, probe, buffer)
    }

    /// One volume step is one number, and the protocol must say so. The drag
    /// path paces `set_volume` at 50 ms, so a step that serialized the queue
    /// and its computed upcoming set would do that twenty times a second to
    /// say what a single scalar says — and the `false` is what keeps the
    /// command loop from following it with a full state.
    #[tokio::test(flavor = "current_thread")]
    async fn a_volume_step_emits_the_scalar_lane_and_no_full_state() {
        let (mut engine, _probe, buffer) = dragging_engine(no_path_cache());
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();

        let result = engine
            .process_command(Command::SetVolume { percent: 30 }, &auth_sender)
            .await;
        assert_eq!(
            result,
            Ok(false),
            "a volume step must not ask the loop for a full state"
        );
        assert_eq!(engine.state.volume, 30, "the loudness still moves");

        let line = take_line(&buffer);
        assert_eq!(line["type"], "volume");
        assert_eq!(line["volume"], 30);
        assert_eq!(
            line.as_object().expect("volume line is an object").len(),
            2,
            "type and volume, nothing else: {line:?}"
        );

        // A repeated value is not news, and costs nothing at all.
        assert_eq!(
            engine
                .process_command(Command::SetVolume { percent: 30 }, &auth_sender)
                .await,
            Ok(false)
        );
        assert!(
            buffer.lock().expect("buffer lock").is_empty(),
            "the same value emits nothing"
        );
    }

    /// The cache write is the drag's real cost — `Cache::save_volume` creates
    /// and rewrites a file per call — so it waits for the value to settle and
    /// is paid once per gesture rather than twenty times a second.
    #[tokio::test(flavor = "current_thread")]
    async fn a_volume_burst_reaches_the_cache_once_and_again_after_it_settles() {
        let fixture = VolumeCacheFixture::new();
        let (mut engine, _probe, _buffer) = dragging_engine(fixture.cache.clone());
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();

        for percent in [30u8, 31, 32, 33] {
            engine
                .process_command(Command::SetVolume { percent }, &auth_sender)
                .await
                .expect("volume applies");
        }
        // Still moving: a work tick that lands mid-drag writes nothing.
        engine.tick_volume_persist();
        assert_eq!(
            fixture.stored(),
            None,
            "no step of a drag may cost a file write"
        );

        // The drag has stopped, and the next tick writes the value it stopped
        // at — once for the whole gesture.
        settle_volume(&mut engine);
        engine.tick_volume_persist();
        assert_eq!(fixture.stored(), Some(percent_to_volume(33)));

        // Not a one-shot: the next settled value is written as well, and only
        // after it settles.
        engine
            .process_command(Command::SetVolume { percent: 40 }, &auth_sender)
            .await
            .expect("volume applies");
        engine.tick_volume_persist();
        assert_eq!(
            fixture.stored(),
            Some(percent_to_volume(33)),
            "a value that is still moving is not written"
        );
        settle_volume(&mut engine);
        engine.tick_volume_persist();
        assert_eq!(fixture.stored(), Some(percent_to_volume(40)));
    }

    /// A player construction reads the transport volume from the cache, and a
    /// reconnect on a queue-less engine adopts what it finds there into
    /// `state.volume`. So a change the deferred write has not stored yet must
    /// reach the cache before the construction that follows it, or a network
    /// blip inside the write-behind window would quietly put the volume back
    /// the way it was.
    #[tokio::test(flavor = "current_thread")]
    async fn a_player_construction_is_given_the_volume_the_user_last_set() {
        let fixture = VolumeCacheFixture::new();
        let (mut engine, _probe, _buffer) = dragging_engine(fixture.cache.clone());
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();

        engine
            .process_command(Command::SetVolume { percent: 62 }, &auth_sender)
            .await
            .expect("volume applies");
        assert_eq!(
            fixture.stored(),
            None,
            "the deferred write has not stored the change yet"
        );

        // A real construction entry point: the flush must land before the
        // spawned task reads the cache.
        assert!(engine.set_normalisation(true, &auth_sender));

        assert_eq!(
            fixture.stored(),
            Some(percent_to_volume(62)),
            "the player that is about to be built starts from the value the user set"
        );
    }

    /// A quit must not lose a volume the deferred write has not stored yet: the
    /// cache file is the engine's only copy, and a clean exit is exactly when
    /// there is still time to write it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_pending_volume_is_written_by_shutdown() {
        let fixture = VolumeCacheFixture::new();
        let (mut engine, _probe, _buffer) = dragging_engine(fixture.cache.clone());
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();

        engine
            .process_command(Command::SetVolume { percent: 25 }, &auth_sender)
            .await
            .expect("volume applies");
        assert_eq!(fixture.stored(), None, "the write is still pending");

        engine.shutdown();

        assert_eq!(
            fixture.stored(),
            Some(percent_to_volume(25)),
            "the last volume survives a clean exit"
        );
    }

    /// Only the transition into the no-device state is news. A machine that
    /// has been without audio since boot re-enters this state on every probe,
    /// and a line per probe would eat the rotating engine log the outages
    /// worth reading are in.
    #[test]
    fn only_a_new_no_device_condition_is_worth_a_log_line() {
        assert!(
            audio_unavailable_is_news(None, "no audio output device"),
            "the first entry into the state is the news"
        );
        assert!(
            audio_unavailable_is_news(
                Some("the audio output stopped responding"),
                "no audio output device"
            ),
            "a different message is a different condition"
        );
        assert!(
            !audio_unavailable_is_news(
                Some("no audio output device"),
                "no audio output device"
            ),
            "a steady-state retry of the same condition stays silent"
        );
    }

    #[test]
    fn changed_shuffle_and_repeat_values_still_update_state() {
        let (mut engine, _) = test_engine();
        engine.history.push(3);

        assert_eq!(engine.set_shuffle(true), Ok(true));
        assert!(engine.state.shuffle);
        assert!(engine.history.is_empty());
        assert_eq!(engine.set_repeat(RepeatMode::Context), Ok(true));
        assert_eq!(engine.state.repeat, RepeatMode::Context);
    }

    /// The three load flags that used to suppress the librespot pause, each on
    /// its own. They say "a load is pending, has failed, or ran off the end" —
    /// none of them says librespot stopped, and when that inference was wrong
    /// the audio kept playing while the engine, and therefore the UI, was
    /// certain it had stopped. Nothing afterwards reconciled the two, so the
    /// only way out was a restart. A pause now performs the stop before it
    /// reports one, and fails outright when it cannot reach the player.
    #[tokio::test(flavor = "current_thread")]
    async fn a_pause_is_reported_only_once_librespot_has_been_told_to_stop() {
        let arms: [(&str, fn(&mut Engine)); 3] = [
            ("a pending load", |engine| engine.current_needs_load = true),
            ("a failed load", |engine| engine.loading_failed = true),
            ("a decoder EOF", |engine| engine.loop_decoder_eof = true),
        ];

        for (arm, set_flag) in arms {
            let mut unreachable = playing_engine();
            unreachable.state.playing = true;
            set_flag(&mut unreachable);
            assert!(
                unreachable.pause().is_err(),
                "{arm}: a pause with no player to command must fail"
            );
            assert!(
                unreachable.state.playing,
                "{arm}: the engine must not report a pause it never performed"
            );

            let (mut engine, _probe) = engine_with_player();
            engine.state.playing = true;
            set_flag(&mut engine);
            assert_eq!(
                engine.pause(),
                Ok(true),
                "{arm}: the pause reaches the player instead of being skipped"
            );
            assert!(!engine.state.playing, "{arm}");
        }
    }

    /// A track change carries the transport intent across, the way `previous`
    /// always has. Loading with a fixed `true` started audible playback the
    /// engine still called paused, and left the disagreement to be settled by
    /// whichever player event happened to arrive next.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_change_carries_the_transport_intent_across() {
        // The engine's own advance carries the intent across.
        let (mut paused, _paused_probe) = engine_with_player();
        paused.state = two_track_state();
        paused.state.playing = false;
        assert_eq!(paused.advance(false), Ok(true));
        assert_eq!(paused.state.current_index, Some(1));
        assert!(
            !paused.state.playing,
            "an automatic advance must not start a paused queue playing"
        );

        // A press of Next is a request to hear the next track, so it resumes.
        let (mut pressed, _pressed_probe) = engine_with_player();
        pressed.state = two_track_state();
        pressed.state.playing = false;
        assert_eq!(
            pressed.advance_with_current_skip(false, false, true),
            Ok(true)
        );
        assert_eq!(pressed.state.current_index, Some(1));
        assert!(
            pressed.state.playing,
            "pressing Next on a paused queue must start playing"
        );

        let (mut playing, _playing_probe) = engine_with_player();
        playing.state = two_track_state();
        playing.state.playing = true;
        assert_eq!(playing.advance(false), Ok(true));
        assert_eq!(playing.state.current_index, Some(1));
        assert!(playing.state.playing);
    }

    /// A runtime failure only moves a queue that is actually playing: the
    /// progression exists to keep audio flowing, and a paused queue has no
    /// flow.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_track_moves_on_only_while_playing() {
        let (mut playing, _playing_probe) = engine_with_player();
        playing.state = two_track_state();
        playing.state.playing = true;
        assert!(playing.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(
            playing.state.current_index,
            Some(0),
            "the first failure buys the row a retry, not a skip"
        );
        assert!(playing.retry_current_at.is_some());
        assert!(
            playing.state.playing,
            "a failure mid-playback keeps the transport intent"
        );

        // The bug the owner hit: librespot finishes a paused load and fails it
        // just the same, and the engine used to skip on that failure, so pause
        // could not stop the queue from walking itself.
        let (mut paused, _paused_probe) = engine_with_player();
        paused.state = two_track_state();
        paused.state.playing = false;
        assert!(paused.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(
            paused.state.current_index,
            Some(0),
            "a failure while paused holds the row instead of advancing"
        );
        assert!(!paused.state.playing);
        assert!(paused.loading_failed, "Play must re-arm a fresh loader");
        assert!(
            paused.retry_current_at.is_none(),
            "a paused failure must not arm a retry either"
        );
    }

    /// A row gets one more attempt before it is given up on: the refusal
    /// behind these failures is transient, and skipping on the first one
    /// discards a track that was never broken.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_row_is_retried_once_and_only_then_skipped() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;

        assert!(engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(0));
        let due = engine.retry_current_at.expect("the row is owed a retry");
        assert!(
            due.duration_since(Instant::now()) <= LOAD_RETRY_BACKOFF,
            "the retry waits out the backoff"
        );

        // Not yet due: nothing happens, and the row is still held.
        assert!(!engine.tick_playback_health());
        assert_eq!(engine.state.current_index, Some(0));

        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_playback_health(), "the retry fires");
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "the retry reloads the same row"
        );
        assert!(engine.current_load_is_retry);
        assert!(engine.retry_current_at.is_none());

        // The retry fails too, and only now is the row given up on.
        engine.play_request_id = Some(11);
        assert!(engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 11,
            track_id: track_uri(),
        }));
        assert_eq!(
            engine.state.current_index,
            Some(1),
            "a row that failed twice is skipped"
        );
        assert!(
            engine.retry_current_at.is_none(),
            "the successor starts with a clean slate"
        );
    }

    /// The engine stops the failed loader itself, and librespot answers that
    /// with a `Stopped` carrying the play request the engine was holding.
    /// Handled as an ordinary stop it would clear `playing` three seconds
    /// before the retry is due, and the retry — which only fires for a queue
    /// that is playing — would never run.
    #[tokio::test(flavor = "current_thread")]
    async fn the_engines_own_stop_does_not_disarm_the_retry_it_just_armed() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        assert!(fail_current_row(&mut engine, 7));
        assert!(engine.retry_current_at.is_some());

        assert!(
            !engine.on_player_event(PlayerEvent::Stopped {
                play_request_id: 7,
                track_id: track_uri(),
            }),
            "the stopped loader's play request is no longer the engine's"
        );
        assert!(
            engine.state.playing,
            "the retry still has a queue to run in"
        );
        fire_due_retry(&mut engine);
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.current_load_is_retry);
    }

    /// The retry a failed row is owed has to ask for the position the row was
    /// loaded at, and the drift projection is what decides that. A failure
    /// takes seconds to arrive — the key request times out, librespot downloads
    /// without decryption, the decoder waits out its own deadline — and the
    /// engine's clock does not stop for it: the projection clamps at the row's
    /// own duration, so on a short row it landed on the final millisecond and
    /// the retry went out as a load *at the end of the track*. No audio either
    /// way, a second failure the row never earned, and a healthy one-second
    /// track skipped as unavailable — a run of them being exactly the queue
    /// that appeared to march through itself in silence.
    ///
    /// Freezing only *after* the failure would be too late: the seconds spent
    /// dying are exactly the seconds the projection has to survive, so the
    /// playhead must not move for a load that has not produced audio at all.
    /// Both halves are asserted here, in the order they happen.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_load_holds_the_playhead_where_the_retry_can_resume() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = playback_state(1_000);
        engine.state.playing = true;
        engine.update_position(0);

        // The load is in flight and has produced nothing yet; nine seconds of
        // wall clock pass before librespot gives up on it.
        engine.position_anchor = Some((0, Instant::now() - Duration::from_secs(9)));
        assert!(
            !engine.tick_position(),
            "a load with no audio yet is not playing, so there is no playhead to advance"
        );
        assert_eq!(
            engine.state.position_ms, 0,
            "the load offset is the last true position until audio exists"
        );

        assert!(fail_current_row(&mut engine, 7));
        assert!(
            !engine.tick_position(),
            "a load that failed is not playing, so there is no playhead to advance"
        );
        assert_eq!(engine.state.position_ms, 0, "the failure did not advance it either");

        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_playback_health(), "the armed retry fires");
        assert_eq!(
            engine.state.position_ms, 0,
            "the retry loads the row where it was asked to start, not at its end"
        );
        assert!(engine.current_load_is_retry);
    }

    /// What the UI needs to stop projecting on its own: a playing row reports
    /// `buffering` until the decoder produces its first packet, and the first
    /// packet is the state change that clears it. The position it clears at is
    /// the load offset — nothing has moved, precisely because the projection
    /// was held — so the local projection can resume from exactly where the
    /// engine says playback starts.
    #[test]
    fn buffering_covers_a_playing_row_until_its_first_packet_and_then_clears() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.update_position(30_000);

        let state = |engine: &mut Engine, buffer: &Arc<std::sync::Mutex<Vec<u8>>>| {
            engine.emit_state().expect("state emits");
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice::<serde_json::Value>(&std::mem::take(&mut *bytes))
                .expect("state json")
        };

        let before = state(&mut engine, &buffer);
        assert_eq!(before["playing"], true);
        assert_eq!(
            before["buffering"], true,
            "intent is playing but no packet has come out of this load yet"
        );
        assert_eq!(before["position_ms"], 30_000);

        // The decoder's first packet for the configured pipeline.
        assert!(
            engine.on_audio_signal(AudioSignal::Output {
                revision: engine.audio_revision,
            }),
            "the first packet is a state change: buffering just ended"
        );
        let after = state(&mut engine, &buffer);
        assert_eq!(after["buffering"], false);
        assert_eq!(
            after["position_ms"], 30_000,
            "audio starts where the load was told to start"
        );
        assert!(
            engine.tick_position(),
            "the playhead projects once there is audio to project"
        );
        assert!(
            engine.state.position_ms >= 30_000 && engine.state.position_ms < 31_000,
            "and it projects from the load offset, not from when the load was issued: {}",
            engine.state.position_ms
        );

        // Later packets are not news.
        assert!(!engine.on_audio_signal(AudioSignal::Output {
            revision: engine.audio_revision,
        }));

        // A paused row is not buffering: its position is simply frozen.
        engine.state.playing = false;
        assert_eq!(state(&mut engine, &buffer)["buffering"], false);
    }

    /// Pause is the cascade's off switch, and an armed retry must not route
    /// around it. The owner's complaint the last time this bug ran was a pause
    /// button that visibly did nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn pause_stops_an_armed_retry() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        assert!(engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert!(engine.retry_current_at.is_some());

        assert_eq!(engine.pause(), Ok(true));
        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(
            !engine.tick_playback_health(),
            "a paused queue does not retry"
        );
        assert!(!engine.state.playing);
        assert_eq!(engine.state.current_index, Some(0));
    }

    /// A run of failures is the key service refusing this client. Stopping is
    /// the only answer that does not empty the queue in silence and does not
    /// keep asking the throttle for more keys.
    #[tokio::test(flavor = "current_thread")]
    async fn consecutive_failures_stop_playback_with_an_error() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;

        // The first row's two attempts, which cost it one skip.
        assert!(fail_current_row(&mut engine, 7));
        assert_eq!(engine.state.current_index, Some(0));
        fire_due_retry(&mut engine);
        assert!(fail_current_row(&mut engine, 101));
        assert_eq!(engine.state.current_index, Some(1));
        assert!(
            engine.state.playing,
            "the queue keeps flowing below the limit"
        );

        // The replacement fails too. Three failures inside the window is the
        // service refusing this client, not three dead rows.
        assert!(fail_current_row(&mut engine, 102));
        assert!(!engine.state.playing, "the run trips the breaker");
        assert_eq!(
            engine.state.current_index,
            Some(1),
            "the breaker stops on the failed row rather than spending another"
        );
        assert!(
            engine.retry_current_at.is_none(),
            "a stop does not leave a retry armed behind it"
        );
        let error = engine.state.error.expect("the breaker explains itself");
        assert!(
            error.contains("tracks in a row"),
            "the UI needs a reason, got {error:?}"
        );
    }

    /// The breaker must not turn one dead row into a stop. Audio coming out
    /// resets the run, which is what tells "this track is gone" apart from
    /// "the service is gone".
    #[tokio::test(flavor = "current_thread")]
    async fn an_isolated_failure_still_skips_and_audio_resets_the_run() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;

        for round in 0..UNAVAILABLE_STOP_LIMIT + 1 {
            let index = engine.state.current_index.expect("a current row");

            // A lone bad row still costs two attempts and exactly one skip.
            assert!(fail_current_row(&mut engine, 300 + round as u64));
            assert_eq!(engine.state.current_index, Some(index));
            fire_due_retry(&mut engine);
            assert!(fail_current_row(&mut engine, 400 + round as u64));
            assert!(
                engine.state.playing,
                "round {round}: an isolated failure skips, it does not stop"
            );
            let landed = engine.state.current_index.expect("a replacement row");
            assert_eq!(landed, (index + 1) % engine.state.queue.len());

            // The replacement is heard, which is what a lone bad row looks
            // like. librespot's `Playing` deliberately does not count: a track
            // loaded without its audio key reaches it too.
            hear_audio(&mut engine);
            assert!(
                engine.recent_unavailable.is_empty(),
                "round {round}: audio ends the run"
            );
        }
    }

    /// The bug the owner reported twice. Spotify's key service refuses the
    /// key, librespot loads the track undecrypted anyway and announces it as
    /// playing, the playhead runs for three seconds over silence, and the
    /// decoder then chokes and reports `EndOfTrack` — indistinguishable, to
    /// the engine that used to believe it, from a track that finished. So the
    /// queue advanced, and advanced, and advanced.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_that_never_produced_audio_did_not_finish() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        // Exactly what librespot reports for a keyless track: loaded, playing,
        // and then three seconds later, over. The report itself is the echo of
        // a play this engine already calls playing, so it is not a change.
        assert!(!engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: track_uri(),
            position_ms: 0,
        }));
        engine.update_position(3_000);

        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "a track that never played is a failed load, not a finished one"
        );
        assert!(engine.loading_failed);
        assert!(engine.retry_current_at.is_some(), "the row is owed a retry");
        assert_eq!(
            engine.recent_unavailable.len(),
            1,
            "and the breaker finally counts it"
        );
    }

    /// The other half of that judgement: a track that really ended must still
    /// advance the queue, or the fix would simply stop playback instead.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_that_played_to_its_end_still_advances() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        // The engine is already playing this row, so librespot's report of the
        // same thing says nothing a state event would have to carry.
        assert!(!engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: track_uri(),
            position_ms: 0,
        }));
        let revision = engine.audio_revision;
        engine.on_audio_signal(AudioSignal::Output { revision });
        engine.update_position(engine.state.duration_ms);

        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(1));
        assert!(engine.state.playing);
        assert!(engine.recent_unavailable.is_empty());
    }

    /// A decoder that collapses halfway through a track it *was* playing also
    /// reports `EndOfTrack`. Audio came out, so the silence test passes it;
    /// the position test is what catches it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_that_broke_halfway_did_not_finish_either() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        let revision = engine.audio_revision;
        engine.on_audio_signal(AudioSignal::Output { revision });
        engine.update_position(engine.state.duration_ms / 2);

        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.loading_failed);
    }

    /// A failed *preload* carries the current track's play request and the
    /// preloaded track's id, so it matched no arm at all and the breaker never
    /// saw it. It is evidence about the service, so it counts — and it must
    /// not disturb the track that is playing perfectly well.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_preload_counts_toward_the_breaker_without_moving_the_queue() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        engine.preload_armed = true;

        let preloaded = SpotifyUri::from_uri(&engine.state.queue[1].uri).expect("a valid uri");
        assert!(
            !engine.on_player_event(PlayerEvent::Unavailable {
                play_request_id: 7,
                track_id: preloaded,
            }),
            "nothing the user can see changed"
        );
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.state.playing);
        assert!(!engine.loading_failed);
        assert_eq!(
            engine.recent_unavailable.len(),
            1,
            "the refusal still counts against the service"
        );
        assert!(
            engine.preload_target().is_none(),
            "and holds the next preload back"
        );
    }

    /// Preloading asks for a second audio key per load. While the key service
    /// is the thing failing, that is the request that must not be made.
    #[test]
    fn a_failure_burst_holds_back_the_next_track_preload() {
        let mut engine = playing_engine();
        engine.state = two_track_state();
        engine.state.playing = true;
        engine.preload_armed = true;

        assert!(
            engine.preload_target().is_some(),
            "an armed, healthy engine preloads the next track"
        );
        engine.record_unavailable(Instant::now());
        assert!(
            engine.preload_target().is_none(),
            "a failing engine must not ask for a second audio key per load"
        );
        engine.clear_unavailable_burst();
        assert!(
            engine.preload_target().is_some(),
            "a recovered engine restores gapless preloading"
        );
    }

    /// The preload watermark. A track the user just started is not about to
    /// finish, so no second audio key is spent on it; a track being listened
    /// to arms one with plenty of lead.
    #[test]
    fn a_preload_waits_until_the_current_track_is_nearly_over() {
        let mut engine = playing_engine();
        engine.state = two_track_state();
        engine.state.playing = true;

        engine.update_position(0);
        assert!(!engine.at_preload_watermark());
        engine.update_position(240_000 - PRELOAD_WATERMARK_MS - 1_000);
        assert!(
            !engine.at_preload_watermark(),
            "a second before the watermark is still churn territory"
        );
        engine.update_position(240_000 - PRELOAD_WATERMARK_MS);
        assert!(engine.at_preload_watermark());

        // A track shorter than the watermark must still preload, so the floor
        // is capped at half its length the way `play_qualifies` caps its own.
        let mut short = playing_engine();
        short.state = playback_state(20_000);
        short.state.playing = true;
        short.update_position(9_000);
        assert!(!short.at_preload_watermark());
        short.update_position(10_000);
        assert!(
            short.at_preload_watermark(),
            "half a short track is its watermark"
        );
    }

    /// Clicking through a queue is what provokes the key service, and it is
    /// the case the watermark makes free: no track is ever near its end, so
    /// no preload is ever asked for.
    #[test]
    fn clicking_through_tracks_never_arms_a_preload() {
        let mut engine = playing_engine();
        engine.state = five_track_state();
        engine.state.playing = true;

        for _ in 0..5 {
            engine.update_position(2_000);
            engine.tick_playback_health();
            assert!(
                !engine.preload_armed,
                "a track two seconds in is not about to finish"
            );
            engine.state.current_index =
                Some((engine.state.current_index.expect("a current row") + 1) % 5);
            engine.state.duration_ms = 180_000;
        }
    }

    /// Edge-triggered per load, and never while paused. A paused track is not
    /// approaching its end, and scrubbing across the watermark must not buy
    /// the same key again.
    #[test]
    fn the_watermark_arms_once_per_load_and_not_while_paused() {
        let mut engine = playing_engine();
        engine.state = two_track_state();
        engine.state.playing = false;
        engine.update_position(239_000);
        engine.tick_playback_health();
        assert!(
            !engine.preload_armed,
            "a paused track is not approaching its end"
        );

        // Resuming re-evaluates on the next tick without anything special.
        engine.state.playing = true;
        engine.tick_playback_health();
        assert!(engine.preload_armed);

        // Scrubbing back and forward across it changes nothing: only a load
        // disarms it.
        engine.update_position(1_000);
        engine.tick_playback_health();
        assert!(engine.preload_armed);
    }

    /// The webview warms the next Canvas when, and only when, the audio is
    /// fetched ahead, so arming the preload is a state of its own and every
    /// state names the row. A seek back keeps librespot's preload, and the
    /// name with it.
    #[test]
    fn an_armed_preload_is_published_with_the_row_it_fetches() {
        let (mut engine, buffer) = test_engine();
        engine.state = two_track_state();
        engine.state.playing = true;
        engine.update_position(1_000);
        assert!(!engine.tick_playback_health(), "a track a second in has nothing to say");
        engine.emit_state().expect("early state emits");

        engine.update_position(239_000);
        assert!(engine.tick_playback_health(), "arming is a state change");
        assert!(!engine.tick_playback_health(), "and happens once per load");
        engine.emit_state().expect("armed state emits");
        engine.update_position(1_000);
        engine.emit_state().expect("sought state emits");

        let values = captured_messages(&buffer);
        let next = engine.state.queue[1].uri.as_str();
        assert!(values[0]["preload_uri"].is_null());
        assert_eq!(values[1]["preload_uri"], next);
        assert_eq!(values[2]["preload_uri"], next);
    }

    #[test]
    fn loop_marker_reloads_when_decoder_eof_arrived_first() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(loop_range(1_500, 2_000, 2)),
        });
        note_audio(&mut engine);
        assert!(!engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert!(engine.loop_decoder_eof);

        assert!(engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 1_500,
            revision: engine.audio_revision,
        }));
        assert_eq!(engine.state.position_ms, 1_500);
        assert!(
            !engine.loop_decoder_eof,
            "the EOF continuation used load_current, not invalid EndOfTrack seek"
        );
    }

    #[test]
    fn loop_eof_after_an_audible_marker_does_not_wait_for_a_second_marker() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(loop_range(900, 1_000, 2)),
        });
        note_audio(&mut engine);
        engine.loop_jump_pending = true;
        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.position_ms, 900);
        assert!(
            !engine.loop_jump_pending,
            "the EOF race immediately reloaded and consumed the pending jump"
        );
    }

    #[test]
    fn fresh_loads_and_seeks_derive_loop_pass_from_source_position() {
        for (position_ms, expected_pass) in
            [(0, 1), (20_000, 1), (39_999, 1), (40_000, 3), (80_000, 3)]
        {
            let (mut engine, _) = test_engine();
            engine.state =
                edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 3)));
            engine.state.position_ms = position_ms;
            engine.current_needs_load = true;

            engine
                .seek_source(position_ms)
                .expect("unloaded seek should not need a player");
            assert_eq!(engine.loop_pass, expected_pass);
        }

        for (position_ms, expected_pass) in [(0, 1), (20_000, 1), (40_000, 3), (80_000, 3)] {
            let (mut engine, _) = test_engine();
            engine.state =
                edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 3)));
            engine.state.position_ms = position_ms;

            assert!(
                engine.load_current(true).is_err(),
                "the capture engine has no player"
            );
            assert_eq!(engine.loop_pass, expected_pass);
        }
    }

    #[test]
    fn preview_load_at_or_after_loop_end_starts_the_completed_pass() {
        let (mut engine, _) = test_engine();
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 100_000,
            ..TrackRef::default()
        };

        assert!(
            engine
                .preview_track_edit(
                    track,
                    Vec::new(),
                    Some(loop_range(20_000, 40_000, 2)),
                    40_000,
                    1,
                )
                .is_err(),
            "the capture engine has no player"
        );
        assert_eq!(engine.state.position_ms, 40_000);
        assert_eq!(engine.loop_pass, 2);
    }

    #[test]
    fn eof_at_or_after_loop_end_advances_without_waiting_for_a_marker() {
        for position_ms in [40_000, 80_000] {
            let mut engine = playing_engine();
            engine.state.duration_ms = 100_000;
            engine.state.queue[0].duration_ms = 100_000;
            engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
                cuts: Vec::new(),
                loop_range: Some(loop_range(20_000, 40_000, 3)),
            });
            engine.state.position_ms = position_ms;
            engine.reset_loop_pass_for_position(position_ms);
            note_audio(&mut engine);

            assert_eq!(engine.loop_pass, 3);
            assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
                play_request_id: 7,
                track_id: track_uri(),
            }));
            assert!(!engine.loop_decoder_eof);
            assert!(!engine.loop_jump_pending);
            assert_eq!(engine.state.position_ms, 100_000);
        }
    }

    #[test]
    fn finite_loop_markers_stop_after_the_requested_total_passes() {
        for play_count in [2, 3] {
            let mut engine = playing_engine();
            engine.state.duration_ms = 100_000;
            engine.state.queue[0].duration_ms = 100_000;
            engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
                cuts: Vec::new(),
                loop_range: Some(loop_range(20_000, 40_000, play_count)),
            });
            engine.state.playing = true;
            engine.current_needs_load = true;
            let mut markers = 0;

            for pass in 2..=play_count {
                assert!(engine.on_audio_signal(AudioSignal::LoopBoundary {
                    position_ms: 20_000,
                    revision: engine.audio_revision,
                }));
                assert_eq!(engine.loop_pass, pass);
                markers += 1;
            }
            assert_eq!(markers, play_count - 1);
            assert!(!engine.on_audio_signal(AudioSignal::LoopBoundary {
                position_ms: 20_000,
                revision: engine.audio_revision,
            }));
            assert_eq!(engine.loop_pass, play_count);
        }
    }

    #[test]
    fn stale_loop_marker_from_replaced_audio_is_ignored() {
        let mut engine = playing_engine();
        engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(loop_range(20_000, 40_000, 2)),
        });
        let stale_revision = engine.audio_revision;
        engine.invalidate_audio_signals();

        assert!(!engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 20_000,
            revision: stale_revision,
        }));
        assert_eq!(engine.loop_pass, 1);
    }

    #[test]
    fn speed_reconfiguration_preserves_internal_pass_and_finishes_at_loop_end() {
        let (mut engine, _) = test_engine();
        engine.state =
            edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 3)));
        engine.current_needs_load = true;
        engine.state.position_ms = 25_000;
        engine.loop_pass = 2;

        assert_eq!(engine.set_playback_speed(1.5), Ok(true));
        assert_eq!(engine.loop_pass, 2);

        engine.state.position_ms = 40_000;

        assert_eq!(engine.set_playback_speed(2.0), Ok(true));
        assert_eq!(engine.loop_pass, 3);
    }

    #[test]
    fn speed_changes_are_finite_bounded_and_do_not_restart_loaded_or_paused_audio() {
        let (mut engine, _) = test_engine();
        engine.state = edited_playback_state(100_000, Vec::new(),
            Some(loop_range(20_000, 40_000, 3)));
        engine.state.position_ms = 25_000;
        engine.state.playing = false;
        engine.loop_pass = 2;
        engine.loop_decoder_eof = true;
        engine.loop_jump_pending = true;
        engine.history.push(0);
        let queue = serde_json::to_value(&engine.state.queue).unwrap();
        for speed in [0.5, 3.0, 4.0, 1.0] {
            assert_eq!(engine.set_playback_speed(speed), Ok(true));
            assert_eq!(engine.state.position_ms, 25_000);
            assert!(!engine.state.playing);
            assert_eq!(engine.loop_pass, 2);
            assert!(engine.loop_decoder_eof);
            assert!(engine.loop_jump_pending);
            assert_eq!(engine.history, vec![0]);
            assert_eq!(serde_json::to_value(&engine.state.queue).unwrap(), queue);
        }
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.499, 4.001] {
            assert!(engine.set_playback_speed(invalid).is_err());
            assert_eq!(engine.state.playback_speed(), 1.0);
            assert_eq!(engine.state.position_ms, 25_000);
        }
        assert_eq!(engine.set_playback_speed(1.0), Ok(false));
    }

    fn episode_ref(id: &str, duration_ms: u32) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:episode:{id}"),
            duration_ms,
            ..TrackRef::default()
        }
    }

    /// Songs and episodes keep their own speed, and the switch happens where
    /// the engine sees the item change — here at the natural end of a song
    /// whose next row is an episode, and back at the episode's end. A speed
    /// change in between edits only the kind that is playing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_natural_boundary_switches_to_the_next_items_speed_and_back() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = two_track_state();
        let song = engine.state.queue[1].clone();
        engine.state.queue[1] = episode_ref("0123456789ABCDEFGHIJKE", 300_000);
        engine.state.queue.push(song);
        engine.state.playing = true;
        assert_eq!(engine.set_playback_speeds(PlaybackSpeeds { track: 1.0, episode: 1.5 }), Ok(true));
        assert_eq!(engine.state.playback_speed(), 1.0, "a song plays at the songs' speed");

        note_audio(&mut engine);
        engine.update_position(engine.state.duration_ms);
        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7, track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(1));
        assert!(engine.state.playing);
        assert_eq!(engine.state.playback_speed(), 1.5, "the episode loads at the episodes' speed");
        assert_eq!(engine.audible_speed, None, "the episode's audio is configured fresh, at its own rate");

        assert_eq!(engine.set_playback_speed(2.0), Ok(true));
        assert_eq!(engine.state.speeds, PlaybackSpeeds { track: 1.0, episode: 2.0 });

        let episode = SpotifyUri::from_uri(&engine.state.queue[1].uri).expect("an episode uri");
        engine.play_request_id = Some(8);
        note_audio(&mut engine);
        engine.update_position(engine.state.duration_ms);
        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 8, track_id: episode,
        }));
        assert_eq!(engine.state.current_index, Some(2));
        assert_eq!(engine.state.playback_speed(), 1.0, "the next song is back at the songs' speed");
        engine.shutdown();
    }

    /// The shell persists both speeds from the state line and hands them back,
    /// in one command, before it restores the queue. The restored row's kind
    /// picks its speed at once, with no audio loaded, so the player bar shows
    /// the right one before anything plays.
    #[test]
    fn both_speeds_ride_the_state_line_and_come_back_before_a_restore() {
        let (mut engine, buffer) = test_engine();
        engine.state.ready = true;
        let restore: Command = serde_json::from_value(serde_json::json!({
            "type": "set_playback_speeds", "track_speed": 1.25, "episode_speed": 1.75,
        }))
        .expect("the restore command parses");
        let Command::SetPlaybackSpeeds { track_speed, episode_speed } = restore else {
            panic!("parsed as another command");
        };
        let speeds = PlaybackSpeeds { track: track_speed, episode: episode_speed };
        assert_eq!(engine.set_playback_speeds(speeds), Ok(true));
        assert_eq!(engine.set_playback_speeds(speeds), Ok(false), "the same pair is no change");
        let song = TrackRef {
            uri: "spotify:track:1abcdefghijklmnopqrstu".to_owned(),
            duration_ms: 240_000,
            ..TrackRef::default()
        };
        let queue = vec![song, episode_ref("2abcdefghijklmnopqrstu", 300_000)];
        assert_eq!(
            engine.restore_queue(queue, 1, 60_000, String::new(), 0, false, false, false),
            Ok(true)
        );
        assert_eq!(engine.state.playback_speed(), 1.75);

        engine.emit_state().unwrap();
        let line = take_line(&buffer);
        assert_eq!(line["playback_speed"], 1.75);
        assert_eq!(line["audible_playback_speed"], 1.75);
        assert_eq!(line["track_speed"], 1.25);
        assert_eq!(line["episode_speed"], 1.75);

        // Either value out of range refuses the pair and keeps both.
        for invalid in [f32::NAN, 0.25, 4.5] {
            assert!(engine.set_playback_speeds(PlaybackSpeeds { track: 1.0, episode: invalid }).is_err());
            assert!(engine.set_playback_speeds(PlaybackSpeeds { track: invalid, episode: 1.0 }).is_err());
            assert_eq!(engine.state.speeds, speeds);
        }
    }

    #[test]
    fn speed_boundary_settles_old_rate_before_projecting_new_rate() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(100_000);
        engine.state.playing = true;
        engine.current_load_produced_audio = true;
        // No device ring in this deterministic fallback-clock scenario.
        engine.audio_revision = u64::MAX;
        let start = Instant::now();
        engine.position_anchor = Some((10_000, start));
        engine.state.speeds.track = 4.0;
        engine.audible_speed = Some(1.0);
        let boundary = start + Duration::from_millis(150);
        assert!(engine.on_audio_signal(AudioSignal::SpeedBoundary {
            speed: 4.0, revision: engine.audio_revision, at: boundary,
        }));
        assert_eq!(engine.state.position_ms, 10_150);
        assert!(engine.tick_position_at(boundary + Duration::from_millis(100)));
        assert_eq!(engine.state.position_ms, 10_550);
        assert!(!engine.on_audio_signal(AudioSignal::SpeedBoundary {
            speed: 0.5, revision: engine.audio_revision - 1, at: boundary,
        }));
        assert_eq!(engine.state.position_ms, 10_550);
    }

    #[test]
    fn resuming_discarded_output_starts_the_consumer_clock_at_the_paused_decoder_offset() {
        let (mut engine, _) = test_engine();
        engine.state = edited_playback_state(100_000, Vec::new(),
            Some(loop_range(20_000, 40_000, 3)));
        engine.state.playing = false;
        engine.update_position(25_000);
        engine.loop_pass = 2;
        engine.current_load_produced_audio = true;
        engine.audio_revision = u64::MAX;
        engine.audio_start_position_ms = 0;
        let transition = engine.prepare_audio_resume().expect("discarded output needs a decoder-owned transition");
        assert!((transition.commit)());
        assert_eq!(engine.state.position_ms, 25_000);
        assert!(!engine.state.playing);
        assert_eq!(engine.loop_pass, 2);
        assert_eq!(engine.source_position_after_audio(0.0), 25_000);
        assert_eq!(engine.source_position_after_audio(300.0), 25_300);
    }

    #[test]
    fn play_reloads_after_decoder_eof_before_a_loop_marker() {
        let (mut engine, _) = test_engine();
        engine.state =
            edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 2)));
        engine.state.position_ms = 25_000;
        engine.loop_pass = 1;
        engine.loop_decoder_eof = true;

        assert!(engine.play().is_err(), "the capture engine has no player");
        assert!(
            !engine.loop_decoder_eof,
            "play must re-arm a loader instead of calling play on EndOfTrack"
        );
    }

    /// The first invalid-session tick must not create the audible stall: an
    /// active queue keeps projecting until replacement handles are ready.
    #[tokio::test(flavor = "current_thread")]
    async fn active_session_reconnect_preserves_playback_until_handover() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        note_audio(&mut engine);
        engine.position_anchor = Some((10_000, Instant::now() - Duration::from_secs(2)));
        engine.play_request_id = Some(7);
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        session.shutdown();
        engine.session = Some(session);
        let generation = engine.generation;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            engine.player_rebuild_is_current(generation, false),
            "a rebuild started while the session was ready is initially current"
        );

        assert_eq!(engine.prepare_session_reconnect(), Some(true));
        assert!(engine.state.playing, "the logical transport must not blip");
        assert!(
            engine.tick_position(),
            "the dead session does not freeze projection"
        );
        assert!(
            (11_900..12_500).contains(&engine.state.position_ms),
            "position must still project from the pre-drop anchor: {}",
            engine.state.position_ms
        );
        assert!(engine.set_normalisation(true, &sender));
        assert!(
            receiver.try_recv().is_err(),
            "backoff must not rebuild a player on the dead session"
        );

        // Exercise handover without credentials or a Spotify network task.
        let reconnect_generation = engine.begin_cached_authentication(true);
        assert_ne!(reconnect_generation, generation);
        assert!(engine.auth_running);
        assert!(engine.state.playing);
        assert!(
            engine.session.is_some(),
            "old handles survive authentication"
        );
        assert!(engine.set_normalisation(false, &sender));
        assert!(
            receiver.try_recv().is_err(),
            "authentication must consume the shared preference without a competing rebuild"
        );
        assert!(!engine.on_player_signal(PlayerSignal::Event {
            generation,
            event: PlayerEvent::Playing {
                play_request_id: 7,
                track_id: track_uri(),
                position_ms: 1,
            },
        }));

        let before_handover = engine.state.position_ms;
        engine.stop_playback_handles();
        assert!(engine.state.position_ms >= before_handover);
        assert!(
            engine.state.playing,
            "handover preserves captured resume intent"
        );
        assert!(
            engine.session.is_none(),
            "the old session is stopped at handover"
        );
    }

    /// A reconnect that keeps the playing player keeps commanding it, so it
    /// has to keep hearing it: an end of track nobody heard left that player's
    /// sink running over an empty ring for as long as the network stayed down.
    #[tokio::test(flavor = "current_thread")]
    async fn a_preserved_player_is_stopped_at_its_end_of_track_during_a_reconnect() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = two_track_state();
        engine.state.playing = true;
        let generation = engine.generation;
        engine.player_generation = Some(generation);
        note_audio(&mut engine);
        engine.update_position(240_000);
        engine.begin_cached_authentication(true);
        // The attempt failed on transport and the retry waits out its backoff.
        engine.auth_running = false;
        engine.awaiting_transport_retry = true;
        let reconnect = Instant::now() + RECONNECT_BACKOFF_MAX;
        engine.next_reconnect = Some(reconnect);
        assert!(engine.player.is_some(), "the kept player sounds through the outage");

        assert!(engine.on_player_signal(PlayerSignal::Event {
            generation,
            event: PlayerEvent::EndOfTrack { play_request_id: 7, track_id: track_uri() },
        }));
        assert_eq!(engine.state.current_index, Some(1), "the queue moves on");
        assert!(engine.state.playing && engine.current_needs_load,
            "the next row loads once the session is back");
        assert!(engine.play_request_id.is_none(), "the kept player is stopped, not handed a load");
        assert_eq!(engine.next_work_deadline(), Some(reconnect), "nothing audible is left to observe");
    }

    /// The whole point of the preserved handover is that the overlap ends the
    /// instant the replacement arrives. An old player left alive would keep
    /// feeding the audio device while pause, next and seek all reached the new
    /// one — audible playback no transport control can touch, until the app is
    /// restarted. Assert the old handles are gone, not merely forgotten.
    #[tokio::test(flavor = "current_thread")]
    async fn preserved_handover_leaves_exactly_one_live_player() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.play_request_id = Some(7);

        let old_sink = SinkProbe::new();
        let (old_player, old_session) = probe_player(&old_sink);
        let old_player_handle = Arc::downgrade(&old_player);
        engine.player = Some(old_player);
        engine.session = Some(old_session.clone());
        old_session.shutdown();

        assert_eq!(engine.prepare_session_reconnect(), Some(true));
        let generation = engine.begin_cached_authentication(true);
        assert!(
            old_player_handle.upgrade().is_some(),
            "the old player plays on while the replacement is built"
        );

        let new_sink = SinkProbe::new();
        let (new_player, new_session) = probe_player(&new_sink);
        let new_player_handle = Arc::downgrade(&new_player);
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation,
                result: Ok(connected_handles(new_player, new_session)),
            },
            player_sender,
        ));

        assert!(
            old_player_handle.upgrade().is_none(),
            "the old player is dropped at handover, not merely forgotten"
        );
        assert!(
            !old_sink.is_alive(),
            "the old player's audio sink dies with it"
        );
        let installed = engine
            .player
            .as_ref()
            .expect("the replacement is installed");
        assert!(
            new_player_handle
                .upgrade()
                .is_some_and(|new| Arc::ptr_eq(installed, &new)),
            "the installed player is the replacement"
        );
        assert_eq!(
            Arc::strong_count(installed),
            1,
            "the engine owns the only reference to the live player"
        );
        assert!(engine.state.playing, "the resume intent survived");
        assert!(new_sink.is_alive());
    }

    /// The same teardown has to run when the attempt began preserved but the
    /// resume intent did not survive it: assigning the replacement over a live
    /// player would silently leave the old one playing and the old session
    /// unshut.
    #[tokio::test(flavor = "current_thread")]
    async fn a_completion_without_resume_intent_still_tears_the_old_player_down() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);

        let old_sink = SinkProbe::new();
        let (old_player, old_session) = probe_player(&old_sink);
        let old_player_handle = Arc::downgrade(&old_player);
        engine.player = Some(old_player);
        engine.session = Some(old_session.clone());
        let generation = engine.begin_cached_authentication(true);

        let new_sink = SinkProbe::new();
        let (new_player, new_session) = probe_player(&new_sink);
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation,
                result: Ok(connected_handles(new_player, new_session)),
            },
            player_sender,
        ));

        assert!(
            old_player_handle.upgrade().is_none(),
            "the old player is dropped rather than assigned over"
        );
        assert!(!old_sink.is_alive());
        assert!(
            old_session.is_invalid(),
            "the old session is shut down, not merely dropped: a clone held by \
             an in-flight browse would otherwise keep it connected"
        );
        assert!(!engine.state.playing);
        assert_eq!(
            Arc::strong_count(
                engine
                    .player
                    .as_ref()
                    .expect("the replacement is installed")
            ),
            1,
        );
    }

    /// Paused reconnects back off between attempts and fall back to explicit
    /// login when there are no cached credentials.
    #[tokio::test(flavor = "current_thread")]
    async fn paused_session_reconnect_remains_cold_and_uses_backoff() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        session.shutdown();
        engine.session = Some(session);
        let generation = engine.generation;
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(engine.prepare_session_reconnect(), Some(true));
        assert_eq!(engine.prepare_session_reconnect(), None);
        engine.next_reconnect = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(engine.prepare_session_reconnect(), Some(true));
        assert!(engine.reconnect_backoff > RECONNECT_BACKOFF_MIN);
        assert!(engine.reconnect_backoff <= RECONNECT_BACKOFF_MAX);
        engine.next_reconnect = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_session_health(&sender));
        assert_ne!(engine.generation, generation, "the reconnect must fire immediately");
        assert!(!engine.auth_running, "no implicit OAuth flow may start");
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.session.is_none());
        assert_eq!(engine.state.auth_state, renderer_engine::protocol::AuthState::NeedsLogin);
    }

    /// A healthy session must cost nothing and must clear any backoff a past
    /// outage built up, so the next drop is recovered from just as quickly.
    #[tokio::test(flavor = "current_thread")]
    async fn a_healthy_session_schedules_no_reconnect() {
        let (mut engine, _) = test_engine();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        engine.next_reconnect = Some(Instant::now());
        engine.reconnect_backoff = RECONNECT_BACKOFF_MAX;

        // No session at all is the pre-auth state, not a dead session.
        assert!(!engine.tick_session_health(&sender));
        assert!(engine.next_reconnect.is_none());
        assert_eq!(engine.reconnect_backoff, RECONNECT_BACKOFF_MIN);

        engine.session = Some(librespot_core::Session::new(
            librespot_core::SessionConfig::default(),
            None,
        ));
        engine.next_reconnect = Some(Instant::now());
        assert!(!engine.tick_session_health(&sender));
        assert!(
            engine.next_reconnect.is_none(),
            "a live session is not a corpse"
        );
    }

    /// The regression that made the classifier inert: it was sized against how
    /// fast a user clicks, but it is fed by how fast loads *fail*, which is
    /// seconds slower. Measured gaps from a real dead-session episode were
    /// 6.5 s and 8.9 s; at the old 2 s window every one of these looked
    /// isolated and evicted the cache for a track that was fine.
    #[test]
    fn failures_at_the_observed_cadence_stay_clustered() {
        let mut engine = playing_engine();
        let start = Instant::now();

        assert!(
            !engine.record_unavailable(start).clustered,
            "the first failure in a quiet period stays eligible for cleanup"
        );
        let second = start + Duration::from_millis(8_900);
        assert!(
            engine.record_unavailable(second).clustered,
            "a failure 8.9 s later is the same outage, not a corrupt cache file"
        );
        assert!(
            engine
                .record_unavailable(second + Duration::from_millis(6_500))
                .clustered,
            "and so is the one after that"
        );
    }

    #[test]
    fn unavailable_burst_classifier_keeps_isolated_cleanup_eligible() {
        let mut engine = playing_engine();
        let start = Instant::now();

        // One quiet load failure is eligible for the corrupt-cache cleanup.
        engine.note_track_change(start);
        assert!(
            !engine
                .record_unavailable(start + Duration::from_millis(10))
                .clustered
        );

        // A second failure shortly afterward is clustered, so valid cache
        // files are preserved while the key/network burst settles.
        assert!(
            engine
                .record_unavailable(start + Duration::from_millis(20))
                .clustered
        );

        // After both windows expire, cleanup is eligible again.
        let quiet = start
            + TRACK_CHANGE_BURST_WINDOW.max(UNAVAILABLE_BURST_WINDOW)
            + Duration::from_millis(25);
        assert!(!engine.record_unavailable(quiet).clustered);

        // Two paced load starts protect even the first failure observed after
        // a rapid-click burst.
        engine.note_track_change(quiet);
        engine.note_track_change(quiet + Duration::from_millis(100));
        assert!(
            engine
                .record_unavailable(quiet + Duration::from_millis(200))
                .clustered
        );
    }

    /// Every message the capture buffer has accumulated, parsed. A test that
    /// reads the wire has to empty it first, so this takes the bytes.
    fn captured_messages(buffer: &Arc<std::sync::Mutex<Vec<u8>>>) -> Vec<serde_json::Value> {
        let mut bytes = buffer.lock().expect("buffer lock");
        std::mem::take(&mut *bytes)
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).expect("every line is one json message"))
            .collect()
    }

    /// The queue rows ride one state per generation, not one per state.
    ///
    /// One track change used to publish four or five full states, each of them
    /// re-serializing the whole queue — a megabyte at playlist scale — to say
    /// nothing about it. The revision is what makes dropping that affordable:
    /// the rows travel with it, every other state travels without them, and a
    /// receiver holding that generation keeps the rows it already has.
    #[test]
    fn a_state_carries_the_queue_rows_only_when_the_revision_moved() {
        let (mut engine, buffer) = test_engine();
        engine.state = two_track_state();

        engine.emit_state().expect("first state emits");
        engine.emit_state().expect("idle state emits");
        assert!(engine.move_queue(1, 0).expect("the move is legal"));
        engine.emit_state().expect("moved state emits");

        let values = captured_messages(&buffer);
        assert_eq!(values.len(), 3, "one line per emit_state");
        let revision = values[0]["queue_revision"].as_u64().expect("a revision");
        assert_eq!(values[0]["queue"].as_array().map(Vec::len), Some(2));
        assert!(values[0]["upcoming"].is_array());

        // The second state says exactly what the first said about the queue, so
        // it does not say it again — and it still says everything else.
        assert_eq!(values[1]["queue_revision"].as_u64(), Some(revision));
        assert!(
            !values[1].as_object().expect("object").contains_key("queue"),
            "an unchanged queue must not be serialized"
        );
        assert!(!values[1].as_object().expect("object").contains_key("upcoming"));
        assert_eq!(values[1]["current_index"], values[0]["current_index"]);

        // The move is a new generation, so the rows come back with it: the
        // reordered list, under a revision the receiver has not been given.
        let moved = values[2]["queue"].as_array().expect("rows after the move");
        assert_eq!(moved[0]["id"], values[0]["queue"][1]["id"]);
        assert!(
            values[2]["queue_revision"].as_u64().expect("a revision") > revision,
            "a change publishes a newer generation"
        );
    }

    /// The revision has to be exact in the number the webview reads it as.
    ///
    /// The wire carries it as a JSON number, and the webview parses that as a
    /// JavaScript number — a double, exact only for integers up to 2^53. A seed
    /// above that bound does not merely round: the increments disappear into
    /// the rounding, so a state offering a changed queue arrives under the
    /// number of the queue the webview already has and its rows are dropped.
    #[test]
    fn a_revision_and_its_successor_are_different_numbers_to_the_webview() {
        let (mut engine, buffer) = test_engine();
        engine.state = two_track_state();

        let seed = engine.rows_revision;
        engine.emit_state().expect("first state emits");
        engine.rows_changed();
        engine.emit_state().expect("bumped state emits");

        // `as_f64` is the rounding JavaScript's `Number` applies to a JSON
        // integer, so this is what `Number(payload.queue_revision)` holds.
        let values = captured_messages(&buffer);
        let read = |value: &serde_json::Value| {
            value["queue_revision"].as_f64().expect("a revision")
        };
        let first = read(&values[0]);
        let second = read(&values[1]);
        assert_eq!(first, seed as f64, "the seed arrives unchanged");
        assert_eq!(second, (seed + 1) as f64, "and so does the next revision");
        assert_ne!(
            first, second,
            "one bump has to be a different number to the webview"
        );
        assert_eq!(second as u64, seed + 1, "the number names exactly one row set");
    }

    #[test]
    fn drift_tick_reports_position_only_while_playing_with_an_anchor() {
        let mut engine = playing_engine();
        engine.state.playing = false;
        assert!(!engine.tick_position());

        // Intent alone is not playback: a load that has not produced a packet
        // yet is buffering, and its playhead is where it will start, not a
        // position anything has reached.
        engine.state.playing = true;
        assert!(!engine.tick_position(), "silence is not progress");

        // The first packet is what starts the projection, anchored where the
        // load was told to start rather than at the instant it was issued.
        note_audio(&mut engine);
        assert!(engine.tick_position());
        assert!(engine.state.position_ms < 1_000);

        engine.update_position(10_000);
        assert!(engine.tick_position());
        assert!(engine.state.position_ms >= 10_000);
        assert!(engine.state.position_ms < 11_000);

        engine.state.playing = false;
        assert!(!engine.tick_position(), "paused positions are static");
    }

    #[test]
    fn drift_tick_clamps_at_the_track_end() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        note_audio(&mut engine);
        engine.state.duration_ms = 30_000;
        engine.update_position(29_950);
        std::thread::sleep(Duration::from_millis(120));
        assert!(engine.tick_position());
        assert_eq!(engine.state.position_ms, 30_000);
    }

    #[test]
    fn heartbeat_emits_a_scalar_position_line_not_a_full_state() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        note_audio(&mut engine);
        engine.update_position(15_000);
        assert!(engine.tick_position());
        engine.emit_position().expect("position emits");

        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("position json");
        let object = value.as_object().expect("position is an object");
        // `type` plus the two playhead scalars: a heartbeat never serializes
        // the queue, so the steady-state cost is O(1) in queue length.
        assert_eq!(object.len(), 3, "only type, position_ms and duration_ms");
        assert_eq!(value["type"], "position");
        assert_eq!(value["duration_ms"], 240_000);
        let position_ms = value["position_ms"].as_u64().expect("scalar position");
        assert!(
            (15_000..16_000).contains(&position_ms),
            "position projects from the anchor: {position_ms}"
        );
        assert!(
            !object.contains_key("queue"),
            "a heartbeat never carries the queue"
        );
        assert!(
            !object.contains_key("playing"),
            "a heartbeat carries no flags"
        );
    }

    #[test]
    fn edited_state_and_heartbeat_serialize_compiled_transport_scalars() {
        let (mut engine, buffer) = test_engine();
        engine.state = edited_playback_state(100_000, vec![range(10_000, 60_000)], None);
        engine.update_position(65_000);

        engine.emit_state().expect("edited state emits");
        let state: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("state json")
        };
        assert_eq!(state["position_ms"], 15_000);
        assert_eq!(state["duration_ms"], 50_000);
        assert_eq!(
            state["queue"][0]["duration_ms"], 100_000,
            "queue metadata stays in source coordinates"
        );

        engine.update_position(100_000);
        engine.emit_position().expect("edited heartbeat emits");
        let heartbeat: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("heartbeat json")
        };
        assert_eq!(heartbeat["position_ms"], 50_000);
        assert_eq!(heartbeat["duration_ms"], 50_000);
    }

    #[test]
    fn unedited_state_serialization_keeps_source_scalars_unchanged() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(100_000);
        engine.update_position(65_000);
        engine.emit_state().expect("unedited state emits");
        let state: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("state json")
        };
        assert_eq!(state["position_ms"], 65_000);
        assert_eq!(state["duration_ms"], 100_000);
    }

    #[test]
    fn edited_tick_crosses_a_long_cut_continuously_at_every_speed() {
        for (speed, expected_compiled_ms, expected_source_ms) in [
            (0.5, 10_500_u32, 60_500_u32),
            (1.0, 11_500_u32, 61_500_u32),
            (2.0, 13_500_u32, 63_500_u32),
        ] {
            let (mut engine, _) = test_engine();
            engine.state = edited_playback_state(100_000, vec![range(10_000, 60_000)], None);
            engine.state.playing = true;
            engine.state.speeds.track = speed;
            note_audio(&mut engine);
            engine.position_anchor = Some((9_500, Instant::now() - Duration::from_secs(2)));

            assert!(engine.tick_position());
            let (compiled_ms, duration_ms) = engine.transport_position_and_duration();
            assert_eq!(duration_ms, 50_000);
            assert!(
                compiled_ms.abs_diff(expected_compiled_ms) <= 5,
                "{speed}x projected to {compiled_ms}, expected {expected_compiled_ms}"
            );
            assert!(
                engine.state.position_ms.abs_diff(expected_source_ms) <= 5,
                "{speed}x source projection did not skip the cut"
            );
        }
    }

    #[test]
    fn compiled_seek_canonicalizes_every_cut_seam_to_audible_source() {
        let (mut engine, _) = test_engine();
        engine.state = edited_playback_state(
            10_000,
            vec![range(0, 1_000), range(3_000, 6_000), range(9_000, 10_000)],
            None,
        );
        engine.current_needs_load = true;

        engine.seek_transport(0).expect("seek at starting seam");
        assert_eq!(engine.state.position_ms, 1_000);
        engine.seek_transport(2_000).expect("seek at middle seam");
        assert_eq!(engine.state.position_ms, 6_000);
        engine
            .seek_transport(u32::MAX)
            .expect("seek clamps at compiled EOF");
        assert_eq!(engine.state.position_ms, 10_000);
        assert!(
            !engine.state.queue[0]
                .effective_edit
                .as_ref()
                .unwrap()
                .cuts
                .iter()
                .any(|cut| {
                    cut.start_ms <= engine.state.position_ms
                        && engine.state.position_ms < cut.end_ms
                })
        );
    }

    #[test]
    fn preview_and_internal_loop_seek_keep_source_coordinates() {
        let (mut preview_engine, _) = test_engine();
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 100_000,
            ..TrackRef::default()
        };
        let error = preview_engine
            .preview_track_edit(track, vec![range(10_000, 60_000)], None, 40_000, 1)
            .expect_err("the test engine has no player");
        assert!(error.contains("player"));
        assert_eq!(
            preview_engine.state.position_ms, 40_000,
            "editor preview position remains in the source timeline"
        );

        let (mut loop_engine, _) = test_engine();
        loop_engine.state = edited_playback_state(
            100_000,
            vec![range(10_000, 60_000)],
            Some(loop_range(70_000, 80_000, 3)),
        );
        loop_engine.state.playing = true;
        loop_engine.current_needs_load = true;
        assert!(loop_engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 70_000,
            revision: loop_engine.audio_revision,
        }));
        assert!(loop_engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 70_000,
            revision: loop_engine.audio_revision,
        }));
        assert_eq!(loop_engine.loop_pass, 3);
        assert_eq!(loop_engine.state.position_ms, 70_000);
        assert_eq!(
            loop_engine.transport_position_and_duration(),
            (20_000, 50_000),
            "the visible playhead wraps to the mapped loop start"
        );
    }

    #[test]
    fn restore_maps_the_compiled_snapshot_after_resolving_live_edits() {
        let root = std::env::temp_dir().join(format!(
            "renderer-compiled-restore-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        let (mut engine, _) = test_engine_in(root.clone());
        engine
            .track_edits
            .save_definition(
                "0123456789ABCDEFGHIJKL".to_owned(),
                100_000,
                vec![range(10_000, 60_000)],
                None,
            )
            .unwrap();
        engine
            .track_edits
            .set_enabled("playlist", "0123456789ABCDEFGHIJKL", true)
            .unwrap();
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 100_000,
            context: "playlist:playlist".to_owned(),
            ..TrackRef::default()
        };

        engine
            .restore_queue(vec![track], 0, 10_000, String::new(), 0, false, false, false)
            .expect("restore");
        assert_eq!(
            engine.state.position_ms, 60_000,
            "the exact compiled seam restores after the cut"
        );
        assert!(!engine.state.playing);
        assert!(engine.current_needs_load);
        assert_eq!(engine.transport_position_and_duration(), (10_000, 50_000));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn previous_restart_uses_the_compiled_beginning() {
        let (mut engine, _) = test_engine();
        engine.state = edited_playback_state(10_000, vec![range(0, 1_000)], None);
        engine.state.position_ms = 5_000;
        engine.current_needs_load = true;

        engine.previous().expect("restart");
        assert_eq!(engine.state.position_ms, 1_000);
        assert_eq!(engine.transport_position_and_duration().0, 0);
    }

    #[test]
    fn stale_generation_events_do_not_emit() {
        let mut engine = playing_engine();
        let uri = track_uri();
        engine.generation = 9;
        assert!(!engine.on_player_signal(PlayerSignal::Event {
            generation: 8,
            event: PlayerEvent::Playing {
                play_request_id: 7,
                track_id: uri,
                position_ms: 5_000,
            },
        }));
    }

    /// librespot answers every command, including the ones whose result the
    /// engine already reported. An echo at the playhead the engine holds is
    /// not a state change; a playhead that really moved is.
    #[test]
    fn a_player_echo_is_not_a_state_change() {
        let (mut engine, _buffer) = test_engine();
        engine.state = two_track_state();
        engine.play_request_id = Some(7);
        let uri = track_uri();
        let position = engine.state.position_ms;

        engine.state.playing = true;
        assert!(!engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: position,
        }));
        assert!(!engine.on_player_event(PlayerEvent::Seeked {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: position,
        }));
        assert!(!engine.on_player_event(PlayerEvent::PositionChanged {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: position,
        }));
        // Pause is the engine's own command; librespot's report of it, or of
        // a pause it took on its own, does not move the intent.
        assert!(!engine.on_player_event(PlayerEvent::Paused {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: position,
        }));
        assert!(engine.state.playing);

        assert!(engine.on_player_event(PlayerEvent::Seeked {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: position + 500,
        }));
        assert!(engine.on_player_event(PlayerEvent::PositionChanged {
            play_request_id: 7,
            track_id: uri,
            position_ms: position + 1_000,
        }));
        assert_eq!(engine.state.position_ms, position + 1_000);
    }

    #[test]
    fn previous_within_restart_window_restarts_the_current_track() {
        // At the first track with no history, previous restarts in place:
        // the engine reports no switch target (None) and never panics.
        let mut engine = playing_engine();
        engine.state.position_ms = 0;
        assert_eq!(engine.previous_index(), None);

        // Past the restart window the current track restarts too.
        engine.state.position_ms = 5_000;
        assert_eq!(engine.previous_index(), None);

        // A track near its start but not first in the queue goes backwards.
        engine.state.position_ms = 500;
        engine.state.current_index = Some(1);
        assert_eq!(engine.previous_index(), Some(0));
    }

    #[test]
    fn previous_pops_history_with_bounds_guard() {
        let mut engine = playing_engine();
        engine.state.current_index = Some(1);
        engine.history.push(0);
        assert_eq!(engine.previous_index(), Some(0));

        // Stale history entries (indices that no longer index the queue) are
        // dropped instead of panicking; the fallback still applies.
        engine.history.clear();
        engine.history.push(99);
        assert_eq!(
            engine.previous_index(),
            Some(0),
            "falls back to current - 1"
        );
        engine.state.current_index = Some(0);
        engine.history.push(99);
        assert_eq!(
            engine.previous_index(),
            None,
            "first track: restart in place"
        );
    }

    #[test]
    fn previous_without_a_player_errors_gracefully() {
        let mut engine = playing_engine();
        engine.state.position_ms = 0;
        // No previous target -> transport seek to zero -> no player attached:
        // fail with an error, never panic or unwrap an empty queue.
        assert!(engine.previous().is_err());
        engine.state.current_index = None;
        assert!(engine.previous().is_err());
    }

    #[test]
    fn empty_queue_transport_commands_error_without_panicking() {
        let mut engine = playing_engine();
        engine.state.queue.clear();
        engine.state.current_index = Some(0); // inconsistent state a bug could create
        assert!(engine.previous().is_err());
        assert!(engine.advance(false).is_err());
        assert!(engine.load_current(true).is_err());
        assert!(
            !engine.on_player_signal(PlayerSignal::Event {
                generation: engine.generation,
                event: PlayerEvent::Playing {
                    play_request_id: engine.play_request_id.unwrap_or(7),
                    track_id: track_uri(),
                    position_ms: 0,
                },
            }),
            "events for a missing queue index are ignored, not panicked"
        );
        engine.state.current_index = None;
        assert!(engine.previous().is_err());
        assert!(engine.advance(false).is_err());
        assert!(engine.load_current(true).is_err());
    }

    #[test]
    fn sequential_queue_stops_or_wraps_at_the_end() {
        assert_eq!(sequential_next_index(0, 3, RepeatMode::Off), Some(1));
        assert_eq!(sequential_next_index(2, 3, RepeatMode::Off), None);
        assert_eq!(sequential_next_index(2, 3, RepeatMode::Context), Some(0));
        assert_eq!(sequential_next_index(0, 0, RepeatMode::Context), None);
    }

    #[test]
    fn track_change_pacing_waits_out_the_minimum_interval() {
        let start = Instant::now();
        let interval = TRACK_CHANGE_MIN_INTERVAL;
        // No prior change: proceed immediately.
        assert_eq!(track_change_wait(None, start, interval), Duration::ZERO);
        // Change made right now: the full interval applies.
        assert_eq!(track_change_wait(Some(start), start, interval), interval);
        // One millisecond before the interval elapses: one millisecond left.
        assert_eq!(
            track_change_wait(
                Some(start),
                start + interval - Duration::from_millis(1),
                interval
            ),
            Duration::from_millis(1)
        );
        // Interval elapsed exactly, or beyond: no wait.
        assert_eq!(
            track_change_wait(Some(start), start + interval, interval),
            Duration::ZERO
        );
        assert_eq!(
            track_change_wait(
                Some(start),
                start + interval + Duration::from_millis(50),
                interval
            ),
            Duration::ZERO
        );
    }

    #[test]
    fn moving_queue_items_preserves_the_current_track() {
        assert_eq!(remap_current_index_after_move(2, 2, 0), 0);
        assert_eq!(remap_current_index_after_move(2, 0, 3), 1);
        assert_eq!(remap_current_index_after_move(2, 4, 1), 3);
        assert_eq!(remap_current_index_after_move(2, 0, 1), 2);
        assert_eq!(remap_current_index_after_move(2, 3, 4), 2);
    }

    /// A scratch state directory with a `credentials.json` already written
    /// (any content: logout only removes the file). Removed on drop.
    struct CredentialsFixture {
        directory: PathBuf,
    }

    impl CredentialsFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "sr_engine_logout_test_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            let credentials_dir = directory.join("credentials");
            std::fs::create_dir_all(&credentials_dir).expect("fixture credentials dir");
            std::fs::write(
                credentials_dir.join("credentials.json"),
                "{\"username\":\"test-user\"}",
            )
            .expect("fixture credentials file");
            Self { directory }
        }

        fn credentials_file(&self) -> PathBuf {
            self.directory.join("credentials").join("credentials.json")
        }

        fn credentials_exist(&self) -> bool {
            self.credentials_file().exists()
        }
    }

    impl Drop for CredentialsFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn logout_clears_cached_credentials_and_emits_needs_login_with_auth_url() {
        let fixture = CredentialsFixture::new();
        let (mut engine, buffer) = {
            let (writer, buffer) = ProtocolWriter::capture();
            let cache = librespot_core::cache::Cache::new(
                Some(fixture.directory.join("credentials")),
                Some(fixture.directory.join("volume")),
                None::<PathBuf>,
                None,
            )
            .expect("cache with credentials path");
            (
                Engine::new(
                    writer,
                    cache,
                    PathBuf::new(),
                    fixture.credentials_file(),
                    fixture.directory.clone(),
                    false,
                    device_present(),
                    device_always_present(),
                ),
                buffer,
            )
        };
        // A live session: playback state that logout must tear down.
        engine.state = playback_state(240_000);
        engine.state.playing = true;

        assert!(engine.logout().expect("logout succeeds"));
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
        assert!(!engine.state.ready);
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.state.queue.is_empty());
        let published_url = engine.state.auth_url.clone().expect("auth url published");
        assert!(published_url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(
            !fixture.credentials_exist(),
            "credentials file must be removed"
        );

        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert_eq!(value["auth_state"], "needs_login");
        assert_eq!(value["ready"], false);
        assert_eq!(value["auth_url"], published_url);
    }

    #[test]
    fn logout_is_idempotent_when_credentials_are_already_cleared() {
        let fixture = CredentialsFixture::new();
        let (writer, _) = ProtocolWriter::capture();
        let cache = librespot_core::cache::Cache::new(
            Some(fixture.directory.join("credentials")),
            None::<PathBuf>,
            None::<PathBuf>,
            None,
        )
        .expect("cache");
        let mut engine = Engine::new(
            writer,
            cache,
            PathBuf::new(),
            fixture.credentials_file(),
            fixture.directory.clone(),
            false,
            device_present(),
            device_always_present(),
        );
        assert!(engine.logout().expect("first logout"));
        assert!(!fixture.credentials_exist());
        assert!(engine.logout().expect("second logout is a no-op"));
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
    }

    #[test]
    fn startup_without_cached_credentials_enters_needs_login_without_a_flow() {
        let (mut engine, buffer) = test_engine(); // cache has no credentials
        // Even if restored state carries stale play intent, normal startup is
        // deliberately cold rather than using reconnect preservation.
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        engine.start_authentication(sender);
        assert!(!engine.auth_running, "no implicit flow may start");
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
        assert!(engine.state.auth_url.is_some());
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.state.queue.is_empty());

        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert_eq!(value["auth_state"], "needs_login");
        assert!(
            value["auth_url"]
                .as_str()
                .is_some_and(|url| { url.starts_with("https://accounts.spotify.com/authorize?") })
        );
    }

    #[test]
    fn login_is_a_noop_while_a_session_is_live() {
        let mut engine = playing_engine(); // ready with a player-less state
        engine.state.auth_state = renderer_engine::protocol::AuthState::Ready;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            engine
                .login(&sender)
                .expect("login no-ops when authenticated")
        );
        assert!(!engine.auth_running);
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::Ready);
    }

    #[test]
    fn login_is_a_noop_while_a_flow_is_already_running() {
        let mut engine = test_engine().0;
        engine.auth_running = true;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        engine.state.auth_url = Some("https://accounts.spotify.com/authorize?running".to_owned());
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            engine
                .login(&sender)
                .expect("login no-ops while authenticating")
        );
        assert!(engine.auth_running);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some("https://accounts.spotify.com/authorize?running"),
            "an in-flight attempt keeps its URL"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalisation_change_during_auth_updates_shared_preference_without_reconnect() {
        let mut engine = test_engine().0;
        assert!(!engine.normalisation.load(Ordering::Acquire));
        let stale_session =
            librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        stale_session.shutdown();
        engine.session = Some(stale_session);
        engine.state.ready = false;
        engine.auth_running = true;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        let generation = engine.generation;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        assert!(engine.set_normalisation(true, &sender));
        assert!(engine.normalisation.load(Ordering::Acquire));
        assert!(
            engine.auth_running,
            "the existing authentication keeps running"
        );
        assert_eq!(
            engine.generation, generation,
            "changing the pending preference must not stale authentication"
        );
        assert!(
            receiver.try_recv().is_err(),
            "an unavailable session must not start a competing player rebuild"
        );
        assert!(
            !engine.tick_session_health(&sender),
            "the health tick has no normalisation rebuild machinery"
        );
    }

    #[test]
    fn normalisation_without_a_session_waits_for_the_next_login() {
        let mut engine = test_engine().0;
        engine.enter_needs_login();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.set_normalisation(true, &sender));
        assert!(engine.normalisation.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::NeedsLogin,
            "the login flow itself must not be disturbed"
        );
    }

    #[test]
    fn begin_login_flow_consumes_the_published_attempt_and_marks_authenticating() {
        let _serialized = crate::auth::lock_oauth_port();
        let mut engine = test_engine().0;
        engine.enter_needs_login();
        let published = engine
            .state
            .auth_url
            .clone()
            .expect("needs_login publishes a url");
        let (pending, _listener) = engine.begin_login_flow().expect("flow begins");
        assert_eq!(
            pending.auth_url, published,
            "the flow must use the published URL"
        );
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::Authenticating);
        assert!(engine.auth_running);
        assert_eq!(engine.state.auth_url.as_deref(), Some(published.as_str()));
        assert!(engine.pending_auth.is_none());
    }

    #[test]
    fn begin_login_flow_prepares_a_fresh_attempt_when_none_is_pending() {
        let _serialized = crate::auth::lock_oauth_port();
        let mut engine = test_engine().0;
        engine.state.auth_state = renderer_engine::protocol::AuthState::NeedsLogin;
        let (pending, _listener) = engine.begin_login_flow().expect("flow begins");
        assert!(
            pending
                .auth_url
                .starts_with("https://accounts.spotify.com/authorize?")
        );
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::Authenticating);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some(pending.auth_url.as_str())
        );
    }

    /// The bug this whole path exists to prevent: the browser used to be
    /// opened without the command ever running, so Spotify redirected to a
    /// port nothing held. The fix is an ordering promise — by the time the
    /// command path returns, the callback socket is already accepting — and
    /// this is that promise measured from outside the engine.
    #[test]
    fn the_command_path_holds_the_callback_port_before_it_returns() {
        let _serialized = crate::auth::lock_oauth_port();
        let mut engine = test_engine().0;
        engine.enter_needs_login();

        let (_pending, listener) = engine.begin_login_flow().expect("flow begins");

        assert_eq!(listener.address().to_string(), "127.0.0.1:5588");
        assert!(
            std::net::TcpListener::bind("127.0.0.1:5588").is_err(),
            "the engine must already hold the port when it answers"
        );
        std::net::TcpStream::connect("127.0.0.1:5588")
            .expect("a redirect arriving now would be accepted, not refused");
    }

    /// A port this application cannot have is a dead end, because the redirect
    /// URI is registered against librespot's client id and cannot be moved.
    /// The click must fail with that news and leave the engine exactly where
    /// it was, ready for a clean retry — not stranded in `Authenticating`
    /// waiting for a callback that can never arrive.
    #[test]
    fn a_taken_callback_port_refuses_the_login_without_disturbing_the_state() {
        let _serialized = crate::auth::lock_oauth_port();
        let squatter =
            std::net::TcpListener::bind("127.0.0.1:5588").expect("the test takes the port first");
        let mut engine = test_engine().0;
        engine.enter_needs_login();
        let published = engine.state.auth_url.clone().expect("a url is published");
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        let error = engine.login(&sender).expect_err("the port is unavailable");
        assert!(error.contains("5588"), "the port must be named: {error}");
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::NeedsLogin,
            "a login that cannot start must not claim to be authenticating"
        );
        assert!(!engine.auth_running);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some(published.as_str()),
            "the prepared attempt survives for the retry"
        );
        assert!(engine.pending_auth.is_some());
        drop(squatter);
    }

    #[test]
    fn failed_auth_signal_returns_to_needs_login_with_a_fresh_url() {
        let mut engine = test_engine().0;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        engine.state.auth_url = Some("https://accounts.spotify.com/authorize?first".to_owned());
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Err(AuthFailure::Rejected(
                    "Spotify authentication failed: test".to_owned(),
                )),
            },
            sender,
        ));
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
        assert!(!engine.state.ready);
        assert!(!engine.auth_running);
        let retry_url = engine.state.auth_url.clone().expect("retry url published");
        assert_ne!(
            retry_url, "https://accounts.spotify.com/authorize?first",
            "a retry must regenerate the URL"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|error| { error.contains("Spotify authentication failed") })
        );
    }

    /// Waking from sleep reaches the first connect before Windows has a
    /// resolver, and librespot reports that as a plain `Unknown` error around
    /// `no such host is known`. Answering it with a login prompt threw away a
    /// session that Spotify had never refused — the credentials were still
    /// good, which is why restarting the app signed the user straight back in.
    #[test]
    fn an_unreachable_spotify_keeps_the_session_and_arms_a_retry() {
        let mut engine = test_engine().0;
        engine.state = playback_state(180_000);
        engine.state.ready = false;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();

        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Err(AuthFailure::Unreachable(
                    "could not reach Spotify: no such host is known".to_owned(),
                )),
            },
            sender,
        ));

        assert!(
            engine.state.auth_state == renderer_engine::protocol::AuthState::Authenticating,
            "a network failure must not present as a logged-out session"
        );
        assert!(engine.state.auth_url.is_none(), "no login is being asked for");
        assert!(!engine.state.ready);
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "the queue survives an outage"
        );
        assert!(
            engine.next_reconnect.is_some(),
            "another attempt must be armed, or nothing would retry"
        );
        assert!(engine.reconnect_backoff > RECONNECT_BACKOFF_MIN);
    }

    /// A generation bump from the audio path while a reconnect is in flight
    /// must not leave the engine permanently mid-auth. The attempt's answer
    /// arrives stale and is discarded — but the flag it set gates every later
    /// start: the health tick returns at its first line, the cached attempt and
    /// `login` both no-op (`login` answering `Ok` without binding the callback
    /// port), and the device probe is refused because `state.ready` never comes
    /// back. The app sat on "reconnecting" until the process was restarted.
    #[tokio::test(flavor = "current_thread")]
    async fn an_audio_failure_during_a_reconnect_cannot_wedge_authentication() {
        let (mut engine, _probe) = engine_with_player();
        engine.state.playing = true;

        // The session librespot invalidated, and the reconnect attempt the
        // health tick left in flight.
        let stale = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        stale.shutdown();
        engine.session = Some(stale);
        engine.state.ready = false;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        engine.auth_running = true;
        engine.next_reconnect = Some(Instant::now() - Duration::from_millis(1));
        let attempt = engine.generation;
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();

        // The device dies while the round trip is out.
        assert!(engine.on_audio_signal(AudioSignal::OutputStalled {
            revision: engine.audio_revision,
        }));
        assert_ne!(engine.generation, attempt, "the failure stales the attempt");
        assert!(engine.audio_unavailable.is_some(), "and arms the device probe");

        // The answer arrives for an attempt that no longer exists.
        assert!(
            !engine.on_auth_signal(
                AuthSignal::Complete {
                    generation: attempt,
                    result: Err(AuthFailure::Unreachable("no such host".to_owned())),
                },
                player_sender,
            ),
            "a stale answer changes nothing"
        );
        assert!(
            !engine.auth_running,
            "and must hand the engine back its retry instead of squatting on the flag"
        );

        // The retry is reachable again: the next due tick runs the start path
        // instead of returning at the flag. This cache holds no credentials, so
        // the start path lands on the login prompt — the state the user can act
        // on; an engine with cached credentials reconnects from here.
        let generation = engine.generation;
        assert!(
            engine.tick_session_health(&auth_sender),
            "the due retry runs the start path again"
        );
        assert_ne!(engine.generation, generation, "and it is a real attempt");
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::NeedsLogin,
            "the engine is out of 'reconnecting'"
        );
        assert!(
            engine.state.auth_url.is_some(),
            "with a URL the Log in button can use"
        );
    }

    /// A stale answer may only clear its *own* attempt. An explicit logout
    /// clears the flag and bumps the generation while a cached attempt is still
    /// in flight, and the Log in click that follows starts the OAuth flow under
    /// a newer generation. When the old answer finally lands it belongs to
    /// neither, and clearing the live flow's flag would leave the next click
    /// failing on the callback port the flow still holds.
    #[test]
    fn a_stale_answer_cannot_abandon_a_live_login_flow() {
        let _serialized = crate::auth::lock_oauth_port();
        let mut engine = test_engine().0;
        engine.state = playback_state(240_000);
        engine.auth_running = true;
        engine.auth_generation = engine.generation;
        let attempt = engine.generation;

        assert!(engine.logout().expect("logout succeeds"));
        assert!(
            !engine.auth_running,
            "the explicit logout ends the old attempt"
        );

        // The click's own bookkeeping. `login` adds only the spawn of the flow,
        // which a test must not start: it waits on the loopback callback for
        // twenty minutes.
        let (_pending, _listener) = engine.begin_login_flow().expect("the flow starts");
        assert!(engine.auth_running, "the flow owns the flag now");
        let flow_generation = engine.auth_generation;
        assert_ne!(flow_generation, attempt, "and answers under a newer one");

        // The abandoned attempt's answer arrives at last.
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            !engine.on_auth_signal(
                AuthSignal::Complete {
                    generation: attempt,
                    result: Err(AuthFailure::Unreachable("no such host".to_owned())),
                },
                player_sender,
            ),
            "a stale answer changes nothing"
        );

        assert!(
            engine.auth_running,
            "the live flow keeps its flag, so its next click is not refused a port it holds"
        );
        assert_eq!(
            engine.auth_generation, flow_generation,
            "the engine is still waiting on the attempt that is actually in flight"
        );
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::Authenticating,
            "and the flow's state was not disturbed"
        );
    }

    /// A rebuild that failed outright must not leave the old player running.
    /// The preference it was rebuilding for is already swapped into the shared
    /// atomic every later rebuild reads, so keeping the incumbent would play
    /// the old configuration while the engine reports the new one as not
    /// applied — and the message saying so was released by the next command
    /// (`clear_error`) as soon as nothing was in force.
    #[tokio::test(flavor = "current_thread")]
    async fn a_fatal_rebuild_failure_does_not_leave_the_old_player_running() {
        let (mut engine, _probe) = engine_with_player();
        engine.state.playing = true;
        engine.normalisation.store(true, Ordering::Release);
        let generation = engine.generation;

        assert!(engine.on_auth_signal(
            AuthSignal::PlayerRebuilt {
                generation,
                normalisation: true,
                result: Err(PlaybackError::Fatal(
                    "could not initialize software volume: test".to_owned(),
                )),
            },
            tokio::sync::mpsc::unbounded_channel().0,
        ));

        assert!(
            engine.player.is_none(),
            "a configuration reported as not applied must not keep playing"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|message| message.contains("could not rebuild the audio player")),
            "and the reason is the user's to read: {:?}",
            engine.state.error
        );
        assert!(
            engine.audio_unavailable.is_some(),
            "it lives in the state `clear_error` respects, and the probe re-arms"
        );

        // A command that succeeds while the failure is in force does not make
        // it go away: nothing it did brought the player back.
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        engine
            .process_command(Command::SetVolume { percent: 30 }, &auth_sender)
            .await
            .expect("volume applies");
        assert!(
            engine.state.error.is_some(),
            "the message survives an unrelated command"
        );
    }

    /// The retry above leaves no session behind, so the health tick cannot look
    /// for a dead one; it has to notice the armed retry instead.
    #[test]
    fn the_health_tick_retries_when_only_a_retry_is_armed() {
        let mut engine = test_engine().0;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        engine.session = None;
        engine.next_reconnect = Some(Instant::now() - Duration::from_millis(1));

        engine.awaiting_transport_retry = true;
        let generation = engine.generation;
        assert!(
            engine.tick_session_health(&sender),
            "a due retry with no session must still fire"
        );
        assert_ne!(engine.generation, generation, "the attempt actually started");
        assert!(engine.reconnect_backoff > RECONNECT_BACKOFF_MIN);
    }

    #[test]
    fn state_events_omit_auth_url_when_no_attempt_is_pending() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.auth_url = None;
        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert!(value.get("auth_url").is_none(), "auth_url must be omitted");
        assert_eq!(value["auth_state"], "ready");
    }

    /// Awaits one signal from the engine, so a task that never answers fails
    /// the test instead of hanging it.
    async fn receive_auth_signal(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<AuthSignal>,
    ) -> AuthSignal {
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("the engine answers within the timeout")
            .expect("the engine holds the sender")
    }

    /// Waits for the load the engine issued to reach librespot. librespot
    /// announces every load with `PlayRequestIdChanged` before it fetches
    /// anything, so this is the only evidence of a real load that exists
    /// outside the engine — and it is what "the row is playing again" has to
    /// mean when the load itself cannot succeed without Spotify.
    async fn receive_load(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<PlayerSignal>) {
        let loaded = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(signal) = receiver.recv().await {
                if matches!(
                    signal,
                    PlayerSignal::Event {
                        event: PlayerEvent::PlayRequestIdChanged { .. },
                        ..
                    }
                ) {
                    return true;
                }
            }
            false
        })
        .await;
        assert!(
            matches!(loaded, Ok(true)),
            "the engine's load must reach librespot"
        );
    }

    /// Puts the engine in the state a machine with no output device boots into,
    /// through the real player-construction path: the device step fails, the
    /// session survives it, and the engine is left holding a message instead of
    /// a player.
    async fn engine_without_output_device(
        engine: &mut Engine,
        device: &Arc<TestAudioDevice>,
    ) -> librespot_core::Session {
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        let cache = librespot_core::cache::Cache::new(
            None::<PathBuf>,
            None::<PathBuf>,
            None::<PathBuf>,
            None,
        )
        .expect("cache");
        let message = match create_playback(session.clone(), cache, false, device.opener()).await {
            Err(PlaybackError::NoOutputDevice(message)) => message,
            Err(error) => panic!("a missing device must be a device failure, got {error}"),
            Ok(_) => panic!("a machine with no output device must not produce a player"),
        };
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Ok(ConnectedSession {
                    session: session.clone(),
                    playback: Err(message),
                }),
            },
            player_sender,
        ));
        session
    }

    /// Booting with no output device must not cost the session, the browsing or
    /// the engine itself. Before this, the device step panicked on librespot's
    /// player thread, playback construction failed, the session was shut down
    /// with it, and the engine answered every browse command with a login
    /// prompt — for a machine whose only problem was that its dongle had not
    /// been recognised yet.
    #[tokio::test(flavor = "current_thread")]
    async fn no_output_device_leaves_a_live_session_browsing_and_says_why() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener(), device.presence());
        let session = engine_without_output_device(&mut engine, &device).await;

        assert_eq!(device.opened(), 0, "nothing was opened on a machine with no device");
        assert!(
            !session.is_invalid(),
            "the output device is not the session's business"
        );
        assert!(engine.state.ready, "the engine is up");
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::Ready,
            "a missing device is not an authentication failure"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|message| message.contains("no audio output device")),
            "the user is told the cause, not a WASAPI stack trace: {:?}",
            engine.state.error
        );
        assert!(engine.player.is_none(), "no player is installed without a device");
        assert!(engine.session.is_some(), "the session is installed");
        assert!(
            engine.browse_session_clone().is_ok(),
            "browse commands have their session"
        );
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            matches!(
                engine.process_command(Command::Status, &auth_sender).await,
                Ok(true)
            ),
            "status still answers"
        );
        let refused = engine
            .player()
            .err()
            .expect("there is no player to reach");
        assert!(
            refused.contains("no audio output device"),
            "a command that needs a player names the device: {refused}"
        );
    }

    /// The other direction, and the owner's expectation: the device appears, so
    /// the engine builds the player it could not build before, forgets the
    /// message, and plays what was asked for — from where the sound stopped.
    #[tokio::test(flavor = "current_thread")]
    async fn a_device_that_appears_rebuilds_the_player_and_resumes_the_queue() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener(), device.presence());
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.update_position(42_000);
        let session = engine_without_output_device(&mut engine, &device).await;
        assert!(engine.player.is_none());

        device.plug_in();
        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, mut player_receiver) = tokio::sync::mpsc::unbounded_channel();
        // The probe waits its backoff before it runs; bring the due time
        // forward the way its deadline arriving would.
        engine
            .audio_unavailable
            .as_mut()
            .expect("no device").retry_at = Some(Instant::now() - Duration::from_millis(1));
        let constructions = device.asked();
        assert!(
            !engine.tick_audio_device(&auth_sender),
            "starting a probe is not itself a state change"
        );
        let signal = receive_auth_signal(&mut auth_receiver).await;
        assert!(engine.on_auth_signal(signal, player_sender));

        assert_eq!(
            device.asked(),
            constructions + 1,
            "a device that appears is constructed once, not once per probe"
        );
        assert_eq!(device.opened(), 1, "the probe opened the device");
        assert!(engine.player.is_some(), "the player is back");
        assert!(engine.audio_unavailable.is_none());
        assert!(
            engine.state.error.is_none(),
            "the message goes with the condition that produced it"
        );
        assert!(engine.audio_probe_backoff > AUDIO_PROBE_BACKOFF_MIN,
            "lazy construction is not proof the device answers");
        engine.on_audio_signal(AudioSignal::OutputReady { revision: engine.audio_revision });
        assert_eq!(engine.audio_probe_backoff, AUDIO_PROBE_BACKOFF_MIN);
        assert!(engine.state.playing, "the requested playback is resumed");
        assert!(
            !engine.current_needs_load,
            "the row is loaded again, not merely marked as needing it"
        );
        assert_eq!(
            engine.state.position_ms, 42_000,
            "it resumes where the sound stopped"
        );
        assert!(!session.is_invalid(), "on the session that never went away");
        receive_load(&mut player_receiver).await;
    }

    /// Switching the system default is different from unplugging a device:
    /// the old endpoint can keep draining forever. A notification must stop
    /// that player, reopen the new default and resume the same track, while a
    /// second switch makes an earlier device open's answer obsolete.
    #[tokio::test(flavor = "current_thread")]
    async fn default_output_switch_reopens_and_resumes_the_current_track() {
        let device = TestAudioDevice::present();
        let (mut engine, _) =
            test_engine_with_audio(PathBuf::new(), device.opener(), device.presence());
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        let old_player = Arc::downgrade(&player);
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.player = Some(player);
        engine.session = Some(session.clone());
        engine.play_request_id = Some(7);
        engine.update_position(42_000);
        note_audio(&mut engine);
        let old_generation = engine.generation;

        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, mut player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_default_output_changed());
        assert!(old_player.upgrade().is_none() && !probe.is_alive());
        assert!(engine.state.playing, "the user's play intent survives the switch");
        assert!(engine.state.position_ms >= 42_000);
        assert!(!session.is_invalid(), "a device change does not reconnect Spotify");
        assert_ne!(engine.generation, old_generation);
        assert!(!engine.tick_audio_device(&auth_sender));
        let old_answer = receive_auth_signal(&mut auth_receiver).await;

        // A new default arrived while the first replacement was opening. Its
        // answer must not install a stream bound to the previous endpoint.
        assert!(engine.on_default_output_changed());
        assert!(!engine.tick_audio_device(&auth_sender));
        let new_answer = receive_auth_signal(&mut auth_receiver).await;
        assert!(!engine.on_auth_signal(old_answer, player_sender.clone()));
        assert!(engine.player.is_none());
        assert!(engine.on_auth_signal(new_answer, player_sender));

        assert_eq!(device.opened(), 2, "one open per notified default change");
        assert!(engine.player.is_some() && engine.audio_unavailable.is_none());
        assert!(engine.state.error.is_none(), "the switch's banner goes with the old player");
        assert!(engine.state.playing);
        assert!(engine.state.position_ms >= 42_000);
        assert!(!engine.current_needs_load, "the same track was reloaded");
        assert!(!session.is_invalid());
        receive_load(&mut player_receiver).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_output_switch_keeps_paused_transport_paused() {
        let device = TestAudioDevice::present();
        let (mut engine, _) =
            test_engine_with_audio(PathBuf::new(), device.opener(), device.presence());
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        engine.state = playback_state(240_000);
        engine.state.playing = false;
        engine.player = Some(player);
        engine.session = Some(session);
        engine.update_position(32_000);
        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();

        assert!(engine.on_default_output_changed());
        let backoff = engine.audio_probe_backoff;
        assert!(!engine.tick_audio_device(&auth_sender));
        let answer = receive_auth_signal(&mut auth_receiver).await;
        assert!(engine.on_auth_signal(answer, player_sender));
        assert!(engine.player.is_some());
        assert!(!engine.state.playing);
        assert_eq!(engine.state.position_ms, 32_000);
        assert!(engine.current_needs_load, "play will load from the paused position");
        assert!(engine.audio_unavailable.is_none());
        assert!(engine.state.error.is_none(), "a paused replacement does not keep the switch's banner");
        assert_eq!(engine.audio_probe_backoff, backoff, "lazy replacement must not reset the ladder");
        assert_eq!(engine.next_work_deadline(), None, "paused lazy output does not spin probes");
        let attempts = device.asked();
        assert!(!engine.tick_audio_device(&auth_sender));
        assert_eq!(device.asked(), attempts);
        engine.seek_transport(45_000).unwrap();
        assert_eq!(engine.state.position_ms, 45_000);
        engine.play().unwrap();
        engine.pause().unwrap();
        assert!(!engine.state.playing);
        assert_eq!(engine.audio_probe_backoff, backoff, "no callback has proven the device yet");
        let revision = engine.audio_revision;
        assert!(engine.on_audio_signal(AudioSignal::OutputFailed { revision, blocked: false }));
        assert!(!engine.state.playing, "a paused native error must not resurrect playback");
        assert!(engine.player.is_none());
        assert!(engine.state.error.is_some(), "the failed open brings the banner back");
        assert!(engine.audio_probe_backoff > backoff, "repeated native failure advances the ladder");
        assert!(!engine.on_audio_signal(AudioSignal::OutputFailed { revision, blocked: false }),
            "retired native errors must not tear down a replacement");
    }

    /// A device that disappears mid-track lands in the same recoverable state,
    /// and does not take the session, the queue or the intent to resume with
    /// it. The sink's report of the stall is the only evidence there is:
    /// librespot turns the write error into a pause and never mentions the
    /// device, and the pause arrives on another channel, in either order.
    #[tokio::test(flavor = "current_thread")]
    async fn a_device_lost_mid_playback_returns_to_the_recoverable_state() {
        let device = TestAudioDevice::present();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener(), device.presence());
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        let player_handle = Arc::downgrade(&player);
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.player = Some(player);
        engine.session = Some(session.clone());
        engine.play_request_id = Some(7);
        engine.update_position(42_000);
        note_audio(&mut engine);
        let generation = engine.generation;

        assert!(engine.on_audio_signal(AudioSignal::OutputStalled {
            revision: engine.audio_revision,
        }));

        assert!(engine.player.is_none(), "the dead player is not left installed");
        assert!(
            player_handle.upgrade().is_none() && !probe.is_alive(),
            "and it is really gone, sink and all"
        );
        assert!(engine.session.is_some(), "the session is not the device's");
        assert!(engine.state.ready);
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::Ready
        );
        assert!(
            engine.state.playing,
            "the user asked for this track and has not asked to stop it"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|message| message.contains("stopped responding")),
            "the stall is reported as what it is: {:?}",
            engine.state.error
        );
        assert!(engine.audio_unavailable.is_some(), "the probe is armed");
        assert!(engine.current_needs_load, "the row will have to be loaded again");
        assert_eq!(engine.state.position_ms, 42_000);
        assert!(
            !engine.tick_position(),
            "nothing is audible, so the playhead does not move"
        );

        // librespot's own reaction to the same failure, and the player thread
        // ending behind it, both belong to the player that was just let go.
        assert!(!engine.on_player_signal(PlayerSignal::Event {
            generation,
            event: PlayerEvent::Paused {
                play_request_id: 7,
                track_id: track_uri(),
                position_ms: 42_000,
            },
        }));
        assert!(!engine.on_player_signal(PlayerSignal::Closed { generation }));
        assert!(
            engine.session.is_some(),
            "a dead player thread must not take the session with it"
        );
        assert!(
            engine.state.playing,
            "and must not erase the intent to resume"
        );

        // Still no device, and the probe does not build one to find that out:
        // the cheap check answers it. The stall's wording stands, because no
        // construction ran to produce another — and nothing is lost, since it
        // already says playback resumes when a device is available.
        device.unplug();
        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, mut player_receiver) = tokio::sync::mpsc::unbounded_channel();
        engine
            .audio_unavailable
            .as_mut()
            .expect("no device").retry_at = Some(Instant::now() - Duration::from_millis(1));
        let constructions = device.asked();
        assert!(!engine.tick_audio_device(&auth_sender));
        assert_eq!(
            device.asked(),
            constructions,
            "a machine with no device builds no player"
        );
        assert_eq!(device.opened(), 0, "and opens nothing");
        assert!(
            auth_receiver.try_recv().is_err(),
            "and starts nothing that could answer later"
        );
        assert!(
            engine
                .audio_unavailable
                .as_ref()
                .expect("still no device")
                .retry_at
                .is_some_and(|deadline| deadline > Instant::now()),
            "the next attempt waits its backoff"
        );
        assert!(engine.state.error.is_some(), "failed probing retains the unresolved output error");

        // The dongle is plugged back in.
        device.plug_in();
        engine
            .audio_unavailable
            .as_mut()
            .expect("no device").retry_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(!engine.tick_audio_device(&auth_sender));
        let signal = receive_auth_signal(&mut auth_receiver).await;
        assert!(engine.on_auth_signal(signal, player_sender));

        assert!(engine.player.is_some(), "the player comes back");
        assert!(engine.audio_unavailable.is_none());
        assert!(engine.state.error.is_none());
        assert!(engine.state.playing, "the track plays again");
        assert_eq!(engine.state.position_ms, 42_000);
        receive_load(&mut player_receiver).await;
    }

    /// The device exemption is from the device check, not from the session's:
    /// an engine that has just logged out must not accept a volume change
    /// because the machine also happens to have no output device. The device
    /// state is forgotten with the session, so the reason it gives is the real
    /// one.
    #[tokio::test(flavor = "current_thread")]
    async fn a_device_failure_never_stands_in_for_a_missing_session() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener(), device.presence());
        let session = engine_without_output_device(&mut engine, &device).await;
        assert!(engine.audio_unavailable.is_some());

        assert!(engine.logout().expect("logout succeeds"));
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::NeedsLogin
        );
        assert!(
            engine.audio_unavailable.is_none(),
            "the device's failure does not outlive the session it was noticed on"
        );
        assert!(session.is_invalid(), "and the session really ends");

        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let refused = engine
            .process_command(Command::SetVolume { percent: 30 }, &auth_sender)
            .await
            .expect_err("a volume change is not what a signed-out engine needs");
        assert!(refused.contains("login"), "{refused}");
        assert_eq!(engine.state.volume, 50, "and the volume did not move");
    }

    /// The retry clock: one probe at a time, each failure waiting longer than
    /// the last, and a ceiling — a machine that has been without audio since
    /// boot must not enumerate devices on every work tick for the rest of the
    /// day, and a device that lands must still be picked up in seconds. A
    /// probe that finds no device must not build a player to confirm it: the
    /// construction is a mixer, a player thread and a failed cpal open, paid
    /// every few seconds for an answer the device list already has.
    #[tokio::test(flavor = "current_thread")]
    async fn device_probes_back_off_to_a_ceiling_without_building_anything() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) =
            test_engine_with_audio(PathBuf::new(), device.opener(), device.presence());
        engine_without_output_device(&mut engine, &device).await;
        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();

        assert!(!engine.tick_audio_device(&auth_sender));
        assert!(
            auth_receiver.try_recv().is_err(),
            "the first probe waits its backoff rather than firing immediately"
        );
        let mut waits = Vec::new();
        for _ in 0..5 {
            let retry_at = engine.audio_unavailable.as_ref().expect("no device").retry_at;
            waits.push(retry_at.expect("fallback deadline").saturating_duration_since(Instant::now()));
            // Bring the due time forward, the way its deadline arriving would.
            engine
                .audio_unavailable
                .as_mut()
                .expect("no device").retry_at = Some(Instant::now() - Duration::from_millis(1));
            let constructions = device.asked();
            assert!(!engine.tick_audio_device(&auth_sender));
            assert_eq!(
                device.asked(),
                constructions,
                "a probe that finds no device constructs nothing"
            );
            assert!(
                auth_receiver.try_recv().is_err(),
                "and starts nothing that would answer later"
            );
            // The probe owns the clock until its next due time, so a tick
            // arriving meanwhile cannot start a second one beside it.
            let held = engine.audio_unavailable.as_ref().expect("no device").retry_at;
            assert!(held.expect("fallback deadline") > Instant::now(), "the probe holds the clock");
            assert!(!engine.tick_audio_device(&auth_sender));
            assert_eq!(
                engine.audio_unavailable.as_ref().expect("no device").retry_at,
                held,
                "and a second tick leaves it exactly where the probe put it"
            );
            assert_eq!(device.opened(), 0, "there is still nothing to open");
        }

        assert!(
            waits
                .windows(2)
                .all(|pair| pair[0] <= pair[1] + Duration::from_millis(1)),
            "each failure waits at least as long as the last: {waits:?}"
        );
        assert!(
            waits.iter().all(|wait| *wait <= AUDIO_PROBE_BACKOFF_MAX),
            "no wait may outlast the ceiling: {waits:?}"
        );
        assert_eq!(
            engine.audio_probe_backoff, AUDIO_PROBE_BACKOFF_MAX,
            "and the backoff stops growing there"
        );
        assert!(
            AUDIO_PROBE_BACKOFF_MIN >= Duration::from_secs(1)
                && AUDIO_PROBE_BACKOFF_MAX <= Duration::from_secs(30),
            "the ceiling is what bounds how long a plugged-in device stays silent"
        );
    }

    /// The driver that never answers is its own condition. The player thread
    /// that ran that open stays parked inside cpal for the rest of the
    /// process, so retrying it on the ordinary ten-second ceiling would leave
    /// another one behind every ten seconds — thousands a day — while the
    /// machine stayed just as silent. The blocked answer must climb to a much
    /// longer ladder, and an ordinary failure afterwards must come back down
    /// to the short one that picks a plugged-in device up in seconds.
    #[tokio::test(flavor = "current_thread")]
    async fn a_blocked_device_open_backs_off_much_harder_than_a_missing_one() {
        let (mut engine, _) = test_engine();
        engine.state.ready = true;
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();

        // What the probe answers when the device is there but its open hung
        // until `AUDIO_START_TIMEOUT`.
        let blocked_answer = |engine: &Engine| AuthSignal::PlayerRebuilt {
            generation: engine.generation,
            normalisation: engine.normalisation.load(Ordering::Acquire),
            result: Err(PlaybackError::DeviceOpenBlocked(
                "no audio output device: opening one did not finish within 10 s".to_owned(),
            )),
        };

        assert!(engine.on_auth_signal(blocked_answer(&engine), player_sender.clone()));
        assert!(
            engine.audio_unavailable.is_some(),
            "a blocked open is still the recoverable device state"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|message| message.contains("did not finish within")),
            "and the user is told what happened: {:?}",
            engine.state.error
        );

        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(
                engine
                    .audio_unavailable
                    .as_ref()
                    .expect("no device")
                    .retry_at
                    .expect("retry deadline armed")
                    .saturating_duration_since(Instant::now()),
            );
            assert!(engine.on_auth_signal(blocked_answer(&engine), player_sender.clone()));
        }
        assert!(
            waits
                .windows(2)
                .all(|pair| pair[0] <= pair[1] + Duration::from_millis(1)),
            "each blocked answer waits at least as long as the last: {waits:?}"
        );
        assert_eq!(
            engine.audio_probe_backoff, AUDIO_BLOCKED_PROBE_BACKOFF_MAX,
            "and the ladder ends at the blocked ceiling"
        );
        assert!(
            AUDIO_BLOCKED_PROBE_BACKOFF_MAX >= AUDIO_PROBE_BACKOFF_MAX * 10,
            "backing off barely harder would leave the parked player threads accumulating"
        );

        // The driver answers again, so this is an ordinary failure: the ladder
        // returns to the ordinary clock that retries a missing device in
        // seconds.
        assert!(engine.on_auth_signal(
            AuthSignal::PlayerRebuilt {
                generation: engine.generation,
                normalisation: engine.normalisation.load(Ordering::Acquire),
                result: Err(PlaybackError::NoOutputDevice(
                    "no audio output device".to_owned(),
                )),
            },
            player_sender,
        ));
        assert_eq!(
            engine.audio_probe_backoff, AUDIO_PROBE_BACKOFF_MAX,
            "an ordinary failure stores the ordinary clock again"
        );
        // And the deadline it actually armed is the ordinary one: clamping
        // only what gets stored would leave this failure — a plainly missing
        // device — waiting out the five minutes the wedged driver earned.
        let wait = engine
            .audio_unavailable
            .as_ref()
            .expect("still no device")
            .retry_at
            .expect("retry deadline armed")
            .saturating_duration_since(Instant::now());
        assert!(
            wait <= AUDIO_PROBE_BACKOFF_MAX,
            "the ordinary failure must not inherit the blocked wait: {wait:?}"
        );
    }
}
