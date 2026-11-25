//! Direct cover delivery: the image URL carries its source, and a cache miss
//! downloads it in the asynchronous protocol handler. Disk and HTTP caches are
//! sufficient; no IPC resolution or second in-memory image cache is needed.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};
use tauri::http::{Request, Response};

use crate::app::data_dir;

type FetchLocks = HashMap<String, Arc<tokio::sync::Mutex<()>>>;

fn fetch_locks() -> &'static parking_lot::Mutex<FetchLocks> {
    static LOCKS: LazyLock<parking_lot::Mutex<FetchLocks>> =
        LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));
    &LOCKS
}

/// Only simultaneous requests share a lock; finished keys hold no memory.
struct ActiveFetch<'a> {
    key: &'a str,
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl<'a> ActiveFetch<'a> {
    fn new(key: &'a str) -> Self {
        let mut locks = fetch_locks().lock();
        let lock = match locks.get(key) {
            Some(lock) => lock.clone(),
            None => {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key.to_owned(), lock.clone());
                lock
            }
        };
        Self { key, lock }
    }
}

impl Drop for ActiveFetch<'_> {
    fn drop(&mut self) {
        let mut locks = fetch_locks().lock();
        if Arc::strong_count(&self.lock) == 2 {
            locks.remove(self.key);
        }
    }
}

const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const COVER_FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const COVER_CACHE_MAGIC: &[u8; 8] = b"SPTCOV02";
const COVER_CACHE_HEADER: usize = 16;
const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;
const COVER_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const COVER_CACHE_PRUNE_TARGET: u64 = 224 * 1024 * 1024;
const COVER_PRUNE_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024;

static WRITTEN_SINCE_PRUNE: AtomicU64 = AtomicU64::new(0);
static PRUNE_RUNNING: AtomicBool = AtomicBool::new(false);

fn is_allowed_cover_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.strip_suffix(".scdn.co").is_some_and(|prefix| !prefix.is_empty())
        || host.strip_suffix(".spotifycdn.com").is_some_and(|prefix| !prefix.is_empty())
}

fn validate_cover_url(parsed: &url::Url) -> Result<(), String> {
    if parsed.scheme() != "https" {
        return Err("cover source must be an HTTPS URL".to_owned());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.port().is_some() {
        return Err("cover source URL contains unsupported credentials or port".to_owned());
    }
    let Some(host) = parsed.host_str() else {
        return Err("cover source URL has no host".to_owned());
    };
    if !is_allowed_cover_host(host) {
        return Err("cover source host is not an allowed Spotify CDN".to_owned());
    }
    Ok(())
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(COVER_FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 3 {
                    attempt.error("too many cover redirects")
                } else if let Err(reason) = validate_cover_url(attempt.url()) {
                    attempt.error(reason)
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .expect("default cover HTTP client configuration is valid")
    });
    &CLIENT
}

fn cache_key(url: &str) -> String {
    format!("{:x}", Sha1::digest(url.as_bytes()))
}

fn source_url(uri: &tauri::http::Uri) -> Result<String, String> {
    // convertFileSrc encodes the complete source as one path segment. Decode
    // that segment once; encoded '+' and '&' remain part of the source URL.
    let encoded = uri.path().trim_start_matches('/');
    let source = url::form_urlencoded::parse(encoded.as_bytes())
        .next()
        .map(|(source, _)| source.into_owned())
        .ok_or_else(|| "cover source is missing".to_owned())?;
    let parsed = url::Url::parse(&source).map_err(|error| error.to_string())?;
    validate_cover_url(&parsed)?;
    Ok(source)
}

fn cached_image(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let read = || -> Option<Vec<u8>> {
        let mut file = std::fs::File::open(path).ok()?;
        let mut header = [0_u8; COVER_CACHE_HEADER];
        file.read_exact(&mut header).ok()?;
        if &header[..8] != COVER_CACHE_MAGIC { return None; }
        let length = u64::from_le_bytes(header[8..].try_into().ok()?);
        if file.metadata().ok()?.len() != length.checked_add(COVER_CACHE_HEADER as u64)? {
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(length).ok()?);
        file.read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 == length && sniff_content_type(&bytes).is_some()).then_some(bytes)
    };
    let bytes = read();
    if bytes.is_none() { let _ = std::fs::remove_file(path); }
    bytes
}

