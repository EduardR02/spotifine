mod audio;
mod auth;
mod browse;
mod collection;
mod customization;
mod edits;
mod engine;
mod follow;
mod history;
mod io;
#[cfg(any(windows, target_os = "macos"))]
mod output_device;
mod resample;
mod time_stretch;
mod waveform;

use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use engine::{AuthSignal, Engine, PlayerSignal};
use io::{Input, ProtocolWriter};
use librespot_audio::AudioFetchParams;
use librespot_core::cache::Cache;
use librespot_core::Session;
use renderer_engine::protocol::{Command, HistoryQuery, Response, TrackWaveform};
use serde::Serialize;
use tokio::sync::mpsc;
use waveform::WaveformService;

const AUDIO_CACHE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;

/// Runs one browse round-trip off the command loop and writes its typed
/// `kind` response from the task. Request ids are correlated by the UI, so
/// out-of-order completion is safe; without a session the reason is answered
/// at once. Playback commands therefore stay prompt while a slow browse is in
/// flight.
fn spawn_browse<T, Fut>(
    engine: &Engine,
    request_id: String,
    kind: &'static str,
    work: impl FnOnce(Session) -> Fut + Send + 'static,
) where
    T: Serialize + Send + 'static,
    Fut: Future<Output = Result<T, String>> + Send + 'static,
{
    spawn_round_trip(engine, request_id, kind, work, ProtocolWriter::send_browse::<T>);
}

/// [`spawn_browse`] for a void playlist edit, answered as `ok`/`error` only.
fn spawn_edit<Fut>(
    engine: &Engine,
    request_id: String,
    kind: &'static str,
    work: impl FnOnce(Session) -> Fut + Send + 'static,
) where
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    spawn_round_trip(engine, request_id, kind, work, ProtocolWriter::send_edit);
}

