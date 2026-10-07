mod receive;

use std::{
    cmp::min,
    fs,
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    sync::{Condvar, Mutex},
    time::Duration,
};

use futures_util::{StreamExt, future::IntoStream};
use hyper::{Response, StatusCode, body::Incoming, header::CONTENT_RANGE};
use hyper_util::client::legacy::ResponseFuture;

use tempfile::NamedTempFile;
use thiserror::Error;
use tokio::sync::{Semaphore, mpsc, oneshot};

use librespot_core::{Error, FileId, Session, SpotifyUri, cache::Cache, cdn_url::CdnUrl};

use self::receive::audio_file_fetch;

use crate::range_set::{Range, RangeSet};

pub type AudioFileResult = Result<(), librespot_core::Error>;

/// An observation of a song's persistent audio file.
#[derive(Debug)]
pub enum AudioCacheEvent {
    Stored { track_uri: SpotifyUri, file_id: FileId, downloaded: bool, epoch: u64 },
    Removed { track_uri: SpotifyUri, file_id: FileId, epoch: u64 },
    /// A failed save may still have pruned other entries before reporting an error.
    Revalidate { epoch: u64 },
}

static AUDIO_CACHE_EVENT_SENDER: OnceLock<mpsc::UnboundedSender<AudioCacheEvent>> = OnceLock::new();
// A clear advances this epoch while holding the same lock as cache commits.
// Pending loaders capture it before resolving metadata or opening audio.
static CACHE_COMMIT_EPOCH: Mutex<u64> = Mutex::new(0);

pub fn cache_epoch() -> u64 {
    *CACHE_COMMIT_EPOCH.lock().expect("cache commit mutex poisoned")
}

pub fn advance_cache_epoch() -> u64 {
    let mut epoch = CACHE_COMMIT_EPOCH.lock().expect("cache commit mutex poisoned");
    *epoch = epoch.wrapping_add(1);
    *epoch
}

