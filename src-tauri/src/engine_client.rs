//! Line-JSON protocol client for the `PlaybackEngine` subprocess.
//!
//! Owns the engine's stdin/stdout pipes, serializes requests with
//! incrementing request ids, routes replies back through tokio oneshots, and
//! fans `state`, `position` and `volume` lines out on a broadcast channel in
//! wire order. A supervisor task respawns the engine with backoff when its
//! pipe closes and re-requests `status` so the session re-syncs after a
//! restart.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use renderer_engine::protocol::MAX_REQUEST_BYTES;
use serde::de::value::{CowStrDeserializer, MapAccessDeserializer};
use serde::de::{DeserializeSeed, IntoDeserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch};

use crate::app::{
    clear_playback_snapshot, engine_state_dir, load_app_settings, load_playback_snapshot,
    save_playback_snapshot, save_playhead_snapshot, PlaybackSnapshot,
};
use crate::log;
use crate::types::{PlaybackState, Track};

/// The client's clocks: how long a request waits for its answer, and how a
/// lost engine is respawned. Carried by each client rather than read from
/// constants so a test can prove the same deadlines and backoff at a fraction
/// of production's seconds; production always runs [`Timing::PRODUCTION`].
#[derive(Clone, Copy, Debug)]
struct Timing {
    /// Replies for playback/session commands arrive promptly.
    command: Duration,
    /// Browse and edit commands run network round-trips inside the engine.
    browse: Duration,
    waveform: Duration,
    respawn: RespawnPolicy,
}

impl Timing {
    const PRODUCTION: Self = Self {
        command: Duration::from_secs(10),
        browse: Duration::from_secs(30),
        waveform: Duration::from_secs(5 * 60),
        respawn: RespawnPolicy {
            start: Duration::from_secs(2),
            max: Duration::from_secs(30),
            // A child that stayed up this long was healthy, so its exit starts
            // the backoff over. A spawn that succeeds proves nothing: a
            // crash-looping engine spawns fine and dies again within seconds,
            // and resetting on the spawn kept it respawning every two seconds
            // for ever. Thirty seconds is past any startup crash
            // (authentication, device open, restore) and equal to the longest
            // wait, so a loop never respawns faster than its last delay.
            stable_uptime: Duration::from_secs(30),
        },
    };

    fn request_timeout(&self, kind: &str) -> Duration {
        if kind == "get_track_waveform" {
            self.waveform
        } else if kind.starts_with("browse_") || kind.starts_with("edit_") {
            self.browse
        } else {
            self.command
        }
    }
}

/// Engine respawn backoff: doubling from `start` to `max`, started over only
/// by a child that stayed up for `stable_uptime`.
#[derive(Clone, Copy, Debug)]
struct RespawnPolicy {
    start: Duration,
    max: Duration,
    stable_uptime: Duration,
}

impl RespawnPolicy {
    /// The wait before respawning a child that exited after `uptime`, given
    /// the `backoff` its predecessor's exit left behind.
    fn delay(&self, backoff: Duration, uptime: Duration) -> Duration {
        if uptime >= self.stable_uptime { self.start } else { backoff }
    }

    fn next(&self, delay: Duration) -> Duration {
        (delay * 2).min(self.max)
    }
}

/// Crash-restore retries use the same shape as the shell's library refresh:
/// at most [`RESTORE_RETRY_ATTEMPTS`] total attempts, with 5s backoff
/// doubling to a 60s cap, after which the durable restore stays disarmed
/// instead of hammering a broken engine forever.
const RESTORE_RETRY_ATTEMPTS: usize = 5;
const RESTORE_RETRY_BASE: Duration = Duration::from_secs(5);
const RESTORE_RETRY_MAX: Duration = Duration::from_secs(60);

/// Delay before the restore retry after the `attempt`-th failure (1-based).
fn restore_retry_delay(attempt: usize) -> Duration {
    let exponent = attempt.saturating_sub(1).min(4) as u32;
    RESTORE_RETRY_BASE
        .saturating_mul(2_u32.pow(exponent))
        .min(RESTORE_RETRY_MAX)
}

/// Playback-state writes are coalesced on one background thread. Successful
/// writes are throttled, while transient failures retry promptly without
/// claiming that the snapshot was committed.
const PERSIST_MIN_INTERVAL: Duration = Duration::from_secs(5);
const PERSIST_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const PERSIST_POSITION_DRIFT_MS: u32 = 15_000;

/// Roll the engine log once past this size, keeping one previous generation.
const ENGINE_LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Spawns the engine with no console at all (Windows only).
///
/// `CREATE_NO_WINDOW` only hides the console window — Windows still allocates
/// a console and a `conhost.exe` to back it. The engine needs neither: stdin
/// and stdout are pipes and stderr is redirected to the log file, so nothing
/// ever touches a console. `DETACHED_PROCESS` skips the allocation outright,
/// dropping a process and any chance of a window flashing on startup.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

const ENGINE_BINARY: &str = if cfg!(windows) {
    "PlaybackEngine.exe"
} else {
    "PlaybackEngine"
};

/// One engine reply (any non-`state`/`position` line). `data` carries the
/// payload of `browse_*`/`edit_*` responses; plain `response` lines leave
/// it `None`.
#[derive(Debug, Default)]
pub struct EngineReply {
    pub ok: bool,
    pub error: Option<String>,
    pub data: Option<ReplyData>,
    /// The download-mark fence as it stood when the reply was routed; the
    /// typed parse applies it to the rows (see [`parse_data`]).
    fence: MarkFence,
}

/// A reply payload exactly as the engine wrote it. Only checked to be JSON
/// on the reader thread, then parsed once, straight into its typed shape, by
/// whoever asked: a ten-thousand-row playlist is never built as a `Value`
/// tree, walked, and parsed a second time.
#[derive(Debug)]
pub struct ReplyData(Box<serde_json::value::RawValue>);

impl ReplyData {
    fn json(&self) -> &str { self.0.get() }
}

impl PartialEq for ReplyData {
    fn eq(&self, other: &Self) -> bool { self.json() == other.json() }
}

/// Download marks a reply may no longer claim. Captured on the reader thread
/// at delivery, the same moment the marks were judged before, and applied to
/// the typed rows.
#[derive(Debug, Default)]
struct MarkFence {
    /// A cache reset landed while the request ran: every claim predates it.
    stale: bool,
    /// Marks the engine withdrew since; no row may claim one of these.
    invalidated: HashSet<String>,
}

impl MarkFence {
    fn apply(&self, payload: &mut impl CachedClaims) {
        if !self.stale && self.invalidated.is_empty() { return; }
        payload.claims(&mut |row| {
            if self.stale || self.invalidated.contains(&row.id) { row.cached = false; }
        });
    }
}

/// The rows of a reply payload that carry a download mark: every
/// [`TrackRef`](renderer_engine::protocol::TrackRef), wherever it sits, is
/// handed to `visit`. Every type [`parse_data`] reads declares this, so a
/// payload that gains rows has to say where they are; one without rows keeps
/// the empty default.
trait CachedClaims {
    fn claims(&mut self, _visit: &mut dyn FnMut(&mut renderer_engine::protocol::TrackRef)) {}
}

struct PendingRequest {
    sender: oneshot::Sender<EngineReply>,
    cache_generation: u64,
    account_generation: u64,
    resets_cache_marks: bool,
}

struct RequestGuard<'a> {
    pending: &'a Mutex<HashMap<String, PendingRequest>>,
    id: &'a str,
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        let mut pending = self.pending.lock();
        // A sent clear still owns a reset even if its caller stops waiting.
        // Its small correlation entry is retired by the ACK or EOF.
        if pending.get(self.id).is_some_and(|request| request.resets_cache_marks) { return; }
        pending.remove(self.id);
    }
}

struct InputLine {
    line: String,
    written: oneshot::Sender<Result<(), String>>,
}

struct EngineInput {
    generation: u64,
    sender: SyncSender<InputLine>,
}

/// The engine's 2-second scalar position heartbeat. Deliberately not a
/// [`PlaybackState`]: a heartbeat carries only the playhead scalars the
/// frontend projects and clamps against, so parsing and forwarding it never
/// touches (or clones) the queue.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
pub struct PositionHeartbeat {
    pub position_ms: u32,
    pub duration_ms: u32,
}

/// One playback or lifecycle event, fanned out to subscribers in wire order.
/// Scalar lines (heartbeats and volume) share the channel with full states so
/// a consumer can never apply an older heartbeat after a newer full state;
/// `Disconnected` marks the gap between an engine EOF and its replacement
/// becoming ready.
#[derive(Clone, Debug)]
pub enum StateLine {
    State(PlaybackState),
    Position(PositionHeartbeat),
    /// The transport volume, and nothing else. Its own lane because a slider
    /// drag writes one of these per command (the frontend paces `set_volume`
    /// at 50 ms), and a full state would re-parse the whole queue to say one
    /// number.
    Volume(u8),
    /// Cache additions still publish when a queue/order delta cannot resolve.
    CachedIds(Vec<String>),
    UncachedIds(Vec<String>),
    CacheMarksReset,
    Disconnected,
}

/// Queue/settings captured either from the durable app snapshot (normal
/// startup, always paused) or from the last fresh state before an unexpected
/// child exit (resume only when that state was playing).
#[derive(Debug, Clone)]
pub struct RestoreSnapshot {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    /// Position in the compiled transport timeline. The engine maps it after
    /// resolving the active edit against the live edit store.
    pub position_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: String,
    pub track_speed: f32,
    pub episode_speed: f32,
    pub resume_playing: bool,
}

impl RestoreSnapshot {
    /// Queue equality for restore completion: identity, order, and
    /// playability. The engine owns several row fields across an install —
    /// `effective_edit` re-resolves against the live edit store, `context`
    /// is refilled, metadata may have drifted server-side since the plan was
    /// captured — so equality stops at the stable identity of each row.
    /// Comparing anything the engine rewrites would hang the restore forever:
    /// every state suppressed as "not yet restored", an empty player bar over
    /// working audio.
    fn rows_match(a: &[Track], b: &[Track]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.id == y.id
                    && x.uri == y.uri
                    && x.duration_ms == y.duration_ms
                    && x.unavailable == y.unavailable
            })
    }

    fn from_playback(state: &PlaybackState, resume_playing: bool) -> Self {
        Self {
            queue: state.queue.to_vec(),
            current_index: state.current_index,
            position_ms: state.position_ms,
            volume: state.volume,
            shuffle: state.shuffle,
            repeat: state.repeat.clone(),
            track_speed: state.track_speed,
            episode_speed: state.episode_speed,
            resume_playing,
        }
        .normalized()
    }

    fn from_durable(snapshot: PlaybackSnapshot) -> Self {
        Self {
            queue: snapshot.queue,
            current_index: snapshot.current_index,
            position_ms: snapshot.position_ms,
            volume: snapshot.volume,
            shuffle: snapshot.shuffle,
            repeat: snapshot.repeat,
            track_speed: snapshot.track_speed,
            episode_speed: snapshot.episode_speed,
            resume_playing: false,
        }
        .normalized()
    }

    fn normalized(mut self) -> Self {
        // Download marks never survive the trip into a plan: they are
        // session-live truth, and this is the one boundary where disk claims
        // become engine instructions.
        for track in &mut self.queue {
            track.cached = false;
        }
        let requested = self.current_index.unwrap_or(0);
        self.current_index = if self.queue.is_empty() {
            None
        } else {
            (requested..self.queue.len())
                .chain(0..requested.min(self.queue.len()))
                .find(|index| !self.queue[*index].unavailable)
        };
        if self.current_index != Some(requested) {
            self.position_ms = 0;
        } else if let Some(index) = self.current_index {
            self.position_ms = self.position_ms.min(self.queue[index].duration_ms);
        }
        self
    }

    pub fn matches(&self, state: &PlaybackState) -> bool {
        // Restore positions are compiled-timeline values. Resolving the live
        // edit store can shorten that timeline after the snapshot was written;
        // RestoreQueue then clamps the request to the new compiled duration.
        // Match the same canonical value or the restore gate would suppress
        // every ready state forever.
        let expected_position = if state.duration_ms == 0 {
            self.position_ms
        } else {
            self.position_ms.min(state.duration_ms)
        };
        let position_matches = if self.resume_playing {
            state.position_ms.abs_diff(expected_position) <= 2_500
        } else {
            state.position_ms == expected_position
        };
        state.auth_state == "ready"
            && Self::rows_match(&state.queue, &self.queue)
            && state.current_index == self.current_index
            && state.volume == self.volume
            && state.shuffle == self.shuffle
            && state.repeat == self.repeat
            && state.track_speed == self.track_speed
            && state.episode_speed == self.episode_speed
            && state.playing == self.resume_playing
            && position_matches
    }

    fn durable(&self) -> PlaybackSnapshot {
        let mut state = PlaybackState::default();
        state.queue = self.queue.as_slice().into();
        state.current_index = self.current_index;
        state.position_ms = self.position_ms;
        state.volume = self.volume;
        state.shuffle = self.shuffle;
        state.repeat = self.repeat.clone();
        state.track_speed = self.track_speed;
        state.episode_speed = self.episode_speed;
        PlaybackSnapshot::from_playback(&state)
    }
}

/// What the writer last committed to disk.
#[derive(Debug)]
struct PersistedSnapshot {
    written_at: Instant,
    snapshot: PlaybackSnapshot,
    /// [`EngineClient::queue_generation`] at the last comparison of the live
    /// queue against `snapshot.queue`. A later pass that reads the same
    /// generation knows no state line has replaced the retained queue since,
    /// and skips the row scan — the one comparison that costs a walk of the
    /// whole queue. Without it the 2-second heartbeat alone would re-walk
    /// those rows thirty times a minute to answer "still the same".
    queue_generation: u64,
}

impl PersistedSnapshot {
    /// Whether anything needs the whole snapshot rewritten: the queue, or a
    /// setting it carries. The playhead is deliberately not here — a moved
    /// playhead owes the sidecar, not 0.9 MB of queue.
    fn differs_structurally_from(&self, next: &PlaybackState, queue_generation: u64) -> bool {
        let previous = &self.snapshot;
        previous.volume != next.volume
            || previous.shuffle != next.shuffle
            || previous.repeat != next.repeat
            || previous.track_speed != next.track_speed
            || previous.episode_speed != next.episode_speed
            || self.queue_differs_from(next, queue_generation)
    }

    /// Row equality against the live queue, skipped when the queue cannot
    /// have been replaced since it was last compared. The generation is
    /// bumped under the same lock that installs a state line, so equal
    /// generations mean equal rows.
    fn queue_differs_from(&self, next: &PlaybackState, queue_generation: u64) -> bool {
        self.queue_generation != queue_generation
            && !normalized_rows_match(&self.snapshot.queue, &next.queue)
    }

    /// The playhead alone: a different current row, or a position that has
    /// drifted far enough to be worth restoring to.
    fn playhead_differs_from(&self, next: &PlaybackState) -> bool {
        let previous = &self.snapshot;
        previous.current_index != next.current_index
            || next.position_ms.abs_diff(previous.position_ms) >= PERSIST_POSITION_DRIFT_MS
    }
}

/// Queue equality under `PlaybackSnapshot::from_playback`'s normalization
/// (it clears the session-live `cached` mark), without cloning the queue: a
/// row whose only difference is that mark counts as equal.
fn normalized_rows_match(a: &[Track], b: &[Track]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x == y || (x.cached != y.cached && same_row_ignoring_cached(x, y)))
}

/// Row equality for the one difference the persisted copy is allowed to
/// carry: the `cached` mark `from_playback` clears from every row.
///
/// Written out field by field rather than by cloning a row to clear the mark
/// in it — the deadline arm compares the live queue against the committed
/// snapshot on every pass, so a `Track` copy per downloaded row (about fifteen
/// allocations each) is exactly the cost this comparison exists to avoid. The
/// destructuring is exhaustive on purpose: a field added to `Track` must be
/// considered here rather than silently escaping the writer's comparison.
fn same_row_ignoring_cached(x: &Track, y: &Track) -> bool {
    let Track {
        id,
        uri,
        name,
        artist_names,
        artist_ids,
        artist_id,
        album_id,
        album_name,
        cover_url,
        duration_ms,
        play_count,
        added_at,
        unavailable,
        unavailable_reason,
        cached: _,
        context,
        effective_edit,
    } = x;
    id == &y.id
        && uri == &y.uri
        && name == &y.name
        && artist_names == &y.artist_names
        && artist_ids == &y.artist_ids
        && artist_id == &y.artist_id
        && album_id == &y.album_id
        && album_name == &y.album_name
        && cover_url == &y.cover_url
        && duration_ms == &y.duration_ms
        && play_count == &y.play_count
        && added_at == &y.added_at
        && unavailable == &y.unavailable
        && unavailable_reason == &y.unavailable_reason
        && context == &y.context
        && effective_edit == &y.effective_edit
}

/// The one write the deadline arm owes, if any.
#[derive(Debug)]
enum PersistWrite {
    /// The queue or a setting differs: replace the durable snapshot.
    Snapshot(PlaybackSnapshot),
    /// Only the playhead moved: the sidecar carries it, and the snapshot's
    /// queue is left untouched rather than rewritten to record two integers.
    Playhead {
        current_index: Option<usize>,
        position_ms: u32,
        /// Identity of the queue in the snapshot this playhead trails. The
        /// sidecar is stamped with it so a later launch can tell which
        /// snapshot it belongs to instead of trusting file timestamps alone.
        queue_identity: u64,
    },
}

/// The deadline arm's verdict: what to write, and the queue generation the
/// comparison was taken against so a pass that found nothing new can record
/// that this queue has already been looked at.
#[derive(Debug)]
struct PersistDecision {
    /// The generation the verdict was taken against, or `None` when this
    /// pass never reached a live queue (a restore in flight, no ready state
    /// yet). Only a queue that was actually compared may be recorded as
    /// compared — recording one that was not would let the next pass skip a
    /// scan it still owes.
    compared: Option<u64>,
    write: Option<PersistWrite>,
}

#[derive(Debug, Default)]
struct PersistenceSchedule {
    committed: Option<PersistedSnapshot>,
    committed_generation: u64,
    pending_generation: Option<u64>,
    retry_at: Option<Instant>,
}

impl PersistenceSchedule {
    fn mark_dirty(&mut self, generation: u64) {
        if generation > self.committed_generation {
            self.pending_generation = Some(
                self.pending_generation
                    .map_or(generation, |pending| pending.max(generation)),
            );
        }
    }

    fn wait_for(&self, now: Instant) -> Option<Duration> {
        self.pending_generation.map(|_| {
            let deadline = self.retry_at.unwrap_or_else(|| {
                self.committed
                    .as_ref()
                    .map_or(now, |committed| committed.written_at + PERSIST_MIN_INTERVAL)
            });
            deadline.saturating_duration_since(now)
        })
    }

    /// Records a pass that found nothing to write. `compared` is the queue
    /// generation the rows were compared against, when that comparison ran:
    /// keeping it lets the next heartbeat-only pass skip the row scan.
    fn record_unchanged(
        &mut self,
        attempted_generation: u64,
        latest_generation: u64,
        compared: Option<u64>,
    ) {
        if let (Some(committed), Some(compared)) = (self.committed.as_mut(), compared) {
            committed.queue_generation = compared;
        }
        self.committed_generation = self.committed_generation.max(attempted_generation);
        self.pending_generation =
            (latest_generation > self.committed_generation).then_some(latest_generation);
        self.retry_at = None;
    }

    fn record_success(
        &mut self,
        write: PersistWrite,
        attempted_generation: u64,
        latest_generation: u64,
        queue_generation: u64,
        written_at: Instant,
    ) {
        match write {
            PersistWrite::Snapshot(snapshot) => {
                self.committed = Some(PersistedSnapshot {
                    written_at,
                    snapshot,
                    queue_generation,
                });
            }
            // The sidecar holds the playhead of the snapshot already on disk.
            // That snapshot was found unchanged (the only way this arm is
            // reached), so its queue stands as written and only its recorded
            // playhead moves forward — otherwise the next pass would compare
            // the live playhead against a playhead that has been persisted.
            PersistWrite::Playhead {
                current_index,
                position_ms,
                // Only the sidecar carries the identity (see `save_playhead_snapshot`):
                // the snapshot already on disk is the one it names.
                queue_identity: _,
            } => {
                if let Some(committed) = self.committed.as_mut() {
                    committed.written_at = written_at;
                    committed.snapshot.current_index = current_index;
                    committed.snapshot.position_ms = position_ms;
                    committed.queue_generation = queue_generation;
                }
            }
        }
        self.committed_generation = attempted_generation;
        self.pending_generation =
            (latest_generation > attempted_generation).then_some(latest_generation);
        self.retry_at = None;
    }

    fn record_failure(&mut self, latest_generation: u64, now: Instant) {
        self.mark_dirty(latest_generation);
        self.retry_at = Some(now + PERSIST_RETRY_INTERVAL);
    }
}

enum PersistCommand {
    Dirty(u64),
    Clear(mpsc::Sender<Result<(), String>>),
    Shutdown {
        generation: u64,
        result_tx: mpsc::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Clone)]
struct RestorePlan {
    snapshot: RestoreSnapshot,
    sent: bool,
    /// Failed restore attempts so far; at [`RESTORE_RETRY_ATTEMPTS`] the
    /// plan is disarmed instead of retried forever.
    attempts: usize,
    /// Preview leases must never be mistaken for startup/crash recovery.
    only_if_preview: bool,
    /// The editor context that currently owns this preview restore. The
    /// snapshot itself remains the first real state captured for the lease.
    preview_lease_id: Option<u64>,
}

fn claim_restore_plan(pending: &mut Option<RestorePlan>) -> Option<RestoreSnapshot> {
    let plan = pending
        .as_mut()
        .filter(|plan| !plan.only_if_preview && !plan.sent)?;
    plan.sent = true;
    Some(plan.snapshot.clone())
}

fn retry_restore_plan(pending: &mut Option<RestorePlan>) -> Option<Duration> {
    let plan = pending.as_mut().filter(|plan| !plan.only_if_preview)?;
    plan.attempts += 1;
    if plan.attempts >= RESTORE_RETRY_ATTEMPTS {
        *pending = None;
        return None;
    }
    plan.sent = false;
    Some(restore_retry_delay(plan.attempts))
}

/// Which independent array lanes were present on the wire. Empty is data;
/// omission is a delta referring to the named revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PayloadRows {
    Sent,
    QueueOnly,
    OrderOnly,
    Omitted,
}

/// What the wire delta base says about one incoming payload.
#[derive(Debug)]
enum DeltaAdoption {
    /// The payload describes its own queue: it carried the rows.
    AsSent,
    /// The payload omitted its rows and the base names the revision they
    /// belong to.
    Fill {
        queue: Arc<[Track]>,
        upcoming: Vec<usize>,
    },
    /// The payload omitted rows for a revision nothing retained.
    Unresolvable { revision: u64 },
}

/// Independent wire bases for queue rows and upcoming order.
///
/// Kept apart from `last_state`: previews and crash-restore intermediates are
/// not durable state, but subsequent deltas still refer to their arrays.
#[derive(Debug, Default)]
struct DeltaBase {
    /// The revision the retained rows belong to, or `None` before a payload
    /// has carried any.
    revision: Option<u64>,
    order_revision: Option<u64>,
    /// The same rows the state that carried them holds; filling a payload
    /// from here copies a pointer.
    queue: Arc<[Track]>,
    upcoming: Vec<usize>,
    /// Positive file marks survive omitted arrays and queue replacement.
    cached_ids: HashSet<String>,
    /// A supplied row cannot revive a removal; only a new positive delta can.
    invalidated_ids: HashSet<String>,
    /// The revision whose missing rows the engine was already asked to
    /// re-send, so one shed payload costs at most one request.
    resynced: Option<u64>,
}

