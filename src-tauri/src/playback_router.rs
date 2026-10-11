//! One output owner for UI commands and system media keys.
//!
//! While a Spotify device plays, the engine stays paused and remains the only
//! queue model: every edit goes to it. Spotify holds a window of the engine's
//! order as the `uris` of one play request, so the device can play through and
//! use its own Next button. Replacing a window can rebuffer: Spotify provides
//! no atomic queue replacement or ordering guarantee across Player endpoints.
//! The router keeps an occurrence cursor, never a second durable queue.
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::app::AppState;
use crate::engine_client::{EngineClient, StateLine};
use crate::media_keys;
use crate::personal_api::{self, player_path, Device, PersonalApi, PlayerState};
use crate::types::{PlaybackEvent, PlaybackState, Track};

/// Spotify starts at most 100 `uris` from one play request.
const WINDOW: usize = 100;
/// A track change extends the window once fewer entries than this remain.
const LOW_WATER: usize = 10;
/// Spotify can keep reporting the previous item for a few seconds after a
/// command; until then a disagreeing reading is retried, not believed.
const GRACE: Duration = Duration::from_secs(8);
/// While the window is visible the device is read this often, playing or paused.
const VISIBLE_POLL: Duration = Duration::from_secs(4);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A start Spotify has not accepted by then is dropped: the selection waits
/// for Play instead of starting long after it was made.
const START_DEADLINE: Duration = Duration::from_secs(90);
/// The last window entry counts as finished this close to its projected end.
const END_SLACK_MS: u32 = 1500;
/// A reading this close to the projected playhead is no news.
const DRIFT_MS: u32 = 1500;
/// Revisions cross the JavaScript bridge as exactly representable integers.
const MAX_REVISION: u64 = (1 << 53) - 1;
const LOCAL_ONLY: &str = "Playback speed and track-editor previews require This computer; Spotify devices play original audio";
const FOREIGN_QUEUE: &str = "This queue belongs to the Spotify app on that device; edit it there, or play something from here";
const MOVED_AWAY: &str = "Spotify playback moved away from the selected device; reselect a device or This computer";
const RELEASED: &str = "The selected output was released";
const DEVICES_OFF: &str = "Spotify devices are turned off in Settings";

pub enum Action {
    Play, Pause, Next, Previous, Seek(u32), Volume(u8), Shuffle(bool), Repeat(String), Speed(f32),
    Queue(Vec<Track>, usize, String, bool), Index(usize), Add(Track, String), AddBatch(Vec<Track>, String), Remove(usize), Move(usize, usize),
    Preview(Track, Vec<renderer_engine::protocol::TimeRange>, Option<renderer_engine::protocol::LoopRange>, u32, u64),
    RestorePreview(u64),
}

/// A queue command for the paused engine. None of these load audio, except
/// that removing the engine's own current row loads its replacement paused.
pub(crate) enum EngineOp<'a> {
    Restore { queue: &'a [Track], index: usize, position_ms: u32, context: &'a str, automatic: bool },
    Cursor { index: usize, position_ms: u32 },
    Add(&'a Track, &'a str),
    AddBatch(&'a [Track], &'a str),
    Remove(usize),
    Move(usize, usize),
    Shuffle(bool),
    Repeat(&'a str),
}

/// The engine after an operation, read consistently with its order.
pub(crate) struct EngineView {
    /// The state the operation produced; `None` when it changed nothing.
    state: Option<PlaybackState>,
    /// The engine's order after its own current row.
    upcoming: Vec<usize>,
}

/// Everything the router does outside itself, so the decisions can be driven
/// against a local HTTP mock and an in-memory engine in tests.
pub(crate) trait Host: Sync {
    /// One Spotify Web API request; `None` is an empty answer.
    fn spotify(&self, method: Method, path: String, body: Option<Value>) -> impl Future<Output = Result<Option<Value>, String>> + Send;
    /// Applies `op` to the engine queue, then reads the engine back.
    fn engine(&self, op: Option<EngineOp<'_>>) -> impl Future<Output = Result<EngineView, String>> + Send;
    fn exclusions(&self, context: &str) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    fn pause_engine(&self) -> impl Future<Output = Result<(), String>> + Send;
    fn local(&self, action: Action) -> impl Future<Output = Result<(), String>> + Send;
    fn local_state(&self) -> PlaybackState;
    /// Shows the engine's own state, rows included.
    fn publish_local(&self);
    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S);
    fn media(&self, state: &PlaybackState);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind { Prepared, Owned, Foreign }

enum Mode {
    /// The paused local queue waits for Play before Spotify hears anything.
    Prepared,
    /// Spotify plays a window of the engine's order.
    Owned(Window),
    /// Something else plays there (the phone started it): shown, not managed.
    Foreign,
}

/// The URIs Spotify holds from the router's last play request.
struct Window {
    /// The engine row behind each URI; `None` once that row was removed.
    rows: Vec<Option<usize>>,
    uris: Vec<String>,
    /// The entry Spotify plays.
    at: usize,
    /// A paused edit changed the order; the next Play sends it.
    stale: bool,
    /// A newer selection's start expired: entries that name no row are shown
    /// as staged from now on, never restarted.
    expired: bool,
}

/// What the router's own last command should make Spotify report.
struct Pending {
    since: Instant,
    uri: Option<String>,
    playing: Option<bool>,
    position_ms: Option<u32>,
    retries: u32,
}

/// Desired start, retained until Spotify accepts its window. It is never an
/// occurrence mapping for the previously applied window.
struct StartIntent {
    current: usize,
    position_ms: u32,
    /// When the selection was made; retries keep it, see [`START_DEADLINE`].
    since: Instant,
}

/// What the webview and the media session were last shown, so a reading that
/// only confirms it publishes nothing.
struct Shown {
    playing: bool,
    volume: u8,
    shuffle: bool,
    repeat: String,
    error: String,
    /// The playhead shown, and when.
    position_ms: u32,
    at: Instant,
    /// Index, URI, duration and device name; a transport-only publish leaves
    /// them as they were.
    row: Option<(Option<usize>, String, u32, String)>,
}

struct Remote {
    /// What the window shows. While the engine's order plays, `queue` is the
    /// engine's rows, `current_index` the device's row and `upcoming` the
    /// order from it.
    state: PlaybackState,
    mode: Mode,
    /// The engine's own current row. It stays put while the device advances.
    anchor: Option<usize>,
    /// The engine's order after `anchor`.
    engine_revision: u64,
    plan: Vec<usize>,
    /// Rows the device has played since `anchor`: shuffle's spent draws.
    consumed: HashSet<usize>,
    supports_volume: bool,
    anchored: Instant,
    published_queue: Option<u64>,
    published_order: Option<u64>,
    shown: Option<Shown>,
    polling: bool,
    /// Only child disconnection is automatically recoverable; a moved or
    /// restricted Spotify output still requires an explicit selection.
    engine_recovery: bool,
    recovery_ready: bool,
    pending: Option<Pending>,
    start_intent: Option<StartIntent>,
    failures: u32,
    repeat_dirty: bool,
    pause_dirty: bool,
    /// The window was looked at again: read the device now, Spotify's own
    /// queue included.
    refresh: bool,
}

#[derive(Clone, Copy)]
enum Remap { Keep, Remove(usize), Move(usize, usize) }

impl Remap {
    /// Where engine row `row` sits after the edit; `None` once it is gone.
    fn apply(self, row: usize) -> Option<usize> {
        match self {
            Remap::Keep => Some(row),
            Remap::Remove(removed) => match row.cmp(&removed) {
                std::cmp::Ordering::Less => Some(row),
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some(row - 1),
            },
            Remap::Move(from, to) => Some(if row == from { to }
                else if from < row && row <= to { row - 1 }
                else if to <= row && row < from { row + 1 }
                else { row }),
        }
    }
}

impl Remote {
    fn new(state: PlaybackState, supports_volume: bool) -> Self {
        Self {
            anchor: state.current_index,
            engine_revision: state.queue_revision,
            plan: state.upcoming.clone(),
            state,
            mode: Mode::Prepared,
            consumed: HashSet::new(),
            supports_volume,
            anchored: Instant::now(),
            published_queue: None,
            published_order: None,
            shown: None,
            polling: false,
            engine_recovery: false,
            recovery_ready: false,
            pending: None,
            start_intent: None,
            failures: 0,
            repeat_dirty: false,
            pause_dirty: false,
            refresh: false,
        }
    }

    fn kind(&self) -> Kind {
        match self.mode { Mode::Prepared => Kind::Prepared, Mode::Owned(_) => Kind::Owned, Mode::Foreign => Kind::Foreign }
    }

    fn position(&self) -> u32 { projected_position(&self.state, self.anchored.elapsed()) }

    fn rebase(&mut self) {
        self.state.position_ms = self.position();
        self.anchored = Instant::now();
    }

    /// Records a publish; `full` when it carried the row fields too.
    fn record_shown(&mut self, full: bool) {
        let state = &self.state;
        let row = if full {
            Some((state.current_index, state.current_uri.clone(), state.duration_ms, state.output_device_name.clone()))
        } else {
            self.shown.take().and_then(|shown| shown.row)
        };
        self.shown = Some(Shown { playing: state.playing, volume: state.volume, shuffle: state.shuffle, repeat: state.repeat.clone(),
            error: state.error.clone(), position_ms: state.position_ms, at: self.anchored, row });
    }

    /// Whether everything last shown still holds: the same rows, row and
    /// settings, and a playhead where the shown one projects.
    fn still_shown(&self) -> bool {
        let Some(shown) = &self.shown else { return false; };
        let state = &self.state;
        shown.row.as_ref().is_some_and(|(index, uri, duration, device)| *index == state.current_index
            && *uri == state.current_uri && *duration == state.duration_ms && *device == state.output_device_name)
            && self.published_queue == Some(state.queue_revision) && self.published_order == Some(state.order_revision)
            && shown.playing == state.playing && shown.volume == state.volume && shown.shuffle == state.shuffle
            && shown.repeat == state.repeat && shown.error == state.error
            && project(shown.position_ms, shown.playing, state.duration_ms, shown.at.elapsed()).abs_diff(self.position()) <= DRIFT_MS
    }

    /// The rows after the device's row that Spotify can play, as many as fit.
    /// Under sequential repeat the lap is cycled to fill them, so a short
    /// queue does not need a new window, and a rebuffer, at every track.
    fn window_tail(&self, order: &[usize]) -> Vec<usize> {
        window_tail(&self.state.queue, order, !self.state.shuffle && self.state.repeat == "context")
    }

    /// Spotify plays the window in order; only repeat-one is its own job.
    fn spotify_repeat(&self) -> &'static str { if self.state.repeat == "track" { "track" } else { "off" } }

    fn device_path(&self, endpoint: &str, query: &[(&str, String)]) -> Result<String, String> {
        player_path(&self.state.output_device_id, endpoint, query)
    }

    fn expect(&mut self, uri: Option<String>, playing: Option<bool>) {
        self.pending = Some(Pending { since: Instant::now(), uri, playing, position_ms: None, retries: 0 });
        self.polling = true;
    }

    /// Points the view at row `row` of the rows it shows.
    fn show_row(&mut self, row: Option<usize>) {
        self.state.current_index = row.filter(|row| *row < self.state.queue.len());
        match self.state.current_index.map(|row| &self.state.queue[row]) {
            Some(track) => {
                self.state.current_uri.clone_from(&track.uri);
                self.state.context.clone_from(&track.context);
                self.state.duration_ms = track.duration_ms;
            }
            None => {
                self.state.current_uri.clear();
                self.state.context.clear();
                self.state.duration_ms = 0;
            }
        }
    }

    fn adopt_engine(&mut self, view: EngineView) {
        if let Some(engine) = view.state {
            // An unchanged revision names rows the view already holds. An
            // unnamed (zero) revision, or a view of the phone's own rows,
            // takes the engine's rows.
            if engine.queue_revision == 0 || engine.queue_revision != self.engine_revision || self.kind() == Kind::Foreign {
                self.state.queue = engine.queue;
                self.state.queue_revision = next_revision(self.state.queue_revision);
            }
            self.engine_revision = engine.queue_revision;
            self.anchor = engine.current_index;
            self.state.shuffle = engine.shuffle;
            self.state.repeat = engine.repeat;
        }
        self.plan = view.upcoming;
    }

    fn set_order(&mut self, order: Vec<usize>) {
        if self.state.upcoming != order {
            self.state.upcoming = order;
            self.state.order_revision = next_revision(self.state.order_revision);
        }
    }

    fn order_from(&self, current: Option<usize>, cut: usize) -> Option<Vec<usize>> {
        order_from(self.anchor, &self.plan, self.state.shuffle, &self.state.repeat, current, cut, &self.consumed)
    }

    /// Shuffle with repeat has drawn every row of this lap: a new lap is a new
    /// draw, which only the engine makes, and only from a fresh anchor.
    fn lap_spent(&self, current: Option<usize>) -> bool {
        self.state.shuffle && self.state.repeat == "context" && (self.anchor != current || !self.consumed.is_empty())
    }

    fn adopt_transport(&mut self, player: &PlayerState) {
        self.state.playing = player.is_playing;
        self.state.position_ms = player.progress_ms.unwrap_or(0);
        self.state.volume = player.device.volume_percent.unwrap_or(self.state.volume);
        self.state.output_device_name.clone_from(&player.device.name);
        self.state.error.clear();
        self.supports_volume = player.device.supports_volume;
        self.anchored = Instant::now();
    }

    /// Shows the device's item followed by Spotify's own queue.
    fn show_foreign(&mut self, player: Option<&PlayerState>, queue: &[Value]) {
        let current = player.and_then(|player| player.item.as_ref()).and_then(|item| track_from_player(item).ok());
        let offset = usize::from(current.is_some());
        let mut rows = Vec::with_capacity(queue.len() + offset);
        rows.extend(current);
        rows.extend(queue.iter().filter_map(|item| track_from_player(item).ok()));
        if *self.state.queue != *rows {
            self.state.queue = rows.into();
            self.state.queue_revision = next_revision(self.state.queue_revision);
        }
        self.show_row((offset == 1).then_some(0));
        self.state.context.clear();
        self.set_order((offset..self.state.queue.len()).collect());
        if let Some(duration) = player.and_then(|player| player.item.as_ref()).and_then(|item| item["duration_ms"].as_u64()) {
            self.state.duration_ms = duration.min(u64::from(u32::MAX)) as u32;
        }
    }
}

impl Pending {
    fn met_by(&self, player: Option<&PlayerState>, repeat: Option<&str>) -> bool {
        let Some(player) = player else { return false; };
        self.uri.as_deref().is_none_or(|uri| item_uri(player) == uri)
            && self.playing.is_none_or(|playing| player.is_playing == playing)
            && self.position_ms.is_none_or(|position| player.progress_ms.is_some_and(|progress| {
                let elapsed = if player.is_playing { self.since.elapsed().as_millis().min(u32::MAX as u128) as u32 } else { 0 };
                progress.abs_diff(position.saturating_add(elapsed)) <= 3000
            }))
            && repeat.is_none_or(|repeat| !player.shuffle_state && player.repeat_state == repeat)
    }
}

pub struct PlaybackRouter {
    client: Arc<EngineClient>,
    personal: Arc<PersonalApi>,
    core: Core,
}

impl PlaybackRouter {
    pub fn new(client: Arc<EngineClient>, personal: Arc<PersonalApi>, devices_enabled: bool) -> Arc<Self> {
        let core = Core::default();
        core.enabled.store(devices_enabled, Ordering::Relaxed);
        Arc::new(Self { client, personal, core })
    }

    fn host<'a>(&'a self, app: &'a AppHandle) -> AppHost<'a> {
        AppHost { app, client: &self.client, personal: &self.personal }
    }

    pub fn is_remote(&self) -> bool { self.core.remote.lock().is_some() }

    /// Whether Spotify devices are on (Settings). Off, the router serves only
    /// This computer and nothing in it wakes.
    pub fn devices_enabled(&self) -> bool { self.core.enabled.load(Ordering::Relaxed) }

    /// Turning devices off first brings playback on a device back here,
    /// paused, through the one hand-back path; the grant is left alone.
    pub async fn set_devices_enabled(&self, app: &AppHandle, enabled: bool) {
        self.core.set_enabled(&self.host(app), enabled).await;
    }

    pub fn snapshot(&self) -> Option<PlaybackState> { self.core.snapshot() }

    pub fn apply_cache_marks(&self, reset: bool, removed: &[String], added: &[String]) {
        self.core.apply_cache_marks(reset, removed, added);
    }

    pub fn report_error(&self, app: &AppHandle, error: &str) { self.core.report_error(&self.host(app), error); }

    pub fn engine_disconnected(&self, app: &AppHandle, error: &str) {
        self.core.engine_disconnected(&self.host(app), error);
    }

    pub async fn engine_recovered(&self, app: &AppHandle, state: &PlaybackState) {
        self.core.engine_recovered(&self.host(app), state).await;
    }

    pub async fn select(&self, app: &AppHandle, device_id: Option<String>) -> Result<(), String> {
        self.core.select(&self.host(app), device_id).await
    }