/// Deletes only this cache's audio directory. Shared limiter state is updated
/// through Cache::remove_file for recognized content-addressed entries.
pub fn clear_cached_audio(cache: &Cache) -> Result<(), Error> {
    let mut epoch = CACHE_COMMIT_EPOCH.lock().expect("cache commit mutex poisoned");
    *epoch = epoch.wrapping_add(1);
    let Some(sample) = cache.file_path(FileId::from_raw(&[0; 20])) else { return Ok(()); };
    let root = sample.parent().and_then(|parent| parent.parent())
        .ok_or_else(|| Error::failed_precondition("audio cache has no root directory"))?;
    if fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::failed_precondition("audio cache root is a symbolic link"));
    }
    match fs::read_dir(root) {
        Ok(entries) => clear_audio_directory(cache, root, root, entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn clear_audio_directory(cache: &Cache, root: &std::path::Path, directory: &std::path::Path, entries: fs::ReadDir) -> Result<(), Error> {
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if directory == root && entry.file_name() == "cache-version" { continue; }
        let kind = entry.file_type()?;
        if kind.is_dir() && !kind.is_symlink() {
            clear_audio_directory(cache, root, &path, fs::read_dir(&path)?)?;
            fs::remove_dir(&path)?;
        } else if let Some(file_id) = cache_file_id(root, &path) {
            cache.remove_file(file_id)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn cache_file_id(root: &std::path::Path, path: &std::path::Path) -> Option<FileId> {
    let relative = path.strip_prefix(root).ok()?;
    let mut components = relative.iter();
    let shard = components.next()?.to_str()?.as_bytes();
    let name = components.next()?.to_str()?.as_bytes();
    if components.next().is_some() || shard.len() != 2 || name.len() != 38 { return None; }
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    let mut bytes = [0; 20];
    bytes[0] = digit(shard[0])? * 16 + digit(shard[1])?;
    for (index, pair) in name.chunks_exact(2).enumerate() {
        bytes[index + 1] = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Some(FileId::from_raw(&bytes))
}

pub fn remove_cached_audio(session: &Session, track_uri: &SpotifyUri, file_id: FileId, epoch: u64) -> Result<(), Error> {
    let active = CACHE_COMMIT_EPOCH.lock().expect("cache commit mutex poisoned");
    if *active != epoch {
        return Err(Error::aborted("audio cache was cleared"));
    }
    let cache = session.cache().ok_or_else(|| Error::failed_precondition("audio cache unavailable"))?;
    cache.remove_file(file_id)?;
    if let Some(sender) = AUDIO_CACHE_EVENT_SENDER.get() {
        let _ = sender.send(AudioCacheEvent::Removed { track_uri: track_uri.clone(), file_id, epoch });
    }
    Ok(())
}

/// Installs the process-wide cache event destination once, before loading audio.
pub fn install_cache_event_sender(sender: mpsc::UnboundedSender<AudioCacheEvent>) {
    assert!(
        AUDIO_CACHE_EVENT_SENDER.set(sender).is_ok(),
        "audio cache event sender already installed"
    );
}

fn send_cache_event(track_uri: &SpotifyUri, file_id: FileId, downloaded: bool, epoch: u64) {
    if let Some(sender) = AUDIO_CACHE_EVENT_SENDER.get() {
        if matches!(track_uri, SpotifyUri::Track { .. }) {
            let _ = sender.send(AudioCacheEvent::Stored {
                track_uri: track_uri.clone(), file_id, downloaded, epoch,
            });
        }
    }
}

const DOWNLOAD_STATUS_POISON_MSG: &str = "audio download status mutex should not be poisoned";

#[derive(Error, Debug)]
pub enum AudioFileError {
    #[error("other end of channel disconnected")]
    Channel,
    #[error("required header not found")]
    Header,
    #[error("streamer received no data")]
    NoData,
    #[error("no output available")]
    Output,
    #[error("invalid status code {0}")]
    StatusCode(StatusCode),
    #[error("wait timeout exceeded")]
    WaitTimeout,
}

impl From<AudioFileError> for Error {
    fn from(err: AudioFileError) -> Self {
        match err {
            AudioFileError::Channel => Error::aborted(err),
            AudioFileError::Header => Error::unavailable(err),
            AudioFileError::NoData => Error::unavailable(err),
            AudioFileError::Output => Error::aborted(err),
            AudioFileError::StatusCode(_) => Error::failed_precondition(err),
            AudioFileError::WaitTimeout => Error::deadline_exceeded(err),
        }
    }
}

#[derive(Clone)]
pub struct AudioFetchParams {
    /// The minimum size of a block that is requested from the Spotify servers in one request.
    /// This is the block size that is typically requested while doing a `seek()` on a file.
    /// The Symphonia decoder requires this to be a power of 2 and > 32 kB.
    /// Note: smaller requests can happen if part of the block is downloaded already.
    pub minimum_download_size: usize,

    /// The minimum network throughput that we expect. Together with the minimum download size,
    /// this will determine the time we will wait for a response.
    pub minimum_throughput: usize,

    /// The ping time that is used for calculations before a ping time was actually measured.
    pub initial_ping_time_estimate: Duration,

    /// If the measured ping time to the Spotify server is larger than this value, it is capped
    /// to avoid run-away block sizes and pre-fetching.
    pub maximum_assumed_ping_time: Duration,

    /// Before playback starts, this many seconds of data must be present.
    /// Note: the calculations are done using the nominal bitrate of the file. The actual amount
    /// of audio data may be larger or smaller.
    pub read_ahead_before_playback: Duration,

    /// While playing back, this many seconds of data ahead of the current read position are
    /// requested.
    /// Note: the calculations are done using the nominal bitrate of the file. The actual amount
    /// of audio data may be larger or smaller.
    pub read_ahead_during_playback: Duration,

    /// If the amount of data that is pending (requested but not received) is less than a certain amount,
    /// data is pre-fetched in addition to the read ahead settings above. The threshold for requesting more
    /// data is calculated as `<pending bytes> < PREFETCH_THRESHOLD_FACTOR * <ping time> * <nominal data rate>`
    pub prefetch_threshold_factor: f32,

    /// The time we will wait to obtain status updates on downloading.
    pub download_timeout: Duration,
}

impl Default for AudioFetchParams {
    fn default() -> Self {
        let minimum_download_size = 64 * 1024;
        let minimum_throughput = 8 * 1024;
        Self {
            minimum_download_size,
            minimum_throughput,
            initial_ping_time_estimate: Duration::from_millis(500),
            maximum_assumed_ping_time: Duration::from_millis(1500),
            read_ahead_before_playback: Duration::from_secs(1),
            read_ahead_during_playback: Duration::from_secs(5),
            prefetch_threshold_factor: 4.0,
            download_timeout: Duration::from_secs(
                (minimum_download_size / minimum_throughput) as u64,
            ),
        }
    }
}

static AUDIO_FETCH_PARAMS: OnceLock<AudioFetchParams> = OnceLock::new();

impl AudioFetchParams {
    pub fn set(params: AudioFetchParams) -> Result<(), AudioFetchParams> {
        AUDIO_FETCH_PARAMS.set(params)
    }

    pub fn get() -> &'static AudioFetchParams {
        AUDIO_FETCH_PARAMS.get_or_init(AudioFetchParams::default)
    }
}

pub enum AudioFile {
    Cached(fs::File),
    Streaming(AudioFileStreaming),
}

#[derive(Debug)]
pub struct StreamingRequest {
    streamer: IntoStream<ResponseFuture>,
    initial_response: Option<Response<Incoming>>,
    offset: usize,
    length: usize,
}

#[derive(Debug)]
pub enum StreamLoaderCommand {
    Fetch(Range), // signal the stream loader to fetch a range of the file
    Close,        // terminate and don't load any more data
}

#[derive(Clone)]
pub struct StreamLoaderController {
    channel_tx: Option<mpsc::UnboundedSender<StreamLoaderCommand>>,
    stream_shared: Option<Arc<AudioFileShared>>,
    file_size: usize,
}

impl StreamLoaderController {
    pub fn len(&self) -> usize {
        self.file_size
    }

    pub fn is_empty(&self) -> bool {
        self.file_size == 0
    }

    pub fn range_available(&self, range: Range) -> bool {
        if let Some(ref shared) = self.stream_shared {
            let download_status = shared
                .download_status
                .lock()
                .expect(DOWNLOAD_STATUS_POISON_MSG);

            range.length
                <= download_status
                    .downloaded
                    .contained_length_from_value(range.start)
        } else {
            range.length <= self.len() - range.start
        }
    }

    pub fn range_to_end_available(&self) -> bool {
        match self.stream_shared {
            Some(ref shared) => {
                let read_position = shared.read_position();
                self.range_available(Range::new(read_position, self.len() - read_position))
            }
            None => true,
        }
    }

    pub fn ping_time(&self) -> Option<Duration> {
        self.stream_shared.as_ref().map(|shared| shared.ping_time())
    }

    fn send_stream_loader_command(&self, command: StreamLoaderCommand) {
        if let Some(ref channel) = self.channel_tx {
            // Ignore the error in case the channel has been closed already.
            // This means that the file was completely downloaded.
            let _ = channel.send(command);
        }
    }

    pub fn fetch(&self, range: Range) {
        // signal the stream loader to fetch a range of the file
        self.send_stream_loader_command(StreamLoaderCommand::Fetch(range));
    }

    pub fn fetch_blocking(&self, mut range: Range) -> AudioFileResult {
        // signal the stream loader to tech a range of the file and block until it is loaded.

        // ensure the range is within the file's bounds.
        if range.start >= self.len() {
            range.length = 0;
        } else if range.end() > self.len() {
            range.length = self.len() - range.start;
        }

        self.fetch(range);

        if let Some(ref shared) = self.stream_shared {
            let mut download_status = shared
                .download_status
                .lock()
                .expect(DOWNLOAD_STATUS_POISON_MSG);
            let download_timeout = AudioFetchParams::get().download_timeout;

            while range.length
                > download_status
                    .downloaded
                    .contained_length_from_value(range.start)
            {
                let (new_download_status, wait_result) = shared
                    .cond
                    .wait_timeout(download_status, download_timeout)
                    .expect(DOWNLOAD_STATUS_POISON_MSG);

                download_status = new_download_status;
                if wait_result.timed_out() {
                    return Err(AudioFileError::WaitTimeout.into());
                }

                if range.length
                    > (download_status
                        .downloaded
                        .union(&download_status.requested)
                        .contained_length_from_value(range.start))
                {
                    // For some reason, the requested range is neither downloaded nor requested.
                    // This could be due to a network error. Request it again.
                    self.fetch(range);
                }
            }
        }

        Ok(())
    }

    pub fn fetch_next_and_wait(
        &self,
        request_length: usize,
        wait_length: usize,
    ) -> AudioFileResult {
        match self.stream_shared {
            Some(ref shared) => {
                let start = shared.read_position();

                let request_range = Range {
                    start,
                    length: request_length,
                };
                self.fetch(request_range);

                let wait_range = Range {
                    start,
                    length: wait_length,
                };
                self.fetch_blocking(wait_range)
            }
            None => Ok(()),
        }
    }

    pub fn set_random_access_mode(&self) {
        // optimise download strategy for random access
        if let Some(ref shared) = self.stream_shared {
            shared.set_download_streaming(false)
        }
    }

    pub fn set_stream_mode(&self) {
        // optimise download strategy for streaming
        if let Some(ref shared) = self.stream_shared {
            shared.set_download_streaming(true)
        }
    }

    pub fn close(&self) {
        // terminate stream loading and don't load any more data for this file.
        self.send_stream_loader_command(StreamLoaderCommand::Close);
    }

    pub fn from_local_file(file_size: u64) -> Self {
        Self {
            channel_tx: None,
            stream_shared: None,
            file_size: file_size as usize,
        }
    }
}

pub struct AudioFileStreaming {
    read_file: fs::File,
    position: u64,
    stream_loader_command_tx: mpsc::UnboundedSender<StreamLoaderCommand>,
    shared: Arc<AudioFileShared>,
}

struct AudioFileDownloadStatus {
    requested: RangeSet,
    downloaded: RangeSet,
}

struct AudioFileShared {
    cdn_url: String,
    file_size: usize,
    bytes_per_second: usize,
    cond: Condvar,
    download_status: Mutex<AudioFileDownloadStatus>,
    download_streaming: AtomicBool,
    download_slots: Semaphore,
    ping_time_ms: AtomicUsize,
    read_position: AtomicUsize,
    throughput: AtomicUsize,
}

impl AudioFileShared {
    fn is_download_streaming(&self) -> bool {
        self.download_streaming.load(Ordering::Acquire)
    }

    fn set_download_streaming(&self, streaming: bool) {
        self.download_streaming.store(streaming, Ordering::Release)
    }

    fn ping_time(&self) -> Duration {
        let ping_time_ms = self.ping_time_ms.load(Ordering::Acquire);
        if ping_time_ms > 0 {
            Duration::from_millis(ping_time_ms as u64)
        } else {
            AudioFetchParams::get().initial_ping_time_estimate
        }
    }

    fn set_ping_time(&self, duration: Duration) {
        self.ping_time_ms
            .store(duration.as_millis() as usize, Ordering::Release)
    }

    fn throughput(&self) -> usize {
        self.throughput.load(Ordering::Acquire)
    }

    fn set_throughput(&self, throughput: usize) {
        self.throughput.store(throughput, Ordering::Release)
    }

    fn read_position(&self) -> usize {
        self.read_position.load(Ordering::Acquire)
    }

    fn set_read_position(&self, position: u64) {
        self.read_position
            .store(position as usize, Ordering::Release)
    }
}

// A per-load capability, not a mutable session setting: a preloaded episode
// must not borrow the persistent cache from a concurrently playing song.
fn audio_cache(session: &Session, track_uri: &SpotifyUri) -> Option<Arc<Cache>> {
    if matches!(track_uri, SpotifyUri::Track { .. }) {
        session.cache().cloned()
    } else {
        None
    }
}

async fn cache_completed_download(
    file_id: FileId,
    track_uri: SpotifyUri,
    cache: Option<Arc<Cache>>,
    complete_rx: oneshot::Receiver<NamedTempFile>,
    epoch: u64,
) {
    let Ok(mut file) = complete_rx.await else {
        return;
    };
    debug!("Downloading file {file_id} complete");

    // Always consume completion, even for episodes, so the temporary file is
    // dropped normally rather than causing the downloader's send to fail.
    let active = CACHE_COMMIT_EPOCH.lock().expect("cache commit mutex poisoned");
    if *active != epoch { return; }
    if let Some(cache) = cache {
        if let Some(cache_id) = cache.file_path(file_id) {
            if let Err(e) = cache.save_file(file_id, &mut file) {
                error!("Error caching file {file_id} to {cache_id:?}: {e}");
                if let Some(sender) = AUDIO_CACHE_EVENT_SENDER.get() {
                    let _ = sender.send(AudioCacheEvent::Revalidate { epoch });
                }
            } else {
                debug!("File {file_id} cached to {cache_id:?}");
                send_cache_event(&track_uri, file_id, true, epoch);
            }
        }
    }
}

impl AudioFile {
    pub async fn open(
        session: &Session,
        file_id: FileId,
        bytes_per_second: usize,
        track_uri: &SpotifyUri,
        epoch: u64,
    ) -> Result<AudioFile, Error> {
        let cache = audio_cache(session, track_uri);
        {
            let active = CACHE_COMMIT_EPOCH.lock().expect("cache commit mutex poisoned");
            if *active != epoch {
                return Err(Error::aborted("audio cache was cleared"));
            }
            if let Some(file) = cache.as_ref().and_then(|cache| cache.file(file_id)) {
                debug!("File {file_id} already in cache");
                send_cache_event(track_uri, file_id, false, epoch);
                return Ok(AudioFile::Cached(file));
            }
        }

        debug!("Downloading file {file_id}");

        let (complete_tx, complete_rx) = oneshot::channel();

        let streaming =
            AudioFileStreaming::open(session.clone(), file_id, complete_tx, bytes_per_second);

        session.spawn(cache_completed_download(
            file_id,
            track_uri.clone(),
            cache,
            complete_rx,
            epoch,
        ));

        Ok(AudioFile::Streaming(streaming.await?))
    }

    pub fn get_stream_loader_controller(&self) -> Result<StreamLoaderController, Error> {
        let controller = match self {
            AudioFile::Streaming(stream) => StreamLoaderController {
                channel_tx: Some(stream.stream_loader_command_tx.clone()),
                stream_shared: Some(stream.shared.clone()),
                file_size: stream.shared.file_size,
            },
            AudioFile::Cached(file) => StreamLoaderController {
                channel_tx: None,
                stream_shared: None,
                file_size: file.metadata()?.len() as usize,
            },
        };

        Ok(controller)
    }

    pub fn is_cached(&self) -> bool {
        matches!(self, AudioFile::Cached { .. })
    }
}

impl AudioFileStreaming {
    pub async fn open(
        session: Session,
        file_id: FileId,
        complete_tx: oneshot::Sender<NamedTempFile>,
        bytes_per_second: usize,
    ) -> Result<AudioFileStreaming, Error> {
        let cdn_url = CdnUrl::new(file_id).resolve_audio(&session).await?;

        let minimum_download_size = AudioFetchParams::get().minimum_download_size;

        let mut response_streamer_url = None;
        let urls = cdn_url.try_get_urls()?;
        for url in &urls {
            // When the audio file is really small, this `download_size` may turn out to be
            // larger than the audio file we're going to stream later on. This is OK; requesting
            // `Content-Range` > `Content-Length` will return the complete file with status code
            // 206 Partial Content.
            let mut streamer =
                session
                    .spclient()
                    .stream_from_cdn(*url, 0, minimum_download_size)?;

            // Get the first chunk with the headers to get the file size.
            // The remainder of that chunk with possibly also a response body is then
            // further processed in `audio_file_fetch`.
            let streamer_result = tokio::time::timeout(Duration::from_secs(10), streamer.next())
                .await
                .map_err(|_| AudioFileError::WaitTimeout.into())
                .and_then(|x| x.ok_or_else(|| AudioFileError::NoData.into()))
                .and_then(|x| x.map_err(Error::from));

            match streamer_result {
                Ok(r) => {
                    response_streamer_url = Some((r, streamer, url));
                    break;
                }
                Err(e) => warn!("Fetching {url} failed with error {e:?}, trying next"),
            }
        }

        let Some((response, streamer, url)) = response_streamer_url else {
            return Err(Error::unavailable(format!(
                "{} URLs failed, none left to try",
                urls.len()
            )));
        };

        trace!("Streaming from {url}");

        let code = response.status();
        if code != StatusCode::PARTIAL_CONTENT {
            debug!("Opening audio file expected partial content but got: {code}");
            return Err(AudioFileError::StatusCode(code).into());
        }

        let header_value = response
            .headers()
            .get(CONTENT_RANGE)
            .ok_or(AudioFileError::Header)?;
        let str_value = header_value.to_str()?;
        let hyphen_index = str_value.find('-').unwrap_or_default();
        let slash_index = str_value.find('/').unwrap_or_default();
        let upper_bound: usize = str_value[hyphen_index + 1..slash_index].parse()?;
        let file_size = str_value[slash_index + 1..].parse()?;

        let initial_request = StreamingRequest {
            streamer,
            initial_response: Some(response),
            offset: 0,
            length: upper_bound + 1,
        };

        let shared = Arc::new(AudioFileShared {
            cdn_url: url.to_string(),
            file_size,
            bytes_per_second,
            cond: Condvar::new(),
            download_status: Mutex::new(AudioFileDownloadStatus {
                requested: RangeSet::new(),
                downloaded: RangeSet::new(),
            }),
            download_streaming: AtomicBool::new(false),
            download_slots: Semaphore::new(1),
            ping_time_ms: AtomicUsize::new(0),
            read_position: AtomicUsize::new(0),
            throughput: AtomicUsize::new(0),
        });

        let write_file = NamedTempFile::new_in(session.config().tmp_dir.clone())?;
        write_file.as_file().set_len(file_size as u64)?;

        let read_file = write_file.reopen()?;

        let (stream_loader_command_tx, stream_loader_command_rx) =
            mpsc::unbounded_channel::<StreamLoaderCommand>();

        session.spawn(audio_file_fetch(
            session.clone(),
            shared.clone(),
            initial_request,
            write_file,
            stream_loader_command_rx,
            complete_tx,
        ));

        Ok(AudioFileStreaming {
            read_file,
            position: 0,
            stream_loader_command_tx,
            shared,
        })
    }
}

impl Read for AudioFileStreaming {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let offset = self.position as usize;

        if offset >= self.shared.file_size {
            return Ok(0);
        }

        let length = min(output.len(), self.shared.file_size - offset);
        if length == 0 {
            return Ok(0);
        }

        let read_ahead_during_playback = AudioFetchParams::get().read_ahead_during_playback;
        let length_to_request = if self.shared.is_download_streaming() {
            let length_to_request = length
                + (read_ahead_during_playback.as_secs_f32() * self.shared.bytes_per_second as f32)
                    as usize;

            // Due to the read-ahead stuff, we potentially request more than the actual request demanded.
            min(length_to_request, self.shared.file_size - offset)
        } else {
            length
        };

        let mut ranges_to_request = RangeSet::new();
        ranges_to_request.add_range(&Range::new(offset, length_to_request));

        let mut download_status = self
            .shared
            .download_status
            .lock()
            .expect(DOWNLOAD_STATUS_POISON_MSG);

        ranges_to_request.subtract_range_set(&download_status.downloaded);
        ranges_to_request.subtract_range_set(&download_status.requested);

        for &range in ranges_to_request.iter() {
            self.stream_loader_command_tx
                .send(StreamLoaderCommand::Fetch(range))
                .map_err(|err| io::Error::new(io::ErrorKind::BrokenPipe, err))?;
        }

        let download_timeout = AudioFetchParams::get().download_timeout;
        while !download_status.downloaded.contains(offset) {
            let (new_download_status, wait_result) = self
                .shared
                .cond
                .wait_timeout(download_status, download_timeout)
                .expect(DOWNLOAD_STATUS_POISON_MSG);

            download_status = new_download_status;
            if wait_result.timed_out() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    Error::deadline_exceeded(AudioFileError::WaitTimeout),
                ));
            }
        }
        let available_length = download_status
            .downloaded
            .contained_length_from_value(offset);

        drop(download_status);

        self.position = self.read_file.seek(SeekFrom::Start(offset as u64))?;
        let read_len = min(length, available_length);
        let read_len = self.read_file.read(&mut output[..read_len])?;

        self.position += read_len as u64;
        self.shared.set_read_position(self.position);

        Ok(read_len)
    }
}

