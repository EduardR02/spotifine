//! System media controls for Windows and macOS.
//!
//! A dedicated thread owns the `MediaControls`, fed by a channel from the state
//! consumer. Windows attaches SMTC to the main HWND; macOS attaches to the
//! application event loop without a window handle. Button presses are
//! forwarded to the selected output through the central playback router.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use crate::engine_client::EngineClient;
use crate::log;
use crate::types::PlaybackState;
use crate::playback_router::{Action, PlaybackRouter};
use tauri::{AppHandle, Manager};

/// Last-known playing flag, so the overlay's toggle button maps to the right
/// engine command even though its press arrives on a foreign thread.
static PLAYING: AtomicBool = AtomicBool::new(false);
static POSITION_MS: AtomicU64 = AtomicU64::new(0);
static DURATION_MS: AtomicU64 = AtomicU64::new(0);

/// Published only after the controls attached successfully; every update
/// helper below becomes a no-op until then, so a failed registration costs
/// nothing beyond one warning at startup.
static UPDATES: OnceLock<Sender<Update>> = OnceLock::new();

enum Update {
    /// A full engine state: track identity plus transport status.
    State {
        transport: Transport,
        metadata: Metadata,
    },
    /// Nothing playable (logged out, empty queue).
    Stopped,
}

/// A playhead and the clock it runs on, as the OS was told it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Transport {
    playing: bool,
    position_ms: u32,
    /// Playhead clock rate; Windows freezes buffering without changing intent.
    rate: f32,
}

impl Transport {
    fn from_state(state: &PlaybackState) -> Self {
        Self {
            playing: state.playing,
            position_ms: state.position_ms,
            rate: if cfg!(windows) && state.buffering { 0.0 } else { state.audible_playback_speed },
        }
    }
}

/// How far a reported playhead may sit from where the OS has extrapolated it
/// before it is worth correcting. The flyout shows whole seconds; any real
/// seek, skip or loop moves the playhead much further than this.
const TIMELINE_TOLERANCE_MS: f64 = 1_000.0;

/// What the media session was last told, and when.
struct Published {
    transport: Transport,
    at: Instant,
}

/// Publish transport changes against the declared playhead clock, separately
/// from command intent. Heartbeats do not publish OS state; full states carry
/// seeks, skips, buffering boundaries and speed changes.
fn timeline_changed(published: Option<&Published>, next: Transport, item_changed: bool, now: Instant) -> bool {
    let Some(published) = published else { return true };
    let last = published.transport;
    if item_changed || last.playing != next.playing || last.rate != next.rate {
        return true;
    }
    let expected = if last.playing {
        f64::from(last.position_ms) + now.saturating_duration_since(published.at).as_secs_f64() * 1_000.0 * f64::from(last.rate)
    } else {
        f64::from(last.position_ms)
    };
    (f64::from(next.position_ms) - expected).abs() > TIMELINE_TOLERANCE_MS
}

/// Rate and timeline are independent OS calls: a successful timeline must not
/// suppress retrying a failed rate. A confirmed rate change also reanchors the
/// position so recovery from a rate failure starts from the current playhead.
fn publish_transport<R, T>(
    previous: &mut Option<Published>,
    confirmed_rate: &mut Option<f32>,
    next: Transport,
    item_changed: bool,
    now: Instant,
    set_rate: impl FnOnce(f32) -> Result<(), R>,
    set_timeline: impl FnOnce(MediaPlayback) -> Result<(), T>,
) -> (Result<(), R>, Result<(), T>) {
    let mut rate_changed = false;
    let rate_result = if *confirmed_rate != Some(next.rate) {
        let result = set_rate(next.rate);
        if result.is_ok() {
            *confirmed_rate = Some(next.rate);
            rate_changed = true;
        }
        result
    } else { Ok(()) };
    let timeline_result = if timeline_changed(previous.as_ref(), next, item_changed || rate_changed, now) {
        let result = set_timeline(transport(next.playing, next.position_ms));
        *previous = result.is_ok().then_some(Published { transport: next, at: now });
        result
    } else { Ok(()) };
    (rate_result, timeline_result)
}