    /// Disconnect never resumes local audio or leaves a dormant remote timer.
    pub async fn disconnect(&self, app: &AppHandle) { self.core.disconnect(&self.host(app)).await; }

    pub async fn run(&self, app: &AppHandle, action: Action) -> Result<(), String> {
        self.core.run(&self.host(app), action).await
    }

    /// Re-read the engine's authoritative order after eligibility metadata
    /// changes. A failed Spotify write is reported here and retried from a
    /// stale window; it never makes the saved change look failed.
    pub async fn eligibility_changed(&self, app: &AppHandle) {
        self.core.eligibility_changed(&self.host(app)).await;
    }

    /// The watcher reads a Spotify device regularly only while the window is
    /// visible; `refresh` reads it at once.
    pub fn set_visible(&self, visible: bool, refresh: bool) { self.core.set_visible(visible, refresh); }

    pub async fn watch(self: Arc<Self>, app: AppHandle) {
        let host = self.host(&app);
        self.core.watch(&host).await;
    }
}

struct Core {
    operation: AsyncMutex<()>,
    remote: Mutex<Option<Remote>>,
    changed: Notify,
    /// Whether the window can be seen; it decides how often a device is read.
    visible: AtomicBool,
    /// Whether Spotify devices are on at all.
    enabled: AtomicBool,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            operation: AsyncMutex::new(()), remote: Mutex::new(None), changed: Notify::new(),
            visible: AtomicBool::new(true), enabled: AtomicBool::new(true),
        }
    }
}

impl Core {
    fn snapshot(&self) -> Option<PlaybackState> {
        self.remote.lock().as_ref().map(|remote| {
            let mut state = remote.state.clone();
            state.position_ms = remote.position();
            state
        })
    }

    fn apply_cache_marks(&self, reset: bool, removed: &[String], added: &[String]) {
        if !reset && removed.is_empty() && added.is_empty() { return; }
        let mut guard = self.remote.lock();
        let Some(remote) = guard.as_mut() else { return; };
        let cached = |track: &Track| {
            added.contains(&track.id) || (!reset && track.cached && !removed.contains(&track.id))
        };
        if remote.state.queue.iter().any(|track| track.cached != cached(track)) {
            for track in Arc::make_mut(&mut remote.state.queue) { track.cached = cached(track); }
        }
        // These are one-shot wire lanes, not part of a remote snapshot.
        // Replaying an older positive lane could revive a later eviction.
        remote.state.cached_ids = None;
        remote.state.uncached_ids = None;
    }

    fn publish<H: Host>(&self, host: &H) {
        let mut guard = self.remote.lock();
        let Some(remote) = guard.as_mut() else { return; };
        remote.rebase();
        let include_queue = remote.published_queue != Some(remote.state.queue_revision);
        let include_order = remote.published_order != Some(remote.state.order_revision);
        host.emit("state", PlaybackEvent::new(&remote.state, include_queue, include_order));
        remote.published_queue = Some(remote.state.queue_revision);
        remote.published_order = Some(remote.state.order_revision);
        host.media(&remote.state);
        remote.record_shown(true);
    }

    /// Transport-only changes must not serialize a long queue or its order.
    fn publish_transport<H: Host>(&self, host: &H) {
        let mut guard = self.remote.lock();
        let Some(remote) = guard.as_mut() else { return; };
        remote.rebase();
        let state = &remote.state;
        host.emit("state", json!({ "playing": state.playing, "position_ms": state.position_ms, "volume": state.volume,
            "shuffle": state.shuffle, "repeat": state.repeat, "error": state.error }));
        host.media(state);
        remote.record_shown(false);
    }

    /// A reading publishes only what it changed: a confirmed playhead is no
    /// news to the webview or the media session.
    fn publish_changed<H: Host>(&self, host: &H) {
        if self.remote.lock().as_ref().is_some_and(|remote| !remote.still_shown()) { self.publish(host); }
    }

    fn set_visible(&self, visible: bool, refresh: bool) {
        let was = self.visible.swap(visible, Ordering::Relaxed);
        // With devices off there is no device to read and nothing to wake.
        if !self.enabled.load(Ordering::Relaxed) { return; }
        // Coming back into view reads the device at once: a hidden, paused
        // window read nothing meanwhile.
        let refresh = visible && (refresh || !was);
        if refresh {
            if let Some(remote) = self.remote.lock().as_mut() { remote.refresh = true; }
        }
        if refresh || was != visible { self.changed.notify_one(); }
    }

    fn report_error<H: Host>(&self, host: &H, error: &str) {
        let remote = match self.remote.lock().as_mut() {
            Some(remote) => { remote.state.error = error.to_owned(); true }
            None => false,
        };
        host.emit("playback-action-error", error);
        self.publish_transport(host);
        // Only a device's watcher has anything to reconsider.
        if remote { self.changed.notify_one(); }
    }