fn write_cover(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_cover_with(path, bytes, |source, destination| {
        spotifine_common::atomic::replace_file_atomically(source, destination, false)
    })?;
    if let Some(parent) = path.parent() {
        maybe_schedule_prune(parent.to_path_buf(), (bytes.len() + COVER_CACHE_HEADER) as u64);
    }
    Ok(())
}

fn maybe_schedule_prune(dir: PathBuf, written_bytes: u64) {
    let accumulated = WRITTEN_SINCE_PRUNE.fetch_add(written_bytes, Ordering::Relaxed) + written_bytes;
    if accumulated < COVER_PRUNE_THRESHOLD_BYTES {
        return;
    }
    if PRUNE_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    WRITTEN_SINCE_PRUNE.store(0, Ordering::Relaxed);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn_blocking(move || {
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    PRUNE_RUNNING.store(false, Ordering::Release);
                }
            }
            let _guard = Guard;
            prune_cover_cache(&dir, COVER_CACHE_MAX_BYTES, COVER_CACHE_PRUNE_TARGET);
        });
    } else {
        PRUNE_RUNNING.store(false, Ordering::Release);
    }
}

fn prune_cover_cache(dir: &Path, max_bytes: u64, target_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue; };
        if !kind.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "tmp") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else { continue; };
        let len = metadata.len();
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        total = total.saturating_add(len);
        files.push((path, len, modified));
    }
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, len, _) in files {
        if total <= target_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

fn png_has_iend(bytes: &[u8]) -> bool {
    if bytes.len() < 8 + 12 {
        return false;
    }
    let mut offset = 8usize;
    while let Some(header) = bytes.get(offset..offset + 8) {
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let chunk_type = &header[4..8];
        let Some(next) = offset.checked_add(12).and_then(|end| end.checked_add(length)) else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        if chunk_type == b"IEND" {
            return true;
        }
        offset = next;
    }
    false
}

/// Validates image magic, header dimensions, and container completeness without
/// running full RGBA pixel decompression on every cover cache miss.
fn validate_image_structure(bytes: &[u8]) -> Result<(), String> {
    let Some(content_type) = sniff_content_type(bytes) else {
        return Err("cover source returned a non-image body".to_owned());
    };
    let (width, height) = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "cover source returned an invalid image".to_owned())?
        .into_dimensions()
        .map_err(|_| "cover source returned an incomplete or invalid image".to_owned())?;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return Err("cover source returned invalid image dimensions".to_owned());
    }
    let complete = match content_type {
        "image/png" => png_has_iend(bytes),
        "image/jpeg" => {
            bytes.len() >= 4
                && bytes[bytes.len().saturating_sub(16)..]
                    .windows(2)
                    .any(|pair| pair == [0xFF, 0xD9])
        }
        "image/webp" => {
            bytes.len() >= 12 && {
                let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                u64::from(riff_size) + 8 <= bytes.len() as u64
            }
        }
        "image/gif" => bytes.len() >= 14 && bytes.last() == Some(&0x3B),
        _ => false,
    };
    if !complete {
        return Err("cover source returned an incomplete or invalid image".to_owned());
    }
    Ok(())
}

fn write_cover_with(
    path: &Path,
    bytes: &[u8],
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    use std::io::Write as _;
    let parent = path.parent().expect("cover cache path has a parent");
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    // Only complete, decoded downloads are published. The envelope rejects
    // old raw entries and incomplete bodies without decoding every cache hit.
    let temp = path.with_extension("tmp");
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(COVER_CACHE_MAGIC)?;
        file.write_all(&(bytes.len() as u64).to_le_bytes())?;
        file.write_all(bytes)?;
        drop(file);
        replace(&temp, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temp); }
    result.map_err(|error| format!("could not cache cover: {error}"))
}