fn publish_stopped<E>(
    metadata: &mut Option<Metadata>,
    timeline: &mut Option<Published>,
    rate: &mut Option<f32>,
    publish: impl FnOnce(MediaPlayback) -> Result<(), E>,
) -> Result<(), E> {
    *metadata = None;
    *timeline = None;
    *rate = None;
    publish(MediaPlayback::Stopped)
}

#[derive(Debug, PartialEq, Eq)]
struct Metadata {
    duration_ms: u32,
    title: String,
    artist: String,
    album: String,
    cover: String,
}

impl Metadata {
    fn as_media(&self) -> MediaMetadata<'_> {
        MediaMetadata {
            title: Some(&self.title),
            artist: Some(&self.artist),
            album: Some(&self.album),
            cover_url: (!self.cover.is_empty()).then_some(self.cover.as_str()),
            duration: Some(Duration::from_millis(self.duration_ms as u64)),
        }
    }
}

fn publish_metadata<E>(
    previous: &mut Option<Metadata>,
    next: Metadata,
    publish: impl FnOnce(MediaMetadata<'_>) -> Result<(), E>,
) -> Result<(), E> {
    if previous.as_ref() != Some(&next) {
        publish(next.as_media())?;
        *previous = Some(next);
    }
    Ok(())
}

#[cfg(windows)]
fn publish_windows_metadata(
    hwnd: usize,
    controls: &mut MediaControls,
    previous: &mut Option<Metadata>,
    next: Metadata,
) -> Result<(), souvlaki::Error> {
    let remove_cover = next.cover.is_empty()
        && previous.as_ref().is_some_and(|metadata| !metadata.cover.is_empty());
    publish_metadata(previous, next, |metadata| {
        // Souvlaki skips SetThumbnail for None; it does not remove the old
        // artwork. Clear and retype before publishing the replacement fields.
        if remove_cover {
            clear_windows_metadata(hwnd)?;
        }
        controls.set_metadata(metadata)
    })
}

/// Attaches system media controls. Failures are warnings, never fatal: a
/// player without media keys still plays.
pub fn init(app: AppHandle, client: Arc<EngineClient>, hwnd: Option<usize>) {
    let (sender, receiver) = channel::<Update>();

    let result = std::thread::Builder::new()
        .name("media-controls".to_owned())
        .spawn(move || run_controls(app, client, hwnd, sender, receiver));
    if let Err(error) = result {
        log::warn(&format!("could not start the media-key thread: {error}"));
    }
}

fn run_controls(
    app: AppHandle,
    client: Arc<EngineClient>,
    hwnd: Option<usize>,
    sender: Sender<Update>,
    receiver: std::sync::mpsc::Receiver<Update>,
) {
    let config = PlatformConfig {
        dbus_name: "spotifine",
        display_name: "Spotifine",
        hwnd: hwnd.map(|value| value as *mut std::ffi::c_void),
    };
    let mut controls = match MediaControls::new(config) {
        Ok(controls) => controls,
        Err(error) => {
            log::warn(&format!("could not register media keys (SMTC): {error:?}"));
            return;
        }
    };

    let handler_app = app.clone();
    if let Err(error) = controls.attach(move |event| handle_event(event, &handler_app)) {
        log::warn(&format!("could not attach media-key handlers: {error:?}"));
        return;
    }
    #[cfg(windows)]
    if let Some(hwnd) = hwnd {
        if let Err(error) = disable_unsupported_seek_buttons(hwnd) {
            log::warn(&format!(
                "could not disable unsupported fast-forward/rewind controls: {error}"
            ));
        }
    }

    // Publish the sender before asking for status. Otherwise a fast response
    // can arrive in the gap and be dropped, leaving SMTC stale until the next
    // full engine state.
    if UPDATES.set(sender).is_err() {
        return;
    }
    log::info("media keys registered (SMTC)");

    let refresh = client.clone();
    tauri::async_runtime::spawn(async move {
        let _ = refresh.request("status", serde_json::Value::Null).await;
    });

    // Deduplicate the actual OS metadata tuple, not the URI: preview edits,
    // restores and refreshed titles/artwork can change it for the same song.
    // Position heartbeats never reach this thread: the OS runs the timeline.
    let mut last_metadata = None;
    let mut last_transport: Option<Published> = None;
    let mut confirmed_rate = None;
    for update in receiver {
        let result = match update {
            Update::State {
                transport: next,
                metadata,
            } => {
                let item_changed = last_metadata.as_ref() != Some(&metadata);
                #[cfg(windows)]
                let publication = publish_windows_metadata(
                    hwnd.expect("Windows media controls require a window"),
                    &mut controls, &mut last_metadata, metadata,
                );
                #[cfg(not(windows))]
                let publication = publish_metadata(&mut last_metadata, metadata,
                    |metadata| controls.set_metadata(metadata));
                if let Err(error) = publication {
                    log::warn(&format!("could not update media metadata: {error:?}"));
                }
                let now = Instant::now();
                let (rate_result, timeline_result) = publish_transport(
                    &mut last_transport, &mut confirmed_rate, next, item_changed, now,
                    |rate| {
                        #[cfg(windows)]
                        { set_windows_playback_rate(hwnd.expect("Windows media controls require a window"), rate) }
                        #[cfg(not(windows))]
                        { let _ = rate; Ok::<_, ()>(()) }
                    },
                    |playback| controls.set_playback(playback),
                );
                if let Err(error) = rate_result {
                    log::warn(&format!("could not update the media playback rate: {error:?}"));
                }
                timeline_result
            }
            Update::Stopped => {
                // The producer owns current intent. A queued stop is historical
                // and must never overwrite a newer playing state.
                #[cfg(windows)]
                if let Some(hwnd) = hwnd {
                    if let Err(error) = clear_windows_metadata(hwnd) {
                        log::warn(&format!("could not clear media metadata: {error}"));
                    }
                }
                #[cfg(not(windows))]
                if let Err(error) = controls.set_metadata(MediaMetadata::default()) {
                    log::warn(&format!("could not clear media metadata: {error:?}"));
                }
                publish_stopped(&mut last_metadata, &mut last_transport, &mut confirmed_rate,
                    |playback| controls.set_playback(playback))
            }
        };
        if let Err(error) = result {
            log::warn(&format!("could not update media session: {error:?}"));
        }
    }
}

fn transport(playing: bool, position_ms: u32) -> MediaPlayback {
    let progress = Some(MediaPosition(Duration::from_millis(position_ms as u64)));
    if playing {
        MediaPlayback::Playing { progress }
    } else {
        MediaPlayback::Paused { progress }
    }
}

fn handle_event(event: MediaControlEvent, app: &AppHandle) {
    use MediaControlEvent as E;
    let action = match event {
        E::Play => Action::Play,
        E::Pause | E::Stop => Action::Pause,
        E::Toggle => if PLAYING.load(Ordering::Relaxed) { Action::Pause } else { Action::Play },
        E::Next => Action::Next,
        E::Previous => Action::Previous,
        E::SetPosition(position) => Action::Seek(absolute_seek_target(duration_ms(position.0))),
        E::SeekBy(direction, amount) => Action::Seek(relative_seek_target(direction, duration_ms(amount))),
        // Indeterminate FF/RW has no honest target; disabled at attach.
        E::Seek(_) => return,
        _ => return,
    };
    let router = app.state::<Arc<PlaybackRouter>>().inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = router.run(&app, action).await;
    });
}