    fn suspend<H: Host>(&self, host: &H, error: &str) {
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.polling = false;
            remote.engine_recovery = false;
            remote.recovery_ready = false;
        }
        self.report_error(host, error);
    }

    fn engine_disconnected<H: Host>(&self, host: &H, error: &str) {
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.engine_recovery |= remote.polling || remote.kind() == Kind::Prepared;
            remote.polling = false;
            remote.recovery_ready = false;
        }
        self.report_error(host, error);
    }

    async fn engine_recovered<H: Host>(&self, host: &H, state: &PlaybackState) {
        if state.auth_state != "ready" { return; }
        let _operation = self.operation.lock().await;
        let eligible = self.remote.lock().as_mut().is_some_and(|remote| {
            if !remote.engine_recovery || remote.state.username != state.username { return false; }
            remote.polling = true;
            remote.recovery_ready = true;
            remote.failures = 0;
            remote.refresh = true;
            true
        });
        if !eligible { return; }
        self.reconcile(host).await;
        self.changed.notify_one();
    }

    async fn recover_engine<H: Host>(&self, host: &H) -> Result<(), String> {
        // A restored child must never become a second audible output.
        host.pause_engine().await?;
        let mut view = host.engine(None).await?;
        let restored = view.state.get_or_insert_with(|| host.local_state());
        {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            if restored.auth_state != "ready" || restored.username != remote.state.username {
                return Err("The engine changed account during output recovery".into());
            }
        }
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            if remote.kind() != Kind::Foreign {
                // The new engine may refresh metadata or redraw shuffle.
                // Preserve occurrences, not indexes into obsolete rows.
                let old_current = remote.state.current_index;
                let remap: Vec<Option<usize>> = {
                    let mut occurrences: HashMap<&str, VecDeque<usize>> = HashMap::new();
                    for (index, track) in restored.queue.iter().enumerate() {
                        occurrences.entry(&track.uri).or_default().push_back(index);
                    }
                    remote.state.queue.iter()
                        .map(|track| occurrences.get_mut(track.uri.as_str()).and_then(VecDeque::pop_front))
                        .collect()
                };
                let current = old_current.and_then(|row| remap.get(row).copied().flatten());
                if let Mode::Owned(window) = &mut remote.mode {
                    for row in &mut window.rows {
                        *row = row.and_then(|row| remap.get(row).copied().flatten());
                    }
                }
                if let Some(intent) = remote.start_intent.as_mut() {
                    if let Some(current) = remap.get(intent.current).copied().flatten() {
                        intent.current = current;
                    } else { remote.start_intent = None; }
                }
                let repeat_before = remote.spotify_repeat();
                remote.adopt_engine(view);
                remote.consumed.clear();
                remote.repeat_dirty |= remote.spotify_repeat() != repeat_before;
                remote.show_row(current.or(remote.anchor));
                if old_current.is_some() && current.is_none() { remote.state.position_ms = 0; }
                if remote.kind() == Kind::Prepared { remote.set_order(remote.plan.clone()); }
            }
            remote.pending = None;
        }
        self.sync(host).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.engine_recovery = false;
            remote.recovery_ready = false;
            remote.state.error.clear();
        }
        self.publish_changed(host);
        Ok(())
    }

    async fn command<H: Host>(&self, host: &H, method: Method, endpoint: &str, query: &[(&str, String)], body: Option<Value>) -> Result<(), String> {
        let path = self.remote.lock().as_ref().ok_or(RELEASED)?.device_path(endpoint, query)?;
        host.spotify(method, path, body).await.map(drop)
    }

    async fn select<H: Host>(&self, host: &H, device_id: Option<String>) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let result = self.select_locked(host, device_id).await;
        if let Err(error) = &result { self.report_error(host, error); }
        // The watcher plans only for a device; a hand-back already woke it.
        if self.remote.lock().is_some() { self.changed.notify_one(); }
        result
    }

    async fn select_locked<H: Host>(&self, host: &H, device_id: Option<String>) -> Result<(), String> {
        let Some(device_id) = device_id.filter(|id| !id.is_empty()) else {
            return self.return_local(host).await;
        };
        if !self.enabled.load(Ordering::Relaxed) { return Err(DEVICES_OFF.into()); }
        player_path(&device_id, "play", &[])?;
        let selected = self.remote.lock().as_ref().map(|remote| (remote.state.output_device_id == device_id, remote.kind(), remote.state.playing));
        if let Some((true, kind, _)) = selected {
            let recovery = self.remote.lock().as_ref().filter(|remote| remote.engine_recovery)
                .map(|remote| remote.recovery_ready);
            if let Some(ready) = recovery {
                if !ready { return Err("Playback engine recovery is still in progress".into()); }
                return self.recover_engine(host).await;
            }
            // Reselecting a staged queue must not adopt an unrelated session there.
            return if kind == Kind::Prepared { Ok(()) } else { self.sync(host).await };
        }
        let devices = host.spotify(Method::GET, "me/player/devices".into(), None).await?.unwrap_or_default();
        let devices: Vec<Device> = serde_json::from_value(devices["devices"].clone())
            .map_err(|_| "Spotify returned an invalid devices response".to_owned())?;
        let device = devices.into_iter().find(|device| device.id.as_deref() == Some(&device_id))
            .ok_or("Spotify device is unavailable; open Spotify there and refresh devices")?;
        if device.is_restricted { return Err("Spotify does not allow controlling this device".into()); }
        if let Some((_, kind, playing)) = selected {
            // Spotify moves its whole session — window, position and all.
            let play = playing && kind != Kind::Prepared;
            host.spotify(Method::PUT, "me/player".into(), Some(json!({ "device_ids": [device_id], "play": play }))).await?;
            {
                let mut guard = self.remote.lock();
                let remote = guard.as_mut().ok_or(RELEASED)?;
                remote.state.output_device_id = device_id;
                remote.state.output_device_name = device.name;
                remote.supports_volume = device.supports_volume;
                remote.state.volume = device.volume_percent.unwrap_or(remote.state.volume);
                if kind != Kind::Prepared { remote.expect(None, Some(play)); }
            }
            self.publish(host);
            return Ok(());
        }
        let mut state = host.local_state();
        if state.preview { return Err("Return to This computer and finish the track preview before selecting another output".into()); }
        // Await the actual engine pause before any remote transfer or start.
        host.pause_engine().await?;
        host.spotify(Method::PUT, "me/player".into(), Some(json!({ "device_ids": [device_id], "play": false }))).await?;
        let view = host.engine(None).await?;
        let was_playing = state.playing;
        state.output_device_id = device_id;
        state.output_device_name = device.name;
        state.volume = device.volume_percent.unwrap_or(state.volume);
        state.buffering = false;
        state.playing = false;
        state.playback_speed = 1.0;
        state.audible_playback_speed = 1.0;
        state.error.clear();
        let position_ms = state.position_ms;
        let mut remote = Remote::new(state, device.supports_volume);
        remote.adopt_engine(view);
        remote.show_row(remote.anchor);
        remote.set_order(remote.plan.clone());
        let current = remote.anchor;
        if current.is_none() {
            // Nothing local to hand over: show what the device plays.
            remote.mode = Mode::Foreign;
            remote.polling = true;
        }
        *self.remote.lock() = Some(remote);
        self.publish(host);
        match current {
            Some(current) if was_playing => self.start(host, current, position_ms).await,
            Some(_) => Ok(()),
            None => self.sync(host).await,
        }
    }

    async fn return_local<H: Host>(&self, host: &H) -> Result<(), String> {
        let Some(kind) = self.remote.lock().as_ref().map(Remote::kind) else { return Ok(()); };
        // Returning is deliberately paused, never an automatic local fallback.
        let needs_pause = kind != Kind::Prepared || self.remote.lock().as_ref().is_some_and(|remote| remote.pause_dirty || remote.start_intent.is_some());
        let stopped = if needs_pause { self.command(host, Method::PUT, "pause", &[], None).await } else { Ok(()) };
        let Some(remote) = self.remote.lock().take() else { return Ok(()); };
        self.changed.notify_one();
        // Resume here where the device was only if it played the engine's
        // order. Anything the phone started leaves the local queue alone.
        let resume = match (&remote.mode, remote.state.current_index) {
            (Mode::Owned(_), Some(row)) => Some((row, remote.position())),
            _ => None,
        };
        let restored = match resume {
            Some((index, position_ms)) => host.engine(Some(EngineOp::Cursor { index, position_ms })).await.map(drop),
            None => Ok(()),
        };
        // A restore publishes the engine's own state through the normal path.
        if resume.is_none() || restored.is_err() { host.publish_local(); }
        restored?;
        stopped.map_err(|error| format!("Could not pause the previous output: {error}. Stop it in Spotify before resuming here."))
    }

    async fn disconnect<H: Host>(&self, host: &H) {
        let _operation = self.operation.lock().await;
        if let Err(error) = self.return_local(host).await { self.report_error(host, &error); }
    }

    /// Off is set before the hand-back, under the same operation lock, so no
    /// selection can slip in between; the hand-back itself is `disconnect`'s.
    async fn set_enabled<H: Host>(&self, host: &H, enabled: bool) {
        let _operation = self.operation.lock().await;
        self.enabled.store(enabled, Ordering::Relaxed);
        if enabled { return; }
        if let Err(error) = self.return_local(host).await { self.report_error(host, &error); }
    }

    async fn run<H: Host>(&self, host: &H, action: Action) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let recovery = self.remote.lock().as_ref()
            .filter(|remote| remote.engine_recovery).map(|remote| remote.recovery_ready);
        if let Some(ready) = recovery {
            let recovered = if ready { self.recover_engine(host).await }
                else { Err("Playback engine recovery is still in progress".to_owned()) };
            if let Err(error) = recovered {
                self.report_error(host, &error);
                return Err(error);
            }
        }
        let kind = self.remote.lock().as_ref().map(Remote::kind);
        let result = match kind {
            None => host.local(action).await,
            Some(kind) => self.run_remote(host, kind, action).await,
        };
        if let Err(error) = &result { self.report_error(host, error); }
        else if kind.is_some() {
            let recovered = self.remote.lock().as_mut().is_some_and(|remote| {
                let unapplied = remote.start_intent.is_some() || remote.repeat_dirty || remote.pause_dirty
                    || matches!(&remote.mode, Mode::Owned(window) if window.stale);
                if remote.state.error.is_empty() || unapplied { return false; }
                remote.state.error.clear();
                true
            });
            if recovered { self.publish_transport(host); }
        }
        if kind.is_some() { self.changed.notify_one(); }
        result
    }

    async fn run_remote<H: Host>(&self, host: &H, kind: Kind, action: Action) -> Result<(), String> {
        let foreign = kind == Kind::Foreign;
        match action {
            Action::Speed(_) | Action::Preview(..) | Action::RestorePreview(_) => Err(LOCAL_ONLY.into()),
            Action::Volume(percent) => self.volume(host, percent).await,
            Action::Seek(position_ms) => self.seek(host, kind, position_ms).await,
            Action::Pause => self.pause(host, kind).await,
            Action::Play => self.play(host, kind).await,
            Action::Next => self.next(host, kind).await,
            Action::Previous => self.previous(host, kind).await,
            Action::Shuffle(enabled) if foreign => self.foreign_setting(host, "shuffle", enabled.to_string()).await,
            Action::Shuffle(enabled) => self.edit(host, kind, EngineOp::Shuffle(enabled)).await,
            Action::Repeat(mode) => {
                if !matches!(mode.as_str(), "off" | "context" | "track") { return Err("invalid repeat mode".into()); }
                if foreign { self.foreign_setting(host, "repeat", mode).await } else { self.edit(host, kind, EngineOp::Repeat(&mode)).await }
            }
            Action::Queue(queue, index, context, automatic) => self.play_queue(host, queue, index, context, automatic).await,
            Action::Index(index) if foreign => self.play_foreign_row(host, index).await,
            Action::Index(index) => self.jump(host, index).await,
            Action::Add(track, _) if foreign => self.queue_on_spotify(host, std::slice::from_ref(&track)).await,
            Action::AddBatch(tracks, _) if foreign => self.queue_on_spotify(host, &tracks).await,
            Action::Add(track, context) => self.edit(host, kind, EngineOp::Add(&track, &context)).await,
            Action::AddBatch(tracks, _) if tracks.is_empty() => Ok(()),
            Action::AddBatch(tracks, context) => self.edit(host, kind, EngineOp::AddBatch(&tracks, &context)).await,
            Action::Remove(_) | Action::Move(..) if foreign => Err(FOREIGN_QUEUE.into()),
            Action::Remove(index) => self.edit(host, kind, EngineOp::Remove(index)).await,
            Action::Move(from, to) => self.edit(host, kind, EngineOp::Move(from, to)).await,
        }
    }

    async fn volume<H: Host>(&self, host: &H, percent: u8) -> Result<(), String> {
        if percent > 100 { return Err("volume percent must be between 0 and 100".into()); }
        if !self.remote.lock().as_ref().is_some_and(|remote| remote.supports_volume) {
            return Err("This Spotify device does not support remote volume control".into());
        }
        self.command(host, Method::PUT, "volume", &[("volume_percent", percent.to_string())], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() { remote.state.volume = percent; }
        self.publish_transport(host);
        Ok(())
    }

    async fn seek<H: Host>(&self, host: &H, kind: Kind, position_ms: u32) -> Result<(), String> {
        let duration = self.remote.lock().as_ref().map_or(0, |remote| remote.state.duration_ms);
        let position_ms = if duration == 0 { position_ms } else { position_ms.min(duration) };
        let staged = kind == Kind::Prepared || self.remote.lock().as_ref().is_some_and(|remote| remote.start_intent.is_some());
        if !staged {
            self.command(host, Method::PUT, "seek", &[("position_ms", position_ms.to_string())], None).await?;
        }
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.state.position_ms = position_ms;
            remote.anchored = Instant::now();
            if let Some(intent) = remote.start_intent.as_mut() { intent.position_ms = position_ms; }
            if !staged {
                remote.expect(Some(remote.state.current_uri.clone()), None);
                if let Some(pending) = remote.pending.as_mut() { pending.position_ms = Some(position_ms); }
            }
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn pause<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        let needs_pause = kind != Kind::Prepared || self.remote.lock().as_ref().is_some_and(|remote| remote.start_intent.is_some());
        if needs_pause { self.command(host, Method::PUT, "pause", &[], None).await?; }
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.start_intent = None;
            remote.rebase();
            remote.state.playing = false;
            // A staged queue with its start cancelled has nothing left to watch.
            if kind != Kind::Prepared { remote.expect(None, Some(false)); } else { remote.polling = remote.pause_dirty; }
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn play<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        let (current, position_ms, stale) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let stale = matches!(&remote.mode, Mode::Owned(window) if window.stale);
            (remote.state.current_index, remote.state.position_ms, stale)
        };
        if kind == Kind::Prepared || stale {
            return self.start(host, current.ok_or("No track is selected")?, position_ms).await;
        }
        self.command(host, Method::PUT, "play", &[], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.rebase();
            remote.state.playing = true;
            remote.expect(None, Some(true));
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn next<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        if kind == Kind::Foreign {
            self.command(host, Method::POST, "next", &[], None).await?;
            if let Some(remote) = self.remote.lock().as_mut() {
                // The phone's queue is shown in Spotify's order: its next row is due.
                let next = remote.state.current_index.and_then(|row| remote.state.queue.get(row + 1)).map(|track| track.uri.clone());
                remote.expect(next, None);
            }
            return Ok(());
        }
        let (current, step) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let step = match &remote.mode {
                Mode::Owned(window) if !window.stale => window.rows.get(window.at + 1).copied().flatten()
                    .map(|row| (row, window.uris[window.at + 1].clone())),
                _ => None,
            };
            (remote.state.current_index, step)
        };
        let Some(current) = current else { return Ok(()); };
        let Some((row, uri)) = step else {
            // Nothing staged next on the device: send the order from the next row.
            let order = self.order(host, Some(current), current).await?;
            return match order.first() {
                Some(&next) => self.start(host, next, 0).await,
                None => self.finish(host).await,
            };
        };
        self.command(host, Method::POST, "next", &[], None).await?;
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let duplicate = remote.state.current_uri == uri;
            if let Mode::Owned(window) = &mut remote.mode { window.at += 1; }
            remote.consumed.insert(row);
            remote.show_row(Some(row));
            remote.state.position_ms = 0;
            remote.state.playing = true;
            remote.anchored = Instant::now();
            // Devices differ on whether a skip while paused resumes; the
            // reading settles that.
            remote.expect(Some(uri), None);
            if duplicate {
                if let Some(pending) = remote.pending.as_mut() { pending.position_ms = Some(0); }
            }
        }
        self.show_order(host, row).await
    }

    async fn previous<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        if kind == Kind::Foreign {
            self.command(host, Method::POST, "previous", &[], None).await?;
            if let Some(remote) = self.remote.lock().as_mut() { remote.expect(None, None); }
            return Ok(());
        }
        let (current, back) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let back = match &remote.mode {
                Mode::Owned(window) if !window.stale => Some((remote.position() < 3000 && window.at > 0)
                    .then(|| window.rows[window.at - 1].map(|row| (row, window.uris[window.at - 1].clone()))).flatten()),
                _ => None,
            };
            (remote.state.current_index, back)
        };
        let current = current.ok_or("No track is selected")?;
        // Nothing earlier on the device (or nothing sent yet): restart this row.
        let Some(back) = back else { return self.start(host, current, 0).await; };
        self.command(host, Method::POST, "previous", &[], None).await?;
        let row = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let row = match back {
                Some((row, uri)) => {
                    // Spotify's Previous steps back once this item is under 3 s in.
                    if let Mode::Owned(window) = &mut remote.mode { window.at -= 1; }
                    remote.consumed.remove(&current);
                    remote.show_row(Some(row));
                    remote.expect(Some(uri), None);
                    row
                }
                None => {
                    let uri = remote.state.current_uri.clone();
                    remote.expect(Some(uri), None);
                    current
                }
            };
            remote.state.position_ms = 0;
            remote.state.playing = true;
            remote.anchored = Instant::now();
            row
        };
        self.show_order(host, row).await
    }

    /// Publishes the order from the device's row after Spotify moved there
    /// on its own; the window itself is checked by the next reconciliation.
    async fn show_order<H: Host>(&self, host: &H, row: usize) -> Result<(), String> {
        self.reanchor(host, row, self.snapshot().map_or(0, |state| state.position_ms)).await?;
        let order = self.order(host, Some(row), row).await?;
        if let Some(remote) = self.remote.lock().as_mut() { remote.set_order(order); }
        self.publish(host);
        Ok(())
    }

    /// The order ran out: stop, and let the next Play start the last row again.
    async fn finish<H: Host>(&self, host: &H) -> Result<(), String> {
        let playing = self.remote.lock().as_ref().is_some_and(|remote| remote.state.playing);
        if playing { self.command(host, Method::PUT, "pause", &[], None).await?; }
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.start_intent = None;
            remote.state.playing = false;
            remote.state.position_ms = 0;
            remote.anchored = Instant::now();
            remote.set_order(Vec::new());
            if let Mode::Owned(window) = &mut remote.mode { window.stale = true; }
            if playing { remote.expect(None, Some(false)); }
        }
        self.publish(host);
        Ok(())
    }

    async fn jump<H: Host>(&self, host: &H, index: usize) -> Result<(), String> {
        {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            validate_remote_track(remote.state.queue.get(index).ok_or("queue index is out of range")?)?;
        }
        self.start(host, index, 0).await
    }

    async fn play_queue<H: Host>(&self, host: &H, queue: Vec<Track>, index: usize, context: String, automatic: bool) -> Result<(), String> {
        if queue.is_empty() {
            let view = host.engine(Some(EngineOp::Restore { queue: &queue, index: 0, position_ms: 0, context: &context, automatic: false })).await?;
            {
                let mut guard = self.remote.lock();
                let remote = guard.as_mut().ok_or(RELEASED)?;
                let needs_pause = remote.kind() != Kind::Prepared || remote.pause_dirty || remote.start_intent.is_some();
                remote.adopt_engine(view);
                remote.show_row(None);
                remote.set_order(Vec::new());
                remote.state.position_ms = 0;
                remote.state.playing = false;
                remote.mode = Mode::Prepared;
                remote.pending = None;
                remote.start_intent = None;
                remote.pause_dirty = needs_pause;
                remote.polling = needs_pause;
            }
            self.publish(host);
            self.apply_pause(host).await?;
            return Ok(());
        }
        if index >= queue.len() { return Err("queue index is out of range".into()); }
        let index = if automatic {
            let excluded = host.exclusions(&context).await?;
            (index..queue.len()).chain(0..index)
                .find(|&row| remote_playable(&queue[row]) && !excluded.contains(&queue[row].id))
                .ok_or("No eligible tracks remain for automatic playback")?
        } else { index };
        validate_remote_track(&queue[index])?;
        let view = host.engine(Some(EngineOp::Restore { queue: &queue, index, position_ms: 0, context: &context, automatic })).await?;
        let current = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.adopt_engine(view);
            remote.consumed.clear();
            // Old URIs remain on the device until the write succeeds, but they
            // no longer name any occurrence in this replacement queue.
            if let Mode::Owned(window) = &mut remote.mode {
                window.rows.fill(None);
                window.stale = true;
            }
            remote.pending = None;
            remote.show_row(remote.anchor);
            remote.state.position_ms = 0;
            remote.state.playing = false;
            remote.anchored = Instant::now();
            remote.set_order(remote.plan.clone());
            remote.anchor
        };
        // Either way publishes once, with the state Spotify's answer settles.
        match current {
            Some(current) => self.start(host, current, 0).await,
            None => self.finish(host).await,
        }
    }

    /// Commit the device's occurrence to the canonical engine queue. The engine
    /// consumes upcoming draws/history without replacing any rows.
    async fn reanchor<H: Host>(&self, host: &H, index: usize, position_ms: u32) -> Result<(), String> {
        let view = host.engine(Some(EngineOp::Cursor { index, position_ms })).await?;
        let mut guard = self.remote.lock();
        let remote = guard.as_mut().ok_or(RELEASED)?;
        remote.adopt_engine(view);
        remote.consumed.clear();
        Ok(())
    }

    /// The engine's order from the device's row (or, after that row was
    /// removed, from where it was). Re-anchors when the engine's own order
    /// cannot answer.
    async fn order<H: Host>(&self, host: &H, current: Option<usize>, cut: usize) -> Result<Vec<usize>, String> {
        let position_ms = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            match remote.order_from(current, cut) {
                Some(order) if !(order.is_empty() && remote.lap_spent(current)) => return Ok(order),
                _ => if current.is_some() { remote.position() } else { 0 },
            }
        };
        self.reanchor(host, current.unwrap_or(cut), position_ms).await?;
        let guard = self.remote.lock();
        Ok(guard.as_ref().ok_or(RELEASED)?.order_from(current, cut).unwrap_or_default())
    }

    /// Starts the engine's order on the device at `current`.
    async fn start<H: Host>(&self, host: &H, current: usize, position_ms: u32) -> Result<(), String> {
        self.start_from(host, current, position_ms, Instant::now()).await
    }

    /// Starts, or retries, a selection made at `since`. It publishes once,
    /// when Spotify has answered: playing, or the staged row with the intent
    /// kept for retry. A play button that showed paused while the request was
    /// out would flicker on every start.
    async fn start_from<H: Host>(&self, host: &H, current: usize, position_ms: u32, since: Instant) -> Result<(), String> {
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            validate_remote_track(remote.state.queue.get(current).ok_or("queue index is out of range")?)?;
            remote.start_intent = Some(StartIntent { current, position_ms, since });
            remote.pending = None;
            remote.show_row(Some(current));
            remote.state.position_ms = position_ms;
            remote.state.playing = false;
            remote.anchored = Instant::now();
            remote.polling = true;
            if let Mode::Owned(window) = &mut remote.mode { window.stale = true; }
        }
        let result = self.send_start(host, current, position_ms).await;
        self.publish(host);
        result
    }

    /// Spotify's own shuffle and repeat go out of the way first unless the
    /// router already owns the device.
    async fn send_start<H: Host>(&self, host: &H, current: usize, position_ms: u32) -> Result<(), String> {
        if self.remote.lock().as_ref().is_some_and(|remote| remote.anchor != Some(current)) {
            self.reanchor(host, current, position_ms).await?;
        }
        let order = self.order(host, Some(current), current).await?;
        let (configure, tail) = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.consumed.insert(current);
            let configure = (remote.kind() != Kind::Owned).then(|| (
                remote.device_path("shuffle", &[("state", "false".into())]),
                remote.device_path("repeat", &[("state", remote.spotify_repeat().into())]),
            ));
            let tail = remote.window_tail(&order);
            remote.set_order(order);
            (configure, tail)
        };
        if let Some((shuffle, repeat)) = configure {
            host.spotify(Method::PUT, shuffle?, None).await?;
            host.spotify(Method::PUT, repeat?, None).await?;
        }
        self.send_window(host, current, tail, position_ms).await
    }

    /// Hands Spotify the device's row followed by `tail`, resuming at
    /// `position_ms`: a brief rebuffer, never a restart from 0.
    async fn send_window<H: Host>(&self, host: &H, current: usize, tail: Vec<usize>, position_ms: u32) -> Result<(), String> {
        let (path, uris) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            validate_remote_track(remote.state.queue.get(current).ok_or("queue index is out of range")?)?;
            let uris: Vec<String> = std::iter::once(current).chain(tail.iter().copied())
                .map(|row| remote.state.queue[row].uri.clone()).collect();
            (remote.device_path("play", &[])?, uris)
        };
        host.spotify(Method::PUT, path, Some(json!({ "uris": uris, "offset": { "position": 0 }, "position_ms": position_ms }))).await?;
        let mut guard = self.remote.lock();
        let remote = guard.as_mut().ok_or(RELEASED)?;
        let uri = uris[0].clone();
        remote.mode = Mode::Owned(Window {
            rows: std::iter::once(Some(current)).chain(tail.into_iter().map(Some)).collect(),
            uris,
            at: 0,
            stale: false,
            expired: false,
        });
        remote.show_row(Some(current));
        remote.state.playing = true;
        remote.state.position_ms = position_ms;
        remote.state.error.clear();
        remote.failures = 0;
        remote.pause_dirty = false;
        remote.anchored = Instant::now();
        remote.expect(Some(uri), Some(true));
        remote.start_intent = None;
        Ok(())
    }

    /// Applies a queue edit to the engine and carries the device through it:
    /// Spotify hears a new window only when what it holds no longer matches.
    async fn edit<H: Host>(&self, host: &H, kind: Kind, op: EngineOp<'_>) -> Result<(), String> {
        let remap = match op { EngineOp::Remove(row) => Remap::Remove(row), EngineOp::Move(from, to) => Remap::Move(from, to), _ => Remap::Keep };
        // Toggling shuffle replaces the bag of draws; repeat keeps it.
        let new_draw = matches!(op, EngineOp::Shuffle(_));
        let shuffling_on = matches!(op, EngineOp::Shuffle(true));
        let (before, repeat_before) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            (remote.state.current_index, remote.spotify_repeat())
        };
        let view = host.engine(Some(op)).await?;
        let (current, removed, anchor, position_ms, repeat_after) = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.adopt_engine(view);
            if let Some(mut intent) = remote.start_intent.take() {
                if let Some(current) = remap.apply(intent.current) {
                    intent.current = current;
                    remote.start_intent = Some(intent);
                } else if let Some(current) = remote.anchor {
                    intent.current = current;
                    intent.position_ms = 0;
                    remote.start_intent = Some(intent);
                }
            }
            if kind == Kind::Prepared {
                // Nothing plays there yet: the view is the engine's own. Its
                // replacement for a removed current row starts from 0.
                if matches!(remap, Remap::Remove(row) if before == Some(row)) { remote.state.position_ms = 0; }
                remote.show_row(remote.anchor);
                remote.set_order(remote.plan.clone());
                drop(guard);
                self.publish(host);
                return Ok(());
            }
            let current = before.and_then(|row| remap.apply(row));
            remote.consumed = if new_draw { HashSet::new() } else { remote.consumed.iter().filter_map(|row| remap.apply(*row)).collect() };
            if let Mode::Owned(window) = &mut remote.mode {
                for row in &mut window.rows { *row = row.and_then(|row| remap.apply(row)); }
            }
            remote.show_row(current);
            let removed = match remap { Remap::Remove(row) if current.is_none() && before.is_some() => Some(row), _ => None };
            remote.repeat_dirty |= remote.spotify_repeat() != repeat_before;
            (current, removed, remote.anchor, remote.position(), remote.spotify_repeat())
        };
        self.publish(host);
        if shuffling_on && current.is_some() && anchor != current {
            // A new shuffle draw has to leave out the row the device plays.
            self.reanchor(host, current.unwrap_or_default(), position_ms).await?;
        }
        if repeat_after != repeat_before { self.apply_repeat(host).await?; }
        if let Some(cut) = removed {
            // The device's row is gone: continue where the engine order resumes.
            let order = self.order(host, None, cut).await?;
            let playing = self.remote.lock().as_ref().is_some_and(|remote| remote.state.playing);
            return match order.first().copied() {
                Some(next) if playing => self.start(host, next, 0).await,
                Some(next) => {
                    if let Some(remote) = self.remote.lock().as_mut() {
                        remote.consumed.insert(next);
                        remote.show_row(Some(next));
                        remote.state.position_ms = 0;
                        remote.set_order(order[1..].to_vec());
                        if let Mode::Owned(window) = &mut remote.mode { window.stale = true; }
                    }
                    self.publish(host);
                    Ok(())
                }
                None => self.finish(host).await,
            };
        }
        let Some(current) = current else {
            self.publish(host);
            return Ok(());
        };
        let order = self.order(host, Some(current), current).await?;
        self.settle(host, current, order).await
    }

    async fn eligibility_changed<H: Host>(&self, host: &H) {
        let _operation = self.operation.lock().await;
        let kind = self.remote.lock().as_ref().map(Remote::kind);
        if !matches!(kind, Some(Kind::Prepared | Kind::Owned)) { return; }
        let result: Result<(), String> = async {
            let view = host.engine(None).await?;
            let current = {
                let mut guard = self.remote.lock();
                let remote = guard.as_mut().ok_or(RELEASED)?;
                remote.adopt_engine(view);
                remote.state.current_index
            };
            if let Some(current) = current {
                let order = self.order(host, Some(current), current).await?;
                self.settle(host, current, order).await?;
            }
            Ok(())
        }.await;
        if let Err(error) = &result {
            // The engine kept the change: a stale window sends it on the next
            // reconciliation, or on Play.
            if let Some(Mode::Owned(window)) = self.remote.lock().as_mut().map(|remote| &mut remote.mode) { window.stale = true; }
            self.report_error(host, error);
        }
        self.changed.notify_one();
    }

    /// Publish canonical edits even if the Spotify write fails. A stale window
    /// keeps ownership and retries on reconciliation; paused edits wait for Play.
    async fn settle<H: Host>(&self, host: &H, current: usize, order: Vec<usize>) -> Result<(), String> {
        let send = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let tail = remote.window_tail(&order);
            let playing = remote.state.playing;
            let position_ms = remote.position();
            remote.set_order(order);
            match &mut remote.mode {
                Mode::Owned(window) if window_differs(window, current, &tail) => {
                    window.stale = true;
                    if playing { Some((tail, position_ms)) } else { None }
                }
                _ => None,
            }
        };
        self.publish_changed(host);
        if let Some((tail, position_ms)) = send {
            self.send_window(host, current, tail, position_ms).await?;
            self.publish_changed(host);
        }
        Ok(())
    }

    async fn foreign_setting<H: Host>(&self, host: &H, endpoint: &str, value: String) -> Result<(), String> {
        self.command(host, Method::PUT, endpoint, &[("state", value.clone())], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            if endpoint == "shuffle" { remote.state.shuffle = value == "true"; } else { remote.state.repeat = value; }
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn play_foreign_row<H: Host>(&self, host: &H, index: usize) -> Result<(), String> {
        let (path, uris) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let rows = &remote.state.queue;
            validate_remote_track(rows.get(index).ok_or("queue index is out of range")?)?;
            let uris: Vec<&str> = rows[index..].iter().filter(|track| remote_playable(track)).take(WINDOW).map(|track| track.uri.as_str()).collect();
            (remote.device_path("play", &[])?, json!({ "uris": uris, "offset": { "position": 0 } }))
        };
        host.spotify(Method::PUT, path, Some(uris)).await?;
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.show_row(Some(index));
            remote.set_order((index + 1..remote.state.queue.len()).collect());
            remote.state.playing = true;
            remote.state.position_ms = 0;
            remote.anchored = Instant::now();
            let uri = remote.state.current_uri.clone();
            remote.expect(Some(uri), Some(true));
        }
        self.publish(host);
        Ok(())
    }

    /// Spotify plays its own queue before the rest of whatever it plays.
    async fn queue_on_spotify<H: Host>(&self, host: &H, tracks: &[Track]) -> Result<(), String> {
        for track in tracks { validate_remote_track(track)?; }
        for track in tracks {
            self.command(host, Method::POST, "queue", &[("uri", track.uri.clone())], None).await?;
        }
        let queue = self.spotify_queue(host).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            let current = remote.state.current_index.and_then(|row| remote.state.queue.get(row)).cloned();
            let offset = usize::from(current.is_some());
            remote.state.queue = current.into_iter().chain(queue.iter().filter_map(|item| track_from_player(item).ok())).collect();
            remote.state.queue_revision = next_revision(remote.state.queue_revision);
            remote.show_row((offset == 1).then_some(0));
            remote.set_order((offset..remote.state.queue.len()).collect());
        }
        self.publish(host);
        Ok(())
    }

    async fn spotify_queue<H: Host>(&self, host: &H) -> Result<Vec<Value>, String> {
        match host.spotify(Method::GET, "me/player/queue".into(), None).await?.as_mut()
            .and_then(Value::as_object_mut).and_then(|answer| answer.remove("queue")) {
            Some(Value::Array(queue)) => Ok(queue),
            _ => Err("Spotify returned no playback queue".into()),
        }
    }

    async fn apply_repeat<H: Host>(&self, host: &H) -> Result<(), String> {
        let repeat = self.remote.lock().as_ref().filter(|remote| remote.repeat_dirty).map(Remote::spotify_repeat);
        if let Some(repeat) = repeat {
            self.command(host, Method::PUT, "repeat", &[("state", repeat.into())], None).await?;
            if let Some(remote) = self.remote.lock().as_mut() {
                remote.repeat_dirty = false;
                remote.expect(None, None);
            }
        }
        Ok(())
    }

    async fn apply_pause<H: Host>(&self, host: &H) -> Result<(), String> {
        if !self.remote.lock().as_ref().is_some_and(|remote| remote.pause_dirty) { return Ok(()); }
        self.command(host, Method::PUT, "pause", &[], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.pause_dirty = false;
            remote.polling = false;
            remote.failures = 0;
            remote.state.error.clear();
        }
        self.publish_transport(host);
        Ok(())
    }

    /// One reconciliation: reads the device and acts on what it reports.
    async fn sync<H: Host>(&self, host: &H) -> Result<(), String> {
        let Some(device) = self.remote.lock().as_ref().map(|remote| remote.state.output_device_id.clone()) else { return Ok(()); };
        self.apply_pause(host).await?;
        self.apply_repeat(host).await?;
        let intent = self.remote.lock().as_ref().and_then(|remote| remote.start_intent.as_ref()
            .map(|intent| (intent.current, intent.position_ms, intent.since)));
        if let Some((current, position_ms, since)) = intent {
            if since.elapsed() < START_DEADLINE { return self.start_from(host, current, position_ms, since).await; }
            self.expire_start(host);
        }
        {
            let mut guard = self.remote.lock();
            let Some(remote) = guard.as_mut() else { return Ok(()); };
            if remote.kind() == Kind::Prepared {
                // Nothing plays there for this queue until Play: nothing to watch.
                remote.polling = false;
                remote.failures = 0;
                remote.refresh = false;
                return Ok(());
            }
        }
        let player = host.spotify(Method::GET, "me/player?additional_types=track,episode".into(), None).await?
            .map(serde_json::from_value::<PlayerState>).transpose()
            .map_err(|_| "Spotify returned an invalid playback state".to_owned())?;
        if player.as_ref().is_some_and(|player| player.device.id.as_deref() != Some(device.as_str())) {
            return Err(MOVED_AWAY.into());
        }
        if player.as_ref().is_some_and(|player| player.device.is_restricted) {
            return Err("Spotify does not allow controlling this device".into());
        }
        let (kind, commanded, refresh) = {
            let mut guard = self.remote.lock();
            let Some(remote) = guard.as_mut() else { return Ok(()); };
            let repeat = (remote.kind() == Kind::Owned).then(|| remote.spotify_repeat());
            if let Some(pending) = remote.pending.as_mut() {
                // Spotify still catching up with the router's own command.
                if pending.since.elapsed() < GRACE && !pending.met_by(player.as_ref(), repeat) {
                    pending.retries += 1;
                    return Ok(());
                }
            }
            let commanded = remote.pending.take().is_some();
            (remote.kind(), commanded, std::mem::take(&mut remote.refresh))
        };
        let result = match kind {
            Kind::Owned => self.observe_owned(host, player, !commanded).await,
            Kind::Foreign => self.observe_foreign(host, player, refresh).await,
            Kind::Prepared => Ok(()),
        };
        if result.is_ok() {
            if let Some(remote) = self.remote.lock().as_mut() { remote.failures = 0; }
        }
        result
    }

    /// A start Spotify never accepted is dropped once it is old: the selection
    /// stays staged and waits for Play instead of starting long after it was
    /// made, and the device's obsolete entries are only shown from now on.
    fn expire_start<H: Host>(&self, host: &H) {
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.start_intent = None;
            if let Mode::Owned(window) = &mut remote.mode {
                window.stale = true;
                window.expired = true;
            } else {
                // The phone's own session was never replaced.
                remote.mode = Mode::Prepared;
            }
        }
        self.publish_changed(host);
    }

    async fn observe_owned<H: Host>(&self, host: &H, player: Option<PlayerState>, infer_transition: bool) -> Result<(), String> {
        enum Seen {
            /// Spotify plays this engine row of the window.
            Row(usize),
            /// Spotify left the order it was handed (a removed row, or
            /// autoplay past the last entry): the order goes on after this row.
            Resume(Option<usize>),
            Elsewhere,
            /// An old occurrence must not replace a newly staged selection:
            /// one the device is paused on, or any after a start expired.
            Staged,
            /// The phone resumed an obsolete removed row: play the staged row,
            /// not the row after it.
            Start(usize),
        }
        let Some(player) = player else {
            // The device dropped its session: show it stopped. Play starts the
            // window again; until then only a visible window keeps reading.
            if let Some(remote) = self.remote.lock().as_mut() {
                remote.rebase();
                remote.state.playing = false;
                remote.state.error.clear();
            }
            self.publish_changed(host);
            return Ok(());
        };
        let seen = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let past_end = remote.state.playing && remote.state.duration_ms > 0
                && remote.position().saturating_add(END_SLACK_MS) >= remote.state.duration_ms;
            let plan_kept = !player.shuffle_state && player.repeat_state == remote.spotify_repeat();
            // Compare actual reports, not an extrapolated playhead: near-end
            // progress and sparse/missing polls cannot identify another copy.
            let reset = infer_transition && remote.state.repeat != "track" && player.is_playing
                && player.progress_ms.is_some_and(|progress| remote.state.position_ms > progress.saturating_add(3000));
            let Mode::Owned(window) = &mut remote.mode else { return Ok(()); };
            let uri = item_uri(&player);
            // Spotify exposes a URI, not an occurrence ID. A progress reset
            // disambiguates consecutive copies; command confirmations must
            // not count an already-applied skip twice.
            let next_copy = reset && window.uris.get(window.at + 1).is_some_and(|next| next == uri)
                && window.uris.get(window.at).is_some_and(|current| current == uri);
            let found = if next_copy { Some(window.at + 1) } else {
                window.uris[window.at..].iter().position(|held| held == uri).map(|j| window.at + j)
                    .or_else(|| window.uris[..window.at].iter().rposition(|held| held == uri))
            };
            match found {
                // Spotify's own shuffle or repeat reorders what it holds: that is
                // the phone taking over, not the window playing.
                Some(_) if !plan_kept => Seen::Elsewhere,
                Some(j) if (window.expired && window.rows[j].is_none()) || (!player.is_playing && window.stale
                    && (window.rows[j].is_none() || window.rows[j] != remote.state.current_index)) => {
                    remote.state.volume = player.device.volume_percent.unwrap_or(remote.state.volume);
                    remote.supports_volume = player.device.supports_volume;
                    Seen::Staged
                }
                Some(j) => {
                    if j > window.at {
                        remote.consumed.extend(window.rows[window.at + 1..=j].iter().flatten());
                    } else {
                        for row in window.rows[j + 1..=window.at].iter().flatten() { remote.consumed.remove(row); }
                    }
                    window.at = j;
                    let row = window.rows[j];
                    let before = window.rows[..j].iter().rev().flatten().next().copied();
                    remote.adopt_transport(&player);
                    match row {
                        Some(row) => {
                            remote.show_row(Some(row));
                            if let Some(duration) = player.item.as_ref().and_then(|item| item["duration_ms"].as_u64()).filter(|duration| *duration > 0) {
                                remote.state.duration_ms = duration.min(u64::from(u32::MAX)) as u32;
                            }
                            Seen::Row(row)
                        }
                        None if player.is_playing => match remote.state.current_index {
                            Some(current) => Seen::Start(current),
                            None => Seen::Resume(before),
                        },
                        None => Seen::Staged,
                    }
                }
                // Past the last entry Spotify's autoplay takes over: the order
                // either goes on (the window was not extended in time) or ends.
                None if past_end && window.at + 1 == window.uris.len() => Seen::Resume(window.rows[window.at]),
                None => Seen::Elsewhere,
            }
        };
        match seen {
            Seen::Staged => { self.publish_changed(host); Ok(()) }
            Seen::Start(current) => self.start(host, current, 0).await,
            Seen::Row(row) => {
                if self.remote.lock().as_ref().is_some_and(|remote| remote.engine_recovery || remote.anchor != Some(row)) {
                    self.reanchor(host, row, player.progress_ms.unwrap_or(0)).await?;
                }
                let order = self.order(host, Some(row), row).await?;
                self.settle(host, row, order).await
            }
            Seen::Resume(after) => {
                let order = match after {
                    Some(row) => self.order(host, Some(row), row).await?,
                    None => self.order(host, None, 0).await?,
                };
                match order.first() { Some(&next) => self.start(host, next, 0).await, None => self.finish(host).await }
            }
            Seen::Elsewhere => {
                // The phone started something else: show it, leave the engine alone.
                // Do not surrender ownership or lose rows on a failed read.
                let queue = self.spotify_queue(host).await?;
                if let Some(remote) = self.remote.lock().as_mut() {
                    remote.mode = Mode::Foreign;
                    remote.adopt_transport(&player);
                    remote.state.shuffle = player.shuffle_state;
                    remote.state.repeat.clone_from(&player.repeat_state);
                    remote.show_foreign(Some(&player), &queue);
                }
                self.publish(host);
                Ok(())
            }
        }
    }

    /// `refresh` when the window was just looked at again.
    async fn observe_foreign<H: Host>(&self, host: &H, player: Option<PlayerState>, refresh: bool) -> Result<(), String> {
        // Spotify's own queue is read only when the item changes or the window
        // is looked at again. A failed read keeps the rows shown rather than
        // silently deleting queued entries.
        let item = player.as_ref().filter(|player| player.item.is_some()).map(item_uri);
        let read = item.is_some_and(|uri| refresh || self.remote.lock().as_ref().is_some_and(|remote| remote.state.current_uri != uri));
        let queue = if read {
            match self.spotify_queue(host).await {
                Ok(queue) => Some(queue),
                Err(error) => {
                    if let Some(remote) = self.remote.lock().as_mut() { remote.refresh |= refresh; }
                    return Err(error);
                }
            }
        } else { None };
        if let Some(remote) = self.remote.lock().as_mut() {
            match &player {
                Some(player) => {
                    remote.adopt_transport(player);
                    remote.state.shuffle = player.shuffle_state;
                    remote.state.repeat.clone_from(&player.repeat_state);
                }
                None => {
                    remote.rebase();
                    remote.state.playing = false;
                    remote.state.error.clear();
                }
            }
            match (&queue, item) {
                (Some(queue), _) => remote.show_foreign(player.as_ref(), queue),
                (None, None) => remote.show_foreign(player.as_ref(), &[]),
                // The same item: the rows shown stand.
                (None, Some(_)) => {}
            }
        }
        self.publish_changed(host);
        Ok(())
    }

    async fn watch<H: Host>(&self, host: &H) {
        loop {
            let visible = self.visible.load(Ordering::Relaxed);
            let delay = self.remote.lock().as_ref()
                .filter(|remote| remote.polling && (!remote.engine_recovery || remote.recovery_ready))
                .and_then(|remote| reconcile_delay(remote, visible));
            let Some(delay) = delay else { self.changed.notified().await; continue; };
            tokio::select! {
                _ = self.changed.notified() => continue,
                _ = tokio::time::sleep(delay) => {},
            }
            let _operation = self.operation.lock().await;
            if self.remote.lock().as_ref().is_some_and(|remote|
                remote.polling && (!remote.engine_recovery || remote.recovery_ready)) {
                self.reconcile(host).await;
            }
        }
    }

    /// One timed check. A transient failure backs off and keeps control; it
    /// surfaces only once it persists.
    async fn reconcile<H: Host>(&self, host: &H) {
        let recovering = self.remote.lock().as_ref().is_some_and(|remote| remote.engine_recovery);
        let result = if recovering { self.recover_engine(host).await } else { self.sync(host).await };
        let Err(error) = result else { return; };
        if !personal_api::is_transient(&error) {
            self.suspend(host, &error);
            return;
        }
        let surface = self.remote.lock().as_mut().is_some_and(|remote| {
            remote.failures = remote.failures.saturating_add(1);
            if remote.failures >= 3 { remote.state.error = error; true } else { false }
        });
        if surface { self.publish_transport(host); }
    }
}