fn spawn_round_trip<T, Fut>(
    engine: &Engine,
    request_id: String,
    kind: &'static str,
    work: impl FnOnce(Session) -> Fut + Send + 'static,
    reply: fn(&ProtocolWriter, &str, &'static str, &Result<T, String>) -> Result<(), String>,
) where
    T: Send + 'static,
    Fut: Future<Output = Result<T, String>> + Send + 'static,
{
    let writer = engine.writer().clone();
    match engine.browse_session_clone() {
        Ok(session) => {
            tokio::spawn(async move {
                let result = work(session).await;
                // A dead pipe ends the command loop on its own next write.
                let _ = reply(&writer, &request_id, kind, &result);
            });
        }
        Err(error) => {
            let _ = reply(&writer, &request_id, kind, &Err(error));
        }
    }
}

/// Audio fetch tuning at engine startup (before any playback).
///
/// `read_ahead_during_playback` shrinks the streaming buffer from the
/// 5-second default so play/pause/seek feel immediate, at the cost of
/// jitter headroom. It is the operative knob for audible stalls: once a
/// CDN range fetch fails (observed: hyper `IncompleteMessage` / DataLoss
/// mid-body connection drops, librespot-audio receive_data), the player's
/// blocked read is woken and the range re-requested, but the playout
/// buffer drains while the retry cycle (failure detection + reconnect +
/// refill, typically 0.5-3 s) runs. 2 seconds underran before the retry
/// landed (audible ~2-5 s control/playback stalls); 3 seconds covers a
/// full retry cycle, so a single fast-failing fetch no longer goes
/// audible. The buffer is only used to cover jitter: it does not delay
/// initial playback start (the first packet read is served by the
/// initial 64 KiB fetch) and only adds ~one round trip to seek landings.
///
/// `download_timeout` drops from the 8-second default so a *silently*
/// stalled stream (accepted connection that never delivers) is detected
/// and surfaced quickly. It is intentionally NOT raised along with the
/// read-ahead: the observed failure mode fails fast (connection reset,
/// `IncompleteMessage`), which never reaches this timeout — every failed
/// range request notifies the waiting reader and re-arms the window, so
/// retries are not serialized through it. A longer timeout would only
/// delay the give-up on dead-hang connections, where 3 seconds is
/// already generous (any delivered chunk re-arms the window; 64 KiB at
/// 320 kbps arrives in ~1.6 s).
const AUDIO_READ_AHEAD_DURING_PLAYBACK: Duration = Duration::from_secs(3);
const AUDIO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(3);

/// The tuned [`AudioFetchParams`]. Pure so tests can pin the tuning
/// values; the process-wide `OnceLock` is only touched by
/// [`configure_audio_fetch`].
fn audio_fetch_params() -> AudioFetchParams {
    AudioFetchParams {
        read_ahead_during_playback: AUDIO_READ_AHEAD_DURING_PLAYBACK,
        download_timeout: AUDIO_DOWNLOAD_TIMEOUT,
        ..AudioFetchParams::default()
    }
}

fn configure_audio_fetch() {
    // The process sets this exactly once at startup; a second call (tests)
    // is a no-op by design.
    let _ = AudioFetchParams::set(audio_fetch_params());
}

fn main() -> ExitCode {
    let (state_directory, log_file, audio_cache_limit_bytes, normalisation) =
        match parse_arguments(std::env::args_os().skip(1).collect()) {
            Ok(arguments) => arguments,
            Err(error) => {
                eprintln!("PlaybackEngine: {error}");
                return ExitCode::from(2);
            }
        };
    let writer = match ProtocolWriter::capture_stdout() {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("PlaybackEngine: could not capture protocol output: {error}");
            return ExitCode::FAILURE;
        }
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();
    // Installed after the logger: every panic (any thread, including
    // librespot's player thread) is appended to the engine log file so a
    // future death is diagnosable even when stderr is gone.
    install_panic_hook(log_file);

    let (cache, temporary_directory, credentials_file) =
        match create_state(&state_directory, audio_cache_limit_bytes) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("PlaybackEngine: {error}");
                return ExitCode::FAILURE;
            }
        };
    // Two workers: the command loop and the browse/auth tasks beside it. The
    // default spawns one per core for an app that is idle almost always;
    // blocking work (history writes and pages, device waits) has its own pool.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("PlaybackEngine: could not start async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(
        writer,
        cache,
        temporary_directory,
        credentials_file,
        state_directory,
        normalisation,
    ));
    runtime.shutdown_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("PlaybackEngine: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(
    writer: ProtocolWriter,
    cache: Cache,
    temporary_directory: PathBuf,
    credentials_file: PathBuf,
    state_directory: PathBuf,
    normalisation: bool,
) -> Result<(), String> {
    let (input_sender, mut input_receiver) = mpsc::unbounded_channel();
    let (auth_sender, mut auth_receiver) = mpsc::unbounded_channel::<AuthSignal>();
    let (player_sender, mut player_receiver) = mpsc::unbounded_channel::<PlayerSignal>();
    let (audio_sender, mut audio_receiver) = mpsc::unbounded_channel();
    let (output_sender, mut output_receiver) = mpsc::unbounded_channel();
    io::spawn_input_reader(input_sender);

    configure_audio_fetch();
    audio::install_signal_sender(audio_sender);
    let (mut waveform_service, mut waveform_receiver) =
        WaveformService::new(cache.clone(), &state_directory)?;

    let mut engine = Engine::new(
        writer,
        cache,
        temporary_directory,
        credentials_file,
        state_directory,
        normalisation,
        audio::default_sink_opener(),
        audio::default_device_presence(),
    );
    #[cfg(any(windows, target_os = "macos"))]
    let _output_watcher = match output_device::watch(output_sender) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            eprintln!("could not watch default audio output changes: {error}");
            None
        }
    };
    #[cfg(any(windows, target_os = "macos"))]
    engine.set_device_notifications(_output_watcher.is_some());
    #[cfg(not(any(windows, target_os = "macos")))]
    drop(output_sender);
    engine.start_authentication(auth_sender.clone());
    engine.emit_state()?;

    loop {
        let deadline = engine.next_work_deadline();
        tokio::select! {
            input = input_receiver.recv() => {
                match input.unwrap_or(Input::Eof) {
                    Input::Request(request) => {
                        // Observe idle sessions on demand. Queue selection is
                        // accepted immediately into authoritative engine state;
                        // a dead session reconnects before loading that target.
                        if !matches!(&request.command, Command::Shutdown)
                            && engine.tick_session_health(&auth_sender)
                        {
                            engine.emit_state()?;
                        }
                        let request_id = request.request_id;
                        match request.command {
                            Command::Shutdown => {
                                let cancelled: Result<TrackWaveform, String> =
                                    Err("waveform service is shutting down".to_owned());
                                for pending_id in waveform_service.shutdown() {
                                    engine.writer().send_browse(
                                        &pending_id,
                                        "get_track_waveform",
                                        &cancelled,
                                    )?;
                                }
                                let success = Ok(true);
                                engine.send_response(&request_id, &success)?;
                                engine.shutdown();
                                break;
                            }
                            Command::GetHistory {
                                offset,
                                limit,
                                query,
                                sort,
                            } => {
                                // Filtering and name orders walk the whole
                                // archive; that belongs on the blocking pool,
                                // not ahead of the next transport command.
                                let reader = engine.history_reader();
                                let writer = engine.writer().clone();
                                tokio::task::spawn_blocking(move || {
                                    let result = reader.page(&HistoryQuery {
                                        offset,
                                        limit,
                                        query,
                                        sort,
                                    });
                                    let _ = writer.send_browse(&request_id, "history", &result);
                                });
                            }
                            Command::ClearHistory => {
                                let result = engine.clear_history();
                                engine.send_response(&request_id, &result)?;
                            }
                            Command::GetTrackWaveform { track_id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        waveform_service.request(request_id, track_id, session);
                                    }
                                    Err(error) => {
                                        let result: Result<TrackWaveform, String> = Err(error);
                                        engine.writer().send_browse(
                                            &request_id,
                                            "get_track_waveform",
                                            &result,
                                        )?;
                                    }
                                }
                            }
                            Command::CancelTrackWaveform { track_id } => {
                                let cancelled: Result<TrackWaveform, String> =
                                    Err("waveform request was cancelled".to_owned());
                                for pending_id in waveform_service.cancel(&track_id) {
                                    engine.writer().send_browse(
                                        &pending_id,
                                        "get_track_waveform",
                                        &cancelled,
                                    )?;
                                }
                                let success = Ok(true);
                                engine.send_response(&request_id, &success)?;
                            }
                            Command::GetTrackEdit { track_id, playlist_id } => {
                                let result: Result<_, String> =
                                    Ok(engine.track_edit_status(&track_id, playlist_id.as_deref()));
                                engine.writer().send_browse(&request_id, "get_track_edit", &result)?;
                            }
                            Command::SaveTrackEdit {
                                track_id,
                                duration_ms,
                                cuts,
                                loop_range,
                            } => {
                                let result =
                                    engine.save_track_edit(track_id, duration_ms, cuts, loop_range);
                                engine.writer().send_browse(&request_id, "save_track_edit", &result)?;
                            }
                            Command::DeleteTrackEdit { track_id } => {
                                let result = engine
                                    .delete_track_edit(&track_id)
                                    .map(|()| true);
                                engine.send_response(&request_id, &result)?;
                            }
                            Command::SetPlaylistTrackEditEnabled {
                                playlist_id,
                                track_id,
                                enabled,
                            } => {
                                let result = engine
                                    .set_playlist_track_edit_enabled(
                                        &playlist_id,
                                        &track_id,
                                        enabled,
                                    )
                                    .map(|()| true);
                                engine.send_response(&request_id, &result)?;
                            }
                            Command::SetPlaylistTrackExcluded {
                                playlist_id,
                                track_id,
                                excluded,
                            } => {
                                let result = engine
                                    .set_playlist_track_excluded(
                                        &playlist_id,
                                        &track_id,
                                        excluded,
                                    )
                                    .map(|()| true);
                                engine.send_response(&request_id, &result)?;
                                // An exclusion changes nothing else in the
                                // state, but it does change the upcoming plan,
                                // so the queue view only tracks a toggle if a
                                // fresh state follows the reply.
                                if result.is_ok() {
                                    engine.emit_state()?;
                                }
                            }
                            Command::QueueMetadata { context } => {
                                let result = engine.queue_metadata(context.as_deref());
                                engine.writer().send_browse(&request_id, "queue_metadata", &result)?;
                            }
                            Command::BrowsePlaylist { id } => {
                                // Exclusions are engine state, so they are read
                                // here and joined to the browse when it lands.
                                let excluded = engine.playlist_excluded_track_ids(&id);
                                spawn_browse(&engine, request_id, "browse_playlist", move |session| async move {
                                    let mut browse = browse::playlist_browse(&session, &id).await?;
                                    browse.excluded_track_ids = excluded?;
                                    Ok(browse)
                                });
                            }
                            Command::BrowsePlaylistMembership { id } => {
                                spawn_browse(&engine, request_id, "browse_playlist_membership", move |session| async move {
                                    browse::playlist_membership_browse(&session, &id).await
                                });
                            }
                            Command::BrowsePlaylistCovers { playlists } => {
                                spawn_browse(&engine, request_id, "browse_playlist_covers", move |session| async move {
                                    browse::playlist_covers_browse(&session, playlists).await
                                });
                            }
                            Command::BrowseRadio { id } => {
                                spawn_browse(&engine, request_id, "browse_radio", move |session| async move {
                                    browse::radio_browse(&session, &id).await
                                });
                            }
                            Command::BrowsePlaylistRecommendations { id } => {
                                spawn_browse(&engine, request_id, "browse_playlist_recommendations", move |session| async move {
                                    browse::playlist_recommendations_browse(&session, &id).await
                                });
                            }
                            Command::BrowseEpisode { id } => {
                                spawn_browse(&engine, request_id, "browse_episode", move |session| async move {
                                    browse::episode_browse(&session, &id).await
                                });
                            }
                            Command::BrowseShow { id } => {
                                spawn_browse(&engine, request_id, "browse_show", move |session| async move {
                                    browse::show_browse(&session, &id).await
                                });
                            }
                            Command::BrowseProfile { username, known_playlists } => {
                                spawn_browse(&engine, request_id, "browse_profile", move |session| async move {
                                    browse::user_profile_browse(&session, &username, &known_playlists).await
                                });
                            }
                            Command::BrowsePlaylistTree { length } => {
                                spawn_browse(&engine, request_id, "browse_playlist_tree", move |session| async move {
                                    browse::playlist_tree_browse(&session, length).await
                                });
                            }
                            Command::BrowseTrack { id } => {
                                spawn_browse(&engine, request_id, "browse_track", move |session| async move {
                                    browse::track_browse(&session, &id).await
                                });
                            }
                            Command::BrowseAlbum { id } => {
                                spawn_browse(&engine, request_id, "browse_album", move |session| async move {
                                    browse::album_browse(&session, &id).await
                                });
                            }
                            Command::BrowseArtist { id } => {
                                spawn_browse(&engine, request_id, "browse_artist", move |session| async move {
                                    browse::artist_browse(&session, &id).await
                                });
                            }
                            Command::BrowseArtistSongwriter { id, name } => {
                                spawn_browse(&engine, request_id, "browse_artist_songwriter", move |session| async move {
                                    browse::artist_songwriter_browse(&session, &id, &name).await
                                });
                            }
                            Command::BrowseArtistCatalogue { id, release_types, offset, limit, refs_only } => {
                                spawn_browse(&engine, request_id, "browse_artist_catalogue", move |session| async move {
                                    browse::artist_catalogue_browse(
                                        &session,
                                        &id,
                                        &release_types,
                                        offset,
                                        limit,
                                        refs_only,
                                    )
                                    .await
                                });
                            }
                            Command::BrowseLikedSongs { cursor } => {
                                spawn_browse(&engine, request_id, "browse_liked_songs", move |session| async move {
                                    browse::liked_songs_browse(&session, cursor.as_deref()).await
                                });
                            }
                            Command::BrowseLikedUris { cursor } => {
                                spawn_browse(&engine, request_id, "browse_liked_uris", move |session| async move {
                                    browse::liked_song_uris_browse(&session, cursor.as_deref()).await
                                });
                            }
                            Command::BrowseTrackCredits { id } => {
                                spawn_browse(&engine, request_id, "browse_track_credits", move |session| async move {
                                    browse::track_credits_browse(&session, &id).await
                                });
                            }
                            Command::BrowseCanvas { id } => {
                                spawn_browse(&engine, request_id, "browse_canvas", move |session| async move {
                                    browse::canvas_browse(&session, &id).await
                                });
                            }
                            Command::EnableAccountCanvas => {
                                spawn_browse(&engine, request_id, "enable_account_canvas", move |session| async move {
                                    browse::enable_account_canvas(&session).await
                                });
                            }
                            Command::BrowseSearch { query, limit } => {
                                spawn_browse(&engine, request_id, "browse_search", move |session| async move {
                                    browse::search_browse(&session, &query, limit).await
                                });
                            }
                            Command::BrowseFollowedArtists => {
                                spawn_browse(&engine, request_id, "browse_followed_artists", move |session| async move {
                                    follow::followed_artists(&session).await
                                });
                            }
                            Command::BrowseSavedShows => {
                                spawn_browse(&engine, request_id, "browse_saved_shows", move |session| async move {
                                    collection::saved_shows(&session).await
                                });
                            }
                            Command::EditCreatePlaylist { name } => {
                                spawn_browse(&engine, request_id, "edit_create_playlist", move |session| async move {
                                    edits::create_playlist(&session, &name).await
                                });
                            }
                            Command::EditRenamePlaylist { id, name } => {
                                spawn_edit(&engine, request_id, "edit_rename_playlist", move |session| async move {
                                    edits::rename_playlist(&session, &id, &name).await
                                });
                            }
                            Command::EditDeletePlaylist { id } => {
                                spawn_edit(&engine, request_id, "edit_delete_playlist", move |session| async move {
                                    edits::delete_playlist(&session, &id).await
                                });
                            }
                            Command::EditFollowPlaylist { id } => {
                                spawn_edit(&engine, request_id, "edit_follow_playlist", move |session| async move {
                                    edits::follow_playlist(&session, &id).await
                                });
                            }
                            Command::EditCreateFolder { name, parent } => {
                                spawn_browse(&engine, request_id, "edit_create_folder", move |session| async move {
                                    edits::create_folder(&session, &name, parent.as_deref()).await
                                });
                            }
                            Command::EditRenameFolder { id, name } => {
                                spawn_edit(&engine, request_id, "edit_rename_folder", move |session| async move {
                                    edits::rename_folder(&session, &id, &name).await
                                });
                            }
                            Command::EditDeleteFolder { id } => {
                                spawn_edit(&engine, request_id, "edit_delete_folder", move |session| async move {
                                    edits::delete_folder(&session, &id).await
                                });
                            }
                            Command::EditMoveLibraryItem { item, folder } => {
                                spawn_edit(&engine, request_id, "edit_move_library_item", move |session| async move {
                                    edits::move_library_item(&session, &item, folder.as_deref()).await
                                });
                            }
                            Command::EditAddPlaylistTracks { id, uris } => {
                                spawn_edit(&engine, request_id, "edit_add_playlist_tracks", move |session| async move {
                                    edits::add_tracks(&session, &id, &uris).await
                                });
                            }
                            Command::EditRemovePlaylistTracks { id, uris, expected_snapshot_id } => {
                                spawn_edit(&engine, request_id, "edit_remove_playlist_tracks", move |session| async move {
                                    edits::remove_tracks(&session, &id, &uris, expected_snapshot_id.as_deref()).await
                                });
                            }
                            Command::EditReorderPlaylistTracks { id, from, to } => {
                                spawn_edit(&engine, request_id, "edit_reorder_playlist_tracks", move |session| async move {
                                    edits::reorder_tracks(&session, &id, from, to).await
                                });
                            }
                            command => {
                                let result = engine.process_command(command, &auth_sender).await;
                                engine.send_response(&request_id, &result)?;
                                if matches!(result, Ok(true)) {
                                    engine.emit_state()?;
                                }
                            }
                        }
                    }
                    Input::Invalid { request_id, error } => {
                        engine.writer().send(&Response {
                            kind: "response",
                            request_id: &request_id,
                            ok: false,
                            error: Some(&error),
                        })?;
                    }
                    Input::Eof => {
                        waveform_service.shutdown();
                        engine.shutdown();
                        break;
                    }
                }
            }
            signal = auth_receiver.recv() => {
                if let Some(signal) = signal {
                    if engine.on_auth_signal(signal, player_sender.clone()) {
                        engine.emit_state()?;
                    }
                }
            }
            signal = player_receiver.recv() => {
                if let Some(signal) = signal {
                    if engine.on_player_signal(signal) {
                        engine.emit_state()?;
                    }
                }
            }
            signal = audio_receiver.recv() => {
                if let Some(signal) = signal {
                    if engine.on_audio_signal(signal) {
                        engine.emit_state()?;
                    }
                }
            }
            outcome = waveform_receiver.recv() => {
                if let Some(outcome) = outcome {
                    if let Some((request_ids, result)) = waveform_service.complete(outcome) {
                        for request_id in request_ids {
                            engine.writer().send_browse(
                                &request_id,
                                "get_track_waveform",
                                &result,
                            )?;
                        }
                    }
                }
            }
            Some(()) = output_receiver.recv() => {
                if engine.on_default_output_changed() {
                    engine.emit_state()?;
                    engine.tick_audio_device(&auth_sender);
                }
            }
            _ = wait_for_work(deadline) => {
                let active_tick = engine.take_active_tick();
                // Writes a volume change that has stopped moving to the
                // librespot cache. `set_volume` defers it there — a slider drag
                // paces that command at 50 ms and every write is a file create
                // — so this is the tick that pays for the whole gesture, once.
                engine.tick_volume_persist();
                // Stops the player once a finished queue's tail has played.
                engine.tick_output_drain();
                // Catches a session librespot invalidated on its own, which is
                // otherwise invisible until a track refuses to load.
                if engine.tick_session_health(&auth_sender) {
                    engine.emit_state()?;
                }
                // Builds a player again once a device can be there — a probe
                // whose deadline the OS notification or a failed open armed.
                if engine.tick_audio_device(&auth_sender) {
                    engine.emit_state()?;
                }
                // Arms the next-track preload once the current track nears its
                // end, ends a failure run once audio is out again, and fires
                // the one retry a failed row is owed.
                if engine.tick_playback_health() {
                    engine.emit_state()?;
                }
                if active_tick && engine.tick_position() {
                    // Scalar playhead sync: O(1) regardless of queue size.
                    // Real changes (track, queue, play/pause, ...) still emit
                    // the full state through the other arms, and a volume step
                    // has a scalar lane of its own.
                    engine.emit_position()?;
                }
            }
        }
    }
    Ok(())
}

