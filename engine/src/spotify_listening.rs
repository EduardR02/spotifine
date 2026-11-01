//! Spotify reporting is fed only by post-processing native consumption, never
//! by the local archive, decoder position, queued PCM, or a wall-clock timer.
use std::collections::VecDeque;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}};
use std::time::{Duration, Instant, SystemTime};

use librespot_core::{FileId, Session, SpotifyUri};
use librespot_metadata::audio::AudioFileFormat;
use renderer_engine::spotify_reporting::{EndReason, PlaybackReport, SpotifyReporter};
use tokio::sync::{mpsc, watch};

const QUEUE_CAPACITY: usize = 32;

/// One selected-file listening segment. Packets retain this identity across
/// seeks, loops, pause and processing revisions; replacement loads get a fresh
/// identity before their selected-file event can race ahead with decoded PCM.
pub struct Consumption {
    valid: AtomicBool,
    pub pending_packets: AtomicUsize,
    played_ns: AtomicU64,
    nominal_ns: AtomicU64,
    first_ns: AtomicU64,
    last_ns: AtomicU64,
    origin: Instant,
    wall_origin: SystemTime,
}

impl Consumption {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            valid: AtomicBool::new(true), pending_packets: AtomicUsize::new(0),
            played_ns: AtomicU64::new(0), nominal_ns: AtomicU64::new(0),
            first_ns: AtomicU64::new(0), last_ns: AtomicU64::new(0),
            origin: Instant::now(), wall_origin: SystemTime::now(),
        })
    }

    /// Called once per consumed packet/callback span, not once per sample.
    /// No allocations, channel operations, locks, or network work.
    pub fn record(&self, frames: usize, rate: u32, speed: f32) {
        if frames == 0 || !self.valid.load(Ordering::Acquire) { return; }
        let duration = frames as f64 / f64::from(rate);
        let played = (duration * 1_000_000_000.0).round() as u64;
        let nominal = (duration * f64::from(speed) * 1_000_000_000.0).round() as u64;
        let now = self.origin.elapsed().as_nanos().min(u128::from(u64::MAX - 1)) as u64 + 1;
        self.played_ns.fetch_add(played, Ordering::Relaxed);
        self.nominal_ns.fetch_add(nominal, Ordering::Relaxed);
        let first = now.saturating_sub(played).max(1);
        let _ = self.first_ns.compare_exchange(0, first, Ordering::Relaxed, Ordering::Relaxed);
        self.last_ns.store(now, Ordering::Release);
    }

    pub(crate) fn cancel(&self) { self.valid.store(false, Ordering::Release); }

    #[cfg(test)]
    pub(crate) fn durations(&self) -> (Duration, Duration) {
        (Duration::from_nanos(self.played_ns.load(Ordering::Acquire)),
            Duration::from_nanos(self.nominal_ns.load(Ordering::Acquire)))
    }
}

/// Player generations and decoder loads are independent of queue URI identity.
/// A delayed file-selection event may resolve its own retired load, never the
/// current occurrence of the same URI.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoadId {
    pub generation: u64,
    pub request: u64,
}
#[derive(Clone)]
struct Selection {
    requested: String,
    load: LoadId,
    uri: SpotifyUri,
    file_id: FileId,
    format: AudioFileFormat,
}

struct Segment {
    requested: String,
    load: Option<LoadId>,
    consumption: Arc<Consumption>,
    selection: Option<Selection>,
    context_uri: Option<String>,
    reason_start: &'static str,
    shuffle: bool,
    repeat_track: bool,
    repeat_context: bool,
}

impl Segment {
    fn report(&self, reason: EndReason) -> Option<PlaybackReport> {
        if !self.consumption.valid.load(Ordering::Acquire) { return None; }
        let selection = self.selection.as_ref()?;
        let played = Duration::from_nanos(self.consumption.played_ns.load(Ordering::Acquire));
        if played.is_zero() { return None; }
        let first = self.consumption.first_ns.load(Ordering::Acquire).saturating_sub(1);
        let last = self.consumption.last_ns.load(Ordering::Acquire).saturating_sub(1);
        Some(PlaybackReport {
            uri: selection.uri.clone(), file_id: selection.file_id, audio_format: selection.format,
            context_uri: self.context_uri.clone(),
            started_at: self.consumption.wall_origin + Duration::from_nanos(first),
            ended_at: self.consumption.wall_origin + Duration::from_nanos(last),
            played, nominal_played: Duration::from_nanos(self.consumption.nominal_ns.load(Ordering::Acquire)),
            reason_start: self.reason_start, reason_end: reason,
            shuffle: self.shuffle, repeat_track: self.repeat_track, repeat_context: self.repeat_context,
        })
    }
}