fn duration_ms(duration: Duration) -> u32 {
    duration.as_millis().min(u32::MAX as u128) as u32
}

fn absolute_seek_target(position_ms: u32) -> u32 {
    let duration = DURATION_MS.load(Ordering::Relaxed).min(u32::MAX as u64) as u32;
    if duration == 0 {
        position_ms
    } else {
        position_ms.min(duration)
    }
}

fn relative_seek_target(direction: SeekDirection, amount_ms: u32) -> u32 {
    // Reserve the playhead before dispatching the asynchronous seek. SMTC can
    // deliver several relative seeks before the engine emits a new state; a
    // plain load would make each request start from the same old position.
    let mut position = POSITION_MS.load(Ordering::Relaxed);
    loop {
        let position_ms = position.min(u32::MAX as u64) as u32;
        let duration = DURATION_MS.load(Ordering::Relaxed).min(u32::MAX as u64) as u32;
        let target = match direction {
            SeekDirection::Forward => position_ms.saturating_add(amount_ms),
            SeekDirection::Backward => position_ms.saturating_sub(amount_ms),
        };
        let target = if duration == 0 {
            target
        } else {
            target.min(duration)
        };
        match POSITION_MS.compare_exchange_weak(
            position,
            target as u64,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return target,
            Err(current) => position = current,
        }
    }
}

