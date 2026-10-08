mod app;
mod autostart;
mod commands;
mod covers;
mod engine_client;
mod log;
mod media_keys;
mod personal_api;
mod playback_router;
mod types;

use std::sync::Arc;

use parking_lot::Mutex;
use tauri::Manager;

use app::{
    data_dir, load_app_settings, update_app_settings, AppState,
};
use engine_client::EngineClient;

fn restore_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        log::warn("could not find the main window for the second app launch");
        return;
    };
    if let Err(error) = window.unminimize() {
        log::warn(&format!("could not restore the main window: {error}"));
    }
    if let Err(error) = window.show() {
        log::warn(&format!("could not show the main window: {error}"));
    }
    if let Err(error) = window.set_focus() {
        log::warn(&format!("could not focus the main window: {error}"));
    }
}

/// Treats the OS registration as authoritative without mutating it. In
/// particular, disabling this app in Windows Startup Apps must not be undone
/// by the next launch. Only `set_launch_at_login` changes the registration.
fn reconcile_autostart_preference(app: &tauri::AppHandle, settings: &mut app::AppSettings) {
    let actual = match autostart::is_enabled(app) {
        Ok(actual) => actual,
        Err(error) => {
            log::warn(&format!(
                "could not read launch-at-login registration at startup: {error}"
            ));
            return;
        }
    };
    if settings.launch_at_login == actual {
        return;
    }
    settings.launch_at_login = actual;
    if let Err(error) = update_app_settings(|settings| settings.launch_at_login = actual) {
        log::warn(&format!(
            "could not persist launch-at-login registration state: {error}"
        ));
    }
}

/// Native Windows state also gates background work: WebView2 can keep its
/// document visible and focused after the HWND has been minimized. Resize
/// covers minimize/restore as well as every maximize path, without polling.
#[cfg(windows)]
fn report_window_state(window: tauri::WebviewWindow) {
    use std::cell::Cell;
    use tauri::Emitter;

    #[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
    struct WindowState {
        maximized: bool,
        minimized: bool,
    }

    // The shell's own background work follows the same state: a window that
    // starts minimized must not wait for its first resize to be known as one.
    if let Ok(minimized) = window.is_minimized() {
        commands::window_minimized_changed(window.app_handle(), minimized);
    }
    let last = Cell::new(None);
    let handle = window.clone();
    window.on_window_event(move |event| {
        if !matches!(event, tauri::WindowEvent::Resized(_)) {
            return;
        }
        let Ok(maximized) = handle.is_maximized() else {
            return;
        };
        let Ok(minimized) = handle.is_minimized() else {
            return;
        };
        let state = WindowState { maximized, minimized };
        let previous: Option<WindowState> = last.replace(Some(state));
        if previous != Some(state) {
            let _ = handle.emit("window-state", state);
        }
        if previous.map(|previous| previous.minimized) != Some(minimized) {
            commands::window_minimized_changed(handle.app_handle(), minimized);
        }
    });
}

