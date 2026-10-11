use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use renderer_engine::atomic::replace_file_atomically;
use renderer_engine::protocol::{
    HistoryItem, HistoryPage, HistoryQuery, HistoryRow, HistorySort, TrackRef,
};
use serde::{Deserialize, Serialize};

/// The archive: one finalized row per line, oldest first, appended to.
const JOURNAL_FILE: &str = "listening_history.jsonl";
/// The in-progress row, alone, so that keeping it durable costs one small
/// atomic replace per playback event instead of rewriting the archive.
const ACTIVE_FILE: &str = "listening_history_active.json";

/// The archive ceiling.
///
/// The old ceiling was 2,000 rows, which at this owner's rate was about a
/// month before the oldest play was silently dropped — unacceptable for
/// something kept as a personal record. Raising it was only safe once the
/// write path stopped rewriting the whole file per event: appending one line
/// costs the same at fifty thousand rows as at fifty.
///
/// The rows still live in memory, because the engine has to answer filtered
/// and name-ordered queries over the whole archive and an index that could do
/// that would hold the same strings anyway. A sanitized row measures a few
/// hundred bytes (the test below asserts the figure this number was chosen
/// against), so a full archive is tens of megabytes — a real cost, accepted
/// deliberately, and bounded rather than open-ended.
const MAX_HISTORY_ITEMS: usize = 50_000;

/// How far past the ceiling the archive is allowed to run before it is
/// trimmed. Trimming means rewriting the journal, so doing it the instant the
/// ceiling is reached would turn every later play into a whole-file rewrite —
/// exactly the cost the append-only journal exists to remove. With slack, a
/// rewrite happens once per thousand plays instead of once per play.
const COMPACT_SLACK: usize = 1_000;

/// How much of a track has to be heard before the play joins the archive.
///
/// The threshold is here to reject spam, and nothing else: spinning through a
/// queue or bouncing on Next used to write a row per press, which is most of
/// why the archive filled with plays its owner never made. It is not a ruling
/// on what counts as listening. Five seconds is enough to tell "I skipped past
/// this" from "I heard it and moved on", and a short listen the owner meant to
/// have belongs in the record.
///
/// This was thirty seconds, borrowed from the convention other players count a
/// play at. That convention is a broadcast-royalty standard wearing a product's
/// clothes; imported here it silently discarded real plays to enforce a rule
/// nobody in this house had asked for.
///
/// The floor is still capped at half the track, so no length is unrecordable
/// short of playing it through: a nine-second skit answers to four and a half
/// seconds. At thirty that halving carried interludes and short songs alike; at
/// five it reaches only tracks under ten seconds, which is too narrow a job to
/// name a constant after — hence a bare `/ 2` at the single place it applies.
/// Reaching the end always qualifies, whatever the length.
const QUALIFYING_MS: u64 = 5_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedActive {
    item: HistoryItem,
    duration_ms: u32,
}

struct ActivePlay {
    persisted: PersistedActive,
    keys: SearchKeys,
    playing_since: Option<Instant>,
}

impl ActivePlay {
    fn new(persisted: PersistedActive, playing_since: Option<Instant>) -> Self {
        let keys = SearchKeys::of(&persisted.item);
        Self { persisted, keys, playing_since }
    }
}

/// A row's title and artists, case-folded once when the row enters memory.
///
/// Search and the name orders compare these, so a query re-folds nothing: a
/// keystroke in the history filter used to lowercase every title (and every
/// artist the title did not match) in a fifty-thousand-row archive.
struct SearchKeys {
    title: Box<str>,
    /// The artist names, folded and joined by [`ARTIST_SEPARATOR`] — a
    /// character no typed query contains, so a match never spans two names.
    artists: Box<str>,
}

const ARTIST_SEPARATOR: char = '\u{1f}';

impl SearchKeys {
    fn of(item: &HistoryItem) -> Self {
        let mut artists = String::new();
        for (index, artist) in item.track.artist_names.iter().enumerate() {
            if index > 0 {
                artists.push(ARTIST_SEPARATOR);
            }
            artists.push_str(&artist.to_lowercase());
        }
        Self {
            title: item.track.name.to_lowercase().into_boxed_str(),
            artists: artists.into_boxed_str(),
        }
    }

    /// Whether the row answers an already-folded query.
    fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.title.contains(needle)
            || self.artists.split(ARTIST_SEPARATOR).any(|artist| artist.contains(needle))
    }
}

/// A finalized row together with its folded search keys.
struct ArchivedRow {
    item: HistoryItem,
    keys: SearchKeys,
}

impl ArchivedRow {
    fn new(item: HistoryItem) -> Self {
        let keys = SearchKeys::of(&item);
        Self { item, keys }
    }
}

/// A position in the archive as an ordering sees it. The in-progress play is
/// its own variant rather than an index because it is not in `finalized` yet
/// and its elapsed time is still moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Active,
    Finalized(u32),
}

/// One filtered, ordered projection of the archive, kept until the archive
/// changes. Scrolling asks for consecutive pages of the same projection, so
/// building it per page would re-scan (and for the name orders, re-sort) fifty
/// thousand rows for every fifty the reader actually sees.
struct CachedOrdering {
    query: String,
    sort: HistorySort,
    mutation: u64,
    /// Membership of the in-progress row flips mid-play, with no mutation to
    /// announce it, so it has to be part of what makes this cache stale.
    active_qualified: bool,
    slots: Vec<Slot>,
}

/// A durable, paged listening archive: an append-only journal of finalized
/// rows plus a one-row sidecar for the play in progress.
///
/// Both files are loaded once at startup and never read again; queries are
/// answered from memory. Writes are single-flight: every playback event queues
/// a generation and one background writer appends the new rows, replaces the
/// sidecar, and fsyncs off the transport loop. Superseded sidecar states
/// coalesce (only the newest matters); appended rows never do (each is
/// distinct data).
///
/// An unreadable or invalid archive leaves the store read-only, so a later
/// playback event cannot replace the owner's record with a fresh empty one.
pub struct ListeningHistory {
    root: PathBuf,
    load_error: Option<String>,
    core: Arc<Mutex<HistoryCore>>,
}

/// Pages the archive from any thread. The command loop hands one to a
/// blocking task per request, so a filtered or name-ordered page over the
/// whole archive never holds up transport or browse replies.
#[derive(Clone)]
pub struct HistoryReader {
    load_error: Option<String>,
    core: Arc<Mutex<HistoryCore>>,
}