impl Seek for AudioFileStreaming {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        // If we are already at this position, we don't need to switch download mode.
        // These checks and locks are less expensive than interrupting streaming.
        let current_position = self.position as i64;
        let requested_pos = match pos {
            SeekFrom::Start(pos) => pos as i64,
            SeekFrom::End(pos) => self.shared.file_size as i64 - pos - 1,
            SeekFrom::Current(pos) => current_position + pos,
        };
        if requested_pos == current_position {
            return Ok(current_position as u64);
        }

        // Again if we have already downloaded this part.
        let available = self
            .shared
            .download_status
            .lock()
            .expect(DOWNLOAD_STATUS_POISON_MSG)
            .downloaded
            .contains(requested_pos as usize);

        let mut was_streaming = false;
        if !available {
            // Ensure random access mode if we need to download this part.
            // Checking whether we are streaming now is a micro-optimization
            // to save an atomic load.
            was_streaming = self.shared.is_download_streaming();
            if was_streaming {
                self.shared.set_download_streaming(false);
            }
        }

        self.position = self.read_file.seek(pos)?;
        self.shared.set_read_position(self.position);

        if !available && was_streaming {
            self.shared.set_download_streaming(true);
        }

        Ok(self.position)
    }
}

impl Read for AudioFile {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match *self {
            AudioFile::Cached(ref mut file) => file.read(output),
            AudioFile::Streaming(ref mut file) => file.read(output),
        }
    }
}

