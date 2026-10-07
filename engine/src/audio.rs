//! Lazy, immediate-pause cpal output for librespot.
//!
//! Construction checks presence without opening a stream. First playback opens
//! output; pause stops the native stream rather than running a silent mixer.
//! The callback cursor and device's submitted buffer survive pause,
//! together with queued packets, WSOLA overlap and rational resampler history.
//! Loads and seeks invalidate retained audio; natural boundaries remain gapless.
//!
//! Processing happens before the ring, at the device rate. The callback only
//! converts sample representation/channel layout and applies transport gain:
//! packet boundaries cannot reset the rational resampling phase.
//!
//! librespot can exit the process on start/stop errors, so those methods always
//! succeed. Native opening is deadline-bounded on a detached worker; failures
//! notify the engine even without a subsequent write. Ring writes are bounded
//! too, so dead output cannot wedge the player thread.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::AudioFormat;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::{AudioPacket, AudioPacketPosition};
use librespot_playback::player::DecoderTransition;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use renderer_engine::protocol::TrackEdit;
use tokio::sync::mpsc;

use crate::resample::Resampler;
use crate::time_stretch::{AudioPipeline, PipelineConfig};

/// The audible output, queue and processing state are claimed together at
/// first start, never when a replacement player is merely constructed.
///
/// Ownership has to be that explicit because two sinks exist at once whenever a
/// player is replaced while another is still playing: a session reconnect that
/// preserves playback, and a normalisation rebuild, both construct the
/// replacement before the incumbent is stopped. Claiming at construction would
/// point every one of these at the silent newcomer, and a newcomer that is then
/// discarded (a stale rebuild) would leave them dangling at a pipeline nothing
/// can hear — for the rest of the process. The `Drop` release is therefore
/// conditional: a sink only ever clears an entry that is still its own.
static LIVE_OUTPUT: Mutex<Weak<OutputControl>> = Mutex::new(Weak::new());
/// Cached gain is read once per device callback, including replacement output.
static SINK_GAIN: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
const LIVE_OUTPUT_POISON_MSG: &str = "live output registry should not be poisoned";
static LIVE_RING: Mutex<Weak<SampleRing>> = Mutex::new(Weak::new());
const LIVE_RING_POISON_MSG: &str = "live sample ring should not be poisoned";
static LIVE_PROCESSING: Mutex<Weak<Mutex<AudioProcessing>>> = Mutex::new(Weak::new());
const LIVE_PROCESSING_POISON_MSG: &str = "live audio processing registry should not be poisoned";
static CUSTOMIZATION_REVISION: AtomicU64 = AtomicU64::new(0);
static COMMITTED_REVISION: AtomicU64 = AtomicU64::new(0);
static CUSTOMIZATION_SPEED: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static CUSTOMIZATION: Mutex<Customization> = Mutex::new(Customization {
    config: None,
    discontinuous: false,
});
static AUDIO_SIGNAL_SENDER: Mutex<Option<mpsc::UnboundedSender<AudioSignal>>> = Mutex::new(None);

#[derive(Clone, Copy, Debug)]
pub enum AudioSignal {
    /// Delivered at the first frame using the new rate, after old-rate queued
    /// audio. Its callback timestamp anchors the transport, not decode-ahead.
    SpeedBoundary {
        speed: f32,
        revision: u64,
        at: Instant,
    },
    LoopBoundary {
        position_ms: u32,
        revision: u64,
    },
    /// The decoder handed this pipeline its first packet, i.e. the track the
    /// engine last configured is genuinely producing audio.
    ///
    /// This exists because librespot's events cannot answer the question. It
    /// reports a track `loaded` and then `Playing` before a single sample has
    /// been decoded, so a track whose audio key the service refused — which
    /// librespot deliberately loads anyway, undecrypted (`player.rs`: "Unable
    /// to load key, continuing without decryption") — announces itself exactly
    /// like a track that works. Everything downstream of that announcement,
    /// the playhead included, is then a fiction until the decoder chokes on
    /// ciphertext seconds later. Only the sink knows the difference, so
    /// the sink is what says so.
    Output {
        revision: u64,
    },
    /// Native callbacks are running for this revision. The callback only sets
    /// an atomic; the player thread's next write reports it, so the realtime
    /// thread never takes a lock or sends on a channel for it.
    OutputReady { revision: u64 },
    /// Native failure is observable even when no decoder write follows.
    OutputFailed { revision: u64, blocked: bool },
    /// A write waited out [`WRITE_DRAIN_TIMEOUT`] without output consumption.
    /// librespot turns write failures into pause without identifying the device,
    /// so this signal tells the engine to recover output rather than the track.
    /// Revision gating prevents a retiring pipeline from replacing its successor.
    OutputStalled {
        revision: u64,
    },
    /// The decoder handed the sink a packet it cannot play (a partial frame,
    /// or no PCM at all). librespot turns the write error into a silent pause,
    /// so this is how the engine learns that the track, not the device, failed.
    DecodeFailed {
        revision: u64,
    },
}

/// Hands one signal to the engine. Called from the player thread (and the
/// device error callback), never from the realtime data callback: what that
/// callback reaches is recorded in atomics or ring state and reported by the
/// producer. The lock is only ever held for the send itself, and a signal
/// nobody is listening for is dropped.
fn signal(signal: AudioSignal) {
    let sender = AUDIO_SIGNAL_SENDER
        .lock()
        .expect("audio signal sender should not be poisoned");
    if let Some(sender) = sender.as_ref() {
        let _ = sender.send(signal);
    }
}

struct Customization {
    config: Option<PipelineConfig>,
    /// Sticky until the sink consumes the newest revision: if a seek/config
    /// reset is followed by a natural track setup before another decoder
    /// packet arrives, the old filter state still must not survive.
    discontinuous: bool,
}

pub fn install_signal_sender(sender: mpsc::UnboundedSender<AudioSignal>) {
    *AUDIO_SIGNAL_SENDER
        .lock()
        .expect("audio signal sender should not be poisoned") = Some(sender);
}

/// Stages a discontinuous customization for its owning decoder command.
/// The command commits it after any old in-flight decoder read has returned.
pub fn configure_customization_at_loop_pass(
    edit: Option<TrackEdit>,
    speed: f32,
    position_ms: u32,
    loop_pass: u32,
) -> u64 {
    set_customization(edit, speed, position_ms, true, loop_pass)
}

/// Stages a gapless track while retaining outgoing audio and filter history.
/// The outgoing pipeline is flushed separately by [`finish_natural_boundary`].
pub fn configure_customization_after_natural_boundary(
    edit: Option<TrackEdit>,
    speed: f32,
    position_ms: u32,
) -> u64 {
    set_customization(edit, speed, position_ms, false, 1)
}

/// Changes rate without clearing the ring, resetting edits or seeking the
/// decoder. The player thread applies it at its next packet boundary.
pub fn set_customization_speed(speed: f32) {
    let mut customization = CUSTOMIZATION
        .lock()
        .expect("audio customization should not be poisoned");
    let config = customization.config.get_or_insert(PipelineConfig {
        edit: None,
        speed,
        position_ms: 0,
        loop_pass: 1,
    });
    config.speed = speed;
    CUSTOMIZATION_SPEED.store(speed.to_bits(), Ordering::Release);
}

fn set_customization(
    edit: Option<TrackEdit>,
    speed: f32,
    position_ms: u32,
    discontinuous: bool,
    loop_pass: u32,
) -> u64 {
    let config = PipelineConfig {
        edit,
        speed,
        position_ms,
        loop_pass: loop_pass.max(1),
    };
    let active = config.active();
    let mut customization = CUSTOMIZATION
        .lock()
        .expect("audio customization should not be poisoned");
    customization.config = active.then_some(config);
    CUSTOMIZATION_SPEED.store(speed.to_bits(), Ordering::Release);
    customization.discontinuous |= discontinuous;
    let revision = CUSTOMIZATION_REVISION
        .fetch_add(1, Ordering::Release)
        .wrapping_add(1);
    drop(customization);
    // Writes retain the decoder's revision, so an in-flight old read cannot
    // consume the staged target config, emit proof for it, or be deferred.

    if discontinuous {
        // Read the user-pause intent as the clear ends it: the decoder's own
        // stop inside a playing seek also clears `running`, so a second quick
        // seek must not mistake that stop for a pause.
        let user_paused = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()
            .is_some_and(|ring| ring.clear());
        if let Some(output) = LIVE_OUTPUT.lock().expect(LIVE_OUTPUT_POISON_MSG).upgrade() {
            output.discard_if_paused(user_paused);
        }
    }

    revision
}

/// Binds the staged revision to a command handled on the decoder thread.
pub fn decoder_transition(revision: u64) -> DecoderTransition {
    DecoderTransition {
        revision,
        commit: Box::new(move || {
            let customization = CUSTOMIZATION.lock().expect("audio customization should not be poisoned");
            if CUSTOMIZATION_REVISION.load(Ordering::Acquire) != revision {
                return false;
            }
            COMMITTED_REVISION.store(revision, Ordering::Release);
            if let Some(output) = LIVE_OUTPUT.lock().expect(LIVE_OUTPUT_POISON_MSG).upgrade() {
                output.status.revision.store(revision, Ordering::Release);
                output.status.ready.store(false, Ordering::Release);
            }
            drop(customization);
            true
        }),
        is_current: Box::new(move || current_decoder_revision(revision)),
    }
}

fn current_decoder_revision(revision: u64) -> bool {
    CUSTOMIZATION_REVISION.load(Ordering::Acquire) == revision
        && COMMITTED_REVISION.load(Ordering::Acquire) == revision
}

pub fn customization_revision() -> u64 {
    CUSTOMIZATION_REVISION.load(Ordering::Acquire)
}

pub fn customization_pending(revision: u64) -> bool {
    CUSTOMIZATION_REVISION.load(Ordering::Acquire) == revision
        && COMMITTED_REVISION.load(Ordering::Acquire) != revision
}

/// Flushes the delayed WSOLA overlap region at decoder EOF without resetting
/// the output queue. This is intentionally separate from `stop`: a natural
/// boundary must drain audibly, while seeks/config changes discard stale
/// queued audio. User pause retains both the queue and unflushed processing.
pub fn finish_natural_boundary() -> Result<(), String> {
    let Some(processing) = LIVE_PROCESSING
        .lock()
        .expect(LIVE_PROCESSING_POISON_MSG)
        .upgrade()
    else {
        return Ok(());
    };
    let Some(ring) = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade() else {
        return Ok(());
    };

    let mut queued = Vec::new();
    let timing;
    {
        let mut processing = processing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut tail = Vec::new();
        if let Some(pipeline) = &mut processing.pipeline {
            pipeline.finish(&mut tail);
        }
        match &mut processing.resampler {
            Some(resampler) => resampler.process(&tail, &mut queued),
            None => queued = tail,
        }
        timing = (processing.pipeline_revision, processing.speed, processing.output_rate);
    }
    if queued.is_empty() {
        return Ok(());
    }
    ring.push_timed_at_generation(ring.generation.load(Ordering::Acquire),
        queued, None, 0, WRITE_DRAIN_TIMEOUT, Some(timing))
        .map_err(|()| "audio output stalled while flushing the natural EOF tail".to_owned())
}

/// Flush the rational filter only at queue EOF, never between gapless tracks.
pub fn finish_output_tail() -> Result<(), String> {
    let processing = LIVE_PROCESSING.lock().expect(LIVE_PROCESSING_POISON_MSG).upgrade();
    let ring = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade();
    let (Some(processing), Some(ring)) = (processing, ring) else { return Ok(()); };
    let mut queued = Vec::new();
    let timing;
    {
        let mut processing = processing.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(resampler) = &mut processing.resampler {
            resampler.finish(&mut queued);
        }
        timing = (processing.pipeline_revision, processing.speed, processing.output_rate);
    }
    ring.push_timed_at_generation(ring.generation.load(Ordering::Acquire),
        queued, None, 0, WRITE_DRAIN_TIMEOUT, Some(timing))
        .map_err(|()| "audio output stalled while flushing the resampler tail".to_owned())
}

#[derive(Debug)]
pub enum OutputError {
    NoDeviceAvailable,
    StreamError(cpal::BuildStreamError),
    Other(String),
}

impl From<cpal::BuildStreamError> for OutputError {
    fn from(error: cpal::BuildStreamError) -> Self {
        Self::StreamError(error)
    }
}

impl fmt::Display for OutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDeviceAvailable => formatter.write_str("no output device is present"),
            Self::StreamError(error) => {
                write!(formatter, "output device could not be opened: {error}")
            }
            Self::Other(text) => formatter.write_str(text),
        }
    }
}

impl From<cpal::DefaultStreamConfigError> for OutputError {
    fn from(error: cpal::DefaultStreamConfigError) -> Self {
        Self::Other(format!("output device configuration failed: {error}"))
    }
}

impl From<cpal::SupportedStreamConfigsError> for OutputError {
    fn from(error: cpal::SupportedStreamConfigsError) -> Self {
        Self::Other(format!("output device configurations could not be listed: {error}"))
    }
}

/// How long [`OutputSink::write`] waits for ring space before declaring the
/// output stalled. Healthy playback frees a chunk every few tens of
/// milliseconds, so this only fires when the audio thread is dead (device
/// unplugged, driver failure) — the case where the stock librespot loop would
/// otherwise spin forever and wedge the player thread mid-track-change.
const WRITE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Decoded-audio write-ahead budget, in milliseconds. This is the engine's
/// *output* latency: every seek, volume change, and track switch waits for this
/// much already-decoded audio to play out first, and the reported position
/// leads the sound by the same amount. librespot's stock budget is ~0.5 s,
/// which made pause/seek/volume/next feel half a second behind. Network stalls
/// are covered separately by the audio-fetch read-ahead window (3 s), not by
/// this ring, so keeping it small does not weaken stall protection.
const WRITE_AHEAD_MS: usize = 150;