impl DeltaBase {
    /// Decides how `state` gets its queue, and records the base it leaves.
    fn adopt(&mut self, state: &PlaybackState, rows: PayloadRows) -> DeltaAdoption {
        if let Some(ids) = &state.uncached_ids {
            for id in ids {
                self.cached_ids.remove(id);
                self.invalidated_ids.insert(id.clone());
            }
            unmark_rows(&mut self.queue, ids);
        }
        let sent_queue = matches!(rows, PayloadRows::Sent | PayloadRows::QueueOnly);
        let sent_order = matches!(rows, PayloadRows::Sent | PayloadRows::OrderOnly);
        let mut new_marks = false;
        if let Some(ids) = &state.cached_ids {
            for id in ids {
                self.invalidated_ids.remove(id);
                if !self.cached_ids.contains(id) {
                    self.cached_ids.insert(id.clone());
                    new_marks = true;
                }
            }
        }
        if !sent_queue && new_marks {
            mark_cached_rows(&mut self.queue, &self.cached_ids);
        }
        if !sent_queue && (state.queue_revision == 0 || self.revision != Some(state.queue_revision)) {
            return DeltaAdoption::Unresolvable { revision: state.queue_revision };
        }
        if !sent_order && (state.order_revision == 0 || self.order_revision != Some(state.order_revision)) {
            return DeltaAdoption::Unresolvable { revision: state.order_revision };
        }
        if sent_queue {
            self.revision = (state.queue_revision != 0).then_some(state.queue_revision);
            self.queue = Arc::clone(&state.queue);
            for track in state.queue.iter() {
                if track.cached && !self.invalidated_ids.contains(&track.id) && !self.cached_ids.contains(&track.id) {
                    self.cached_ids.insert(track.id.clone());
                }
            }
        }
        if sent_order {
            self.order_revision = (state.order_revision != 0).then_some(state.order_revision);
            self.upcoming.clone_from(&state.upcoming);
        }
        let removed_rows = sent_queue && unmark_rows_set(&mut self.queue, &self.invalidated_ids);
        let marked_rows = sent_queue && mark_cached_rows(&mut self.queue, &self.cached_ids);
        self.resynced = None;
        if sent_queue && sent_order && !marked_rows && !removed_rows { DeltaAdoption::AsSent }
        else { DeltaAdoption::Fill { queue: Arc::clone(&self.queue), upcoming: self.upcoming.clone() } }
    }

    /// Records that the missing rows of `revision` are being re-requested,
    /// reporting whether that was the first time.
    fn mark_resynced(&mut self, revision: u64) -> bool {
        if self.resynced == Some(revision) {
            return false;
        }
        self.resynced = Some(revision);
        true
    }
}

/// Sets the download mark on the rows `cached_ids` names, copying shared rows
/// only when one actually changes. Reports whether any did.
fn mark_cached_rows(rows: &mut Arc<[Track]>, cached_ids: &HashSet<String>) -> bool {
    if !rows.iter().any(|track| !track.cached && cached_ids.contains(&track.id)) {
        return false;
    }
    for track in Arc::make_mut(rows) {
        track.cached |= cached_ids.contains(&track.id);
    }
    true
}

/// Clears every download mark, copying shared rows only when one is set.
pub fn unmark_cached_rows(rows: &mut Arc<[Track]>) {
    if rows.iter().any(|track| track.cached) {
        for track in Arc::make_mut(rows) {
            track.cached = false;
        }
    }
}

fn unmark_rows_set(rows: &mut Arc<[Track]>, ids: &HashSet<String>) -> bool {
    if !rows.iter().any(|track| track.cached && ids.contains(&track.id)) { return false; }
    for track in Arc::make_mut(rows) {
        if ids.contains(&track.id) { track.cached = false; }
    }
    true
}

fn unmark_rows(rows: &mut Arc<[Track]>, ids: &[String]) {
    if !ids.is_empty() {
        unmark_rows_set(rows, &ids.iter().cloned().collect());
    }
}

mod claims {
    //! Where each reply payload keeps its rows. `TrackRef` is the only
    //! protocol type with a download mark.
    use super::CachedClaims;
    use renderer_engine::protocol::*;

    type Visit<'a> = &'a mut dyn FnMut(&mut TrackRef);

    impl CachedClaims for TrackRef {
        fn claims(&mut self, visit: Visit) { visit(self) }
    }
    impl<T: CachedClaims> CachedClaims for Vec<T> {
        fn claims(&mut self, visit: Visit) { for item in self { item.claims(visit) } }
    }
    impl<T: CachedClaims> CachedClaims for Option<T> {
        fn claims(&mut self, visit: Visit) { if let Some(item) = self { item.claims(visit) } }
    }
    impl CachedClaims for HistoryPage {
        fn claims(&mut self, visit: Visit) { for item in &mut self.items { visit(&mut item.track) } }
    }
    impl CachedClaims for EpisodeRef {
        fn claims(&mut self, visit: Visit) { visit(&mut self.track) }
    }
    impl CachedClaims for ShowBrowse {
        fn claims(&mut self, visit: Visit) { self.episodes.claims(visit) }
    }
    impl CachedClaims for PlaylistBrowse {
        fn claims(&mut self, visit: Visit) { self.tracks.claims(visit) }
    }
    impl CachedClaims for PlaylistRead {
        fn claims(&mut self, visit: Visit) {
            match self {
                PlaylistRead::Changed(browse) => browse.claims(visit),
                // The marks of an unchanged read are ids, not rows: the shell
                // fences them against the rows it holds.
                PlaylistRead::Unchanged(_) => {}
            }
        }
    }
    impl CachedClaims for RadioBrowse {
        fn claims(&mut self, visit: Visit) { visit(&mut self.seed); self.tracks.claims(visit) }
    }
    impl CachedClaims for PlaylistRecommendations {
        fn claims(&mut self, visit: Visit) { self.tracks.claims(visit) }
    }
    impl CachedClaims for AlbumBrowse {
        fn claims(&mut self, visit: Visit) { self.tracks.claims(visit) }
    }
    impl CachedClaims for ArtistCataloguePage {
        fn claims(&mut self, visit: Visit) { self.releases.claims(visit) }
    }
    impl CachedClaims for ArtistBrowse {
        fn claims(&mut self, visit: Visit) {
            self.top_tracks.claims(visit);
            if let Some(ArtistPick { item: ArtistPickItem::Track(track), .. }) =
                self.overview.as_mut().and_then(|overview| overview.artist_pick.as_mut())
            {
                visit(track);
            }
        }
    }
    impl CachedClaims for SongwriterPlaylist {
        fn claims(&mut self, visit: Visit) { self.tracks.claims(visit) }
    }
    impl CachedClaims for LikedSongsPage {
        fn claims(&mut self, visit: Visit) { self.tracks.claims(visit) }
    }
    impl CachedClaims for SearchBrowse {
        fn claims(&mut self, visit: Visit) {
            if let Some(SearchTopRef::Track(track)) = &mut self.top { visit(track); }
            self.tracks.claims(visit);
            self.episodes.claims(visit);
        }
    }

    // Payloads without rows.
    impl CachedClaims for TrackWaveform {}
    impl CachedClaims for LibraryNode {}
    impl CachedClaims for UserProfile {}
    impl CachedClaims for PlaylistRef {}
    impl CachedClaims for PlaylistMembership {}
    impl CachedClaims for QueueMetadata {}
    impl CachedClaims for LikedUrisPage {}
    impl CachedClaims for TrackCredits {}
    impl CachedClaims for Canvas {}
    impl CachedClaims for TrackEditStatus {}
    impl CachedClaims for TrackEditDefinition {}
    impl CachedClaims for ArtistRef {}
    impl CachedClaims for ShowRef {}
    impl CachedClaims for bool {}
    impl CachedClaims for String {}
}

pub struct EngineClient {
    state_dir: PathBuf,
    pending: Mutex<HashMap<String, PendingRequest>>,
    stdin: Mutex<Option<EngineInput>>,
    process: Mutex<Option<Child>>,
    state_tx: tokio::sync::broadcast::Sender<StateLine>,
    exit_tx: watch::Sender<bool>,
    last_state: Mutex<Option<PlaybackState>>,
    /// The newest complete wire state, including restore intermediates.
    observed_state: Mutex<Option<PlaybackState>>,
    has_authenticated_state: AtomicBool,
    cache_generation: AtomicU64,
    account_generation: AtomicU64,
    account_identity: Mutex<Option<String>>,
    /// The rows of the newest payload that carried a queue, for filling the
    /// ones that omit it. Kept apart from `last_state` on purpose: the
    /// durability filter below refuses previews and restore intermediates,
    /// and a delta base that followed it would lose the very rows the next
    /// payload leans on. See [`DeltaBase`].
    delta_base: Mutex<DeltaBase>,
    /// The latest full engine state is a draft preview. Scalar heartbeats carry
    /// no mode bit, so this keeps them from advancing the retained real queue
    /// snapshot while a preview is live.
    preview_active: AtomicBool,
    restore_pending: Mutex<Option<RestorePlan>>,
    /// Invalidates an in-flight restore attempt when its engine exits or the
    /// session is explicitly cleared.
    restore_generation: AtomicU64,
    persist_tx: SyncSender<PersistCommand>,
    persist_generation: AtomicU64,
    /// Bumped under the retained-state lock whenever a state line installs a
    /// queue there. The persistence writer treats an unchanged value as proof
    /// that the retained rows are unchanged and skips its row scan, which is
    /// what keeps a heartbeat-only pass from walking (and, before, cloning)
    /// the whole queue every two seconds.
    queue_generation: AtomicU64,
    persist_thread: Mutex<Option<JoinHandle<()>>>,
    next_request_id: AtomicU64,
    process_generation: AtomicU64,
    shutting_down: AtomicBool,
    /// Serializes child admission/publication, EOF cleanup and final shutdown.
    shutdown_lock: Mutex<()>,
    timing: Timing,
}

impl EngineClient {
    /// Creates a client and spawns the engine subprocess. The supervisor task
    /// (see [`EngineClient::supervise`]) takes over respawns from there.
    ///
    /// The receiver is subscribed before the child exists, so it hears the
    /// state every engine announces at startup: the consumer it is handed to
    /// needs no `status` request to learn the first state, and the engine
    /// never re-serializes its queue for one.
    pub fn start() -> (Arc<Self>, tokio::sync::broadcast::Receiver<StateLine>) {
        let (state_tx, _) = tokio::sync::broadcast::channel(64);
        let (exit_tx, _) = watch::channel(false);
        let (persist_tx, persist_rx) = mpsc::sync_channel(1);
        let restore_pending = load_playback_snapshot().map(|snapshot| RestorePlan {
            snapshot: RestoreSnapshot::from_durable(snapshot),
            sent: false,
            attempts: 0,
            only_if_preview: false,
            preview_lease_id: None,
        });
        let client = Arc::new(Self {
            state_dir: engine_state_dir(),
            pending: Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            process: Mutex::new(None),
            state_tx,
            exit_tx,
            last_state: Mutex::new(None),
            observed_state: Mutex::new(None),
            has_authenticated_state: AtomicBool::new(false),
            cache_generation: AtomicU64::new(0),
            account_generation: AtomicU64::new(0),
            account_identity: Mutex::new(None),
            delta_base: Mutex::new(DeltaBase::default()),
            preview_active: AtomicBool::new(false),
            restore_pending: Mutex::new(restore_pending),
            restore_generation: AtomicU64::new(0),
            persist_tx,
            persist_generation: AtomicU64::new(0),
            queue_generation: AtomicU64::new(0),
            persist_thread: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            process_generation: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            shutdown_lock: Mutex::new(()),
            timing: Timing::PRODUCTION,
        });
        *client.persist_thread.lock() = Some(spawn_persistence_writer(&client, persist_rx));
        let lines = client.subscribe_lines();
        if let Err(error) = client.spawn_engine() {
            log::error(&format!("engine spawn failed at startup: {error}"));
        }
        (client, lines)
    }

    /// Subscribes to the engine's playback lines (full `state` plus the scalar
    /// `position` and `volume` lanes) in wire order.
    pub fn subscribe_lines(&self) -> tokio::sync::broadcast::Receiver<StateLine> {
        self.state_tx.subscribe()
    }

    /// The reader's canonical snapshot, fresher than the UI's asynchronous copy.
    pub fn current_state(&self) -> Option<PlaybackState> { self.last_state.lock().clone() }

    pub fn cache_generation(&self) -> u64 { self.cache_generation.load(Ordering::Acquire) }

    pub fn reconcile_cached_rows(&self, rows: &mut [Track], generation: u64) {
        let base = self.delta_base.lock();
        let stale = generation != self.cache_generation();
        for row in rows {
            if stale || base.invalidated_ids.contains(&row.id) { row.cached = false; }
            if base.cached_ids.contains(&row.id) { row.cached = true; }
        }
    }

    /// [`Self::reconcile_cached_rows`] for marks that arrive as ids rather
    /// than rows (an unchanged playlist read): a reset since `generation`
    /// withdraws them all, a later removal its own.
    pub fn reconcile_cached_ids(&self, ids: &mut Vec<String>, generation: u64) {
        let base = self.delta_base.lock();
        if generation != self.cache_generation() { ids.clear(); }
        ids.retain(|id| !base.invalidated_ids.contains(id));
    }

    /// A response barrier has proved the restore commands finished. Adopt the
    /// engine's actual result even if end-of-track restart or a concurrent
    /// command legitimately changed it from the original intent.
    pub fn complete_pending_restore(&self, generation: u64) -> Option<PlaybackState> {
        let mut pending = self.restore_pending.lock();
        if self.restore_generation.load(Ordering::Acquire) != generation
            || pending.as_ref().is_some_and(|plan| plan.only_if_preview || !plan.sent)
        { return None; }
        let Some(state) = self.observed_state.lock().as_ref()
            .filter(|state| state.auth_state == "ready" && !state.preview).cloned()
        else { return None; };
        *pending = None;
        *self.last_state.lock() = Some(state.clone());
        self.queue_generation.fetch_add(1, Ordering::AcqRel);
        self.mark_persistence_dirty();
        drop(pending);
        let _ = self.state_tx.send(StateLine::State(state.clone()));
        Some(state)
    }

    /// Starts the pending startup/crash restore exactly once, after a fresh
    /// engine reports that authentication is ready. Preview leases are kept
    /// separate from this path and are never consumed here.
    /// The generation is captured under the same lock that claims the plan,
    /// so a failed old attempt cannot re-arm a replacement after an EOF.
    pub fn begin_pending_restore_with_generation(
        &self,
        state: &PlaybackState,
    ) -> Option<(RestoreSnapshot, u64)> {
        if state.auth_state != "ready" {
            return None;
        }
        let mut pending = self.restore_pending.lock();
        let snapshot = claim_restore_plan(&mut pending)?;
        Some((snapshot, self.restore_generation.load(Ordering::Acquire)))
    }

    /// Claims the already-authenticated restore when its backoff expires.
    /// An old timer cannot claim a replacement plan after an engine exit.
    pub fn begin_pending_retry(&self, generation: u64) -> Option<RestoreSnapshot> {
        let mut pending = self.restore_pending.lock();
        if self.restore_generation.load(Ordering::Acquire) != generation {
            return None;
        }
        claim_restore_plan(&mut pending)
    }

    pub fn restore_is_pending(&self) -> bool {
        self.restore_pending
            .lock()
            .as_ref()
            .is_some_and(|plan| !plan.only_if_preview)
    }

    fn clear_preview_restore_pending(&self) {
        let mut pending = self.restore_pending.lock();
        if pending.as_ref().is_some_and(|plan| plan.only_if_preview) {
            *pending = None;
        }
    }