impl Seek for AudioFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match *self {
            AudioFile::Cached(ref mut file) => file.seek(pos),
            AudioFile::Streaming(ref mut file) => file.seek(pos),
        }
    }
}

#[cfg(test)]
mod cache_event_tests {
    use std::{io::Write, path::PathBuf};

    use librespot_core::SessionConfig;

    use super::*;

    fn completed_file() -> oneshot::Receiver<NamedTempFile> {
        let mut file = NamedTempFile::new().expect("completed download");
        file.write_all(b"complete audio file").unwrap();
        file.rewind().unwrap();
        let (sender, receiver) = oneshot::channel();
        sender.send(file).expect("completion receiver");
        receiver
    }

    #[tokio::test]
    async fn cache_events_require_successful_song_persistence() {
        // One test owns the process-wide sender. Distinct URIs isolate events
        // from the existing persistence tests, which may run concurrently.
        let song_uri =
            SpotifyUri::from_uri("spotify:track:0000000000000000000200").unwrap();
        let episode_uri =
            SpotifyUri::from_uri("spotify:episode:0000000000000000000201").unwrap();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        install_cache_event_sender(sender);
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::new(
            None::<PathBuf>,
            None::<PathBuf>,
            Some(directory.path().join("audio")),
            None,
        )
        .unwrap();
        let session = Session::new(SessionConfig::default(), Some(cache));
        let cache = session.cache().unwrap();
        let saved_id = FileId::from_raw(&[200; 20]);

        cache_completed_download(
            saved_id,
            song_uri.clone(),
            audio_cache(&session, &song_uri),
            completed_file(),
            cache_epoch(),
        )
        .await;
        assert_eq!(
            fs::read(cache.file_path(saved_id).unwrap()).unwrap(),
            b"complete audio file"
        );

        let mut cached = AudioFile::open(&session, saved_id, 1, &song_uri, cache_epoch())
            .await
            .expect("saved song opens without network");
        assert!(cached.is_cached());
        let mut bytes = Vec::new();
        cached.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"complete audio file");

