//! Which tracks' audio the cache holds, kept across restarts.
//!
//! A track's download mark means "one of its files is in the audio cache", and
//! the cache is content-addressed: files are named by file id, and only track
//! metadata says which ids belong to a track. Within a session the browse
//! index learns that from every metadata parse, but a fresh process knows
//! nothing until it fetches metadata again. This index keeps the part of that
//! knowledge worth keeping — the files actually stored, by the track they were
//! stored or found for — so an unchanged playlist can report its marks after a
//! restart without a single metadata request.
//!
//! It vouches for marks only while it accounts for every file in the cache. A
//! file nobody has mapped (cached before this index existed, or whose commit
//! raced a crash) could belong to any row, so while one remains the index
//! answers "cannot say" and the caller resolves metadata as it always did;
//! resolves, plays and prunes map those files away over time. Mappings are
//! hints, never trusted alone: every answer checks the files it names on disk,
//! so a file removed behind the index's back costs a hint, never a false mark.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use librespot_core::FileId;
use librespot_core::cache::Cache;
use serde::{Deserialize, Serialize};

struct Index {
    path: PathBuf,
    tracks: HashMap<String, Vec<FileId>>,
    /// Files on disk that no track is known for.
    unmapped: HashSet<FileId>,
    dirty: bool,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);
/// Serializes disk writes so concurrent saves cannot race on renames or
/// overwrite a newer snapshot with an older one. Lock ordering is always
/// `WRITE_LOCK -> INDEX`.
static WRITE_LOCK: Mutex<()> = Mutex::new(());
static SAVE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Stored {
    tracks: HashMap<String, Vec<String>>,
}

fn file_hex(file_id: FileId) -> Option<String> {
    file_id.to_base16().ok()
}

fn file_from_hex(hex: &str) -> Option<FileId> {
    if hex.len() != 40 { return None; }
    let mut bytes = [0u8; 20];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(FileId::from_raw(&bytes))
}

fn on_disk(cache: &Cache, file_id: FileId) -> bool {
    cache.file_path(file_id).is_some_and(|path| std::fs::exists(path).unwrap_or(false))
}

/// Opens the index at `path` against the files the cache holds now
/// (`librespot_audio::install_audio_cache`'s answer), dropping mappings to
/// files that are gone and noting files no mapping covers.
pub fn install(path: PathBuf, cached_files: Vec<FileId>) {
    let stored: Stored = std::fs::read(&path).ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut unmapped: HashSet<FileId> = cached_files.into_iter().collect();
    let present = unmapped.clone();
    let mut dirty = false;
    let mut tracks = HashMap::with_capacity(stored.tracks.len());
    for (track, files) in stored.tracks {
        let total = files.len();
        let files: Vec<FileId> = files.iter().filter_map(|hex| file_from_hex(hex))
            .filter(|file_id| present.contains(file_id)).collect();
        dirty |= files.len() != total;
        for file_id in &files { unmapped.remove(file_id); }
        if !files.is_empty() { tracks.insert(track, files); }
    }
    *INDEX.lock().expect("cache index mutex poisoned") = Some(Index { path, tracks, unmapped, dirty });
    save_if_dirty_sync();
}

/// `file_id` holds `track_id`'s audio: it was stored or played for it, or a
/// metadata parse found it on disk.
pub fn record(track_id: &str, file_id: FileId) {
    let mut guard = INDEX.lock().expect("cache index mutex poisoned");
    let Some(index) = guard.as_mut() else { return };
    index.unmapped.remove(&file_id);
    let files = index.tracks.entry(track_id.to_owned()).or_default();
    if !files.contains(&file_id) {
        files.push(file_id);
        index.dirty = true;
    }
}

/// The file is gone from the cache.
pub fn forget_file(file_id: FileId) {
    let mut guard = INDEX.lock().expect("cache index mutex poisoned");
    let Some(index) = guard.as_mut() else { return };
    index.unmapped.remove(&file_id);
    index.tracks.retain(|_, files| {
        let before = files.len();
        files.retain(|held| *held != file_id);
        if files.len() != before { index.dirty = true; }
        !files.is_empty()
    });
}