#[cfg(windows)]
fn windows_controls(
    hwnd: usize,
) -> windows::core::Result<windows::Media::SystemMediaTransportControls> {
    use windows::Media::SystemMediaTransportControls;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;

    let interop = windows::core::factory::<
        SystemMediaTransportControls,
        ISystemMediaTransportControlsInterop,
    >()?;
    unsafe { interop.GetForWindow(HWND(hwnd as isize)) }
}

/// Publish the declared progress rate independently of Playing/Paused intent.
/// Buffering declares a held playhead (zero); resuming restores audible speed.
#[cfg(windows)]
fn set_windows_playback_rate(hwnd: usize, rate: f32) -> windows::core::Result<()> {
    windows_controls(hwnd)?.SetPlaybackRate(f64::from(rate))
}

#[cfg(windows)]
fn disable_unsupported_seek_buttons(hwnd: usize) -> windows::core::Result<()> {
    let controls = windows_controls(hwnd)?;
    controls.SetIsFastForwardEnabled(false)?;
    controls.SetIsRewindEnabled(false)
}

#[cfg(windows)]
fn clear_windows_metadata(hwnd: usize) -> windows::core::Result<()> {
    use windows::Media::MediaPlaybackType;

    let updater = windows_controls(hwnd)?.DisplayUpdater()?;
    updater.ClearAll()?;
    updater.Update()?;
    // `ClearAll` also resets the playback type to `Unknown`, and an untyped
    // updater refuses to hand out `MusicProperties` at all — every later track
    // would fail to publish with ERROR_NOT_SUPPORTED, which is exactly what
    // happened: the first engine state arrives before authentication finishes,
    // carries no track, and poisoned the updater for the rest of the session.
    // Souvlaki types the updater once when it attaches, so restoring the type
    // it chose is what puts the updater back where the rest of this file
    // assumes it is. Retyping after `Update` rather than before it keeps the
    // published card empty; setting the type first would push a blank music
    // item to Windows on the way past.
    updater.SetType(MediaPlaybackType::Music)?;
    Ok(())
}

/// Mirrors one full engine state into the media session. Cheap: a channel
/// send; the owning thread deduplicates metadata against the previous track.
pub fn update_state(state: &PlaybackState) {
    let track = state.current_index.and_then(|index| state.queue.get(index));
    record_intent(&PLAYING, state.playing, track.is_some());
    POSITION_MS.store(state.position_ms as u64, Ordering::Relaxed);
    let duration_ms = track.map_or(0, |track| track_duration(state.duration_ms, track.duration_ms));
    DURATION_MS.store(duration_ms as u64, Ordering::Relaxed);
    let Some(sender) = UPDATES.get() else { return; };
    let Some(track) = track else {
        let _ = sender.send(Update::Stopped);
        return;
    };
    let _ = sender.send(Update::State {
        transport: Transport::from_state(state),
        metadata: Metadata {
            duration_ms,
            title: track.name.clone(),
            artist: track.artist_names.join(", "),
            album: track.album_name.clone(),
            cover: track.cover_url.clone(),
        },
    });
}

/// Notes a position heartbeat for relative seeks from the overlay. Nothing is
/// sent to the OS: it runs a playing timeline forward itself, and every real
/// change (play, pause, seek, skip, rate) arrives as a full state.
pub fn update_position(position_ms: u32) {
    POSITION_MS.store(position_ms as u64, Ordering::Relaxed);
}