/// Playback state, the ordering cache, and the generation bookkeeping that
/// keeps exactly one background writer draining to disk.
struct HistoryCore {
    finalized: VecDeque<ArchivedRow>,
    active: Option<ActivePlay>,
    /// Rows already in `finalized` that the journal has not accepted yet.
    /// They stay here until a write succeeds, which is what makes a failed
    /// append retry rather than lose a play.
    pending: VecDeque<HistoryItem>,
    /// Bumped when rows left the front of `finalized`, or when the loaded
    /// journal had to be replaced: the journal no longer matches memory and
    /// must be rewritten whole rather than appended to. A counter rather than
    /// a flag because a rewrite is serialized under the lock and written
    /// outside it, and a trim landing in that gap must not be marked done by
    /// the write it arrived too late for.
    rewrite_requested: u64,
    /// The highest trim generation a rewrite has actually put on disk.
    rewrite_written: u64,
    /// Bumped by every playback event that must reach disk.
    requested_generation: u64,
    /// Highest generation known to be fully written. Advances on success
    /// only, which is what makes a failed write retry on the next event.
    written_generation: u64,
    /// Highest generation any writer has attempted. A failed attempt parks
    /// here so the writer exits instead of spinning on a persistent error.
    attempted_generation: u64,
    worker_running: bool,
    /// Bumped by anything that changes which rows exist, so the ordering
    /// cache can tell whether it still describes the archive.
    mutation: u64,
    ordering: Option<CachedOrdering>,
}

impl HistoryCore {
    fn accrue_active(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some(since) = active.playing_since.take() else {
            return;
        };
        active.persisted.item.row.ms_played = active
            .persisted
            .item
            .row
            .ms_played
            .saturating_add(elapsed_ms(Some(since)))
            .min(u64::from(active.persisted.duration_ms));
    }

    /// The in-progress row as it stands right now, with time accrued since the
    /// last event folded in.
    fn live_active(&self) -> Option<HistoryItem> {
        let active = self.active.as_ref()?;
        let mut item = active.persisted.item.clone();
        if active.playing_since.is_some() {
            item.row.ms_played = item
                .row
                .ms_played
                .saturating_add(elapsed_ms(active.playing_since))
                .min(u64::from(active.persisted.duration_ms));
        }
        Some(item)
    }

    /// Whether the in-progress play has been listened to for long enough to
    /// belong in the archive. The view shows exactly the rows that will be
    /// kept, so a play appears once it qualifies and never disappears again —
    /// no row is written and later withdrawn.
    fn active_qualified(&self) -> bool {
        self.active.as_ref().is_some_and(|active| {
            let elapsed = active
                .persisted
                .item
                .row
                .ms_played
                .saturating_add(elapsed_ms(active.playing_since))
                .min(u64::from(active.persisted.duration_ms));
            play_qualifies(elapsed, active.persisted.item.row.completed, active.persisted.duration_ms)
        })
    }

    /// Moves the in-progress play into the archive if it earned a place, and
    /// discards it otherwise. Evaluating here — rather than committing every
    /// play and withdrawing the trivial ones later — means a trivial play is
    /// never written at all, so no reader of the journal, including recovery
    /// after a crash, can ever observe one.
    fn retire_active(&mut self, completed: bool) {
        self.accrue_active();
        let Some(mut active) = self.active.take() else {
            return;
        };
        if completed {
            active.persisted.item.row.completed = true;
            active.persisted.item.row.ms_played = u64::from(active.persisted.duration_ms);
        }
        let item = active.persisted.item;
        if !play_qualifies(item.row.ms_played, item.row.completed, active.persisted.duration_ms) {
            // Nothing to invalidate: a play that never entered the archive
            // cannot have been in an ordering built from it.
            return;
        }
        self.pending.push_back(item.clone());
        self.finalized.push_back(ArchivedRow { item, keys: active.keys });
        self.enforce_bound();
        self.invalidate();
    }

    /// Trims the archive back to the ceiling once it has run [`COMPACT_SLACK`]
    /// rows past it, and marks the journal for a rewrite because dropping from
    /// the front is the one edit an append cannot express.
    fn enforce_bound(&mut self) {
        if self.finalized.len() <= MAX_HISTORY_ITEMS + COMPACT_SLACK {
            return;
        }
        while self.finalized.len() > MAX_HISTORY_ITEMS {
            self.finalized.pop_front();
        }
        self.rewrite_requested = self.rewrite_requested.wrapping_add(1);
    }

    fn invalidate(&mut self) {
        self.mutation = self.mutation.wrapping_add(1);
        self.ordering = None;
    }
}

/// Whether one play belongs in the archive. See [`QUALIFYING_MS`].
fn play_qualifies(ms_played: u64, completed: bool, duration_ms: u32) -> bool {
    if completed {
        return true;
    }
    let duration = u64::from(duration_ms);
    // An unknown duration cannot supply a fraction, so it answers to the floor
    // alone rather than to a threshold of zero that everything clears.
    let threshold = if duration == 0 {
        QUALIFYING_MS
    } else {
        QUALIFYING_MS.min(duration / 2)
    };
    ms_played >= threshold
}

impl ListeningHistory {
    pub fn new(root: PathBuf) -> Self {
        if root.as_os_str().is_empty() {
            return Self::from_parts(root, VecDeque::new(), None, false, None);
        }

        match load_archive(&root) {
            Ok(loaded) => Self::from_parts(
                root,
                loaded.finalized,
                loaded.active,
                loaded.needs_rewrite,
                None,
            ),
            Err(error) => {
                eprintln!("listening history persistence disabled: {error}");
                Self::from_parts(root, VecDeque::new(), None, false, Some(error))
            }
        }
    }

    fn from_parts(
        root: PathBuf,
        finalized: VecDeque<ArchivedRow>,
        active: Option<ActivePlay>,
        needs_rewrite: bool,
        load_error: Option<String>,
    ) -> Self {
        let store = Self {
            root,
            load_error,
            core: Arc::new(Mutex::new(HistoryCore {
                finalized,
                active,
                pending: VecDeque::new(),
                rewrite_requested: u64::from(needs_rewrite),
                rewrite_written: 0,
                requested_generation: 0,
                written_generation: 0,
                attempted_generation: 0,
                worker_running: false,
                mutation: 0,
                ordering: None,
            })),
        };
        // A trimmed or replaced archive is only in memory until this lands.
        if needs_rewrite {
            store.persist();
        }
        store
    }

    /// Begins a logical play only after the authoritative `Playing` event.
    /// Replacing an active play retires it — into the archive if it qualified,
    /// out of existence if it did not — before the new row is installed.
    pub fn start(&mut self, track: &TrackRef) {
        if self.root.as_os_str().is_empty() || !self.writable() {
            return;
        }

        {
            let mut core = self.lock_core();
            core.retire_active(false);
            core.active = Some(ActivePlay::new(
                PersistedActive {
                    item: HistoryItem {
                        row: HistoryRow {
                            track_id: track.id.clone(),
                            started_at: now_millis(),
                            ms_played: 0,
                            completed: false,
                            context: compact_context(&track.context),
                        },
                        track: sanitize_track(track),
                    },
                    duration_ms: track.duration_ms,
                },
                Some(Instant::now()),
            ));
        }
        // A failed background write leaves the store dirty; pause, finalize,
        // or the next transition queues a fresh generation and retries.
        self.persist();
    }

    pub fn resume(&mut self) {
        {
            let mut core = self.lock_core();
            let Some(active) = core.active.as_mut() else {
                return;
            };
            if active.playing_since.is_some() {
                return;
            }
            active.playing_since = Some(Instant::now());
        }
        self.persist();
    }