/// The whole cache was cleared.
pub fn clear() {
    let mut guard = INDEX.lock().expect("cache index mutex poisoned");
    let Some(index) = guard.as_mut() else { return };
    index.dirty |= !index.tracks.is_empty();
    index.tracks.clear();
    index.unmapped.clear();
}

/// Which of `ids` have audio on disk, or `None` while some cached file is not
/// accounted for (and any of `ids` might be the one it belongs to). Only the
/// candidates the index names are checked on disk.
pub fn cached_among<'a>(ids: impl IntoIterator<Item = &'a str>, cache: &Cache) -> Option<Vec<String>> {
    let mut guard = INDEX.lock().expect("cache index mutex poisoned");
    let index = guard.as_mut()?;
    if !index.unmapped.is_empty() {
        index.unmapped.retain(|file_id| on_disk(cache, *file_id));
        if !index.unmapped.is_empty() { return None; }
    }
    let mut seen = HashSet::new();
    let mut cached = Vec::new();
    for id in ids {
        if !seen.insert(id) { continue; }
        let Some(files) = index.tracks.get_mut(id) else { continue };
        let before = files.len();
        files.retain(|file_id| on_disk(cache, *file_id));
        index.dirty |= files.len() != before;
        if files.is_empty() {
            index.tracks.remove(id);
        } else {
            cached.push(id.to_owned());
        }
    }
    Some(cached)
}

/// Serializes pending dirty snapshots to disk under [`WRITE_LOCK`] using a
/// unique temporary file in the destination directory. Returns `true` when no
/// dirty state remains unwritten and `false` if a disk write failed.
fn write_pending_now() -> bool {
    let _write = WRITE_LOCK.lock().expect("cache index write mutex poisoned");
    loop {
        let (path, bytes) = {
            let mut guard = INDEX.lock().expect("cache index mutex poisoned");
            let Some(index) = guard.as_mut().filter(|index| index.dirty) else {
                return true;
            };
            let stored = Stored {
                tracks: index
                    .tracks
                    .iter()
                    .map(|(track, files)| {
                        (
                            track.clone(),
                            files.iter().copied().filter_map(file_hex).collect(),
                        )
                    })
                    .collect(),
            };
            let Ok(bytes) = serde_json::to_vec(&stored) else {
                return true;
            };
            index.dirty = false;
            (index.path.clone(), bytes)
        };
        let seq = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("audio-index");
        let temporary = path.with_file_name(format!(".{file_name}.{}.{seq}.tmp", std::process::id()));
        let written = std::fs::write(&temporary, &bytes).and_then(|()| {
            spotifine_common::atomic::replace_file_atomically(&temporary, &path, false)
        });
        if let Err(error) = written {
            eprintln!("could not save the audio cache index: {error}");
            let _ = std::fs::remove_file(&temporary);
            if let Some(index) = INDEX.lock().expect("cache index mutex poisoned").as_mut() {
                index.dirty = true;
            }
            return false;
        }
    }
}

/// Synchronously flushes any dirty cache index state to disk (used at startup
/// and during shutdown so the final snapshot lands before process exit).
pub fn save_if_dirty_sync() {
    let _ = write_pending_now();
}

/// Writes the index if it changed since it was last written. Small (one line
/// per cached track) and written only after the cache itself changed. Inside a
/// Tokio runtime, coalesces concurrent requests onto a single-flight
/// `spawn_blocking` worker so async tasks never block on disk I/O.
pub fn save_if_dirty() {
    let is_dirty = INDEX
        .lock()
        .expect("cache index mutex poisoned")
        .as_ref()
        .is_some_and(|index| index.dirty);
    if !is_dirty {
        return;
    }
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if SAVE_IN_FLIGHT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        handle.spawn_blocking(|| {
            struct FlightGuard(bool);
            impl Drop for FlightGuard {
                fn drop(&mut self) {
                    if self.0 {
                        SAVE_IN_FLIGHT.store(false, Ordering::Release);
                    }
                }
            }
            let mut guard = FlightGuard(true);
            loop {
                let flushed = write_pending_now();
                guard.0 = false;
                SAVE_IN_FLIGHT.store(false, Ordering::Release);
                if !flushed {
                    return;
                }
                let still_dirty = INDEX
                    .lock()
                    .expect("cache index mutex poisoned")
                    .as_ref()
                    .is_some_and(|index| index.dirty);
                if !still_dirty {
                    return;
                }
                if SAVE_IN_FLIGHT
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return;
                }
                guard.0 = true;
            }
        });
    } else {
        let _ = write_pending_now();
    }
}