/// [`WRITE_AHEAD_MS`] as interleaved samples at `rate`. The ring holds
/// device-rate audio (the resampler runs before it), so the budget is derived
/// from the rate that is actually queued rather than from librespot's.
fn write_ahead_samples(rate: u32) -> usize {
    rate as usize * NUM_CHANNELS as usize * WRITE_AHEAD_MS / 1000
}

/// Audio handed from the player thread to the audio callback.
///
/// The producer pushes processed packets with bounded backpressure. The
/// consumer locks once per packet, never once per sample, and keeps the current
/// packet in the native stream's callback for its entire lifetime.
struct SampleRing {
    /// Interleaved samples the ring will hold before [`SampleRing::push`]
    /// blocks. See [`write_ahead_samples`].
    capacity: AtomicUsize,
    state: Mutex<RingState>,
    /// Signalled when the consumer frees space, and when [`SampleRing::clear`]
    /// empties the ring, so a blocked producer wakes promptly instead of
    /// polling. This replaces the stock backend's 10 ms sleep loop, which
    /// burned idle CPU in an app whose whole point is not to.
    space_freed: Condvar,
    /// Bumped by [`SampleRing::clear`]. [`LiveSource`] reads it on every frame
    /// boundary and drops the packet it is mid-way through when it changes, so
    /// `stop()` silences audio already handed to the audio thread rather than
    /// letting up to a packet of stale sound trail the stop.
    generation: AtomicU64,
    retaining_pause: AtomicBool,
    #[cfg(test)]
    empty_pops: AtomicU64,
}

#[derive(Default)]
struct RingPacket {
    samples: Vec<f32>,
    /// Nonzero on the packet that ends a loop pass. The callback acknowledges
    /// it once played through; see [`RingState::marker`].
    boundary_id: u64,
    timing: Option<PacketTiming>,
}

/// A queued loop marker the engine has not been told about yet.
#[derive(Clone, Copy)]
struct LoopMarker {
    boundary_id: u64,
    position_ms: u32,
    /// The customization revision that produced the marker. A packet can
    /// outlive a seek/track change, so reading the current global revision at
    /// delivery time would incorrectly retag stale audio as current.
    revision: u64,
}

#[derive(Clone, Copy)]
struct PacketTiming {
    revision: u64,
    start_ms: f64,
    duration_ms: f64,
    speed: f32,
}

#[derive(Clone, Copy)]
struct AudibleClock {
    timing: PacketTiming,
    at: Instant,
}

struct RingState {
    packets: VecDeque<RingPacket>,
    /// Kept alongside `packets` so the producer's fullness check is a field
    /// read rather than a walk of the deque.
    queued_samples: usize,
    next_boundary_id: u64,
    consumed_boundary_id: u64,
    /// The newest marker queued and not yet reported. The producer waits for
    /// the callback to play through it and then reports it itself, so the
    /// realtime thread never takes the signal lock or sends on a channel.
    marker: Option<LoopMarker>,
    /// A rate the callback has just started playing, for the producer's next
    /// push to report on the same terms.
    speed_boundary: Option<AudioSignal>,
    timeline_revision: u64,
    timeline_ms: f64,
    audible: Option<AudibleClock>,
    paused_elapsed_ms: Option<f64>,
}

impl SampleRing {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(SampleRing {
            capacity: AtomicUsize::new(capacity),
            state: Mutex::new(RingState {
                packets: VecDeque::with_capacity(64),
                queued_samples: 0,
                next_boundary_id: 0,
                consumed_boundary_id: 0,
                marker: None,
                speed_boundary: None,
                timeline_revision: 0,
                timeline_ms: 0.0,
                audible: None,
                paused_elapsed_ms: None,
            }),
            space_freed: Condvar::new(),
            generation: AtomicU64::new(0),
            retaining_pause: AtomicBool::new(false),
            #[cfg(test)]
            empty_pops: AtomicU64::new(0),
        })
    }

    /// A poisoned ring must not take the audio callback or the player thread
    /// down with it: every field is updated in one uninterruptible step, so the
    /// state behind a poisoned lock is always consistent and safe to reuse.
    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Test-only: the production paths read the field under a lock they
    /// already hold, so taking a second one here would be wasted work.
    #[cfg(test)]
    fn queued_samples(&self) -> usize {
        self.lock().queued_samples
    }

    #[cfg(test)]
    fn push(&self, packet: Vec<f32>, timeout: Duration) -> Result<(), ()> {
        self.push_marked(packet, None, 0, timeout)
    }

    #[cfg(test)]
    fn push_marked(
        &self,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        loop_revision: u64,
        timeout: Duration,
    ) -> Result<(), ()> {
        let generation = self.generation.load(Ordering::Acquire);
        self.push_marked_at_generation(generation, samples, loop_to_ms, loop_revision, timeout)
    }

    #[cfg(test)]
    fn push_marked_at_generation(
        &self,
        generation: u64,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        loop_revision: u64,
        timeout: Duration,
    ) -> Result<(), ()> {
        self.push_timed_at_generation(
            generation, samples, loop_to_ms, loop_revision, timeout, None,
        )
    }

    fn push_timed_at_generation(
        &self,
        generation: u64,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        loop_revision: u64,
        timeout: Duration,
        timing: Option<(u64, f32, u32)>,
    ) -> Result<(), ()> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        if self.generation.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        while state.queued_samples >= self.capacity.load(Ordering::Relaxed)
            && !self.retaining_pause.load(Ordering::Acquire)
        {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(());
            };
            let (guard, _) = self
                .space_freed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            if self.generation.load(Ordering::Acquire) != generation {
                return Ok(());
            }
        }
        let boundary_id = if let Some(position_ms) = loop_to_ms {
            state.next_boundary_id = state.next_boundary_id.wrapping_add(1);
            let boundary_id = state.next_boundary_id;
            state.marker = Some(LoopMarker { boundary_id, position_ms, revision: loop_revision });
            boundary_id
        } else {
            0
        };
        let timing = timing.filter(|_| !samples.is_empty()).map(|(revision, speed, rate)| {
            if state.timeline_revision != revision {
                state.timeline_revision = revision;
                state.timeline_ms = 0.0;
            }
            let duration_ms = samples.len() as f64 * 1_000.0
                / (f64::from(rate) * f64::from(NUM_CHANNELS));
            let timing = PacketTiming {
                revision,
                start_ms: state.timeline_ms,
                duration_ms,
                speed,
            };
            state.timeline_ms += duration_ms * f64::from(speed);
            timing
        });
        state.queued_samples += samples.len();
        state.packets.push_back(RingPacket {
            samples,
            boundary_id,
            timing,
        });
        self.settle(state, deadline)
    }

    /// Reports what the callback has reached since the producer last looked,
    /// and holds the producer at an outstanding loop marker until the callback
    /// has played through it.
    ///
    /// A loop marker is flow control, not a notification attached to an
    /// otherwise ordinary packet: holding the decoder here bounds
    /// decode/network run-ahead even when every packet beyond the loop is
    /// filtered to empty, and it is what lets this thread, not the realtime
    /// one, report the boundary the moment it is heard. A clear drops the
    /// marker with its audio. A user pause releases the producer with the
    /// marker still outstanding; the resuming [`OutputSink::start`] settles it.
    fn settle<'a>(
        &'a self,
        mut state: std::sync::MutexGuard<'a, RingState>,
        deadline: Instant,
    ) -> Result<(), ()> {
        loop {
            let speed = state.speed_boundary.take();
            let reached = state.marker
                .filter(|marker| state.consumed_boundary_id >= marker.boundary_id);
            if reached.is_some() {
                state.marker = None;
            }
            let hold = state.marker.is_some() && !self.retaining_pause.load(Ordering::Acquire);
            if speed.is_some() || reached.is_some() {
                drop(state);
                if let Some(speed) = speed {
                    signal(speed);
                }
                if let Some(marker) = reached {
                    signal(AudioSignal::LoopBoundary {
                        position_ms: marker.position_ms,
                        revision: marker.revision,
                    });
                }
                if !hold {
                    return Ok(());
                }
                state = self.lock();
                continue;
            }
            if !hold {
                return Ok(());
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(());
            };
            let (guard, _) = self
                .space_freed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
    }

    /// Takes the next packet, or `None` when the producer has fallen behind.
    fn pop(&self) -> Option<RingPacket> {
        let mut state = self.lock();
        let Some(packet) = state.packets.pop_front() else {
            #[cfg(test)]
            self.empty_pops.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        state.queued_samples -= packet.samples.len();
        if let Some(timing) = packet.timing {
            let previous = state.audible;
            let at = Instant::now();
            state.audible = Some(AudibleClock { timing, at });
            if previous.is_none_or(|previous| previous.timing.revision != timing.revision
                || previous.timing.speed != timing.speed)
            {
                state.speed_boundary = Some(AudioSignal::SpeedBoundary {
                    speed: timing.speed,
                    revision: timing.revision,
                    at,
                });
            }
        }
        drop(state);
        self.space_freed.notify_one();
        Some(packet)
    }

    fn acknowledge_boundary(&self, boundary_id: u64) {
        if boundary_id == 0 {
            return;
        }
        let mut state = self.lock();
        state.consumed_boundary_id = state.consumed_boundary_id.max(boundary_id);
        drop(state);
        self.space_freed.notify_all();
    }

    fn audible_elapsed_ms(&self, revision: u64) -> Option<f64> {
        self.audible_elapsed_at(revision, Instant::now())
    }

    /// Wall time until everything handed to this ring has been heard: the rest
    /// of the packet the device is playing plus every packet queued behind it.
    /// `None` once it has all played.
    fn tail_remaining_at(&self, now: Instant) -> Option<Duration> {
        let state = self.lock();
        let playing = state.audible.map_or(0.0, |clock| {
            (clock.timing.duration_ms
                - now.saturating_duration_since(clock.at).as_secs_f64() * 1_000.0)
                .max(0.0)
        });
        let queued: f64 = state.packets.iter()
            .filter_map(|packet| packet.timing)
            .map(|timing| timing.duration_ms)
            .sum();
        let remaining = playing + queued;
        (remaining > 0.0).then(|| Duration::from_secs_f64(remaining / 1_000.0))
    }

    fn audible_elapsed_at(&self, revision: u64, at: Instant) -> Option<f64> {
        let state = self.lock();
        let Some(clock) = state.audible.filter(|clock| clock.timing.revision == revision) else {
            // Before the first callback, queued audio is still at its initial
            // source offset; a decoder-head event is not audible progress.
            return state.packets.iter().find_map(|packet|
                packet.timing.filter(|timing| timing.revision == revision)
                    .map(|timing| timing.start_ms));
        };
        Some(clock.timing.start_ms
            + state.paused_elapsed_ms.unwrap_or_else(||
                (at.saturating_duration_since(clock.at).as_secs_f64() * 1_000.0)
                    .min(clock.timing.duration_ms)) * f64::from(clock.timing.speed))
    }

    fn pause_at(&self, at: Instant) {
        let mut state = self.lock();
        if state.paused_elapsed_ms.is_none() {
            state.paused_elapsed_ms = Some(state.audible.map_or(0.0, |clock|
                (at.saturating_duration_since(clock.at).as_secs_f64() * 1_000.0)
                    .min(clock.timing.duration_ms)));
        }
        self.retaining_pause.store(true, Ordering::Release);
        drop(state);
        // A producer may be inside write() when pause arrives. Admit that one
        // in-flight packet so librespot can poll the pause command instead of
        // waiting on a device we have deliberately stopped.
        self.space_freed.notify_all();
    }

    fn resume_at(&self, at: Instant) {
        let mut state = self.lock();
        if let Some(elapsed) = state.paused_elapsed_ms.take() {
            if let Some(clock) = &mut state.audible {
                clock.at = at - Duration::from_secs_f64(elapsed / 1_000.0);
            }
        }
        self.retaining_pause.store(false, Ordering::Release);
    }

    fn stop_processing(&self, processing: &Mutex<AudioProcessing>) {
        if !self.retaining_pause.load(Ordering::Acquire) {
            self.clear();
            processing.lock().unwrap_or_else(PoisonError::into_inner).reset();
        }
    }

    /// Drops every queued packet and tells [`LiveSource`] to drop the one it
    /// holds. Non-blocking: this is what makes `stop()` instant. Returns
    /// whether this ended a user pause's retention.
    fn clear(&self) -> bool {
        let mut state = self.lock();
        state.packets.clear();
        state.queued_samples = 0;
        state.consumed_boundary_id = state.next_boundary_id;
        state.marker = None;
        state.speed_boundary = None;
        state.audible = None;
        state.paused_elapsed_ms = None;
        let retained = self.retaining_pause.swap(false, Ordering::AcqRel);
        state.timeline_ms = 0.0;
        state.timeline_revision = 0;
        self.generation.fetch_add(1, Ordering::Release);
        drop(state);
        self.space_freed.notify_all();
        retained
    }
}

/// Source-time elapsed on the packet actually feeding the device, excluding
/// startup/underrun silence and decode-ahead. Cuts map through Engine's timeline.
pub fn audible_elapsed_ms(revision: u64) -> Option<f64> {
    LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()?
        .audible_elapsed_ms(revision)
}

/// How long the live output still needs to play what it already holds — the
/// natural end of a queue drains this tail before the engine stops the player.
/// `None` once it has drained, and when nothing owns the output.
pub fn output_tail_remaining() -> Option<Duration> {
    LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()?
        .tail_remaining_at(Instant::now())
}

pub fn audible_speed(revision: u64) -> Option<f32> {
    let ring = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()?;
    let state = ring.lock();
    state.audible.filter(|clock| clock.timing.revision == revision)
        .map(|clock| clock.timing.speed)
        .or_else(|| state.packets.iter().find_map(|packet|
            packet.timing.filter(|timing| timing.revision == revision)
                .map(|timing| timing.speed)))
}

/// A user pause retains the audible packet, queued audio and processing
/// lookahead. Seeks/loads still clear all three through customization.
pub fn pause_output() {
    let output = LIVE_OUTPUT.lock().expect(LIVE_OUTPUT_POISON_MSG).upgrade();
    if let Some(output) = output {
        output.pause();
    }
    if let Some(ring) = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade() {
        ring.pause_at(Instant::now());
    }
}

/// Callback proof remains valid after pause, even if no later write reports it.
pub fn output_is_ready(revision: u64) -> bool {
    LIVE_OUTPUT.lock().expect(LIVE_OUTPUT_POISON_MSG).upgrade()
        .is_some_and(|output| output.status.revision.load(Ordering::Acquire) == revision
            && output.status.ready.load(Ordering::Acquire)
            && !output.status.failed.load(Ordering::Acquire))
}

/// The packet cursor belongs to the native callback and survives stream pause.
struct LiveSource {
    ring: Arc<SampleRing>,
    /// The generation this source is playing; a mismatch means `stop()` ran.
    generation: u64,
    packet: RingPacket,
    pos: usize,
    /// Silence is emitted in whole frames so an underrun cannot swap channels.
    silence_remaining: usize,
    underrun: bool,
}

impl LiveSource {
    fn new(ring: Arc<SampleRing>) -> Self {
        let generation = ring.generation.load(Ordering::Acquire);
        LiveSource {
            ring,
            generation,
            packet: RingPacket::default(),
            pos: 0,
            silence_remaining: 0,
            underrun: false,
        }
    }

    fn render<T: cpal::Sample + cpal::FromSample<f32>>(
        &mut self,
        data: &mut [T],
        channels: usize,
        running: bool,
        gain: f32,
    ) {
        if !running {
            data.fill(T::EQUILIBRIUM);
            return;
        }
        let mut frames = data.chunks_exact_mut(channels);
        while let Some(frame) = frames.next() {
            let left = self.next().unwrap_or(0.0) * gain;
            if self.underrun {
                // Do not lock an empty ring once per silent frame. The next
                // native callback checks for newly produced audio.
                frame.fill(T::EQUILIBRIUM);
                for remaining in frames {
                    remaining.fill(T::EQUILIBRIUM);
                }
                self.silence_remaining = 0;
                return;
            }
            let right = self.next().unwrap_or(0.0) * gain;
            if channels == 1 {
                frame[0] = T::from_sample((left + right) * 0.5);
            } else {
                frame[0] = T::from_sample(left);
                frame[1] = T::from_sample(right);
                frame[2..].fill(T::EQUILIBRIUM);
            }
        }
    }
}

impl Iterator for LiveSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        self.underrun = false;
        if self.silence_remaining > 0 {
            self.silence_remaining -= 1;
            return Some(0.0);
        }

        // Dropping buffered audio part-way through a frame would swap the
        // channels, so only act on a stop once the frame is complete. The
        // check costs one uncontended atomic load per sample.
        if self.pos % NUM_CHANNELS as usize == 0 {
            let generation = self.ring.generation.load(Ordering::Acquire);
            if generation != self.generation {
                self.generation = generation;
                self.packet = RingPacket::default();
                self.pos = 0;
            }
        }

        while self.pos == self.packet.samples.len() {
            // The loop pass is now heard; the waiting producer reports it.
            self.ring.acknowledge_boundary(std::mem::take(&mut self.packet.boundary_id));
            match self.ring.pop() {
                Some(packet) => {
                    self.packet = packet;
                    self.pos = 0;
                }
                None => {
                    // Underrun, or simply paused/stopped. Packets hold whole
                    // frames, so this is always a frame boundary.
                    self.silence_remaining = NUM_CHANNELS as usize - 1;
                    self.underrun = true;
                    return Some(0.0);
                }
            }
        }

        let sample = self.packet.samples[self.pos];
        self.pos += 1;
        Some(sample)
    }

}

