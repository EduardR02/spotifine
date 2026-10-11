//! Direct cover delivery: the image URL carries its source, and a cache miss
//! downloads it in the asynchronous protocol handler. Disk and HTTP caches are
//! sufficient; no IPC resolution or second in-memory image cache is needed.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

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

fn http_client() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(COVER_FETCH_TIMEOUT)
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
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("cover source must be an HTTP URL".to_owned());
    }
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
        renderer_engine::atomic::replace_file_atomically(source, destination, false)
    })
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
    if let Some(bytes) = tauri::async_runtime::spawn_blocking(move || cached_image(&read_path))
        .await
        .map_err(|error| error.to_string())?
    {
        return Ok(bytes);
    }
    let response = http_client()
        .get(source)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    // Decode once off the async worker, before trusting the downloaded body.
    // Cache hits need only their complete validated envelope, not pixels again.
    tauri::async_runtime::spawn_blocking(move || {
        if sniff_content_type(&bytes).is_none() {
            return Err("cover source returned a non-image body".to_owned());
        }
        image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format().map_err(|_| "cover source returned an invalid image".to_owned())?
            .decode().map_err(|_| "cover source returned an incomplete or invalid image".to_owned())?;
        if let Err(error) = write_cover(&path, &bytes) {
            eprintln!("Spotifine: {error}");
        }
        Ok(bytes.to_vec())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn image_response(bytes: Vec<u8>, key: &str, if_none_match: Option<&str>) -> Response<Cow<'static, [u8]>> {
    let etag = format!("\"{key}\"");
    let revalidated = if_none_match.is_some_and(|value| {
        value.split(',').any(|tag| {
            let tag = tag.trim();
            tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag
        })
    });
    let response = Response::builder()
        .header("Cache-Control", CACHE_CONTROL)
        .header("ETag", etag)
        .header("Access-Control-Allow-Origin", "*");
    if revalidated {
        response.status(304).body(Cow::Borrowed(&[][..]))
    } else {
        response
            .status(200)
            .header("Content-Type", sniff_content_type(&bytes).expect("only validated image bytes are served"))
            .body(Cow::Owned(bytes))
    }
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
    match image_bytes(&data_dir().join("covers"), &source, &key).await {
        Ok(bytes) => image_response(
            bytes,
            &key,
            request.headers().get("If-None-Match").and_then(|value| value.to_str().ok()),
        ),
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
        let source = "https://example.com/art/a+b.jpg?size=640&name=a%20b";
        let encoded: String = url::form_urlencoded::byte_serialize(source.as_bytes()).collect();
        let uri = format!("http://cover.localhost/{encoded}").parse().unwrap();
        assert_eq!(source_url(&uri).unwrap(), source);
        assert!(source_url(&"cover://localhost/file%3A%2F%2Fsecret".parse().unwrap()).is_err());
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