    pub fn start_or_resume(&mut self, track: &TrackRef) {
        let same_track = {
            let core = self.lock_core();
            core.active
                .as_ref()
                .is_some_and(|active| active.persisted.item.row.track_id == track.id)
        };
        if same_track {
            self.resume();
        } else {
            self.start(track);
        }
    }

    pub fn pause(&mut self) {
        if !self.writable() {
            return;
        }
        let has_active = {
            let mut core = self.lock_core();
            core.accrue_active();
            core.active.is_some()
        };
        if has_active {
            self.persist();
        }
    }

    /// Retires the current play. The files themselves are written by the
    /// single-flight background writer; a failed write leaves the appended
    /// rows queued so the next playback event retries them.
    pub fn finalize(&mut self, completed: bool) -> bool {
        if !self.writable() {
            return false;
        }
        {
            let mut core = self.lock_core();
            if core.active.is_none() {
                return true;
            }
            core.retire_active(completed);
        }
        self.persist();
        true
    }

    /// Test seam: credit the in-progress play with enough listening to qualify.
    /// Tests about what happens *to* a real play would otherwise have to burn
    /// the floor in wall clock to have a real play at all.
    #[cfg(test)]
    pub fn pretend_listened(&mut self) {
        {
            let mut core = self.lock_core();
            if let Some(active) = core.active.as_mut() {
                // Credit only; the wall clock keeps running, because some of
                // the callers are testing exactly when it stops.
                let row = &mut active.persisted.item.row;
                row.ms_played = row.ms_played.max(QUALIFYING_MS);
            }
        }
        self.persist();
    }

    /// A handle that pages this archive from another thread.
    pub fn reader(&self) -> HistoryReader {
        HistoryReader {
            load_error: self.load_error.clone(),
            core: Arc::clone(&self.core),
        }
    }

    pub fn clear(&mut self) -> Result<(), String> {
        self.ensure_writable()?;
        if self.root.as_os_str().is_empty() {
            return Ok(());
        }

        {
            let mut core = self.lock_core();
            core.finalized.clear();
            core.pending.clear();
            core.active = None;
            core.rewrite_requested = core.rewrite_requested.wrapping_add(1);
            core.invalidate();
        }
        self.persist();
        Ok(())
    }
}

impl HistoryReader {
    /// One filtered, ordered window of the archive.
    pub fn page(&self, request: &HistoryQuery) -> Result<HistoryPage, String> {
        if let Some(error) = &self.load_error {
            return Err(format!("listening history is read-only: {error}"));
        }

        let mut core = lock_history_core(&self.core);
        let active_qualified = core.active_qualified();
        let recorded = core.finalized.len() + usize::from(active_qualified);
        let needle = request.query.trim().to_lowercase();

        // Chronological order with no filter needs no projection at all: the
        // deque is already in that order, so a page is arithmetic.
        let (total, slots): (usize, Vec<Slot>) = if needle.is_empty()
            && matches!(request.sort, HistorySort::Recent | HistorySort::Oldest)
        {
            let end = request.offset.saturating_add(request.limit).min(recorded);
            let range = request.offset.min(end)..end;
            (
                recorded,
                range
                    .map(|index| {
                        chronological_slot(index, core.finalized.len(), active_qualified, request.sort)
                    })
                    .collect(),
            )
        } else {
            ensure_ordering(&mut core, &needle, request.sort, active_qualified);
            let cached = core.ordering.as_ref().expect("ordering built above");
            let total = cached.slots.len();
            let end = request.offset.saturating_add(request.limit).min(total);
            let range = request.offset.min(end)..end;
            (total, cached.slots[range].to_vec())
        };

        let live_active = core.live_active();
        let items = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Active => live_active.clone(),
                Slot::Finalized(index) => {
                    core.finalized.get(*index as usize).map(|row| row.item.clone())
                }
            })
            .collect::<Vec<_>>();
        let next = request.offset.saturating_add(items.len());
        Ok(HistoryPage {
            items,
            total,
            recorded,
            offset: request.offset,
            next_offset: (next < total).then_some(next),
        })
    }
}

impl ListeningHistory {
    fn writable(&self) -> bool {
        if let Some(error) = &self.load_error {
            eprintln!("listening history mutation rejected: {error}");
            false
        } else {
            true
        }
    }

    fn ensure_writable(&self) -> Result<(), String> {
        match &self.load_error {
            Some(error) => Err(format!("listening history is read-only: {error}")),
            None => Ok(()),
        }
    }

    fn lock_core(&self) -> MutexGuard<'_, HistoryCore> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues the newest state for persistence and keeps exactly one writer
    /// loop draining generations. Inside the engine runtime the write work
    /// runs on the blocking pool, so transport events never wait on disk; with
    /// no runtime (tests) it drains inline and stays synchronous and
    /// deterministic. `written_generation` advances on success only, so a
    /// failed write is retried by the next playback event.
    fn persist(&self) {
        if self.root.as_os_str().is_empty() || self.load_error.is_some() {
            return;
        }
        let start_worker = {
            let mut core = self.lock_core();
            core.requested_generation = core.requested_generation.wrapping_add(1).max(1);
            let start_worker = !core.worker_running;
            core.worker_running = true;
            start_worker
        };
        if start_worker {
            self.spawn_persistence_worker();
        }
    }

    fn spawn_persistence_worker(&self) {
        let core = Arc::clone(&self.core);
        let root = self.root.clone();
        match tokio::runtime::Handle::try_current() {
            // Off the async loop: serialization, fsync, and MoveFileExW belong
            // on the blocking pool, not inside the engine's select loop.
            Ok(handle) => {
                handle.spawn_blocking(move || drain_persistence(core, root));
            }
            Err(_) => drain_persistence(core, root),
        }
    }
}

/// The logical position `index` occupies in an unfiltered chronological
/// ordering. Newest-first puts the in-progress play at the head; oldest-first
/// puts it at the tail.
fn chronological_slot(
    index: usize,
    finalized_len: usize,
    active_qualified: bool,
    sort: HistorySort,
) -> Slot {
    match sort {
        HistorySort::Oldest => {
            if index < finalized_len {
                Slot::Finalized(index as u32)
            } else {
                Slot::Active
            }
        }
        _ => {
            if active_qualified {
                if index == 0 {
                    Slot::Active
                } else {
                    Slot::Finalized((finalized_len - index) as u32)
                }
            } else {
                Slot::Finalized((finalized_len - 1 - index) as u32)
            }
        }
    }
}

