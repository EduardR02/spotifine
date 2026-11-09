//! Audio keys, kept once Spotify has handed them over. Renderer patch.
//!
//! Spotify encrypts an audio file once, for every listener, so the key that
//! opens a file id never changes. librespot asks for it again on every load,
//! cached file or not, and Spotify paces those requests: a key kept here is a
//! request that is never made. Songs keep theirs on disk under the audio cache
//! root, one small file per audio file, fenced by the cache epoch exactly like
//! the audio is and removed with it by [`super::clear_cached_audio`]. Episodes
//! persist no audio, so their keys live only as long as the process.
//!
//! Requests for one file share one flight, and the flight runs on the
//! session's runtime rather than inside whoever asked first: a loader that is
//! abandoned mid-request strands nobody, and the key it asked for is still
//! kept for the next load.

use std::{
    collections::{HashMap, hash_map::Entry},
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use futures_util::future::BoxFuture;
use librespot_core::{
    Error, FileId, Session, SpotifyId, SpotifyUri,
    audio_key::{AudioKey, AudioKeyError},
};
use tempfile::NamedTempFile;
use tokio::sync::oneshot;

use super::{CACHE_COMMIT_EPOCH, audio_cache, cache_root};

const KEYS_POISON_MSG: &str = "audio key mutex poisoned";

/// Why Spotify did not hand over an audio key.
///
/// Spotify's answer carries a code (`error audio key 0 1`, `0 2`), but
/// librespot-core only logs it, so a refusal is all that can be told apart
/// here. It is also all that matters to a caller: either way the track is
/// fine, and asking again later is what works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioKeyFailure {
    /// Spotify answered and declined. This is how its pacing of key requests
    /// shows itself.
    Refused,
    /// No answer arrived in time, or the connection went first.
    Unanswered,
}

impl AudioKeyFailure {
    fn of(error: &Error) -> Self {
        match error.error.downcast_ref::<AudioKeyError>() {
            Some(AudioKeyError::AesKey) => Self::Refused,
            _ => Self::Unanswered,
        }
    }
}

impl fmt::Display for AudioKeyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Refused => "refused",
            Self::Unanswered => "unanswered",
        })
    }
}

/// Where a key came from, so a decoder that rejects it knows whether asking
/// Spotify once more could help.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Kept,
    Spotify,
}

type Answer = Result<AudioKey, AudioKeyFailure>;

#[derive(Default)]
struct Keys {
    kept: HashMap<FileId, AudioKey>,
    flights: HashMap<FileId, Vec<oneshot::Sender<Answer>>>,
}

static KEYS: LazyLock<Mutex<Keys>> = LazyLock::new(Default::default);

/// The key for `file_id`, from what is kept if possible and from Spotify
/// otherwise. `epoch` is the cache epoch the load captured: a key that lands
/// after a clear is handed over but not kept.
pub async fn audio_key(
    session: &Session,
    track_uri: &SpotifyUri,
    track: SpotifyId,
    file_id: FileId,
    epoch: u64,
) -> Result<(AudioKey, KeySource), AudioKeyFailure> {
    let fetch = {
        let session = session.clone();
        async move {
            session.audio_key().request(track, file_id).await
                .map_err(|error| AudioKeyFailure::of(&error))
        }
    };
    obtain(&KEYS, key_store(session, track_uri), file_id, epoch, fetch, |flight| session.spawn(flight)).await
}

/// Drops a kept key a decoder could not open the file with, so the next
/// request for it goes to Spotify.
pub fn forget_audio_key(session: &Session, track_uri: &SpotifyUri, file_id: FileId) {
    forget(&KEYS, key_store(session, track_uri).as_deref(), file_id);
}

/// Called by [`super::clear_cached_audio`] while it holds the epoch lock; the
/// files themselves go with the rest of the audio directory.
pub(super) fn forget_all_audio_keys() {
    KEYS.lock().expect(KEYS_POISON_MSG).kept.clear();
}

/// Songs keep their keys beside their audio; nothing else touches the disk.
fn key_store(session: &Session, track_uri: &SpotifyUri) -> Option<PathBuf> {
    let cache = audio_cache(session, track_uri)?;
    Some(cache_root(&cache)?.join("keys"))
}