async fn image_bytes(directory: &Path, source: &str, key: &str) -> Result<Vec<u8>, String> {
    let active = ActiveFetch::new(key);
    let _guard = active.lock.lock().await;
    let path = directory.join(key);
    let read_path = path.clone();
    if let Some(bytes) = tokio::task::spawn_blocking(move || cached_image(&read_path))
        .await
        .map_err(|error| error.to_string())?
    {
        return Ok(bytes);
    }
    let mut response = http_client()
        .get(source)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response.content_length().is_some_and(|len| len > MAX_COVER_BYTES as u64) {
        return Err("cover image exceeds size limit".to_owned());
    }
    let initial_capacity = response
        .content_length()
        .and_then(|len| usize::try_from(len).ok())
        .map_or(0, |len| len.min(MAX_COVER_BYTES));
    let mut bytes = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len().saturating_add(chunk.len()) > MAX_COVER_BYTES {
            return Err("cover image exceeds size limit".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    // Validate magic/header/structure and persist off the async worker without
    // full RGBA pixel decompression.
    tokio::task::spawn_blocking(move || {
        validate_image_structure(&bytes)?;
        if let Err(error) = write_cover(&path, &bytes) {
            eprintln!("Spotifine: {error}");
        }
        Ok(bytes)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn etag_matches(key: &str, if_none_match: Option<&str>) -> bool {
    let etag = format!("\"{key}\"");
    if_none_match.is_some_and(|value| {
        value.split(',').any(|tag| {
            let tag = tag.trim();
            tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag
        })
    })
}

fn not_modified_response(key: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(304)
        .header("Cache-Control", CACHE_CONTROL)
        .header("ETag", format!("\"{key}\""))
        .header("Access-Control-Allow-Origin", "*")
        .body(Cow::Borrowed(&[][..]))
        .expect("cover 304 response headers are valid")
}

fn image_response(bytes: Vec<u8>, key: &str, if_none_match: Option<&str>) -> Response<Cow<'static, [u8]>> {
    if etag_matches(key, if_none_match) {
        return not_modified_response(key);
    }
    Response::builder()
        .status(200)
        .header("Cache-Control", CACHE_CONTROL)
        .header("ETag", format!("\"{key}\""))
        .header("Access-Control-Allow-Origin", "*")
        .header("Content-Type", sniff_content_type(&bytes).expect("only validated image bytes are served"))
        .body(Cow::Owned(bytes))
        .expect("cover response headers are valid")
}

fn error_response(status: u16, message: String) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header("Cache-Control", "no-store")
        .header("Access-Control-Allow-Origin", "*")
        .body(Cow::Owned(message.into_bytes()))
        .expect("cover error response is valid")
}

pub async fn serve_cover(request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let source = match source_url(request.uri()) {
        Ok(source) => source,
        Err(error) => return error_response(400, error),
    };
    let key = cache_key(&source);
    let if_none_match = request
        .headers()
        .get("If-None-Match")
        .and_then(|value| value.to_str().ok());
    // Revalidate before reading any cached bytes from disk.
    if etag_matches(&key, if_none_match) {
        return not_modified_response(&key);
    }
    match image_bytes(&data_dir().join("covers"), &source, &key).await {
        Ok(bytes) => image_response(bytes, &key, None),
        Err(error) => error_response(502, error),
    }
}

fn sniff_content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_png_color(color: [u8; 4]) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba(color)))
            .write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn test_png() -> Vec<u8> {
        test_png_color([255, 0, 0, 255])
    }

    #[test]
    fn source_urls_round_trip_queries_and_encoded_characters() {
        for source in [
            "https://i.scdn.co/image/a+b.jpg?size=640&name=a%20b",
            "https://mosaic.scdn.co/640/abc",
            "https://image-cdn-ak.spotifycdn.com/image/actual",
        ] {
            let encoded: String = url::form_urlencoded::byte_serialize(source.as_bytes()).collect();
            let uri = format!("http://cover.localhost/{encoded}").parse().unwrap();
            assert_eq!(source_url(&uri).unwrap(), source);
        }
        for rejected in [
            "file://secret",
            "http://i.scdn.co/image/insecure",
            "https://example.com/art/evil.jpg",
            "https://127.0.0.1/internal",
            "https://i.scdn.co:8443/image/port",
            "https://user@i.scdn.co/image/creds",
            "https://scdn.co/bare",
            "https://notscdn.co/spoof",
        ] {
            let encoded: String = url::form_urlencoded::byte_serialize(rejected.as_bytes()).collect();
            let uri = format!("http://cover.localhost/{encoded}").parse().unwrap();
            assert!(source_url(&uri).is_err(), "{rejected} must be rejected");
        }
    }

    #[test]
    fn cover_cache_prune_evicts_oldest_files_down_to_target() {
        let directory = std::env::temp_dir().join(format!("renderer-cover-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let old = directory.join("old");
        let mid = directory.join("mid");
        let fresh = directory.join("fresh");
        std::fs::write(&old, vec![1u8; 60]).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        std::fs::write(&mid, vec![2u8; 60]).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        std::fs::write(&fresh, vec![3u8; 60]).unwrap();
        prune_cover_cache(&directory, 150, 100);
        assert!(!old.exists(), "oldest entry should be evicted first");
        assert!(!mid.exists(), "second oldest entry should be evicted to reach target");
        assert!(fresh.exists(), "newest entry should remain");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn image_responses_revalidate_without_sending_pixels() {
        let bytes = b"GIF89a".to_vec();
        let first = image_response(bytes.clone(), "image-key", None);
        assert_eq!(first.status(), 200);
        assert_eq!(first.body().as_ref(), bytes);
        assert_eq!(first.headers()["Content-Type"], "image/gif");
        let second = image_response(bytes.clone(), "image-key", Some("W/\"image-key\""));
        assert_eq!(second.status(), 304);
        assert!(second.body().is_empty());
        assert_eq!(image_response(bytes, "image-key", Some("\"other\"")).status(), 200);
    }

    #[tokio::test]
    async fn a_missing_or_corrupt_cover_is_fetched_and_then_survives_offline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = format!("http://{}/image", listener.local_addr().unwrap());
        let key = cache_key(&source);
        let directory = std::env::temp_dir().join(format!("renderer-cover-{}-{key}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(&key), b"<html>error</html>").unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            let bytes = test_png();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
        });
        assert_eq!(image_bytes(&directory, &source, &key).await.unwrap(), test_png());
        server.await.unwrap();
        assert_eq!(image_bytes(&directory, &source, &key).await.unwrap(), test_png());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn interrupted_supported_cache_body_is_rejected_and_failed_publish_keeps_good_image() {
        let directory = std::env::temp_dir().join(format!("renderer-cover-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("key");
        let png = test_png();
        write_cover(&path, &png).unwrap();
        assert_eq!(cached_image(&path).unwrap(), png);
        let result = write_cover_with(&path, &test_png_color([0, 0, 255, 255]), |_, _| {
            Err(std::io::Error::other("injected replacement failure"))
        });
        assert!(result.is_err());
        assert_eq!(cached_image(&path).unwrap(), png);
        let mut interrupted = std::fs::read(&path).unwrap();
        interrupted.truncate(COVER_CACHE_HEADER + 8);
        std::fs::write(&path, interrupted).unwrap();
        assert!(cached_image(&path).is_none());
        assert!(!path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    async fn one_image_server(bytes: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = format!("http://{}/image", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
        });
        (source, server)
    }

    #[tokio::test]
    async fn valid_download_is_served_when_cache_directory_is_unwritable() {
        let png = test_png();
        let (source, server) = one_image_server(png.clone()).await;
        let key = cache_key(&source);
        let directory = std::env::temp_dir().join(format!("renderer-cover-write-failure-{}-{key}", std::process::id()));
        std::fs::write(&directory, b"not a directory").unwrap();
        let downloaded = image_bytes(&directory, &source, &key).await.unwrap();
        let response = image_response(downloaded, &key, None);
        assert_eq!(response.status(), 200);
        assert_eq!(response.body().as_ref(), png);
        server.await.unwrap();
        std::fs::remove_file(directory).unwrap();
    }

    #[tokio::test]
    async fn header_only_download_is_not_served_or_persisted() {
        let (source, server) = one_image_server(vec![0xFF, 0xD8, 0xFF]).await;
        let key = cache_key(&source);
        let directory = std::env::temp_dir().join(format!("renderer-cover-truncated-{}-{key}", std::process::id()));
        assert!(image_bytes(&directory, &source, &key).await.is_err());
        assert!(!directory.join(&key).exists());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn truncated_validated_cache_refetches_complete_pixels_and_then_works_offline() {
        let png = test_png();
        let (source, server) = one_image_server(png.clone()).await;
        let key = cache_key(&source);
        let directory = std::env::temp_dir().join(format!("renderer-cover-refetch-{}-{key}", std::process::id()));
        let path = directory.join(&key);
        write_cover(&path, &png).unwrap();
        let mut damaged = std::fs::read(&path).unwrap();
        damaged.truncate(COVER_CACHE_HEADER + 8);
        std::fs::write(&path, damaged).unwrap();
        assert_eq!(image_bytes(&directory, &source, &key).await.unwrap(), png);
        server.await.unwrap();
        assert_eq!(image_bytes(&directory, &source, &key).await.unwrap(), png);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