/// Builds the filtered, ordered projection for this query, reusing the cached
/// one when the archive has not moved.
fn ensure_ordering(
    core: &mut HistoryCore,
    needle: &str,
    sort: HistorySort,
    active_qualified: bool,
) {
    let fresh = core.ordering.as_ref().is_some_and(|cached| {
        cached.mutation == core.mutation
            && cached.sort == sort
            && cached.query == needle
            && cached.active_qualified == active_qualified
    });
    if fresh {
        return;
    }

    // Every row carries its folded keys (see [`SearchKeys`]), so a rebuild
    // allocates nothing per row: the filter and the comparator only read
    // strings folded when the row entered memory.
    let slots = {
        let core = &*core;
        let active = core
            .active
            .as_ref()
            .filter(|_| active_qualified)
            .map(|active| (Slot::Active, &active.keys, active.persisted.item.row.started_at));
        let rows = core.finalized.iter().enumerate().map(|(index, row)| {
            (Slot::Finalized(index as u32), &row.keys, row.item.row.started_at)
        });
        // The scan walks oldest-first with the in-progress row ahead of it.
        let matching = active
            .into_iter()
            .chain(rows)
            .filter(|(_, keys, _)| keys.matches(needle));
        match sort {
            HistorySort::Recent | HistorySort::Oldest => {
                let mut slots: Vec<Slot> = matching.map(|(slot, ..)| slot).collect();
                let active_first = slots.first() == Some(&Slot::Active);
                if sort == HistorySort::Recent {
                    // Newest-first is the walk read backwards, behind the
                    // in-progress row.
                    slots[usize::from(active_first)..].reverse();
                } else if active_first {
                    slots.rotate_left(1);
                }
                slots
            }
            HistorySort::Title | HistorySort::Artist => {
                let mut keyed: Vec<_> = matching.collect();
                keyed.sort_by(|(_, left, left_started), (_, right, right_started)| {
                    let primary = if sort == HistorySort::Artist {
                        left.artists.cmp(&right.artists)
                    } else {
                        std::cmp::Ordering::Equal
                    };
                    primary
                        .then_with(|| left.title.cmp(&right.title))
                        .then_with(|| right_started.cmp(left_started))
                });
                keyed.into_iter().map(|(slot, ..)| slot).collect()
            }
        }
    };

    core.ordering = Some(CachedOrdering {
        query: needle.to_owned(),
        sort,
        mutation: core.mutation,
        active_qualified,
        slots,
    });
}

/// Lock recovery: every history field update is one uninterruptible step
/// behind this mutex, so the state behind a poison is consistent and safe to
/// reuse.
fn lock_history_core(core: &Mutex<HistoryCore>) -> MutexGuard<'_, HistoryCore> {
    core.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What one pass of the writer has to put on disk, and what its success is
/// allowed to mark as done. Both bookkeeping numbers are captured at
/// serialization time so that anything queued while the write is in flight
/// stays queued.
struct WriteTask {
    journal: Option<JournalWrite>,
    /// How many queued rows the journal bytes account for.
    covered: usize,
    /// The trim generation a full rewrite puts on disk; `None` for an append.
    rewrite: Option<u64>,
    /// The sidecar's new contents, or `None` to remove it.
    active: Option<Vec<u8>>,
}

enum JournalWrite {
    Append(Vec<u8>),
    Rewrite(Vec<u8>),
}

/// Drains requested generations until the store is caught up, or until a
/// failed write parks the attempt counter so retry waits for the next playback
/// event instead of spinning.
///
/// Order matters for recovery: the journal is written before the sidecar, so a
/// crash between them leaves a row in both places rather than in neither. The
/// loader deduplicates that case; it could not invent a play the other order
/// would have lost.
fn drain_persistence(core: Arc<Mutex<HistoryCore>>, root: PathBuf) {
    let journal_path = root.join(JOURNAL_FILE);
    let active_path = root.join(ACTIVE_FILE);
    loop {
        let (target, task) = {
            let mut core = lock_history_core(&core);
            if core.requested_generation <= core.attempted_generation {
                // Caught up, or the last attempt failed and no newer event has
                // arrived: release the single worker slot either way.
                core.worker_running = false;
                return;
            }
            core.attempted_generation = core.requested_generation;
            (core.requested_generation, serialize_task(&core))
        };

        let Ok(task) = task.map_err(|error: String| {
            eprintln!("could not persist listening history: {error}");
        }) else {
            continue;
        };

        match apply_task(&journal_path, &active_path, &task) {
            Ok(()) => {
                let mut core = lock_history_core(&core);
                core.written_generation = target;
                for _ in 0..task.covered {
                    core.pending.pop_front();
                }
                if let Some(rewrite) = task.rewrite {
                    core.rewrite_written = rewrite;
                }
            }
            // The queued rows stay queued, so the next event retries them.
            Err(error) => eprintln!("could not persist listening history: {error}"),
        }
    }
}

fn serialize_task(core: &HistoryCore) -> Result<WriteTask, String> {
    let (journal, rewrite) = if core.rewrite_requested != core.rewrite_written {
        let mut bytes = Vec::new();
        for row in &core.finalized {
            write_line(&mut bytes, &row.item)?;
        }
        // The rewrite covers `finalized`, which already contains every queued
        // row, so the queue is cleared by it too.
        (
            Some(JournalWrite::Rewrite(bytes)),
            Some(core.rewrite_requested),
        )
    } else if core.pending.is_empty() {
        (None, None)
    } else {
        let mut bytes = Vec::new();
        for item in &core.pending {
            write_line(&mut bytes, item)?;
        }
        (Some(JournalWrite::Append(bytes)), None)
    };
    let covered = if journal.is_some() {
        core.pending.len()
    } else {
        0
    };

    let active = core
        .active
        .as_ref()
        .map(|active| {
            serde_json::to_vec(&active.persisted)
                .map_err(|error| format!("could not serialize the in-progress play: {error}"))
        })
        .transpose()?;
    Ok(WriteTask {
        journal,
        covered,
        rewrite,
        active,
    })
}

/// The tag every journal line carries, and the whole of how a journal proves
/// it is ours.
///
/// An abandoned prototype had already written its own rows to
/// [`JOURNAL_FILE`], and `HistoryItem` is deliberately lenient — every field
/// defaults — so those rows *parse*, arrive with an empty track, and fail
/// validation on line one. A file this code never wrote was therefore fatal:
/// the store went read-only. The tag turns "was this ever ours?" into a
/// question the file answers itself, rather than one inferred from whether
/// foreign bytes happen to fit our shape.
///
/// The alternative was to move the journal to a filename the prototype could
/// not have taken. Rejected: it dodges the one name that collided and leaves
/// the next collision just as fatal, and the file at the old name would still
/// have to be judged rather than deleted unread. Judging it means recognising
/// our own format, which is this code either way.
/// The cost is about thirty bytes a line, paid to make the archive
/// self-describing.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum JournalFormat {
    #[serde(rename = "listening-history/1")]
    V1,
}

/// A journal line on the way in.
#[derive(Deserialize)]
struct JournalLine {
    /// Never read in Rust: deserializing it *is* the check, and a line that
    /// carries no tag of ours cannot produce one of these.
    #[allow(dead_code)]
    format: JournalFormat,
    #[serde(flatten)]
    item: HistoryItem,
}

/// A journal line on the way out. Separate from [`JournalLine`] only so a row
/// can be tagged without being cloned.
#[derive(Serialize)]
struct TaggedRow<'a> {
    format: JournalFormat,
    #[serde(flatten)]
    item: &'a HistoryItem,
}

fn write_line(bytes: &mut Vec<u8>, item: &HistoryItem) -> Result<(), String> {
    let line = TaggedRow {
        format: JournalFormat::V1,
        item,
    };
    serde_json::to_writer(&mut *bytes, &line)
        .map_err(|error| format!("could not serialize a history row: {error}"))?;
    bytes.push(b'\n');
    Ok(())
}