async fn obtain<F>(
    keys: &'static Mutex<Keys>,
    store: Option<PathBuf>,
    file_id: FileId,
    epoch: u64,
    fetch: F,
    spawn: impl FnOnce(BoxFuture<'static, ()>),
) -> Result<(AudioKey, KeySource), AudioKeyFailure>
where
    F: Future<Output = Answer> + Send + 'static,
{
    if let Some(key) = kept(keys, store.as_deref(), file_id) {
        return Ok((key, KeySource::Kept));
    }
    let (sender, receiver) = oneshot::channel();
    let lead = {
        let mut keys = keys.lock().expect(KEYS_POISON_MSG);
        // A flight may have landed while the disk was read.
        if let Some(key) = keys.kept.get(&file_id) {
            return Ok((*key, KeySource::Kept));
        }
        match keys.flights.entry(file_id) {
            Entry::Occupied(mut waiters) => {
                waiters.get_mut().push(sender);
                false
            }
            Entry::Vacant(slot) => {
                slot.insert(vec![sender]);
                true
            }
        }
    };
    if lead {
        spawn(Box::pin(flight(keys, store, file_id, epoch, fetch)));
    }
    match receiver.await {
        Ok(answer) => answer.map(|key| (key, KeySource::Spotify)),
        // The flight was dropped unanswered: the runtime is going away.
        Err(_) => Err(AudioKeyFailure::Unanswered),
    }
}

async fn flight<F: Future<Output = Answer>>(
    keys: &'static Mutex<Keys>,
    store: Option<PathBuf>,
    file_id: FileId,
    epoch: u64,
    fetch: F,
) {
    // Dropped unanswered, the waiters' senders go with the entry, so every
    // receiver hears that much instead of waiting forever.
    struct Unanswered {
        keys: &'static Mutex<Keys>,
        file_id: FileId,
        armed: bool,
    }
    impl Drop for Unanswered {
        fn drop(&mut self) {
            if self.armed {
                if let Ok(mut keys) = self.keys.lock() {
                    keys.flights.remove(&self.file_id);
                }
            }
        }
    }
    let mut guard = Unanswered { keys, file_id, armed: true };

    let answer = fetch.await;
    if let Ok(key) = answer {
        keep(keys, store.as_deref(), file_id, key, epoch);
    }
    guard.armed = false;
    let waiters = keys.lock().expect(KEYS_POISON_MSG).flights.remove(&file_id).unwrap_or_default();
    for waiter in waiters {
        let _ = waiter.send(answer);
    }
}

fn kept(keys: &Mutex<Keys>, store: Option<&Path>, file_id: FileId) -> Option<AudioKey> {
    if let Some(key) = keys.lock().expect(KEYS_POISON_MSG).kept.get(&file_id) {
        return Some(*key);
    }
    let path = key_path(store?, file_id)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!("Could not read the kept audio key {path:?}: {error}");
            return None;
        }
    };
    let Ok(key) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        warn!("Discarding malformed audio key {path:?}");
        let _ = fs::remove_file(&path);
        return None;
    };
    let key = AudioKey(key);
    keys.lock().expect(KEYS_POISON_MSG).kept.insert(file_id, key);
    Some(key)
}

fn keep(keys: &Mutex<Keys>, store: Option<&Path>, file_id: FileId, key: AudioKey, epoch: u64) {
    // The same lock a clear advances the epoch under, so a key fetched for a
    // cache that has since been cleared cannot reappear inside the new one.
    let active = CACHE_COMMIT_EPOCH.lock().expect("cache commit mutex poisoned");
    if *active != epoch {
        return;
    }
    keys.lock().expect(KEYS_POISON_MSG).kept.insert(file_id, key);
    let Some(path) = store.and_then(|store| key_path(store, file_id)) else { return };
    if let Err(error) = write_key(&path, key) {
        warn!("Could not keep the audio key for {file_id}: {error}");
    }
}

fn forget(keys: &Mutex<Keys>, store: Option<&Path>, file_id: FileId) {
    keys.lock().expect(KEYS_POISON_MSG).kept.remove(&file_id);
    let Some(path) = store.and_then(|store| key_path(store, file_id)) else { return };
    match fs::remove_file(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => warn!("Could not forget the audio key {path:?}: {error}"),
    }
}

