//! One platform boundary for launch-at-login registration. Windows uses the
//! same auto-launch implementation as the Tauri plugin, with a quoted
//! executable path; the plugin's current builder cannot override that path.

use tauri::{plugin::TauriPlugin, AppHandle, Runtime};
#[cfg(windows)]
use tauri::Manager;
#[cfg(not(windows))]
use tauri_plugin_autostart::ManagerExt;

#[cfg(windows)]
fn windows_registration(name: &str, executable: &std::path::Path) -> auto_launch::AutoLaunch {
    // Windows filenames cannot contain quotes. There are no startup arguments;
    // auto-launch writes this path verbatim as the Run command's first token.
    auto_launch::AutoLaunch::new(name, &format!("\"{}\"", executable.display()), &[] as &[&str])
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    #[cfg(windows)]
    {
        tauri::plugin::Builder::new("autostart")
            .setup(|app, _| {
                let registration = windows_registration(
                    &app.package_info().name, &std::env::current_exe()?,
                );
                app.manage(registration);
                Ok(())
            })
            .build()
    }
    #[cfg(not(windows))]
    {
        tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None)
    }
}

pub fn is_enabled(app: &AppHandle) -> Result<bool, String> {
    #[cfg(windows)]
    let registration = app.state::<auto_launch::AutoLaunch>();
    #[cfg(not(windows))]
    let registration = app.autolaunch();
    registration.is_enabled().map_err(|error| error.to_string())
}

pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    let registration = app.state::<auto_launch::AutoLaunch>();
    #[cfg(not(windows))]
    let registration = app.autolaunch();
    let result = if enabled { registration.enable() } else { registration.disable() };
    result.map_err(|error| error.to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn spaced_unicode_startup_executable_is_one_native_command_line_argument() {
        #[link(name = "shell32")]
        extern "system" {
            fn CommandLineToArgvW(command: *const u16, count: *mut i32) -> *mut *mut u16;
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }
        struct Arguments(*mut *mut u16);
        impl Drop for Arguments {
            fn drop(&mut self) { unsafe { LocalFree(self.0.cast()); } }
        }
        let executable = std::path::Path::new(r"C:\Users\Zoë Example\AppData\Local\Spotifine\Spotifine.exe");
        let registration = windows_registration("spotifine", executable);
        // Match auto-launch's Run value without touching the registry.
        let command = format!("{} {}", registration.get_app_path(), registration.get_args().join(" "));
        let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let mut count = 0;
        let arguments = Arguments(unsafe { CommandLineToArgvW(wide.as_ptr(), &mut count) });
        assert!(!arguments.0.is_null(), "native command line parser must succeed");
        assert_eq!(count, 1, "the spaced executable is not split into arguments");
        let first = unsafe { *arguments.0 };
        let mut length = 0;
        unsafe { while *first.add(length) != 0 { length += 1; } }
        let parsed = String::from_utf16(unsafe { std::slice::from_raw_parts(first, length) }).unwrap();
        assert_eq!(parsed, executable.to_string_lossy());
    }
}