fn apply_task(journal_path: &Path, active_path: &Path, task: &WriteTask) -> Result<(), String> {
    if let Some(parent) = journal_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    match &task.journal {
        Some(JournalWrite::Append(bytes)) => append_journal(journal_path, bytes)?,
        Some(JournalWrite::Rewrite(bytes)) => write_atomic(journal_path, bytes)?,
        None => {}
    }
    match &task.active {
        Some(bytes) => write_atomic(active_path, bytes),
        None => match fs::remove_file(active_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "could not remove {}: {error}",
                active_path.display()
            )),
        },
    }
}

/// The whole point of the journal: one open, one write, one fsync, whatever
/// the archive already holds.
fn append_journal(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// Create parent, temp file, fsync, atomic replace.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)
        .map_err(|error| format!("could not create {}: {error}", temporary.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    replace_file_atomically(&temporary, path, true)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

struct LoadedArchive {
    finalized: VecDeque<ArchivedRow>,
    active: Option<ActivePlay>,
    /// Whether what was loaded differs from what is on disk — a discarded
    /// foreign journal or an over-ceiling trim — and so must be written back
    /// whole.
    needs_rewrite: bool,
}

fn load_archive(root: &Path) -> Result<LoadedArchive, String> {
    let journal_path = root.join(JOURNAL_FILE);
    let (mut finalized, mut needs_rewrite) = match fs::read(&journal_path) {
        Ok(bytes) => match parse_journal(&journal_path, &bytes)? {
            Journal::Ours {
                finalized,
                torn_tail,
            } => (finalized, torn_tail),
            Journal::Foreign => {
                eprintln!(
                    "{} holds no row this engine wrote; ignoring it",
                    journal_path.display()
                );
                // Not one line of it is ours, so there is no record here to
                // protect: it is replaced whole rather than treated as damage.
                (VecDeque::new(), true)
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (VecDeque::new(), false),
        Err(error) => {
            return Err(format!(
                "could not read {}: {error}",
                journal_path.display()
            ))
        }
    };

    let active = read_active_sidecar(&root.join(ACTIVE_FILE), &finalized)?;

    if finalized.len() > MAX_HISTORY_ITEMS {
        while finalized.len() > MAX_HISTORY_ITEMS {
            finalized.pop_front();
        }
        needs_rewrite = true;
    }

    Ok(LoadedArchive {
        finalized: finalized.into_iter().map(ArchivedRow::new).collect(),
        active,
        needs_rewrite,
    })
}

/// The in-progress row left by the previous run, unless the journal already
/// holds the same play.
fn read_active_sidecar(
    path: &Path,
    finalized: &VecDeque<HistoryItem>,
) -> Result<Option<ActivePlay>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    let persisted: PersistedActive = serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    validate_item(&persisted.item)?;
    // The journal is written before the sidecar, so a crash between them
    // leaves the same play in both. Identity is the track plus the millisecond
    // it started, which no two plays share.
    let duplicated = finalized.back().is_some_and(|last| {
        last.row.track_id == persisted.item.row.track_id
            && last.row.started_at == persisted.item.row.started_at
    });
    Ok((!duplicated).then(|| ActivePlay::new(persisted, None)))
}

/// What the file at [`JOURNAL_FILE`] turned out to be.
enum Journal {
    /// Written by this engine: its rows, and whether the final line was torn.
    Ours {
        finalized: VecDeque<HistoryItem>,
        torn_tail: bool,
    },
    /// Not one line of it came from us — an abandoned prototype's file at the
    /// same path, say. It holds no archive of ours to lose or to protect.
    Foreign,
}

/// Parses the journal, tolerating exactly one kind of damage: a torn final
/// line, which is what a crash mid-append looks like. Anything earlier is real
/// corruption and must not be silently dropped — the store goes read-only
/// instead, so playback cannot overwrite a record it failed to understand.
///
/// That protection only makes sense for a file that is ours, which is what
/// [`JournalFormat`] settles first. A file none of whose lines we wrote is not
/// damage and must not be fatal.
fn parse_journal(path: &Path, bytes: &[u8]) -> Result<Journal, String> {
    // Split on bytes rather than decoding the file whole: a stranger's file
    // need not even be UTF-8, and refusing to start over its encoding would be
    // the same failure in a different coat.
    let lines: Vec<&[u8]> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .collect();
    if lines.is_empty() {
        return Ok(Journal::Ours {
            finalized: VecDeque::new(),
            torn_tail: false,
        });
    }
    // Ownership is decided by the whole file, not by its first line. Damage
    // lands on one line, and a real archive whose first line rotted must still
    // be recognised as ours and protected — not mistaken for a stranger's and
    // thrown away, which is exactly the blanket "discard what will not parse"
    // this avoids.
    if !lines.iter().any(|line| line_is_ours(line)) {
        return Ok(Journal::Foreign);
    }

    let mut finalized = VecDeque::with_capacity(lines.len());
    let mut torn_tail = false;
    for (index, line) in lines.iter().enumerate() {
        match parse_line(line) {
            Ok(item) => {
                validate_item(&item)?;
                finalized.push_back(item);
            }
            Err(error) => {
                if index + 1 == lines.len() {
                    torn_tail = true;
                } else {
                    return Err(format!(
                        "could not parse line {} of {}: {error}",
                        index + 1,
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(Journal::Ours {
        finalized,
        torn_tail,
    })
}

/// Whether one line is evidence that this journal is ours.
fn line_is_ours(line: &[u8]) -> bool {
    if serde_json::from_slice::<JournalLine>(line).is_ok() {
        return true;
    }
    // A tag can only mark lines written from the day it shipped, so a journal
    // an earlier build of this engine wrote carries none. Those rows are still
    // recognisably ours: a row carries the track it is a row *of*, and the two
    // ids agree — which is precisely what the prototype's rows never had.
    serde_json::from_slice::<HistoryItem>(line).is_ok_and(|item| validate_item(&item).is_ok())
}

fn parse_line(line: &[u8]) -> Result<HistoryItem, serde_json::Error> {
    match serde_json::from_slice::<JournalLine>(line) {
        Ok(line) => Ok(line.item),
        // Untagged, from an earlier build of ours. `validate_item` in the
        // caller is what still rejects a row no build of ours wrote.
        Err(_) => serde_json::from_slice::<HistoryItem>(line),
    }
}

fn validate_item(item: &HistoryItem) -> Result<(), String> {
    if item.row.track_id.is_empty() {
        return Err("invalid listening history: empty track id".to_owned());
    }
    let track = &item.track;
    if track.id != item.row.track_id {
        return Err("invalid listening history: row and track ids differ".to_owned());
    }
    if track.play_count.is_some()
        || track.added_at.is_some()
        || track.unavailable
        || track.unavailable_reason.is_some()
        || track.cached
        || !track.context.is_empty()
        || track.effective_edit.is_some()
    {
        return Err("invalid listening history: track contains volatile playback data".to_owned());
    }
    Ok(())
}

/// Retains identity and display metadata only. History replay must resolve its
/// current context, edit, availability, and cache state instead of reviving a
/// stale browse/queue snapshot.
fn sanitize_track(track: &TrackRef) -> TrackRef {
    let mut track = track.clone();
    track.play_count = None;
    track.added_at = None;
    track.unavailable = false;
    track.unavailable_reason = None;
    track.cached = false;
    track.context.clear();
    track.effective_edit = None;
    track
}

fn compact_context(context: &str) -> String {
    let context = context.trim();
    if context.len() <= 128 {
        return context.to_owned();
    }
    context.chars().take(128).collect()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn elapsed_ms(since: Option<Instant>) -> u64 {
    since
        .and_then(|since| u64::try_from(since.elapsed().as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer_engine::protocol::{TimeRange, TrackEdit};
    use std::time::Duration;

    fn scratch() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "renderer-history-test-{}-{}-{ordinal}",
            std::process::id(),
            now_millis()
        ))
    }

    fn track(id: &str) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            name: format!("Track {id}"),
            artist_names: vec!["Artist".to_owned()],
            duration_ms: 180_000,
            play_count: Some(42),
            added_at: Some(123),
            unavailable: true,
            unavailable_reason: Some("country".to_owned()),
            cached: true,
            context: " playlist:source ".to_owned(),
            effective_edit: Some(TrackEdit {
                cuts: vec![TimeRange {
                    start_ms: 1,
                    end_ms: 2,
                }],
                loop_range: None,
            }),
            ..TrackRef::default()
        }
    }

    /// Everything a caller wants, in one request. Only tests may ask for this:
    /// the view pages.
    fn everything(history: &ListeningHistory) -> Vec<HistoryItem> {
        history
            .reader().page(&HistoryQuery {
                limit: usize::MAX,
                ..HistoryQuery::default()
            })
            .unwrap()
            .items
    }

    /// Pushes a play that has been listened to for long enough to be kept,
    /// without waiting for real time to pass.
    fn record(history: &mut ListeningHistory, id: &str) {
        history.start(&track(id));
        {
            let mut core = history.lock_core();
            let active = core.active.as_mut().expect("start installs an active play");
            active.playing_since = None;
            active.persisted.item.row.ms_played = QUALIFYING_MS;
        }
        history.finalize(false);
    }

    #[test]
    fn archive_and_in_progress_row_survive_a_restart_together() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        history.start(&track("finalized"));
        assert!(history.finalize(true));
        history.start(&track("active"));
        history.pause();

        let recovered = ListeningHistory::new(root.clone());
        // The in-progress play has not been listened to yet, so it is not part
        // of the archive — but it is still recoverable and still running.
        let items = everything(&recovered);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].row.track_id, "finalized");
        assert!(items[0].row.completed);
        assert!(recovered.lock_core().active.is_some());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resume_rewrites_the_in_progress_row_for_crash_recovery() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        history.start(&track("resumed"));
        history.pause();

        let mut resumed = ListeningHistory::new(root.clone());
        fs::remove_file(root.join(ACTIVE_FILE)).unwrap();
        resumed.resume();

        let recovered = ListeningHistory::new(root.clone());
        let active = recovered.lock_core().active.is_some();
        assert!(active, "the in-progress row must be back on disk");

        let _ = fs::remove_dir_all(root);
    }

    /* =====================================================================
       WHAT COUNTS AS A PLAY
       ===================================================================== */

    #[test]
    fn a_play_qualifies_at_five_seconds_or_half_of_a_very_short_track() {
        // Full-length track: the floor applies, to the millisecond.
        assert!(!play_qualifies(4_999, false, 180_000));
        assert!(play_qualifies(5_000, false, 180_000));
        // The complaint that lowered the floor: ten seconds is nobody's skip,
        // and the thirty-second rule threw it away.
        assert!(play_qualifies(10_000, false, 180_000));
        // Interludes and short songs now answer to the floor like anything
        // else; the halving no longer reaches them.
        assert!(!play_qualifies(4_999, false, 45_000));
        assert!(play_qualifies(5_000, false, 45_000));
        assert!(!play_qualifies(4_999, false, 20_000));
        // Where it still binds: a nine-second skit stays recordable without
        // having to be played through.
        assert!(!play_qualifies(4_499, false, 9_000));
        assert!(play_qualifies(4_500, false, 9_000));
        // Reaching the end always counts, however short the track.
        assert!(play_qualifies(1, true, 9_000));
        // An unknown duration answers to the floor alone.
        assert!(!play_qualifies(4_999, false, 0));
        assert!(play_qualifies(5_000, false, 0));
    }

    #[test]
    fn skipping_away_before_the_threshold_writes_nothing() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        // Three tracks touched and left: exactly the Next-spam the archive
        // used to fill up with.
        history.start(&track("spam-1"));
        history.start(&track("spam-2"));
        history.start(&track("spam-3"));
        history.finalize(false);
        assert!(everything(&history).is_empty());

        // Ten seconds of a full-length track is the play the old floor lost:
        // too long to be a press of Next, too short to survive thirty seconds.
        history.start(&track("heard-briefly"));
        {
            let mut core = history.lock_core();
            let active = core.active.as_mut().expect("start installs an active play");
            active.playing_since = None;
            active.persisted.item.row.ms_played = 10_000;
        }
        history.finalize(false);

        record(&mut history, "listened");
        let items = everything(&history);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].row.track_id, "listened");
        assert_eq!(items[1].row.track_id, "heard-briefly");

        // And nothing trivial reached disk either, so a restart cannot revive
        // one of the skipped plays.
        let recovered = ListeningHistory::new(root.clone());
        assert_eq!(everything(&recovered).len(), 2);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_in_progress_play_joins_the_archive_only_once_it_qualifies() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        history.start(&track("listening"));
        assert_eq!(history.reader().page(&HistoryQuery::default()).unwrap().recorded, 0);

        {
            let mut core = history.lock_core();
            let active = core.active.as_mut().unwrap();
            active.playing_since = None;
            active.persisted.item.row.ms_played = QUALIFYING_MS;
        }
        let page = history.reader().page(&HistoryQuery::default()).unwrap();
        assert_eq!(page.recorded, 1);
        assert_eq!(page.items[0].row.track_id, "listening");

        // Finalizing keeps it in the same place rather than moving it.
        history.finalize(false);
        let page = history.reader().page(&HistoryQuery::default()).unwrap();
        assert_eq!(page.recorded, 1);
        assert_eq!(page.items[0].row.track_id, "listening");

        let _ = fs::remove_dir_all(root);
    }

    /* =====================================================================
       PAGING
       ===================================================================== */

    #[test]
    fn pages_walk_the_archive_newest_first_without_ever_returning_it_whole() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        for index in 0..250 {
            record(&mut history, &format!("track-{index:03}"));
        }

        let page = |offset: usize| {
            history
                .reader().page(&HistoryQuery {
                    offset,
                    limit: 50,
                    ..HistoryQuery::default()
                })
                .unwrap()
        };

        let first = page(0);
        assert_eq!(first.items.len(), 50);
        assert_eq!(first.total, 250);
        assert_eq!(first.recorded, 250);
        assert_eq!(first.items[0].row.track_id, "track-249");
        assert_eq!(first.next_offset, Some(50));

        let middle = page(100);
        assert_eq!(middle.items.len(), 50);
        assert_eq!(middle.items[0].row.track_id, "track-149");
        assert_eq!(middle.next_offset, Some(150));

        let last = page(200);
        assert_eq!(last.items.len(), 50);
        assert_eq!(last.items.last().unwrap().row.track_id, "track-000");
        assert_eq!(last.next_offset, None, "the last page ends the walk");

        let past_the_end = page(250);
        assert!(past_the_end.items.is_empty());
        assert_eq!(past_the_end.total, 250);
        assert_eq!(past_the_end.next_offset, None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oldest_first_pages_from_the_other_end() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        for index in 0..30 {
            record(&mut history, &format!("track-{index:02}"));
        }
        let page = history
            .reader().page(&HistoryQuery {
                offset: 0,
                limit: 3,
                sort: HistorySort::Oldest,
                ..HistoryQuery::default()
            })
            .unwrap();
        assert_eq!(page.items[0].row.track_id, "track-00");
        assert_eq!(page.items[2].row.track_id, "track-02");
        assert_eq!(page.next_offset, Some(3));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_filter_and_the_name_orders_are_answered_by_the_engine() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        for id in ["beta", "alpha", "gamma", "alpaca"] {
            record(&mut history, id);
        }

        let filtered = history
            .reader().page(&HistoryQuery {
                query: "  ALP  ".trim().to_owned(),
                ..HistoryQuery::default()
            })
            .unwrap();
        assert_eq!(filtered.total, 2, "the filter narrows the paged list");
        assert_eq!(filtered.recorded, 4, "the archive size is reported whole");

        let by_title = history
            .reader().page(&HistoryQuery {
                sort: HistorySort::Title,
                ..HistoryQuery::default()
            })
            .unwrap();
        let names: Vec<&str> = by_title
            .items
            .iter()
            .map(|item| item.row.track_id.as_str())
            .collect();
        assert_eq!(names, ["alpaca", "alpha", "beta", "gamma"]);

        let _ = fs::remove_dir_all(root);
    }

    /// The name orders fold case-insensitively and break ties on the title,
    /// then on the newest play — the artist order had no test at all, and the
    /// folded keys the comparator now compares are only correct if all three
    /// still come out the same.
    #[test]
    fn the_artist_order_folds_names_and_keeps_both_tie_breaks() {
        let root = scratch();
        let history = ListeningHistory::new(root.clone());
        {
            let mut core = history.lock_core();
            for (id, artist, title, started_at) in [
                ("same-old", "Beta Band", "Same", 1i64),
                ("same-new", "beta band", "SAME", 2),
                ("zzz", "Alpha", "zzz", 3),
                ("aaa", "Beta Band", "aaa", 4),
            ] {
                let mut track = track(id);
                track.name = title.to_owned();
                track.artist_names = vec![artist.to_owned()];
                core.finalized.push_back(ArchivedRow::new(HistoryItem {
                    row: HistoryRow {
                        track_id: id.to_owned(),
                        started_at,
                        ..HistoryRow::default()
                    },
                    track: sanitize_track(&track),
                }));
            }
        }

        let ids = |sort: HistorySort| -> Vec<String> {
            history
                .reader().page(&HistoryQuery {
                    limit: usize::MAX,
                    sort,
                    ..HistoryQuery::default()
                })
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.row.track_id)
                .collect()
        };
        assert_eq!(
            ids(HistorySort::Artist),
            ["zzz", "aaa", "same-new", "same-old"],
            "artist first, then title, then the newest tie"
        );

        let _ = fs::remove_dir_all(root);
    }

    /// The filter reads keys folded when the row entered memory, one artist at
    /// a time, so a query never matches across two names.
    #[test]
    fn the_filter_matches_each_artist_from_folded_keys() {
        let history = ListeningHistory::new(PathBuf::new());
        {
            let mut core = history.lock_core();
            let mut duet = track("duet");
            duet.name = "Duet".to_owned();
            duet.artist_names = vec!["Alpha".to_owned(), "Beta Band".to_owned()];
            core.finalized.push_back(ArchivedRow::new(HistoryItem {
                row: HistoryRow {
                    track_id: "duet".to_owned(),
                    started_at: 1,
                    ..HistoryRow::default()
                },
                track: sanitize_track(&duet),
            }));
        }
        let reader = history.reader();
        let total = |query: &str| {
            reader
                .page(&HistoryQuery {
                    query: query.to_owned(),
                    ..HistoryQuery::default()
                })
                .unwrap()
                .total
        };
        assert_eq!(total("DUET"), 1);
        assert_eq!(total("band"), 1);
        assert_eq!(total("alpha"), 1);
        assert_eq!(total("alphabeta"), 0);
        assert_eq!(total("alpha, beta"), 0, "a match never spans two artists");
    }

    /* =====================================================================
       STORAGE
       ===================================================================== */

    #[test]
    fn plays_are_appended_rather_than_rewritten() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        record(&mut history, "one");
        let after_one = fs::read(root.join(JOURNAL_FILE)).unwrap();
        record(&mut history, "two");
        let after_two = fs::read(root.join(JOURNAL_FILE)).unwrap();

        assert!(
            after_two.starts_with(&after_one),
            "the second play must extend the journal, not replace it"
        );
        assert_eq!(after_two.iter().filter(|byte| **byte == b'\n').count(), 2);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_archive_is_trimmed_in_batches_once_it_runs_past_the_ceiling() {
        let root = scratch();
        let history = ListeningHistory::new(root.clone());
        {
            let mut core = history.lock_core();
            for index in 0..MAX_HISTORY_ITEMS + COMPACT_SLACK {
                let track = track(&format!("track-{index}"));
                core.finalized.push_back(ArchivedRow::new(HistoryItem {
                    row: HistoryRow {
                        track_id: track.id.clone(),
                        started_at: index as i64,
                        ..HistoryRow::default()
                    },
                    track: sanitize_track(&track),
                }));
            }
            // At the ceiling plus the slack, nothing has been dropped yet.
            core.enforce_bound();
            assert_eq!(core.finalized.len(), MAX_HISTORY_ITEMS + COMPACT_SLACK);
            assert_eq!(core.rewrite_requested, core.rewrite_written);

            core.finalized.push_back(ArchivedRow::new(HistoryItem {
                row: HistoryRow {
                    track_id: "overflow".to_owned(),
                    ..HistoryRow::default()
                },
                track: sanitize_track(&track("overflow")),
            }));
            core.enforce_bound();
        }
        let core = history.lock_core();
        assert_eq!(core.finalized.len(), MAX_HISTORY_ITEMS);
        assert_eq!(core.finalized.front().unwrap().item.row.track_id, "track-1001");
        assert_ne!(
            core.rewrite_requested, core.rewrite_written,
            "a trim can only reach disk as a rewrite"
        );
        drop(core);

        let _ = fs::remove_dir_all(root);
    }


    #[test]
    fn a_torn_final_line_is_dropped_and_earlier_damage_is_never_overwritten() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        record(&mut history, "kept");
        record(&mut history, "also-kept");

        // A crash mid-append: the last line never finished.
        let path = root.join(JOURNAL_FILE);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(br#"{"track_id":"torn","start"#);
        fs::write(&path, &bytes).unwrap();

        let recovered = ListeningHistory::new(root.clone());
        assert_eq!(everything(&recovered).len(), 2);

        // Damage anywhere else is corruption, and playback must not paper
        // over it with a fresh archive.
        let mut lines: Vec<String> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.insert(0, "{not json".to_owned());
        fs::write(&path, lines.join("\n")).unwrap();

        let mut broken = ListeningHistory::new(root.clone());
        assert!(broken.reader().page(&HistoryQuery::default()).unwrap_err().contains("read-only"));
        broken.start(&track("new"));
        assert!(!broken.finalize(true));
        assert!(broken.clear().unwrap_err().contains("read-only"));
        assert!(fs::read_to_string(&path).unwrap().starts_with("{not json"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_row_appended_before_the_crash_is_not_duplicated_by_its_sidecar() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        record(&mut history, "played");

        // The journal took the row; the sidecar rewrite never landed, so it
        // still describes the play that is now in the archive.
        let last = everything(&history)[0].clone();
        fs::write(
            root.join(ACTIVE_FILE),
            serde_json::to_vec(&PersistedActive {
                item: last,
                duration_ms: 180_000,
            })
            .unwrap(),
        )
        .unwrap();

        let recovered = ListeningHistory::new(root.clone());
        assert_eq!(everything(&recovered).len(), 1);
        assert!(recovered.lock_core().active.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persisted_track_metadata_is_safe_to_replay() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        record(&mut history, "sanitized");

        let recovered = ListeningHistory::new(root.clone());
        let items = everything(&recovered);
        let item = &items[0];
        assert_eq!(item.row.context, "playlist:source");
        let track = &item.track;
        assert_eq!(track.name, "Track sanitized");
        assert!(track.play_count.is_none());
        assert!(track.added_at.is_none());
        assert!(!track.unavailable);
        assert!(track.unavailable_reason.is_none());
        assert!(!track.cached);
        assert!(track.context.is_empty());
        assert!(track.effective_edit.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clearing_empties_both_files() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        record(&mut history, "one");
        history.start(&track("two"));
        history.clear().unwrap();

        let recovered = ListeningHistory::new(root.clone());
        assert!(everything(&recovered).is_empty());
        assert!(recovered.lock_core().active.is_none());
        assert!(fs::read(root.join(JOURNAL_FILE)).unwrap().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    /// The single-flight writer must coalesce superseded sidecar states while
    /// never coalescing an appended row away.
    #[tokio::test]
    async fn the_background_writer_keeps_every_appended_row() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        record(&mut history, "first");
        record(&mut history, "second");
        history.start(&track("third"));
        history.pause();

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let caught_up = {
                    let core = history.lock_core();
                    !core.worker_running
                        && core.written_generation == core.requested_generation
                        && core.pending.is_empty()
                };
                if caught_up {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the single-flight writer must drain every queued generation");

        let recovered = ListeningHistory::new(root.clone());
        let items = everything(&recovered);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].row.track_id, "second");
        assert_eq!(items[1].row.track_id, "first");
        assert_eq!(
            recovered
                .lock_core()
                .active
                .as_ref()
                .map(|active| active.persisted.item.row.track_id.clone()),
            Some("third".to_owned())
        );

        let _ = fs::remove_dir_all(root);
    }

    /* =====================================================================
       FILES THAT WERE NEVER OURS
       ===================================================================== */

    /// Lines copied verbatim out of the file an abandoned prototype left at
    /// the journal's path: our field names, no `track` at all.
    const REAL_PROTOTYPE_ROWS: [&str; 3] = [
        r#"{"track_id":"3TxKtkCNR1yQARsvHxvNnP","started_at":1787416619638,"ms_played":189103,"completed":false,"context":"playlist:7jL0XCEu7RG0DXlr7JpgRI"}"#,
        r#"{"track_id":"4N8svBAsJvG0LW15icoiuW","started_at":1787416808832,"ms_played":19456,"completed":false,"context":"playlist:5evgTEnTDxEnEjr4eoGess"}"#,
        r#"{"track_id":"0BAQjCC9gfqOebxpmonHKz","started_at":1787417090001,"ms_played":116849,"completed":false,"context":"playlist:5evgTEnTDxEnEjr4eoGess"}"#,
    ];

    fn write_prototype_journal(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join(JOURNAL_FILE),
            format!("{}\n", REAL_PROTOTYPE_ROWS.join("\n")),
        )
        .unwrap();
    }

    #[test]
    fn a_journal_that_was_never_ours_is_replaced_rather_than_fatal() {
        let root = scratch();
        write_prototype_journal(&root);

        let mut history = ListeningHistory::new(root.clone());
        assert!(
            everything(&history).is_empty(),
            "a stranger's rows are not an archive of ours"
        );
        record(&mut history, "the-first-real-play");

        let reopened = ListeningHistory::new(root.clone());
        let items = everything(&reopened);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].row.track_id, "the-first-real-play");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_untagged_journal_from_an_earlier_build_is_ours_and_stays_protected() {
        let root = scratch();
        fs::create_dir_all(&root).unwrap();
        let mut bytes = Vec::new();
        for id in ["untagged-one", "untagged-two"] {
            let item = HistoryItem {
                row: HistoryRow {
                    track_id: id.to_owned(),
                    started_at: 1,
                    ms_played: QUALIFYING_MS,
                    ..HistoryRow::default()
                },
                track: sanitize_track(&track(id)),
            };
            bytes.extend_from_slice(&serde_json::to_vec(&item).unwrap());
            bytes.push(b'\n');
        }
        fs::write(root.join(JOURNAL_FILE), &bytes).unwrap();

        let mut history = ListeningHistory::new(root.clone());
        assert_eq!(
            everything(&history).len(),
            2,
            "rows written before the tag existed are still ours"
        );
        record(&mut history, "tagged");
        assert_eq!(everything(&ListeningHistory::new(root.clone())).len(), 3);

        // And the protection holds: a row no build of ours wrote, sitting
        // mid-file, is damage to our archive rather than a foreign file.
        let mut lines: Vec<String> = fs::read_to_string(root.join(JOURNAL_FILE))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.insert(1, REAL_PROTOTYPE_ROWS[0].to_owned());
        fs::write(root.join(JOURNAL_FILE), lines.join("\n")).unwrap();

        let damaged = ListeningHistory::new(root.clone());
        assert!(damaged
            .reader().page(&HistoryQuery::default())
            .unwrap_err()
            .contains("read-only"));
        assert!(fs::read_to_string(root.join(JOURNAL_FILE))
            .unwrap()
            .contains("3TxKtkCNR1yQARsvHxvNnP"));

        let _ = fs::remove_dir_all(root);
    }
}