fn claim_live<T>(slot: &Mutex<Weak<T>>, value: &Arc<T>, poison: &str) {
    *slot.lock().expect(poison) = Arc::downgrade(value);
}

#[derive(Default)]
struct OutputStatus {
    running: AtomicBool,
    failed: AtomicBool,
    revision: AtomicU64,
    attempt: AtomicU64,
    /// Set by the native callback once it runs for the current revision;
    /// reported to the engine by the player thread (see
    /// [`AudioSignal::OutputReady`]).
    ready: AtomicBool,
    failure_gate: Mutex<()>,
    #[cfg(test)]
    callbacks: AtomicU64,
}

impl OutputStatus {
    fn fail(&self, blocked: bool) {
        self.fail_at_attempt(self.attempt.load(Ordering::Acquire), blocked);
    }

    fn fail_at_attempt(&self, attempt: u64, blocked: bool) {
        let _guard = self.failure_gate.lock().unwrap_or_else(PoisonError::into_inner);
        if self.attempt.load(Ordering::Acquire) != attempt {
            return;
        }
        self.running.store(false, Ordering::Release);
        if !self.failed.swap(true, Ordering::AcqRel) {
            signal(AudioSignal::OutputFailed {
                revision: self.revision.load(Ordering::Acquire),
                blocked,
            });
        }
    }

    fn begin_attempt(&self) -> u64 {
        let _guard = self.failure_gate.lock().unwrap_or_else(PoisonError::into_inner);
        self.failed.store(false, Ordering::Release);
        self.attempt.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }

    fn abandon_attempt(&self) {
        let _guard = self.failure_gate.lock().unwrap_or_else(PoisonError::into_inner);
        self.attempt.fetch_add(1, Ordering::AcqRel);
    }
}

/// A native output stream that is built on the detached opener thread, kept by
/// the player thread, and paused or dropped from whichever thread stops the sink.
///
/// cpal 0.16 declares its CoreAudio `Stream` `!Send` only by omission: the boxed
/// closure of its device-disconnect listener has no `Send` bound. Everything
/// that closure captures is `Send` (a `Weak` of the mutex-guarded stream state
/// and an `Arc<Mutex<_>>` of the `Send` error callback), `AudioUnit` is `Send`,
/// and CoreAudio already invokes the closure from its own thread, so moving the
/// stream between threads adds no new access. cpal 0.17 makes this official.
/// Other platforms keep the compiler-checked `Send`.
struct OutputStream(cpal::Stream);

#[cfg(target_os = "macos")]
// SAFETY: see the type's documentation; the stream state is mutex-guarded.
unsafe impl Send for OutputStream {}

impl std::ops::Deref for OutputStream {
    type Target = cpal::Stream;

    fn deref(&self) -> &cpal::Stream {
        &self.0
    }
}

#[derive(Default)]
struct OutputControl {
    stream: Mutex<Option<OutputStream>>,
    status: Arc<OutputStatus>,
    error: Mutex<Option<String>>,
    pending: Mutex<Option<std::sync::mpsc::Sender<Result<(OutputStream, cpal::SupportedStreamConfig), OutputError>>>>,
}

impl OutputControl {
    fn fail(&self, error: impl fmt::Display) {
        *self.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(error.to_string());
        self.status.fail(false);
    }

    fn pause(&self) {
        self.status.running.store(false, Ordering::Release);
        if let Some(pending) = self.pending.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = pending.send(Err(OutputError::Other("audio opening cancelled".to_owned())));
        }
        let mut stream = self.stream.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(output) = stream.as_ref() {
            if let Err(error) = output.pause() {
                self.fail(error);
                // A failed pause must not leave callbacks running.
                stream.take();
            }
        }
    }

    /// Reopening the device costs an audible gap, so a playing discontinuity
    /// keeps the stream: what the device still holds is one period (~10 ms) of
    /// the old position. During a user pause that period would play on resume,
    /// however long after, so the next start opens a fresh stream instead.
    fn discard_if_paused(&self, user_paused: bool) {
        let mut stream = self.stream.lock().unwrap_or_else(PoisonError::into_inner);
        if user_paused && !self.status.running.load(Ordering::Acquire) {
            stream.take();
        }
    }
}

/// Clears one live-pipeline entry, but only while it still names this sink. A
/// sink outlived by its replacement must not deregister the pipeline that has
/// already taken over from it.
fn release_live<T>(slot: &Mutex<Weak<T>>, value: &Arc<T>, poison: &str) {
    let mut slot = slot.lock().expect(poison);
    if slot.ptr_eq(&Arc::downgrade(value)) {
        *slot = Weak::new();
    }
}

struct AudioProcessing {
    resampler: Option<Resampler>,
    pipeline_revision: u64,
    pipeline: Option<AudioPipeline>,
    speed: f32,
    output_rate: u32,
}

impl AudioProcessing {
    fn synchronize_pipeline(&mut self, revision: u64) -> bool {
        if self.pipeline_revision == revision {
            return current_decoder_revision(revision);
        }
        let (config, discontinuous) = {
            let mut customization = CUSTOMIZATION
                .lock()
                .expect("audio customization should not be poisoned");
            if !current_decoder_revision(revision) {
                return false;
            }
            let config = customization.config.clone();
            let discontinuous = std::mem::take(&mut customization.discontinuous);
            (config, discontinuous)
        };
        self.pipeline = config.as_ref().map(AudioPipeline::new);
        self.speed = config.as_ref().map_or(1.0, |config| config.speed);
        self.pipeline_revision = revision;
        if discontinuous {
            if let Some(resampler) = &mut self.resampler {
                resampler.reset();
            }
        }
        true
    }

    fn synchronize_speed(&mut self, pending: &mut Vec<f32>) -> Option<f32> {
        let speed = f32::from_bits(CUSTOMIZATION_SPEED.load(Ordering::Acquire));
        if self.speed == speed {
            return None;
        }
        if let Some(pipeline) = &mut self.pipeline {
            pipeline.set_speed(speed, pending);
        } else if speed != 1.0 {
            let mut pipeline = AudioPipeline::new(&PipelineConfig {
                edit: None,
                speed: 1.0,
                position_ms: 0,
                loop_pass: 1,
            });
            pipeline.set_speed(speed, pending);
            self.pipeline = Some(pipeline);
        }
        if speed == 1.0 && self.pipeline.as_ref().is_some_and(AudioPipeline::is_passthrough) {
            self.pipeline = None;
        }
        self.speed = speed;
        Some(speed)
    }

    /// Shared by the real sink and device-free audio regression tests. Exact
    /// 1x/no edit returns the converter's allocation untouched.
    fn render_packet(
        &mut self,
        samples: Vec<f32>,
        source_frame: u64,
        pipeline_scratch: &mut Vec<f32>,
        resampler_scratch: &mut Vec<f32>,
    ) -> (Vec<f32>, Option<u32>) {
        if self.pipeline.is_none() && self.resampler.is_none() {
            return (samples, None);
        }
        pipeline_scratch.clear();
        resampler_scratch.clear();
        let (filtered, loop_to) = match &mut self.pipeline {
            Some(pipeline) => {
                let loop_to = pipeline.process(source_frame, &samples, pipeline_scratch);
                (&pipeline_scratch[..], loop_to)
            }
            None => (&samples[..], None),
        };
        match &mut self.resampler {
            Some(resampler) => {
                resampler.process(filtered, resampler_scratch);
                // An intentional loop jump resets the filter on the next
                // revision. Emit its lookahead before the audible marker;
                // ordinary gapless packets keep their rational phase/history.
                if loop_to.is_some() {
                    resampler.finish(resampler_scratch);
                }
                (resampler_scratch.clone(), loop_to)
            }
            None => (pipeline_scratch.clone(), loop_to),
        }
    }

    fn reset(&mut self) {
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        if let Some(pipeline) = &mut self.pipeline {
            pipeline.reset_buffers();
        }
    }
}

pub struct OutputSink {
    host: cpal::HostId,
    format: AudioFormat,
    output: Arc<OutputControl>,
    ring: Arc<SampleRing>,
    output_rate: u32,
    processing: Arc<Mutex<AudioProcessing>>,
    /// Per-packet staging reused across writes. The final clone deliberately
    /// gives the ring a tight allocation: its budget counts audible samples,
    /// not capacity, so moving decoder-sized scratch would inflate retained
    /// memory at high rates or with sparse cuts. Backpressure never holds
    /// processing; only the player thread touches these buffers.
    pipeline_scratch: Vec<f32>,
    resampler_scratch: Vec<f32>,
    /// The customization revision [`AudioSignal::Output`] was last sent for.
    /// One signal per revision is all the engine needs — it asks whether this
    /// load produced audio at all, not how much — and a revision changes on
    /// every load, seek and loop jump, so this re-arms wherever the engine
    /// starts caring about a new stretch of audio. Only `write` touches it.
    signalled_revision: Option<u64>,
    /// The revision [`AudioSignal::OutputReady`] was last sent for, on the
    /// same once-per-revision terms.
    ready_revision: Option<u64>,
    /// Pause can cancel native opening before the rate is known. Retain
    /// in-flight decoded packets unprocessed until the resumed open.
    deferred: VecDeque<(u64, AudioPacketPosition, AudioPacket)>,
}