async fn wait_for_work(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending::<()>().await,
    }
}

/// Parses `--state-dir <path>` (required), `--log-file <path>` (optional,
/// the diagnostic log the parent redirects stderr to; the panic hook appends
/// there too), `--audio-cache-limit-mb <n>` (optional) and
/// `--normalisation <true|false>` (optional, default off). Accepts the
/// arguments in any order and tolerates extra pairs, so older launchers that
/// only pass `--state-dir` keep working.
fn parse_arguments(
    arguments: Vec<OsString>,
) -> Result<(PathBuf, Option<PathBuf>, Option<u64>, bool), String> {
    let mut state_directory: Option<PathBuf> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut audio_cache_limit_bytes = Some(AUDIO_CACHE_LIMIT_BYTES);
    let mut audio_cache_limit_seen = false;
    let mut normalisation = false;
    let mut index = 0usize;
    while index < arguments.len() {
        let name = arguments[index].to_string_lossy().into_owned();
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{name} requires a value"))?;
        match name.as_str() {
            "--state-dir" => {
                if state_directory.is_some() {
                    return Err("--state-dir given more than once".to_owned());
                }
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("--state-dir must be an absolute path".to_owned());
                }
                state_directory = Some(path);
            }
            "--log-file" => {
                log_file = Some(PathBuf::from(value));
            }
            "--audio-cache-limit-mb" => {
                if audio_cache_limit_seen {
                    return Err("--audio-cache-limit-mb given more than once".to_owned());
                }
                audio_cache_limit_seen = true;
                let mb = value.to_string_lossy().parse::<u64>().map_err(|_| {
                    "--audio-cache-limit-mb must be a non-negative integer".to_owned()
                })?;
                audio_cache_limit_bytes = if mb == 0 {
                    None
                } else {
                    Some(
                        mb.checked_mul(1024 * 1024)
                            .ok_or_else(|| "--audio-cache-limit-mb is too large".to_owned())?,
                    )
                };
            }
            "--normalisation" => {
                normalisation = match value.to_string_lossy().as_ref() {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(format!(
                            "--normalisation must be \"true\" or \"false\", got {other}"
                        ));
                    }
                };
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        index += 2;
    }
    let state_directory = state_directory
        .ok_or_else(|| "usage: PlaybackEngine --state-dir <absolute-app-owned-path>".to_owned())?;
    Ok((
        state_directory,
        log_file,
        audio_cache_limit_bytes,
        normalisation,
    ))
}

