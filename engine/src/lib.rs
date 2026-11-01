//! Public API of the Spotify playback engine crate.
//!
//! The engine itself is a binary (`PlaybackEngine`, with `.exe` on Windows) speaking a
//! line-delimited JSON protocol over stdin/stdout; the protocol wire types
//! live here so the Tauri shell (`src-tauri`) can depend on the exact
//! serde shapes without duplicating them.

pub mod protocol;
pub mod spotify_reporting;

/// Atomic single-file replacement shared by engine and shell writers.
/// Standard rename replaces existing files on Windows too. Only durable
/// user-state commits need the additional Windows write-through flag.
pub mod atomic {
    use std::io;
    use std::path::Path;

    pub fn replace_file_atomically(
        source: &Path,
        destination: &Path,
        durable: bool,
    ) -> io::Result<()> {
        #[cfg(windows)]
        if durable {
            return replace_durably(source, destination);
        }
        #[cfg(not(windows))]
        let _ = durable;
        std::fs::rename(source, destination)
    }

    #[cfg(windows)]
    fn replace_durably(source: &Path, destination: &Path) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(
                existing_file_name: *const u16,
                new_file_name: *const u16,
                flags: u32,
            ) -> i32;
        }
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let moved = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn atomic_replacement_overwrites_existing_destination_for_both_durability_modes() {
            let directory = std::env::temp_dir().join(format!(
                "renderer-atomic-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            ));
            std::fs::create_dir(&directory).unwrap();
            let destination = directory.join("destination");
            let source = directory.join("source");
            std::fs::write(&destination, b"old").unwrap();
            for (durable, bytes) in [(false, b"cache".as_slice()), (true, b"durable".as_slice())] {
                std::fs::write(&source, bytes).unwrap();
                super::replace_file_atomically(&source, &destination, durable).unwrap();
                assert_eq!(std::fs::read(&destination).unwrap(), bytes);
                assert!(!source.exists());
            }
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