/// When the watcher next reads the device; `None` waits for a change.
/// Commands are confirmed quickly with bounded retries. A visible window
/// reads the device regularly, playing or paused, and at the projected track
/// end. Hidden, only an owned window that plays is read, at its track end: it
/// must be extended, continue its repeat lap or stop autoplay, and the media
/// flyout and keys must stay right. Anything else hidden does no work.
fn reconcile_delay(remote: &Remote, visible: bool) -> Option<Duration> {
    if remote.failures > 0 { return Some(Duration::from_secs(1 << remote.failures.min(6)).min(MAX_BACKOFF)); }
    if let Some(pending) = &remote.pending { return Some(Duration::from_millis(750 << pending.retries.min(3))); }
    if remote.start_intent.is_some() || remote.repeat_dirty || remote.pause_dirty { return Some(Duration::from_millis(750)); }
    if remote.refresh { return Some(Duration::ZERO); }
    let track_end = (remote.state.playing && remote.state.duration_ms > 0).then(|| {
        Duration::from_millis(u64::from(remote.state.duration_ms.saturating_sub(remote.position())) + 750)
    });
    if visible { return Some(track_end.map_or(VISIBLE_POLL, |end| end.min(VISIBLE_POLL))); }
    track_end.filter(|_| remote.kind() == Kind::Owned)
}

