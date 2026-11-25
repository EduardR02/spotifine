//! The tray icon is the return path to a hidden main window. Install it before
//! hiding anything; removing it must leave the window reachable from the taskbar.

use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

use crate::{app::{update_app_settings, AppSettings}, log, media_keys, playback_router::Action, restore_main_window};

const ID: &str = "main-tray";
const OPEN: &str = "tray-open";
const PLAY_PAUSE: &str = "tray-play-pause";
const NEXT: &str = "tray-next";
const QUIT: &str = "tray-quit";

// Tauri executes run_on_main_thread inline on the UI thread. Queue from a
// worker so ShowWindow cannot reenter a locked resize/menu listener. The
// entire check and window operation then run in one event-loop turn.
fn defer(app: &AppHandle, action: fn(&AppHandle)) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let handle = app.clone();
        if let Err(error) = app.run_on_main_thread(move || action(&handle)) {
            log::warn(&format!("could not schedule window action: {error}"));
        }
    });
}

pub fn open(app: &AppHandle) {
    defer(app, restore_main_window);
}

fn install(app: &AppHandle) -> tauri::Result<()> {
    let image = app.default_window_icon().ok_or_else(|| std::io::Error::other("missing window icon"))?;
    let menu = MenuBuilder::new(app)
        .text(OPEN, "Open")
        .separator()
        .text(PLAY_PAUSE, "Play/Pause")
        .text(NEXT, "Next")
        .separator()
        .text(QUIT, "Quit")
        .build()?;
    let tray = TrayIconBuilder::with_id(ID)
        .menu(&menu)
        .tooltip("Spotifine")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|icon, event| {
            if matches!(event, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }) {
                open(icon.app_handle());
            }
        })
        .build(app)?;
    // On Windows build() can succeed even if Explorer rejected NIM_ADD
    // (e.g. at login). set_icon checks both conversion and NIM_MODIFY.
    if let Err(error) = tray.set_icon(Some(image.clone())) {
        app.remove_tray_by_id(ID);
        return Err(error);
    }
    Ok(())
}

pub fn setup(app: &AppHandle, enabled: bool) -> bool {
    // Tauri's menu handlers are global. Register just once, not on each enable.
    app.on_menu_event(|app, event| match event.id().as_ref() {
        OPEN => open(app),
        PLAY_PAUSE => media_keys::dispatch(app, Action::Toggle),
        NEXT => media_keys::dispatch(app, Action::Next),
        QUIT => app.exit(0), // RunEvent::Exit flushes playback and stops the engine.
        _ => {}
    });
    if !enabled { return false; }
    match install(app) {
        Ok(()) => true,
        Err(error) => {
            log::warn(&format!("notification area unavailable; minimizing to the taskbar: {error}"));
            false
        }
    }
}

fn hide_if_minimized(app: &AppHandle) {
    // An Open, a taskbar restore, or disabling the preference can beat the
    // deferred hide. Never hide a restored window or one without a tray icon.
    if app.tray_by_id(ID).is_none() { return; }
    let Some(window) = app.get_webview_window("main") else { return };
    if window.is_minimized().unwrap_or(false) && window.is_visible().unwrap_or(false) {
        // Synchronize Tao's cached flags with IsIconic (an external minimize
        // needn't have sent WM_SYSCOMMAND), so hide keeps the HWND minimized.
        if let Err(error) = window.minimize().and_then(|()| window.hide()) {
            log::warn(&format!("could not minimize to the notification area: {error}"));
        }
    }
}

pub fn minimized(app: &AppHandle) {
    if app.tray_by_id(ID).is_some() {
        defer(app, hide_if_minimized);
    }
}

// Run on the UI thread as one operation: both a concurrent minimize and a
// second setting change must see a usable icon or a reachable taskbar window.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<AppSettings, String> {
    let had_icon = app.tray_by_id(ID).is_some();
    if enabled && !had_icon {
        install(app).map_err(|error| format!("Could not create the notification area icon: {error}"))?;
    } else if !enabled && had_icon {
        if let Some(window) = app.get_webview_window("main") {
            if !window.is_visible().map_err(|e| e.to_string())? {
                window.show().map_err(|e| e.to_string())?;
            }
        }
    }
    let settings = update_app_settings(|settings| settings.minimize_to_tray = enabled)
        .inspect_err(|_| {
            if enabled && !had_icon { app.remove_tray_by_id(ID); }
        })?;
    if enabled {
        hide_if_minimized(app);
    } else if had_icon {
        app.remove_tray_by_id(ID);
    }
    Ok(settings)
}