struct Delivery {
    sender: mpsc::Sender<PlaybackReport>,
    session_updates: watch::Sender<Session>,
    task: tokio::task::JoinHandle<()>,
}

impl Delivery {
    fn new(session: Session) -> Self {
        let (sender, mut receiver) = mpsc::channel::<PlaybackReport>(QUEUE_CAPACITY);
        let (session_updates, mut sessions) = watch::channel(session.clone());
        let task = tokio::spawn(async move {
            let mut reporter = SpotifyReporter::new(session);
            while let Some(report) = receiver.recv().await {
                if let Err(error) = reporter.report(&report, &mut sessions).await {
                    eprintln!("Spotify listening report failed: {error}");
                }
            }
        });
        Self { sender, session_updates, task }
    }
}

pub struct SpotifyListening {
    enabled: bool,
    session: Option<Session>,
    account: Option<String>,
    delivery: Option<Delivery>,
    active: Option<Segment>,
    retired: VecDeque<(Segment, EndReason)>,
    load: Option<LoadId>,
    last_selection: Option<Selection>,
    next_start: &'static str,
    next_end: Option<EndReason>,
}

impl SpotifyListening {
    pub fn new() -> Self {
        Self { enabled: true, session: None, account: None, delivery: None, active: None,
            retired: VecDeque::new(), load: None, last_selection: None, next_start: "playbtn", next_end: None }
    }