        let cancelled_id = FileId::from_raw(&[201; 20]);
        let (cancelled, completion) = oneshot::channel();
        drop(cancelled);
        cache_completed_download(
            cancelled_id,
            song_uri.clone(),
            audio_cache(&session, &song_uri),
            completion,
            cache_epoch(),
        )
        .await;
        assert!(!cache.file_path(cancelled_id).unwrap().exists());

        let uncached_id = FileId::from_raw(&[202; 20]);
        cache_completed_download(
            uncached_id,
            song_uri.clone(),
            None,
            completed_file(),
            cache_epoch(),
        )
        .await;
        assert!(!cache.file_path(uncached_id).unwrap().exists());

        let episode_id = FileId::from_raw(&[203; 20]);
        assert!(audio_cache(&session, &episode_uri).is_none());
        cache_completed_download(
            episode_id,
            episode_uri.clone(),
            audio_cache(&session, &episode_uri),
            completed_file(),
            cache_epoch(),
        )
        .await;
        assert!(!cache.file_path(episode_id).unwrap().exists());

        // A directory at the destination forces a real save error on all
        // platforms, rather than relying on permissions or read-only flags.
        let failed_id = FileId::from_raw(&[204; 20]);
        fs::create_dir_all(cache.file_path(failed_id).unwrap()).unwrap();
        cache_completed_download(
            failed_id,
            song_uri.clone(),
            audio_cache(&session, &song_uri),
            completed_file(),
            cache_epoch(),
        )
        .await;
        assert!(cache.file_path(failed_id).unwrap().is_dir());

