//! System media controls for Windows and macOS.
//!
//! A dedicated thread owns the `MediaControls`, fed by a channel from the state
//! consumer. Windows attaches SMTC to the main HWND; macOS attaches to the
//! application event loop without a window handle. Button presses are
//! forwarded to the selected output through the central playback router.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

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
        playing: bool,
        position_ms: u32,
        metadata: Metadata,
    },
    /// A scalar heartbeat: only the playhead moved.
    Position(u32),
    /// Nothing playable (logged out, empty queue).
    Stopped,
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
    // Position heartbeats stay on the scalar transport-only lane.
    let mut last_metadata = None;
    for update in receiver {
        let result = match update {
            Update::State {
                playing,
                position_ms,
                metadata,
            } => {
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
                controls.set_playback(transport(playing, position_ms))
            }
            Update::Position(position_ms) => {
                controls.set_playback(transport(PLAYING.load(Ordering::Relaxed), position_ms))
            }
            Update::Stopped => {
                PLAYING.store(false, Ordering::Relaxed);
                last_metadata = None;
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
                controls.set_playback(MediaPlayback::Stopped)
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
    PLAYING.store(state.playing, Ordering::Relaxed);
    POSITION_MS.store(state.position_ms as u64, Ordering::Relaxed);
    let Some(sender) = UPDATES.get() else {
        return;
    };
    let Some(track) = state.current_index.and_then(|index| state.queue.get(index)) else {
        DURATION_MS.store(0, Ordering::Relaxed);
        let _ = sender.send(Update::Stopped);
        return;
    };
    let duration_ms = track_duration(state.duration_ms, track.duration_ms);
    DURATION_MS.store(duration_ms as u64, Ordering::Relaxed);
    let _ = sender.send(Update::State {
        playing: state.playing,
        position_ms: state.position_ms,
        metadata: Metadata {
            duration_ms,
            title: track.name.clone(),
            artist: track.artist_names.join(", "),
            album: track.album_name.clone(),
            cover: track.cover_url.clone(),
        },
    });
}

/// Mirrors a position heartbeat. Skipped entirely while paused: a frozen
/// playhead is already what the last transport update described.
pub fn update_position(position_ms: u32) {
    POSITION_MS.store(position_ms as u64, Ordering::Relaxed);
    if !PLAYING.load(Ordering::Relaxed) {
        return;
    }
    if let Some(sender) = UPDATES.get() {
        let _ = sender.send(Update::Position(position_ms));
    }
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

fn track_duration(compiled_ms: u32, source_ms: u32) -> u32 {
    if compiled_ms > 0 { compiled_ms } else { source_ms }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn failed_os_metadata_publication_is_retried_on_next_full_state() {
        let song = || Metadata { duration_ms: 10, title: "Song".into(),
            artist: String::new(), album: String::new(), cover: String::new() };
        let mut previous = None;
        assert_eq!(publish_metadata(&mut previous, song(), |_| Err("OS unavailable")), Err("OS unavailable"));
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