/// Maps a u16 volume (librespot's 0..=65535 scale) to the audible gain used
/// by librespot's `Cubic(60)` volume control: `(0.1 + 0.9 * normalized)^3`.
/// Keep both endpoints explicit: cubic's 0.1 floor is useful for control
/// granularity, but transport zero must still be exact mute.
fn volume_to_gain(volume: u16) -> f32 {
    if volume == 0 {
        return 0.0;
    }
    if volume == u16::MAX {
        return 1.0;
    }

    let normalized = f64::from(volume) / f64::from(u16::MAX);
    let gain = (0.1 + 0.9 * normalized).powi(3);
    if gain.is_finite() {
        gain.clamp(0.0, 1.0) as f32
    } else {
        0.0
    }
}

/// Gain changes apply to retained and queued samples on the next callback.
pub fn set_sink_volume(volume: u16) {
    SINK_GAIN.store(volume_to_gain(volume).to_bits(), Ordering::Release);
}

/// Picks the stereo output config to open, in descending order of how little
/// conversion it forces on the audio.
///
/// Prefer native 44.1 kHz, then the device's default rate, with the requested
/// representation preferred at either rate. Use an actually supported sample
/// format; do not overwrite the advertised format after selecting a config.
fn select_output_config(
    device: &cpal::Device,
    sample_format: cpal::SampleFormat,
    default_config: &cpal::SupportedStreamConfig,
) -> Result<cpal::SupportedStreamConfig, OutputError> {
    let stereo: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_output_configs()?
        .filter(|c| c.channels() == NUM_CHANNELS as cpal::ChannelCount)
        .collect();

    let at_rate = |rate: cpal::SampleRate, want_format: bool| {
        stereo
            .iter()
            .filter(|c| !want_format || c.sample_format() == sample_format)
            .find_map(|c| c.clone().try_with_sample_rate(rate))
    };

    let native = cpal::SampleRate(SAMPLE_RATE);
    let device_rate = default_config.sample_rate();

    Ok(
        // 1. Native 44.1 kHz in the format librespot will feed: no rate
        //    conversion and no sample-format conversion.
        at_rate(native, true)
            // 2. Native 44.1 kHz in some other format. Still no resampling;
            //    cpal converts the sample type.
            .or_else(|| at_rate(native, false))
            // 3. The device's own rate in the requested format. The sink
            //    resamples (see `crate::resample`), which costs a little CPU
            //    but nothing audible.
            .or_else(|| at_rate(device_rate, true))
            // 4. The device's own rate in any format.
            .or_else(|| at_rate(device_rate, false))
            // 5. Whatever the device defaults to, which may not be stereo.
            .unwrap_or_else(|| default_config.clone()),
    )
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    ring: Arc<SampleRing>,
    status: Arc<OutputStatus>,
    attempt: u64,
) -> Result<cpal::Stream, OutputError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels() as usize;
    let mut source = LiveSource::new(ring);
    let errors = Arc::clone(&status);
    Ok(device.build_output_stream(
        &config.config(),
        move |data: &mut [T], _| {
            #[cfg(test)]
            status.callbacks.fetch_add(1, Ordering::Relaxed);
            let running = status.running.load(Ordering::Acquire)
                && status.attempt.load(Ordering::Acquire) == attempt;
            if running && !status.ready.load(Ordering::Relaxed) {
                status.ready.store(true, Ordering::Release);
            }
            source.render(data, channels, running,
                f32::from_bits(SINK_GAIN.load(Ordering::Acquire)));
        },
        move |error| {
            eprintln!("audio output failed: {error}");
            errors.fail_at_attempt(attempt, false);
        },
        None,
    )?)
}

fn create_stream(
    host: &cpal::Host,
    format: AudioFormat,
    ring: &Arc<SampleRing>,
    status: &Arc<OutputStatus>,
    attempt: u64,
) -> Result<(OutputStream, cpal::SupportedStreamConfig), OutputError> {
    let device = host.default_output_device().ok_or(OutputError::NoDeviceAvailable)?;
    let requested = match format {
        AudioFormat::F64 => cpal::SampleFormat::F64,
        AudioFormat::F32 => cpal::SampleFormat::F32,
        AudioFormat::S32 => cpal::SampleFormat::I32,
        AudioFormat::S24 | AudioFormat::S24_3 => cpal::SampleFormat::I24,
        AudioFormat::S16 => cpal::SampleFormat::I16,
    };
    let default = device.default_output_config()?;
    let preferred = select_output_config(&device, requested, &default)?;
    let build = |config: &cpal::SupportedStreamConfig| {
        macro_rules! build {
            ($sample:ty) => { build_stream::<$sample>(&device, config, Arc::clone(ring), Arc::clone(status), attempt) };
        }
        match config.sample_format() {
            cpal::SampleFormat::I8 => build!(i8),
            cpal::SampleFormat::I16 => build!(i16),
            cpal::SampleFormat::I24 => build!(cpal::I24),
            cpal::SampleFormat::I32 => build!(i32),
            cpal::SampleFormat::I64 => build!(i64),
            cpal::SampleFormat::U8 => build!(u8),
            cpal::SampleFormat::U16 => build!(u16),
            cpal::SampleFormat::U32 => build!(u32),
            cpal::SampleFormat::U64 => build!(u64),
            cpal::SampleFormat::F32 => build!(f32),
            cpal::SampleFormat::F64 => build!(f64),
            other => Err(OutputError::Other(format!("unsupported output sample format: {other}"))),
        }
    };
    match build(&preferred) {
        Ok(stream) => Ok((OutputStream(stream), preferred)),
        Err(_) if preferred != default => Ok((OutputStream(build(&default)?), default)),
        Err(error) => Err(error),
    }
}

/// The native opener may wedge in a driver. Its worker is deliberately detached:
/// timeout frees the player thread, and a late stream is dropped without playing.
fn open_with_deadline<T: Send + 'static>(
    open: impl FnOnce() -> Result<T, OutputError> + Send + 'static,
    timeout: Duration,
    on_pending: impl FnOnce(std::sync::mpsc::Sender<Result<T, OutputError>>),
) -> Result<T, (String, bool)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    on_pending(sender.clone());
    if let Ok(result) = receiver.try_recv() {
        return result.map_err(|error| (error.to_string(), false));
    }
    std::thread::Builder::new().name("audio-output-open".to_owned())
        .spawn(move || { let _ = sender.send(open()); })
        .map_err(|error| (format!("audio open worker failed: {error}"), false))?;
    match receiver.recv_timeout(timeout) {
        Ok(result) => result.map_err(|error| (error.to_string(), false)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) =>
            Err(("audio output device did not answer while opening".to_owned(), true)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) =>
            Err(("audio output worker terminated while opening".to_owned(), false)),
    }
}

/// Injectable construction boundary. Production constructs a lazy sink;
/// tests may still return a typed construction failure for recovery scenarios.
pub type SinkOpener = Arc<dyn Fn(AudioFormat) -> Result<Box<dyn Sink>, OutputError> + Send + Sync>;

/// The opener the engine uses in production: whatever output device Windows
/// currently calls the default, in the format the player feeds.
pub fn default_sink_opener() -> SinkOpener {
    Arc::new(|format| {
        let host = cpal::default_host();
        if host.default_output_device().is_none() {
            return Err(OutputError::NoDeviceAvailable);
        }
        Ok(Box::new(open(host, format)))
    })
}

/// Answers whether the machine currently has a default output device, and
/// nothing else.
///
/// One entry in this machine's device list, with no stream, mixer or player
/// thread behind it — the question a device probe asks before deciding that a
/// full construction is worth starting. Held next to [`SinkOpener`], and
/// injectable for the same reason: a machine with no output device is a state
/// the engine has to reach on demand, and no real hardware can be asked to
/// provide one.
pub type DevicePresence = Arc<dyn Fn() -> bool + Send + Sync>;

/// The presence check the engine uses in production: whether Windows has a
/// default output device at all right now.
/// Presence does not guarantee the device can open. Production discovers that
/// on first start and reports failure from write; the engine's injected opener
/// can also return a construction failure. This probe only answers presence.
pub fn default_device_presence() -> DevicePresence {
    Arc::new(|| cpal::default_host().default_output_device().is_some())
}

/// The sink librespot is handed when the device step has already failed.
///
/// It exists because the failure cannot travel back through the builder: the
/// player thread calls it and needs a `Sink` to store. Nothing is ever played
/// into it — `auth::create_playback` drops the player the moment it reads the
/// device error beside it — so every packet is discarded.
pub struct SilentSink;

impl Sink for SilentSink {
    fn write(&mut self, _position: AudioPacketPosition, _packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        Ok(())
    }
}

/// Constructs an unstarted sink. No device or stream is opened until `start`.
pub fn open(host: cpal::Host, format: AudioFormat) -> OutputSink {
    OutputSink {
        host: host.id(),
        format,
        output: Arc::new(OutputControl::default()),
        ring: SampleRing::new(write_ahead_samples(SAMPLE_RATE)),
        output_rate: SAMPLE_RATE,
        processing: Arc::new(Mutex::new(AudioProcessing {
            resampler: None,
            pipeline_revision: CUSTOMIZATION_REVISION.load(Ordering::Acquire).wrapping_sub(1),
            pipeline: None,
            speed: 1.0,
            output_rate: SAMPLE_RATE,
        })),
        pipeline_scratch: Vec::new(),
        resampler_scratch: Vec::new(),
        signalled_revision: None,
        ready_revision: None,
        deferred: VecDeque::new(),
    }
}

impl Drop for OutputSink {
    fn drop(&mut self) {
        self.output.pause();
        self.output.stream.lock().unwrap_or_else(PoisonError::into_inner).take();
        release_live(&LIVE_OUTPUT, &self.output, LIVE_OUTPUT_POISON_MSG);
        release_live(&LIVE_RING, &self.ring, LIVE_RING_POISON_MSG);
        release_live(
            &LIVE_PROCESSING,
            &self.processing,
            LIVE_PROCESSING_POISON_MSG,
        );
    }
}