#[cfg(test)]
pub(crate) fn uninstall() {
    *INDEX.lock().expect("cache index mutex poisoned") = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);

    impl Directory {
        fn path(&self) -> &std::path::Path { &self.0 }
    }

    impl Drop for Directory {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn fixture() -> (Directory, Cache) {
        let directory = Directory(std::env::temp_dir().join(format!(
            "renderer-cache-index-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        )));
        let cache = Cache::new(None::<PathBuf>, None::<PathBuf>, Some(directory.path().join("audio")), None).unwrap();
        (directory, cache)
    }

    fn write(cache: &Cache, file_id: FileId) {
        let path = cache.file_path(file_id).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"audio").unwrap();
    }

    #[test]
    fn the_index_vouches_only_when_every_cached_file_is_accounted_for_and_survives_a_restart() {
        let (directory, cache) = fixture();
        let path = directory.path().join("audio-index.json");
        let [a, b, orphan] = [1u8, 2, 3].map(|byte| FileId::from_raw(&[byte; 20]));
        for file_id in [a, b, orphan] { write(&cache, file_id); }
        install(path.clone(), vec![a, b, orphan]);
        assert_eq!(cached_among(["track-a"], &cache), None, "three files, none mapped: cannot say");
        record("track-a", a);
        record("track-b", b);
        assert_eq!(cached_among(["track-a"], &cache), None, "one file still unaccounted for");
        // The orphan goes: everything left is accounted for.
        std::fs::remove_file(cache.file_path(orphan).unwrap()).unwrap();
        assert_eq!(cached_among(["track-a", "track-a", "track-c"], &cache), Some(vec!["track-a".to_owned()]));
        save_if_dirty();

        // A restart reads the mappings back and checks them against the disk.
        std::fs::remove_file(cache.file_path(b).unwrap()).unwrap();
        install(path.clone(), vec![a]);
        assert_eq!(cached_among(["track-a", "track-b"], &cache), Some(vec!["track-a".to_owned()]));
        // A file deleted behind the index is a stale hint, never a mark.
        std::fs::remove_file(cache.file_path(a).unwrap()).unwrap();
        assert_eq!(cached_among(["track-a"], &cache), Some(Vec::new()));
        write(&cache, a);
        record("track-a", a);
        forget_file(a);
        assert_eq!(cached_among(["track-a"], &cache), Some(Vec::new()), "a reported removal unmaps the file");
        record("track-a", a);
        clear();
        assert_eq!(cached_among(["track-a"], &cache), Some(Vec::new()), "a clear unmaps everything");
        uninstall();
        assert_eq!(cached_among(["track-a"], &cache), None, "no index, no answer");
    }

    #[test]
    fn concurrent_saves_serialize_with_unique_temp_files_and_leave_no_tmp_behind() {
        let (directory, cache) = fixture();
        let path = directory.path().join("audio-index.json");
        let files: Vec<FileId> = (1u8..=16).map(|byte| FileId::from_raw(&[byte; 20])).collect();
        for &file_id in &files {
            write(&cache, file_id);
        }
        install(path.clone(), files.clone());

        let handles: Vec<_> = files
            .iter()
            .enumerate()
            .map(|(idx, &file_id)| {
                std::thread::spawn(move || {
                    record(&format!("track-{idx}"), file_id);
                    save_if_dirty_sync();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        save_if_dirty_sync();

        let tmp_leftovers: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(tmp_leftovers.is_empty(), "no temp files left behind: {tmp_leftovers:?}");

        install(path, files);
        let ids: Vec<String> = (0..16).map(|idx| format!("track-{idx}")).collect();
        let cached = cached_among(ids.iter().map(String::as_str), &cache).unwrap();
        assert_eq!(cached.len(), 16);
        uninstall();
    }
}