    /// Claims the preview restore for `preview_lease_id`, capturing the first
    /// real state when necessary. Replacements transfer ownership before the
    /// engine validates the draft, but never replace the retained snapshot.
    fn capture_preview_restore(&self, preview_lease_id: u64) -> bool {
        let mut pending = self.restore_pending.lock();
        if let Some(plan) = pending.as_mut() {
            if plan.only_if_preview {
                plan.preview_lease_id = Some(preview_lease_id);
                plan.sent = false;
            } else {
                // A startup/crash restore is already a real snapshot. If an
                // editor opens before that restore is observed, promote the
                // same snapshot to a preview lease rather than losing it.
                plan.only_if_preview = true;
                plan.preview_lease_id = Some(preview_lease_id);
                plan.sent = false;
            }
            return false;
        }
        let Some(state) = self
            .last_state
            .lock()
            .as_ref()
            .filter(|state| state.auth_state == "ready")
            .cloned()
        else {
            return false;
        };
        *pending = Some(RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&state, state.playing),
            sent: false,
            attempts: 0,
            only_if_preview: true,
            preview_lease_id: Some(preview_lease_id),
        });
        true
    }

    fn finish_preview_restore(&self, preview_lease_id: u64, success: bool) {
        let mut pending = self.restore_pending.lock();
        let Some(plan) = pending
            .as_mut()
            .filter(|plan| plan.only_if_preview && plan.preview_lease_id == Some(preview_lease_id))
        else {
            return;
        };
        if success {
            *pending = None;
        } else {
            plan.sent = false;
        }
    }

    /// Re-arms the durable restore after a failed attempt, returning its
    /// backoff. At the attempt cap the plan is disarmed (`Ok(None)`); a failure
    /// from an exited engine or a cleared plan cannot mutate its replacement
    /// (`Err(())`). Preview leases are never consumed here.
    pub fn retry_pending_restore_for_generation(
        &self,
        generation: u64,
    ) -> Result<Option<Duration>, ()> {
        let mut pending = self.restore_pending.lock();
        if self.restore_generation.load(Ordering::Acquire) != generation {
            return Err(());
        }
        if !pending
            .as_ref()
            .is_some_and(|plan| !plan.only_if_preview && plan.sent)
        {
            return Err(());
        }
        Ok(retry_restore_plan(&mut pending))
    }

    /// Respawn loop: waits for the reader thread to signal EOF, sleeps with
    /// exponential backoff, then starts a fresh engine and re-requests
    /// `status` so the engine re-emits its current state. The backoff starts
    /// over only after a child stayed up for [`RespawnPolicy::stable_uptime`].
    pub async fn supervise(self: Arc<Self>) {
        self.supervise_with_spawn(Self::spawn_engine).await;
    }

    /// The same supervisor admits bundled children and isolated test children.
    async fn supervise_with_spawn(
        self: Arc<Self>,
        mut spawn: impl FnMut(&Arc<Self>) -> Result<(), String>,
    ) {
        let policy = self.timing.respawn;
        let mut backoff = policy.start;
        // The startup child was spawned by `start` just before this runs.
        let mut spawned_at = tokio::time::Instant::now();
        // Even a startup child that closed stdout before this task first ran
        // was admitted: charge its exit before attempting the next spawn.
        let mut respawn = self.process_generation.load(Ordering::Acquire) == 0;
        loop {
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            if respawn {
                // Startup spawn failed or the previous exit finished backoff.
                match spawn(&self) {
                    Ok(()) => {
                        spawned_at = tokio::time::Instant::now();
                    }
                    Err(error) => {
                        log::error(&format!(
                            "engine spawn failed ({error}); retrying in {}s",
                            backoff.as_secs()
                        ));
                        if !self.wait_respawn(backoff).await { return; }
                        backoff = policy.next(backoff);
                        continue;
                    }
                }
                // Re-sync the fresh engine's session ("re-send status").
                let mut shutdown = self.exit_tx.subscribe();
                let reply = tokio::select! {
                    reply = self.request("status", Value::Null) => reply,
                    _ = self.wait_shutdown(&mut shutdown) => return,
                };
                match reply {
                    Ok(_) => log::info("engine respawned; status re-requested"),
                    Err(error) => log::warn(&format!(
                        "engine respawned but status re-request failed: {error}"
                    )),
                }
            }
            let mut exited = self.exit_tx.subscribe();
            // A child that exited during the status request still goes through
            // the same uptime/backoff path. Admission resets the latch before
            // starting its reader, never after an early EOF.
            while self.process.lock().is_some() && !*exited.borrow() {
                if self.shutting_down.load(Ordering::Acquire) { return; }
                if exited.changed().await.is_err() {
                    return;
                }
            }
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            let uptime = spawned_at.elapsed();
            let delay = policy.delay(backoff, uptime);
            log::warn(&format!(
                "engine exited after {}s; respawning in {}s",
                uptime.as_secs(),
                delay.as_secs()
            ));
            if !self.wait_respawn(delay).await { return; }
            backoff = policy.next(delay);
            respawn = true;
        }
    }

    async fn wait_shutdown(&self, exited: &mut watch::Receiver<bool>) {
        while !self.shutting_down.load(Ordering::Acquire) {
            if exited.changed().await.is_err() { return; }
        }
    }

    /// Shutdown wakes both an exit watch and a backoff timer immediately.
    async fn wait_respawn(&self, delay: Duration) -> bool {
        let mut exited = self.exit_tx.subscribe();
        tokio::select! {
            _ = self.wait_shutdown(&mut exited) => false,
            _ = tokio::time::sleep(delay) => !self.shutting_down.load(Ordering::Acquire),
        }
    }

    /// Spawns the engine process and its reader thread. Only called when no
    /// process is currently running.
    fn spawn_engine(self: &Arc<Self>) -> Result<(), String> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("the playback engine is shutting down".to_owned());
        }
        let exe = locate_engine()
            .ok_or_else(|| format!("bundled {ENGINE_BINARY} not found"))?;
        std::fs::create_dir_all(&self.state_dir)
            .map_err(|error| format!("could not create engine state dir: {error}"))?;
        // The engine's diagnostics live in the same file the panic hook
        // appends to: env_logger stderr (apresolve/connect attempts and
        // failures at info level) plus panic reports via --log-file.
        let logs_dir = crate::app::logs_dir();
        let _ = std::fs::create_dir_all(&logs_dir);
        let engine_log = logs_dir.join("playback_engine.log");
        crate::log::rotate_if_large(&engine_log, ENGINE_LOG_MAX_BYTES);
        let mut command = Command::new(&exe);
        // Read fresh on every spawn (including supervisor respawns) so a
        // saved preference is picked up without restarting the whole app.
        let settings = load_app_settings();
        let audio_cache_limit_mb = settings.audio_cache_limit_mb;
        command
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--log-file")
            .arg(&engine_log)
            .arg("--audio-cache-limit-mb")
            .arg(audio_cache_limit_mb.to_string())
            .arg("--normalisation")
            .arg(settings.normalisation.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(
                match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&engine_log)
                {
                    Ok(file) => Stdio::from(file),
                    Err(_) => Stdio::null(),
                },
            )
            // Info level surfaces librespot's "Connecting to AP ..." lines
            // and apresolve failures; the default warn level only shows
            // failures, which hides whether the engine is even trying.
            // Symphonia is pinned to error: it warns once per frame gap
            // ("skipping junk at N bytes"), thousands of lines per track,
            // which is continuous disk I/O for no diagnostic value.
            .env(
                "RUST_LOG",
                "info,symphonia_bundle_mp3=error,symphonia_core=error",
            );
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(DETACHED_PROCESS);
        }
        let pid = self.spawn_child(&mut command)
            .map_err(|error| format!("could not spawn {exe:?}: {error}"))?;
        log::info(&format!(
            "engine spawned (pid {pid}); log={}",
            engine_log.display()
        ));
        Ok(())
    }

    fn spawn_child(self: &Arc<Self>, command: &mut Command) -> Result<u32, String> {
        let _lifecycle = self.shutdown_lock.lock();
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("the playback engine is shutting down".to_owned());
        }
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("engine input/output pipe is unavailable".to_owned());
        };
        let pid = child.id();
        let generation = self.process_generation.fetch_add(1, Ordering::AcqRel) + 1;
        *self.process.lock() = Some(child);
        *self.stdin.lock() = Some(spawn_input_writer(stdin, generation));
        self.exit_tx.send_replace(false);
        spawn_reader(stdout, self);
        Ok(pid)
    }

    fn next_request_id(&self) -> String {
        format!(
            "request-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Writes one request line and awaits its reply. `args` are the command's
    /// own fields, serialized straight into the line.
    pub async fn request(&self, kind: &str, args: impl Serialize) -> Result<EngineReply, String> {
        self.request_cancelling(kind, args, None).await
    }

    /// [`Self::request`], telling the engine to stop work on `cancellation_track_id`
    /// when the caller stops waiting for a request the engine did receive.
    async fn request_cancelling(
        &self,
        kind: &str,
        args: impl Serialize,
        cancellation_track_id: Option<&str>,
    ) -> Result<EngineReply, String> {
        let timeout = self.timing.request_timeout(kind);
        let deadline = tokio::time::Instant::now() + timeout;
        let request_id = self.next_request_id();
        // Encoded and measured before any bookkeeping: an oversized line is
        // refused here with its reason, never sent to time out unanswered.
        let line = request_line(&request_id, kind, args)?;
        let (sender, mut receiver) = oneshot::channel();
        let (generation, written) = {
            // Registration and writer admission belong to one child. EOF must
            // never finish this receiver before a replacement gets its line.
            let _lifecycle = self.shutdown_lock.lock();
            if self.shutting_down.load(Ordering::Acquire) {
                return Err("the playback engine is shutting down".to_owned());
            }
            self.pending.lock().insert(request_id.clone(), PendingRequest {
                sender,
                cache_generation: self.cache_generation(),
                account_generation: self.account_generation.load(Ordering::Acquire),
                resets_cache_marks: kind == "clear_audio_cache",
            });
            match self.write_line(line) {
                Ok(queued) => queued,
                Err(error) => {
                    self.pending.lock().remove(&request_id);
                    return Err(error);
                }
            }
        };
        let _pending = RequestGuard { pending: &self.pending, id: &request_id };
        let mut write_complete = false;
        let result = tokio::time::timeout_at(deadline, async {
            written.await.map_err(|_| "engine input writer stopped".to_owned())??;
            write_complete = true;
            // A dispatched cache clear owns disk work until terminal ACK/EOF.
            // Only its input write is bounded; ordinary replies keep the same
            // command deadline, including time already spent writing.
            if kind == "clear_audio_cache" { return Ok(None); }
            let reply = (&mut receiver).await.map_err(|_| "engine request was dropped".to_owned())?;
            Ok(Some(reply))
        }).await;
        if !write_complete {
            self.retire_process(generation);
            if kind == "clear_audio_cache" {
                // A partial write can have reached the child. Retirement must
                // be observed at EOF before releasing the disk-work owner.
                let _ = (&mut receiver).await;
            }
        }
        match result {
            Ok(Ok(reply)) => {
                let reply = match reply {
                    Some(reply) => reply,
                    None => receiver.await.map_err(|_| "engine request was dropped".to_owned())?,
                };
                if reply.ok { Ok(reply) }
                else { Err(reply.error.unwrap_or_else(|| "engine rejected the request".to_owned())) }
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                if write_complete {
                    if let Some(track_id) = cancellation_track_id {
                        let cancel = request_line(&self.next_request_id(), "cancel_track_waveform",
                            json!({"track_id": track_id}));
                        let input = self.stdin.lock();
                        if let (Ok(cancel), Some(input)) =
                            (cancel, input.as_ref().filter(|input| input.generation == generation))
                        {
                            let (written, _) = oneshot::channel();
                            let _ = input.sender.try_send(InputLine { line: cancel, written });
                        }
                    }
                }
                Err(format!("engine did not answer {kind} within {}s", timeout.as_secs()))
            }
        }
    }

    fn write_line(&self, line: String) -> Result<(u64, oneshot::Receiver<Result<(), String>>), String> {
        let stdin = self.stdin.lock();
        let input = stdin.as_ref().ok_or_else(|| "the playback engine is not running".to_owned())?;
        let (written, completion) = oneshot::channel();
        input.sender.try_send(InputLine { line, written })
            .map_err(|error| match error {
                TrySendError::Full(_) => "engine input queue is full".to_owned(),
                TrySendError::Disconnected(_) => "engine input writer stopped".to_owned(),
            })?;
        Ok((input.generation, completion))
    }

    fn retire_process(&self, generation: u64) {
        let mut process = self.process.lock();
        if self.process_generation.load(Ordering::Acquire) != generation { return; }
        if let Some(child) = process.as_mut() { let _ = child.kill(); }
    }

    // ------------------------------------------------------------------
    // Typed requests
    // ------------------------------------------------------------------

    pub async fn status(&self) -> Result<(), String> {
        self.request("status", Value::Null).await.map(|_| ())
    }

    pub async fn clear_audio_cache(&self) -> Result<(), String> {
        self.request("clear_audio_cache", Value::Null).await.map(|_| ())
    }

    pub async fn play(&self) -> Result<(), String> {
        self.request("play", Value::Null).await.map(|_| ())
    }

    pub async fn pause(&self) -> Result<(), String> {
        self.request("pause", Value::Null).await.map(|_| ())
    }

    pub async fn next(&self) -> Result<(), String> {
        self.request("next", Value::Null).await.map(|_| ())
    }

    pub async fn previous(&self) -> Result<(), String> {
        self.request("previous", Value::Null).await.map(|_| ())
    }

    pub async fn seek(&self, position_ms: u32) -> Result<(), String> {
        self.request("seek", json!({"position_ms": position_ms}))
            .await
            .map(|_| ())
    }

    /// Rejects out-of-range percentages before they reach the engine: the
    /// IPC layer passes raw `u8`s and a corrupted durable snapshot could too.
    pub async fn set_volume(&self, percent: u8) -> Result<(), String> {
        if percent > 100 {
            return Err("volume percent must be between 0 and 100".to_owned());
        }
        self.request("set_volume", json!({"percent": percent}))
            .await
            .map(|_| ())
    }

    pub async fn set_shuffle(&self, enabled: bool) -> Result<(), String> {
        self.request("set_shuffle", json!({"enabled": enabled}))
            .await
            .map(|_| ())
    }

    pub async fn set_repeat(&self, mode: &str) -> Result<(), String> {
        if !matches!(mode, "off" | "context" | "track") {
            return Err(format!(
                "invalid repeat mode {mode:?} (expected off, context or track)"
            ));
        }
        self.request("set_repeat", json!({"mode": mode}))
            .await
            .map(|_| ())
    }

    pub async fn set_playback_speed(&self, speed: f32) -> Result<(), String> {
        if !speed.is_finite() || !(0.5..=4.0).contains(&speed) {
            return Err("playback speed must be between 0.5 and 4.0".to_owned());
        }
        self.request("set_playback_speed", json!({"speed": speed}))
            .await
            .map(|_| ())
    }

    /// Both remembered speeds, songs' and episodes', as a restore hands them
    /// back; the engine applies whichever the current item's kind uses.
    pub async fn set_playback_speeds(&self, track_speed: f32, episode_speed: f32) -> Result<(), String> {
        if ![track_speed, episode_speed]
            .iter()
            .all(|speed| speed.is_finite() && (0.5..=4.0).contains(speed))
        {
            return Err("playback speed must be between 0.5 and 4.0".to_owned());
        }
        self.request(
            "set_playback_speeds",
            json!({"track_speed": track_speed, "episode_speed": episode_speed}),
        )
        .await
        .map(|_| ())
    }

    /// `start_paused` loads the chosen row paused instead of playing it.
    pub async fn play_queue(
        &self,
        queue: &[Track],
        index: usize,
        position_ms: u32,
        context: &str,
        automatic_start: bool,
        start_paused: bool,
    ) -> Result<(), String> {
        let result = self
            .request(
                "play_queue",
                QueueRequest {
                    queue,
                    index,
                    position_ms,
                    context,
                    automatic_start,
                    start_paused,
                    ..QueueRequest::default()
                },
            )
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn preview_track_edit(
        &self,
        track: &Track,
        cuts: &[renderer_engine::protocol::TimeRange],
        loop_range: Option<renderer_engine::protocol::LoopRange>,
        position_ms: u32,
        preview_lease_id: u64,
    ) -> Result<(), String> {
        // Claim ownership before sending the draft. The engine performs the
        // same transfer before replacement validation, so a rejected draft
        // still leaves the installed preview restorable by this context.
        self.capture_preview_restore(preview_lease_id);
        self.request(
            "preview_track_edit",
            json!({
                "track": track,
                "cuts": cuts,
                "loop_range": loop_range,
                "position_ms": position_ms,
                "preview_lease_id": preview_lease_id,
            }),
        )
        .await
        .map(|_| ())
    }

    /// Restores the first real queue captured before an editor preview. The
    /// engine guards this request with preview mode and the lease ID, so
    /// tearing down an old editor can never replace a newer real queue or
    /// preview.
    pub async fn restore_preview(&self, preview_lease_id: u64) -> Result<(), String> {
        let snapshot = {
            let mut pending = self.restore_pending.lock();
            let Some(plan) = pending.as_mut().filter(|plan| {
                plan.only_if_preview && plan.preview_lease_id == Some(preview_lease_id)
            }) else {
                // A stale teardown is deliberately an idempotent no-op. In
                // particular it must not consume the current editor lease.
                return Ok(());
            };
            if plan.sent {
                return Ok(());
            }
            plan.sent = true;
            plan.snapshot.clone()
        };
        let result = self
            .request(
                "restore_queue",
                QueueRequest {
                    queue: &snapshot.queue,
                    index: snapshot.current_index.unwrap_or(0),
                    position_ms: snapshot.position_ms,
                    only_if_preview: true,
                    preview_lease_id: Some(preview_lease_id),
                    resume_playing: Some(snapshot.resume_playing),
                    ..QueueRequest::default()
                },
            )
            .await
            .map(|_| ());
        // A transport error is uncertain: the engine may have executed the
        // restore. Keep ownership and permit a later settled teardown to
        // retry. A matching successful response may consume the lease.
        self.finish_preview_restore(preview_lease_id, result.is_ok());
        result
    }

    pub async fn restore_queue(
        &self,
        queue: &[Track],
        index: usize,
        position_ms: u32,
        context: &str,
        automatic_start: bool,
        resume_playing: bool,
    ) -> Result<(), String> {
        let result = self
            .request(
                "restore_queue",
                QueueRequest {
                    queue,
                    index,
                    position_ms,
                    context,
                    automatic_start,
                    resume_playing: Some(resume_playing),
                    ..QueueRequest::default()
                },
            )
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    /// Align a paused canonical occurrence without replacing its rows or order.
    pub async fn set_queue_cursor(&self, index: usize, position_ms: u32) -> Result<(), String> {
        self.request("set_queue_cursor", json!({ "index": index, "position_ms": position_ms }))
            .await.map(|_| ())
    }

    pub async fn play_queue_index(&self, index: usize) -> Result<(), String> {
        let result = self
            .request("play_queue_index", json!({"index": index}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn add_queue(&self, track: &Track, context: &str) -> Result<(), String> {
        let result = self
            .request("add_queue", json!({"track": track, "context": context}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn add_queue_batch(&self, tracks: &[Track], context: &str) -> Result<(), String> {
        let result = self
            .request("add_queue_batch", TracksRequest { tracks, context })
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn remove_queue(&self, index: usize) -> Result<(), String> {
        let result = self
            .request("remove_queue", json!({"index": index}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn move_queue(&self, from: usize, to: usize) -> Result<(), String> {
        let result = self
            .request("move_queue", json!({"from": from, "to": to}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn get_history(
        &self,
        offset: usize,
        limit: usize,
        query: &str,
        sort: &str,
    ) -> Result<renderer_engine::protocol::HistoryPage, String> {
        let reply = self
            .request(
                "get_history",
                json!({"offset": offset, "limit": limit, "query": query, "sort": sort}),
            )
            .await?;
        parse_data(reply, "get_history")
    }

    pub async fn clear_history(&self) -> Result<(), String> {
        self.request("clear_history", Value::Null).await.map(|_| ())
    }

    pub async fn get_track_waveform(
        &self,
        track_id: &str,
    ) -> Result<renderer_engine::protocol::TrackWaveform, String> {
        let reply = self
            .request_cancelling("get_track_waveform", json!({"track_id": track_id}), Some(track_id))
            .await?;
        parse_data(reply, "get_track_waveform")
    }

    pub async fn cancel_track_waveform(&self, track_id: &str) -> Result<(), String> {
        self.request("cancel_track_waveform", json!({"track_id": track_id}))
            .await
            .map(|_| ())
    }

    pub async fn login(&self) -> Result<(), String> {
        self.request("login", Value::Null).await.map(|_| ())
    }

    /// Applies the volume-normalisation preference to the live engine. The
    /// engine rebuilds its player from cached credentials, so a moment of
    /// reconnection is expected while a session is up.
    pub async fn set_normalisation(&self, enabled: bool) -> Result<(), String> {
        self.request("set_normalisation", json!({ "enabled": enabled }))
            .await
            .map(|_| ())
    }

    pub async fn logout(&self) -> Result<(), String> {
        self.request("logout", Value::Null).await?;
        // EOF can race a successful logout. Keep this lock order identical to
        // on_eof and on_state so a pre-logout real queue can never be
        // installed after logout has invalidated it.
        self.clear_session_restore();
        self.clear_persisted_playback_state().await
    }

    fn clear_session_restore(&self) {
        self.account_generation.fetch_add(1, Ordering::AcqRel);
        *self.account_identity.lock() = None;
        {
            let mut pending = self.restore_pending.lock();
            let mut last_state = self.last_state.lock();
            *pending = None;
            self.restore_generation.fetch_add(1, Ordering::AcqRel);
            *last_state = None;
            *self.observed_state.lock() = None;
            self.has_authenticated_state.store(false, Ordering::Release);
        }
        self.preview_active.store(false, Ordering::Release);
        self.clear_cache_marks();
    }

    /// The writer may be mid-write (an fsync) and its queue holds one command,
    /// so both handing it the clear and hearing back can block: the whole
    /// round trip runs on the blocking pool, never on an async worker.
    async fn clear_persisted_playback_state(&self) -> Result<(), String> {
        // Invalidates any snapshot an in-flight attempt captured before logout.
        self.persist_generation.fetch_add(1, Ordering::AcqRel);
        let persist_tx = self.persist_tx.clone();
        tokio::task::spawn_blocking(move || {
            let (result_tx, result_rx) = mpsc::channel();
            let result = persist_tx
                .send(PersistCommand::Clear(result_tx))
                .map_err(|_| "playback-state writer is unavailable".to_owned())
                .and_then(|_| {
                    result_rx.recv().map_err(|error| {
                        format!("playback-state writer stopped during clear: {error}")
                    })?
                });
            match result {
                Ok(()) => Ok(()),
                Err(error) => clear_playback_snapshot().map_err(|fallback| {
                    format!("{error}; direct playback-state clear failed: {fallback}")
                }),
            }
        })
        .await
        .map_err(|error| format!("playback-state clear did not finish: {error}"))?
    }

    // ------------------------------------------------------------------
    // Browse / edit requests (typed payloads from `data`)
    // ------------------------------------------------------------------


    pub async fn browse_playlist_tree(
        &self,
        length: usize,
    ) -> Result<Vec<renderer_engine::protocol::LibraryNode>, String> {
        let reply = self
            .request("browse_playlist_tree", json!({"length": length}))
            .await?;
        parse_data(reply, "browse_playlist_tree")
    }

    pub async fn browse_show(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::ShowBrowse, String> {
        let reply = self.request("browse_show", json!({"id": id})).await?;
        parse_data(reply, "browse_show")
    }

    pub async fn browse_episode(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::EpisodeRef, String> {
        let reply = self.request("browse_episode", json!({"id": id})).await?;
        parse_data(reply, "browse_episode")
    }

    pub async fn browse_profile(
        &self,
        username: &str,
        known_playlists: &[renderer_engine::protocol::PlaylistRef],
    ) -> Result<renderer_engine::protocol::UserProfile, String> {
        let reply = self
            .request("browse_profile", json!({"username": username, "known_playlists": known_playlists}))
            .await?;
        parse_data(reply, "browse_profile")
    }

    pub async fn browse_playlist_covers(
        &self,
        playlists: &[renderer_engine::protocol::PlaylistRef],
    ) -> Result<Vec<renderer_engine::protocol::PlaylistRef>, String> {
        let reply = self.request("browse_playlist_covers", json!({"playlists": playlists})).await?;
        parse_data(reply, "browse_playlist_covers")
    }

    /// `tracks_key` names rows the caller holds; while the playlist still
    /// answers to it the engine resolves and sends no rows (`Unchanged`).
    pub async fn browse_playlist(
        &self,
        id: &str,
        tracks_key: Option<&str>,
    ) -> Result<renderer_engine::protocol::PlaylistRead, String> {
        let reply = self.request("browse_playlist", json!({"id": id, "tracks_key": tracks_key})).await?;
        parse_data(reply, "browse_playlist")
    }

    pub async fn browse_playlist_membership(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::PlaylistMembership, String> {
        parse_data(self.request("browse_playlist_membership", json!({"id": id})).await?, "browse_playlist_membership")
    }

    pub async fn queue_metadata(
        &self,
        context: Option<&str>,
    ) -> Result<renderer_engine::protocol::QueueMetadata, String> {
        parse_data(self.request("queue_metadata", json!({"context": context})).await?, "queue_metadata")
    }

    pub async fn browse_radio(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::RadioBrowse, String> {
        let reply = self.request("browse_radio", json!({"id": id})).await?;
        parse_data(reply, "browse_radio")
    }

    pub async fn browse_playlist_recommendations(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::PlaylistRecommendations, String> {
        let reply = self
            .request("browse_playlist_recommendations", json!({"id": id}))
            .await?;
        parse_data(reply, "browse_playlist_recommendations")
    }

    pub async fn browse_track(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::TrackRef, String> {
        let reply = self.request("browse_track", json!({"id": id})).await?;
        parse_data(reply, "browse_track")
    }

    pub async fn browse_album(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::AlbumBrowse, String> {
        let reply = self.request("browse_album", json!({"id": id})).await?;
        parse_data(reply, "browse_album")
    }

    pub async fn browse_artist(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::ArtistBrowse, String> {
        let reply = self.request("browse_artist", json!({"id": id})).await?;
        parse_data(reply, "browse_artist")
    }

    pub async fn browse_artist_songwriter(
        &self,
        id: &str,
        name: &str,
    ) -> Result<Option<renderer_engine::protocol::SongwriterPlaylist>, String> {
        let reply = self
            .request("browse_artist_songwriter", json!({"id": id, "name": name}))
            .await?;
        parse_data(reply, "browse_artist_songwriter")
    }

    pub async fn browse_artist_catalogue(
        &self,
        id: &str,
        release_types: &[String],
        offset: usize,
        limit: usize,
    ) -> Result<renderer_engine::protocol::ArtistCataloguePage, String> {
        let reply = self
            .request(
                "browse_artist_catalogue",
                json!({"id": id, "release_types": release_types, "offset": offset, "limit": limit}),
            )
            .await?;
        parse_data(reply, "browse_artist_catalogue")
    }

    pub async fn browse_liked_songs(
        &self,
        cursor: Option<&str>,
    ) -> Result<renderer_engine::protocol::LikedSongsPage, String> {
        let reply = self
            .request("browse_liked_songs", json!({"cursor": cursor}))
            .await?;
        parse_data(reply, "browse_liked_songs")
    }

    /// Saved Tracks as bare URIs — the membership lookup the saved mark
    /// reconciles with. Never fetches track metadata.
    pub async fn browse_liked_uris(
        &self,
        cursor: Option<&str>,
    ) -> Result<renderer_engine::protocol::LikedUrisPage, String> {
        let reply = self
            .request("browse_liked_uris", json!({"cursor": cursor}))
            .await?;
        parse_data(reply, "browse_liked_uris")
    }

    pub async fn browse_track_credits(
        &self,
        id: &str,
    ) -> Result<renderer_engine::protocol::TrackCredits, String> {
        let reply = self
            .request("browse_track_credits", json!({"id": id}))
            .await?;
        parse_data(reply, "browse_track_credits")
    }

    pub async fn browse_canvas(
        &self,
        id: &str,
    ) -> Result<Option<renderer_engine::protocol::Canvas>, String> {
        let reply = self.request("browse_canvas", json!({"id": id})).await?;
        parse_data(reply, "browse_canvas")
    }

    /// Turns the account's Canvas preference on when it is off; answers
    /// whether anything was written.
    pub async fn enable_account_canvas(&self) -> Result<bool, String> {
        let reply = self.request("enable_account_canvas", json!({})).await?;
        parse_data(reply, "enable_account_canvas")
    }

    pub async fn track_edit_status(
        &self,
        track_id: &str,
        playlist_id: Option<&str>,
    ) -> Result<renderer_engine::protocol::TrackEditStatus, String> {
        let reply = self
            .request(
                "get_track_edit",
                json!({"track_id": track_id, "playlist_id": playlist_id}),
            )
            .await?;
        parse_data(reply, "get_track_edit")
    }

    pub async fn save_track_edit(
        &self,
        track_id: &str,
        duration_ms: u32,
        cuts: &[renderer_engine::protocol::TimeRange],
        loop_range: Option<renderer_engine::protocol::LoopRange>,
    ) -> Result<renderer_engine::protocol::TrackEditDefinition, String> {
        let reply = self
            .request(
                "save_track_edit",
                json!({
                    "track_id": track_id,
                    "duration_ms": duration_ms,
                    "cuts": cuts,
                    "loop_range": loop_range,
                }),
            )
            .await?;
        parse_data(reply, "save_track_edit")
    }

    pub async fn delete_track_edit(&self, track_id: &str) -> Result<(), String> {
        self.request("delete_track_edit", json!({"track_id": track_id}))
            .await
            .map(|_| ())
    }

    pub async fn set_playlist_track_edit_enabled(
        &self,
        playlist_id: &str,
        track_id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.request(
            "set_playlist_track_edit_enabled",
            json!({
                "playlist_id": playlist_id,
                "track_id": track_id,
                "enabled": enabled,
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn set_playlist_track_excluded(
        &self,
        playlist_id: &str,
        track_id: &str,
        excluded: bool,
    ) -> Result<(), String> {
        self.request(
            "set_playlist_track_excluded",
            json!({
                "playlist_id": playlist_id,
                "track_id": track_id,
                "excluded": excluded,
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn browse_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<renderer_engine::protocol::SearchBrowse, String> {
        let reply = self
            .request("browse_search", json!({"query": query, "limit": limit}))
            .await?;
        parse_data(reply, "browse_search")
    }

    pub async fn browse_followed_artists(
        &self,
    ) -> Result<Vec<renderer_engine::protocol::ArtistRef>, String> {
        let reply = self.request("browse_followed_artists", json!({})).await?;
        parse_data(reply, "browse_followed_artists")
    }

    pub async fn browse_saved_shows(
        &self,
    ) -> Result<Vec<renderer_engine::protocol::ShowRef>, String> {
        let reply = self.request("browse_saved_shows", json!({})).await?;
        parse_data(reply, "browse_saved_shows")
    }

    pub async fn create_playlist(
        &self,
        name: &str,
    ) -> Result<renderer_engine::protocol::PlaylistRef, String> {
        let reply = self
            .request("edit_create_playlist", json!({"name": name}))
            .await?;
        parse_data(reply, "edit_create_playlist")
    }

    pub async fn rename_playlist(&self, id: &str, name: &str) -> Result<(), String> {
        let _ = self
            .request("edit_rename_playlist", json!({"id": id, "name": name}))
            .await?;
        Ok(())
    }

    pub async fn delete_playlist(&self, id: &str) -> Result<(), String> {
        let _ = self
            .request("edit_delete_playlist", json!({"id": id}))
            .await?;
        Ok(())
    }

    pub async fn follow_playlist(&self, id: &str) -> Result<(), String> {
        let _ = self
            .request("edit_follow_playlist", json!({"id": id}))
            .await?;
        Ok(())
    }

    /// Answers with the new folder's id.
    pub async fn create_folder(&self, name: &str, parent: Option<&str>) -> Result<String, String> {
        let reply = self
            .request("edit_create_folder", json!({"name": name, "parent": parent}))
            .await?;
        parse_data(reply, "edit_create_folder")
    }

    pub async fn rename_folder(&self, id: &str, name: &str) -> Result<(), String> {
        let _ = self
            .request("edit_rename_folder", json!({"id": id, "name": name}))
            .await?;
        Ok(())
    }

    pub async fn delete_folder(&self, id: &str) -> Result<(), String> {
        let _ = self
            .request("edit_delete_folder", json!({"id": id}))
            .await?;
        Ok(())
    }

    pub async fn move_library_item(
        &self,
        item: &renderer_engine::protocol::LibraryItem,
        folder: Option<&str>,
    ) -> Result<(), String> {
        let _ = self
            .request("edit_move_library_item", json!({"item": item, "folder": folder}))
            .await?;
        Ok(())
    }

    pub async fn add_playlist_tracks(&self, id: &str, uris: &[String]) -> Result<(), String> {
        let _ = self
            .request("edit_add_playlist_tracks", json!({"id": id, "uris": uris}))
            .await?;
        Ok(())
    }

    pub async fn remove_playlist_tracks(
        &self,
        id: &str,
        uris: &[String],
        expected_snapshot_id: Option<&str>,
    ) -> Result<(), String> {
        let _ = self
            .request(
                "edit_remove_playlist_tracks",
                json!({"id": id, "uris": uris, "expected_snapshot_id": expected_snapshot_id}),
            )
            .await?;
        Ok(())
    }

    pub async fn reorder_playlist_tracks(
        &self,
        id: &str,
        from: usize,
        to: usize,
        row_count: usize,
    ) -> Result<(), String> {
        let _ = self
            .request(
                "edit_reorder_playlist_tracks",
                json!({"id": id, "from": from, "to": to, "row_count": row_count}),
            )
            .await?;
        Ok(())
    }

    /// Stops the engine before requesting an unthrottled final flush from the
    /// sole persistence writer. Once shutdown starts, reader callbacks freeze
    /// state publication so the final finite generation is stable.
    pub fn shutdown_engine(&self) {
        let _shutdown = self.shutdown_lock.lock();
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        self.exit_tx.send_replace(true);
        log::info("engine shutdown requested");

        if let Ok(line) = request_line(&self.next_request_id(), "shutdown", ()) {
            let _ = self.write_line(line);
        }
        let child = self.process.lock().take();
        if let Some(mut child) = child {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    _ => { let _ = child.kill(); let _ = child.wait(); break; }
                }
            }
        }
        *self.stdin.lock() = None;
        self.fail_pending_on_exit();

        let generation = self.persist_generation.load(Ordering::Acquire);
        let (result_tx, result_rx) = mpsc::channel();
        if self
            .persist_tx
            .send(PersistCommand::Shutdown {
                generation,
                result_tx,
            })
            .is_ok()
        {
            match result_rx.recv() {
                Ok(Err(error)) => log::warn(&format!("could not persist playback state: {error}")),
                Err(error) => log::warn(&format!(
                    "playback-state writer stopped before shutdown flush: {error}"
                )),
                Ok(Ok(())) => {}
            }
        }
        if let Some(writer) = self.persist_thread.lock().take() {
            let _ = writer.join();
        }
    }

    /// A session boundary or successful explicit clear invalidates every
    /// retained mark and fences browse work already in flight.
    pub fn clear_cache_marks(&self) {
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
        {
            let mut base = self.delta_base.lock();
            base.cached_ids.clear();
            base.invalidated_ids.clear();
            unmark_cached_rows(&mut base.queue);
        }
        for state in [&self.last_state, &self.observed_state] {
            if let Some(state) = state.lock().as_mut() {
                state.cached_ids = None;
                state.uncached_ids = None;
                unmark_cached_rows(&mut state.queue);
            }
        }
        let _ = self.state_tx.send(StateLine::CacheMarksReset);
    }

    /// O(1) reader-thread notification. The bounded channel coalesces bursts;
    /// the generation lets the writer detect changes that arrived while it
    /// cloned or wrote a previous snapshot.
    fn mark_persistence_dirty(&self) {
        let generation = self.persist_generation.fetch_add(1, Ordering::AcqRel) + 1;
        match self.persist_tx.try_send(PersistCommand::Dirty(generation)) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) if self.shutting_down.load(Ordering::Relaxed) => {}
            Err(TrySendError::Disconnected(_)) => {
                log::warn("playback-state writer is unavailable");
            }
        }
    }

    /// The final shutdown flush keeps an unresolved restore's source ahead of
    /// a replacement's blank authentication state. Established sessions remain
    /// restorable through reconnect/error; only explicit session clearing retires them.
    fn playback_snapshot_for_shutdown(&self) -> Option<PlaybackSnapshot> {
        let pending = self.restore_pending.lock();
        let last_state = self.last_state.lock();
        pending.as_ref().map(|plan| plan.snapshot.durable()).or_else(|| {
            last_state.as_ref()
                .filter(|state| self.has_authenticated_state.load(Ordering::Acquire)
                    && !matches!(state.auth_state.as_str(), "needs_login" | "logged_out"))
                .map(PlaybackSnapshot::from_playback)
        })
    }

    /// The writer's deadline arm: borrows the retained state under the lock,
    /// consults the committed snapshot, and clones into an owned snapshot only
    /// when the whole queue is actually due. The 2-second heartbeat must never
    /// deep-copy the queue just to decide against writing — and must never
    /// re-walk it either, so a playhead-only difference answers with the
    /// sidecar.
    /// Restore intermediates are never durable state, mirroring
    /// [`Self::playback_snapshot_for_shutdown`]'s eligibility.
    fn due_write(&self, committed: Option<&PersistedSnapshot>) -> PersistDecision {
        if self.restore_pending.lock().is_some() {
            return PersistDecision {
                compared: None,
                write: None,
            };
        }
        let last_state = self.last_state.lock();
        let Some(state) = last_state.as_ref().filter(|state| state.auth_state == "ready") else {
            return PersistDecision {
                compared: None,
                write: None,
            };
        };
        // Read under the same lock that installs a state line: a generation
        // read beside a queue is a claim about that queue, and the writer's
        // next pass may skip the row scan on the strength of it.
        let queue_generation = self.queue_generation.load(Ordering::Acquire);
        let write = match committed {
            // Nothing durable yet: the first commit has to carry the queue.
            None => Some(PersistWrite::Snapshot(PlaybackSnapshot::from_playback(
                state,
            ))),
            Some(committed) if committed.differs_structurally_from(state, queue_generation) => {
                Some(PersistWrite::Snapshot(PlaybackSnapshot::from_playback(
                    state,
                )))
            }
            Some(committed) if committed.playhead_differs_from(state) => {
                Some(PersistWrite::Playhead {
                    current_index: state.current_index,
                    position_ms: state.position_ms,
                    // Taken from the snapshot this pass compared against, which
                    // is the snapshot the sidecar will trail on disk.
                    queue_identity: committed.snapshot.queue_identity(),
                })
            }
            Some(_) => None,
        };
        PersistDecision {
            compared: Some(queue_generation),
            write,
        }
    }

    // ------------------------------------------------------------------
    // Reader-thread callbacks
    // ------------------------------------------------------------------

    fn on_state(&self, state: &PlaybackState, rows: PayloadRows) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let account_changed = {
            let mut identity = self.account_identity.lock();
            let next = if state.auth_state == "ready" { Some(state.username.clone()) }
                else if matches!(state.auth_state.as_str(), "logged_out" | "needs_login") { None }
                else { identity.clone() };
            let changed = identity.is_some() && *identity != next;
            if changed { self.account_generation.fetch_add(1, Ordering::AcqRel); }
            *identity = next;
            changed
        };
        if account_changed { self.clear_cache_marks(); }
        // A false state only reconciles a lease after this client has
        // observed the engine in preview mode. An unrelated false state
        // arriving before activation must not clear a freshly claimed lease.
        let was_preview = self.preview_active.swap(state.preview, Ordering::AcqRel);
        // Fill independently omitted arrays before restoring, persisting or
        // publishing the state. Explicit empty arrays are never filled.
        let adoption = self.delta_base.lock().adopt(state, rows);
        let merged;
        let state = match adoption {
            DeltaAdoption::AsSent => state,
            DeltaAdoption::Fill { queue, upcoming } => {
                merged = PlaybackState {
                    queue,
                    upcoming,
                    ..state.clone()
                };
                &merged
            }
            DeltaAdoption::Unresolvable { revision } => {
                if let Some(ids) = &state.uncached_ids {
                    let _ = self.state_tx.send(StateLine::UncachedIds(ids.clone()));
                }
                if let Some(ids) = &state.cached_ids {
                    let _ = self.state_tx.send(StateLine::CachedIds(ids.clone()));
                }
                // The engine hands the rows of a revision over exactly once,
                // and that payload never made it here, so there is no base to
                // merge from and no honest queue to publish: filling the gap
                // from the retained frame of another revision would publish
                // rows the engine never named, and forwarding the parsed
                // (empty) vectors would read downstream as "the queue is
                // gone" — media keys would report Stopped and the window
                // would drop the rows it holds. So the payload is withheld,
                // and the engine is asked to re-send its state once per
                // revision. A fresh engine process answers with the rows (it
                // has handed nothing over yet); otherwise the shell stays on
                // its last complete frame until the next queue change, which
                // always carries rows, and the scalars it withheld are the
                // ones that state replaces anyway.
                log::warn(&format!(
                    "engine state omitted the queue of revision {revision}, which was never received: withholding it and asking the engine to re-send"
                ));
                self.request_state_resync(revision);
                return;
            }
        };
        let mut pending = self.restore_pending.lock();
        *self.observed_state.lock() = Some(state.clone());
        if state.auth_state == "ready" && !state.preview {
            self.has_authenticated_state.store(true, Ordering::Release);
        }
        if was_preview
            && !state.preview
            && pending.as_ref().is_some_and(|plan| plan.only_if_preview)
        {
            *pending = None;
        }
        let matched = pending
            .as_ref()
            .is_some_and(|plan| !plan.only_if_preview && plan.sent && plan.snapshot.matches(state));
        if matched {
            *pending = None;
        }
        let suppress_snapshot = state.preview
            || pending
                .as_ref()
                .is_some_and(|plan| !plan.only_if_preview && state.auth_state == "ready");
        if !suppress_snapshot {
            let mut last = self.last_state.lock();
            *last = Some(state.clone());
            // Bumped under the same lock that installs the rows: the writer
            // treats an unchanged generation as proof that the retained queue
            // is unchanged, so every install has to count as one.
            self.queue_generation.fetch_add(1, Ordering::AcqRel);
            drop(last);
            self.mark_persistence_dirty();
        }
        drop(pending);
        let _ = self.state_tx.send(StateLine::State(state.clone()));
    }

    /// Asks the engine to re-emit its state after a payload arrived without
    /// rows this client never received (see [`Self::on_state`]).
    ///
    /// `status` is the protocol's re-sync request — the supervisor sends the
    /// same one after a respawn — and its answer re-emits the whole state. The
    /// reply is deliberately not awaited: this runs on the reader thread, and
    /// the state it produces comes back through the normal lane.
    ///
    /// Asked at most once per revision: an engine that has already handed that
    /// generation over would answer with the same row-less payload, and asking
    /// again would be a request loop.
    fn request_state_resync(&self, revision: u64) {
        if !self.delta_base.lock().mark_resynced(revision) {
            return;
        }
        let line = request_line(&self.next_request_id(), "status", ());
        if let Err(error) = line.and_then(|line| self.write_line(line)) {
            log::warn(&format!(
                "could not ask the engine to re-send the state of revision {revision}: {error}"
            ));
        }
    }

    /// Applies a scalar position heartbeat to the heartbeat-fresh last real
    /// state. Preview heartbeats still reach the window, but cannot mutate the
    /// queue snapshot used for persistence and child-crash recovery.
    fn on_position(&self, heartbeat: PositionHeartbeat) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if let Some(observed) = self.observed_state.lock().as_mut() {
            observed.position_ms = heartbeat.position_ms;
            observed.duration_ms = heartbeat.duration_ms;
        }
        let changed = if self.preview_active.load(Ordering::Acquire) {
            false
        } else if let Some(last) = self.last_state.lock().as_mut() {
            last.position_ms = heartbeat.position_ms;
            last.duration_ms = heartbeat.duration_ms;
            true
        } else {
            false
        };
        if changed {
            self.mark_persistence_dirty();
        }
        let _ = self.state_tx.send(StateLine::Position(heartbeat));
    }

    /// Applies a scalar volume line to the heartbeat-fresh last real state and
    /// fans it out.
    ///
    /// Unlike a position heartbeat this is not a projection of something the
    /// window already knows: the volume is the setting the engine actually
    /// applied, so it freshens the retained snapshot — the one `status`,
    /// `get_state` and the durable playback snapshot are built from — and is
    /// worth a persistence check when it changes. The queue is never touched:
    /// the line carries one number.
    fn on_volume(&self, volume: u8) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if let Some(observed) = self.observed_state.lock().as_mut() { observed.volume = volume; }
        let changed = match self.last_state.lock().as_mut() {
            Some(last) => {
                last.volume = volume;
                true
            }
            None => false,
        };
        if changed {
            self.mark_persistence_dirty();
        }
        let _ = self.state_tx.send(StateLine::Volume(volume));
    }

    fn on_eof(&self) {
        let _lifecycle = self.shutdown_lock.lock();
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        log::warn("engine process exited (stdout closed)");
        let mut restore = self.restore_pending.lock();
        self.restore_generation.fetch_add(1, Ordering::AcqRel);
        let last = self.last_state.lock().clone();
        if restore.as_ref().is_some_and(|plan| !plan.only_if_preview) {
            // A replacement may crash while still authenticating: its blank
            // intermediate must never replace the original restore source.
            restore.as_mut().unwrap().sent = false;
        } else if let Some(last) = last.filter(|state| self.has_authenticated_state.load(Ordering::Acquire)
            && !matches!(state.auth_state.as_str(), "needs_login" | "logged_out"))
        {
            *restore = Some(RestorePlan {
                snapshot: RestoreSnapshot::from_playback(&last, last.playing),
                sent: false,
                attempts: 0,
                only_if_preview: false,
                preview_lease_id: None,
            });
        } else if restore.as_ref().is_some_and(|plan| plan.only_if_preview) {
            *restore = None;
        } else if let Some(plan) = restore.as_mut() {
            plan.sent = false;
        }
        drop(restore);
        *self.observed_state.lock() = None;
        self.clear_cache_marks();
        *self.delta_base.lock() = DeltaBase::default();
        let _ = self.state_tx.send(StateLine::Disconnected);
        *self.stdin.lock() = None;
        let child = self.process.lock().take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.fail_pending_on_exit();
        self.exit_tx.send_replace(true);
    }

    /// Only called after the owned child has stopped: a terminal error cannot
    /// release disk-operation ownership while the subprocess still runs.
    fn fail_pending_on_exit(&self) {
        let mut pending = self.pending.lock();
        for (_, request) in pending.drain() {
            let _ = request.sender.send(EngineReply {
                ok: false,
                error: Some("the playback engine exited".to_owned()),
                ..EngineReply::default()
            });
        }
    }

    fn deliver(&self, request_id: &str, mut reply: EngineReply) {
        let mut pending = self.pending.lock();
        if let Some(request) = pending.remove(request_id) {
            if request.account_generation != self.account_generation.load(Ordering::Acquire) {
                let _ = request.sender.send(EngineReply {
                    ok: false,
                    error: Some("the engine account changed while the request was running".to_owned()),
                    ..EngineReply::default()
                });
                return;
            }
            if reply.ok && request.resets_cache_marks {
                // The wire ACK is the reset boundary. The caller may wake
                // after new positive lines, so it must never reset again.
                self.clear_cache_marks();
            }
            // Judged now, applied by the typed parse: the claims are read
            // there, once, instead of rewritten here in a parsed tree.
            if reply.data.is_some() {
                reply.fence.stale = request.cache_generation != self.cache_generation();
                let base = self.delta_base.lock();
                if !reply.fence.stale && !base.invalidated_ids.is_empty() {
                    reply.fence.invalidated.clone_from(&base.invalidated_ids);
                }
            }
            let _ = request.sender.send(reply);
        }
    }
}

/// One ordered owner of each child's blocking pipe; killing the child
/// releases a stalled write without occupying an async runtime worker.
fn spawn_input_writer(mut pipe: ChildStdin, generation: u64) -> EngineInput {
    let (sender, receiver) = mpsc::sync_channel::<InputLine>(64);
    std::thread::Builder::new().name("engine-writer".to_owned()).spawn(move || {
        for input in receiver {
            let result = pipe.write_all(input.line.as_bytes())
                .and_then(|_| pipe.write_all(b"\n"))
                .and_then(|_| pipe.flush())
                .map_err(|error| format!("could not write to the playback engine: {error}"));
            let failed = result.is_err();
            let _ = input.written.send(result);
            if failed { break; }
        }
    }).expect("could not start engine input writer thread");
    EngineInput { generation, sender }
}

fn spawn_persistence_writer(
    client: &Arc<EngineClient>,
    receiver: Receiver<PersistCommand>,
) -> JoinHandle<()> {
    let client = Arc::downgrade(client);
    std::thread::Builder::new()
        .name("playback-state-writer".to_owned())
        .spawn(move || run_persistence_writer(client, receiver))
        .expect("could not start playback-state writer")
}

fn run_persistence_writer(client: Weak<EngineClient>, receiver: Receiver<PersistCommand>) {
    let mut schedule = PersistenceSchedule::default();
    loop {
        let command = match schedule.wait_for(Instant::now()) {
            Some(wait) => match receiver.recv_timeout(wait) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match receiver.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            },
        };

        match command {
            Some(PersistCommand::Dirty(generation)) => {
                schedule.mark_dirty(generation);
            }
            Some(PersistCommand::Clear(result_tx)) => {
                let result = clear_playback_snapshot();
                if result.is_ok() {
                    schedule = PersistenceSchedule::default();
                    schedule.committed_generation = client.upgrade().map_or(0, |client| {
                        client.persist_generation.load(Ordering::Acquire)
                    });
                }
                let _ = result_tx.send(result);
            }
            Some(PersistCommand::Shutdown {
                generation,
                result_tx,
            }) => {
                let result = client.upgrade().map_or(Ok(()), |client| {
                    // Shutdown is ordered after the engine is frozen, but
                    // retain a finite generation fence around the final clone
                    // so a last reader callback can never overwrite it with
                    // an older snapshot.
                    let current = client.persist_generation.load(Ordering::Acquire);
                    let mut snapshot = client.playback_snapshot_for_shutdown();
                    if current > generation {
                        snapshot = client.playback_snapshot_for_shutdown();
                    }
                    let latest = client.persist_generation.load(Ordering::Acquire);
                    if latest > current {
                        snapshot = client.playback_snapshot_for_shutdown();
                    }
                    snapshot.map_or(Ok(()), |snapshot| save_playback_snapshot(&snapshot))
                });
                let _ = result_tx.send(result);
                return;
            }
            None => {
                let Some(client) = client.upgrade() else {
                    return;
                };
                // The queued wake may represent an older generation because
                // the bounded channel coalesces bursts. Snapshot the current
                // generation at the deadline, and let the borrowed decision
                // under the lock decide before anything is cloned.
                let attempted_generation = client.persist_generation.load(Ordering::Acquire);
                let decision = client.due_write(schedule.committed.as_ref());
                let Some(queue_generation) = decision.compared else {
                    // A restore is in flight, or no ready state has been seen
                    // yet: no live queue was compared, so none may be recorded
                    // as compared.
                    let latest = client.persist_generation.load(Ordering::Acquire);
                    schedule.record_unchanged(attempted_generation, latest, None);
                    continue;
                };
                let Some(write) = decision.write else {
                    let latest = client.persist_generation.load(Ordering::Acquire);
                    schedule.record_unchanged(attempted_generation, latest, Some(queue_generation));
                    continue;
                };

                let written_at = Instant::now();
                let result = match &write {
                    PersistWrite::Snapshot(snapshot) => save_playback_snapshot(snapshot),
                    PersistWrite::Playhead {
                        current_index,
                        position_ms,
                        queue_identity,
                    } => save_playhead_snapshot(*current_index, *position_ms, *queue_identity),
                };
                let latest = client.persist_generation.load(Ordering::Acquire);
                match result {
                    Ok(()) => {
                        schedule.record_success(
                            write,
                            attempted_generation,
                            latest,
                            queue_generation,
                            written_at,
                        );
                    }
                    Err(error) => {
                        log::warn(&format!("could not persist playback state: {error}"));
                        schedule.record_failure(latest.max(attempted_generation), written_at);
                    }
                }
            }
        }
    }
}

/// Reader thread: parses one protocol line per iteration. `state`, `position`
/// and `volume` lines are fanned out to subscribers in wire order; every other
/// line is routed to the pending request with the matching id.
fn spawn_reader(stdout: ChildStdout, client: &Arc<EngineClient>) {
    let client = Arc::clone(client);
    std::thread::Builder::new()
        .name("engine-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match parse_line(trimmed) {
                            Some(Line::State { state, rows }) => client.on_state(&state, rows),
                            Some(Line::Position(heartbeat)) => client.on_position(heartbeat),
                            Some(Line::Volume(volume)) => client.on_volume(volume),
                            Some(Line::Reply {
                                request_id,
                                ok,
                                error,
                                data,
                            }) => {
                                client.deliver(&request_id, EngineReply { ok, error, data, ..EngineReply::default() });
                            }
                            None => {} // unparseable line: logged, dropped
                        }
                    }
                    Err(_) => break,
                }
            }
            client.on_eof();
        })
        .expect("could not start engine reader thread");
}

/// What one engine output line means. `state`, `position` and `volume` lines
/// are fanned out; every other line is a reply to a pending request.
#[derive(Debug, PartialEq)]
enum Line {
    State {
        state: PlaybackState,
        /// Whether the payload carried its queue rows; see [`PayloadRows`].
        rows: PayloadRows,
    },
    Position(PositionHeartbeat),
    Volume(u8),
    Reply {
        request_id: String,
        ok: bool,
        error: Option<String>,
        data: Option<ReplyData>,
    },
}

/// The engine's scalar volume line. One number, so parsing it never costs a
/// queue parse — which is the whole point of the lane.
#[derive(Debug, serde::Deserialize)]
struct VolumeLine {
    volume: u8,
}

/// A reply's routing fields, as tolerant as routing has always been: a
/// request id or error that is not a string reads as none, and only a literal
/// `true` is success. `data` is kept whenever the key is present, `null`
/// included, because that is a payload some browses answer with — kept as
/// its text, which the requester parses once into its own type.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct ReplyFields {
    request_id: Value,
    ok: Value,
    error: Value,
    #[serde(deserialize_with = "present_data")]
    data: Option<ReplyData>,
}

fn present_data<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<ReplyData>, D::Error> {
    Box::<serde_json::value::RawValue>::deserialize(deserializer).map(|raw| Some(ReplyData(raw)))
}

/// Classifies and parses one protocol line in a single pass over its text.
///
/// The engine writes `type` first, so the rest of the line streams straight
/// into its typed shape: a ten-thousand-row state is read once, into tracks,
/// never into a `Value` tree that is then walked a second time. Keys that
/// arrive ahead of `type` are held as values and replayed, so key order can
/// change the cost but never the result. Scalar lanes are parsed into their
/// own shapes only — never into a [`PlaybackState`], so a heartbeat or a
/// volume step can never cost a queue parse. A malformed state, position or
/// volume line is logged and dropped (`None`); text that is not a JSON object
/// was never routable and is dropped silently.
fn parse_line(line: &str) -> Option<Line> {
    let mut lane = None;
    let mut deserializer = serde_json::Deserializer::from_str(line);
    let parsed = LineSeed { lane: &mut lane }
        .deserialize(&mut deserializer)
        .and_then(|parsed| deserializer.end().map(|()| parsed));
    match parsed {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            if let Some(lane) = lane {
                log::error(&format!("could not parse engine {lane} line: {error}"));
            }
            None
        }
    }
}

/// Parses one line, recording which lane it turned out to be so a failure
/// can be reported against it.
struct LineSeed<'a> {
    lane: &'a mut Option<&'static str>,
}

impl<'de> DeserializeSeed<'de> for LineSeed<'_> {
    type Value = Line;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Line, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for LineSeed<'_> {
    type Value = Line;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("an engine protocol object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Line, A::Error> {
        let mut ahead = Vec::new();
        let mut kind = Value::Null;
        while let Some(WireKey(key)) = map.next_key()? {
            if key == "type" {
                kind = map.next_value()?;
                break;
            }
            ahead.push((key, map.next_value::<Value>()?));
        }
        let mut fields = LineFields {
            ahead: ahead.into_iter(),
            pending: None,
            map,
            done: false,
            queue: false,
            upcoming: false,
        };
        match kind.as_str() {
            Some("state") => {
                *self.lane = Some("state");
                let state = PlaybackState::deserialize(MapAccessDeserializer::new(&mut fields))?;
                let rows = match (fields.queue, fields.upcoming) {
                    (true, true) => PayloadRows::Sent,
                    (true, false) => PayloadRows::QueueOnly,
                    (false, true) => PayloadRows::OrderOnly,
                    (false, false) => PayloadRows::Omitted,
                };
                Ok(Line::State { state, rows })
            }
            Some("position") => {
                *self.lane = Some("position");
                PositionHeartbeat::deserialize(MapAccessDeserializer::new(&mut fields)).map(Line::Position)
            }
            Some("volume") => {
                *self.lane = Some("volume");
                VolumeLine::deserialize(MapAccessDeserializer::new(&mut fields))
                    .map(|line| Line::Volume(line.volume))
            }
            _ => {
                *self.lane = Some("reply");
                let reply = ReplyFields::deserialize(MapAccessDeserializer::new(&mut fields))?;
                Ok(Line::Reply {
                    request_id: match reply.request_id {
                        Value::String(request_id) => request_id,
                        _ => String::new(),
                    },
                    ok: reply.ok == Value::Bool(true),
                    error: match reply.error {
                        Value::String(error) => Some(error),
                        _ => None,
                    },
                    data: reply.data,
                })
            }
        }
    }
}

/// An object key, borrowed from the line unless it had to be unescaped.
struct WireKey<'de>(Cow<'de, str>);

impl<'de> Deserialize<'de> for WireKey<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = WireKey<'de>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object key")
            }
            fn visit_borrowed_str<E>(self, key: &'de str) -> Result<Self::Value, E> {
                Ok(WireKey(Cow::Borrowed(key)))
            }
            fn visit_str<E>(self, key: &str) -> Result<Self::Value, E> {
                Ok(WireKey(Cow::Owned(key.to_owned())))
            }
            fn visit_string<E>(self, key: String) -> Result<Self::Value, E> {
                Ok(WireKey(Cow::Owned(key)))
            }
        }
        deserializer.deserialize_str(KeyVisitor)
    }
}

/// The fields after `type`: any held ahead of it first, then the rest of the
/// line as it streams. Notes whether the `queue` and `upcoming` keys were
/// present at all — omission is a delta, an empty array is data.
struct LineFields<'de, A> {
    ahead: std::vec::IntoIter<(Cow<'de, str>, Value)>,
    pending: Option<Value>,
    map: A,
    done: bool,
    queue: bool,
    upcoming: bool,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for LineFields<'de, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, A::Error> {
        let key = match self.ahead.next() {
            Some((key, value)) => {
                self.pending = Some(value);
                key
            }
            None if self.done => return Ok(None),
            None => match self.map.next_key::<WireKey<'de>>()? {
                Some(WireKey(key)) => key,
                None => {
                    self.done = true;
                    return Ok(None);
                }
            },
        };
        match &*key {
            "queue" => self.queue = true,
            "upcoming" => self.upcoming = true,
            _ => {}
        }
        let key: CowStrDeserializer<'_, A::Error> = key.into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, A::Error> {
        match self.pending.take() {
            Some(value) => seed.deserialize(value).map_err(serde::de::Error::custom),
            None => self.map.next_value_seed(seed),
        }
    }
}

/// One request line: the id and the command, then the command's own fields
/// at the same level.
#[derive(Serialize)]
struct RequestLine<'a, A> {
    request_id: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(flatten)]
    args: A,
}

/// A queue replacement, `play_queue` or `restore_queue`, serialized straight
/// from the borrowed rows rather than through a `Value` copy of all of them.
/// The optional fields are the restore-only ones and stay off the line when
/// unset, exactly as the engine defaults them.
#[derive(Default, Serialize)]
struct QueueRequest<'a> {
    queue: &'a [Track],
    index: usize,
    position_ms: u32,
    context: &'a str,
    automatic_start: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    start_paused: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    only_if_preview: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_lease_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resume_playing: Option<bool>,
}

#[derive(Serialize)]
struct TracksRequest<'a> {
    tracks: &'a [Track],
    context: &'a str,
}

/// Encodes one request line, refusing one the engine would not read. The
/// engine's answer to an oversized line carries no request id, so sending it
/// would only leave the caller waiting out its timeout with nothing to say.
fn request_line(request_id: &str, kind: &str, args: impl Serialize) -> Result<String, String> {
    let line = serde_json::to_string(&RequestLine { request_id, kind, args })
        .map_err(|error| format!("could not encode the {kind} request: {error}"))?;
    // The writer appends a newline, and the engine counts it.
    let bytes = line.len() + 1;
    if bytes > MAX_REQUEST_BYTES {
        return Err(oversized_request_error(kind, bytes));
    }
    Ok(line)
}

fn oversized_request_error(kind: &str, bytes: usize) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let what = match kind {
        "play_queue" | "restore_queue" => "This queue is",
        "add_queue_batch" => "These tracks are",
        _ => "This request is",
    };
    format!(
        "{what} too large for the playback engine: {:.1} MB, over its {} MB limit.",
        bytes as f64 / MIB,
        MAX_REQUEST_BYTES / (1024 * 1024),
    )
}

/// Reads a reply payload once, straight into its type, then withdraws the
/// download marks the fence says it may no longer claim.
fn parse_data<T: serde::de::DeserializeOwned + CachedClaims>(reply: EngineReply, kind: &str) -> Result<T, String> {
    let data = reply
        .data
        .ok_or_else(|| format!("{kind} reply carried no payload"))?;
    let mut payload: T = serde_json::from_str(data.json())
        .map_err(|error| format!("unexpected {kind} payload: {error}"))?;
    reply.fence.apply(&mut payload);
    Ok(payload)
}

/// Release builds use only the bundled resource (or Windows sibling).
/// Development builds may override it or use the workspace release binary.
fn locate_engine() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("SPOTIFY_ENGINE_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // macOS resources live in <app>.app/Contents/Resources, while the app
    // executable lives in Contents/MacOS. Keep the engine in the signed bundle.
    #[cfg(target_os = "macos")]
    {
        let bundled = exe_dir.parent()?.join("Resources").join(ENGINE_BINARY);
        if bundled.is_file() {
            return Some(bundled);
        }
    }
    let sibling = exe_dir.join(ENGINE_BINARY);
    if sibling.is_file() {
        return Some(sibling);
    }
    // In development the shell runs from target/debug, but the bundler builds
    // its engine resource into the shared workspace target/release directory.
    #[cfg(debug_assertions)]
    {
        let workspace_release = exe_dir.parent()?.join("release").join(ENGINE_BINARY);
        if workspace_release.is_file() { return Some(workspace_release); }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_line(request_id: &str, kind: &str, args: impl Serialize) -> String {
        request_line(request_id, kind, args).expect("request encodes")
    }

    /// `json!` objects serialize with sorted keys, which puts `type` late:
    /// these lines take the held-ahead path, the engine's own the streaming one.
    fn parse_value(value: Value) -> Option<Line> {
        parse_line(&value.to_string())
    }

    #[test]
    fn exit_waits_for_final_flush_and_freezes_the_snapshot_before_writer_acknowledgement() {
        let initial = PlaybackState { auth_state: "ready".into(), position_ms: 12_345,
            current_index: Some(0), current_uri: "spotify:track:a".into(),
            queue: vec![Track { id: "a".into(), uri: "spotify:track:a".into(), ..Track::default() }].into(),
            ..PlaybackState::default() };
        let mut client = client_with_last_state(initial);
        let (persist_tx, persist_rx) = mpsc::sync_channel(1);
        Arc::get_mut(&mut client).unwrap().persist_tx = persist_tx;
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let (flush_tx, flush_rx) = mpsc::channel();
        let weak = Arc::downgrade(&client);
        let writer = std::thread::spawn(move || {
            let PersistCommand::Shutdown { result_tx, .. } = persist_rx.recv().unwrap()
                else { panic!("exit must request the unthrottled final flush") };
            let client = weak.upgrade().unwrap();
            client.on_position(PositionHeartbeat { position_ms: 99_999, duration_ms: 180_000 });
            snapshot_tx.send(client.playback_snapshot_for_shutdown().unwrap()).unwrap();
            flush_rx.recv().unwrap();
            result_tx.send(Ok(())).unwrap();
        });
        *client.persist_thread.lock() = Some(writer);
        let (returned_tx, returned_rx) = mpsc::channel();
        let shutdown = client.clone();
        let exit = std::thread::spawn(move || {
            shutdown.shutdown_engine();
            returned_tx.send(()).unwrap();
        });
        let snapshot = snapshot_rx.recv().unwrap();
        assert_eq!(snapshot.position_ms, 12_345);
        assert_eq!(snapshot.queue[0].id, "a");
        assert!(matches!(returned_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        flush_tx.send(()).unwrap();
        returned_rx.recv().unwrap();
        exit.join().unwrap();
        // The normal Exit event may follow the updater hook. It must be a
        // completed no-op rather than requesting a second writer shutdown.
        client.shutdown_engine();
        assert!(client.persist_thread.lock().is_none());
    }


    #[test]
    fn cached_deltas_mark_retained_and_later_rows_without_replacing_queue_identity() {
        let initial = PlaybackState { auth_state: "ready".into(), queue_revision: 11, order_revision: 21,
            queue: vec![Track { id: "a".into(), ..Track::default() }].into(), ..PlaybackState::default() };
        let client = client_with_last_state(initial.clone());
        let Some(Line::State { state: delta, rows }) = parse_value(json!({
            "type": "state", "auth_state": "ready", "queue_revision": 11, "order_revision": 21,
            "cached_ids": ["a", "detail-only"]
        })) else { panic!("cache delta did not parse") };
        client.on_state(&delta, rows);
        let marked = client.current_state().unwrap();
        assert!(marked.queue[0].cached);
        let event = serde_json::to_value(crate::types::PlaybackEvent::new(&marked, false, false)).unwrap();
        assert!(event.get("queue").is_none());
        assert_eq!(event["cached_ids"], json!(["a", "detail-only"]));
        client.on_state(&PlaybackState { cached_ids: Some(Vec::new()), queue: Vec::new().into(), ..initial.clone() },
            PayloadRows::Omitted);
        assert!(client.current_state().unwrap().queue[0].cached);
        for raw in [Value::Null, json!(false), json!({})] {
            let Some(Line::State { state, rows }) = parse_value(json!({
                "type": "state", "auth_state": "ready", "queue_revision": 11,
                "order_revision": 21, "cached_ids": raw
            })) else { panic!("non-array marks must behave as omission") };
            client.on_state(&state, rows);
            assert!(client.current_state().unwrap().queue[0].cached);
        }
        client.on_state(&PlaybackState { queue_revision: 12, order_revision: 22,
            queue: vec![Track { id: "detail-only".into(), ..Track::default() }].into(), ..initial.clone() },
            PayloadRows::Sent);
        assert!(client.current_state().unwrap().queue[0].cached);
        client.clear_cache_marks();
        client.on_state(&PlaybackState { queue: Vec::new().into(), queue_revision: 12, order_revision: 22, ..initial },
            PayloadRows::Omitted);
        assert!(!client.current_state().unwrap().queue[0].cached);
    }

    #[test]
    fn unresolved_queue_delta_still_delivers_off_queue_cache_additions() {
        let initial = PlaybackState { auth_state: "ready".into(), queue_revision: 1, order_revision: 1,
            queue: vec![Track { id: "a".into(), ..Track::default() }].into(), ..PlaybackState::default() };
        let client = client_with_last_state(initial.clone());
        let mut events = client.subscribe_lines();
        client.on_state(&PlaybackState { queue_revision: 2, cached_ids: Some(vec!["detail-only".into()]),
            queue: Vec::new().into(), ..initial.clone() }, PayloadRows::Omitted);
        assert_eq!(client.current_state().unwrap().queue, initial.queue);
        let StateLine::CachedIds(ids) = events.try_recv().unwrap() else { panic!("cache marks were held behind queue") };
        assert_eq!(ids, ["detail-only"]);
        client.on_state(&PlaybackState { queue_revision: 2, order_revision: 2,
            queue: vec![Track { id: "detail-only".into(), ..Track::default() }].into(), ..initial },
            PayloadRows::Sent);
        assert!(client.current_state().unwrap().queue[0].cached);
    }
    #[test]
    fn order_only_deltas_advance_duplicate_occurrences_without_erasing_rows() {
        let rows = vec![
            Track { id: "same".into(), uri: "spotify:track:same".into(), ..Track::default() },
            Track { id: "same".into(), uri: "spotify:track:same".into(), ..Track::default() },
        ];
        let initial = PlaybackState { auth_state: "ready".into(), queue: rows.clone().into(),
            queue_revision: 11, order_revision: 21, upcoming: vec![1], current_index: Some(0), ..PlaybackState::default() };
        let client = client_with_last_state(initial.clone());
        let update = PlaybackState { queue: Vec::new().into(), upcoming: Vec::new(), order_revision: 22,
            current_index: Some(1), position_ms: 500, ..initial };
        client.on_state(&update, PayloadRows::OrderOnly);
        let state = client.current_state().unwrap();
        assert_eq!(*state.queue, rows[..]);
        assert_eq!(state.current_index, Some(1));
        assert!(state.upcoming.is_empty());
        let heartbeat = PlaybackState { queue: Vec::new().into(), position_ms: 1000, ..state };
        client.on_state(&heartbeat, PayloadRows::Omitted);
        let state = client.current_state().unwrap();
        assert_eq!(*state.queue, rows[..]);
        assert!(state.upcoming.is_empty());
        assert_eq!(state.position_ms, 1000);
    }

    /// An `EngineClient` with no live engine process, for unit tests of the
    /// reader-side callbacks.
    fn client_with_last_state(state: PlaybackState) -> Arc<EngineClient> {
        let (state_tx, _) = tokio::sync::broadcast::channel(64);
        let (exit_tx, _) = tokio::sync::watch::channel(false);
        let (persist_tx, _persist_rx) = mpsc::sync_channel(1);
        // The state stands for a full payload this client has already seen, so
        // it also seeds the wire base: without it nothing could merge a later
        // payload that omits its rows, exactly as in production, where the
        // base is installed by the first payload that carries them.
        let mut delta_base = DeltaBase::default();
        delta_base.adopt(&state, PayloadRows::Sent);
        Arc::new(EngineClient {
            state_dir: PathBuf::new(),
            pending: Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            process: Mutex::new(None),
            state_tx,
            exit_tx,
            observed_state: Mutex::new(Some(state.clone())),
            has_authenticated_state: AtomicBool::new(state.auth_state == "ready"),
            cache_generation: AtomicU64::new(0),
            account_generation: AtomicU64::new(0),
            account_identity: Mutex::new((state.auth_state == "ready").then(|| state.username.clone())),
            last_state: Mutex::new(Some(state)),
            delta_base: Mutex::new(delta_base),
            preview_active: AtomicBool::new(false),
            restore_pending: Mutex::new(None),
            persist_tx,
            restore_generation: AtomicU64::new(0),
            persist_generation: AtomicU64::new(0),
            queue_generation: AtomicU64::new(0),
            persist_thread: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            process_generation: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            shutdown_lock: Mutex::new(()),
            timing: Timing::PRODUCTION,
        })
    }

    /// Production's deadlines and backoff in milliseconds. The command
    /// deadline is the one these tests wait out; browse stays generous
    /// because a fresh PowerShell child has to start up inside it.
    const FAST: Timing = Timing {
        command: Duration::from_millis(250),
        browse: Duration::from_secs(10),
        waveform: Duration::from_secs(10),
        respawn: RespawnPolicy {
            start: Duration::from_millis(100),
            max: Duration::from_secs(10),
            stable_uptime: Duration::from_secs(30),
        },
    };

    fn fast_client(state: PlaybackState) -> Arc<EngineClient> {
        let mut client = client_with_last_state(state);
        Arc::get_mut(&mut client).expect("a fresh client is unshared").timing = FAST;
        client
    }

    fn boundary_state() -> PlaybackState {
        PlaybackState { auth_state: "ready".into(), playing: true, current_index: Some(0),
            position_ms: 180_000, duration_ms: 180_000, queue_revision: 7, order_revision: 7,
            queue: vec![Track { id: "a".into(), uri: "spotify:track:a".into(), duration_ms: 180_000,
                cached: true, ..Default::default() }].into(), ..Default::default() }
    }

    #[test]
    fn successful_restore_barrier_accepts_end_restart_and_concurrent_transport_changes() {
        for concurrent_pause in [false, true] {
            let original = boundary_state();
            let client = client_with_last_state(original.clone());
            client.on_eof();
            let generation = client.restore_generation.load(Ordering::Acquire);
            client.begin_pending_retry(generation).unwrap();
            let mut actual = original;
            actual.position_ms = 0;
            actual.playing = !concurrent_pause;
            actual.queue_revision = 8;
            actual.order_revision = 8;
            client.on_state(&actual, PayloadRows::Sent);
            assert!(client.restore_is_pending());
            assert!(client.complete_pending_restore(generation).is_some());
            assert!(!client.restore_is_pending());
            assert_eq!(client.current_state().unwrap().position_ms, 0);
            assert_eq!(client.current_state().unwrap().playing, !concurrent_pause);
        }
    }

    #[test]
    fn reconnect_and_error_crashes_keep_queue_but_explicit_logout_cannot_resurrect_it() {
        for auth in ["authenticating", "error"] {
            let original = boundary_state();
            let client = client_with_last_state(original.clone());
            let mut unavailable = original;
            unavailable.auth_state = auth.into();
            client.on_state(&unavailable, PayloadRows::Sent);
            assert_eq!(client.playback_snapshot_for_shutdown().unwrap().queue[0].id, "a");
            client.on_eof();
            let pending = client.restore_pending.lock().clone().unwrap();
            assert_eq!(pending.snapshot.queue[0].id, "a");
            assert!(pending.snapshot.resume_playing);
            client.clear_session_restore();
            client.on_eof();
            assert!(!client.restore_is_pending());
            assert!(client.playback_snapshot_for_shutdown().is_none());
            assert!(client.current_state().is_none());
        }
    }

    #[test]
    fn replacement_authentication_crash_keeps_original_plan_and_watch_latch_resets_without_receivers() {
        let client = client_with_last_state(boundary_state());
        client.on_eof();
        let mut blank = PlaybackState::default();
        blank.auth_state = "authenticating".into();
        client.on_state(&blank, PayloadRows::Sent);
        client.on_eof();
        assert_eq!(client.restore_pending.lock().as_ref().unwrap().snapshot.queue[0].id, "a");
        assert!(*client.exit_tx.subscribe().borrow());
        client.exit_tx.send_replace(false);
        let receiver = client.exit_tx.subscribe();
        assert!(!*receiver.borrow());
    }

    #[test]
    fn cache_removal_reset_and_stale_browse_claims_cannot_revive_old_downloads() {
        let client = client_with_last_state(boundary_state());
        let generation = client.cache_generation();
        let mut removal = boundary_state();
        removal.uncached_ids = Some(vec!["a".into()]);
        client.on_state(&removal, PayloadRows::Sent);
        assert!(!client.current_state().unwrap().queue[0].cached);
        let mut stale_rows = vec![Track { id: "a".into(), cached: true, ..Default::default() }];
        client.reconcile_cached_rows(&mut stale_rows, generation);
        assert!(!stale_rows[0].cached);
        removal.uncached_ids = None;
        removal.cached_ids = Some(vec!["a".into()]);
        client.on_state(&removal, PayloadRows::Sent);
        assert!(client.current_state().unwrap().queue[0].cached);
        client.on_eof();
        let mut fresh = boundary_state();
        Arc::make_mut(&mut fresh.queue)[0].cached = false;
        client.on_state(&fresh, PayloadRows::Sent);
        assert!(!client.delta_base.lock().queue[0].cached);
        client.reconcile_cached_rows(&mut stale_rows, generation);
        assert!(!stale_rows[0].cached);
        // A reset between request and reply withdraws every claimed row; a
        // later removal, only its own.
        let reply = |fence| EngineReply { ok: true, data: Some(test_data(json!({"tracks": [
            {"id": "a", "cached": true}, {"id": "b", "cached": true}]}))), fence, ..EngineReply::default() };
        let stale = MarkFence { stale: true, ..MarkFence::default() };
        assert!(rows(reply(stale)).iter().all(|row| !row.cached));
        let removed = MarkFence { invalidated: HashSet::from(["a".to_owned()]), ..MarkFence::default() };
        assert_eq!(rows(reply(removed)).iter().map(|row| row.cached).collect::<Vec<_>>(), [false, true]);
        assert!(rows(reply(MarkFence::default())).iter().all(|row| row.cached));
    }

    /// A playlist read parses once, into the variant it is. A changed read's
    /// rows are fenced by the reply; an unchanged read's marks are ids, fenced
    /// by the shell against the same evidence when it adopts them.
    #[test]
    fn a_playlist_read_parses_into_its_variant_with_both_kinds_of_marks_fenced() {
        use renderer_engine::protocol::PlaylistRead;
        let changed = test_data(json!({"changed": {"id": "p", "uri": "spotify:playlist:p", "name": "P",
            "owner_id": "o", "owner_name": "O", "tracks_key": "k",
            "tracks": [{"id": "a", "cached": true}, {"id": "b", "cached": true}]}}));
        let fence = MarkFence { invalidated: HashSet::from(["a".to_owned()]), ..MarkFence::default() };
        let PlaylistRead::Changed(browse) = parse_data::<PlaylistRead>(
            EngineReply { ok: true, data: Some(changed), fence, ..EngineReply::default() }, "browse_playlist",
        ).unwrap() else { panic!("a changed read is a changed read") };
        assert_eq!(browse.tracks.iter().map(|row| row.cached).collect::<Vec<_>>(), [false, true]);
        assert_eq!(browse.tracks_key.as_deref(), Some("k"));

        let unchanged = test_data(json!({"unchanged": {"cached_ids": ["a", "b"], "excluded_track_ids": ["c"]}}));
        let PlaylistRead::Unchanged(read) = parse_data::<PlaylistRead>(
            EngineReply { ok: true, data: Some(unchanged), ..EngineReply::default() }, "browse_playlist",
        ).unwrap() else { panic!("an unchanged read is an unchanged read") };
        assert_eq!(read.excluded_track_ids, ["c"]);
        let client = client_with_last_state(boundary_state());
        let generation = client.cache_generation();
        let mut removal = boundary_state();
        removal.uncached_ids = Some(vec!["a".into()]);
        client.on_state(&removal, PayloadRows::Sent);
        let mut ids = read.cached_ids.clone();
        client.reconcile_cached_ids(&mut ids, generation);
        assert_eq!(ids, ["b"], "a removal since withdraws its own mark");
        client.clear_cache_marks();
        let mut ids = read.cached_ids;
        client.reconcile_cached_ids(&mut ids, generation);
        assert!(ids.is_empty(), "a reset since withdraws them all");
    }

    /// A test reply's payload, as the reader keeps it.
    fn test_data(value: Value) -> ReplyData {
        ReplyData(serde_json::value::RawValue::from_string(value.to_string()).unwrap())
    }

    fn reply_value(data: &ReplyData) -> Value {
        serde_json::from_str(data.json()).unwrap()
    }

    /// A browse-shaped test payload: just its rows.
    #[derive(serde::Deserialize)]
    struct Rows {
        tracks: Vec<renderer_engine::protocol::TrackRef>,
    }

    impl CachedClaims for Rows {
        fn claims(&mut self, visit: &mut dyn FnMut(&mut renderer_engine::protocol::TrackRef)) {
            for track in &mut self.tracks { visit(track) }
        }
    }

    /// The rows of a reply, read the way `parse_data` reads every payload:
    /// once, typed, with the reply's fence applied.
    fn rows(reply: EngineReply) -> Vec<renderer_engine::protocol::TrackRef> {
        parse_data::<Rows>(reply, "browse_playlist").unwrap().tracks
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fake_engine_pipe_fences_browse_marks_invalidated_before_reply() {
        use std::os::windows::process::CommandExt;
        let client = client_with_last_state(boundary_state());
        let mut lines = client.subscribe_lines();
        // One isolated line-JSON subprocess, with no state directory, account,
        // network, audio device or owner's app involved.
        let script = r#"
$ErrorActionPreference = 'Stop'
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine('{"type":"state","auth_state":"ready","queue_revision":7,"order_revision":7,"uncached_ids":["a"]}')
[Console]::WriteLine((@{type='browse_playlist'; request_id=$request.request_id; ok=$true; data=@{tracks=@(@{id='a'; cached=$true})}} | ConvertTo-Json -Depth 5 -Compress))
"#;
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            // PowerShell's Console API needs initialized redirected handles;
            // hide its window without detaching it from those handles.
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn().unwrap();
        *client.stdin.lock() = child.stdin.take().map(|pipe| spawn_input_writer(pipe, 0));
        spawn_reader(child.stdout.take().unwrap(), &client);
        let reply = client.request("browse_playlist", json!({"id": "temporary"})).await.unwrap();
        assert!(!rows(reply)[0].cached);
        let first = lines.recv().await.unwrap();
        assert!(matches!(first, StateLine::State(state) if state.uncached_ids == Some(vec!["a".into()])
            && !state.queue[0].cached));
        assert!(child.wait().unwrap().success());
        loop {
            if matches!(lines.recv().await.unwrap(), StateLine::CacheMarksReset) { break; }
        }
        assert!(!client.current_state().unwrap().queue[0].cached);
    }

    #[cfg(windows)]
    fn start_fake_child(client: &Arc<EngineClient>, script: &str) {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            .creation_flags(0x0800_0000);
        client.exit_tx.send_replace(false);
        client.spawn_child(&mut command).unwrap();
    }

    /// A real owned child that exits at once. `cmd` starts in a fraction of
    /// PowerShell's time, and a child that only has to end needs nothing more.
    #[cfg(windows)]
    fn start_exiting_child(client: &Arc<EngineClient>) {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", "exit 0"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            .creation_flags(0x0800_0000);
        client.exit_tx.send_replace(false);
        client.spawn_child(&mut command).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_backs_off_every_early_owned_child_eof_and_shutdown_cancels_sleep() {
        let client = fast_client(boundary_state());
        let policy = client.timing.respawn;
        start_exiting_child(&client);
        let mut startup_exit = client.exit_tx.subscribe();
        tokio::time::timeout(Duration::from_secs(10), startup_exit.wait_for(|value| *value)).await.unwrap().unwrap();
        let startup_eof = Instant::now();
        let (admitted, mut admissions) = tokio::sync::mpsc::unbounded_channel();
        let supervised = {
            let client = client.clone();
            tokio::spawn(client.supervise_with_spawn(move |client| {
                start_exiting_child(client);
                // Own a real child, and make EOF deterministic before returning
                // from admission: status cannot successfully observe this child.
                let mut exited = client.exit_tx.subscribe();
                std::thread::spawn(move || {
                    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
                        .block_on(async {
                            tokio::time::timeout(Duration::from_secs(10), exited.wait_for(|value| *value))
                                .await.unwrap().unwrap();
                        });
                }).join().unwrap();
                assert!(client.process.lock().is_none());
                admitted.send(Instant::now()).unwrap();
                Ok(())
            }))
        };
        let first = tokio::time::timeout(Duration::from_secs(15), admissions.recv()).await.unwrap().unwrap();
        let second = tokio::time::timeout(Duration::from_secs(15), admissions.recv()).await.unwrap().unwrap();
        assert!(first.duration_since(startup_eof) >= policy.start);
        assert!(second.duration_since(first) >= policy.next(policy.start));
        // The next sleep is four times the first, but shutdown must end it now.
        let pending = policy.next(policy.next(policy.start));
        let shutdown = Instant::now();
        client.shutdown_engine();
        tokio::time::timeout(Duration::from_secs(2), supervised).await.unwrap().unwrap();
        assert!(shutdown.elapsed() < pending, "shutdown waited out the backoff instead of ending it");
        assert!(admissions.recv().await.is_none(), "shutdown must not admit another child");
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_shutdown_wakes_live_child_exit_watch_and_spawn_failure_backoff() {
        // Production deadlines: the status below waits on PowerShell starting.
        let client = client_with_last_state(boundary_state());
        start_fake_child(&client, r#"
while ($line = [Console]::ReadLine()) {
    $request = $line | ConvertFrom-Json
    if ($request.type -eq 'shutdown') { break }
    [Console]::WriteLine((@{request_id=$request.request_id; ok=$true} | ConvertTo-Json -Compress))
}
"#);
        let supervised = tokio::spawn(client.clone().supervise_with_spawn(|_| {
            panic!("shutdown may not replace an admitted live child")
        }));
        // Starting status on the real pipe ensures the child is responsive;
        // the supervisor is waiting on its exit latch, not on a fake condition.
        client.status().await.unwrap();
        client.shutdown_engine();
        tokio::time::timeout(Duration::from_secs(2), supervised).await.unwrap().unwrap();

        let client = fast_client(boundary_state());
        let (attempted, mut attempts) = tokio::sync::mpsc::unbounded_channel();
        let supervised = tokio::spawn(client.clone().supervise_with_spawn(move |_| {
            attempted.send(Instant::now()).unwrap();
            Err("isolated spawn failure".into())
        }));
        let first = attempts.recv().await.unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), attempts.recv()).await.unwrap().unwrap();
        assert!(second.duration_since(first) >= client.timing.respawn.start);
        client.shutdown_engine();
        tokio::time::timeout(Duration::from_secs(2), supervised).await.unwrap().unwrap();
        assert!(attempts.recv().await.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_racing_real_child_admission_never_leaves_a_late_process_alive() {
        use std::os::windows::process::CommandExt;
        for _ in 0..4 {
            let client = client_with_last_state(boundary_state());
            let gate = Arc::new(std::sync::Barrier::new(2));
            let launched = {
                let client = client.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    let mut command = Command::new("powershell.exe");
                    command.args(["-NoProfile", "-NonInteractive", "-Command",
                        "[Console]::ReadLine() | Out-Null"])
                        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
                        .creation_flags(0x0800_0000);
                    gate.wait();
                    client.spawn_child(&mut command)
                })
            };
            gate.wait();
            client.shutdown_engine();
            if let Ok(pid) = launched.join().unwrap() {
                // Observe the OS process boundary, not merely the cached handle.
                let query = format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }}");
                assert!(Command::new("powershell.exe")
                    .args(["-NoProfile", "-NonInteractive", "-Command", &query])
                    .creation_flags(0x0800_0000).status().unwrap().success(),
                    "a child published after shutdown survived");
            }
        }
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reader_account_departure_rejects_old_browse_before_shell_consumes_state() {
        for auth in ["ready", "logged_out", "needs_login", "authenticating"] {
            let state = PlaybackState { username: "account-one".into(), ..boundary_state() };
            let client = client_with_last_state(state);
            let identity = if auth == "ready" { "account-two" } else { "account-one" };
            let line = json!({"type": "state", "auth_state": auth, "username": identity,
                "queue_revision": 7, "order_revision": 7}).to_string();
            let script = format!(r#"
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine('{line}')
[Console]::WriteLine((@{{request_id=$request.request_id; ok=$true; data=@{{tracks=@(@{{id='old-account-row'}})}}}} | ConvertTo-Json -Depth 5 -Compress))
[Console]::ReadLine() | Out-Null
"#);
            start_fake_child(&client, &script);
            // No shell consumer subscribes: the reader itself owns this fence.
            let result = client.request("browse_playlist", json!({"id": "temporary"})).await;
            if auth == "authenticating" {
                assert!(result.is_ok(), "same-account reconnect keeps its namespace");
            } else {
                assert!(result.is_err());
            }
            client.retire_process(client.process_generation.load(Ordering::Acquire));
        }
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clear_ack_resets_before_fresh_cache_evidence_and_failed_clear_keeps_marks() {
        for ok in [true, false] {
            let client = client_with_last_state(boundary_state());
            let mut lines = client.subscribe_lines();
            let script = format!(r#"
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine((@{{request_id=$request.request_id; ok=${ok}; error='clear failed'}} | ConvertTo-Json -Compress))
[Console]::WriteLine('{{"type":"state","auth_state":"ready","queue_revision":7,"order_revision":7,"cached_ids":["b"]}}')
[Console]::WriteLine('{{"type":"volume","volume":52}}')
[Console]::ReadLine() | Out-Null
"#);
            start_fake_child(&client, &script);
            let clear = {
                let client = client.clone();
                tokio::spawn(async move { client.clear_audio_cache().await })
            };
            // Deliberately observe the immediate post-ACK positives before
            // resuming the caller's continuation.
            let mut reset_seen = false;
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match lines.recv().await.unwrap() {
                        StateLine::CacheMarksReset => reset_seen = true,
                        StateLine::Volume(52) => break,
                        _ => {}
                    }
                }
            }).await.unwrap();
            assert_eq!(clear.await.unwrap().is_ok(), ok);
            assert_eq!(reset_seen, ok, "failed clears must not erase valid badges");
            let mut browsed = vec![Track { id: "b".into(), ..Default::default() }];
            client.reconcile_cached_rows(&mut browsed, client.cache_generation());
            assert!(browsed[0].cached, "a fresh post-clear download survives the late awaiter");
            assert_eq!(client.current_state().unwrap().queue[0].cached, !ok,
                "only acknowledged clear retires earlier queue badges");
            client.retire_process(client.process_generation.load(Ordering::Acquire));
        }
    }

    struct IsolatedChild(Arc<EngineClient>);

    impl Drop for IsolatedChild {
        fn drop(&mut self) { self.0.shutdown_engine(); }
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_written_audio_clear_waits_past_the_command_deadline_for_its_real_ack() {
        let client = fast_client(boundary_state());
        let _child = IsolatedChild(client.clone());
        let mut lines = client.subscribe_lines();
        start_fake_child(&client, r#"
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine('{"type":"volume","volume":61}')
[Console]::ReadLine() | Out-Null
[Console]::WriteLine((@{request_id=$request.request_id; ok=$true} | ConvertTo-Json -Compress))
[Console]::ReadLine() | Out-Null
"#);
        let mut clear = {
            let client = client.clone();
            tokio::spawn(async move { client.clear_audio_cache().await })
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if matches!(lines.recv().await.unwrap(), StateLine::Volume(61)) { break; }
            }
        }).await.unwrap();
        // The child has received the request but is still deleting. The
        // ordinary command deadline cannot release cache-operation ownership.
        let past_deadline = client.timing.command + Duration::from_millis(250);
        assert!(tokio::time::timeout(past_deadline, &mut clear).await.is_err());
        assert_eq!(client.pending.lock().len(), 1);
        assert!(client.current_state().unwrap().queue[0].cached, "no success has been acknowledged");
        assert!(client.process.lock().is_some(), "slow disk work is not a crashed child");
        let release = build_line(&client.next_request_id(), "status", Value::Null);
        client.write_line(release).unwrap().1.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(10), clear).await.unwrap().unwrap().unwrap();
        assert!(client.pending.lock().is_empty());
        assert!(!client.current_state().unwrap().queue[0].cached, "only the terminal success resets marks");
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_audio_clear_ending_at_child_eof_fails_and_releases_its_pending_owner() {
        let client = client_with_last_state(boundary_state());
        let _child = IsolatedChild(client.clone());
        let mut lines = client.subscribe_lines();
        let mut exited = client.exit_tx.subscribe();
        start_fake_child(&client, r#"
[Console]::ReadLine() | Out-Null
[Console]::WriteLine('{"type":"volume","volume":63}')
[Console]::ReadLine() | Out-Null
exit 0
"#);
        let clear = {
            let client = client.clone();
            tokio::spawn(async move { client.clear_audio_cache().await })
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if matches!(lines.recv().await.unwrap(), StateLine::Volume(63)) { break; }
            }
        }).await.unwrap();
        assert_eq!(client.pending.lock().len(), 1);
        let release = build_line(&client.next_request_id(), "status", Value::Null);
        client.write_line(release).unwrap().1.await.unwrap().unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(10), clear).await.unwrap().unwrap().is_err(),
            "EOF is not a successful empty cache measurement");
        tokio::time::timeout(Duration::from_secs(10), exited.wait_for(|value| *value)).await.unwrap().unwrap();
        assert!(client.pending.lock().is_empty());
        assert!(client.process.lock().is_none(), "failure is terminal only after the owned child stops");
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_clear_awaiter_cannot_suppress_its_later_successful_reset() {
        let client = client_with_last_state(boundary_state());
        let mut lines = client.subscribe_lines();
        start_fake_child(&client, r#"
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine('{"type":"volume","volume":53}')
[Console]::ReadLine() | Out-Null
[Console]::WriteLine((@{request_id=$request.request_id; ok=$true} | ConvertTo-Json -Compress))
[Console]::WriteLine('{"type":"volume","volume":54}')
[Console]::ReadLine() | Out-Null
"#);
        let clear = {
            let client = client.clone();
            tokio::spawn(async move { client.clear_audio_cache().await })
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(lines.recv().await.unwrap(), StateLine::Volume(53)) { break; }
            }
        }).await.unwrap();
        clear.abort();
        assert!(clear.await.unwrap_err().is_cancelled());
        let release = build_line(&client.next_request_id(), "status", Value::Null);
        client.write_line(release).unwrap().1.await.unwrap().unwrap();
        let mut reset_seen = false;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match lines.recv().await.unwrap() {
                    StateLine::CacheMarksReset => reset_seen = true,
                    StateLine::Volume(54) => break,
                    _ => {}
                }
            }
        }).await.unwrap();
        assert!(reset_seen);
        assert!(!client.current_state().unwrap().queue[0].cached);
        client.retire_process(client.process_generation.load(Ordering::Acquire));
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unrelated_eviction_preserves_browse_badges_and_positive_then_reset_precedence() {
        let client = client_with_last_state(boundary_state());
        let mut lines = client.subscribe_lines();
        start_fake_child(&client, r#"
$ErrorActionPreference = 'Stop'
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine('{"type":"state","auth_state":"ready","queue_revision":7,"order_revision":7,"uncached_ids":["a"]}')
[Console]::WriteLine((@{request_id=$request.request_id; ok=$true; data=@{tracks=@(@{id='a'; cached=$true}, @{id='b'; cached=$true})}} | ConvertTo-Json -Depth 5 -Compress))
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine('{"type":"state","auth_state":"ready","queue_revision":7,"order_revision":7,"cached_ids":["a"]}')
[Console]::WriteLine((@{request_id=$request.request_id; ok=$true; data=@{tracks=@(@{id='a'; cached=$true}, @{id='b'; cached=$true})}} | ConvertTo-Json -Depth 5 -Compress))
$request = [Console]::ReadLine() | ConvertFrom-Json
    [Console]::WriteLine('{"type":"volume","volume":51}')
    [Console]::ReadLine() | Out-Null
[Console]::WriteLine((@{request_id=$request.request_id; ok=$true; data=@{tracks=@(@{id='a'; cached=$true}, @{id='b'; cached=$true})}} | ConvertTo-Json -Depth 5 -Compress))
[Console]::ReadLine() | Out-Null
"#);
        let reply = client.request("browse_playlist", json!({"id": "temporary"})).await.unwrap();
        let mut tracks: Vec<Track> = rows(reply).into_iter().map(Track::from).collect();
        client.reconcile_cached_rows(&mut tracks, client.cache_generation());
        assert!(!tracks[0].cached);
        assert!(tracks[1].cached, "unrelated file is still cached");
        let reply = client.request("browse_playlist", json!({"id": "temporary"})).await.unwrap();
        assert!(rows(reply)[0].cached, "new positive wins");
        let request = {
            let client = client.clone();
            tokio::spawn(async move { client.request("browse_playlist", json!({"id": "temporary"})).await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(lines.recv().await.unwrap(), StateLine::Volume(51)) { break; }
            }
        }).await.unwrap();
        client.clear_cache_marks();
        let release = build_line(&client.next_request_id(), "status", Value::Null);
        client.write_line(release).unwrap().1.await.unwrap().unwrap();
        let data = rows(request.await.unwrap().unwrap());
        assert!(!data[0].cached);
        assert!(!data[1].cached, "reset fences every row");
        let generation = client.process_generation.load(Ordering::Acquire);
        client.retire_process(generation);
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_child_input_times_out_without_blocking_runtime_and_replacement_answers() {
        let client = fast_client(boundary_state());
        let mut exited = client.exit_tx.subscribe();
        start_fake_child(&client, "Start-Sleep -Seconds 60");
        let request = {
            let client = client.clone();
            tokio::spawn(async move { client.request("play_queue", json!({"queue": "x".repeat(4 * 1024 * 1024)})).await })
        };
        // On two workers this also catches a second command blocked on the
        // old stdin mutex: both timers must still get runtime time.
        let second = {
            let client = client.clone();
            tokio::spawn(async move { client.status().await })
        };
        tokio::time::timeout(Duration::from_secs(2), tokio::time::sleep(Duration::from_millis(100))).await.unwrap();
        let (first, second) = tokio::time::timeout(Duration::from_secs(30), async {
            (request.await.unwrap(), second.await.unwrap())
        }).await.expect("stalled input must release both request waiters");
        assert!(first.is_err() && second.is_err());
        tokio::time::timeout(Duration::from_secs(3), async {
            while !*exited.borrow() { exited.changed().await.unwrap(); }
        }).await.unwrap();
        start_fake_child(&client, r#"
$request = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine((@{request_id=$request.request_id; ok=$true} | ConvertTo-Json -Compress))
[Console]::ReadLine() | Out-Null
"#);
        // A browse-lane request: its deadline outlasts PowerShell starting up,
        // which the shortened command deadline here would not.
        client.request("browse_replacement", Value::Null).await.unwrap();
        client.retire_process(client.process_generation.load(Ordering::Acquire));
    }

    fn persisted(snapshot: PlaybackSnapshot, queue_generation: u64) -> PersistedSnapshot {
        PersistedSnapshot {
            written_at: Instant::now(),
            snapshot,
            queue_generation,
        }
    }

    #[test]
    fn long_running_waveform_work_does_not_extend_cancellation_or_transport_deadlines() {
        let timing = Timing::PRODUCTION;
        assert!(timing.request_timeout("get_track_waveform") > timing.request_timeout("browse_album"));
        assert!(timing.request_timeout("browse_album") > timing.request_timeout("status"));
        assert_eq!(timing.request_timeout("cancel_track_waveform"), timing.request_timeout("pause"));
    }

    #[test]
    fn missing_or_malformed_reply_payloads_fail_at_the_typed_client_boundary() {
        for data in [None, Some(Value::Null), Some(json!({"track_id": "a", "interval_ms": "bad"}))] {
            let result = parse_data::<renderer_engine::protocol::TrackWaveform>(
                EngineReply { ok: true, data: data.map(test_data), ..EngineReply::default() }, "get_track_waveform",
            );
            assert!(result.is_err());
        }
    }


    /// Heartbeats notify the writer several times a second. Rewriting the
    /// queue each time would be pure churn, so only a real change — or a
    /// playhead that has moved far enough to be worth restoring to — counts;
    /// and a playhead alone owes the sidecar, not the snapshot.
    #[test]
    fn only_a_real_change_or_a_drifted_playhead_owes_a_write() {
        let mut state = PlaybackState::default();
        state.queue = vec![Track {
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        state.current_index = Some(0);
        state.position_ms = 30_000;
        let written = persisted(PlaybackSnapshot::from_playback(&state), 7);

        assert!(
            !written.differs_structurally_from(&state, 7) && !written.playhead_differs_from(&state),
            "an identical snapshot is not worth a write"
        );

        // The persisted queue strips the session-live download mark, so a
        // row that only gained a `cached` flag is still the same snapshot.
        let mut marked = state.clone();
        Arc::make_mut(&mut marked.queue)[0].cached = true;
        assert!(
            !written.differs_structurally_from(&marked, 8),
            "a download mark alone is not worth a write"
        );
        assert!(
            !written.playhead_differs_from(&marked),
            "nor is it a playhead move worth the sidecar"
        );

        // A pass that cannot have seen a new state line skips the row scan
        // entirely, which is only sound because every install of a queue
        // bumps the generation it is compared under.
        let mut edited = state.clone();
        edited.queue = edited.queue.iter().cloned().chain([Track {
            uri: "spotify:track:0VjIjW4GlUZAMYd2vXMi3b".to_owned(),
            duration_ms: 200_000,
            ..Track::default()
        }]).collect();
        assert!(
            !written.differs_structurally_from(&edited, 7),
            "the same generation cannot have replaced the queue"
        );
        assert!(
            written.differs_structurally_from(&edited, 8),
            "a queue edit is still detected by the scan that a new generation unlocks"
        );

        let mut nudged = state.clone();
        nudged.position_ms = 30_000 + PERSIST_POSITION_DRIFT_MS - 1;
        assert!(
            !written.playhead_differs_from(&nudged),
            "the playhead alone is exempt until it has drifted far enough"
        );

        let mut drifted = state.clone();
        drifted.position_ms = 30_000 + PERSIST_POSITION_DRIFT_MS;
        assert!(written.playhead_differs_from(&drifted));
        assert!(
            !written.differs_structurally_from(&drifted, 8),
            "a drifted playhead never rewrites the queue"
        );

        // A track change is a playhead move, not a queue rewrite: the sidecar
        // carries the current row index for exactly this.
        let mut next_track = state.clone();
        next_track.current_index = Some(1);
        next_track.position_ms = 0;
        assert!(written.playhead_differs_from(&next_track));
        assert!(
            !written.differs_structurally_from(&next_track, 8),
            "a new current row never rewrites the queue"
        );

        let mut paused_elsewhere = state.clone();
        paused_elsewhere.position_ms = 30_100;
        paused_elsewhere.volume = 41;
        assert!(
            written.differs_structurally_from(&paused_elsewhere, 8),
            "anything but the playhead is worth a write immediately"
        );
        assert!(
            !written.playhead_differs_from(&paused_elsewhere),
            "a volume change is not a playhead move, however small its drift"
        );

        // The speed the bar does not show right now — episodes', while a song
        // plays — is persisted as soon as it changes too.
        let mut episodes_faster = state.clone();
        episodes_faster.episode_speed = 1.5;
        assert!(
            written.differs_structurally_from(&episodes_faster, 7),
            "either kind's speed is worth a write"
        );
        let rewritten = PlaybackSnapshot::from_playback(&episodes_faster);
        assert_eq!((rewritten.track_speed, rewritten.episode_speed), (1.0, 1.5));
    }

    /// The deadline arm's split, as the writer sees it: a heartbeat that has
    /// only moved the position owes the sidecar, a state line that replaces
    /// the queue owes the snapshot, and the row scan runs only for the latter.
    #[test]
    fn a_heartbeat_owes_the_sidecar_and_a_state_line_the_snapshot() {
        let mut state = PlaybackState::default();
        state.auth_state = "ready".to_owned();
        state.queue = vec![Track {
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        state.current_index = Some(0);
        state.position_ms = 30_000;
        let client = client_with_last_state(state.clone());
        let written = persisted(
            PlaybackSnapshot::from_playback(&state),
            client.queue_generation.load(Ordering::Acquire),
        );

        assert!(
            client.due_write(Some(&written)).write.is_none(),
            "an untouched pass writes nothing"
        );

        client.on_position(PositionHeartbeat {
            position_ms: 33_000,
            duration_ms: 240_000,
        });
        assert!(
            client.due_write(Some(&written)).write.is_none(),
            "a heartbeat that has not drifted far enough writes nothing"
        );

        client.on_position(PositionHeartbeat {
            position_ms: 46_000,
            duration_ms: 240_000,
        });
        let decision = client.due_write(Some(&written));
        assert_eq!(
            decision.compared,
            Some(client.queue_generation.load(Ordering::Acquire)),
            "the queue was compared: the generation is recorded against it"
        );
        match decision.write {
            Some(PersistWrite::Playhead {
                current_index,
                position_ms,
                queue_identity,
            }) => {
                assert_eq!(current_index, Some(0));
                assert_eq!(position_ms, 46_000);
                assert_eq!(
                    queue_identity,
                    written.snapshot.queue_identity(),
                    "the sidecar is stamped with the queue of the snapshot it trails"
                );
            }
            other => panic!("a drifted playhead owes the sidecar, got {other:?}"),
        }

        // A state line that replaces the queue needs the whole snapshot, even
        // though its playhead happens to match what was last written.
        let mut edited = state.clone();
        edited.queue_revision = state.queue_revision + 1;
        edited.queue = edited.queue.iter().cloned().chain([Track {
            uri: "spotify:track:0VjIjW4GlUZAMYd2vXMi3b".to_owned(),
            duration_ms: 200_000,
            ..Track::default()
        }]).collect();
        client.on_state(&edited, PayloadRows::Sent);
        match client.due_write(Some(&written)).write {
            Some(PersistWrite::Snapshot(snapshot)) => {
                assert_eq!(snapshot.queue.len(), 2, "the snapshot carries the new row");
            }
            other => panic!("a replaced queue owes the snapshot, got {other:?}"),
        }
    }

    /// A state line carries the queue only when the queue it describes has
    /// changed: the engine drops `queue` and `upcoming` and leaves the
    /// revision behind. Nothing downstream may read that as an emptied queue —
    /// the restore gate would never match again, and the persistence
    /// comparison would write the emptiness to disk.
    #[test]
    fn a_revision_matched_state_keeps_the_retained_queue() {
        let mut state = PlaybackState::default();
        state.ready = true;
        state.auth_state = "ready".to_owned();
        state.queue_revision = 41;
        state.order_revision = 41;
        state.queue = vec![Track {
            id: "kept".to_owned(),
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        state.upcoming = vec![7];
        let client = client_with_last_state(state.clone());
        let mut lines = client.subscribe_lines();

        let mut resumed = PlaybackState::default();
        resumed.ready = true;
        resumed.auth_state = "ready".to_owned();
        resumed.queue_revision = 41;
        resumed.order_revision = 41;
        resumed.playing = true;
        resumed.position_ms = 12_000;
        client.on_state(&resumed, PayloadRows::Omitted);

        let retained = client.last_state.lock().clone().expect("state retained");
        assert_eq!(retained.queue, state.queue, "the omitted queue is the held one");
        assert!(Arc::ptr_eq(&retained.queue, &client.delta_base.lock().queue),
            "filling an omitted queue copies a pointer, not the rows");
        assert_eq!(retained.upcoming, state.upcoming);
        assert_eq!(retained.position_ms, 12_000, "the scalars still move");
        match lines.try_recv().expect("state fanned out") {
            StateLine::State(fanned) => assert!(Arc::ptr_eq(&fanned.queue, &retained.queue),
                "and so does every subscriber's copy"),
            StateLine::Position(_) => panic!("a full state does not arrive on the position lane"),
            StateLine::Volume(_) => panic!("nor the volume lane"),
            StateLine::CachedIds(_) => panic!("the full state carries its cache marks"),
            StateLine::UncachedIds(_) | StateLine::CacheMarksReset => panic!("not a cache invalidation"),
            StateLine::Disconnected => panic!("the reader stays connected"),
        }

        // A new revision carries its own rows even when they are none: the
        // engine sends the full queue whenever the revision moves, so an
        // empty array is authoritative rather than an omission.
        let mut emptied = state.clone();
        emptied.queue_revision = 42;
        emptied.order_revision = 42;
        emptied.queue = Vec::new().into();
        emptied.current_index = None;
        emptied.upcoming = Vec::new();
        client.on_state(&emptied, PayloadRows::Sent);
        let retained = client.last_state.lock().clone().expect("state retained");
        assert!(retained.queue.is_empty(), "a new revision's rows stand as sent");
        assert!(retained.upcoming.is_empty());
    }

    /// The base an omitted queue is merged from is the *wire's* last full
    /// payload, not the last state the durability filter agreed to keep. A
    /// crash resume is what separates the two: the engine comes back, restores
    /// the queue and publishes it PAUSED at a revision of its own — a state
    /// the restore gate cannot match yet (`resume_playing` is what a crashed
    /// player meant), so it is deliberately never adopted — and the `Play`
    /// after it changes no revision, so its payload omits the rows. Merged
    /// against durable state there are no rows for that revision, and the
    /// resumed state arrives with an emptied queue: the gate never matches
    /// again and keeps suppressing every ready state while audio plays.
    #[test]
    fn a_restore_intermediate_carries_its_rows_into_the_play_that_follows() {
        let mut previous = PlaybackState::default();
        previous.ready = true;
        previous.auth_state = "ready".to_owned();
        previous.playing = true;
        previous.queue_revision = 7;
        previous.order_revision = 7;
        previous.queue = vec![Track {
            id: "restored".to_owned(),
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        previous.current_index = Some(0);
        previous.position_ms = 42_000;
        let client = client_with_last_state(previous.clone());
        *client.restore_pending.lock() = Some(RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&previous, true),
            sent: true,
            attempts: 0,
            only_if_preview: false,
            preview_lease_id: None,
        });
        let mut lines = client.subscribe_lines();

        // `restore_queue` landed: the restored queue is published paused.
        let mut restored = previous.clone();
        restored.queue_revision = 8;
        restored.order_revision = 8;
        restored.playing = false;
        client.on_state(&restored, PayloadRows::Sent);
        assert!(
            client.restore_is_pending(),
            "a paused restore state does not match resume intent"
        );

        // `Play` follows without touching the queue: no rows on the wire.
        let mut playing = restored.clone();
        playing.playing = true;
        playing.queue = Vec::new().into();
        playing.upcoming = Vec::new();
        client.on_state(&playing, PayloadRows::Omitted);

        assert!(
            !client.restore_is_pending(),
            "the resumed state completes the restore"
        );
        assert_eq!(
            client.last_state.lock().as_ref().unwrap().queue,
            previous.queue,
            "the durable state holds the restored rows"
        );
        let _ = lines.try_recv().expect("the paused restore state fans out");
        match lines.try_recv().expect("the playing state fans out") {
            StateLine::State(fanned) => {
                assert_eq!(fanned.queue, previous.queue, "with the engine's rows");
                assert!(fanned.playing);
            }
            StateLine::Position(_) => panic!("a full state does not arrive on the position lane"),
            StateLine::Volume(_) => panic!("nor the volume lane"),
            StateLine::CachedIds(_) => panic!("the full state carries its cache marks"),
            StateLine::UncachedIds(_) | StateLine::CacheMarksReset => panic!("not a cache invalidation"),
            StateLine::Disconnected => panic!("the reader stays connected"),
        }
    }

    /// Draft previews are the other half of the same rule: their full states
    /// never become durable state either, so a pause, play or seek inside the
    /// editor would reach the window — and media keys, which read an empty
    /// queue as Stopped — with no rows at all.
    #[test]
    fn a_preview_delta_keeps_the_draft_rows_its_full_state_carried() {
        let mut real = PlaybackState::default();
        real.ready = true;
        real.auth_state = "ready".to_owned();
        real.playing = true;
        real.queue_revision = 12;
        real.order_revision = 12;
        real.queue = vec![Track {
            id: "real".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        real.current_index = Some(0);
        let client = client_with_last_state(real.clone());
        let mut lines = client.subscribe_lines();

        let mut preview = real.clone();
        preview.preview = true;
        preview.queue_revision = 13;
        preview.order_revision = 13;
        preview.queue = vec![Track {
            id: "draft".to_owned(),
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_owned(),
            duration_ms: 200_000,
            ..Track::default()
        }].into();
        client.on_state(&preview, PayloadRows::Sent);
        let _ = lines.try_recv().expect("the preview state fans out");

        // A transport change inside the draft: same revision, no rows.
        let mut paused = preview.clone();
        paused.playing = false;
        paused.queue = Vec::new().into();
        paused.upcoming = Vec::new();
        client.on_state(&paused, PayloadRows::Omitted);

        match lines.try_recv().expect("the paused draft fans out") {
            StateLine::State(fanned) => {
                assert_eq!(fanned.queue, preview.queue, "the draft keeps its rows");
                assert!(!fanned.playing);
                assert!(fanned.preview);
            }
            StateLine::Position(_) => panic!("a full state does not arrive on the position lane"),
            StateLine::Volume(_) => panic!("nor the volume lane"),
            StateLine::CachedIds(_) => panic!("the full state carries its cache marks"),
            StateLine::UncachedIds(_) | StateLine::CacheMarksReset => panic!("not a cache invalidation"),
            StateLine::Disconnected => panic!("the reader stays connected"),
        }
        assert_eq!(
            client.last_state.lock().as_ref().unwrap().queue,
            real.queue,
            "a draft is still never the durable state"
        );
    }

    /// Revision `0` is the pre-revision payload — "assume changed", see
    /// `PlaybackState::queue_revision` — so it is never merged into: two
    /// explicit revision-0 queues both stand as sent, and a base keyed on that
    /// revision would replace the second of them with the first.
    #[test]
    fn revision_zero_payloads_stand_as_sent() {
        let mut first = PlaybackState::default();
        first.ready = true;
        first.auth_state = "ready".to_owned();
        first.queue = vec![Track {
            id: "first".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        let client = client_with_last_state(first);

        let mut second = PlaybackState::default();
        second.ready = true;
        second.auth_state = "ready".to_owned();
        second.queue = vec![Track {
            id: "second".to_owned(),
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_owned(),
            duration_ms: 200_000,
            ..Track::default()
        }].into();
        client.on_state(&second, PayloadRows::Sent);

        let retained = client.last_state.lock().clone().expect("state retained");
        assert_eq!(
            retained.queue, second.queue,
            "an explicit queue is never replaced by a revision-0 base"
        );
        assert_eq!(retained.queue_revision, 0);
    }

    /// A payload that omits rows for a revision nothing retained cannot be
    /// published as a queue: there is no base to merge from, and the parsed
    /// (empty) vectors would read downstream as a cleared queue. It is
    /// withheld and the engine is asked to re-send its state — once, so an
    /// engine with nothing new to say cannot turn the recovery into a request
    /// loop.
    #[test]
    fn a_payload_without_retained_rows_is_withheld_and_re_requested_once() {
        let mut state = PlaybackState::default();
        state.ready = true;
        state.auth_state = "ready".to_owned();
        let client = client_with_last_state(state);
        let mut lines = client.subscribe_lines();

        let mut omitted = PlaybackState::default();
        omitted.ready = true;
        omitted.auth_state = "ready".to_owned();
        omitted.playing = true;
        omitted.queue_revision = 12;
        client.on_state(&omitted, PayloadRows::Omitted);

        assert!(
            lines.try_recv().is_err(),
            "an unmergeable payload is not published"
        );
        assert!(
            !client.last_state.lock().as_ref().unwrap().playing,
            "nor installed as the live state"
        );
        assert_eq!(
            client.delta_base.lock().resynced,
            Some(12),
            "the engine was asked to re-send that revision"
        );
        assert!(
            !client.delta_base.lock().mark_resynced(12),
            "and only once: a second payload for it asks for nothing"
        );
    }

    #[test]
    fn persistence_coalesces_dirty_generations_until_the_write_deadline() {
        let started = Instant::now();
        let snapshot = PlaybackSnapshot::from_playback(&PlaybackState::default());
        let mut schedule = PersistenceSchedule::default();

        schedule.mark_dirty(1);
        schedule.mark_dirty(2);
        assert_eq!(schedule.pending_generation, Some(2));
        assert_eq!(schedule.wait_for(started), Some(Duration::ZERO));

        schedule.record_success(PersistWrite::Snapshot(snapshot), 2, 2, 0, started);
        schedule.mark_dirty(3);
        schedule.mark_dirty(4);
        assert_eq!(schedule.pending_generation, Some(4));
        assert_eq!(
            schedule.wait_for(started + PERSIST_MIN_INTERVAL / 2),
            Some(PERSIST_MIN_INTERVAL - PERSIST_MIN_INTERVAL / 2)
        );
        assert_eq!(
            schedule.wait_for(started + PERSIST_MIN_INTERVAL),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn persistence_failure_does_not_advance_the_committed_marker_and_retries() {
        let started = Instant::now();
        let original = PlaybackSnapshot::from_playback(&PlaybackState::default());
        let mut changed_state = PlaybackState::default();
        changed_state.volume = 42;
        let mut schedule = PersistenceSchedule::default();
        schedule.record_success(PersistWrite::Snapshot(original.clone()), 1, 1, 0, started);
        schedule.mark_dirty(2);

        let failed_at = started + PERSIST_MIN_INTERVAL;
        schedule.record_failure(2, failed_at);
        assert_eq!(schedule.committed_generation, 1);
        assert_eq!(
            schedule.committed.as_ref().map(|value| &value.snapshot),
            Some(&original)
        );
        assert_eq!(schedule.wait_for(failed_at), Some(PERSIST_RETRY_INTERVAL));
        assert!(
            schedule
                .committed
                .as_ref()
                .unwrap()
                .differs_structurally_from(&changed_state, 0),
            "a volume change is structural even on a pass that skips the row scan"
        );

        schedule.record_success(
            PersistWrite::Snapshot(PlaybackSnapshot::from_playback(&changed_state)),
            2,
            2,
            0,
            failed_at + PERSIST_RETRY_INTERVAL,
        );
        assert_eq!(schedule.committed_generation, 2);
        assert_eq!(
            schedule.committed.as_ref().map(|value| &value.snapshot),
            Some(&PlaybackSnapshot::from_playback(&changed_state))
        );
        assert_eq!(schedule.wait_for(Instant::now()), None);
    }

    #[test]
    fn persistence_keeps_a_newer_generation_dirty_after_a_success() {
        let mut schedule = PersistenceSchedule::default();
        let snapshot = PlaybackSnapshot::from_playback(&PlaybackState::default());
        schedule.mark_dirty(1);
        schedule.record_success(PersistWrite::Snapshot(snapshot), 1, 3, 0, Instant::now());
        assert_eq!(schedule.committed_generation, 1);
        assert_eq!(schedule.pending_generation, Some(3));
    }


    #[test]
    fn reply_lines_keep_error_and_data_fields() {
        let reply = parse_value(serde_json::json!({
            "type": "browse_playlist",
            "request_id": "request-8",
            "ok": false,
            "error": "browse failed",
            "data": {"tracks": [{"id": "track-1"}]},
        }));
        match reply {
            Some(Line::Reply {
                request_id,
                ok,
                error,
                data,
            }) => {
                assert_eq!(request_id, "request-8");
                assert!(!ok);
                assert_eq!(error.as_deref(), Some("browse failed"));
                assert_eq!(reply_value(&data.unwrap())["tracks"][0]["id"], "track-1");
            }
            _ => panic!("reply fields stay on the reply path"),
        }
    }

    #[test]
    fn position_lines_parse_as_scalar_heartbeats_not_states() {
        let line = parse_value(serde_json::json!({
            "type": "position",
            "position_ms": 12_345,
            "duration_ms": 240_000,
        }));
        let Line::Position(heartbeat) = line.expect("position line parses") else {
            panic!("expected a position heartbeat");
        };
        assert_eq!(heartbeat.position_ms, 12_345);
        assert_eq!(heartbeat.duration_ms, 240_000);
    }

    #[test]
    fn state_and_reply_lines_still_classify_as_before() {
        let state = parse_value(serde_json::json!({
            "type": "state",
            "ready": true,
            "auth_state": "ready",
            "playing": true,
            "position_ms": 1,
            "duration_ms": 2,
            "volume": 50,
            "shuffle": false,
            "repeat": "off",
            "queue": [],
        }));
        assert!(matches!(
            state,
            Some(Line::State {
                rows: PayloadRows::QueueOnly,
                ..
            })
        ));

        // The rows travel with the revision that names them: a payload that
        // leaves `queue` out is one whose generation this client already
        // holds, and the empty vector it parses into must never be read as the
        // queue itself.
        let omitted = parse_value(serde_json::json!({
            "type": "state",
            "ready": true,
            "auth_state": "ready",
            "position_ms": 4_000,
            "queue_revision": 9,
        }));
        assert!(matches!(
            omitted,
            Some(Line::State {
                rows: PayloadRows::Omitted,
                ..
            })
        ));

        let reply = parse_value(serde_json::json!({
            "type": "response",
            "request_id": "request-7",
            "ok": true,
        }));
        match reply {
            Some(Line::Reply {
                request_id,
                ok,
                error,
                data,
            }) => {
                assert_eq!(request_id, "request-7");
                assert!(ok);
                assert!(error.is_none());
                assert!(data.is_none());
            }
            _ => panic!("response lines stay on the reply path"),
        }
    }

    #[test]
    fn position_heartbeats_freshen_the_last_state_scalars_in_place() {
        let mut state = PlaybackState::default();
        state.auth_state = "ready".to_owned();
        state.queue = vec![Track::default(); 3].into();
        let client = client_with_last_state(state);
        let queue_ptr = client.last_state.lock().as_ref().unwrap().queue.as_ptr();

        // Subscribe before the send: broadcast messages sent with no active
        // receiver are dropped, exactly like the production flow where
        // consume_states subscribes before the engine produces lines.
        let mut receiver = client.subscribe_lines();
        client.on_position(PositionHeartbeat {
            position_ms: 42_000,
            duration_ms: 240_000,
        });

        let last = client.last_state.lock();
        let last = last.as_ref().expect("last state kept");
        assert_eq!(last.position_ms, 42_000);
        assert_eq!(last.duration_ms, 240_000);
        assert_eq!(
            last.queue.as_ptr(),
            queue_ptr,
            "a heartbeat must never clone the queue"
        );
        assert_eq!(last.queue.len(), 3, "queue contents untouched");

        // Subscribers see the heartbeat as a Position line, in wire order.
        match receiver.try_recv().expect("heartbeat fanned out") {
            StateLine::Position(heartbeat) => {
                assert_eq!(heartbeat.position_ms, 42_000);
                assert_eq!(heartbeat.duration_ms, 240_000);
            }
            StateLine::State(_) => panic!("heartbeat must not arrive as a full state"),
            StateLine::Volume(_) => panic!("nor as a volume step"),
            StateLine::CachedIds(_) => panic!("nor as cache marks"),
            StateLine::UncachedIds(_) | StateLine::CacheMarksReset => panic!("not a cache invalidation"),
            StateLine::Disconnected => panic!("heartbeat path must stay connected"),
        }
    }

    /// Volume changes preserve the cached playhead and held queue allocation.
    #[test]
    fn volume_lines_update_volume_without_changing_playhead_or_queue_identity() {
        let mut state = PlaybackState::default();
        state.auth_state = "ready".to_owned();
        state.volume = 50;
        state.position_ms = 12_000;
        state.queue = vec![Track::default(); 3].into();
        let client = client_with_last_state(state);
        let queue_ptr = client.last_state.lock().as_ref().unwrap().queue.as_ptr();
        // Subscribe before the send: broadcast messages sent with no active
        // receiver are dropped, exactly like the production flow where
        // consume_states subscribes before the engine produces lines.
        let mut receiver = client.subscribe_lines();

        let Some(Line::Volume(volume)) = parse_value(serde_json::json!({
            "type": "volume",
            "volume": 31,
        })) else {
            panic!("a volume line parses as a scalar");
        };
        client.on_volume(volume);

        let last = client.last_state.lock();
        let last = last.as_ref().expect("last state kept");
        assert_eq!(last.volume, 31, "the cached volume is the engine's");
        assert_eq!(
            last.position_ms, 12_000,
            "and the playhead is not part of a volume step"
        );
        assert_eq!(
            last.queue.as_ptr(),
            queue_ptr,
            "a volume step must never clone the queue"
        );
        match receiver.try_recv().expect("volume fanned out") {
            StateLine::Volume(fanned) => assert_eq!(fanned, 31),
            StateLine::State(_) => panic!("a volume step must not arrive as a full state"),
            StateLine::Position(_) => panic!("nor as a position heartbeat"),
            StateLine::CachedIds(_) => panic!("nor as cache marks"),
            StateLine::UncachedIds(_) | StateLine::CacheMarksReset => panic!("not a cache invalidation"),
            StateLine::Disconnected => panic!("the volume path must stay connected"),
        }
    }

    #[test]
    fn preview_states_and_heartbeats_keep_the_real_crash_restore_snapshot() {
        let mut normal = PlaybackState::default();
        normal.ready = true;
        normal.auth_state = "ready".to_owned();
        normal.playing = true;
        normal.position_ms = 12_000;
        normal.duration_ms = 240_000;
        normal.current_index = Some(0);
        normal.queue = vec![Track {
            id: "real-track".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        let client = client_with_last_state(normal.clone());

        let mut preview = normal.clone();
        preview.preview = true;
        preview.position_ms = 80_000;
        Arc::make_mut(&mut preview.queue)[0].id = "draft-track".to_owned();
        // A draft that changed rows carries a new queue revision, exactly as
        // the engine emits it: it is the payload that proves the editor can
        // never speak for the retained queue.
        preview.queue_revision = normal.queue_revision + 1;
        client.on_state(&preview, PayloadRows::Sent);
        client.on_position(PositionHeartbeat {
            position_ms: 90_000,
            duration_ms: 180_000,
        });

        let retained = client.last_state.lock().clone().unwrap();
        assert_eq!(retained.queue[0].id, "real-track");
        assert_eq!(retained.position_ms, 12_000);

        client.on_eof();
        let restore = client.restore_pending.lock();
        let restore = &restore.as_ref().expect("real queue retained").snapshot;
        assert_eq!(restore.queue[0].id, "real-track");
        assert_eq!(restore.position_ms, 12_000);
        assert!(restore.resume_playing);
    }

    #[test]
    fn preview_restore_lease_replacement_keeps_snapshot_and_changes_owner() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.playing = true;
        real.current_index = Some(0);
        real.queue = vec![Track {
            id: "real".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        let client = client_with_last_state(real);

        assert!(client.capture_preview_restore(17));
        let captured = client
            .restore_pending
            .lock()
            .as_ref()
            .expect("preview lease captured")
            .snapshot
            .clone();

        assert!(!client.capture_preview_restore(29));
        let plan = client.restore_pending.lock();
        let plan = plan.as_ref().expect("replacement keeps the lease");
        assert_eq!(plan.preview_lease_id, Some(29));
        assert_eq!(plan.snapshot.queue, captured.queue);
        assert_eq!(plan.snapshot.position_ms, captured.position_ms);
    }

    #[tokio::test]
    async fn stale_preview_restore_is_a_noop_for_the_current_lease() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.queue = vec![Track::default()].into();
        let client = client_with_last_state(real);

        assert!(client.capture_preview_restore(17));
        assert!(!client.capture_preview_restore(29));
        assert_eq!(client.restore_preview(17).await, Ok(()));

        let plan = client.restore_pending.lock();
        let plan = plan.as_ref().expect("stale restore keeps current lease");
        assert_eq!(plan.preview_lease_id, Some(29));
        assert!(!plan.sent, "stale restore must not reserve the request");
    }

    #[tokio::test]
    async fn preview_restore_request_error_retains_lease_for_reconciliation() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.queue = vec![Track::default()].into();
        let client = client_with_last_state(real);

        assert!(client.capture_preview_restore(17));
        assert!(client.restore_preview(17).await.is_err());

        let plan = client
            .restore_pending
            .lock()
            .clone()
            .expect("transport uncertainty retains the snapshot");
        assert_eq!(plan.preview_lease_id, Some(17));
        assert!(!plan.sent, "a failed request is retryable");
    }

    #[test]
    fn preview_true_to_false_clears_only_an_authoritatively_active_lease() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.queue = vec![Track::default()].into();
        let client = client_with_last_state(real.clone());
        assert!(client.capture_preview_restore(17));

        // An unrelated real state before activation cannot consume the lease.
        client.on_state(&real, PayloadRows::Sent);
        assert!(client.restore_pending.lock().is_some());

        let mut preview = real.clone();
        preview.preview = true;
        client.on_state(&preview, PayloadRows::Sent);
        assert!(client.restore_pending.lock().is_some());

        let mut restored = preview;
        restored.preview = false;
        client.on_state(&restored, PayloadRows::Sent);
        assert!(
            client.restore_pending.lock().is_none(),
            "authoritative preview exit reconciles a dropped restore reply"
        );
    }

    #[test]
    fn blank_ready_state_is_suppressed_until_restored_state_matches() {
        let mut previous = PlaybackState::default();
        previous.auth_state = "ready".to_owned();
        previous.queue = vec![Track {
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        previous.current_index = Some(0);
        previous.position_ms = 42_000;
        previous.volume = 37;
        previous.shuffle = true;
        previous.repeat = "context".to_owned();
        let client = client_with_last_state(previous.clone());
        *client.restore_pending.lock() = Some(RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&previous, false),
            sent: false,
            attempts: 0,
            only_if_preview: false,
            preview_lease_id: None,
        });

        let mut blank = PlaybackState::default();
        blank.ready = true;
        blank.auth_state = "ready".to_owned();
        client.on_state(&blank, PayloadRows::Sent);
        assert_eq!(
            client.last_state.lock().as_ref().unwrap().queue,
            previous.queue,
            "fresh-child blank state must not erase the crash snapshot"
        );
        let (restore, _) = client
            .begin_pending_restore_with_generation(&blank)
            .expect("ready state starts restore");
        assert!(!restore.resume_playing);
        assert!(client
            .begin_pending_restore_with_generation(&blank)
            .is_none());

        let mut restored = previous;
        restored.playing = false;
        client.on_state(&restored, PayloadRows::Sent);
        assert!(!client.restore_is_pending());
        assert_eq!(
            client.last_state.lock().as_ref().unwrap().position_ms,
            42_000
        );
    }

    #[test]
    fn durable_restore_retries_are_capped_and_then_disarmed() {
        let client = client_with_last_state(PlaybackState::default());
        let plan = || RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&PlaybackState::default(), false),
            sent: false,
            attempts: 0,
            only_if_preview: false,
            preview_lease_id: None,
        };
        *client.restore_pending.lock() = Some(plan());

        // Durable retries stay bounded and disarm after their final attempt.
        let mut delays = Vec::new();
        loop {
            match retry_restore_plan(&mut client.restore_pending.lock()) {
                Some(delay) => delays.push(delay),
                None => break,
            }
        }
        assert_eq!(delays.len(), RESTORE_RETRY_ATTEMPTS - 1);
        assert_eq!(delays[0], RESTORE_RETRY_BASE);
        assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(delays[1] > delays[0]);
        assert!(delays.iter().all(|delay| *delay <= RESTORE_RETRY_MAX));
        assert!(client.restore_pending.lock().is_none());
        assert!(retry_restore_plan(&mut client.restore_pending.lock()).is_none());

        // A preview lease is never consumed (nor disarmed) by this path.
        *client.restore_pending.lock() = Some(RestorePlan {
            preview_lease_id: Some(7),
            only_if_preview: true,
            ..plan()
        });
        assert!(retry_restore_plan(&mut client.restore_pending.lock()).is_none());
        assert!(client
            .restore_pending
            .lock()
            .as_ref()
            .is_some_and(|plan| plan.only_if_preview && plan.attempts == 0));
    }

    #[test]
    fn due_restore_claims_its_generation_without_a_status_request() {
        let mut state = PlaybackState::default();
        state.auth_state = "ready".to_owned();
        let client = client_with_last_state(state.clone());
        *client.restore_pending.lock() = Some(RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&state, false),
            sent: false,
            attempts: 0,
            only_if_preview: false,
            preview_lease_id: None,
        });
        let (_, generation) = client
            .begin_pending_restore_with_generation(&state)
            .expect("first attempt");
        assert_eq!(
            client.retry_pending_restore_for_generation(generation),
            Ok(Some(RESTORE_RETRY_BASE))
        );
        assert!(client.begin_pending_retry(generation).is_some());
        assert!(client.begin_pending_retry(generation).is_none());
    }

    #[test]
    fn failed_restore_from_dead_engine_cannot_rearm_replacement_plan() {
        let mut state = PlaybackState::default();
        state.auth_state = "ready".to_owned();
        let client = client_with_last_state(state.clone());
        *client.restore_pending.lock() = Some(RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&state, false),
            sent: false,
            attempts: 0,
            only_if_preview: false,
            preview_lease_id: None,
        });

        let (_, old_generation) = client
            .begin_pending_restore_with_generation(&state)
            .expect("first engine claims restore");
        client.on_eof();
        assert!(client.restore_is_pending(), "crash retains a fresh plan");
        assert_eq!(
            client.retry_pending_restore_for_generation(old_generation),
            Err(())
        );
        assert!(client.begin_pending_retry(old_generation).is_none());
        let pending = client.restore_pending.lock();
        let replacement = pending.as_ref().unwrap();
        assert_eq!(replacement.attempts, 0);
        assert!(
            !replacement.sent,
            "replacement remains ready for its own attempt"
        );
    }


    #[test]
    fn restore_plans_never_carry_download_marks() {
        let mut state = PlaybackState::default();
        state.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            cached: true,
            ..Track::default()
        }].into();
        state.current_index = Some(0);

        // Both constructors funnel through `normalized`: a live crash-recovery
        // state and an on-disk snapshot written by an older build alike.
        assert!(!RestoreSnapshot::from_playback(&state, false).queue[0].cached);
        let durable = RestoreSnapshot::from_playback(&state, false).durable();
        assert!(!durable.queue[0].cached);
        assert!(!RestoreSnapshot::from_durable(durable).queue[0].cached);
    }

    #[test]
    fn restore_matches_ignores_fields_the_engine_rewrites_on_install() {
        // The snapshot as saved: marks, an edit, and a context from a past
        // session. The engine's restored queue legitimately differs in all
        // three — cached is dropped on install, edits re-resolve against the
        // store, context is refilled — and none of that may hang the restore.
        let mut snapshot_state = PlaybackState::default();
        snapshot_state.auth_state = "ready".to_owned();
        snapshot_state.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            cached: true,
            effective_edit: None,
            context: "playlist:abc".to_owned(),
            ..Track::default()
        }].into();
        snapshot_state.current_index = Some(0);
        let restore = RestoreSnapshot::from_playback(&snapshot_state, false);

        let mut restored = PlaybackState::default();
        restored.auth_state = "ready".to_owned();
        restored.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            cached: false,
            ..Track::default()
        }].into();
        restored.current_index = Some(0);

        assert!(
            restore.matches(&restored),
            "volatile row fields must not keep the restore pending forever"
        );

        // Identity still matters: a different queue is not "restored".
        Arc::make_mut(&mut restored.queue)[0].id = "zzzzzzzzzzzzzzzzzzzzzz".to_owned();
        assert!(!restore.matches(&restored));
    }

    #[test]
    fn restore_matches_a_position_clamped_by_a_new_shorter_compiled_timeline() {
        let mut snapshot_state = PlaybackState::default();
        snapshot_state.auth_state = "ready".to_owned();
        snapshot_state.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }].into();
        snapshot_state.current_index = Some(0);
        snapshot_state.position_ms = 200_000;
        let restore = RestoreSnapshot::from_playback(&snapshot_state, false);

        let mut restored = snapshot_state;
        restored.position_ms = 150_000;
        restored.duration_ms = 150_000;

        assert!(
            restore.matches(&restored),
            "a newly enabled cut may clamp an old compiled snapshot to the new duration"
        );
    }

    /// Finds a built engine binary: `SPOTIFY_ENGINE_PATH`, the workspace
    /// target dir, or the engine package's own target dir.
    fn find_engine() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("SPOTIFY_ENGINE_PATH") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        for candidate in [
            root.join("target").join("debug").join(ENGINE_BINARY),
            root.join("target").join("release").join(ENGINE_BINARY),
            root.join("engine")
                .join("target")
                .join("debug")
                .join(ENGINE_BINARY),
            root.join("engine")
                .join("target")
                .join("release")
                .join(ENGINE_BINARY),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// End-to-end wire test against the real engine: spawn, initial state
    /// line, a `status` round-trip, the unauthenticated browse error path,
    /// and a graceful shutdown.
    #[tokio::test]
    async fn engine_client_round_trips_over_the_line_protocol() {
        let Some(exe) = find_engine() else {
            eprintln!(
                "skipping: {ENGINE_BINARY} is not built \
                 (run `cargo build -p renderer-engine`)"
            );
            return;
        };
        let state_dir = std::env::temp_dir().join(format!(
            "renderer-engine-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).unwrap();
        let mut client = client_with_last_state(PlaybackState::default());
        Arc::get_mut(&mut client).unwrap().state_dir = state_dir.clone();
        *client.last_state.lock() = None;
        *client.observed_state.lock() = None;
        let _child = IsolatedChild(client.clone());
        let mut command = Command::new(exe);
        command.arg("--state-dir").arg(&state_dir)
            .arg("--log-file").arg(state_dir.join("engine.log"))
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        client.spawn_child(&mut command).expect("isolated engine starts");
        let mut lines = client.subscribe_lines();

        // A fast child may publish before subscription; the production status
        // resync also restores that first observable state. Explicit paths
        // keep this test out of process-global settings and the owner's logs.
        client
            .status()
            .await
            .expect("initial status command round-trips");

        // The engine announces its session in response to the status sync.
        let first = match tokio::time::timeout(Duration::from_secs(20), lines.recv())
            .await
            .expect("engine emits its initial state within 20s")
            .expect("state channel stays open")
        {
            StateLine::State(state) => state,
            StateLine::Position(_) => panic!("the initial line is a full state, not a heartbeat"),
            StateLine::Volume(_) => panic!("nor a volume step"),
            StateLine::CachedIds(_) => panic!("nor cache marks"),
            StateLine::UncachedIds(_) | StateLine::CacheMarksReset => panic!("not a cache invalidation"),
            StateLine::Disconnected => panic!("engine disconnected before its initial state"),
        };
        assert_eq!(
            first.auth_state, "needs_login",
            "fresh state dir has no session"
        );

        // A command round-trip: status is answered and re-emits the state.
        client.status().await.expect("status command round-trips");
        let after_status = match tokio::time::timeout(Duration::from_secs(20), lines.recv())
            .await
            .expect("status triggers a fresh state line")
            .expect("state channel stays open")
        {
            StateLine::State(state) => state,
            StateLine::Position(_) => panic!("status re-emits a full state"),
            StateLine::Volume(_) => panic!("status re-emits a full state, not a volume step"),
            StateLine::CachedIds(_) => panic!("nor cache marks"),
            StateLine::UncachedIds(_) | StateLine::CacheMarksReset => panic!("not a cache invalidation"),
            StateLine::Disconnected => panic!("engine disconnected before the status state"),
        };
        assert_eq!(after_status.auth_state, first.auth_state);

        // Browse without a session fails cleanly through the reply channel.
        let error = client.browse_playlist_tree(10).await;
        assert!(error.is_err(), "browse without a session must fail cleanly");

        client.shutdown_engine();
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn a_crash_loop_backs_off_and_only_a_stable_child_starts_the_backoff_over() {
        let policy = Timing::PRODUCTION.respawn;
        let mut backoff = policy.start;
        let mut delays = Vec::new();
        // Each child spawns fine and dies a second later.
        for _ in 0..6 {
            let delay = policy.delay(backoff, Duration::from_secs(1));
            delays.push(delay.as_secs());
            backoff = policy.next(delay);
        }
        assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(delays[1] > delays[0], "early successful spawns cannot reset backoff");
        assert!(delays.iter().all(|seconds| Duration::from_secs(*seconds) <= policy.max));
        assert_eq!(delays[4], delays[5], "crash retries remain capped");
        // A child that stayed up long enough was healthy: its exit starts over.
        let delay = policy.delay(backoff, policy.stable_uptime);
        assert_eq!(delay, policy.start);
        assert!(policy.next(delay) > delay);
        assert_eq!(policy.delay(Duration::from_secs(16), policy.stable_uptime - Duration::from_millis(1)),
            Duration::from_secs(16));
    }


    /// The same object with `type` first, the way the engine writes it.
    fn type_first(kind: &str, value: &Value) -> String {
        let mut line = format!("{{\"type\":{}", Value::String(kind.into()));
        for (key, field) in value.as_object().unwrap() {
            if key == "type" { continue; }
            line.push_str(&format!(",{}:{}", Value::String(key.clone()), field));
        }
        line.push('}');
        line
    }

    #[test]
    fn state_parser_preserves_omission_empty_and_populated_rows_in_either_key_order() {
        let row = json!({"id": "a", "uri": "spotify:track:a", "name": "A", "duration_ms": 1000, "cached": true});
        let marks = [
            (None, None), (Some(Value::Null), None), (Some(json!(false)), None),
            (Some(json!({})), None), (Some(json!([])), Some(vec![])),
            (Some(json!(["a", "b"])), Some(vec!["a".to_owned(), "b".to_owned()])),
        ];
        for (queue, expected_queue) in [
            (None, Vec::new()), (Some(json!([])), Vec::new()),
            (Some(json!([row])), vec![Track { id: "a".into(), uri: "spotify:track:a".into(),
                name: "A".into(), duration_ms: 1000, cached: true, ..Track::default() }]),
        ] {
            for (upcoming, expected_upcoming) in [
                (None, Vec::new()), (Some(json!([])), Vec::new()), (Some(json!([2, 1])), vec![2, 1]),
            ] {
                let expected_rows = match (queue.is_some(), upcoming.is_some()) {
                    (true, true) => PayloadRows::Sent,
                    (true, false) => PayloadRows::QueueOnly,
                    (false, true) => PayloadRows::OrderOnly,
                    (false, false) => PayloadRows::Omitted,
                };
                for (cached, expected_cached) in &marks {
                    for (uncached, expected_uncached) in &marks {
                        let mut value = json!({"type": "state", "auth_state": "ready",
                            "queue_revision": 7, "order_revision": 9, "playing": true, "position_ms": 12});
                        for (key, field) in [("queue", &queue), ("upcoming", &upcoming),
                            ("cached_ids", cached), ("uncached_ids", uncached)] {
                            if let Some(field) = field { value[key] = field.clone(); }
                        }
                        for line in [type_first("state", &value), value.to_string()] {
                            let Some(Line::State { state, rows }) = parse_line(&line) else {
                                panic!("valid state rejected: {line}");
                            };
                            assert_eq!(rows, expected_rows);
                            assert_eq!(state.queue.as_ref(), expected_queue.as_slice());
                            assert_eq!(state.upcoming, expected_upcoming);
                            assert_eq!(&state.cached_ids, expected_cached);
                            assert_eq!(&state.uncached_ids, expected_uncached);
                            assert_eq!((state.playing, state.position_ms), (true, 12));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_engines_own_state_line_streams_into_the_typed_state() {
        use renderer_engine::protocol::{AuthState, RepeatMode, StateEvent, TrackRef};
        let rows = vec![TrackRef { id: "a".into(), uri: "spotify:track:a".into(), name: "Esc\"aped\n".into(),
            duration_ms: 1_000, cached: true, ..TrackRef::default() }];
        let marks = vec!["gone".to_owned()];
        let event = |queue: Option<&[TrackRef]>, upcoming: Option<Vec<usize>>| serde_json::to_string(&StateEvent {
            kind: "state", ready: true, auth_state: AuthState::Ready, auth_url: None, username: Some("me".into()),
            playing: true, buffering: false, preview: false, position_ms: 500, duration_ms: 1_000, volume: 40,
            shuffle: true, repeat: RepeatMode::Off, playback_speed: 1.5, track_speed: 1.5, episode_speed: 1.0,
            audible_playback_speed: 1.5, current_index: Some(0), current_uri: Some("spotify:track:a"),
            context: "playlist:p", queue, queue_revision: 3, order_revision: 4, upcoming, preload_uri: None,
            cached_ids: None, uncached_ids: Some(&marks), error: None,
        }).unwrap();
        for (line, expected_rows) in [
            (event(Some(&rows), Some(vec![])), PayloadRows::Sent),
            (event(Some(&rows), None), PayloadRows::QueueOnly),
            (event(None, Some(vec![0])), PayloadRows::OrderOnly),
            (event(None, None), PayloadRows::Omitted),
        ] {
            let parsed = parse_line(&line).expect("engine state parses");
            let Line::State { state, rows: parsed_rows } = parsed else { panic!("a state line is a state") };
            assert_eq!(parsed_rows, expected_rows);
            if matches!(expected_rows, PayloadRows::Sent | PayloadRows::QueueOnly) {
                assert_eq!(state.queue[0].name, "Esc\"aped\n");
                assert_eq!(state.queue[0].uri, "spotify:track:a");
            } else {
                assert!(state.queue.is_empty());
            }
            assert_eq!(state.upcoming, if expected_rows == PayloadRows::OrderOnly { vec![0] } else { vec![] });
            assert_eq!((state.username.as_str(), state.uncached_ids.as_deref()), ("me", Some(&marks[..])));
            assert_eq!(state.cached_ids, None);
        }
    }

    #[test]
    fn reply_parser_routes_valid_payloads_and_drops_malformed_lanes() {
        for value in [
            json!({"type": "browse_playlist", "request_id": "r", "ok": true, "data": {"tracks": [{"id": "t"}]}}),
            json!({"type": "response", "request_id": "r", "ok": false, "error": "refused"}),
            json!({"type": "browse_x", "request_id": "r", "ok": true, "data": null}),
        ] {
            for line in [type_first(value["type"].as_str().unwrap(), &value), value.to_string()] {
                let Some(Line::Reply { request_id, ok, error, data }) = parse_line(&line) else {
                    panic!("valid reply rejected: {line}");
                };
                assert_eq!(request_id, "r");
                assert_eq!(ok, value["ok"].as_bool().unwrap());
                assert_eq!(error.as_deref(), value.get("error").and_then(Value::as_str));
                assert_eq!(data.as_ref().map(reply_value), value.get("data").cloned());
            }
        }
        assert_eq!(parse_line(r#"{"type":"position","position_ms":5,"duration_ms":9}"#),
            Some(Line::Position(PositionHeartbeat { position_ms: 5, duration_ms: 9 })));
        assert_eq!(parse_line(r#"{"type":"volume","volume":33}"#), Some(Line::Volume(33)));
        for dropped in [r#"{"type":"state","queue":7}"#, r#"{"type":"volume","volume":300}"#,
            r#"{"type":"position"}"#, "not json", "[1,2]", "\"text\"", r#"{"type":"state"} trailing"#] {
            assert_eq!(parse_line(dropped), None, "{dropped}");
        }
    }

    #[test]
    fn typed_queue_requests_are_readable_by_the_engine_with_restore_guards_intact() {
        let queue = vec![Track { id: "a".into(), uri: "spotify:track:a".into(), duration_ms: 1_000,
            artist_names: vec!["A".into()], ..Track::default() }];
        let request: renderer_engine::protocol::Request = serde_json::from_str(&build_line("r", "play_queue",
            QueueRequest { queue: &queue, index: 0, position_ms: 5, context: "album:x",
                automatic_start: true, ..QueueRequest::default() })).unwrap();
        assert_eq!(request.request_id, "r");
        let renderer_engine::protocol::Command::PlayQueue { queue: rows, position_ms, context, automatic_start, start_paused, .. }
            = request.command else { panic!("queue request must be routable"); };
        assert_eq!(rows[0].uri, queue[0].uri);
        assert_eq!((position_ms, context.as_str(), automatic_start, start_paused), (5, "album:x", true, false));
        let request: renderer_engine::protocol::Request = serde_json::from_str(&build_line("r", "play_queue",
            QueueRequest { queue: &queue, start_paused: true, ..QueueRequest::default() })).unwrap();
        assert!(matches!(request.command,
            renderer_engine::protocol::Command::PlayQueue { start_paused: true, .. }), "a paused start reaches the engine");

        let request: renderer_engine::protocol::Request = serde_json::from_str(&build_line("restore", "restore_queue",
            QueueRequest { queue: &queue, only_if_preview: true, preview_lease_id: Some(17),
                resume_playing: Some(false), ..QueueRequest::default() })).unwrap();
        let renderer_engine::protocol::Command::RestoreQueue { only_if_preview, preview_lease_id, resume_playing, .. }
            = request.command else { panic!("guarded restore must be routable"); };
        assert!(only_if_preview);
        assert_eq!(preview_lease_id, 17);
        assert!(!resume_playing);
    }

    #[test]
    fn a_request_the_engine_would_refuse_is_refused_before_it_is_sent() {
        let line_for = |name_len: usize| request_line("request-1", "play_queue", QueueRequest {
            queue: &[Track { name: "x".repeat(name_len), ..Track::default() }], ..QueueRequest::default() });
        let overhead = line_for(0).unwrap().len();
        // The engine reads up to MAX bytes including the newline the writer adds.
        let largest = MAX_REQUEST_BYTES - 1 - overhead;
        assert_eq!(line_for(largest).unwrap().len() + 1, MAX_REQUEST_BYTES);
        assert!(line_for(largest + 1).is_err());

        // A running engine's input, so the refusal is the size check's alone:
        // without one, any request fails as "not running" whatever its size.
        let client = client_with_last_state(PlaybackState::default());
        let (sender, written) = mpsc::sync_channel::<InputLine>(4);
        *client.stdin.lock() = Some(EngineInput { generation: 1, sender });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let queue = vec![Track { name: "x".repeat(MAX_REQUEST_BYTES), ..Track::default() }];
        let error = runtime.block_on(client.play_queue(&queue, 0, 0, "", false, false)).unwrap_err();
        assert!(error.starts_with("This queue is too large for the playback engine"), "{error}");
        assert!(error.contains("16 MB limit"), "the message names the limit: {error}");
        assert!(written.try_recv().is_err(), "an oversized line is never handed to the engine");
        assert!(client.pending.lock().is_empty(), "nothing waits for an answer that cannot come");

        // The same input carries a request that fits.
        let small = vec![Track { name: "x".into(), ..Track::default() }];
        let request = runtime.block_on(async {
            let sending = client.play_queue(&small, 0, 0, "", false, false);
            tokio::pin!(sending);
            assert!(futures_poll_once(sending.as_mut()).await.is_none(), "a fitting request waits for its reply");
            written.try_recv().expect("a fitting request is written")
        });
        assert!(request.line.contains("\"type\":\"play_queue\""));
    }

    /// Polls `future` once, answering its output if it was already done.
    async fn futures_poll_once<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) -> Option<F::Output> {
        std::future::poll_fn(|cx| std::task::Poll::Ready(match future.as_mut().poll(cx) {
            std::task::Poll::Ready(output) => Some(output),
            std::task::Poll::Pending => None,
        })).await
    }
}