impl Sink for OutputSink {
    fn start(&mut self) -> SinkResult<()> {
        let revision = COMMITTED_REVISION.load(Ordering::Acquire);
        self.output.status.revision.store(revision, Ordering::Release);
        self.output.status.running.store(true, Ordering::Release);
        self.output.status.ready.store(false, Ordering::Release);
        claim_live(&LIVE_OUTPUT, &self.output, LIVE_OUTPUT_POISON_MSG);
        claim_live(&LIVE_RING, &self.ring, LIVE_RING_POISON_MSG);
        claim_live(&LIVE_PROCESSING, &self.processing, LIVE_PROCESSING_POISON_MSG);
        let needs_open = self.output.stream.lock().unwrap_or_else(PoisonError::into_inner).is_none();
        if needs_open {
            let attempt = self.output.status.begin_attempt();
            let host = self.host;
            let format = self.format;
            let ring = Arc::clone(&self.ring);
            let status = Arc::clone(&self.output.status);
            let opened = open_with_deadline(move || {
                let host = cpal::host_from_id(host)
                    .map_err(|error| OutputError::Other(error.to_string()))?;
                create_stream(&host, format, &ring, &status, attempt)
            }, crate::auth::AUDIO_START_TIMEOUT, |sender| {
                *self.output.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
                if !self.output.status.running.load(Ordering::Acquire) {
                    self.output.pause();
                }
            });
            self.output.pending.lock().unwrap_or_else(PoisonError::into_inner).take();
            match opened {
                Ok((stream, config)) => {
                    let mut native = self.output.stream.lock().unwrap_or_else(PoisonError::into_inner);
                    if !self.output.status.running.load(Ordering::Acquire)
                        || self.output.status.attempt.load(Ordering::Acquire) != attempt
                        || !current_decoder_revision(revision)
                    {
                        return Ok(());
                    }
                    self.output_rate = config.sample_rate().0;
                    self.ring.capacity.store(write_ahead_samples(self.output_rate), Ordering::Relaxed);
                    let mut processing = self.processing.lock().unwrap_or_else(PoisonError::into_inner);
                    processing.output_rate = self.output_rate;
                    processing.resampler = Resampler::new(SAMPLE_RATE, self.output_rate, NUM_CHANNELS as u16);
                    eprintln!("audio output: {} Hz, {} channels, {:?}", self.output_rate, config.channels(), config.sample_format());
                    *native = Some(stream);
                    *self.output.error.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
                Err((error, blocked)) => {
                    self.output.status.abandon_attempt();
                    if !self.output.status.running.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    *self.output.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                    self.output.status.fail(blocked);
                    return Ok(());
                }
            }
        }
        // Pause may have arrived while native opening was in flight.
        if !self.output.status.running.load(Ordering::Acquire) {
            return Ok(());
        }
        self.ring.resume_at(Instant::now());
        let stream = self.output.stream.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(stream) = stream.as_ref() {
            if let Err(error) = stream.play() {
                self.output.fail(format_args!("output device playback failed: {error}"));
            }
        }
        drop(stream);
        // A pause can release the producer before the callback reached a loop
        // marker, and the decoder may have nothing left to write after it, so
        // the resumed stream's arrival there is reported from here.
        if !self.output.status.failed.load(Ordering::Acquire)
            && self.ring.settle(self.ring.lock(), Instant::now() + WRITE_DRAIN_TIMEOUT).is_err()
        {
            signal(AudioSignal::OutputStalled { revision });
        }
        let _ = self.drain_deferred();
        Ok(())
    }

    /// Stops callbacks and keeps the native stream. The decoder stops the sink
    /// inside every seek and at a loop pass, so dropping the stream here would
    /// reopen the device on each of them (see [`OutputControl::discard_if_paused`]).
    fn stop(&mut self) -> SinkResult<()> {
        self.output.pause();
        let retaining = self.ring.retaining_pause.load(Ordering::Acquire);
        self.ring.stop_processing(&self.processing);
        if !retaining {
            self.deferred.clear();
        }
        Ok(())
    }
    fn write(&mut self, position: AudioPacketPosition, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let revision = position.revision;
        if !current_decoder_revision(revision) {
            return Ok(());
        }
        let ring_generation = self.ring.generation.load(Ordering::Acquire);
        // librespot answers every write error with a pause it reports as an
        // ordinary one, so each error also tells the engine its cause: an
        // unplayable packet fails the track, a dead device the output.
        let unplayable = |message: String| {
            signal(AudioSignal::DecodeFailed { revision });
            SinkError::OnWrite(message)
        };
        let samples = packet.samples().map_err(|error| unplayable(error.to_string()))?;

        // A partial frame would shift the interleave for everything after it
        // and swap the channels, so refuse it rather than corrupt the stream.
        if !samples.len().is_multiple_of(NUM_CHANNELS as usize) {
            return Err(unplayable(format!(
                "decoder produced a partial frame: {} samples is not a multiple of {NUM_CHANNELS}",
                samples.len()
            )));
        }

        if self.output.status.failed.load(Ordering::Acquire) {
            signal(AudioSignal::OutputStalled { revision });
            let error = self.output.error.lock().unwrap_or_else(PoisonError::into_inner)
                .clone().unwrap_or_else(|| "audio output device failed".to_owned());
            return Err(SinkError::OnWrite(error));
        }
        if !self.output.status.running.load(Ordering::Acquire)
            && self.output.stream.lock().unwrap_or_else(PoisonError::into_inner).is_none()
        {
            // Held for the resumed open; the replayed write reports it then.
            self.deferred.push_back((ring_generation, position, packet));
            return Ok(());
        }
        // Once the packet is accepted, and before any processing: the question
        // this answers is about the decoder, not about what the edit pipeline
        // does with its output — a packet the cuts remove entirely still
        // proves the track decodes.
        if self.signalled_revision != Some(revision) {
            self.signalled_revision = Some(revision);
            signal(AudioSignal::Output { revision });
        }
        if self.ready_revision != Some(revision)
            && self.output.status.ready.load(Ordering::Acquire)
        {
            self.ready_revision = Some(revision);
            signal(AudioSignal::OutputReady { revision });
        }
        let samples_f32 = converter.f64_to_f32(samples);

        let processing = Arc::clone(&self.processing);
        let mut processing = processing.lock().unwrap_or_else(PoisonError::into_inner);
        if !processing.synchronize_pipeline(revision) {
            return Ok(());
        }
        self.pipeline_scratch.clear();
        if let Some(speed) = processing.synchronize_speed(&mut self.pipeline_scratch) {
            // The retiring stretcher hands its lookahead to exact 1x, so these
            // samples follow the marker. Never wait with processing locked.
            self.resampler_scratch.clear();
            match &mut processing.resampler {
                Some(resampler) => {
                    resampler.process(&self.pipeline_scratch, &mut self.resampler_scratch);
                }
                None => std::mem::swap(&mut self.pipeline_scratch, &mut self.resampler_scratch),
            }
            if !self.resampler_scratch.is_empty() {
                let tail = self.resampler_scratch.clone();
                drop(processing);
                self.queue(ring_generation, revision, tail, None, speed)?;
                processing = self.processing.lock().unwrap_or_else(PoisonError::into_inner);
            }
        }

        let (queued, loop_to_ms) = processing.render_packet(
            samples_f32, position.source_frame, &mut self.pipeline_scratch, &mut self.resampler_scratch,
        );
        let speed = processing.speed;
        drop(processing);

        // Do not pace an empty customized packet. The ring's queued audible
        // samples already provide normal backpressure; sleeping for removed
        // source frames makes LiveSource emit underrun silence for the cut's
        // original duration, turning a cut into a mute.
        if queued.is_empty() && loop_to_ms.is_none() {
            return Ok(());
        }

        self.queue(ring_generation, revision, queued, loop_to_ms, speed)
    }
}

impl OutputSink {
    fn drain_deferred(&mut self) -> SinkResult<()> {
        for _ in 0..self.deferred.len() {
            if !self.output.status.running.load(Ordering::Acquire) {
                break;
            }
            let Some((generation, position, packet)) = self.deferred.pop_front() else { break; };
            if generation != self.ring.generation.load(Ordering::Acquire) {
                continue;
            }
            let remaining = self.deferred.len();
            // Converter's f32 path has no dither/config state.
            self.write(position, packet, &mut Converter::new(None))?;
            if self.deferred.len() > remaining {
                // Pause raced this write and retained it again. Restore its
                // place ahead of the still-unprocessed packets.
                self.deferred.rotate_right(1);
                break;
            }
        }
        Ok(())
    }