    fn cancel_pending(&mut self) {
        if let Some(active) = self.active.take() { active.consumption.cancel(); }
        for (segment, _) in self.retired.drain(..) { segment.consumption.cancel(); }
        if let Some(delivery) = self.delivery.take() { delivery.task.abort(); }
    }

    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        if self.enabled == enabled { return false; }
        self.enabled = enabled;
        self.cancel_pending();
        // Keep only the incumbent decoder's file identity across opt-out so
        // opting back in can identify fresh PCM without reloading playback.
        // Every consumption receipt and queued/in-flight report is cancelled.
        if enabled {
            if let Some(session) = self.session.clone() { self.delivery = Some(Delivery::new(session)); }
        }
        true
    }

    pub fn set_session(&mut self, session: Session) {
        let account = session.username();
        if self.account.as_ref() != Some(&account) || account.is_empty() {
            self.cancel_pending();
            self.last_selection = None;
            self.load = None;
        }
        if account.is_empty() {
            self.session = None;
            self.account = None;
            self.last_selection = None;
            return;
        }
        self.account = Some(account);
        self.session = Some(session.clone());
        if self.enabled {
            if let Some(delivery) = &self.delivery {
                // Preserve queued/in-flight event identities while the reporter
                // refreshes same-account authentication, including during retries.
                delivery.session_updates.send_replace(session);
            } else {
                self.delivery = Some(Delivery::new(session));
            }
        }
    }

    pub fn logout(&mut self) {
        self.cancel_pending();
        self.session = None;
        self.account = None;
        self.last_selection = None;
        self.load = None;
        self.next_start = "playbtn";
        self.next_end = None;
    }

    pub fn navigation(&mut self, back: bool) {
        self.next_start = if back { "backbtn" } else { "fwdbtn" };
        self.next_end = Some(if back { EndReason::BackButton } else { EndReason::ForwardButton });
    }

    pub fn queue_ended(&mut self) {
        self.next_start = "playbtn";
        self.next_end = None;
    }

    /// A replacement decoder may select a different file even for the same
    /// requested URI. Give it a fresh counter before it can produce PCM; its
    /// SelectedAudio event will identify that counter, independent of how far
    /// the player thread gets ahead of the engine event loop.
    pub fn prepare_load(&mut self) {
        let continuing_reason = self.active.as_ref().map(|active| active.reason_start);
        self.finish(false);
        if let Some(reason) = continuing_reason { self.next_start = reason; }
        self.last_selection = None;
        self.load = None;
    }

    pub(crate) fn loaded(&mut self, load: LoadId) {
        self.load = Some(load);
        if let Some(active) = &mut self.active { active.load = Some(load); }
    }

    pub fn begin(&mut self, requested: &str, context: &str, shuffle: bool, repeat_track: bool, repeat_context: bool) {
        if !self.enabled || self.session.is_none() { return; }
        if self.active.as_ref().is_some_and(|active| active.requested == requested) { return; }
        self.finish(false);
        let context_uri = match context.split_once(':') {
            Some(("spotify", _)) => Some(context.to_owned()),
            Some(("playlist" | "album" | "artist" | "show", _)) => Some(format!("spotify:{context}")),
            _ => None,
        };
        self.active = Some(Segment {
            requested: requested.to_owned(), consumption: Consumption::new(),
            load: self.load,
            selection: self.last_selection.as_ref()
                .filter(|selection| selection.requested == requested && Some(selection.load) == self.load).cloned(),
            context_uri, reason_start: self.next_start, shuffle, repeat_track, repeat_context,
        });
        self.next_start = "playbtn";
    }

    pub fn counter(&self) -> Option<Arc<Consumption>> {
        self.active.as_ref().map(|active| Arc::clone(&active.consumption))
    }

    pub(crate) fn selected(&mut self, load: LoadId, requested: &str, uri: SpotifyUri, file_id: FileId, format: AudioFileFormat) {
        let selection = Selection { load, requested: requested.to_owned(), uri, file_id, format };
        for segment in self.active.iter_mut().chain(self.retired.iter_mut().map(|(segment, _)| segment)) {
            if segment.load == Some(load) && segment.requested == requested && segment.selection.is_none() {
                segment.selection = Some(selection.clone());
            }
        }
        if self.load == Some(load) && self.last_selection.is_none() {
            self.last_selection = Some(selection);
        }
    }

    pub fn finish(&mut self, completed: bool) {
        if let Some(segment) = self.active.take() {
            let reason = self.next_end.take().unwrap_or(if completed { EndReason::TrackDone } else { EndReason::EndPlay });
            if self.retired.len() == QUEUE_CAPACITY {
                if let Some((old, _)) = self.retired.pop_front() { old.consumption.cancel(); }
                eprintln!("Spotify listening report queue full; dropping oldest pending segment");
            }
            self.retired.push_back((segment, reason));
        }
        if completed { self.next_start = "trackdone"; }
    }

    pub fn collect(&mut self) {
        if self.delivery.is_none() { return; }
        let mut index = 0;
        while index < self.retired.len() {
            let segment = &self.retired[index].0;
            // Selection is sent before PCM, but engine commands can retire the
            // load before that event is handled. Keep measured unresolved
            // loads until their identity arrives; finish() bounds this queue.
            if segment.consumption.pending_packets.load(Ordering::Acquire) != 0
                || segment.selection.is_none() && segment.load.is_some()
                    && segment.consumption.played_ns.load(Ordering::Acquire) != 0
            {
                index += 1;
                continue;
            }
            let (segment, reason) = self.retired.remove(index).expect("retired index exists");
            let report = segment.report(reason);
            if let (Some(delivery), Some(report)) = (self.delivery.as_ref(), report) {
                if delivery.sender.try_send(report).is_err() {
                    eprintln!("Spotify listening report queue full or closed; dropping segment");
                }
            }
        }
    }

    pub async fn drain(&mut self, deadline: tokio::time::Instant) {
        self.finish(false);
        self.collect();
        if let Some(delivery) = self.delivery.take() {
            let Delivery { sender, session_updates, mut task } = delivery;
            drop(sender);
            if tokio::time::timeout_at(deadline, &mut task).await.is_err() {
                task.abort();
                eprintln!("Spotify listening shutdown drain timed out; dropping pending reports");
            }
            // A closed auth watch is terminal to the reporter. Keep its sender
            // alive while the worker drains reports from the closed PCM queue.
            drop(session_updates);
        }
    }

    #[cfg(test)]
    pub(crate) fn observe_reports(&mut self) -> mpsc::UnboundedReceiver<PlaybackReport> {
        if let Some(delivery) = self.delivery.take() { delivery.task.abort(); }
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (reports, observed) = mpsc::unbounded_channel();
        self.delivery = Some(Delivery {
            sender,
            session_updates: watch::channel(self.session.as_ref().expect("test session").clone()).0,
            task: tokio::spawn(async move {
                while let Some(report) = receiver.recv().await {
                    if reports.send(report).is_err() { break; }
                }
            }),
        });
        observed
    }
}