/// The engine's order from the device's row `current`, derived from the
/// engine's own order after `anchor`. After the device's row was removed,
/// `current` is `None` and `cut` is where that row was. `None` when the
/// engine's order does not reach back that far (sequential, without repeat,
/// behind the engine's own row): the engine must be re-anchored.
fn order_from(anchor: Option<usize>, plan: &[usize], shuffle: bool, repeat: &str, current: Option<usize>, cut: usize, consumed: &HashSet<usize>) -> Option<Vec<usize>> {
    if shuffle {
        // Shuffle's plan is a bag of draws: the device has spent some of them.
        return Some(anchor.into_iter().chain(plan.iter().copied())
            .filter(|row| Some(*row) != current && !consumed.contains(row)).collect());
    }
    let after = |row: usize| match current { Some(current) => row > current, None => row >= cut };
    if repeat == "context" {
        // One sequential lap holds every eligible row once: begin it after the device's row.
        let mut lap = plan.to_vec();
        lap.sort_unstable();
        lap.dedup();
        let split = lap.partition_point(|row| !after(*row));
        lap.rotate_left(split);
        return Some(lap);
    }
    if anchor.is_some_and(|anchor| cut < anchor) { return None; }
    Some(anchor.into_iter().chain(plan.iter().copied()).filter(|row| after(*row)).collect())
}

/// The rows after the device's row that Spotify can play, as many as fit;
/// `cycle` repeats the order (one lap) until they do.
fn window_tail(rows: &[Track], order: &[usize], cycle: bool) -> Vec<usize> {
    let playable = order.iter().copied().filter(|row| rows.get(*row).is_some_and(remote_playable));
    if cycle { playable.cycle().take(WINDOW - 1).collect() } else { playable.take(WINDOW - 1).collect() }
}

/// Whether Spotify's remaining entries fall short of `tail`: a different
/// order, or running low while the order goes on.
fn window_differs(window: &Window, current: usize, tail: &[usize]) -> bool {
    if window.stale || window.rows.get(window.at) != Some(&Some(current)) { return true; }
    let held = &window.rows[window.at + 1..];
    let prefix = held.len() <= tail.len() && held.iter().zip(tail).all(|(held, row)| *held == Some(*row));
    !prefix || held.len() < LOW_WATER && tail.len() > held.len()
}

fn next_revision(revision: u64) -> u64 { revision % MAX_REVISION + 1 }

fn projected_position(state: &PlaybackState, elapsed: Duration) -> u32 {
    project(state.position_ms, state.playing, state.duration_ms, elapsed)
}

fn project(position_ms: u32, playing: bool, duration_ms: u32, elapsed: Duration) -> u32 {
    let position = if playing { position_ms.saturating_add(elapsed.as_millis().min(u32::MAX as u128) as u32) } else { position_ms };
    if duration_ms == 0 { position } else { position.min(duration_ms) }
}

fn item_uri(player: &PlayerState) -> &str {
    player.item.as_ref().and_then(|item| item["uri"].as_str()).unwrap_or_default()
}

fn remote_playable(track: &Track) -> bool { validate_remote_track(track).is_ok() }

fn validate_remote_track(track: &Track) -> Result<(), String> {
    if track.unavailable { return Err("This item is unavailable for playback".into()); }
    let id = track.uri.strip_prefix("spotify:track:").or_else(|| track.uri.strip_prefix("spotify:episode:"));
    if !id.is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric())) {
        return Err("Remote playback requires a Spotify track or episode URI".into());
    }
    Ok(())
}

fn track_from_player(item: &Value) -> Result<Track, String> {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let uri = item["uri"].as_str().ok_or("Spotify playback item has no URI")?;
    let episode = item["type"] == "episode";
    let album = if episode { &item["show"] } else { &item["album"] };
    let images = if episode { &item["images"] } else { &album["images"] };
    let artists = item["artists"].as_array();
    Ok(Track {
        id: text(&item["id"]), uri: uri.to_owned(), name: text(&item["name"]),
        artist_names: if episode { vec![text(&album["publisher"])] } else { artists.map(|a| a.iter().map(|v| text(&v["name"])).collect()).unwrap_or_default() },
        artist_ids: artists.map(|a| a.iter().map(|v| text(&v["id"])).collect()).unwrap_or_default(),
        artist_id: artists.and_then(|a| a.first()).map(|a| text(&a["id"])).unwrap_or_default(),
        album_id: text(&album["id"]), album_name: text(&album["name"]),
        cover_url: images.as_array().and_then(|a| a.first()).map(|image| text(&image["url"])).unwrap_or_default(),
        duration_ms: item["duration_ms"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        ..Track::default()
    })
}

async fn run_local(client: &EngineClient, action: Action) -> Result<(), String> {
    match action {
        Action::Play => client.play().await,
        Action::Pause => client.pause().await,
        Action::Next => client.next().await,
        Action::Previous => client.previous().await,
        Action::Seek(ms) => client.seek(ms).await,
        Action::Volume(percent) => client.set_volume(percent).await,
        Action::Shuffle(enabled) => client.set_shuffle(enabled).await,
        Action::Repeat(mode) => client.set_repeat(&mode).await,
        Action::Speed(speed) => client.set_playback_speed(speed).await,
        Action::Queue(queue, index, context, automatic) => client.play_queue(&queue, index, 0, &context, automatic).await,
        Action::Index(index) => client.play_queue_index(index).await,
        Action::Add(track, context) => client.add_queue(&track, &context).await,
        Action::AddBatch(tracks, context) => client.add_queue_batch(&tracks, &context).await,
        Action::Remove(index) => client.remove_queue(index).await,
        Action::Move(from, to) => client.move_queue(from, to).await,
        Action::Preview(track, cuts, loop_range, ms, lease) => client.preview_track_edit(&track, &cuts, loop_range, ms, lease).await,
        Action::RestorePreview(lease) => client.restore_preview(lease).await,
    }
}

struct AppHost<'a> {
    app: &'a AppHandle,
    client: &'a EngineClient,
    personal: &'a PersonalApi,
}