    /// Hands one finished packet to the ring; the shared backpressure tail of
    /// every write path. The wait is bounded so a dead audio thread cannot
    /// wedge the player thread forever: after WRITE_DRAIN_TIMEOUT the write
    /// fails with a normal sink error and librespot pauses playback (its
    /// `handle_packet` error path), which stops further writes and leaves the
    /// engine alive and recoverable.
    fn queue(
        &mut self,
        ring_generation: u64,
        revision: u64,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        speed: f32,
    ) -> SinkResult<()> {
        let queued = self.ring.push_timed_at_generation(
            ring_generation,
            samples,
            loop_to_ms,
            loop_to_ms.map_or(0, |_| revision),
            WRITE_DRAIN_TIMEOUT,
            Some((revision, speed, self.output_rate)),
        );
        if queued.is_err() {
            // The only way this wait expires is that nothing consumed the ring
            // for the whole timeout — `stop()` clears it, which returns `Ok` —
            // so the device that was taking audio is not taking it any more.
            // The decode side cannot act on that on its own, and librespot only
            // ever reports the resulting write error as a pause.
            signal(AudioSignal::OutputStalled { revision });
            return Err(SinkError::OnWrite(
                "audio output stalled: device is not draining".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test here models the reference rig: a 48 kHz device, which is what
    /// the ring is sized for once the resampler has run.
    const OUT_RATE: u32 = 48_000;
    const TEST_RING_CAPACITY: usize =
        OUT_RATE as usize * NUM_CHANNELS as usize * WRITE_AHEAD_MS / 1000;

    fn test_ring() -> Arc<SampleRing> {
        SampleRing::new(TEST_RING_CAPACITY)
    }

    fn packet(samples: usize) -> Vec<f32> {
        vec![1.0; samples]
    }

    fn configured(edit: Option<TrackEdit>, speed: f32, position_ms: u32, pass: u32) -> u64 {
        let revision = configure_customization_at_loop_pass(edit, speed, position_ms, pass);
        (decoder_transition(revision).commit)();
        revision
    }

    fn configured_natural(edit: Option<TrackEdit>, speed: f32, position_ms: u32) -> u64 {
        let revision = configure_customization_after_natural_boundary(edit, speed, position_ms);
        (decoder_transition(revision).commit)();
        revision
    }

    fn packet_position(source_frame: u64) -> AudioPacketPosition {
        AudioPacketPosition {
            source_frame,
            position_ms: (source_frame * 1_000 / u64::from(SAMPLE_RATE)) as u32,
            revision: COMMITTED_REVISION.load(Ordering::Acquire),
            skipped: false,
        }
    }

    #[test]
    fn an_inflight_decoder_read_cannot_consume_or_report_a_staged_target() {
        use librespot_playback::decoder::{AudioDecoder, DecoderError, DecoderResult};
        use renderer_engine::protocol::TimeRange;
        struct BlockedDecoder {
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl AudioDecoder for BlockedDecoder {
            fn seek(&mut self, position_ms: u32) -> Result<u32, DecoderError> {
                Ok(position_ms)
            }
            fn next_packet(&mut self) -> DecoderResult<Option<(AudioPacketPosition, AudioPacket)>> {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
                Ok(Some((AudioPacketPosition {
                    position_ms: 0, source_frame: 0, revision: 0, skipped: false,
                }, AudioPacket::Samples(vec![0.25; 882]))))
            }
        }

        let _guard = customization_guard();
        let old_revision = configured(None, 1.0, 0, 1);
        let (signals, mut receiver) = mpsc::unbounded_channel();
        install_signal_sender(signals);
        let mut sink = open(cpal::default_host(), AudioFormat::F32);
        claim_live(&LIVE_RING, &sink.ring, LIVE_RING_POISON_MSG);
        claim_live(&LIVE_PROCESSING, &sink.processing, LIVE_PROCESSING_POISON_MSG);
        claim_live(&LIVE_OUTPUT, &sink.output, LIVE_OUTPUT_POISON_MSG);
        let (entered, reading) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let (transition_tx, transition_rx) = std::sync::mpsc::channel::<DecoderTransition>();
        let worker = std::thread::spawn(move || {
            let mut decoder = BlockedDecoder { entered, release: blocked };
            let (mut position, pcm) = decoder.next_packet().unwrap().unwrap();
            // This is the owning player's revision, not a write-time global.
            position.revision = old_revision;
            sink.write(position.clone(), pcm, &mut Converter::new(None)).unwrap();
            sink.write(position, AudioPacket::Raw(vec![1]), &mut Converter::new(None)).unwrap();
            assert!(sink.deferred.is_empty(), "stale reads must not survive as deferred audio");
            assert_eq!(sink.signalled_revision, None);
            assert!(sink.processing.lock().unwrap_or_else(PoisonError::into_inner).pipeline.is_none());
            let transition = transition_rx.recv().unwrap();
            assert!((transition.commit)());
            sink.output.status.running.store(true, Ordering::Release);
            sink.write(AudioPacketPosition {
                position_ms: 100, source_frame: 4_410, revision: transition.revision, skipped: false,
            }, AudioPacket::Samples(vec![0.75; 2_646]), &mut Converter::new(None)).unwrap();
            assert_eq!(sink.ring.queued_samples(), 882,
                "only the target's final 10 ms survives its 20 ms cut");
            sink
        });
        reading.recv_timeout(Duration::from_secs(1)).unwrap();
        let target = configure_customization_at_loop_pass(Some(TrackEdit {
            cuts: vec![TimeRange { start_ms: 100, end_ms: 120 }], loop_range: None,
        }), 1.0, 100, 1);
        let transition = decoder_transition(target);
        release.send(()).unwrap();
        transition_tx.send(transition).unwrap();
        let sink = worker.join().unwrap();
        assert!(matches!(receiver.try_recv(), Ok(AudioSignal::Output { revision }) if revision == target));
        assert!(receiver.try_recv().is_err(), "old PCM and malformed data emit no target proof/failure");
        drop(sink);
    }

    #[test]
    fn superseded_transitions_cannot_commit_and_the_latest_transition_can() {
        let _guard = customization_guard();
        let old = configure_customization_at_loop_pass(None, 1.0, 100, 1);
        let old_transition = decoder_transition(old);
        assert!(!(old_transition.is_current)());
        let latest = configure_customization_at_loop_pass(None, 2.0, 200, 1);
        assert!(!(old_transition.commit)());
        assert!(!(old_transition.is_current)());
        assert!(customization_pending(latest));
        let latest_transition = decoder_transition(latest);
        assert!(!(latest_transition.is_current)());
        assert!((latest_transition.commit)());
        assert!((latest_transition.is_current)());
        assert!(current_decoder_revision(latest));
        assert!(!customization_pending(latest));
        let replacement = configure_customization_at_loop_pass(None, 1.0, 300, 1);
        assert!(!(latest_transition.is_current)());
        assert!((decoder_transition(replacement).commit)());
        assert!(!customization_pending(replacement));
    }

    #[test]
    fn native_open_timeout_does_not_join_a_blocked_driver_and_drops_late_output() {
        struct LateOutput(std::sync::mpsc::Sender<()>);
        impl Drop for LateOutput {
            fn drop(&mut self) { let _ = self.0.send(()); }
        }
        let (release, blocked) = std::sync::mpsc::channel();
        let (dropped, observed) = std::sync::mpsc::channel();
        let started = Instant::now();
        let result = open_with_deadline(move || {
            blocked.recv().unwrap();
            Ok(LateOutput(dropped))
        }, Duration::from_millis(20), |_| {});
        assert!(matches!(result, Err((_, true))), "a driver deadline is a blocked-device failure");
        assert!(started.elapsed() < Duration::from_secs(1), "timeout must not join the driver");
        release.send(()).unwrap();
        observed.recv_timeout(Duration::from_secs(1)).expect("late output must be dropped");
    }

    #[test]
    fn pause_cancels_open_wait_before_the_driver_returns() {
        let (release, blocked) = std::sync::mpsc::channel();
        let (result, observed) = std::sync::mpsc::channel();
        let (pending, cancellation) = std::sync::mpsc::channel();
        let (started, opening) = std::sync::mpsc::channel();
        let player = std::thread::spawn(move || {
            let opened = open_with_deadline(move || {
                started.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(42u32)
            }, Duration::from_secs(10), |sender| pending.send(sender).unwrap());
            result.send(opened).unwrap();
        });
        let cancellation = cancellation.recv().unwrap();
        opening.recv_timeout(Duration::from_secs(1)).expect("native worker entered opening");
        cancellation.send(Err(OutputError::Other("cancelled".to_owned()))).unwrap();
        assert_eq!(observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(("cancelled".to_owned(), false)));
        player.join().unwrap();
        release.send(()).unwrap();
    }

    #[test]
    fn cancelled_open_retains_raw_audio_for_the_resumed_device_rate_and_seek_discards_it() {
        let _guard = customization_guard();
        configured(None, 1.0, 0, 1);
        let mut sink = open(cpal::default_host(), AudioFormat::F32);
        let samples: Vec<f64> = (0..1024).map(|n| (n as f64 / 64.0).sin() * 0.25).collect();
        sink.ring.pause_at(Instant::now());
        for (index, chunk) in samples.chunks(512).enumerate() {
            sink.write(packet_position(index as u64 * 256), AudioPacket::Samples(chunk.to_vec()), &mut Converter::new(None)).unwrap();
        }
        sink.stop().unwrap();
        sink.output_rate = OUT_RATE;
        {
            let mut processing = sink.processing.lock().unwrap_or_else(PoisonError::into_inner);
            processing.output_rate = OUT_RATE;
            processing.resampler = Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16);
        }
        sink.output.status.running.store(true, Ordering::Release);
        sink.ring.resume_at(Instant::now());
        sink.drain_deferred().unwrap();
        let mut expected = Vec::new();
        Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16).unwrap()
            .process(&samples.iter().map(|sample| *sample as f32).collect::<Vec<_>>(), &mut expected);
        let mut source = LiveSource::new(Arc::clone(&sink.ring));
        let mut rendered = vec![0.0f32; expected.len()];
        source.render(&mut rendered, NUM_CHANNELS as usize, true, 1.0);
        assert_eq!(rendered, expected, "retained packets must use the resumed rate without drops/repeats");
        sink.output.status.running.store(false, Ordering::Release);
        sink.ring.pause_at(Instant::now());
        sink.write(packet_position(0), AudioPacket::Samples(samples), &mut Converter::new(None)).unwrap();
        sink.ring.clear();
        sink.output.status.running.store(true, Ordering::Release);
        sink.drain_deferred().unwrap();
        let mut silence = [1.0f32; 8];
        source.render(&mut silence, NUM_CHANNELS as usize, true, 1.0);
        assert_eq!(silence, [0.0; 8], "seek invalidates retained decoded and consumer audio");
    }

    #[test]
    fn native_failure_is_revision_owned_and_sent_once_without_a_decoder_write() {
        let _guard = customization_guard();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        install_signal_sender(sender);
        let status = OutputStatus::default();
        status.revision.store(u64::MAX - 1, Ordering::Release);
        status.attempt.store(42, Ordering::Release);
        status.fail_at_attempt(41, true);
        status.fail(false);
        status.fail(false);
        let mut failures = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            if let AudioSignal::OutputFailed { revision, blocked } = event {
                failures.push((revision, blocked));
            }
        }
        assert_eq!(failures, [(u64::MAX - 1, false)]);
        *AUDIO_SIGNAL_SENDER.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// A full ring must not block the player thread forever. This is the
    /// property that keeps a dead audio device recoverable.
    #[test]
    fn push_gives_up_when_the_ring_never_drains() {
        let ring = test_ring();
        while ring.queued_samples() < TEST_RING_CAPACITY {
            ring.push(packet(512), Duration::from_millis(50))
                .expect("space is available");
        }
        let started = Instant::now();
        assert!(
            ring.push(packet(512), Duration::from_millis(50)).is_err(),
            "a ring that never drains must fail the write, not block"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the failure must arrive at the deadline, not later"
        );
    }

    /// Popping must free space for the producer, otherwise playback deadlocks
    /// after the first ~150 ms.
    #[test]
    fn popping_frees_space_for_the_producer() {
        let ring = test_ring();
        while ring.queued_samples() < TEST_RING_CAPACITY {
            ring.push(packet(512), Duration::from_millis(50)).unwrap();
        }
        let popped = ring.pop().expect("a packet is queued");
        assert_eq!(popped.samples.len(), 512);
        ring.push(packet(512), Duration::from_millis(50))
            .expect("popping made room");
    }

    /// `stop()` must drop queued audio instantly, including the packet the
    /// audio thread is part-way through, so a pause is not trailed by up to a
    /// packet of stale sound.
    #[test]
    fn clear_drops_queued_audio_and_the_packet_in_flight() {
        let ring = test_ring();
        ring.push(packet(8), Duration::from_millis(50)).unwrap();
        ring.push(packet(8), Duration::from_millis(50)).unwrap();

        let mut source = LiveSource::new(ring.clone());
        assert_eq!(source.next(), Some(1.0));

        ring.clear();
        assert_eq!(ring.queued_samples(), 0, "queued packets are dropped");
        // The in-flight packet is dropped at the next frame boundary: one more
        // sample completes the current frame, then everything is silence.
        let tail: Vec<f32> = (0..16).filter_map(|_| source.next()).collect();
        assert!(
            tail[1..].iter().all(|s| *s == 0.0),
            "audio already handed to the source must stop within one frame: {tail:?}"
        );
    }

    #[test]
    fn a_packet_processed_before_stop_is_not_requeued_after_stop() {
        let ring = test_ring();
        let generation = ring.generation.load(Ordering::Acquire);
        ring.clear();

        ring.push_marked_at_generation(generation, packet(8), None, 0, Duration::from_millis(50))
            .unwrap();
        assert_eq!(ring.queued_samples(), 0);
    }

    /// Underrun silence must be a whole number of frames. A single stray
    /// sample would shift the interleave for the rest of the session and swap
    /// left with right.
    #[test]
    fn underrun_silence_is_frame_aligned() {
        let ring = test_ring();
        // Two frames of audio, then nothing.
        ring.push(vec![1.0, 2.0, 3.0, 4.0], Duration::from_millis(50))
            .unwrap();
        let mut source = LiveSource::new(ring.clone());
        assert_eq!(
            (0..4).filter_map(|_| source.next()).collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0, 4.0]
        );

        // Underrun: silence arrives, and when audio resumes it must land on
        // the left channel again.
        let silence: Vec<f32> = (0..6).filter_map(|_| source.next()).collect();
        assert!(silence.iter().all(|s| *s == 0.0));
        assert!(
            silence.len().is_multiple_of(NUM_CHANNELS as usize),
            "silence must be emitted in whole frames"
        );
        ring.push(vec![5.0, 6.0], Duration::from_millis(50))
            .unwrap();
        assert_eq!(
            source.next(),
            Some(5.0),
            "audio resumes on the left channel"
        );
        assert_eq!(source.next(), Some(6.0));
    }

    #[test]
    fn an_empty_callback_checks_the_ring_once_and_resumes_on_a_left_frame() {
        for channels in [1, 2, 6] {
            let ring = test_ring();
            let mut source = LiveSource::new(Arc::clone(&ring));
            let mut buffer = vec![1.0f32; 480 * channels];
            source.render(&mut buffer, channels, true, 1.0);
            assert!(buffer.iter().all(|sample| *sample == 0.0));
            assert_eq!(ring.empty_pops.load(Ordering::Relaxed), 1);

            ring.push(vec![0.2, 0.6], Duration::from_millis(50)).unwrap();
            source.render(&mut buffer, channels, true, 1.0);
            if channels == 1 {
                assert_eq!(buffer[0], 0.4);
            } else {
                assert_eq!(&buffer[..2], &[0.2, 0.6]);
            }
            assert!(buffer[channels..].iter().all(|sample| *sample == 0.0));
            assert_eq!(ring.empty_pops.load(Ordering::Relaxed), 2,
                "an underrun after real audio is still checked only once");

            ring.push(vec![0.3, 0.7, 0.4, 0.8], Duration::from_millis(50)).unwrap();
            let mut first = vec![0.0f32; channels];
            source.render(&mut first, channels, true, 1.0);
            ring.clear();
            ring.push(vec![0.5, 0.9], Duration::from_millis(50)).unwrap();
            source.render(&mut first, channels, true, 1.0);
            if channels == 1 {
                assert_eq!(first[0], 0.7);
            } else {
                assert_eq!(&first[..2], &[0.5, 0.9]);
            }
        }
    }

    /// Packet boundaries must neither alter resampled values nor add frames.
    #[test]
    fn device_rendering_is_transparent_at_the_device_rate() {
        const BLOCK_FRAMES: usize = 480;
        const SECONDS: usize = 15;
        let ring = test_ring();
        let mut source = LiveSource::new(ring.clone());

        let mut resampler =
            Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16).expect("rates differ");

        // Packet sizes cycle through what symphonia hands librespot for
        // Spotify's Ogg Vorbis (blocksizes 256/2048 => 128/576/1024 frames).
        let sizes = [256usize, 2048, 1152, 2048, 256, 1024, 2048, 512];
        let total_samples = SECONDS * SAMPLE_RATE as usize * NUM_CHANNELS as usize;

        // A ramp exposes dropped or repeated samples that DC would hide.
        let mut phase = 0usize;
        let mut source_sample = || {
            phase += 1;
            (phase % 1024) as f32 / 1024.0 - 0.5
        };

        let mut fed_samples = 0usize;
        let mut next_size = 0usize;
        let mut frame = 0usize;
        let mut queued_total = 0usize;
        let mut rendered_values: Vec<f32> = Vec::new();
        let mut expected_values: Vec<f32> = Vec::new();
        let (mut first, mut last) = (None, None);
        let mut silent_blocks = 0usize;
        let mut interior_silent_frames = 0usize;
        let mut block = [0.0f32; BLOCK_FRAMES * NUM_CHANNELS as usize];

        loop {
            while fed_samples < total_samples && ring.queued_samples() < TEST_RING_CAPACITY {
                let n = sizes[next_size % sizes.len()].min(total_samples - fed_samples);
                next_size += 1;
                let input: Vec<f32> = (0..n).map(|_| source_sample()).collect();
                let mut resampled = Vec::new();
                resampler.process(&input, &mut resampled);
                fed_samples += n;
                if resampled.is_empty() {
                    continue;
                }
                queued_total += resampled.len();
                expected_values.extend_from_slice(&resampled);
                ring.push(resampled, Duration::from_millis(50)).unwrap();
            }
            let mut block_had_audio = false;
            source.render(&mut block, NUM_CHANNELS as usize, true, 1.0);
            for stereo in block.chunks_exact(NUM_CHANNELS as usize) {
                let [left, right] = [stereo[0], stereo[1]];
                if left != 0.0 || right != 0.0 {
                    first.get_or_insert(frame);
                    last = Some(frame);
                    block_had_audio = true;
                    rendered_values.push(left);
                    rendered_values.push(right);
                } else if first.is_some() {
                    interior_silent_frames += 1;
                }
                frame += 1;
            }
            // An empty ring is not an empty pipeline: the source still holds
            // the packet it is playing. The source never ends, so the only way
            // to know the tail has been rendered is to keep pulling until the
            // output goes and stays quiet. If the tail were genuinely dropped
            // rather than merely un-pulled, no amount of extra pulling would
            // recover it and the counts below would come up short.
            silent_blocks = if block_had_audio {
                0
            } else {
                silent_blocks + 1
            };
            if fed_samples == total_samples && silent_blocks >= 4 {
                break;
            }
        }
        // Silence counted after the last real frame is drain, not a gap.
        interior_silent_frames -= frame - 1 - last.expect("audio was rendered");

        let rendered = last.expect("audio was rendered") - first.expect("audio was rendered") + 1;
        let fed_frames = total_samples / NUM_CHANNELS as usize;
        let ideal = fed_frames * OUT_RATE as usize / SAMPLE_RATE as usize;

        assert_eq!(
            interior_silent_frames, 0,
            "the write-ahead budget must keep device callbacks fed without gaps"
        );
        assert_eq!(
            rendered * NUM_CHANNELS as usize,
            queued_total,
            "the device must render every sample queued and no others"
        );
        assert_eq!(
            rendered_values, expected_values,
            "device-rate f32 output must not alter sample values"
        );
        // The only permitted shortfall is the filter look-ahead still holding
        // the last few input frames — a fixed handful, not a growing fraction.
        let shortfall = ideal - rendered;
        assert!(
            shortfall <= 64,
            "15 s produced {rendered} frames against an ideal {ideal}: the \
             conversion must be exactly rational, not merely close"
        );
    }

    #[test]
    fn paused_callbacks_preserve_the_cursor_and_queued_audio() {
        let ring = test_ring();
        ring.push(vec![0.1, -0.1, 0.2, -0.2, 0.3, -0.3], Duration::from_millis(50)).unwrap();
        ring.push(vec![0.4, -0.4], Duration::from_millis(50)).unwrap();
        let mut source = LiveSource::new(ring.clone());
        let mut frame = [0.0f32; 2];
        source.render(&mut frame, 2, true, 1.0);
        assert_eq!(frame, [0.1, -0.1]);
        for _ in 0..8 {
            source.render(&mut frame, 2, false, 1.0);
            assert_eq!(frame, [0.0; 2]);
        }
        let mut resumed = [0.0f32; 6];
        source.render(&mut resumed, 2, true, 1.0);
        assert_eq!(resumed, [0.2, -0.2, 0.3, -0.3, 0.4, -0.4]);
    }

    #[test]
    fn device_channel_layout_and_gain_do_not_shift_stereo_frames() {
        let ring = test_ring();
        ring.push(vec![0.2, 0.6, -0.4, -0.8, 0.5, -0.5], Duration::from_millis(50)).unwrap();
        let mut source = LiveSource::new(ring);
        let mut mono = [0.0f32; 1];
        source.render(&mut mono, 1, true, 0.5);
        assert!((mono[0] - 0.2).abs() < f32::EPSILON);
        let mut surround = [1.0f32; 6];
        source.render(&mut surround, 6, true, 0.5);
        assert_eq!(surround, [-0.2, -0.4, 0.0, 0.0, 0.0, 0.0]);
        let mut stereo = [0i16; 2];
        source.render(&mut stereo, 2, true, 1.0);
        assert_eq!(stereo, [16384, -16384]);
    }

    #[test]
    fn device_failure_surfaces_on_write_without_fallible_stop() {
        let _guard = customization_guard();
        configured(None, 1.0, 0, 1);
        let mut sink = open(cpal::default_host(), AudioFormat::F32);
        sink.output.fail("device disappeared");
        sink.stop().expect("librespot stop must remain infallible");
        let error = sink.write(packet_position(0), AudioPacket::Samples(vec![0.25, -0.25]), &mut Converter::new(None)).unwrap_err();
        assert!(matches!(error, SinkError::OnWrite(message) if message == "device disappeared"));
    }

    /// Run explicitly on a machine with an output device; no Spotify session
    /// needed. Counts the actual native callbacks, not silence/source polls.
    #[test]
    #[ignore = "requires a default output device"]
    fn native_stream_is_lazy_and_stops_callbacks_while_paused() {
        let _guard = customization_guard();
        configured(None, 1.0, 0, 1);
        let mut sink = open(cpal::default_host(), AudioFormat::F32);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sink.output.status.callbacks.load(Ordering::Relaxed), 0,
            "launch must not start device callbacks");
        sink.start().unwrap();
        assert!(!sink.output.status.failed.load(Ordering::Acquire));
        sink.queue(sink.ring.generation.load(Ordering::Acquire),
            customization_revision(), vec![0.0; sink.output_rate as usize / 10 * 2], None, 1.0).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(sink.output.status.callbacks.load(Ordering::Relaxed) > 0, "first play must run the device");
        let retained_attempt = sink.output.status.attempt.load(Ordering::Acquire);
        pause_output();
        sink.stop().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let paused = sink.output.status.callbacks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sink.output.status.callbacks.load(Ordering::Relaxed), paused,
            "paused device must not issue callbacks");
        sink.start().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(sink.output.status.callbacks.load(Ordering::Relaxed) > paused, "resume must run the retained device");
        assert_eq!(sink.output.status.attempt.load(Ordering::Acquire), retained_attempt,
            "ordinary pause/resume retains the native submitted buffer");
        pause_output();
        sink.stop().unwrap();
        let opened = sink.output.status.attempt.load(Ordering::Acquire);
        let seek_revision = configured(None, 1.0, 5_000, 1);
        sink.start().unwrap();
        assert!(!sink.output.status.failed.load(Ordering::Acquire));
        assert!(sink.output.status.attempt.load(Ordering::Acquire) > opened,
            "a paused seek must reopen output on resume");
        sink.queue(sink.ring.generation.load(Ordering::Acquire),
            seek_revision, vec![0.0; sink.output_rate as usize / 10 * 2], None, 1.0).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(sink.output.status.callbacks.load(Ordering::Relaxed) > paused,
            "the reopened output must run");
        sink.ring.clear();
        sink.stop().unwrap();
        let stopped = sink.output.status.callbacks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sink.output.status.callbacks.load(Ordering::Relaxed), stopped);
        eprintln!("native lifecycle: idle=0, played={paused}, paused={paused}, resumed={stopped}, stopped={stopped}");
    }

    /// Uses only silent PCM on the default device. A seek while playing is the
    /// decoder's stop → seek → start, which must keep the native stream; the
    /// natural end of the queue must leave a tail that drains, after which the
    /// engine's stop silences the callbacks for good.
    #[test]
    #[ignore = "requires a default output device"]
    fn native_stream_survives_a_playing_seek_and_stops_after_the_queue_drains() {
        let _guard = customization_guard();
        let revision = configured(None, 1.0, 0, 1);
        let mut sink = open(cpal::default_host(), AudioFormat::F32);
        sink.start().unwrap();
        assert!(!sink.output.status.failed.load(Ordering::Acquire));
        let opened = sink.output.status.attempt.load(Ordering::Acquire);
        let tenth = sink.output_rate as usize / 10 * NUM_CHANNELS as usize;
        sink.queue(sink.ring.generation.load(Ordering::Acquire), revision,
            vec![0.0; 2 * tenth], None, 1.0).unwrap();
        std::thread::sleep(Duration::from_millis(100));

        configured(None, 1.0, 30_000, 1);
        assert!(sink.output.stream.lock().unwrap_or_else(PoisonError::into_inner).is_some(),
            "a playing seek keeps the native stream");
        sink.stop().unwrap();
        // A second seek lands while librespot is still between the first
        // one's stop and start: not a user pause either.
        let seek = configured(None, 1.0, 40_000, 1);
        sink.start().unwrap();
        assert_eq!(sink.output.status.attempt.load(Ordering::Acquire), opened,
            "quick seeks while playing must not reopen the device");
        assert!(sink.output.stream.lock().unwrap_or_else(PoisonError::into_inner).is_some());
        let before = sink.output.status.callbacks.load(Ordering::Relaxed);
        sink.queue(sink.ring.generation.load(Ordering::Acquire), seek,
            vec![0.0; tenth], None, 1.0).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(sink.output.status.callbacks.load(Ordering::Relaxed) > before,
            "the kept stream runs after the seek");

        finish_natural_boundary().unwrap();
        finish_output_tail().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while output_tail_remaining().is_some() {
            assert!(Instant::now() < deadline, "the queued tail must drain");
            std::thread::sleep(Duration::from_millis(10));
        }
        // What the engine's drain stop amounts to at the sink.
        sink.stop().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let stopped = sink.output.status.callbacks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sink.output.status.callbacks.load(Ordering::Relaxed), stopped,
            "a drained queue must not keep the device calling back");
    }

    #[test]
    fn the_tail_is_the_audible_remainder_plus_the_queue() {
        let ring = test_ring();
        let generation = ring.generation.load(Ordering::Acquire);
        let frames = OUT_RATE as usize / 10;
        for _ in 0..2 {
            ring.push_timed_at_generation(generation, packet(frames * NUM_CHANNELS as usize),
                None, 0, Duration::from_millis(50), Some((3, 1.0, OUT_RATE))).unwrap();
        }
        let remaining_ms = |at| ring.tail_remaining_at(at).map(|tail| tail.as_secs_f64() * 1_000.0);
        let close = |actual: Option<f64>, expected: f64| {
            actual.is_some_and(|actual| (actual - expected).abs() < 0.01)
        };
        let at = Instant::now();
        assert!(close(remaining_ms(at), 200.0), "nothing audible yet: the whole queue");
        let playing = ring.lock().packets.pop_front().unwrap();
        ring.lock().audible = Some(AudibleClock { timing: playing.timing.unwrap(), at });
        assert!(close(remaining_ms(at + Duration::from_millis(40)), 160.0));
        ring.lock().packets.clear();
        assert!(close(remaining_ms(at + Duration::from_millis(60)), 40.0));
        assert_eq!(remaining_ms(at + Duration::from_millis(100)), None,
            "a played-out packet with nothing behind it is a drained output");
    }

    /// `configure_customization` owns process-wide statics, so the tests that
    /// drive it take turns rather than racing each other's flag and ring.
    static CUSTOMIZATION_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn customization_guard() -> std::sync::MutexGuard<'static, ()> {
        CUSTOMIZATION_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn live_ring() -> Option<Arc<SampleRing>> {
        LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()
    }

    /// Two sinks exist at once whenever a player is replaced while another is
    /// still playing (a preserved-playback reconnect, a normalisation rebuild).
    /// The registry has to follow the audible one through that overlap in both
    /// orders, and the retiring sink must never take its replacement's entry
    /// with it: an entry cleared by the wrong sink stays empty for the rest of
    /// the process, so volume changes, seek discontinuities and the natural
    /// boundary flush all silently stop reaching the audio that is playing.
    #[test]
    fn the_live_pipeline_follows_the_audible_sink_through_a_handover() {
        let _guard = customization_guard();
        let old = test_ring();
        claim_live(&LIVE_RING, &old, LIVE_RING_POISON_MSG);

        // Replacement becomes audible first, incumbent is torn down after.
        let new = test_ring();
        claim_live(&LIVE_RING, &new, LIVE_RING_POISON_MSG);
        release_live(&LIVE_RING, &old, LIVE_RING_POISON_MSG);
        assert!(
            live_ring().is_some_and(|ring| Arc::ptr_eq(&ring, &new)),
            "the retired sink deregistered its replacement"
        );

        // Incumbent is torn down first, replacement becomes audible after.
        let newest = test_ring();
        release_live(&LIVE_RING, &new, LIVE_RING_POISON_MSG);
        assert!(live_ring().is_none());
        claim_live(&LIVE_RING, &newest, LIVE_RING_POISON_MSG);
        assert!(live_ring().is_some_and(|ring| Arc::ptr_eq(&ring, &newest)));

        release_live(&LIVE_RING, &newest, LIVE_RING_POISON_MSG);
        assert!(live_ring().is_none());
    }

    /// A player built for a preference change that is superseded before it is
    /// installed is constructed, never started, and dropped. Because the claim
    /// happens at `start`, that sink never owned the pipeline, and its drop
    /// leaves the audible one exactly where it was.
    #[test]
    fn a_sink_that_never_became_audible_leaves_the_live_pipeline_alone() {
        let _guard = customization_guard();
        let playing = test_ring();
        claim_live(&LIVE_RING, &playing, LIVE_RING_POISON_MSG);

        let discarded = test_ring();
        release_live(&LIVE_RING, &discarded, LIVE_RING_POISON_MSG);

        assert!(
            live_ring().is_some_and(|ring| Arc::ptr_eq(&ring, &playing)),
            "a discarded replacement must not unregister the playing pipeline"
        );
        release_live(&LIVE_RING, &playing, LIVE_RING_POISON_MSG);
    }

    #[test]
    fn exact_one_customization_returns_the_converter_packet_without_stretcher_or_copy() {
        let _guard = customization_guard();
        let revision = configured(Some(TrackEdit::default()), 1.0, 0, 1);
        let mut processing = AudioProcessing {
            resampler: None,
            pipeline_revision: revision.wrapping_sub(1),
            pipeline: None,
            speed: 1.0,
            output_rate: SAMPLE_RATE,
        };
        processing.synchronize_pipeline(revision);
        let samples = vec![0.0, -0.0, f32::MIN_POSITIVE, -0.125, 0.75, -1.0];
        let allocation = samples.as_ptr();
        let bits: Vec<_> = samples.iter().map(|sample| sample.to_bits()).collect();
        let mut pipeline_scratch = Vec::new();
        let mut resampler_scratch = Vec::new();
        let (output, loop_to) = processing.render_packet(samples, 0, &mut pipeline_scratch, &mut resampler_scratch);
        assert_eq!(output.as_ptr(), allocation);
        assert_eq!(output.iter().map(|sample| sample.to_bits()).collect::<Vec<_>>(), bits);
        assert_eq!(loop_to, None);
        assert!(processing.pipeline.is_none());
        assert_eq!(pipeline_scratch.capacity(), 0);
        assert_eq!(resampler_scratch.capacity(), 0);
    }

    #[test]
    fn resampled_loop_emits_the_complete_pass_before_its_audible_marker() {
        let _guard = customization_guard();
        let revision = 37;
        let config = PipelineConfig {
            edit: Some(TrackEdit {
                cuts: Vec::new(),
                loop_range: Some(renderer_engine::protocol::LoopRange {
                    start_ms: 0, end_ms: 100, play_count: 2,
                }),
            }),
            speed: 1.0, position_ms: 0, loop_pass: 1,
        };
        let mut processing = AudioProcessing {
            resampler: Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16),
            pipeline_revision: revision, pipeline: Some(AudioPipeline::new(&config)),
            speed: 1.0, output_rate: OUT_RATE,
        };
        let samples = vec![0.25; 4_410 * NUM_CHANNELS as usize];
        let mut reference = Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16).unwrap();
        let mut expected = Vec::new();
        reference.process(&samples, &mut expected);
        reference.finish(&mut expected);
        let (output, marker) = processing.render_packet(samples, 0, &mut Vec::new(), &mut Vec::new());
        assert_eq!(output.len(), 4_800 * NUM_CHANNELS as usize,
            "the 34 lookahead frames cannot be discarded at the loop reset");
        assert_eq!(output, expected);
        assert_eq!(marker, Some(0));
        let ring = test_ring();
        let producer_ring = Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            producer_ring.push_marked(output, marker, revision, Duration::from_secs(1)).unwrap();
        });
        while ring.lock().next_boundary_id == 0 {
            std::thread::yield_now();
        }
        let mut source = LiveSource::new(Arc::clone(&ring));
        for sample in expected {
            assert_eq!(source.next(), Some(sample));
            assert_eq!(ring.lock().consumed_boundary_id, 0,
                "loop marker preceded the resampler tail");
        }
        source.next();
        assert_eq!(ring.lock().consumed_boundary_id, 1);
        producer.join().unwrap();
    }

    #[test]
    fn ordinary_gapless_tracks_keep_resampler_history_and_rational_phase() {
        let _guard = customization_guard();
        let samples: Vec<_> = (0..6_617 * NUM_CHANNELS as usize)
            .map(|index| (index as f32 * 0.03).sin()).collect();
        let mut reference = Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16).unwrap();
        let mut expected = Vec::new();
        reference.process(&samples, &mut expected);
        reference.finish(&mut expected);
        let split = 4_411 * NUM_CHANNELS as usize;
        // Engine tests drive the same process-wide customization without this
        // guard. A run one of their discontinuities lands in is repeated, not
        // judged: the sticky flag it leaves would reset the resampler here.
        for _ in 0..100 {
            let revision = configured(None, 1.0, 0, 1);
            let mut processing = AudioProcessing {
                resampler: Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16),
                pipeline_revision: revision.wrapping_sub(1), pipeline: None,
                speed: 1.0, output_rate: OUT_RATE,
            };
            processing.synchronize_pipeline(revision);
            let mut pipeline_scratch = Vec::new();
            let mut resampler_scratch = Vec::new();
            let (mut output, marker) = processing.render_packet(samples[..split].to_vec(), 0, &mut pipeline_scratch, &mut resampler_scratch);
            assert_eq!(marker, None);
            let natural = configured_natural(None, 1.0, 0);
            processing.synchronize_pipeline(natural);
            if natural != revision.wrapping_add(1) || customization_revision() != natural {
                continue;
            }
            let (tail, marker) = processing.render_packet(samples[split..].to_vec(), 0, &mut pipeline_scratch, &mut resampler_scratch);
            assert_eq!(marker, None);
            output.extend(tail);
            processing.resampler.as_mut().unwrap().finish(&mut output);
            assert_eq!(output, expected,
                "a natural boundary must sound exactly like one uninterrupted resampled stream");
            return;
        }
        panic!("every run was interleaved with another customization");
    }

    #[test]
    fn queued_audio_keeps_old_rate_until_the_new_packet_is_audible() {
        let ring = test_ring();
        let revision = 19;
        let generation = ring.generation.load(Ordering::Acquire);
        let frames = OUT_RATE as usize / 10;
        for speed in [1.0, 4.0] {
            ring.push_timed_at_generation(generation, packet(frames * NUM_CHANNELS as usize),
                None, 0, Duration::from_millis(50), Some((revision, speed, OUT_RATE))).unwrap();
        }
        let first = ring.lock().packets.pop_front().unwrap();
        let second = ring.lock().packets.pop_front().unwrap();
        let at = Instant::now();
        ring.lock().audible = Some(AudibleClock { timing: first.timing.unwrap(), at });
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(50)), Some(50.0));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(150)), Some(100.0));
        ring.lock().audible = Some(AudibleClock {
            timing: second.timing.unwrap(), at: at + Duration::from_millis(100),
        });
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(100)), Some(100.0));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(150)), Some(300.0));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(250)), Some(500.0));
        assert_eq!(ring.audible_elapsed_at(revision + 1, at), None);
        ring.clear();
        assert_eq!(ring.audible_elapsed_at(revision, at), None);
    }

    #[test]
    fn pause_retains_inflight_audio_and_freezes_source_time_until_resume() {
        let ring = test_ring();
        let samples = vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0];
        ring.push(samples.clone(), Duration::from_millis(50)).unwrap();
        ring.push(vec![4.0, -4.0], Duration::from_millis(50)).unwrap();
        let mut source = LiveSource::new(ring.clone());
        assert_eq!(source.next(), Some(1.0));
        assert_eq!(source.next(), Some(-1.0));
        let at = Instant::now();
        let revision = 31;
        ring.lock().audible = Some(AudibleClock {
            timing: PacketTiming { revision, start_ms: 13_000.0, duration_ms: 100.0, speed: 1.0 },
            at,
        });
        let processing = Mutex::new(AudioProcessing {
            resampler: None, pipeline_revision: revision,
            pipeline: Some(AudioPipeline::new(&PipelineConfig {
                edit: None, speed: 4.0, position_ms: 0, loop_pass: 1,
            })),
            speed: 4.0, output_rate: SAMPLE_RATE,
        });
        let mut pipeline_scratch = Vec::new();
        let mut resampler_scratch = Vec::new();
        let prefix = SAMPLE_RATE as usize / 50;
        let suffix = SAMPLE_RATE as usize * 2 / 25;
        let (first, _) = processing.lock().unwrap_or_else(PoisonError::into_inner).render_packet(vec![0.25; prefix * NUM_CHANNELS as usize], 0, &mut pipeline_scratch, &mut resampler_scratch);
        ring.pause_at(at + Duration::from_millis(50));
        ring.stop_processing(&processing);
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_secs(5)), Some(13_050.0));
        ring.resume_at(at + Duration::from_secs(5));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_secs(5)), Some(13_050.0));
        assert_eq!(ring.audible_elapsed_at(revision,
            at + Duration::from_secs(5) + Duration::from_millis(25)), Some(13_075.0));
        assert_eq!((0..6).map(|_| source.next().unwrap()).collect::<Vec<_>>(),
            vec![2.0, -2.0, 3.0, -3.0, 4.0, -4.0]);
        let (last, _) = processing.lock().unwrap_or_else(PoisonError::into_inner).render_packet(vec![0.25; suffix * NUM_CHANNELS as usize], 0, &mut pipeline_scratch, &mut resampler_scratch);
        let mut tail = Vec::new();
        processing.lock().unwrap_or_else(PoisonError::into_inner)
            .pipeline.as_mut().unwrap().finish(&mut tail);
        assert_eq!((first.len() + last.len() + tail.len()) / NUM_CHANNELS as usize,
            ((prefix + suffix) as f64 / 4.0).round() as usize,
            "pause must not lose the stretcher's un-emitted source lookahead");
        ring.clear();
        assert_eq!(ring.audible_elapsed_at(revision, at), None);
    }

    #[test]
    fn pause_releases_a_capacity_wait_without_losing_the_inflight_packet() {
        let ring = SampleRing::new(2);
        ring.push(vec![1.0, -1.0], Duration::from_millis(50)).unwrap();
        let producer = ring.clone();
        let (done, result) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            done.send(producer.push(vec![2.0, -2.0], Duration::from_secs(1))).unwrap();
        });
        ring.pause_at(Instant::now());
        assert_eq!(result.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
        writer.join().unwrap();
        assert_eq!(ring.pop().unwrap().samples, vec![1.0, -1.0]);
        assert_eq!(ring.pop().unwrap().samples, vec![2.0, -2.0]);
        // The intent a seek's device-buffer discard is decided on.
        assert!(ring.clear(), "a discontinuity reports the user pause it ends");
        assert!(!ring.clear(), "and nothing once it has ended");
    }

    /// Natural gapless setup must preserve the outgoing queue, while every
    /// discontinuous seek/config reset must discard it.
    #[test]
    fn natural_and_discontinuous_boundaries_have_distinct_queue_semantics() {
        let _guard = customization_guard();
        let ring = test_ring();
        *LIVE_RING.lock().expect(LIVE_RING_POISON_MSG) = Arc::downgrade(&ring);

        ring.push(packet(1024), Duration::from_millis(50)).unwrap();
        configured_natural(None, 1.0, 0);
        assert_eq!(
            ring.queued_samples(),
            1024,
            "a natural track boundary must leave the audible tail queued"
        );

        configured(None, 1.25, 0, 1);
        assert_eq!(
            ring.queued_samples(),
            0,
            "a configuration discontinuity must discard stale queued samples"
        );
    }

    #[test]
    fn natural_eof_flushes_wsola_tail_into_the_live_queue() {
        let _guard = customization_guard();
        let ring = test_ring();
        let speed = 0.5;
        let frames = SAMPLE_RATE as usize * 37 / 1_000;
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: None,
            speed,
            position_ms: 0,
            loop_pass: 1,
        });
        let input = vec![0.25; frames * NUM_CHANNELS as usize];
        let mut already_emitted = Vec::new();
        pipeline.process(0, &input, &mut already_emitted);
        let processing = Arc::new(Mutex::new(AudioProcessing {
            resampler: None,
            pipeline_revision: CUSTOMIZATION_REVISION.load(Ordering::Acquire),
            pipeline: Some(pipeline),
            speed,
            output_rate: SAMPLE_RATE,
        }));
        *LIVE_RING.lock().expect(LIVE_RING_POISON_MSG) = Arc::downgrade(&ring);
        *LIVE_PROCESSING.lock().expect(LIVE_PROCESSING_POISON_MSG) = Arc::downgrade(&processing);

        finish_natural_boundary().expect("the output queue has room");
        let expected_total =
            (frames as f64 / speed as f64).round() as usize * NUM_CHANNELS as usize;
        assert_eq!(
            ring.queued_samples(),
            expected_total - already_emitted.len(),
            "every delayed WSOLA sample must be queued before natural advance"
        );
    }

    /// A loop end that lands on a packet boundary produces a marker with no
    /// samples. `LiveSource` must still deliver it — and must not index past
    /// the end of the empty packet on the way.
    #[test]
    fn a_marker_without_samples_is_delivered_and_does_not_panic() {
        let _guard = customization_guard();
        let ring = test_ring();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        install_signal_sender(sender);
        let producer_ring = Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            producer_ring
                .push_marked(Vec::new(), Some(4_200), 0, Duration::from_secs(1))
                .unwrap();
        });
        while ring.lock().next_boundary_id == 0 {
            std::thread::yield_now();
        }

        let mut source = LiveSource::new(Arc::clone(&ring));
        // Pulling once drains the empty marker, acknowledges its backpressure,
        // and returns underrun silence without indexing the empty vector.
        assert_eq!(source.next(), Some(0.0));
        producer.join().unwrap();
        match receiver.try_recv() {
            Ok(AudioSignal::LoopBoundary {
                position_ms,
                revision: 0,
            }) => assert_eq!(position_ms, 4_200),
            other => panic!("expected the loop boundary, got {other:?}"),
        }
    }

    #[test]
    fn loop_marker_blocks_the_decoder_until_it_is_audible() {
        let _guard = customization_guard();
        let ring = test_ring();
        let producer_ring = Arc::clone(&ring);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            producer_ring
                .push_marked(Vec::new(), Some(900), 0, Duration::from_secs(1))
                .unwrap();
            done_tx.send(()).unwrap();
        });
        assert!(
            done_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "the decoder ran past a queued loop boundary"
        );
        while ring.lock().next_boundary_id == 0 {
            std::thread::yield_now();
        }

        let mut source = LiveSource::new(ring);
        source.next();
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("audible boundary releases decoder backpressure");
        producer.join().unwrap();
    }

    /// A pause releases the decoder before the callback reaches a queued loop
    /// marker. The marker must still be reported once the resumed callback
    /// plays through it, by the producer side, never by the callback.
    #[test]
    fn a_marker_outstanding_across_a_pause_is_reported_once_heard() {
        let _guard = customization_guard();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        install_signal_sender(sender);
        let mut boundaries = move || {
            let mut found = Vec::new();
            while let Ok(signal) = receiver.try_recv() {
                if let AudioSignal::LoopBoundary { position_ms, revision: 0xA11CE } = signal {
                    found.push(position_ms);
                }
            }
            found
        };
        let ring = test_ring();
        let producer_ring = Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            producer_ring.push_marked(packet(4), Some(900), 0xA11CE, Duration::from_secs(1)).unwrap();
        });
        while ring.lock().next_boundary_id == 0 {
            std::thread::yield_now();
        }
        ring.pause_at(Instant::now());
        producer.join().unwrap();
        assert!(boundaries().is_empty(), "nothing has heard the marker yet");

        ring.resume_at(Instant::now());
        let mut source = LiveSource::new(Arc::clone(&ring));
        for _ in 0..5 {
            source.next();
        }
        assert!(boundaries().is_empty(), "the callback itself sends nothing");
        ring.settle(ring.lock(), Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(boundaries(), [900]);
        ring.settle(ring.lock(), Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(boundaries().is_empty(), "a marker is reported once");
        *AUDIO_SIGNAL_SENDER.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The gain curve must mirror librespot's `Cubic(60)` volume
    /// control so switching volume from per-packet attenuation to the sink
    /// does not change the audible volume curve.
    #[test]
    fn volume_to_gain_matches_the_softmixer_cubic_curve() {
        let expected = |volume: u16| {
            let normalized = f64::from(volume) / f64::from(u16::MAX);
            (0.1 + 0.9 * normalized).powi(3) as f32
        };
        assert_eq!(volume_to_gain(0), 0.0, "mute is exactly zero");
        assert_eq!(volume_to_gain(u16::MAX), 1.0, "max volume is unity");

        let table = [
            (1u16, 0.0010004121),
            (655u16, 0.0012948577),
            (6553u16, 0.0068582564),
            (16384u16, 0.034329213),
            (32768u16, 0.16638123),
            (49151u16, 0.46547818),
            (58982u16, 0.7535881),
        ];
        for (volume, expected_gain) in table {
            let actual = volume_to_gain(volume);
            assert!(
                (actual - expected_gain).abs() <= 1e-6,
                "mapping mismatch at raw volume {volume}: {actual} != {expected_gain}"
            );
            assert!(
                (actual - expected(volume)).abs() <= 1e-6,
                "mapping formula mismatch at raw volume {volume}"
            );
        }
    }

    #[test]
    fn volume_to_gain_is_monotonic_and_bounded() {
        let mut previous = volume_to_gain(0);
        for volume in 1u16..=u16::MAX {
            let gain = volume_to_gain(volume);
            assert!(
                gain.is_finite(),
                "gain at raw volume {volume} is not finite"
            );
            assert!(
                (0.0..=1.0).contains(&gain),
                "gain at raw volume {volume} escaped [0, 1]: {gain}"
            );
            assert!(
                gain >= previous,
                "gain decreased at raw volume {volume}: {gain} < {previous}"
            );
            previous = gain;
        }
    }

}