impl Drop for SpotifyListening {
    fn drop(&mut self) { self.cancel_pending(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_duration_uses_device_rate_and_speed_not_wall_time_or_seek() {
        let counter = Consumption::new();
        counter.record(48_000, 48_000, 2.0);
        counter.record(44_100, 44_100, 0.5);
        assert_eq!(counter.played_ns.load(Ordering::Acquire), 2_000_000_000);
        assert_eq!(counter.nominal_ns.load(Ordering::Acquire), 2_500_000_000);
        counter.cancel();
        counter.record(48_000, 48_000, 1.0);
        assert_eq!(counter.played_ns.load(Ordering::Acquire), 2_000_000_000);
    }

    #[test]
    fn opt_out_invalidates_already_owned_audio_and_does_not_backfill() {
        let mut listening = SpotifyListening::new();
        let counter = Consumption::new();
        listening.active = Some(Segment {
            requested: "track".to_owned(), consumption: Arc::clone(&counter), selection: None,
            load: None,
            context_uri: None, reason_start: "playbtn", shuffle: false, repeat_track: false, repeat_context: false,
        });
        counter.record(48_000, 48_000, 1.0);
        assert!(listening.set_enabled(false));
        counter.record(48_000, 48_000, 1.0);
        assert!(!counter.valid.load(Ordering::Acquire));
        assert!(listening.set_enabled(true));
        assert!(listening.counter().is_none());
        assert!(listening.retired.is_empty());
    }

    #[tokio::test]
    async fn account_replacement_invalidates_old_audio_and_selected_identity() {
        let mut listening = SpotifyListening::new();
        let old = Session::new(librespot_core::SessionConfig::default(), None);
        old.set_username("old");
        listening.set_session(old);
        listening.begin("spotify:track:test", "", false, false, false);
        let counter = listening.counter().unwrap();
        counter.record(48_000, 48_000, 1.0);
        let new = Session::new(librespot_core::SessionConfig::default(), None);
        new.set_username("new");
        listening.set_session(new);
        assert!(!counter.valid.load(Ordering::Acquire));
        assert!(listening.retired.is_empty());
        assert!(listening.counter().is_none());
        listening.logout();
        assert!(listening.session.is_none());
    }

    #[tokio::test]
    async fn eof_delivery_waits_for_native_tail_and_uses_selected_relinked_file() {
        let mut listening = SpotifyListening::new();
        let session = Session::new(librespot_core::SessionConfig::default(), None);
        session.set_username("listener");
        // Observe reports at the real bounded delivery boundary, without
        // authenticating, making requests, or replacing native consumption.
        listening.session = Some(session);
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        listening.delivery = Some(Delivery {
            sender, task: tokio::spawn(std::future::pending()),
            session_updates: watch::channel(listening.session.as_ref().unwrap().clone()).0,
        });
        let requested = "spotify:track:0123456789ABCDEFGHIJKL";
        let resolved = SpotifyUri::from_uri("spotify:track:72AZ3V52rs9NfgMNhALxln").unwrap();
        listening.begin(requested, "playlist:0123456789ABCDEFGHIJKL", true, false, true);
        let load = LoadId { generation: 1, request: 1 };
        listening.loaded(load);
        listening.selected(load, requested, resolved.clone(), FileId([0x17; 20]), AudioFileFormat::OGG_VORBIS_320);
        let counter = listening.counter().unwrap();
        counter.pending_packets.store(1, Ordering::Release);
        counter.record(24_000, 48_000, 2.0);
        listening.finish(true);
        listening.collect();
        assert!(receiver.try_recv().is_err(), "decoder EOF is not an audible EOF");
        counter.record(24_000, 48_000, 2.0);
        counter.pending_packets.store(0, Ordering::Release);
        listening.collect();
        let report = receiver.try_recv().expect("tail consumed");
        assert_eq!(report.uri, resolved);
        assert_eq!(report.file_id, FileId([0x17; 20]));
        assert_eq!(report.played, Duration::from_secs(1));
        assert_eq!(report.nominal_played, Duration::from_secs(2));
        assert!(matches!(report.reason_end, EndReason::TrackDone));
        assert_eq!(report.context_uri.as_deref(), Some("spotify:playlist:0123456789ABCDEFGHIJKL"));
        assert!(receiver.try_recv().is_err(), "one report per segment");
    }

    #[tokio::test]
    async fn disabling_removes_delivery_queue_and_cancels_waiting_send() {
        let mut listening = SpotifyListening::new();
        let (sender, _receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        let task_observer = task.abort_handle();
        listening.delivery = Some(Delivery {
            sender, task,
            session_updates: watch::channel(Session::new(Default::default(), None)).0,
        });
        assert!(listening.set_enabled(false));
        tokio::task::yield_now().await;
        assert!(task_observer.is_finished(), "the in-flight future must be dropped");
        assert!(listening.delivery.is_none());
    }

    #[tokio::test]
    async fn replacement_load_cannot_relabel_old_or_new_frames_with_another_file() {
        let mut listening = SpotifyListening::new();
        listening.session = Some(Session::new(librespot_core::SessionConfig::default(), None));
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        listening.delivery = Some(Delivery {
            sender, task: tokio::spawn(std::future::pending()),
            session_updates: watch::channel(listening.session.as_ref().unwrap().clone()).0,
        });
        let requested = "spotify:track:0123456789ABCDEFGHIJKL";
        let uri = SpotifyUri::from_uri(requested).unwrap();
        listening.begin(requested, "", false, false, false);
        let old_load = LoadId { generation: 1, request: 1 };
        listening.loaded(old_load);
        listening.selected(old_load, requested, uri.clone(), FileId([1; 20]), AudioFileFormat::OGG_VORBIS_320);
        let old = listening.counter().unwrap();
        old.record(480, 48_000, 1.0);
        listening.prepare_load();
        listening.begin(requested, "", false, false, false);
        let new_load = LoadId { generation: 1, request: 2 };
        listening.loaded(new_load);
        let new = listening.counter().unwrap();
        new.record(960, 48_000, 1.0);
        // PCM may be consumed before the engine receives SelectedAudio.
        listening.selected(new_load, requested, uri, FileId([2; 20]), AudioFileFormat::OGG_VORBIS_160);
        listening.finish(false);
        listening.collect();
        let first = receiver.try_recv().unwrap();
        let second = receiver.try_recv().unwrap();
        assert_eq!((first.file_id, first.played), (FileId([1; 20]), Duration::from_millis(10)));
        assert_eq!((second.file_id, second.played), (FileId([2; 20]), Duration::from_millis(20)));
    }

    #[tokio::test]
    async fn same_account_reconnect_preserves_consumed_audio_and_queued_delivery() {
        let mut listening = SpotifyListening::new();
        let old = Session::new(Default::default(), None);
        old.set_username("listener");
        listening.account = Some(old.username());
        listening.session = Some(old.clone());
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (session_updates, _) = watch::channel(old);
        let task = tokio::spawn(std::future::pending());
        let observer = task.abort_handle();
        listening.delivery = Some(Delivery { sender, session_updates, task });
        let uri = "spotify:track:72AZ3V52rs9NfgMNhALxln";
        listening.begin(uri, "", false, false, false);
        let load = LoadId { generation: 1, request: 1 };
        listening.loaded(load);
        listening.selected(load, uri, SpotifyUri::from_uri(uri).unwrap(),
            FileId([0x17; 20]), AudioFileFormat::OGG_VORBIS_320);
        let counter = listening.counter().unwrap();
        counter.record(24_000, 48_000, 1.0);
        let replacement = Session::new(Default::default(), None);
        replacement.set_username("listener");
        listening.set_session(replacement);
        assert!(counter.valid.load(Ordering::Acquire));
        assert!(!observer.is_finished(), "an in-flight report retains its identity");
        counter.record(24_000, 48_000, 1.0);
        listening.finish(false);
        listening.collect();
        assert_eq!(receiver.try_recv().unwrap().played, Duration::from_secs(1));
        assert!(receiver.try_recv().is_err(), "no reconnect replay or duplication");
    }

    #[tokio::test]
    async fn unresolved_heard_loads_are_bounded_without_blocking_later_resolved_reports() {
        let mut listening = SpotifyListening::new();
        listening.session = Some(Session::new(Default::default(), None));
        let mut reports = listening.observe_reports();
        let requested = "spotify:track:72AZ3V52rs9NfgMNhALxln";
        let mut oldest = None;
        for request in 0..=QUEUE_CAPACITY as u64 {
            listening.prepare_load();
            listening.begin(requested, "", false, false, false);
            listening.loaded(LoadId { generation: 1, request });
            let counter = listening.counter().unwrap();
            counter.record(480, 48_000, 1.0);
            if oldest.is_none() { oldest = Some(counter); }
            listening.finish(false);
            listening.collect();
            assert_eq!(listening.retired.len(), (request as usize + 1).min(QUEUE_CAPACITY));
        }
        assert!(!oldest.unwrap().valid.load(Ordering::Acquire), "overflow cancels the oldest owner");
        listening.selected(LoadId { generation: 1, request: 0 }, requested,
            SpotifyUri::from_uri(requested).unwrap(), FileId([1; 20]), AudioFileFormat::OGG_VORBIS_320);
        assert!(listening.retired.iter().all(|(segment, _)| segment.selection.is_none()),
            "an evicted selection cannot identify another occurrence");
        listening.selected(LoadId { generation: 1, request: QUEUE_CAPACITY as u64 }, requested,
            SpotifyUri::from_uri(requested).unwrap(), FileId([2; 20]), AudioFileFormat::OGG_VORBIS_160);
        listening.collect();
        tokio::task::yield_now().await;
        let report = reports.try_recv().expect("resolved load must pass unresolved predecessors");
        assert_eq!(report.file_id, FileId([2; 20]));
        assert_eq!(report.played, Duration::from_millis(10));
        assert_eq!(listening.retired.len(), QUEUE_CAPACITY - 1);

        listening.prepare_load();
        listening.begin(requested, "", false, false, false);
        listening.loaded(LoadId { generation: 1, request: 100 });
        listening.finish(false);
        listening.collect();
        assert_eq!(listening.retired.len(), QUEUE_CAPACITY - 1,
            "zero-consumption unresolved loads need not wait for metadata");
    }

    #[tokio::test]
    async fn late_selection_after_opt_in_counts_only_fresh_pcm_and_logout_forgets_ownership() {
        let mut listening = SpotifyListening::new();
        let session = Session::new(Default::default(), None);
        session.set_username("listener");
        listening.set_session(session);
        let requested = "spotify:track:72AZ3V52rs9NfgMNhALxln";
        let load = LoadId { generation: 1, request: 1 };
        listening.begin(requested, "", false, false, false);
        listening.loaded(load);
        let cancelled = listening.counter().unwrap();
        cancelled.record(480, 48_000, 1.0);
        listening.set_enabled(false);
        listening.set_enabled(true);
        let mut reports = listening.observe_reports();
        listening.begin(requested, "", false, false, false);
        let fresh = listening.counter().unwrap();
        cancelled.record(960, 48_000, 1.0);
        fresh.record(240, 48_000, 1.0);
        // This metadata still identifies the incumbent decoder, not its
        // cancelled pre-opt-out receipt.
        listening.selected(load, requested, SpotifyUri::from_uri(requested).unwrap(),
            FileId([1; 20]), AudioFileFormat::OGG_VORBIS_320);
        listening.finish(false);
        listening.collect();
        tokio::task::yield_now().await;
        let report = reports.try_recv().unwrap();
        assert_eq!(report.played, Duration::from_millis(5));
        assert_eq!(cancelled.durations().0, Duration::from_millis(10));
        listening.logout();
        listening.selected(load, requested, SpotifyUri::from_uri(requested).unwrap(),
            FileId([2; 20]), AudioFileFormat::OGG_VORBIS_160);
        let replacement = Session::new(Default::default(), None);
        replacement.set_username("listener");
        listening.set_session(replacement);
        listening.begin(requested, "", false, false, false);
        assert!(listening.active.as_ref().unwrap().selection.is_none());
        assert!(listening.active.as_ref().unwrap().load.is_none());
    }

    #[tokio::test]
    async fn account_change_discards_retired_audio_and_cannot_reuse_late_selection_for_the_new_account() {
        let mut listening = SpotifyListening::new();
        let old_session = Session::new(Default::default(), None);
        old_session.set_username("old");
        listening.set_session(old_session);
        let requested = "spotify:track:72AZ3V52rs9NfgMNhALxln";
        let old_load = LoadId { generation: 1, request: 7 };
        listening.begin(requested, "", false, false, false);
        listening.loaded(old_load);
        let old_counter = listening.counter().unwrap();
        old_counter.record(480, 48_000, 1.0);
        listening.finish(false);
        let new_session = Session::new(Default::default(), None);
        new_session.set_username("new");
        listening.set_session(new_session);
        let mut reports = listening.observe_reports();
        let new_load = LoadId { generation: 2, request: 7 };
        listening.begin(requested, "", false, false, false);
        listening.loaded(new_load);
        let fresh = listening.counter().unwrap();
        fresh.record(960, 48_000, 1.0);
        listening.selected(old_load, requested, SpotifyUri::from_uri(requested).unwrap(),
            FileId([1; 20]), AudioFileFormat::OGG_VORBIS_320);
        assert!(!old_counter.valid.load(Ordering::Acquire));
        assert!(listening.retired.is_empty());
        assert!(listening.active.as_ref().unwrap().selection.is_none());
        listening.selected(new_load, requested, SpotifyUri::from_uri(requested).unwrap(),
            FileId([2; 20]), AudioFileFormat::OGG_VORBIS_160);
        listening.drain(tokio::time::Instant::now() + Duration::from_secs(3)).await;
        let report = reports.try_recv().expect("only the new account's PCM remains");
        assert_eq!((report.file_id, report.played), (FileId([2; 20]), Duration::from_millis(20)));
        assert!(reports.try_recv().is_err());
    }

    #[tokio::test]
    async fn shutdown_keeps_authentication_watch_open_until_queued_pcm_reports_are_delivered() {
        let mut listening = SpotifyListening::new();
        let session = Session::new(Default::default(), None);
        session.set_username("listener");
        listening.session = Some(session.clone());
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (session_updates, mut sessions) = watch::channel(session);
        let (observed, delivered) = tokio::sync::oneshot::channel();
        listening.delivery = Some(Delivery {
            sender, session_updates,
            task: tokio::spawn(async move {
                // Match the reporter's priority: a closed auth watch is
                // terminal, even when a PCM report is already ready.
                let report = tokio::select! {
                    biased;
                    _ = sessions.changed() => None,
                    report = receiver.recv() => report,
                };
                let _ = observed.send(report);
            }),
        });
        let requested = "spotify:track:72AZ3V52rs9NfgMNhALxln";
        let load = LoadId { generation: 1, request: 1 };
        listening.begin(requested, "", false, false, false);
        listening.loaded(load);
        listening.selected(load, requested, SpotifyUri::from_uri(requested).unwrap(),
            FileId([1; 20]), AudioFileFormat::OGG_VORBIS_320);
        listening.counter().unwrap().record(480, 48_000, 1.0);
        listening.drain(tokio::time::Instant::now() + Duration::from_secs(3)).await;
        let report = delivered.await.unwrap().expect("closing the report queue must not close authentication early");
        assert_eq!((report.file_id, report.played), (FileId([1; 20]), Duration::from_millis(10)));
    }
}