/// Clears transport and metadata immediately while the engine is unavailable.
pub fn update_disconnected() {
    PLAYING.store(false, Ordering::Relaxed);
    POSITION_MS.store(0, Ordering::Relaxed);
    DURATION_MS.store(0, Ordering::Relaxed);
    if let Some(sender) = UPDATES.get() {
        let _ = sender.send(Update::Stopped);
    }
}

/// Only producers update current intent; draining historical OS updates does not.
fn record_intent(playing: &AtomicBool, intent: bool, playable: bool) {
    playing.store(intent && playable, Ordering::Relaxed);
}

fn track_duration(compiled_ms: u32, source_ms: u32) -> u32 {
    if compiled_ms > 0 { compiled_ms } else { source_ms }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_rate_retries_independently_of_successful_timeline_and_then_deduplicates() {
        let now = Instant::now();
        let next = Transport { playing: true, position_ms: 12_000, rate: 1.5 };
        let mut timeline = None;
        let mut rate = None;
        let mut positions = Vec::new();
        let (rate_result, timeline_result) = publish_transport(&mut timeline, &mut rate, next, false, now,
            |_| Err("rate rejected"), |playback| { positions.push(playback); Ok::<_, ()>(()) });
        assert!(rate_result.is_err());
        assert!(timeline_result.is_ok());
        assert_eq!(timeline.as_ref().unwrap().transport, next);
        assert_eq!(rate, None);
        let mut rates = Vec::new();
        let (rate_result, timeline_result) = publish_transport(&mut timeline, &mut rate, next, false, now,
            |value| { rates.push(value); Ok::<_, ()>(()) },
            |playback| { positions.push(playback); Ok::<_, ()>(()) });
        assert!(rate_result.is_ok() && timeline_result.is_ok());
        assert_eq!(rate, Some(1.5));
        let (a, b) = publish_transport(&mut timeline, &mut rate, next, false, now,
            |value| { rates.push(value); Ok::<_, ()>(()) },
            |playback| { positions.push(playback); Ok::<_, ()>(()) });
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(rates, [1.5]);
        assert_eq!(positions.len(), 2);
        let changed = Transport { position_ms: 50_000, rate: 2.0, ..next };
        let (_, result) = publish_transport(&mut timeline, &mut rate, changed, false, now,
            |_| Ok::<_, ()>(()), |_| Err("timeline rejected"));
        assert!(result.is_err());
        assert_eq!(rate, Some(2.0));
        assert!(timeline.is_none());
        let (_, result) = publish_transport(&mut timeline, &mut rate, changed, false, now,
            |_| -> Result<(), ()> { panic!("confirmed rate must not be retried") }, |_| Ok::<_, ()>(()));
        assert!(result.is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn buffering_holds_progress_without_changing_play_intent_and_restores_audible_speed() {
        let now = Instant::now();
        let mut timeline = None;
        let mut rate = None;
        let mut rates = Vec::new();
        let mut playback = Vec::new();
        let mut state = PlaybackState { playing: true, position_ms: 10_000,
            audible_playback_speed: 1.5, ..Default::default() };
        for (buffering, position_ms, elapsed) in [
            (false, 10_000, 0), (true, 10_000, 0), (true, 10_000, 60),
            (false, 10_000, 60), (false, 13_000, 62),
        ] {
            state.buffering = buffering;
            state.position_ms = position_ms;
            let (a, b) = publish_transport(&mut timeline, &mut rate, Transport::from_state(&state), false,
                now + Duration::from_secs(elapsed),
                |value| { rates.push(value); Ok::<_, ()>(()) },
                |value| { playback.push(value); Ok::<_, ()>(()) });
            assert!(a.is_ok() && b.is_ok());
        }
        assert_eq!(rates, [1.5, 0.0, 1.5]);
        assert_eq!(playback.len(), 3, "held and resumed clocks need no heartbeat publications");
        assert!(playback.iter().all(|value| matches!(value, MediaPlayback::Playing { .. })),
            "buffering cannot turn the user's playing intent into Pause/Resume");
    }

    #[test]
    fn draining_old_stop_does_not_overwrite_newer_producer_toggle_intent() {
        let playing = AtomicBool::new(false);
        let mut metadata = None;
        let mut timeline = None;
        let mut rate = Some(1.0);
        record_intent(&playing, false, false);
        record_intent(&playing, true, true);
        let mut stopped = false;
        publish_stopped(&mut metadata, &mut timeline, &mut rate, |value| {
            stopped = matches!(value, MediaPlayback::Stopped);
            Ok::<_, ()>(())
        }).unwrap();
        assert!(stopped);
        assert!(playing.load(Ordering::Relaxed), "Toggle still derives Pause from the newest producer");
        record_intent(&playing, true, false);
        assert!(!playing.load(Ordering::Relaxed), "a no-playable boundary stops intent synchronously");
    }

    #[test]
    fn media_metadata_publishes_preview_duration_restore_and_same_song_refresh_only_when_changed() {
        let song = |duration_ms, title: &str| Metadata { duration_ms, title: title.into(),
            artist: "Artist".into(), album: "Album".into(), cover: "cover".into() };
        let mut previous = None;
        let mut published = Vec::new();
        for next in [song(180_000, "Song"), song(180_000, "Song"),
            song(30_000, "Song"), song(180_000, "Song"), song(180_000, "Corrected title")] {
            publish_metadata(&mut previous, next, |metadata| {
                published.push((metadata.duration.unwrap().as_millis(), metadata.title.unwrap().to_owned()));
                Ok::<_, ()>(())
            }).unwrap();
        }
        assert_eq!(published, [(180_000, "Song".into()), (30_000, "Song".into()),
            (180_000, "Song".into()), (180_000, "Corrected title".into())]);
    }

    #[test]
    fn the_timeline_is_republished_only_on_a_real_transport_change() {
        let at = Instant::now();
        let playing = |position_ms, rate| Transport { playing: true, position_ms, rate };
        let published = Published { transport: playing(10_000, 1.0), at };
        let after = |ms| at + Duration::from_millis(ms);
        assert!(timeline_changed(None, playing(0, 1.0), false, at), "the first state is always published");
        // Steady playback: every later full state agrees with the OS's own clock.
        assert!(!timeline_changed(Some(&published), playing(12_000, 1.0), false, after(2_000)));
        assert!(!timeline_changed(Some(&published), playing(70_400, 1.0), false, after(60_000)));
        // Play and pause, a seek either way, a stall, a rate change, a new item.
        assert!(timeline_changed(Some(&published), Transport { playing: false, ..playing(12_000, 1.0) }, false, after(2_000)));
        assert!(timeline_changed(Some(&published), playing(95_000, 1.0), false, after(2_000)));
        assert!(timeline_changed(Some(&published), playing(1_000, 1.0), false, after(2_000)));
        assert!(timeline_changed(Some(&published), playing(10_000, 1.0), false, after(5_000)));
        assert!(timeline_changed(Some(&published), playing(12_000, 1.5), false, after(2_000)));
        assert!(timeline_changed(Some(&published), playing(12_000, 1.0), true, after(2_000)));
        // At another rate the OS runs the timeline at that rate.
        let fast = Published { transport: playing(10_000, 2.0), at };
        assert!(!timeline_changed(Some(&fast), playing(14_000, 2.0), false, after(2_000)));
        // A paused playhead stays put until something moves it.
        let paused = Published { transport: Transport { playing: false, ..playing(10_000, 1.0) }, at };
        let still = Transport { playing: false, ..playing(10_000, 1.0) };
        assert!(!timeline_changed(Some(&paused), still, false, after(600_000)));
        assert!(timeline_changed(Some(&paused), Transport { position_ms: 40_000, ..still }, false, after(1)));
    }

    #[test]
    fn failed_os_metadata_publication_is_retried_on_next_full_state() {
        let song = || Metadata { duration_ms: 10, title: "Song".into(),
            artist: String::new(), album: String::new(), cover: String::new() };
        let mut previous = None;
        assert!(publish_metadata(&mut previous, song(), |_| Err("OS unavailable")).is_err());
        let mut calls = 0;
        publish_metadata(&mut previous, song(), |_| { calls += 1; Ok::<_, ()>(()) }).unwrap();
        publish_metadata(&mut previous, song(), |_| { calls += 1; Ok::<_, ()>(()) }).unwrap();
        assert_eq!(calls, 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_media_session_removes_old_artwork_and_can_publish_artwork_again() {
        use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

        // A hidden, independent HWND: no running app, Spotify account or
        // visible window is needed to inspect the actual SMTC metadata.
        #[link(name = "user32")]
        extern "system" {
            fn CreateWindowExW(
                extended_style: u32, class: *const u16, title: *const u16, style: u32,
                x: i32, y: i32, width: i32, height: i32,
                parent: isize, menu: isize, instance: isize, parameter: *const std::ffi::c_void,
            ) -> isize;
            fn DestroyWindow(hwnd: isize) -> i32;
        }
        struct Session {
            hwnd: isize,
            cover: std::path::PathBuf,
        }
        impl Drop for Session {
            fn drop(&mut self) {
                unsafe {
                    if self.hwnd != 0 { DestroyWindow(self.hwnd); }
                    RoUninitialize();
                }
                let _ = std::fs::remove_file(&self.cover);
            }
        }
        unsafe { RoInitialize(RO_INIT_MULTITHREADED).unwrap(); }
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let mut session = Session {
            hwnd: 0,
            cover: std::env::temp_dir().join(format!(
                "spotifine-smtc-cover-{}-{}.png", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            )),
        };
        session.hwnd = unsafe {
            CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0,
                0, 0, 1, 1, 0, 0, 0, std::ptr::null())
        };
        assert_ne!(session.hwnd, 0, "independent SMTC window must be created");
        std::fs::write(&session.cover, include_bytes!("../icons/32x32.png")).unwrap();
        let cover = format!("file://{}", session.cover.display());
        let hwnd = session.hwnd as usize;
        let mut controls = MediaControls::new(PlatformConfig {
            dbus_name: "spotifine-test", display_name: "Spotifine test",
            hwnd: Some(hwnd as *mut std::ffi::c_void),
        }).unwrap();
        clear_windows_metadata(hwnd).unwrap();
        let updater = windows_controls(hwnd).unwrap().DisplayUpdater().unwrap();
        let mut previous = None;
        for (title, artwork) in [
            ("With artwork", cover.as_str()), ("Without artwork", ""),
            ("Restored artwork", cover.as_str()), ("Restored artwork", ""),
        ] {
            publish_windows_metadata(hwnd, &mut controls, &mut previous, Metadata {
                duration_ms: 30_000, title: title.into(), artist: "Artist".into(),
                album: "Album".into(), cover: artwork.into(),
            }).unwrap();
            let properties = updater.MusicProperties().unwrap();
            assert_eq!(properties.Title().unwrap().to_string(), title);
            assert_eq!(properties.Artist().unwrap().to_string(), "Artist");
            assert_eq!(properties.AlbumTitle().unwrap().to_string(), "Album");
            // The native getter returns S_OK plus a null interface for no
            // artwork. windows 0.44's non-null wrapper converts that to an
            // Err(S_OK), so inspect the ABI result separately from its pointer.
            let mut thumbnail = std::ptr::null_mut();
            let result = unsafe {
                (windows::core::Vtable::vtable(&updater).Thumbnail)(
                    windows::core::Vtable::as_raw(&updater), &mut thumbnail,
                )
            };
            let thumbnail = if thumbnail.is_null() {
                None
            } else {
                Some(unsafe { <windows::core::IUnknown as windows::core::Vtable>::from_raw(thumbnail) })
            };
            result.ok().expect("native thumbnail getter must succeed");
            assert_eq!(thumbnail.is_some(), !artwork.is_empty(),
                "artwork presence follows the current metadata, including removal and restoration");
        }
    }
}