/// Shared by normal RunEvent::Exit and the Windows updater's direct exit.
pub(crate) fn shutdown_for_exit(app: &tauri::AppHandle) {
    if let Some(client) = app.try_state::<Arc<EngineClient>>() {
        client.shutdown_engine();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    log::init(app::logs_dir());
    tauri::Builder::default()
        // This MUST be the first plugin: its second-process callback runs
        // before setup, so no duplicate state or playback engine is created.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            restore_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(autostart::init())
        .invoke_handler(tauri::generate_handler![
            commands::check_for_update,
            commands::play,
            commands::set_window_visible,
            commands::pause,
            commands::next,
            commands::previous,
            commands::seek,
            commands::set_volume,
            commands::set_shuffle,
            commands::set_repeat,
            commands::set_playback_speed,
            commands::play_queue,
            commands::preview_track_edit,
            commands::restore_preview,
            commands::play_queue_index,
            commands::add_queue,
            commands::add_queue_batch,
            commands::remove_queue,
            commands::move_queue,
            commands::get_history,
            commands::clear_history,
            commands::get_track_waveform,
            commands::cancel_track_waveform,
            commands::get_track_edit,
            commands::save_track_edit,
            commands::delete_track_edit,
            commands::set_playlist_track_edit_enabled,
            commands::set_playlist_track_excluded,
            commands::search,
            commands::browse_playlist,
            commands::hydrate_library_covers,
            commands::browse_radio,
            commands::browse_playlist_recommendations,
            commands::browse_track,
            commands::browse_album,
            commands::browse_artist,
            commands::browse_artist_songwriter,
            commands::browse_artist_catalogue,
            commands::browse_liked_songs,
            commands::browse_show,
            commands::browse_episode,
            commands::browse_profile,
            commands::browse_track_credits,
            commands::browse_canvas,
            commands::browse_followed_artists,
            commands::browse_saved_shows,
            commands::create_playlist,
            commands::rename_playlist,
            commands::delete_playlist,
            commands::follow_playlist,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::move_library_item,
            commands::add_playlist_tracks,
            commands::remove_playlist_tracks,
            commands::reorder_playlist_tracks,
            commands::login,
            commands::logout,
            commands::set_normalisation,
            commands::personal_api_status,
            commands::personal_api_configure,
            commands::personal_api_authorize,
            commands::personal_api_disconnect,
            commands::personal_api_contains,
            commands::personal_api_set_saved,
            commands::personal_api_devices,
            commands::select_output,
            commands::get_state,
            commands::get_cache_stats,
            commands::clear_cache,
            commands::set_audio_cache_limit,
            commands::set_launch_at_login,
            commands::set_start_minimized,
            commands::set_animated_canvas,
            commands::set_devices_enabled,
            commands::enable_account_canvas,
            commands::touch_playlist,
            commands::touch_playlist_activity,
            commands::get_track_playlists,
        ])
        .setup(|app| {
            let mut startup_settings = load_app_settings();
            reconcile_autostart_preference(app.handle(), &mut startup_settings);
            if let Some(window) = app.get_webview_window("main") {
                if startup_settings.start_minimized {
                    if let Err(error) = window.minimize() {
                        log::warn(&format!(
                            "could not minimize the main window at startup: {error}"
                        ));
                    }
                }
                // The configured window starts hidden. Minimize preserves that
                // visibility flag, so show it afterward in its iconic state
                // rather than briefly showing a normal window first.
                if let Err(error) = window.show() {
                    log::warn(&format!(
                        "could not show the main window at startup: {error}"
                    ));
                }
            } else {
                log::warn("could not find the main window at startup");
            }

            // The last signed-in account's library is on screen from the first
            // paint, offline included; the session confirms or replaces it.
            let mut state = AppState::new(data_dir());
            state.select_last_account();
            app.manage(Mutex::new(state));
            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                report_window_state(window);
            }
            let personal = personal_api::PersonalApi::new().map_err(std::io::Error::other)?;
            app.manage(personal.clone());

            // Spawn the playback engine and keep it alive across crashes.
            let client = EngineClient::start();
            app.manage(client.clone());
            let router = playback_router::PlaybackRouter::new(client.clone(), personal, startup_settings.devices_enabled);
            app.manage(router.clone());
            let output_app = app.handle().clone();
            tauri::async_runtime::spawn(async move { router.watch(output_app).await });
            let supervisor = client.clone();
            tauri::async_runtime::spawn(async move { supervisor.supervise().await });

            // Souvlaki needs the native window handle on Windows. On macOS it
            // attaches to the app's event loop without one.
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(windows)]
                match window.hwnd() {
                    Ok(hwnd) => media_keys::init(app.handle().clone(), client.clone(), Some(hwnd.0 as usize)),
                    Err(error) => log::warn(&format!(
                        "could not get the main window handle for media keys: {error}"
                    )),
                }
                #[cfg(target_os = "macos")]
                {
                    let _ = window;
                    media_keys::init(app.handle().clone(), client.clone(), None);
                }
            } else {
                log::warn("could not find the main window for media keys");
            }

            // Mirror engine state into AppState and the frontend.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(commands::consume_states(app_handle));
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("cover", |_context, request, responder| {
            tauri::async_runtime::spawn(async move {
                responder.respond(covers::serve_cover(request).await);
            });
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                shutdown_for_exit(app_handle);
            }
        });
}