/// One panic report: thread, payload, location, and a captured backtrace.
/// Pure so the hook logic is unit-testable without panicking.
fn format_panic_report(thread: &str, info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "Box<dyn Any>".to_owned()
    };
    let location = info
        .location()
        .map(|location| format!("{location}"))
        .unwrap_or_else(|| "unknown location".to_owned());
    let backtrace = std::backtrace::Backtrace::capture();
    format!("thread '{thread}' panicked at {location}:\n{payload}\nstack backtrace:\n{backtrace}")
}

/// Appends one panic report to the engine log file. Called from the panic
/// hook, so it must never panic itself: every fallible step is ignored.
fn append_panic_to_log(path: &std::path::Path, report: &str) {
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{report}");
        let _ = file.flush();
    }
}

/// Installs the process panic hook: every panic in any thread prints the
/// usual stderr report (so the parent's redirected stderr still shows it)
/// and appends a copy to the engine log file. The hook never panics; a
/// log-file write failure is silently ignored.
fn install_panic_hook(log_file: Option<PathBuf>) {
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_owned();
        let report = format_panic_report(&thread, info);
        eprintln!("{report}");
        if let Some(path) = &log_file {
            append_panic_to_log(path, &report);
        }
    }));
}

/// Marker-file version of the audio cache layout. When the marker is absent
/// or stale, every cached audio file is dropped once (a one-time cleanup of
/// corrupt/truncated entries from earlier builds, which decode-fail with
/// Symphonia "end of stream" and wedge next/prev/shuffle) and the new layout
/// is recorded.
const AUDIO_CACHE_VERSION: &str = "2";