fn key_path(store: &Path, file_id: FileId) -> Option<PathBuf> {
    Some(store.join(file_id.to_base16().ok()?))
}

/// Written beside its destination and renamed into place, so a reader sees
/// the whole key or no key.
fn write_key(path: &Path, key: AudioKey) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| io::Error::other("audio key path has no parent"))?;
    fs::create_dir_all(directory)?;
    let mut file = NamedTempFile::new_in(directory)?;
    file.write_all(&key.0)?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use librespot_core::{SessionConfig, cache::Cache};

    use super::*;
    use crate::{cache_epoch, clear_cached_audio};

    const KEY: AudioKey = AudioKey([7; 16]);

    fn keys() -> &'static Mutex<Keys> {
        Box::leak(Box::default())
    }

    /// A stand-in for Spotify that counts how often it is actually asked.
    fn spotify(asked: &Arc<AtomicUsize>, answer: Answer) -> impl Future<Output = Answer> + Send + 'static {
        let asked = Arc::clone(asked);
        async move {
            asked.fetch_add(1, Ordering::SeqCst);
            answer
        }
    }

    fn spawn(flight: BoxFuture<'static, ()>) {
        tokio::spawn(flight);
    }

    #[tokio::test]
    async fn a_kept_key_is_never_asked_for_again_even_by_a_fresh_process() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("keys");
        let file_id = FileId::from_raw(&[1; 20]);
        let asked = Arc::new(AtomicUsize::new(0));

        let first = obtain(keys(), Some(store.clone()), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
        assert_eq!(first.await, Ok((KEY, KeySource::Spotify)));
        assert_eq!(fs::read(store.join(file_id.to_base16().unwrap())).unwrap(), KEY.0);
        assert_eq!(fs::read_dir(&store).unwrap().count(), 1, "no temporary file is left beside the key");

        // A fresh memory is what the next process starts with: the disk answers.
        let next_process = keys();
        let fresh = obtain(next_process, Some(store.clone()), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
        assert_eq!(fresh.await, Ok((KEY, KeySource::Kept)));
        let again = obtain(next_process, Some(store), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
        assert_eq!(again.await, Ok((KEY, KeySource::Kept)));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_forgotten_or_malformed_key_goes_back_to_spotify() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("keys");
        let file_id = FileId::from_raw(&[2; 20]);
        let asked = Arc::new(AtomicUsize::new(0));
        let memory = keys();

        obtain(memory, Some(store.clone()), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn).await.unwrap();
        forget(memory, Some(&store), file_id);
        assert!(!store.join(file_id.to_base16().unwrap()).exists());
        let refetched = obtain(memory, Some(store.clone()), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
        assert_eq!(refetched.await, Ok((KEY, KeySource::Spotify)));
        assert_eq!(asked.load(Ordering::SeqCst), 2);

        // A key file that is not a key is a miss, and is cleaned up.
        let other = FileId::from_raw(&[3; 20]);
        let path = store.join(other.to_base16().unwrap());
        fs::write(&path, b"short").unwrap();
        let read = obtain(keys(), Some(store), other, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
        assert_eq!(read.await, Ok((KEY, KeySource::Spotify)));
        assert_eq!(fs::read(&path).unwrap(), KEY.0);
    }

    #[tokio::test]
    async fn concurrent_requests_for_one_file_share_one_flight() {
        let file_id = FileId::from_raw(&[4; 20]);
        let asked = Arc::new(AtomicUsize::new(0));
        let memory = keys();
        let (release, released) = oneshot::channel::<()>();
        let slow = {
            let asked = Arc::clone(&asked);
            async move {
                asked.fetch_add(1, Ordering::SeqCst);
                released.await.unwrap();
                Ok(KEY)
            }
        };
        let first = tokio::spawn(obtain(memory, None, file_id, cache_epoch(), slow, spawn));
        tokio::task::yield_now().await;
        let second = tokio::spawn(obtain(memory, None, file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn));
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        assert_eq!(first.await.unwrap(), Ok((KEY, KeySource::Spotify)));
        assert_eq!(second.await.unwrap(), Ok((KEY, KeySource::Spotify)));
        assert_eq!(asked.load(Ordering::SeqCst), 1, "the second asker joined the first flight");
    }

    #[tokio::test]
    async fn an_abandoned_asker_strands_nobody_and_its_key_is_still_kept() {
        let file_id = FileId::from_raw(&[5; 20]);
        let asked = Arc::new(AtomicUsize::new(0));
        let memory = keys();
        let (release, released) = oneshot::channel::<()>();
        let slow = async move {
            released.await.unwrap();
            Ok(KEY)
        };
        let abandoned = tokio::spawn(obtain(memory, None, file_id, cache_epoch(), slow, spawn));
        tokio::task::yield_now().await;
        let waiting = tokio::spawn(obtain(memory, None, file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn));
        tokio::task::yield_now().await;
        abandoned.abort();
        release.send(()).unwrap();
        assert_eq!(waiting.await.unwrap(), Ok((KEY, KeySource::Spotify)));
        let later = obtain(memory, None, file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
        assert_eq!(later.await, Ok((KEY, KeySource::Kept)));
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_refusal_reaches_every_asker_and_keeps_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("keys");
        let file_id = FileId::from_raw(&[6; 20]);
        let asked = Arc::new(AtomicUsize::new(0));
        let memory = keys();
        let (release, released) = oneshot::channel::<()>();
        let refused = async move {
            released.await.unwrap();
            Err(AudioKeyFailure::Refused)
        };
        let first = tokio::spawn(obtain(memory, Some(store.clone()), file_id, cache_epoch(), refused, spawn));
        tokio::task::yield_now().await;
        let second = tokio::spawn(obtain(memory, Some(store.clone()), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn));
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        assert_eq!(first.await.unwrap(), Err(AudioKeyFailure::Refused));
        assert_eq!(second.await.unwrap(), Err(AudioKeyFailure::Refused));
        assert!(!store.exists(), "a refusal writes nothing");
        let retried = obtain(memory, Some(store), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
        assert_eq!(retried.await, Ok((KEY, KeySource::Spotify)));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn only_spotifys_own_refusal_counts_as_refused() {
        assert_eq!(AudioKeyFailure::of(&AudioKeyError::AesKey.into()), AudioKeyFailure::Refused);
        assert_eq!(AudioKeyFailure::of(&AudioKeyError::Timeout.into()), AudioKeyFailure::Unanswered);
        assert_eq!(AudioKeyFailure::of(&Error::unavailable("not connected")), AudioKeyFailure::Unanswered);
    }

    #[tokio::test]
    async fn clearing_the_audio_cache_takes_the_keys_and_fences_late_ones() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::new(None::<PathBuf>, None::<PathBuf>, Some(directory.path().join("audio")), None).unwrap();
        let session = Session::new(SessionConfig::default(), Some(cache));
        let cache = session.cache().unwrap();
        let song = SpotifyUri::from_uri("spotify:track:0000000000000000000300").unwrap();
        let episode = SpotifyUri::from_uri("spotify:episode:0000000000000000000301").unwrap();
        let store = key_store(&session, &song).unwrap();
        assert!(key_store(&session, &episode).is_none(), "episodes keep nothing on disk");
        let asked = Arc::new(AtomicUsize::new(0));
        let kept_id = FileId::from_raw(&[7; 20]);
        let late_id = FileId::from_raw(&[8; 20]);

        obtain(&KEYS, Some(store.clone()), kept_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn).await.unwrap();
        let epoch = cache_epoch();
        let (release, released) = oneshot::channel::<()>();
        let late = tokio::spawn(obtain(&KEYS, Some(store.clone()), late_id, epoch, async move {
            released.await.unwrap();
            Ok(KEY)
        }, spawn));
        tokio::task::yield_now().await;

        clear_cached_audio(cache).unwrap();
        assert!(!store.exists());
        release.send(()).unwrap();
        assert_eq!(late.await.unwrap(), Ok((KEY, KeySource::Spotify)), "the load that asked still gets its key");
        assert!(!store.exists(), "but a key from before the clear is not kept after it");

        for file_id in [kept_id, late_id] {
            let after = obtain(&KEYS, Some(store.clone()), file_id, cache_epoch(), spotify(&asked, Ok(KEY)), spawn);
            assert_eq!(after.await, Ok((KEY, KeySource::Spotify)));
        }
        assert_eq!(asked.load(Ordering::SeqCst), 3);
    }
}