impl Host for AppHost<'_> {
    async fn spotify(&self, method: Method, path: String, body: Option<Value>) -> Result<Option<Value>, String> {
        self.personal.player(self.app, method, &path, body).await
    }

    async fn engine(&self, op: Option<EngineOp<'_>>) -> Result<EngineView, String> {
        // Subscribed before the command: the engine answers a command first
        // and publishes the state it produced right after.
        let mut lines = self.client.subscribe_lines();
        match op {
            None => {}
            Some(EngineOp::Restore { queue, index, position_ms, context, automatic }) => self.client.restore_queue(queue, index, position_ms, context, automatic, false).await?,
            Some(EngineOp::Cursor { index, position_ms }) => self.client.set_queue_cursor(index, position_ms).await?,
            Some(EngineOp::Add(track, context)) => self.client.add_queue(track, context).await?,
            Some(EngineOp::AddBatch(tracks, context)) => self.client.add_queue_batch(tracks, context).await?,
            Some(EngineOp::Remove(index)) => self.client.remove_queue(index).await?,
            Some(EngineOp::Move(from, to)) => self.client.move_queue(from, to).await?,
            Some(EngineOp::Shuffle(enabled)) => self.client.set_shuffle(enabled).await?,
            Some(EngineOp::Repeat(mode)) => self.client.set_repeat(mode).await?,
        }
        // The engine handles requests in order, so once this answer arrives
        // every state line the command produced has already been fanned out.
        let metadata = self.client.queue_metadata(None).await?;
        let mut state = None;
        loop {
            match lines.try_recv() {
                Ok(StateLine::State(line)) => state = Some(line),
                Ok(StateLine::Disconnected) => return Err("The playback engine disconnected during the queue operation".into()),
                Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
        if state.is_none() { state = self.client.current_state(); }
        let upcoming = metadata.upcoming;
        Ok(EngineView { state, upcoming })
    }

    async fn exclusions(&self, context: &str) -> Result<Vec<String>, String> {
        Ok(self.client.queue_metadata(Some(context)).await?.excluded_track_ids)
    }

    async fn pause_engine(&self) -> Result<(), String> { self.client.pause().await }

    async fn local(&self, action: Action) -> Result<(), String> { run_local(self.client, action).await }

    fn local_state(&self) -> PlaybackState {
        let managed = self.app.state::<Mutex<AppState>>();
        let guard = managed.lock();
        if guard.playback.preview { return guard.playback.clone(); }
        self.client.current_state().unwrap_or_else(|| guard.playback.clone())
    }

    fn publish_local(&self) {
        let managed = self.app.state::<Mutex<AppState>>();
        let guard = managed.lock();
        let _ = self.app.emit("state", PlaybackEvent::new(&guard.playback, true, true));
        media_keys::update_state(&guard.playback);
    }

    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) { let _ = self.app.emit(event, payload); }

    fn media(&self, state: &PlaybackState) { media_keys::update_state(state); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn track(id: &str) -> Track { Track { id: id.into(), uri: format!("spotify:track:{id}"), name: id.into(), duration_ms: 100_000, ..Track::default() } }

    #[test]
    fn remote_snapshots_cannot_reintroduce_invalidated_local_download_marks() {
        let core = Core::default();
        let row = Track { cached: true, ..track("a") };
        *core.remote.lock() = Some(Remote::new(PlaybackState { queue: vec![row].into(),
            cached_ids: Some(vec!["a".into()]), ..Default::default() }, true));
        core.apply_cache_marks(false, &["a".into()], &[]);
        assert!(!core.snapshot().unwrap().queue[0].cached);
        assert!(core.snapshot().unwrap().cached_ids.is_none());
        core.apply_cache_marks(false, &["a".into()], &["a".into()]);
        assert!(core.snapshot().unwrap().queue[0].cached);
        core.apply_cache_marks(true, &[], &[]);
        assert!(!core.snapshot().unwrap().queue[0].cached);
    }

    fn device() -> Value {
        json!({"id":"phone", "name":"Phone", "type":"Smartphone", "is_active":true, "is_private_session":false, "is_restricted":false, "volume_percent":35, "supports_volume":true})
    }

    fn reading(uri: &str, playing: bool, progress: u32) -> Value {
        json!({"device": device(), "is_playing": playing, "progress_ms": progress, "shuffle_state": false, "repeat_state": "off",
            "item": {"id": uri.rsplit(':').next(), "uri": uri, "name": uri, "type": "track", "duration_ms": 100_000}})
    }

    /// A local stand-in for the Spotify Web API: it answers player reads from
    /// what the test sets and records every request.
    #[derive(Default)]
    struct Spotify {
        player: Option<Value>,
        queue: Vec<Value>,
        devices: Option<Vec<Value>>,
        failures: std::collections::VecDeque<u16>,
        write_failures: std::collections::VecDeque<u16>,
        queue_failures: std::collections::VecDeque<u16>,
        log: Vec<(String, String, Option<Value>)>,
    }

    impl Spotify {
        /// Requests other than reads, as `METHOD path?query`.
        fn writes(&self) -> Vec<String> {
            self.log.iter().filter(|(method, ..)| method != "GET").map(|(method, path, _)| format!("{method} {path}")).collect()
        }
        fn last_play(&self) -> Option<Value> {
            self.log.iter().rev().find(|(method, path, _)| method == "PUT" && path.starts_with("me/player/play?")).and_then(|(.., body)| body.clone())
        }
        fn reads_of(&self, wanted: &str) -> usize {
            self.log.iter().filter(|(method, path, _)| method == "GET" && path.split('?').next() == Some(wanted)).count()
        }
    }

    async fn serve(spotify: Arc<Mutex<Spotify>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let spotify = spotify.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let end = loop {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).await.unwrap();
                        assert!(count > 0, "request ended before its headers");
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") { break end + 4; }
                    };
                    let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
                    let mut request = head.lines().next().unwrap().split(' ');
                    let method = request.next().unwrap().to_owned();
                    let path = request.next().unwrap().strip_prefix("/v1/").unwrap().to_owned();
                    let length = head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap()));
                    assert!(method == "GET" || length.is_some(), "{method} {path} was sent without a length");
                    let length = length.unwrap_or(0);
                    while bytes.len() - end < length {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).await.unwrap();
                        assert!(count > 0, "request ended before its body");
                        bytes.extend_from_slice(&chunk[..count]);
                    }
                    let body = (length > 0).then(|| serde_json::from_slice::<Value>(&bytes[end..end + length]).unwrap());
                    let (status, answer) = {
                        let mut spotify = spotify.lock();
                        spotify.log.push((method.clone(), path.clone(), body));
                        match (method.as_str(), path.split('?').next().unwrap()) {
                            ("GET", "me/player") => match spotify.failures.pop_front() {
                                Some(status) => (status, None),
                                None => spotify.player.clone().map_or((204, None), |player| (200, Some(player))),
                            },
                            ("GET", "me/player/devices") => (200, Some(json!({"devices": spotify.devices.clone().unwrap_or_else(|| vec![device()])}))),
                            ("GET", "me/player/queue") => match spotify.queue_failures.pop_front() {
                                Some(status) => (status, None),
                                None => (200, Some(json!({"queue": spotify.queue}))),
                            },
                            _ => (spotify.write_failures.pop_front().unwrap_or(204), None),
                        }
                    };
                    let body = answer.map(|answer| answer.to_string()).unwrap_or_default();
                    stream.write_all(format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                });
            }
        });
        base
    }

    /// The engine's queue semantics, sequential only, without audio.
    struct FakeEngine {
        rows: Vec<Track>,
        current: Option<usize>,
        repeat: String,
        revision: u64,
        playing: bool,
        position_ms: u32,
        volume: u8,
        ops: Vec<String>,
        excluded: HashSet<String>,
    }

    impl FakeEngine {
        fn upcoming(&self) -> Vec<usize> {
            let eligible = |row: &usize| !self.rows[*row].unavailable && !self.excluded.contains(&self.rows[*row].id);
            let Some(current) = self.current else { return (0..self.rows.len()).filter(eligible).collect(); };
            let mut upcoming: Vec<usize> = (current + 1..self.rows.len()).filter(eligible).collect();
            if self.repeat == "context" { upcoming.extend((0..=current).filter(eligible)); }
            upcoming
        }
        fn state(&self) -> PlaybackState {
            PlaybackState { queue: self.rows.as_slice().into(), current_index: self.current, queue_revision: self.revision, repeat: self.repeat.clone(),
                upcoming: self.upcoming(), playing: self.playing, position_ms: self.position_ms, auth_state: "ready".into(), ready: true,
                current_uri: self.current.map(|row| self.rows[row].uri.clone()).unwrap_or_default(), duration_ms: 100_000, volume: self.volume, ..PlaybackState::default() }
        }
        fn apply(&mut self, op: Option<EngineOp<'_>>) -> EngineView {
            let Some(op) = op else { return EngineView { state: None, upcoming: self.upcoming() }; };
            match op {
                EngineOp::Cursor { index, position_ms } => {
                    self.ops.push(format!("cursor {index} {position_ms}"));
                    self.current = Some(index);
                    self.position_ms = position_ms;
                    return EngineView { state: Some(self.state()), upcoming: self.upcoming() };
                }
                EngineOp::Restore { queue, index, position_ms, .. } => {
                    self.ops.push(format!("restore {index} {position_ms}"));
                    self.rows = queue.to_vec();
                    self.current = (!queue.is_empty()).then_some(index);
                    self.position_ms = position_ms;
                }
                EngineOp::Add(track, _) => { self.ops.push(format!("add {}", track.id)); self.rows.push(track.clone()); }
                EngineOp::AddBatch(tracks, _) => { self.ops.push("add batch".into()); self.rows.extend_from_slice(tracks); }
                EngineOp::Remove(row) => {
                    self.ops.push(format!("remove {row}"));
                    self.rows.remove(row);
                    self.current = self.current.and_then(|current| match Remap::Remove(row).apply(current) {
                        Some(current) => Some(current),
                        None => (!self.rows.is_empty()).then(|| row.min(self.rows.len() - 1)),
                    });
                }
                EngineOp::Move(from, to) => {
                    self.ops.push(format!("move {from} {to}"));
                    let track = self.rows.remove(from);
                    self.rows.insert(to, track);
                    self.current = self.current.map(|current| Remap::Move(from, to).apply(current).unwrap());
                }
                EngineOp::Shuffle(enabled) => self.ops.push(format!("shuffle {enabled}")),
                EngineOp::Repeat(mode) => { self.ops.push(format!("repeat {mode}")); self.repeat = mode.into(); }
            }
            self.revision += 1;
            EngineView { state: Some(self.state()), upcoming: self.upcoming() }
        }
    }

    struct TestHost {
        base: String,
        http: reqwest::Client,
        spotify: Arc<Mutex<Spotify>>,
        engine: Mutex<FakeEngine>,
        local_shown: Mutex<u32>,
        events: Mutex<Vec<Value>>,
    }

    impl Host for TestHost {
        async fn spotify(&self, method: Method, path: String, body: Option<Value>) -> Result<Option<Value>, String> {
            let url = url::Url::parse(&format!("{}/v1/{path}", self.base)).unwrap();
            let response = personal_api::api_request(&self.http, "fixture", method, url, body).send().await
                .map_err(|_| personal_api::UNREACHABLE.to_owned())?;
            if !response.status().is_success() { return Err(personal_api::spotify_error(response.status())); }
            personal_api::read_player_response(response).await
        }
        async fn engine(&self, op: Option<EngineOp<'_>>) -> Result<EngineView, String> { Ok(self.engine.lock().apply(op)) }
        async fn exclusions(&self, _context: &str) -> Result<Vec<String>, String> { Ok(self.engine.lock().excluded.iter().cloned().collect()) }
        async fn pause_engine(&self) -> Result<(), String> { self.engine.lock().playing = false; Ok(()) }
        async fn local(&self, _action: Action) -> Result<(), String> { Ok(()) }
        fn local_state(&self) -> PlaybackState { self.engine.lock().state() }
        fn publish_local(&self) { *self.local_shown.lock() += 1; }
        fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) {
            if event == "state" { self.events.lock().push(serde_json::to_value(payload).unwrap()); }
        }
        fn media(&self, _state: &PlaybackState) {}
    }

    async fn fixture(rows: usize, playing: bool) -> (Core, TestHost) {
        let spotify = Arc::new(Mutex::new(Spotify::default()));
        let base = serve(spotify.clone()).await;
        let engine = FakeEngine { rows: (0..rows).map(|row| track(&format!("t{row}"))).collect(), current: Some(0), repeat: "off".into(),
            revision: 1, playing, position_ms: 0, volume: 50, ops: Vec::new(), excluded: HashSet::new() };
        let host = TestHost { base, http: reqwest::Client::builder().no_proxy().build().unwrap(), spotify, engine: Mutex::new(engine),
            local_shown: Mutex::new(0), events: Mutex::new(Vec::new()) };
        (Core::default(), host)
    }

    #[tokio::test]
    async fn selecting_paused_outputs_seeds_actual_device_volume_without_polling_or_commands() {
        for playing in [false, true] {
            for (volume, supported) in [(Some(35), true), (None, true), (None, false), (Some(0), true), (Some(35), false)] {
                let (core, host) = fixture(2, playing).await;
                host.engine.lock().volume = 80;
                let mut target = device();
                target["volume_percent"] = json!(volume);
                target["supports_volume"] = json!(supported);
                host.spotify.lock().devices = Some(vec![target.clone()]);
                core.select(&host, Some("phone".into())).await.unwrap();
                assert_eq!(core.snapshot().unwrap().volume, volume.unwrap_or(80));
                assert_eq!(remote(&core, |remote| remote.supports_volume), supported);
                if playing { core.run(&host, Action::Pause).await.unwrap(); }
                // Different output, including a Prepared queue that will never
                // poll player state until explicit Play.
                remote(&core, |remote| remote.state.volume = 80);
                target["id"] = json!("other");
                target["name"] = json!("Other");
                host.spotify.lock().devices = Some(vec![target]);
                host.spotify.lock().log.clear();
                core.select(&host, Some("other".into())).await.unwrap();
                let state = core.snapshot().unwrap();
                assert_eq!(state.output_device_id, "other");
                assert_eq!(state.volume, volume.unwrap_or(80));
                assert!(!state.playing);
                assert_eq!(remote(&core, |remote| remote.supports_volume), supported);
                assert_eq!(host.spotify.lock().writes(), ["PUT me/player"]);
                assert_eq!(host.spotify.lock().reads_of("me/player"), 0);
                if !supported {
                    assert!(core.run(&host, Action::Volume(70)).await.is_err());
                    assert_eq!(core.snapshot().unwrap().volume, volume.unwrap_or(80));
                    assert_eq!(host.spotify.lock().writes(), ["PUT me/player"]);
                }
            }
        }
    }

    async fn duplicate_window() -> (Core, TestHost) {
        let (core, host) = fixture(4, true).await;
        host.engine.lock().rows = vec![track("a"), track("a"), track("a"), track("b")];
        core.select(&host, Some("phone".into())).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:a", true, 1000));
        core.sync(&host).await.unwrap();
        host.spotify.lock().log.clear();
        (core, host)
    }

    #[tokio::test]
    async fn duplicate_occurrences_only_advance_on_observed_reset_not_near_end_or_repeated_reset() {
        let (core, host) = duplicate_window().await;
        for (uri, progress, expected) in [
            ("a", 99_000, 0), ("a", 99_000, 0), ("a", 0, 1), ("a", 0, 1),
            ("a", 99_000, 1), ("a", 99_000, 1), ("a", 500, 2), ("a", 500, 2),
            ("b", 500, 3),
        ] {
            host.spotify.lock().player = Some(reading(&format!("spotify:track:{uri}"), true, progress));
            core.sync(&host).await.unwrap();
            assert_eq!(core.snapshot().unwrap().current_index, Some(expected));
            assert_eq!(host.engine.lock().current, Some(expected));
        }
        assert!(host.spotify.lock().writes().is_empty(), "observed transitions must not restart the window");
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 0);
    }

    #[tokio::test]
    async fn sparse_or_missing_progress_and_command_confirmation_cannot_skip_another_duplicate() {
        let (core, host) = duplicate_window().await;
        remote(&core, |remote| remote.anchored -= Duration::from_secs(120));
        host.spotify.lock().player = Some(reading("spotify:track:a", true, 2000));
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(0), "projected end is not an observed reset");
        host.spotify.lock().player.as_mut().unwrap()["progress_ms"] = Value::Null;
        host.spotify.lock().player.as_mut().unwrap()["item"]["duration_ms"] = Value::Null;
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(0), "missing progress is not zero");
        host.spotify.lock().player = Some(reading("spotify:track:a", true, 99_000));
        core.sync(&host).await.unwrap();
        core.run(&host, Action::Next).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(1));
        // The old near-end reading is still pending; the reset confirms the
        // command's already-applied occurrence exactly once.
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(1));
        host.spotify.lock().player = Some(reading("spotify:track:a", true, 0));
        core.sync(&host).await.unwrap();
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(1));
        assert_eq!(host.spotify.lock().writes(), ["POST me/player/next?device_id=phone"]);
    }

    #[tokio::test]
    async fn removing_a_paused_current_row_stays_staged_until_explicit_or_device_resume() {
        for device_resume in [false, true] {
            let (core, host) = owned(3).await;
            core.run(&host, Action::Pause).await.unwrap();
            host.spotify.lock().player = Some(reading("spotify:track:t0", false, 1000));
            core.sync(&host).await.unwrap();
            host.spotify.lock().log.clear();
            core.run(&host, Action::Remove(0)).await.unwrap();
            for _ in 0..3 { core.sync(&host).await.unwrap(); }
            let state = core.snapshot().unwrap();
            assert!(!state.playing);
            assert_eq!(state.current_uri, "spotify:track:t1");
            assert_eq!(state.current_index, Some(0));
            assert_eq!(state.position_ms, 0);
            assert!(host.spotify.lock().writes().is_empty());
            if device_resume {
                host.spotify.lock().player = Some(reading("spotify:track:t0", true, 1000));
                core.sync(&host).await.unwrap();
            } else {
                core.run(&host, Action::Play).await.unwrap();
            }
            assert_eq!(uris(&host.spotify.lock().last_play().unwrap()), ["spotify:track:t1", "spotify:track:t2"]);
            assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:t1");
        }
    }

    #[tokio::test]
    async fn a_failed_replacement_retains_intent_without_relabeling_the_old_window() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.extend([502, 502]);
        assert!(core.run(&host, Action::Queue(vec![track("new0"), track("new1")], 0, String::new(), false)).await.is_err());
        core.reconcile(&host).await;
        let state = core.snapshot().unwrap();
        assert_eq!(state.current_uri, "spotify:track:new0");
        assert!(!state.playing);
        assert!(!state.error.is_empty());
        remote(&core, |remote| {
            assert!(remote.start_intent.is_some());
            assert!(remote.polling);
            let Mode::Owned(window) = &remote.mode else { panic!("lost applied window"); };
            assert_eq!(window.uris, ["spotify:track:t0", "spotify:track:t1"]);
            assert_eq!(window.rows, [None, None]);
        });
        assert_eq!(host.spotify.lock().reads_of("me/player"), 0);
        core.reconcile(&host).await;
        assert_eq!(uris(&host.spotify.lock().last_play().unwrap()), ["spotify:track:new0", "spotify:track:new1"]);
        host.spotify.lock().player = Some(reading("spotify:track:new0", true, 200));
        core.sync(&host).await.unwrap();
        assert!(core.snapshot().unwrap().playing);
        assert!(core.snapshot().unwrap().error.is_empty());
        assert!(!remote(&core, |remote| remote.start_intent.is_some()));
        assert_eq!(remote(&core, |remote| remote.failures), 0);
    }

    #[tokio::test]
    async fn clearing_a_failed_replacement_stops_the_old_session_and_cancels_retry() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Queue(vec![track("new")], 0, String::new(), false)).await.is_err());
        assert!(!core.snapshot().unwrap().playing);
        host.spotify.lock().log.clear();
        core.run(&host, Action::Queue(Vec::new(), 0, String::new(), false)).await.unwrap();
        core.reconcile(&host).await;
        assert_eq!(host.spotify.lock().writes(), ["PUT me/player/pause?device_id=phone"]);
        assert!(core.snapshot().unwrap().queue.is_empty());
        assert!(!remote(&core, |remote| remote.polling || remote.start_intent.is_some()));
        assert!(host.engine.lock().rows.is_empty());
        assert!(!host.engine.lock().playing);
    }

    #[tokio::test]
    async fn a_failed_start_follows_occurrences_through_edits_and_seek_but_pause_cancels_it() {
        let (core, host) = owned(3).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Index(1)).await.is_err());
        core.run(&host, Action::Move(1, 2)).await.unwrap();
        core.run(&host, Action::Seek(4321)).await.unwrap();
        core.reconcile(&host).await;
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t1"]);
        assert_eq!(play["position_ms"], 4321);

        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Index(0)).await.is_err());
        core.run(&host, Action::Pause).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t1", false, 4321));
        host.spotify.lock().log.clear();
        core.sync(&host).await.unwrap();
        assert!(!core.snapshot().unwrap().playing);
        assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:t0");
        assert!(host.spotify.lock().writes().is_empty());
        assert!(!remote(&core, |remote| remote.start_intent.is_some()));
    }

    #[tokio::test]
    async fn eligibility_changes_replace_playing_order_but_stage_paused_order() {
        for paused in [false, true] {
            let (core, host) = owned(3).await;
            if paused {
                core.run(&host, Action::Pause).await.unwrap();
                host.spotify.lock().player = Some(reading("spotify:track:t0", false, 4321));
                core.sync(&host).await.unwrap();
            } else {
                remote(&core, |remote| remote.state.position_ms = 4321);
            }
            host.spotify.lock().log.clear();
            host.engine.lock().excluded.insert("t1".into());
            core.eligibility_changed(&host).await;
            let state = core.snapshot().unwrap();
            assert_eq!(state.current_uri, "spotify:track:t0");
            assert_eq!(state.playing, !paused);
            assert_eq!(state.upcoming, [2]);
            if paused {
                assert_eq!(state.position_ms, 4321);
                assert!(host.spotify.lock().writes().is_empty());
                core.run(&host, Action::Play).await.unwrap();
            }
            let play = host.spotify.lock().last_play().unwrap();
            assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t2"]);
            assert!((4321..5321).contains(&play["position_ms"].as_u64().unwrap()));
            host.spotify.lock().log.clear();
            core.run(&host, Action::Next).await.unwrap();
            assert_eq!(host.spotify.lock().writes(), ["POST me/player/next?device_id=phone"]);
            assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:t2");
        }
    }

    #[tokio::test]
    async fn volume_and_native_previous_do_not_replace_the_window_and_local_return_is_paused() {
        let (core, host) = owned(3).await;
        core.run(&host, Action::Volume(73)).await.unwrap();
        assert_eq!(core.snapshot().unwrap().volume, 73);
        assert_eq!(host.spotify.lock().writes(), ["PUT me/player/volume?device_id=phone&volume_percent=73"]);
        core.run(&host, Action::Next).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 100));
        core.sync(&host).await.unwrap();
        host.spotify.lock().log.clear();
        core.run(&host, Action::Previous).await.unwrap();
        assert_eq!(host.spotify.lock().writes(), ["POST me/player/previous?device_id=phone"]);
        assert_eq!(core.snapshot().unwrap().current_index, Some(0));
        core.select(&host, None).await.unwrap();
        assert!(!host.engine.lock().playing);
        assert_eq!(host.engine.lock().current, Some(0));
        assert!(core.snapshot().is_none());
    }

    #[test]
    fn a_visible_window_reads_regularly_and_a_hidden_one_only_at_an_owned_track_end() {
        let window = Window { rows: vec![Some(0)], uris: vec!["spotify:track:t0".into()], at: 0, stale: false, expired: false };
        let mut remote = Remote::new(PlaybackState { playing: true, position_ms: 20000, duration_ms: 100000, ..PlaybackState::default() }, true);
        remote.mode = Mode::Owned(window);
        assert_eq!(reconcile_delay(&remote, true), Some(VISIBLE_POLL));
        let hidden = reconcile_delay(&remote, false).unwrap();
        assert!((Duration::from_millis(80_000)..=Duration::from_millis(80_750)).contains(&hidden), "only the track end: {hidden:?}");
        remote.state.position_ms = 99000;
        assert!(reconcile_delay(&remote, true).unwrap() <= Duration::from_millis(1750));
        assert!(reconcile_delay(&remote, false).unwrap() <= Duration::from_millis(1750));

        remote.state.playing = false;
        assert_eq!(reconcile_delay(&remote, true), Some(VISIBLE_POLL), "paused, but looked at");
        assert_eq!(reconcile_delay(&remote, false), None, "hidden and paused does no work");
        remote.mode = Mode::Foreign;
        remote.state.playing = true;
        assert_eq!(reconcile_delay(&remote, false), None, "the phone's own session is not watched while hidden");
        assert!(reconcile_delay(&remote, true).unwrap() <= VISIBLE_POLL);

        remote.refresh = true;
        assert_eq!(reconcile_delay(&remote, false), Some(Duration::ZERO));
        remote.expect(None, Some(true));
        assert_eq!(reconcile_delay(&remote, false), Some(Duration::from_millis(750)), "a command is confirmed even while hidden");
        remote.pending.as_mut().unwrap().retries = 100;
        assert_eq!(reconcile_delay(&remote, false), Some(Duration::from_secs(6)));
    }

    #[tokio::test]
    async fn failed_window_write_retains_additions_and_reconciles_without_resetting_position() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Add(track("added"), String::new())).await.is_err());
        assert_eq!(host.engine.lock().rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "added"]);
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Owned);
            assert!(matches!(&remote.mode, Mode::Owned(window) if window.stale));
            assert_eq!(remote.state.current_index, Some(0));
        });
        core.reconcile(&host).await;
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t1", "spotify:track:added"]);
        assert!(play["position_ms"].as_u64().unwrap() >= 1000);
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
    }

    #[tokio::test]
    async fn failed_foreign_queue_read_does_not_surrender_the_owned_order() {
        let (core, host) = owned(3).await;
        host.spotify.lock().player = Some(reading("spotify:track:phone", true, 100));
        host.spotify.lock().queue_failures.push_back(502);
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
        assert_eq!(core.snapshot().unwrap().queue.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "t2"]);
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Foreign);
        assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:phone");
        assert_eq!(host.engine.lock().current, Some(0));
    }

    #[tokio::test]
    async fn consecutive_copies_advance_occurrences_without_double_counting_own_next() {
        let (core, host) = fixture(3, true).await;
        host.engine.lock().rows[1] = track("t0");
        core.select(&host, Some("phone".into())).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 5000));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| remote.anchored -= Duration::from_secs(100));
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 200));
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(1));
        assert_eq!(host.engine.lock().current, Some(1));
        core.run(&host, Action::Previous).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 0));
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(0));
        core.run(&host, Action::Next).await.unwrap();
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(1));
    }

    #[tokio::test]
    async fn explicit_empty_queue_clears_both_arrays_and_never_restores_old_rows() {
        let (core, host) = owned(2).await;
        core.run(&host, Action::Queue(Vec::new(), 0, String::new(), false)).await.unwrap();
        let events = host.events.lock();
        let clear = events.iter().rev().find(|event| event.get("queue").is_some()).unwrap();
        assert_eq!(clear["queue"], json!([]));
        assert_eq!(clear["upcoming"], json!([]));
        drop(events);
        core.select(&host, None).await.unwrap();
        assert!(host.engine.lock().rows.is_empty());
        assert_eq!(host.engine.lock().current, None);
    }

    #[tokio::test]
    async fn a_paused_edit_stages_new_order_until_resume_and_clears_recovered_error() {
        let (core, host) = owned(3).await;
        core.run(&host, Action::Pause).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", false, 1000));
        core.sync(&host).await.unwrap();
        host.spotify.lock().log.clear();
        core.run(&host, Action::Move(2, 1)).await.unwrap();
        assert!(host.spotify.lock().writes().is_empty());
        assert_eq!(core.snapshot().unwrap().upcoming, [1, 2]);
        remote(&core, |remote| remote.state.error = "old failure".into());
        core.run(&host, Action::Play).await.unwrap();
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t2", "spotify:track:t1"]);
        assert_eq!(play["position_ms"], 1000);
        assert!(core.snapshot().unwrap().error.is_empty());
    }

    #[tokio::test]
    async fn repeat_write_failure_retries_without_mistaking_old_setting_for_phone_takeover() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Repeat("track".into())).await.is_err());
        assert_eq!(host.engine.lock().repeat, "track");
        host.spotify.lock().player.as_mut().unwrap()["repeat_state"] = json!("track");
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
        assert!(!remote(&core, |remote| remote.repeat_dirty));
        assert!(core.snapshot().unwrap().error.is_empty());
    }

    #[tokio::test]
    async fn returning_local_never_overwrites_newer_canonical_rows() {
        let (core, host) = owned(2).await;
        // A canonical write landed outside the cached projection.
        host.engine.lock().rows.push(track("newer"));
        host.engine.lock().revision += 1;
        core.select(&host, None).await.unwrap();
        assert_eq!(host.engine.lock().rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "newer"]);
        assert_eq!(host.engine.lock().current, Some(0));
    }

    /// Whether anything has woken the watcher since the last look.
    async fn woken(core: &Core) -> bool {
        let notified = core.changed.notified();
        tokio::pin!(notified);
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            notified.as_mut().poll(cx).is_ready()
        )).await
    }

    #[tokio::test]
    async fn devices_off_hands_playback_back_then_nothing_wakes_and_on_needs_no_new_grant() {
        let (core, host) = owned(3).await;
        woken(&core).await;
        core.set_enabled(&host, false).await;
        // The one hand-back path: the device is paused, and the engine takes
        // its row here, paused.
        assert_eq!(host.spotify.lock().writes(), ["PUT me/player/pause?device_id=phone"]);
        assert!(core.snapshot().is_none());
        assert!(!host.engine.lock().playing);
        assert_eq!(host.engine.lock().current, Some(0));
        woken(&core).await;
        host.spotify.lock().log.clear();

        // Off: a selection is refused before any request, the window's
        // visibility wakes nothing, local playback reaches no device, and the
        // watcher is never woken.
        assert!(core.select(&host, Some("phone".into())).await.is_err());
        core.set_visible(false, false);
        core.set_visible(true, true);
        core.run(&host, Action::Play).await.unwrap();
        assert!(host.spotify.lock().log.is_empty());
        assert!(!woken(&core).await);

        // On again is instant: the same grant serves the next selection.
        core.set_enabled(&host, true).await;
        core.select(&host, Some("phone".into())).await.unwrap();
        assert!(core.snapshot().is_some());
        assert!(woken(&core).await);
    }

    #[tokio::test]
    async fn clear_pause_failure_retries_while_preserving_explicit_empty_queue() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Queue(Vec::new(), 0, String::new(), false)).await.is_err());
        assert!(core.snapshot().unwrap().queue.is_empty());
        assert!(remote(&core, |remote| remote.pause_dirty && remote.polling));
        core.reconcile(&host).await;
        assert!(!remote(&core, |remote| remote.pause_dirty || remote.polling));
        assert!(host.engine.lock().rows.is_empty());
        assert!(core.snapshot().unwrap().error.is_empty());
    }

    #[tokio::test]
    async fn unavailable_direct_play_does_not_replace_the_current_canonical_queue() {
        let (core, host) = owned(2).await;
        let mut unavailable = track("blocked");
        unavailable.unavailable = true;
        assert!(core.run(&host, Action::Queue(vec![unavailable], 0, String::new(), false)).await.is_err());
        assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:t0");
        assert_eq!(host.engine.lock().rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1"]);
        assert!(host.spotify.lock().writes().is_empty());
    }

    #[tokio::test]
    async fn moved_or_restricted_devices_suspend_without_starting_local_audio() {
        for restricted in [false, true] {
            let (core, host) = owned(2).await;
            let mut player = reading("spotify:track:t0", true, 2000);
            if restricted { player["device"]["is_restricted"] = json!(true); }
            else { player["device"]["id"] = json!("elsewhere"); }
            host.spotify.lock().player = Some(player);
            core.reconcile(&host).await;
            assert!(!remote(&core, |remote| remote.polling));
            assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
            assert!(!host.engine.lock().playing);
            assert!(host.spotify.lock().writes().is_empty());
            assert!(!core.snapshot().unwrap().error.is_empty(), "suspension is visible to the user");
        }
    }

    #[tokio::test]
    async fn engine_restart_reconciles_device_progress_and_restored_metadata_without_restarting_matching_window() {
        let (core, host) = owned(150).await;
        core.engine_disconnected(&host, "engine disconnected");
        assert!(next_read(&core).is_none());
        {
            let mut engine = host.engine.lock();
            engine.revision += 1;
            engine.rows[1].name = "Restored title".into();
            engine.position_ms = 0;
            engine.playing = true;
        }
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 42_000));
        let ready = host.local_state();
        core.engine_recovered(&host, &ready).await;
        let shown = core.snapshot().unwrap();
        assert_eq!(shown.current_uri, "spotify:track:t1");
        assert_eq!(shown.queue[1].name, "Restored title");
        assert!((42_000..43_000).contains(&shown.position_ms));
        assert!(shown.error.is_empty());
        assert!(!host.engine.lock().playing, "local audio stays paused");
        assert_eq!(host.engine.lock().position_ms, 42_000);
        assert!(next_read(&core).is_some());
        assert!(host.spotify.lock().writes().is_empty(), "matching device window is not restarted");
        host.spotify.lock().player = Some(reading("spotify:track:t95", true, 1234));
        core.reconcile(&host).await;
        assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:t95");
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play)[0], "spotify:track:t95");
        assert_eq!(play["position_ms"], 1234, "window extension preserves progress");
    }

    #[tokio::test]
    async fn engine_restart_uses_restored_canonical_eligibility_and_preserves_staged_output() {
        for staged in [false, true] {
            let (core, host) = if staged { fixture(3, false).await } else { owned(3).await };
            if staged { core.select(&host, Some("phone".into())).await.unwrap(); }
            host.spotify.lock().log.clear();
            core.engine_disconnected(&host, "engine disconnected");
            {
                let mut engine = host.engine.lock();
                engine.revision += 1;
                engine.excluded.insert("t1".into());
            }
            if !staged { host.spotify.lock().player = Some(reading("spotify:track:t0", true, 4321)); }
            core.engine_recovered(&host, &host.local_state()).await;
            let state = core.snapshot().unwrap();
            assert_eq!(state.upcoming, [2]);
            assert!(!host.engine.lock().playing);
            if staged {
                assert!(!state.playing);
                assert!(host.spotify.lock().writes().is_empty());
                assert!(next_read(&core).is_none());
            } else {
                let play = host.spotify.lock().last_play().unwrap();
                assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t2"]);
                assert_eq!(play["position_ms"], 4321);
            }
        }
    }

    #[tokio::test]
    async fn restored_engine_redraw_replaces_remote_shuffle_plan_at_device_progress() {
        struct RedrawHost {
            base: TestHost,
        }
        impl Host for RedrawHost {
            async fn spotify(&self, method: Method, path: String, body: Option<Value>) -> Result<Option<Value>, String> {
                self.base.spotify(method, path, body).await
            }
            async fn engine(&self, op: Option<EngineOp<'_>>) -> Result<EngineView, String> {
                let mut view = self.base.engine(op).await?;
                let mut state = view.state.take().unwrap_or_else(|| self.base.local_state());
                state.shuffle = true;
                view.upcoming = [2, 1].into_iter().filter(|row| Some(*row) != state.current_index).collect();
                state.upcoming = view.upcoming.clone();
                view.state = Some(state);
                Ok(view)
            }
            async fn exclusions(&self, context: &str) -> Result<Vec<String>, String> { self.base.exclusions(context).await }
            async fn pause_engine(&self) -> Result<(), String> { self.base.pause_engine().await }
            async fn local(&self, action: Action) -> Result<(), String> { self.base.local(action).await }
            fn local_state(&self) -> PlaybackState { self.base.local_state() }
            fn publish_local(&self) { self.base.publish_local(); }
            fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) { self.base.emit(event, payload); }
            fn media(&self, state: &PlaybackState) { self.base.media(state); }
        }
        let (core, base) = owned(3).await;
        remote(&core, |remote| remote.state.shuffle = true);
        core.engine_disconnected(&base, "engine disconnected");
        base.engine.lock().revision += 1;
        base.spotify.lock().player = Some(reading("spotify:track:t0", true, 4321));
        let host = RedrawHost { base };
        core.engine_recovered(&host, &host.local_state()).await;
        let state = core.snapshot().unwrap();
        assert_eq!(state.upcoming, [2, 1]);
        let play = host.base.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t2", "spotify:track:t1"]);
        assert_eq!(play["position_ms"], 4321);
        assert!(!host.base.engine.lock().playing);
    }

    #[tokio::test]
    async fn engine_ready_cannot_revive_moved_output_or_recover_under_a_different_account() {
        let (core, host) = owned(3).await;
        core.suspend(&host, MOVED_AWAY);
        core.engine_disconnected(&host, "engine disconnected");
        core.engine_recovered(&host, &host.local_state()).await;
        assert!(next_read(&core).is_none());
        assert!(host.spotify.lock().log.is_empty());
        let (core, host) = owned(3).await;
        core.engine_disconnected(&host, "engine disconnected");
        let ready = PlaybackState { username: "other-account".into(), ..host.local_state() };
        core.engine_recovered(&host, &ready).await;
        assert!(next_read(&core).is_none());
        assert!(host.spotify.lock().log.is_empty());
    }

    #[tokio::test]
    async fn engine_restart_keeps_foreign_queue_and_reads_its_device_without_adopting_local_rows() {
        let (core, host) = owned(3).await;
        host.spotify.lock().player = Some(reading("spotify:track:foreign", true, 12_000));
        core.sync(&host).await.unwrap();
        core.engine_disconnected(&host, "engine disconnected");
        host.spotify.lock().log.clear();
        core.engine_recovered(&host, &host.local_state()).await;
        let state = core.snapshot().unwrap();
        assert_eq!(state.current_uri, "spotify:track:foreign");
        assert_eq!(state.queue[0].id, "foreign");
        assert!(!host.engine.lock().playing);
        assert!(next_read(&core).is_some());
        assert!(host.spotify.lock().writes().is_empty());
    }

    /// The router owns the phone, which plays `t0` of a queue of `rows`.
    async fn owned(rows: usize) -> (Core, TestHost) {
        let (core, host) = fixture(rows, true).await;
        core.select(&host, Some("phone".into())).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 1000));
        // Spotify confirms the start, settling the router's own expectation.
        core.sync(&host).await.unwrap();
        assert!(remote(&core, |remote| remote.pending.is_none()));
        host.spotify.lock().log.clear();
        (core, host)
    }

    fn remote<R>(core: &Core, read: impl FnOnce(&mut Remote) -> R) -> R { read(core.remote.lock().as_mut().unwrap()) }

    fn window_uris(remote: &Remote) -> Vec<String> {
        match &remote.mode { Mode::Owned(window) => window.uris.clone(), _ => panic!("the router does not own the device") }
    }

    fn uris(body: &Value) -> Vec<&str> { body["uris"].as_array().unwrap().iter().map(|uri| uri.as_str().unwrap()).collect() }

    #[tokio::test]
    async fn selecting_a_playing_queue_hands_spotify_a_window_of_the_engine_order() {
        let (core, host) = fixture(3, true).await;
        host.engine.lock().position_ms = 4321;
        core.select(&host, Some("phone".into())).await.unwrap();
        let spotify = host.spotify.lock();
        assert_eq!(spotify.writes(), [
            "PUT me/player", "PUT me/player/shuffle?device_id=phone&state=false",
            "PUT me/player/repeat?device_id=phone&state=off", "PUT me/player/play?device_id=phone",
        ]);
        let play = spotify.last_play().unwrap();
        assert_eq!(play, json!({"uris": ["spotify:track:t0", "spotify:track:t1", "spotify:track:t2"], "offset": {"position": 0}, "position_ms": 4321}));
        assert!(host.engine.lock().ops.is_empty(), "the engine queue is not touched by a handover");
        assert_eq!(remote(&core, |remote| remote.state.upcoming.clone()), [1, 2]);
    }

    #[tokio::test]
    async fn a_stale_reading_after_next_is_retried_and_never_takes_control_away() {
        let (core, host) = owned(3).await;
        core.sync(&host).await.unwrap();
        core.run(&host, Action::Next).await.unwrap();
        assert_eq!(host.spotify.lock().writes(), ["POST me/player/next?device_id=phone"]);
        assert_eq!(remote(&core, |remote| remote.state.current_index), Some(1));
        // Spotify still reports the item before the skip.
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Owned);
            assert_eq!(remote.state.current_index, Some(1));
            assert_eq!(remote.pending.as_ref().unwrap().retries, 1);
        });
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 0, "a stale reading is not an outside change");
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 400));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert!(remote.pending.is_none());
            assert_eq!(remote.state.current_index, Some(1));
            assert!(matches!(&remote.mode, Mode::Owned(window) if window.at == 1));
        });
        assert_eq!(host.engine.lock().current, Some(1));
    }

    #[tokio::test]
    async fn adding_while_the_device_plays_reaches_spotify_without_restarting_the_song() {
        let (core, host) = owned(2).await;
        core.sync(&host).await.unwrap();
        remote(&core, |remote| remote.anchored -= Duration::from_secs(5));
        core.run(&host, Action::Add(track("added"), String::new())).await.unwrap();
        assert_eq!(host.engine.lock().ops, ["add added"]);
        let play = host.spotify.lock().last_play().expect("the short window is extended with the addition");
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t1", "spotify:track:added"]);
        assert!(play["position_ms"].as_u64().unwrap() >= 6000, "resumes where the song was, not from 0");
        assert_eq!(remote(&core, |remote| remote.state.upcoming.clone()), [1, 2]);

        // A long window already holds enough: the addition waits for its turn.
        let (core, host) = owned(30).await;
        core.sync(&host).await.unwrap();
        core.run(&host, Action::Add(track("late"), String::new())).await.unwrap();
        assert!(host.spotify.lock().last_play().is_none());
        assert_eq!(remote(&core, |remote| remote.state.upcoming.last().copied()), Some(30));
    }

    #[tokio::test]
    async fn adding_while_the_phone_plays_its_own_queue_uses_spotifys_queue() {
        let (core, host) = owned(2).await;
        host.spotify.lock().player = Some(reading("spotify:track:phone", true, 30_000));
        core.sync(&host).await.unwrap();
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Foreign);
        host.spotify.lock().queue = vec![json!({"id": "added", "uri": "spotify:track:added", "name": "added", "duration_ms": 1000})];
        core.run(&host, Action::Add(track("added"), String::new())).await.unwrap();
        assert!(host.spotify.lock().writes().contains(&"POST me/player/queue?device_id=phone&uri=spotify%3Atrack%3Aadded".to_owned()));
        assert!(host.engine.lock().ops.is_empty(), "the phone's queue is not the engine's");
        assert_eq!(remote(&core, |remote| remote.state.queue.iter().map(|track| track.id.clone()).collect::<Vec<_>>()), ["phone", "added"]);
    }

    #[tokio::test]
    async fn a_transient_failure_backs_off_and_keeps_control() {
        let (core, host) = owned(3).await;
        core.sync(&host).await.unwrap();
        host.spotify.lock().failures.extend([502, 502]);
        core.reconcile(&host).await;
        remote(&core, |remote| {
            assert!(remote.polling);
            assert_eq!(remote.kind(), Kind::Owned);
            assert_eq!(remote.failures, 1);
            assert!(remote.state.error.is_empty(), "one failure is not worth a message");
            assert_eq!(reconcile_delay(remote, true), Some(Duration::from_secs(2)));
        });
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| reconcile_delay(remote, false)), Some(Duration::from_secs(4)));
        core.reconcile(&host).await;
        remote(&core, |remote| {
            assert_eq!(remote.failures, 0);
            assert!(remote.polling);
        });
        // A permanent failure still stops reconciling with a visible error.
        host.spotify.lock().failures.push_back(401);
        core.reconcile(&host).await;
        remote(&core, |remote| {
            assert!(!remote.polling);
            assert!(!remote.state.error.is_empty(), "permanent authorization failure is visible");
        });
    }

    #[tokio::test]
    async fn the_device_plays_through_the_window_and_the_end_of_the_queue_is_seen() {
        let (core, host) = owned(2).await;
        core.sync(&host).await.unwrap();
        // Spotify moved on by itself: nothing is sent, the view follows.
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 800));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert_eq!(remote.state.current_index, Some(1));
            assert!(remote.state.playing);
            assert!(remote.state.upcoming.is_empty());
        });
        assert!(host.spotify.lock().writes().is_empty());
        // With autoplay off a finished single item can read as paused at 0.
        remote(&core, |remote| remote.anchored -= Duration::from_secs(120));
        host.spotify.lock().player = Some(reading("spotify:track:t1", false, 0));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert!(!remote.state.playing);
            assert_eq!(remote.kind(), Kind::Owned);
        });

        // With autoplay on, a song after the last entry is stopped, not adopted.
        let (core, host) = owned(2).await;
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 800));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| remote.anchored -= Duration::from_secs(120));
        host.spotify.lock().player = Some(reading("spotify:track:autoplay", true, 900));
        core.sync(&host).await.unwrap();
        assert_eq!(host.spotify.lock().writes(), ["PUT me/player/pause?device_id=phone"]);
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Owned);
            assert!(!remote.state.playing);
            assert_eq!(remote.state.current_index, Some(1));
        });
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 0);
    }

    #[tokio::test]
    async fn a_track_change_near_the_end_of_the_window_extends_it() {
        let (core, host) = owned(150).await;
        core.sync(&host).await.unwrap();
        assert_eq!(remote(&core, |remote| window_uris(remote).len()), WINDOW);
        host.spotify.lock().player = Some(reading("spotify:track:t91", true, 1200));
        core.sync(&host).await.unwrap();
        let play = host.spotify.lock().last_play().expect("eight entries left is too few");
        let sent = uris(&play);
        assert_eq!(sent.len(), 59);
        assert_eq!((sent[0], sent[58]), ("spotify:track:t91", "spotify:track:t149"));
        assert!((1200..1300).contains(&play["position_ms"].as_u64().unwrap()));
        // The extension's own reading is pending until Spotify reports it.
        assert!(remote(&core, |remote| remote.pending.is_some()));
    }

    #[tokio::test]
    async fn the_phone_starting_something_else_is_shown_without_touching_the_engine() {
        let (core, host) = owned(3).await;
        core.sync(&host).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:podcast", true, 30_000));
        host.spotify.lock().queue = vec![json!({"id": "next", "uri": "spotify:track:next", "name": "next", "duration_ms": 1000})];
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Foreign);
            assert_eq!(remote.state.current_uri, "spotify:track:podcast");
            assert_eq!(remote.state.upcoming, [1]);
            assert!(remote.state.queue_revision <= MAX_REVISION);
        });
        assert!(host.engine.lock().ops.is_empty());
        // No more window: later readings are only shown.
        host.spotify.lock().player = Some(reading("spotify:track:podcast", false, 31_000));
        core.sync(&host).await.unwrap();
        assert!(host.spotify.lock().last_play().is_none());
        // This computer keeps the local queue the phone never played.
        core.select(&host, None).await.unwrap();
        assert!(core.remote.lock().is_none());
        assert!(host.engine.lock().ops.is_empty());
        assert_eq!(*host.local_shown.lock(), 1);
    }

    #[tokio::test]
    async fn this_computer_resumes_where_the_device_played_the_engine_order() {
        let (core, host) = owned(3).await;
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 42_000));
        core.sync(&host).await.unwrap();
        host.spotify.lock().log.clear();
        core.select(&host, None).await.unwrap();
        assert_eq!(host.spotify.lock().writes(), ["PUT me/player/pause?device_id=phone"]);
        let engine = host.engine.lock();
        assert_eq!(engine.current, Some(1));
        assert!((42_000..43_000).contains(&engine.position_ms));
        assert_eq!(engine.rows.iter().map(|track| track.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "t2"]);
    }

    #[tokio::test]
    async fn a_paused_selection_waits_for_play_and_edits_stay_in_the_engine() {
        let (core, host) = fixture(3, false).await;
        core.select(&host, Some("phone".into())).await.unwrap();
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Prepared);
        assert!(!remote(&core, |remote| remote.polling), "nothing to watch before Play");
        core.run(&host, Action::Move(2, 1)).await.unwrap();
        assert_eq!(host.engine.lock().ops, ["move 2 1"]);
        assert!(host.spotify.lock().last_play().is_none());
        core.run(&host, Action::Play).await.unwrap();
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t2", "spotify:track:t1"]);
    }

    #[tokio::test]
    async fn removing_the_playing_row_continues_with_the_next_one() {
        let (core, host) = owned(3).await;
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 5000));
        core.sync(&host).await.unwrap();
        core.run(&host, Action::Remove(1)).await.unwrap();
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t2"]);
        assert_eq!(play["position_ms"], 0);
        assert_eq!(remote(&core, |remote| remote.state.current_index), Some(1));
    }

    #[test]
    fn the_order_from_the_device_is_read_off_the_engine_order() {
        let none = HashSet::new();
        // Sequential: what follows the device's row.
        assert_eq!(order_from(Some(0), &[1, 2, 3], false, "off", Some(2), 2, &none), Some(vec![3]));
        // A removed row resumes at its position, the engine's row included.
        assert_eq!(order_from(Some(2), &[3, 4], false, "off", None, 2, &none), Some(vec![2, 3, 4]));
        // Behind the engine's own row nothing is known.
        assert_eq!(order_from(Some(2), &[3], false, "off", Some(1), 1, &none), None);
        // Repeat walks one lap from after the device's row.
        assert_eq!(order_from(Some(0), &[1, 2, 3, 0], false, "context", Some(2), 2, &none), Some(vec![3, 0, 1, 2]));
        // Shuffle: the remaining draws, without the ones the device spent.
        let spent = HashSet::from([0, 7]);
        assert_eq!(order_from(Some(0), &[7, 3, 5, 1], true, "off", Some(3), 3, &spent), Some(vec![5, 1]));
    }

    #[test]
    fn a_window_is_resent_only_when_spotify_holds_the_wrong_order_or_too_little() {
        let window = |rows: &[usize], at: usize| Window { rows: rows.iter().copied().map(Some).collect(), uris: Vec::new(), at, stale: false, expired: false };
        let long: Vec<usize> = (1..=20).collect();
        assert!(!window_differs(&window(&(0..=20).collect::<Vec<_>>(), 0), 0, &long));
        assert!(window_differs(&window(&(0..=20).collect::<Vec<_>>(), 0), 1, &long), "another row plays");
        let mut reordered: Vec<usize> = (0..=20).collect();
        reordered.swap(3, 4);
        assert!(window_differs(&window(&reordered, 0), 0, &long));
        assert!(window_differs(&window(&[0, 1, 2], 0), 0, &[1, 2, 3]), "low and the order goes on");
        assert!(!window_differs(&window(&[0, 1, 2], 0), 0, &[1, 2]), "low but nothing more to send");
    }

    #[test]
    fn edits_carry_rows_to_their_new_places() {
        assert_eq!((0..4).map(|row| Remap::Remove(1).apply(row)).collect::<Vec<_>>(), [Some(0), None, Some(1), Some(2)]);
        assert_eq!((0..4).map(|row| Remap::Move(3, 0).apply(row).unwrap()).collect::<Vec<_>>(), [1, 2, 3, 0]);
        assert_eq!((0..4).map(|row| Remap::Move(0, 2).apply(row).unwrap()).collect::<Vec<_>>(), [2, 0, 1, 3]);
    }

    #[test]
    fn positions_do_not_drift_while_paused_or_past_the_end() {
        let mut state = PlaybackState { playing: false, position_ms: 90000, duration_ms: 100000, ..PlaybackState::default() };
        assert_eq!(projected_position(&state, Duration::from_secs(60)), 90000);
        state.playing = true;
        assert_eq!(projected_position(&state, Duration::from_secs(60)), 100000);
        assert_eq!(projected_position(&state, Duration::from_secs(2)), 92000);
    }

    #[test]
    fn the_phones_own_items_are_shown_with_their_details_and_duplicates() {
        let player: PlayerState = serde_json::from_value(json!({
            "device": device(), "is_playing": true, "progress_ms": 4321, "shuffle_state": false, "repeat_state": "off",
            "item": {"id":"episode", "uri":"spotify:episode:episode", "name":"New episode", "type":"episode", "duration_ms":500000, "images":[{"url":"cover"}], "show":{"id":"show", "name":"Show", "publisher":"Publisher"}}
        })).unwrap();
        let queue = vec![json!({"id":"a", "uri":"spotify:track:a", "name":"a", "duration_ms":100000}),
            json!({"id":"a", "uri":"spotify:track:a", "name":"a", "duration_ms":100000})];
        let mut remote = Remote::new(PlaybackState { queue: vec![track("old")].into(), current_index: Some(0), queue_revision: 7, ..PlaybackState::default() }, true);
        remote.show_foreign(Some(&player), &queue);
        assert_eq!(remote.state.current_uri, "spotify:episode:episode");
        assert_eq!(remote.state.queue[0].artist_names, vec!["Publisher"]);
        assert_eq!(remote.state.queue[0].cover_url, "cover");
        assert_eq!(remote.state.duration_ms, 500000);
        assert_eq!(remote.state.upcoming, [1, 2]);
        assert_eq!(remote.state.queue_revision, 8);
        remote.show_foreign(None, &[]);
        assert_eq!(remote.state.current_index, None);
        assert_eq!(remote.state.current_uri, "");
    }

    /// When the watcher would next read the device, as `watch` decides it.
    fn next_read(core: &Core) -> Option<Duration> {
        let visible = core.visible.load(Ordering::Relaxed);
        core.remote.lock().as_ref().filter(|remote| remote.polling).and_then(|remote| reconcile_delay(remote, visible))
    }

    #[tokio::test]
    async fn hiding_a_paused_window_stops_reading_and_showing_it_again_reads_at_once() {
        let (core, host) = owned(3).await;
        core.run(&host, Action::Pause).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", false, 1000));
        core.sync(&host).await.unwrap();
        assert_eq!(next_read(&core), Some(VISIBLE_POLL));
        core.set_visible(false, false);
        assert_eq!(next_read(&core), None, "hidden and paused does no work");
        core.set_visible(true, false);
        assert_eq!(next_read(&core), Some(Duration::ZERO), "coming back into view reads at once");
        core.sync(&host).await.unwrap();
        assert_eq!(next_read(&core), Some(VISIBLE_POLL));
        core.set_visible(true, true);
        assert_eq!(next_read(&core), Some(Duration::ZERO), "focus reads at once");
        core.sync(&host).await.unwrap();

        // Hidden and playing, only the track end is read.
        core.set_visible(false, false);
        core.run(&host, Action::Play).await.unwrap();
        assert_eq!(next_read(&core), Some(Duration::from_millis(750)), "the command is confirmed");
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 40_000));
        core.sync(&host).await.unwrap();
        let end = next_read(&core).unwrap();
        assert!((Duration::from_millis(60_000)..=Duration::from_millis(60_750)).contains(&end), "{end:?}");
    }

    #[tokio::test]
    async fn the_phones_queue_is_read_only_for_a_new_item_or_a_fresh_look() {
        let (core, host) = owned(2).await;
        host.spotify.lock().player = Some(reading("spotify:track:phone", true, 30_000));
        core.sync(&host).await.unwrap();
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Foreign);
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 1);
        for _ in 0..3 { core.sync(&host).await.unwrap(); }
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 1, "the same item keeps its rows");
        core.set_visible(true, true);
        core.sync(&host).await.unwrap();
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 2, "a fresh look reads it");
        host.spotify.lock().player = Some(reading("spotify:track:other", true, 100));
        core.sync(&host).await.unwrap();
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 3, "a new item reads it");
        assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:other");
    }

    #[tokio::test]
    async fn a_reading_that_confirms_what_is_shown_publishes_nothing() {
        let (core, host) = owned(3).await;
        host.events.lock().clear();
        for _ in 0..3 { core.sync(&host).await.unwrap(); }
        assert!(host.events.lock().is_empty(), "the playhead moved as projected");
        host.spotify.lock().player.as_mut().unwrap()["device"]["volume_percent"] = json!(60);
        core.sync(&host).await.unwrap();
        assert_eq!(host.events.lock().len(), 1);
        // A seek on the phone moves the playhead away from its projection.
        host.spotify.lock().player.as_mut().unwrap()["progress_ms"] = json!(50_000);
        core.sync(&host).await.unwrap();
        let events = host.events.lock();
        assert_eq!(events.len(), 2);
        assert!(events[1]["position_ms"].as_u64().unwrap() >= 50_000);
    }

    #[tokio::test]
    async fn an_expired_start_waits_for_play_instead_of_starting_late() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Queue(vec![track("new0"), track("new1")], 0, String::new(), false)).await.is_err());
        remote(&core, |remote| remote.start_intent.as_mut().unwrap().since -= START_DEADLINE);
        // Spotify answers again, and the speaker still plays the old window.
        host.spotify.lock().log.clear();
        for _ in 0..3 { core.reconcile(&host).await; }
        assert!(host.spotify.lock().writes().is_empty(), "nothing starts long after it was chosen");
        let state = core.snapshot().unwrap();
        assert_eq!(state.current_uri, "spotify:track:new0");
        assert!(!state.playing);
        remote(&core, |remote| {
            assert!(remote.start_intent.is_none());
            assert!(matches!(&remote.mode, Mode::Owned(window) if window.stale && window.expired));
        });
        core.run(&host, Action::Play).await.unwrap();
        assert_eq!(uris(&host.spotify.lock().last_play().unwrap()), ["spotify:track:new0", "spotify:track:new1"]);
    }

    #[tokio::test]
    async fn under_repeat_a_short_queue_fills_the_window_with_laps() {
        let (core, host) = fixture(3, true).await;
        host.engine.lock().repeat = "context".into();
        core.select(&host, Some("phone".into())).await.unwrap();
        let play = host.spotify.lock().last_play().unwrap();
        let sent = uris(&play);
        assert_eq!(sent.len(), WINDOW);
        assert_eq!(sent[..5], ["spotify:track:t0", "spotify:track:t1", "spotify:track:t2", "spotify:track:t0", "spotify:track:t1"]);
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 1000));
        core.sync(&host).await.unwrap();
        for (row, uri) in [(1, "spotify:track:t1"), (2, "spotify:track:t2"), (0, "spotify:track:t0")] {
            host.spotify.lock().player = Some(reading(uri, true, 500));
            core.sync(&host).await.unwrap();
            assert_eq!(core.snapshot().unwrap().current_index, Some(row));
        }
        let plays = host.spotify.lock().log.iter().filter(|(method, path, _)| method == "PUT" && path.starts_with("me/player/play?")).count();
        assert_eq!(plays, 1, "the device plays its laps without a new window");
    }

    #[tokio::test]
    async fn a_remote_start_publishes_once_with_what_spotify_answered() {
        let (core, host) = owned(3).await;
        host.events.lock().clear();
        core.run(&host, Action::Index(2)).await.unwrap();
        core.run(&host, Action::Queue(vec![track("a"), track("b")], 1, String::new(), false)).await.unwrap();
        let events = host.events.lock();
        assert_eq!(events.len(), 2, "one publish per start");
        assert!(events.iter().all(|event| event["playing"] == json!(true)), "the play button never flickers to paused");
        assert_eq!(events[1]["current_uri"], "spotify:track:b");
    }

    #[tokio::test]
    async fn pausing_a_failed_staged_start_settles_to_no_work() {
        let (core, host) = fixture(3, false).await;
        core.select(&host, Some("phone".into())).await.unwrap();
        host.spotify.lock().write_failures.extend([502, 502]);
        assert!(core.run(&host, Action::Play).await.is_err());
        core.reconcile(&host).await;
        assert!(remote(&core, |remote| remote.failures) > 0);
        core.run(&host, Action::Pause).await.unwrap();
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Prepared);
        assert_eq!(next_read(&core), None, "nothing is left to watch");
    }

    #[tokio::test]
    async fn a_failed_window_write_after_an_exclusion_is_reported_and_retried() {
        let (core, host) = owned(3).await;
        host.spotify.lock().write_failures.push_back(502);
        host.engine.lock().excluded.insert("t1".into());
        core.eligibility_changed(&host).await;
        assert!(!core.snapshot().unwrap().error.is_empty());
        assert!(remote(&core, |remote| matches!(&remote.mode, Mode::Owned(window) if window.stale)));
        core.reconcile(&host).await;
        assert_eq!(uris(&host.spotify.lock().last_play().unwrap()), ["spotify:track:t0", "spotify:track:t2"]);
        assert!(core.snapshot().unwrap().error.is_empty());
    }
}