/// Brings the audio cache directory up to the current layout version: on a
/// version change the directory is wiped (files and subdirectories) except
/// for the marker itself, then the marker is (re)written. Idempotent and
/// cheap on steady-state starts.
fn version_audio_cache(state_directory: &std::path::Path) -> Result<(), String> {
    let audio = state_directory.join("audio");
    std::fs::create_dir_all(&audio)
        .map_err(|error| format!("could not create audio cache directory: {error}"))?;
    let marker = audio.join("cache-version");
    let expected = format!("{AUDIO_CACHE_VERSION}\n");
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(expected.as_str()) {
        for entry in std::fs::read_dir(&audio)
            .map_err(|error| format!("could not read audio cache directory: {error}"))?
            .flatten()
        {
            let path = entry.path();
            if path == marker {
                continue;
            }
            let is_directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            let result = if is_directory {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(error) = result {
                eprintln!(
                    "could not clear stale audio cache entry {}: {error}",
                    path.display()
                );
            }
        }
        std::fs::write(&marker, expected)
            .map_err(|error| format!("could not write the audio cache version marker: {error}"))?;
    }
    Ok(())
}

fn create_state(
    state_directory: &std::path::Path,
    audio_cache_limit_bytes: Option<u64>,
) -> Result<(Cache, PathBuf, PathBuf), String> {
    std::fs::create_dir_all(state_directory)
        .map_err(|error| format!("could not create state directory: {error}"))?;
    let credentials = state_directory.join("credentials");
    let volume = state_directory.join("volume");
    let audio = state_directory.join("audio");
    let temporary = state_directory.join("tmp");
    std::fs::create_dir_all(&temporary)
        .map_err(|error| format!("could not create temporary directory: {error}"))?;
    sweep_temporary_directory(&temporary);
    version_audio_cache(state_directory)?;
    let cache = Cache::new(
        Some(credentials),
        Some(volume),
        Some(audio),
        audio_cache_limit_bytes,
    )
    .map_err(|error| format!("could not initialize the app-owned cache: {error}"))?;
    // librespot stores credentials as credentials.json inside the credentials
    // directory; `logout` removes it (Cache offers no removal API).
    let credentials_file = state_directory.join("credentials").join("credentials.json");
    Ok((cache, temporary, credentials_file))
}

/// Deletes everything left in the download scratch directory.
///
/// librespot streams each track into a temporary file here and only moves it
/// into the audio cache once it is complete, so anything abandoned part-way —
/// a skip, a stall, a quit mid-fetch — stays behind forever. Nothing else
/// removes them, and they do not count against the audio cache's size limit,
/// so the directory grows without bound: 94 MB of orphans accumulated in two
/// days of ordinary use on the reference machine, all of it invisible to `ls`
/// because librespot names them `.tmpXXXXXX`.
///
/// Startup is the one moment this is unconditionally safe: the engine has not
/// begun fetching, so every file present is by definition abandoned by a
/// previous run. Failures are ignored — a file that cannot be removed is a
/// wasted megabyte, not a reason to refuse to start.
fn sweep_temporary_directory(temporary: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(temporary) else {
        return;
    };
    let mut removed = 0u64;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
            bytes += size;
        }
    }
    if removed > 0 {
        eprintln!(
            "cleared {removed} abandoned download(s) from the scratch directory ({:.1} MB)",
            bytes as f64 / (1024.0 * 1024.0)
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    struct ScratchDir {
        directory: PathBuf,
    }

    impl ScratchDir {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "sr_engine_cache_test_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            Self { directory }
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn audio_cache_version_bump_clears_stale_entries_once() {
        let scratch = ScratchDir::new();
        // An old build's layout: corrupt audio files plus a format
        // subdirectory, and no version marker.
        let audio = scratch.directory.join("audio");
        fs::create_dir_all(audio.join("ab")).expect("fixture subdir");
        fs::write(audio.join("ab").join("deadbeef"), b"truncated").expect("fixture file");
        fs::write(audio.join("0123456789abcdef"), b"junk").expect("fixture file");

        version_audio_cache(&scratch.directory).expect("version bump succeeds");
        assert!(
            !audio.join("0123456789abcdef").exists(),
            "stale file cleared"
        );
        assert!(!audio.join("ab").exists(), "stale subdirectory cleared");
        assert_eq!(
            fs::read_to_string(audio.join("cache-version")).expect("marker written"),
            format!("{AUDIO_CACHE_VERSION}\n")
        );

        // Steady state: a matching marker leaves new entries untouched.
        fs::write(audio.join("fresh-entry"), b"data").expect("fresh entry");
        version_audio_cache(&scratch.directory).expect("steady-state start");
        assert!(audio.join("fresh-entry").exists(), "fresh entries survive");

        // A later layout version clears everything again (one-time per bump).
        fs::write(audio.join("cache-version"), "1\n").expect("stale marker");
        version_audio_cache(&scratch.directory).expect("second bump");
        assert!(
            !audio.join("fresh-entry").exists(),
            "old-layout entries cleared"
        );
        assert_eq!(
            fs::read_to_string(audio.join("cache-version")).expect("marker rewritten"),
            format!("{AUDIO_CACHE_VERSION}\n")
        );
    }

    #[test]
    fn audio_cache_marker_is_matched_exactly_not_prefixwise() {
        let scratch = ScratchDir::new();
        // The marker is compared with exact content (`"2\n"`), so any
        // deviation — no trailing newline, CRLF, extra text — counts as a
        // different layout and wipes the cache. The engine always writes the
        // exact form, so this only fires if something else touched the file;
        // wiping is the safe failure mode (worst case: one refetch).
        let audio = scratch.directory.join("audio");
        fs::create_dir_all(&audio).expect("fixture dir");
        fs::write(audio.join("cache-version"), AUDIO_CACHE_VERSION).expect("no-newline marker");
        fs::write(audio.join("fresh-entry"), b"data").expect("fixture entry");

        version_audio_cache(&scratch.directory).expect("mismatched marker wipes");
        assert!(
            !audio.join("fresh-entry").exists(),
            "a no-newline marker must be treated as stale"
        );
        assert_eq!(
            fs::read_to_string(audio.join("cache-version")).expect("marker rewritten"),
            format!("{AUDIO_CACHE_VERSION}\n")
        );
    }

    #[test]
    fn audio_fetch_params_pin_the_tuned_headroom_and_timeout() {
        let params = audio_fetch_params();
        // Tuning decision (see AUDIO_READ_AHEAD_DURING_PLAYBACK): the buffer
        // must cover one full fetch-failure retry cycle (fast-failing CDN
        // errors, observed as hyper IncompleteMessage/DataLoss) or the
        // underrun becomes an audible multi-second stall.
        assert_eq!(
            params.read_ahead_during_playback,
            Duration::from_secs(3),
            "buffer headroom covers a fetch retry cycle"
        );
        // The timeout only caps *silent* stalls: fast failures re-arm the
        // wait window via the download-status condvar, so they never reach
        // it, and a shorter give-up is strictly better for dead hangs.
        assert_eq!(
            params.download_timeout,
            Duration::from_secs(3),
            "dead-hang detection must stay quick"
        );
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(|arg| OsString::from(*arg)).collect()
    }

    fn test_path(name: &str) -> String {
        std::env::temp_dir()
            .join("sr")
            .join(name)
            .to_str()
            .expect("test temp directory is UTF-8")
            .to_owned()
    }

    #[test]
    fn parse_arguments_requires_state_dir_and_accepts_an_optional_log_file() {
        let state_dir = test_path("engine");
        let log_file = test_path("logs/playback_engine.log");
        let (state, log, cache_limit, normalisation) =
            parse_arguments(os(&["--state-dir", &state_dir])).expect("state dir alone");
        assert_eq!(state, PathBuf::from(&state_dir));
        assert!(log.is_none(), "log file is optional");
        assert_eq!(cache_limit, Some(AUDIO_CACHE_LIMIT_BYTES));
        assert!(!normalisation, "normalisation defaults to off");

        let (state, log, cache_limit, _) = parse_arguments(os(&[
            "--state-dir",
            &state_dir,
            "--log-file",
            &log_file,
            "--audio-cache-limit-mb",
            "4096",
        ]))
        .expect("both flags");
        assert_eq!(state, PathBuf::from(&state_dir));
        assert_eq!(log, Some(PathBuf::from(&log_file)));
        assert_eq!(cache_limit, Some(4096 * 1024 * 1024));

        // Flag order must not matter.
        let (state, log, cache_limit, _) = parse_arguments(os(&[
            "--audio-cache-limit-mb",
            "0",
            "--log-file",
            &log_file,
            "--state-dir",
            &state_dir,
        ]))
        .expect("log file first");
        assert_eq!(state, PathBuf::from(&state_dir));
        assert!(log.is_some());
        assert_eq!(cache_limit, None, "zero selects an unlimited cache");
    }

    #[test]
    fn parse_arguments_reads_the_normalisation_flag() {
        let state_dir = test_path("engine");
        let (_, _, _, normalisation) = parse_arguments(os(&[
            "--state-dir",
            &state_dir,
            "--normalisation",
            "true",
        ]))
        .expect("normalisation on");
        assert!(normalisation);

        let (_, _, _, normalisation) = parse_arguments(os(&[
            "--state-dir",
            &state_dir,
            "--normalisation",
            "false",
        ]))
        .expect("explicit off");
        assert!(!normalisation);

        assert!(
            parse_arguments(os(&[
                "--state-dir",
                &state_dir,
                "--normalisation",
                "maybe",
            ]))
            .is_err(),
            "a non-boolean value is rejected rather than silently ignored"
        );
    }

    #[test]
    fn parse_arguments_rejects_relative_state_dir_unknown_flags_and_duplicates() {
        let state_dir = test_path("a");
        let duplicate_dir = test_path("b");
        assert!(parse_arguments(os(&[])).is_err(), "state dir required");
        assert!(
            parse_arguments(os(&["--state-dir"])).is_err(),
            "missing value rejected"
        );
        assert!(
            parse_arguments(os(&["--state-dir", "relative/engine"])).is_err(),
            "relative state dir rejected"
        );
        assert!(
            parse_arguments(os(&["--state-dir", &state_dir, "--bogus", "x"])).is_err(),
            "unknown flag rejected"
        );
        assert!(
            parse_arguments(os(&[
                "--state-dir",
                &state_dir,
                "--state-dir",
                &duplicate_dir,
            ]))
            .is_err(),
            "duplicate state dir rejected"
        );
        assert!(
            parse_arguments(os(&[
                "--state-dir",
                &state_dir,
                "--audio-cache-limit-mb",
                "wat",
            ]))
            .is_err(),
            "cache limit must be numeric"
        );
        assert!(
            parse_arguments(os(&[
                "--state-dir",
                &state_dir,
                "--audio-cache-limit-mb",
                "1024",
                "--audio-cache-limit-mb",
                "2048",
            ]))
            .is_err(),
            "duplicate cache limit rejected"
        );
    }

    /// The std::panic hook is process-global while every test thread runs
    /// concurrently: tests that install it must not overlap each other, nor
    /// any deliberately-panicking test such as the waveform worker regression
    /// test, whose report would otherwise land in whatever hook happens to be
    /// installed. Both sides take this lock; poisoning is tolerated so one
    /// genuinely failing test cannot cascade into its siblings.
    pub(crate) static TEST_PANIC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn panic_report_names_the_thread_payload_and_location() {
        let _guard = TEST_PANIC_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Capture the report produced for a real panic, then assert on it
        // after the unwind completes (asserting inside the hook would
        // double-panic and abort the process).
        let report = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let captured = {
            let report = std::sync::Arc::clone(&report);
            std::panic::catch_unwind(|| {
                std::panic::set_hook(Box::new(move |info| {
                    let built = format_panic_report("test-thread", info);
                    *report.lock().expect("report lock") = Some(built);
                }));
                panic!("test payload");
            })
        };
        let _ = std::panic::take_hook(); // drop the test hook
        assert!(captured.is_err());
        let report = report
            .lock()
            .expect("report lock")
            .take()
            .expect("hook ran");
        assert!(report.contains("panicked at"));
        assert!(report.contains("test payload"));
        assert!(report.contains("stack backtrace"));
        assert!(report.contains("main.rs"), "location in the report");
    }

    #[test]
    fn panic_hook_appends_the_report_to_the_engine_log_file() {
        let _guard = TEST_PANIC_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scratch = ScratchDir::new();
        // Mirror production: the app creates the log directory before the
        // engine starts (the hook opens the file with create(true), which
        // cannot create missing parent directories).
        fs::create_dir_all(&scratch.directory).expect("scratch dir created");
        let log = scratch.directory.join("playback_engine.log");
        let previous = std::panic::take_hook();
        install_panic_hook(Some(log.clone()));
        let result = std::panic::catch_unwind(|| panic!("diagnosable failure"));
        std::panic::set_hook(previous);
        assert!(result.is_err(), "the panic still unwinds normally");

        let contents = fs::read_to_string(&log).expect("panic report appended");
        assert!(contents.contains("panicked at"), "report header present");
        assert!(
            contents.contains("diagnosable failure"),
            "payload present: {contents}"
        );
        assert!(
            contents.contains("panic_hook_appends_the_report_to_the_engine_log_file"),
            "call-site location present"
        );

        // A second panic appends, never overwrites.
        let previous = std::panic::take_hook();
        install_panic_hook(Some(log.clone()));
        let result = std::panic::catch_unwind(|| panic!("second failure"));
        std::panic::set_hook(previous);
        assert!(result.is_err());
        let contents = fs::read_to_string(&log).expect("second report appended");
        assert_eq!(
            contents.matches("panicked at").count(),
            2,
            "both reports kept"
        );
    }
}