        let disabled_cache = Arc::new(
            Cache::new(None::<PathBuf>, None::<PathBuf>, None::<PathBuf>, None).unwrap(),
        );
        assert!(disabled_cache.file_path(saved_id).is_none());
        cache_completed_download(
            saved_id,
            song_uri.clone(),
            Some(disabled_cache),
            completed_file(),
            cache_epoch(),
        )
        .await;

        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            if matches!(&event, AudioCacheEvent::Stored { track_uri, .. } | AudioCacheEvent::Removed { track_uri, .. } if track_uri == &song_uri || track_uri == &episode_uri) {
                events.push(event);
            }
        }
        assert_eq!(events.len(), 2, "only successful save and cache hit emit");
        for (index, event) in events.iter().enumerate() {
            let AudioCacheEvent::Stored { track_uri, file_id, downloaded, .. } = event else { panic!("unexpected removal") };
            assert_eq!(track_uri, &song_uri);
            assert_eq!(file_id, &saved_id);
            assert_eq!(*downloaded, index == 0);
        }

        // The producer starts before clear, but completion reaches persistence
        // afterwards. Clearing must fence the commit, not only its UI event.
        let epoch = cache_epoch();
        let late_id = FileId::from_raw(&[205; 20]);
        let (completion, completion_receiver) = oneshot::channel();
        let late = tokio::spawn(cache_completed_download(
            late_id, song_uri.clone(), audio_cache(&session, &song_uri), completion_receiver, epoch,
        ));
        let root = directory.path().join("audio");
        fs::write(root.join("cache-version"), b"2\n").unwrap();
        let credentials = directory.path().join("credentials.json");
        fs::write(&credentials, b"outside audio cache").unwrap();
        clear_cached_audio(cache).unwrap();
        let new_epoch = cache_epoch();
        assert_ne!(new_epoch, epoch);
        assert_eq!(fs::read(root.join("cache-version")).unwrap(), b"2\n");
        assert_eq!(fs::read(&credentials).unwrap(), b"outside audio cache");
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"late complete download").unwrap();
        file.rewind().unwrap();
        completion.send(file).unwrap();
        late.await.unwrap();
        assert!(!cache.file_path(late_id).unwrap().exists());
        assert!(!cache.file_path(saved_id).unwrap().exists());
        assert!(AudioFile::open(&session, saved_id, 1, &song_uri, epoch).await.is_err());

        cache_completed_download(late_id, song_uri.clone(), audio_cache(&session, &song_uri), completed_file(), new_epoch).await;
        assert!(cache.file_path(late_id).unwrap().exists());
        remove_cached_audio(&session, &song_uri, late_id, new_epoch).unwrap();
        assert!(!cache.file_path(late_id).unwrap().exists());
        let mut removal = false;
        while let Ok(event) = receiver.try_recv() {
            if matches!(event, AudioCacheEvent::Removed { file_id, epoch, .. } if file_id == late_id && epoch == new_epoch) {
                removal = true;
            }
        }
        assert!(removal, "successful decoder rejection publishes file removal");

        let limited_root = directory.path().join("limited-audio");
        let limited = Cache::new(None::<PathBuf>, None::<PathBuf>, Some(limited_root), Some(32)).unwrap();
        limited.save_file(saved_id, &mut std::io::Cursor::new([1u8; 24])).unwrap();
        clear_cached_audio(&limited).unwrap();
        limited.save_file(late_id, &mut std::io::Cursor::new([2u8; 24])).unwrap();
        assert!(limited.file_path(late_id).unwrap().exists(), "clear updates the existing cache limiter instead of leaving phantom entries");
    }
}
