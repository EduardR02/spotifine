//! Spotify's authenticated listening-event transport, separate from local history.
//!
//! Wire fields follow the installed desktop client's `RawCoreStream`, gabito and
//! AUDIO_FILES descriptors. The closed librespot PR #1759 is a protocol reference,
//! not an accounting source: the caller supplies actual rendered and nominal time.
//! No playback/metadata work runs on the audio callback. Dropping `report` cancels
//! metadata, authentication, backoff and HTTP waits; callers own account/opt-out
//! cancellation and must never resubmit an old report with another reporter.
//! Audio identity lookup must confirm the resolved entity owns the rendered
//! file and format. Changed catalog metadata can therefore reject a cached-file
//! listen rather than sending a guessed identity. Metadata and receiver bodies
//! are capped before collection; upstream token APIs retain their own body
//! handling, with their entire waits bounded here.
//!
//! Receiver schema-compatibility identifiers below come from desktop 1.2.96.518
//! and that reference, NOT the installed app or this renderer's version. SDK,
//! bundle and stack labels declare the receiver dialect, not our implementation.
//! Device labels identify the software endpoint, not physical hardware.
//! Audio-only zero counters and no-parent/local-control sentinels describe this
//! renderer. An accepted event is not proof of ingestion.

use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::{Bytes, BytesMut};
use flate2::{Compression, write::GzEncoder};
use http::{Method, Request};
use http_body_util::BodyExt;
use librespot_core::{FileId, Session, SpotifyUri, config, error::ErrorKind};
use librespot_metadata::audio::AudioFileFormat;
use librespot_protocol::extended_metadata::{BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::metadata::AudioFile;
use protobuf::{CodedInputStream, CodedOutputStream, EnumOrUnknown, Message};
use sha1::{Digest, Sha1};
use tokio::sync::watch;
use uuid::Uuid;

const ENDPOINT: &str = "https://spclient.wg.spotify.com/gabo-receiver-service/v3/events/";
const REPORTING_VERSION: &str = "1.2.96.518";
const REPORTING_VERSION_CODE: i64 = 129600518;
const REPORTING_CORE_VERSION: i64 = 6005400000002005;
const REPORTING_SDK: &str = "0.9.4-rl-essopt-loginsend-onlinesend-bcdsend-heartbeat300.0s/30.0s-modern-payload125kB-batch100";
const ATTEMPTS: usize = 3;
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);
const REPORT_TIMEOUT: Duration = Duration::from_secs(60);
// One entity and one event are requested. Leave generous schema-growth room,
// but never collect arbitrary server bodies or unbounded rejection diagnostics.
const METADATA_LIMIT: usize = 128 * 1024;
const RESPONSE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndReason {
    TrackDone,
    EndPlay,
    ForwardButton,
    BackButton,
}

impl EndReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::TrackDone => "trackdone",
            Self::EndPlay => "endplay",
            Self::ForwardButton => "fwdbtn",
            Self::BackButton => "backbtn",
        }
    }
}

/// One completed Spotify audio segment. `uri` is the resolved playable URI,
/// including relinking, and `file_id`/`audio_format` describe the actual file.
#[derive(Debug)]
pub struct PlaybackReport {
    pub uri: SpotifyUri,
    pub file_id: FileId,
    pub audio_format: AudioFileFormat,
    pub context_uri: Option<String>,
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
    pub played: Duration,
    pub nominal_played: Duration,
    pub reason_start: &'static str,
    pub reason_end: EndReason,
    pub shuffle: bool,
    pub repeat_track: bool,
    pub repeat_context: bool,
}

/// Construct once per authenticated account/playback delivery task. All event
/// identities, compression and retries live here, never in the output callback.
pub struct SpotifyReporter {
    session: Session,
    account: String,
    app_session_id: [u8; 16],
    sequence_id: [u8; 20],
    sequence: i64,
    clock: Instant,
}

struct PreparedReport {
    body: Bytes,
    attempts: usize,
}

impl SpotifyReporter {
    pub fn new(session: Session) -> Self {
        let app_session_id = *Uuid::new_v4().as_bytes();
        Self {
            account: session.username(),
            session,
            app_session_id,
            sequence_id: Sha1::digest(app_session_id).into(),
            sequence: 0,
            clock: Instant::now(),
        }
    }

    fn update_session(&mut self, session: Session) -> Result<(), String> {
        check_account_binding(&session, &self.account)?;
        self.session = session;
        Ok(())
    }

    /// Bounded transient retries reuse the exact compressed payload and event
    /// sequence. Errors contain no tokens, credentials or server response bodies.
    /// Same-account session updates interrupt any stage and refresh credentials,
    /// retaining prepared event bytes, identities and the total attempt budget.
    /// All handovers share one deadline; account changes fail without rebinding.
    pub async fn report(
        &mut self, report: &PlaybackReport, sessions: &mut watch::Receiver<Session>,
    ) -> Result<(), String> {
        validate_report(report)?;
        if report.played.is_zero() {
            return Ok(());
        }
        let mut prepared = None;
        tokio::time::timeout(REPORT_TIMEOUT, async {
            loop {
                self.update_session(sessions.borrow_and_update().clone())?;
                if prepared.as_ref().is_some_and(|event: &PreparedReport| event.attempts >= ATTEMPTS) {
                    return Err("Spotify listening retry attempts exhausted".to_owned());
                }
                // AP invalidation is not credential revocation. A handover
                // shuts the old AP down before publishing its replacement.
                if self.session.is_invalid() {
                    sessions.changed().await
                        .map_err(|_| "Spotify listening session updates ended".to_owned())?;
                    continue;
                }
                let result = tokio::select! {
                    biased;
                    changed = sessions.changed() => {
                        changed.map_err(|_| "Spotify listening session updates ended".to_owned())?;
                        continue;
                    }
                    result = self.report_inner(report, &mut prepared) => result,
                };
                if result.is_err() && self.session.is_invalid() {
                    sessions.changed().await
                        .map_err(|_| "Spotify listening session updates ended".to_owned())?;
                    continue;
                }
                return result;
            }
        }).await
            .map_err(|_| "Spotify listening report deadline exceeded".to_owned())?
    }

    async fn report_inner(
        &mut self, report: &PlaybackReport, prepared: &mut Option<PreparedReport>,
    ) -> Result<(), String> {
        self.check_account()?;
        let token = tokio::time::timeout(ATTEMPT_TIMEOUT, self.session.login5().auth_token())
            .await
            .map_err(|_| "Spotify listening authentication deadline exceeded".to_owned())?
            .map_err(|error| safe_transport_error("authentication", &error).message)?;
        self.check_account()?;
        let client_token = tokio::time::timeout(ATTEMPT_TIMEOUT, self.session.spclient().client_token())
            .await
            .map_err(|_| "Spotify listening client authentication deadline exceeded".to_owned())?
            .map_err(|error| safe_transport_error("client authentication", &error).message)?;
        self.check_account()?;
        let authorization = format!("{} {}", token.token_type, token.access_token);
        if prepared.is_none() {
            let audio_id = tokio::time::timeout(
                ATTEMPT_TIMEOUT, self.audio_identity(report, &authorization, &client_token),
            ).await
                .map_err(|_| "Spotify listening audio identity deadline exceeded".to_owned())??;
            self.check_account()?;
            self.sequence = self.sequence.checked_add(1)
                .ok_or("Spotify listening sequence exhausted")?;
            let stream = stream_message(report, &audio_id, &self.app_session_id)?;
            let payload = event_request(
                &self.session, &stream, &self.app_session_id, &self.sequence_id,
                self.sequence, report.ended_at, self.clock.elapsed(),
            )?;
            *prepared = Some(PreparedReport { body: gzip(&payload)?, attempts: 0 });
        }
        let prepared = prepared.as_mut().expect("report prepared before publishing");
        let body = prepared.body.clone();
        let request = Request::builder()
            .method(Method::POST)
            .uri(ENDPOINT)
            .header("content-type", "application/x-protobuf")
            .header("accept", "application/x-protobuf")
            .header("content-encoding", "gzip")
            .header("app-platform", match config::OS {
                "windows" => "Win32",
                "macos" => "OSX",
                _ => "Linux",
            })
            .header("spotify-app-version", REPORTING_VERSION_CODE.to_string())
            .header("authorization", authorization)
            .header("client-token", client_token)
            .body(body.clone())
            .map_err(|_| "Spotify listening request construction failed".to_owned())?;
        let session = &self.session;
        let account = &self.account;
        publish_with_retry(body, Duration::from_secs(1), &mut prepared.attempts, |body| {
            let mut request = request.clone();
            *request.body_mut() = body;
            async move {
                check_account_binding(session, account).map_err(Failure::permanent)?;
                if session.is_invalid() {
                    return Err(Failure::transient("Spotify listening session replaced"));
                }
                // Session's HTTP client can itself wait on Retry-After; bound that
                // wait too, rather than allowing a 429 to retry indefinitely.
                let response = tokio::time::timeout(
                    ATTEMPT_TIMEOUT, request_bounded(session, request, RESPONSE_LIMIT),
                ).await
                    .map_err(|_| Failure::transient("Spotify listening HTTP deadline exceeded"))??;
                check_account_binding(session, account).map_err(Failure::permanent)?;
                Ok(response)
            }
        }).await
    }

    async fn audio_identity(
        &self, report: &PlaybackReport, authorization: &str, client_token: &str,
    ) -> Result<Vec<u8>, String> {
        let query = BatchedEntityRequest {
            entity_request: vec![EntityRequest {
                entity_uri: report.uri.to_string(),
                query: vec![ExtensionQuery {
                    extension_kind: EnumOrUnknown::new(ExtensionKind::AUDIO_FILES),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let body = query.write_to_bytes()
            .map_err(|_| "Spotify listening audio identity request encoding failed".to_owned())?;
        let base_url = self.session.spclient().base_url().await
            .map_err(|error| safe_transport_error("audio identity endpoint", &error).message)?;
        self.check_account()?;
        // Use the session endpoint, HTTP client and credentials, but collect its
        // response here: upstream get_metadata collects without a size bound and
        // pops an entry without checking its entity, extension kind or status.
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{base_url}/extended-metadata/v0/extended-metadata"))
            .header("content-type", "application/x-protobuf")
            .header("accept", "application/x-protobuf")
            .header("authorization", authorization)
            .header("client-token", client_token)
            .body(Bytes::from(body))
            .map_err(|_| "Spotify listening audio identity request construction failed".to_owned())?;
        let response = request_bounded(&self.session, request, METADATA_LIMIT).await
            .map_err(|error| error.message)?;
        parse_audio_identity_response(&response, report)
    }

    fn check_account(&self) -> Result<(), String> {
        check_account_binding(&self.session, &self.account)
    }
}

fn check_account_binding(session: &Session, account: &str) -> Result<(), String> {
    if account.is_empty() || session.username() != account {
        return Err("Spotify listening account changed or is unauthenticated".to_owned());
    }
    Ok(())
}

fn timestamp(time: SystemTime) -> Result<i64, String> {
    let duration = time.duration_since(UNIX_EPOCH)
        .map_err(|_| "Spotify listening timestamp predates Unix epoch".to_owned())?;
    i64::try_from(duration.as_millis())
        .map_err(|_| "Spotify listening timestamp exceeds protocol range".to_owned())
}

fn milliseconds(duration: Duration) -> Result<i32, String> {
    i32::try_from(duration.as_millis())
        .map_err(|_| "Spotify listening duration exceeds protocol range".to_owned())
}

fn validate_report(report: &PlaybackReport) -> Result<(), String> {
    if !matches!(report.uri, SpotifyUri::Track { .. } | SpotifyUri::Episode { .. }) {
        return Err("Only Spotify tracks and episodes can be reported".to_owned());
    }
    if report.file_id.0.iter().all(|byte| *byte == 0) {
        return Err("Spotify listening report has no media file identity".to_owned());
    }
    if !matches!(report.reason_start, "playbtn" | "trackdone" | "fwdbtn" | "backbtn") {
        return Err("Spotify listening report has an unsupported start reason".to_owned());
    }
    if report.ended_at < report.started_at {
        return Err("Spotify listening report ends before it starts".to_owned());
    }
    timestamp(report.started_at)?;
    timestamp(report.ended_at)?;
    milliseconds(report.played)?;
    milliseconds(report.nominal_played)?;
    Ok(())
}

/// Small schema-specific encoders follow collection.rs's wire-stream convention.
fn wire(encode: impl FnOnce(&mut CodedOutputStream<'_>) -> protobuf::Result<()>) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut out = CodedOutputStream::vec(&mut bytes);
        encode(&mut out).and_then(|()| out.flush())
            .map_err(|error| format!("Spotify listening protobuf encoding failed: {error}"))?;
    }
    Ok(bytes)
}

fn stream_message(report: &PlaybackReport, audio_id: &[u8], app_session_id: &[u8; 16]) -> Result<Vec<u8>, String> {
    let start = timestamp(report.started_at)?;
    let end = timestamp(report.ended_at)?;
    let played = milliseconds(report.played)?;
    let nominal = milliseconds(report.nominal_played)?;
    let content_uri = report.uri.to_string();
    wire(|out| {
        out.write_bytes(1, Uuid::new_v4().as_bytes())?;
        // Each report is an independent, locally controlled audio playback.
        // Reference no-parent/no-video sentinels are explicit, not invented IDs.
        out.write_bytes(2, &[0; 16])?;
        out.write_string(3, "")?;
        out.write_bytes(4, &report.file_id.0)?;
        out.write_string(5, "audio")?;
        out.write_string(9, "spotifine")?;
        out.write_string(10, report.reason_start)?;
        out.write_string(11, "spotifine")?;
        out.write_string(12, report.reason_end.as_str())?;
        out.write_int64(13, start)?;
        out.write_int32(14, played)?;
        out.write_int32(15, nominal)?;
        // One file feeds native output; WSOLA's internal sample overlap is not
        // overlapping media, video, fullscreen video or external playback.
        out.write_int32(16, 0)?;
        out.write_int32(17, 0)?;
        out.write_int32(19, 0)?;
        out.write_bool(20, false)?;
        out.write_bool(21, report.shuffle)?;
        out.write_string(22, audio_format_name(report.audio_format))?;
        // Local/search queues have no authoritative Spotify container. Do not
        // attribute their plays to an invented playlist, album, or track context.
        if let Some(context) = &report.context_uri {
            out.write_string(23, context)?;
        }
        out.write_string(28, if report.context_uri.is_some() { "context" } else { "unknown" })?;
        out.write_string(24, &content_uri)?;
        out.write_string(25, "")?;
        // These describe this client, not an account-wide Spotify setting:
        // it has no offline-download/private-session mode. Opt-out suppresses
        // the whole event; transient audio cache hits are not offline downloads.
        out.write_bool(26, false)?;
        out.write_bool(27, false)?;
        out.write_string(29, "unknown")?;
        // This renderer applies no streaming-rule policy and has no social or
        // remote Connect playback. Reports originate in an authenticated session.
        out.write_string(32, "none")?;
        out.write_int64(37, REPORTING_CORE_VERSION)?;
        // Receiver compatibility labels; source_start/end still identify us.
        out.write_string(38, "full")?;
        // This is an assumption flag: this standalone renderer requires Premium.
        out.write_bool(39, true)?;
        out.write_int32(40, 0)?;
        out.write_bool(42, false)?;
        out.write_bool(43, false)?;
        out.write_string(44, "local")?;
        out.write_string(45, "boombox")?;
        out.write_int32(46, 0)?;
        // The counters already exclude discarded/cut samples. No further
        // trimming may be subtracted from this measured listening time.
        out.write_int32(50, 0)?;
        out.write_int32(51, 0)?;
        out.write_bytes(53, Uuid::new_v4().as_bytes())?;
        out.write_bool(54, report.repeat_track)?;
        out.write_bool(55, report.repeat_context)?;
        out.write_int64(56, end)?;
        out.write_string(59, &hex::encode(app_session_id))?;
        // RawCoreStream's bytes field carries ASCII hex, unlike media_id.
        out.write_bytes(63, hex::encode(audio_id).as_bytes())?;
        out.write_int32(67, 0)?;
        out.write_string(69, "boombox")?;
        out.write_string(71, "context-player")?;
        out.write_string(78, "Spotifine")?;
        out.write_string(79, "Spotifine")?;
        out.write_string(80, "computer")?;
        Ok(())
    })
}

fn audio_format_name(format: AudioFileFormat) -> &'static str {
    use AudioFileFormat::*;
    match format {
        OGG_VORBIS_96 => "Vorbis 96 kbps",
        OGG_VORBIS_160 => "Vorbis 160 kbps",
        OGG_VORBIS_320 => "Vorbis 320 kbps",
        MP3_256 => "MP3 256 kbps",
        MP3_320 => "MP3 320 kbps",
        MP3_160 => "MP3 160 kbps",
        MP3_96 => "MP3 96 kbps",
        MP3_160_ENC => "MP3 160 kbps",
        AAC_24 => "AAC 24 kbps",
        AAC_48 => "AAC 48 kbps",
        AAC_160 => "AAC 160 kbps",
        AAC_320 => "AAC 320 kbps",
        XHE_AAC_24 => "xHE-AAC 24 kbps",
        XHE_AAC_16 => "xHE-AAC 16 kbps",
        XHE_AAC_12 => "xHE-AAC 12 kbps",
        FLAC_FLAC => "FLAC",
        FLAC_FLAC_24BIT => "FLAC 24 bit",
        MP4_128 => "MP4 128 kbps",
        OTHER5 => "OTHER5",
    }
}

fn fragment(out: &mut CodedOutputStream<'_>, name: &str, data: &[u8]) -> protobuf::Result<()> {
    let mut bytes = Vec::new();
    {
        let mut fragment = CodedOutputStream::vec(&mut bytes);
        fragment.write_string(1, name)?;
        fragment.write_bytes(2, data)?;
        fragment.flush()?;
    }
    out.write_bytes(3, &bytes)
}

fn event_request(
    session: &Session, stream: &[u8], app_session_id: &[u8; 16], sequence_id: &[u8; 20],
    sequence: i64, ended_at: SystemTime, elapsed: Duration,
) -> Result<Vec<u8>, String> {
    let client_id = hex::decode(session.client_id())
        .map_err(|_| "Spotify listening client ID is not hexadecimal".to_owned())?;
    if client_id.len() != 16 {
        return Err("Spotify listening client ID must be 16 bytes".to_owned());
    }
    let client = wire(|out| out.write_bytes(1, &client_id))?;
    let installation = wire(|out| out.write_bytes(1, &Sha1::digest(session.device_id().as_bytes())[..16]))?;
    let application = wire(|out| {
        out.write_string(1, REPORTING_VERSION)?;
        out.write_int64(2, REPORTING_VERSION_CODE)?;
        out.write_bytes(3, app_session_id)
    })?;
    let device = wire(|out| {
        out.write_string(1, config::OS)?;
        out.write_string(2, "Spotifine")?;
        out.write_string(3, "Spotifine")?;
        out.write_string(4, session.device_id())?;
        out.write_string(5, &config::os_version())
    })?;
    let end = timestamp(ended_at)?;
    let time = wire(|out| out.write_int64(1, end))?;
    let monotonic_ms = i64::try_from(elapsed.as_millis())
        .map_err(|_| "Spotify listening monotonic clock exceeds protocol range".to_owned())?;
    let monotonic = wire(|out| {
        out.write_int64(1, 1)?;
        out.write_int64(2, monotonic_ms)
    })?;
    let sdk = wire(|out| {
        out.write_string(1, REPORTING_SDK)?;
        out.write_string(2, "cpp")
    })?;
    let envelope = wire(|out| {
        out.write_string(2, "RawCoreStream")?;
        for (name, data) in [
            ("message", stream),
            ("context_client_id", client.as_slice()),
            ("context_installation_id", installation.as_slice()),
            ("context_application_desktop", application.as_slice()),
            ("context_device_desktop", device.as_slice()),
            ("context_time", time.as_slice()),
            ("context_monotonic_clock", monotonic.as_slice()),
            ("context_sdk", sdk.as_slice()),
            ("context_client_context_id", &[]),
        ] {
            fragment(out, name, data)?;
        }
        out.write_bytes(4, sequence_id)?;
        out.write_int64(5, sequence)
    })?;
    wire(|out| out.write_bytes(1, &envelope))
}

fn gzip(payload: &[u8]) -> Result<Bytes, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload)
        .map_err(|error| format!("Spotify listening compression failed: {error}"))?;
    encoder.finish().map(Bytes::from)
        .map_err(|error| format!("Spotify listening compression failed: {error}"))
}

fn parse_metadata<M: Message>(payload: &[u8]) -> Result<M, String> {
    if payload.len() > METADATA_LIMIT {
        return Err("Spotify audio identity metadata exceeds size limit".to_owned());
    }
    let parse = || -> protobuf::Result<M> {
        let mut input = CodedInputStream::from_bytes(payload);
        // from_bytes alone has no logical limit. Enforce the actual slice size
        // before a malicious length prefix can trigger a larger allocation.
        input.push_limit(payload.len() as u64)?;
        M::parse_from(&mut input)
    };
    parse().map_err(|_| "Spotify audio identity metadata is malformed".to_owned())
}

fn parse_audio_identity_response(payload: &[u8], report: &PlaybackReport) -> Result<Vec<u8>, String> {
    let response: BatchedExtensionResponse = parse_metadata(payload)?;
    let uri = report.uri.to_string();
    let mut matching = None;
    for array in &response.extended_metadata {
        if array.extension_kind.enum_value() != Ok(ExtensionKind::AUDIO_FILES) {
            continue;
        }
        if !matches!(array.header.provider_error_status, 0 | 200) {
            return Err("Spotify audio identity provider rejected lookup".to_owned());
        }
        for entry in &array.extension_data {
            if entry.entity_uri != uri {
                continue;
            }
            if matching.is_some() {
                return Err("Spotify audio identity response contains duplicate entities".to_owned());
            }
            if entry.header.status_code != 200 {
                return Err(format!("Spotify audio identity lookup failed: HTTP {}", entry.header.status_code));
            }
            matching = Some(entry.extension_data.as_ref()
                .ok_or("Spotify audio identity response has no extension payload")?);
        }
    }
    let data = matching.ok_or("Spotify audio identity response has no matching entity")?;
    parse_audio_id(&data.value, report)
}

fn parse_audio_id(payload: &[u8], report: &PlaybackReport) -> Result<Vec<u8>, String> {
    if payload.len() > METADATA_LIMIT {
        return Err("Spotify audio identity metadata exceeds size limit".to_owned());
    }
    let parse = || -> protobuf::Result<(Vec<u8>, bool)> {
        let mut input = CodedInputStream::from_bytes(payload);
        input.push_limit(payload.len() as u64)?;
        let mut audio_id = Vec::new();
        let mut matches = false;
        while let Some(tag) = input.read_raw_tag_or_eof()? {
            match tag {
                // AUDIO_FILES entries wrap metadata.AudioFile in field one;
                // sibling fields include bitrate. The pinned protocol crate
                // provides the inner message, but not this extension wrapper.
                10 => {
                    let length = input.read_raw_varint64()?;
                    let previous_limit = input.push_limit(length)?;
                    let mut file = AudioFile::new();
                    while let Some(tag) = input.read_raw_tag_or_eof()? {
                        match tag {
                            10 => input.merge_message(&mut file)?,
                            other => protobuf::rt::skip_field_for_tag(other, &mut input)?,
                        }
                    }
                    input.pop_limit(previous_limit);
                    matches |= file.file_id() == report.file_id.0
                        && file.format.is_some_and(|format| {
                            AudioFileFormat::try_from(format.value()) == Ok(report.audio_format)
                        });
                }
                34 => input.read_bytes_into(&mut audio_id)?,
                other => protobuf::rt::skip_field_for_tag(other, &mut input)?,
            }
        }
        Ok((audio_id, matches))
    };
    let (audio_id, matches) = parse()
        .map_err(|_| "Spotify audio identity metadata is malformed".to_owned())?;
    if audio_id.is_empty() {
        return Err("Spotify audio metadata has no audio identity".to_owned());
    }
    // audio_id belongs to the entity, not an arbitrary format in its file list.
    // Confirm that this entity actually owns the file/format rendered natively.
    if !matches {
        return Err("Spotify audio identity metadata does not match the rendered file".to_owned());
    }
    Ok(audio_id)
}

async fn request_bounded(
    session: &Session, request: Request<Bytes>, limit: usize,
) -> Result<Bytes, Failure> {
    let response = session.http_client().request(request).await
        .map_err(|error| safe_transport_error("HTTP request", &error))?;
    if response.headers().get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Failure::permanent("Spotify listening response exceeds size limit"));
    }
    collect_bounded(response.into_body(), limit).await
}

async fn collect_bounded(
    mut body: impl BodyExt<Data = Bytes> + Unpin, limit: usize,
) -> Result<Bytes, Failure> {
    let mut bytes = BytesMut::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| Failure::transient("Spotify listening HTTP body read failed"))?;
        if let Ok(data) = frame.into_data() {
            if data.len() > limit.saturating_sub(bytes.len()) {
                return Err(Failure::permanent("Spotify listening response exceeds size limit"));
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes.freeze())
}

#[derive(Debug)]
struct Failure {
    message: String,
    transient: bool,
}

impl Failure {
    fn permanent(message: impl Into<String>) -> Self {
        Self { message: message.into(), transient: false }
    }

    fn transient(message: impl Into<String>) -> Self {
        Self { message: message.into(), transient: true }
    }
}

fn safe_transport_error(operation: &str, error: &librespot_core::Error) -> Failure {
    use librespot_core::http_client::HttpClientError;
    let status = match error.error.downcast_ref::<HttpClientError>() {
        Some(HttpClientError::StatusCode(status)) => Some(status.as_u16()),
        _ => None,
    };
    let transient = match status {
        Some(status) => status == 408 || status == 429 || (500..=599).contains(&status),
        None => matches!(error.kind, ErrorKind::Unavailable | ErrorKind::DeadlineExceeded | ErrorKind::Aborted | ErrorKind::DataLoss),
    };
    Failure {
        message: match status {
            Some(status) => format!("Spotify listening {operation} failed: HTTP {status}"),
            None => format!("Spotify listening {operation} failed: {}", error.kind),
        },
        transient,
    }
}

/// HTTP 200 can contain multiple event errors. Any permanent error wins over
/// transient ones, and malformed/out-of-batch errors must never count as success.
fn check_response(payload: &[u8]) -> Result<(), Failure> {
    if payload.len() > RESPONSE_LIMIT {
        return Err(Failure::permanent("Spotify listening response exceeds size limit"));
    }
    let parse = || -> protobuf::Result<Option<(i32, bool, i32)>> {
        let mut rejection = None;
        let mut input = CodedInputStream::from_bytes(payload);
        input.push_limit(payload.len() as u64)?;
        while let Some(tag) = input.read_raw_tag_or_eof()? {
            if tag != 10 {
                protobuf::rt::skip_field_for_tag(tag, &mut input)?;
                continue;
            }
            let length = input.read_raw_varint32()?;
            let limit = input.push_limit(u64::from(length))?;
            let (mut index, mut transient, mut reason) = (0, false, 0);
            while let Some(tag) = input.read_raw_tag_or_eof()? {
                match tag {
                    8 => index = input.read_int32()?,
                    16 => transient = input.read_bool()?,
                    24 => reason = input.read_int32()?,
                    other => protobuf::rt::skip_field_for_tag(other, &mut input)?,
                }
            }
            input.pop_limit(limit);
            let retryable = index == 0 && transient;
            // Keep a representative permanent error over a transient one.
            // Never allocate one diagnostic per server-supplied error.
            if rejection.is_none() || !retryable {
                rejection = Some((index, retryable, reason));
            }
        }
        Ok(rejection)
    };
    let rejection = parse().map_err(|_| Failure::permanent("Spotify listening response is malformed"))?;
    match rejection {
        None => Ok(()),
        Some((index, transient, reason)) => Err(Failure {
            message: format!("Spotify listening event rejected: index {index}, reason {reason}"),
            transient,
        }),
    }
}

async fn publish_with_retry<F, Fut>(
    body: Bytes, delay: Duration, attempts: &mut usize, mut publish: F,
) -> Result<(), String>
where
    F: FnMut(Bytes) -> Fut,
    Fut: Future<Output = Result<Bytes, Failure>>,
{
    while *attempts < ATTEMPTS {
        let attempt = *attempts;
        *attempts += 1;
        let result = match publish(body.clone()).await {
            Ok(response) => check_response(&response),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.transient && *attempts < ATTEMPTS => {
                tokio::time::sleep(delay * (1 << attempt)).await;
            }
            Err(error) => return Err(format!("{} ({} attempt(s))", error.message, attempt + 1)),
        }
    }
    Err("Spotify listening retry attempts exhausted".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> PlaybackReport {
        PlaybackReport {
            uri: SpotifyUri::from_uri("spotify:track:72AZ3V52rs9NfgMNhALxln").unwrap(),
            file_id: FileId([0x17; 20]),
            audio_format: AudioFileFormat::OGG_VORBIS_320,
            context_uri: Some("spotify:album:72AZ3V52rs9NfgMNhALxln".to_owned()),
            started_at: UNIX_EPOCH + Duration::from_millis(1_700_000_000_000),
            ended_at: UNIX_EPOCH + Duration::from_millis(1_700_000_180_000),
            played: Duration::from_millis(32_125),
            nominal_played: Duration::from_millis(48_187),
            reason_start: "backbtn",
            reason_end: EndReason::ForwardButton,
            shuffle: true,
            repeat_track: false,
            repeat_context: true,
        }
    }


    #[test]
    fn invalid_media_reasons_timestamps_and_duration_overflow_are_rejected() {
        let mut value = report();
        value.uri = SpotifyUri::from_uri("spotify:album:72AZ3V52rs9NfgMNhALxln").unwrap();
        assert!(validate_report(&value).is_err());
        let mut value = report();
        value.file_id = FileId([0; 20]);
        assert!(validate_report(&value).is_err());
        let mut value = report();
        value.reason_start = "unknown";
        assert!(validate_report(&value).is_err());
        let mut value = report();
        value.ended_at = value.started_at - Duration::from_millis(1);
        assert!(validate_report(&value).is_err());
        let mut value = report();
        value.started_at = UNIX_EPOCH - Duration::from_millis(1);
        assert!(validate_report(&value).is_err());
        for nominal in [false, true] {
            let mut value = report();
            let duration = Duration::from_millis(i32::MAX as u64 + 1);
            if nominal { value.nominal_played = duration; } else { value.played = duration; }
            assert!(validate_report(&value).is_err());
        }
        assert_eq!(milliseconds(Duration::from_millis(i32::MAX as u64)).unwrap(), i32::MAX);
    }

    #[test]
    fn audio_identity_requires_well_formed_nonempty_extension_field_four() {
        let value = report();
        assert!(parse_audio_id(&[], &value).is_err());
        assert!(parse_audio_id(&[34, 0], &value).is_err());
        assert!(parse_audio_id(&[34, 16, 1], &value).is_err());
        // An unrelated file message must not become an audio identity.
        assert!(parse_audio_id(&[10, 2, 8, 1], &value).is_err());
        let bytes = audio_metadata(&[(value.file_id, 2)]);
        assert_eq!(parse_audio_id(&bytes, &value).unwrap(), vec![0xab; 16]);
    }

    fn audio_metadata(files: &[(FileId, i32)]) -> Vec<u8> {
        wire(|out| {
            for (id, format) in files {
                let file = wire(|file| {
                    file.write_bytes(1, &id.0)?;
                    file.write_int32(2, *format)
                }).unwrap();
                let entry = wire(|entry| {
                    entry.write_bytes(1, &file)?;
                    entry.write_uint32(4, 320_000)
                }).unwrap();
                out.write_bytes(1, &entry)?;
            }
            out.write_bytes(4, &[0xab; 16])
        }).unwrap()
    }

    fn metadata_response(value: &PlaybackReport) -> BatchedExtensionResponse {
        let mut response = BatchedExtensionResponse::new();
        response.extended_metadata.push(Default::default());
        let array = &mut response.extended_metadata[0];
        array.extension_kind = EnumOrUnknown::new(ExtensionKind::AUDIO_FILES);
        array.extension_data.push(Default::default());
        let entry = &mut array.extension_data[0];
        entry.entity_uri = value.uri.to_string();
        entry.header.mut_or_insert_default().status_code = 200;
        entry.extension_data.mut_or_insert_default().value = audio_metadata(&[(value.file_id, 2)]);
        response
    }

    #[test]
    fn audio_identity_requires_the_actual_file_and_format_not_just_an_entity_audio_id() {
        let value = report();
        assert!(parse_audio_id(&audio_metadata(&[(FileId([0x18; 20]), 2)]), &value).is_err());
        assert!(parse_audio_id(&audio_metadata(&[(value.file_id, 1)]), &value).is_err());
        assert!(parse_audio_id(&audio_metadata(&[(value.file_id, 999)]), &value).is_err());
        // Several formats are normal; the selected file need not be first.
        let bytes = audio_metadata(&[(FileId([0x18; 20]), 1), (value.file_id, 2)]);
        assert_eq!(parse_audio_id(&bytes, &value).unwrap(), vec![0xab; 16]);
        let mut episode = report();
        episode.uri = SpotifyUri::from_uri("spotify:episode:72AZ3V52rs9NfgMNhALxln").unwrap();
        assert_eq!(parse_audio_id(&bytes, &episode).unwrap(), vec![0xab; 16]);
        // Unknown numeric enum values used by metadata still map through the
        // existing format conversion, rather than being mislabeled as Vorbis.
        episode.audio_format = AudioFileFormat::AAC_160;
        assert_eq!(parse_audio_id(&audio_metadata(&[(episode.file_id, 10)]), &episode).unwrap(), vec![0xab; 16]);
    }

    #[test]
    fn metadata_must_identify_the_requested_entity_and_successful_extension() {
        let value = report();
        let original = metadata_response(&value);
        let parse = |response: &BatchedExtensionResponse| {
            parse_audio_identity_response(&response.write_to_bytes().unwrap(), &value)
        };
        assert_eq!(parse(&original).unwrap(), vec![0xab; 16]);
        let mut wrong_entity = original.clone();
        wrong_entity.extended_metadata[0].extension_data[0].entity_uri =
            "spotify:track:0000000000000000000000".to_owned();
        assert!(parse(&wrong_entity).is_err());
        let mut wrong_kind = original.clone();
        wrong_kind.extended_metadata[0].extension_kind = EnumOrUnknown::new(ExtensionKind::TRACK_V4);
        assert!(parse(&wrong_kind).is_err());
        for status in [0, 401, 403, 404, 429, 500] {
            let mut response = original.clone();
            response.extended_metadata[0].extension_data[0].header
                .mut_or_insert_default().status_code = status;
            assert!(parse(&response).is_err(), "metadata status {status} is not success");
        }
        let mut provider_error = original.clone();
        provider_error.extended_metadata[0].header
            .mut_or_insert_default().provider_error_status = 500;
        assert!(parse(&provider_error).is_err());
        let mut duplicate = original.clone();
        duplicate.extended_metadata.push(original.extended_metadata[0].clone());
        assert!(parse(&duplicate).is_err());
        let mut missing = original.clone();
        missing.extended_metadata[0].extension_data[0].extension_data.clear();
        assert!(parse(&missing).is_err());
        // Unrelated trailing entries must not replace the requested entity.
        let mut response = original;
        response.extended_metadata.extend(wrong_entity.extended_metadata);
        assert_eq!(parse(&response).unwrap(), vec![0xab; 16]);
    }

    #[test]
    fn oversized_and_truncated_metadata_fail_without_exposing_remote_contents() {
        let value = report();
        for payload in [
            vec![0; METADATA_LIMIT + 1],
            vec![34, 0xff, 0xff, 0xff, 0xff, 0x0f],
            vec![34, 1],
        ] {
            assert!(parse_audio_id(&payload, &value).is_err());
            assert!(parse_audio_identity_response(&payload, &value).is_err());
        }
        let error = parse_audio_id(b"private server body", &value).unwrap_err();
        assert!(!error.contains("private server body"));
    }

    #[tokio::test]
    async fn response_collection_enforces_limits_without_a_content_length() {
        use http_body_util::Full;
        let bytes = Bytes::from(vec![0; RESPONSE_LIMIT]);
        assert_eq!(collect_bounded(Full::new(bytes.clone()), RESPONSE_LIMIT).await.unwrap(), bytes);
        let error = collect_bounded(Full::new(Bytes::from(vec![0; RESPONSE_LIMIT + 1])), RESPONSE_LIMIT)
            .await.unwrap_err();
        assert!(!error.transient);
        assert!(collect_bounded(Full::new(Bytes::from_static(b"metadata")), 0).await.is_err());
    }

    #[test]
    fn rejection_diagnostics_stay_bounded_and_permanent_errors_win_in_either_order() {
        let transient = [10, 4, 16, 1, 24, 2];
        let permanent = [10, 2, 24, 3];
        for errors in [
            [transient.as_slice(), permanent.as_slice()].concat(),
            [permanent.as_slice(), transient.as_slice()].concat(),
        ] {
            let error = check_response(&errors).unwrap_err();
            assert!(!error.transient);
            assert!(error.message.contains("reason 3"));
        }
        let errors = transient.repeat(RESPONSE_LIMIT / transient.len());
        let error = check_response(&errors).unwrap_err();
        assert!(error.transient);
        assert!(error.message.len() < 128);
        for errors in [
            vec![0; RESPONSE_LIMIT + 1],
            vec![10, 0xff, 0xff, 0xff, 0xff, 0x0f],
            vec![10, 3, 16, 1],
        ] {
            assert!(!check_response(&errors).unwrap_err().transient);
        }
    }


    #[test]
    fn http_success_does_not_hide_event_errors_or_malformed_responses() {
        assert!(check_response(&[]).is_ok());
        let error = check_response(&[10, 2, 24, 2]).unwrap_err();
        assert!(!error.transient);
        assert!(error.message.contains("index 0, reason 2"));
        assert!(check_response(&[10, 4, 16, 1, 24, 2]).unwrap_err().transient);
        assert!(!check_response(&[10, 1, 24]).unwrap_err().transient);
        assert!(!check_response(&[10, 4, 8, 1, 16, 1]).unwrap_err().transient);
        assert!(!check_response(&[10, 2, 16, 1, 10, 2, 24, 2]).unwrap_err().transient);
    }

    #[tokio::test]
    async fn transient_network_and_event_errors_retry_identical_compressed_bytes() {
        let body = gzip(b"one immutable serialized event").unwrap();
        let mut attempts = 0;
        let mut retry_attempts = 0;
        publish_with_retry(body.clone(), Duration::ZERO, &mut retry_attempts, |sent| {
            attempts += 1;
            assert_eq!(sent, body);
            std::future::ready(match attempts {
                1 => Err(Failure::transient("temporary transport failure")),
                2 => Ok(Bytes::from_static(&[10, 4, 16, 1, 24, 2])),
                _ => Ok(Bytes::new()),
            })
        }).await.unwrap();
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn retries_are_bounded_and_permanent_errors_stop_immediately() {
        for transient in [false, true] {
            let mut attempts = 0;
            let mut retry_attempts = 0;
            let result = publish_with_retry(Bytes::from_static(b"event"), Duration::ZERO, &mut retry_attempts, |_| {
                attempts += 1;
                std::future::ready(Err(Failure { message: "rejected".into(), transient }))
            }).await;
            assert!(result.is_err());
            assert_eq!(attempts, if transient { ATTEMPTS } else { 1 });
        }
        let mut attempts = 0;
        let mut retry_attempts = 0;
        let result = publish_with_retry(Bytes::from_static(b"event"), Duration::ZERO, &mut retry_attempts, |_| {
            attempts += 1;
            std::future::ready(Ok(Bytes::from_static(&[10, 2, 24, 2])))
        }).await;
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn empty_audio_is_not_sent_and_unauthenticated_audio_fails_before_transport() {
        let mut reporter = SpotifyReporter::new(Session::new(Default::default(), None));
        let (_updates, mut sessions) = watch::channel(reporter.session.clone());
        let mut value = report();
        value.played = Duration::ZERO;
        value.nominal_played = Duration::ZERO;
        reporter.report(&value, &mut sessions).await.unwrap();
        value.played = Duration::from_millis(1);
        let error = reporter.report(&value, &mut sessions).await.unwrap_err();
        assert!(error.contains("unauthenticated"));
        assert_eq!(reporter.sequence, 0);
    }

    #[tokio::test]
    async fn authentication_refresh_cannot_rebind_an_event_stream_to_another_account() {
        let original = Session::new(Default::default(), None);
        original.set_username("listener");
        let mut reporter = SpotifyReporter::new(original);
        let replacement = Session::new(Default::default(), None);
        replacement.set_username("listener");
        reporter.update_session(replacement).unwrap();
        let other = Session::new(Default::default(), None);
        other.set_username("another-listener");
        assert!(reporter.update_session(other).is_err());
        assert_eq!(reporter.session.username(), "listener");
        assert!(reporter.update_session(Session::new(Default::default(), None)).is_err());
        assert_eq!(reporter.session.username(), "listener");
    }

    #[test]
    fn transient_status_classification_does_not_retry_auth_or_leak_nested_errors() {
        use librespot_core::http_client::HttpClientError;
        for (status, transient) in [
            (400, false), (401, false), (403, false), (408, true),
            (429, true), (500, true), (502, true), (503, true),
        ] {
            let error = HttpClientError::StatusCode(http::StatusCode::from_u16(status).unwrap()).into();
            assert_eq!(safe_transport_error("HTTP", &error).transient, transient);
        }
        for error in [
            librespot_core::Error::from(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
            librespot_core::Error::from(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
        ] {
            assert!(safe_transport_error("HTTP", &error).transient);
        }
        let error = librespot_core::Error::unavailable("private token must not appear");
        let failure = safe_transport_error("HTTP", &error);
        assert!(failure.transient);
        assert!(!failure.message.contains("private token"));
    }

    #[tokio::test]
    async fn same_account_ap_handover_waits_for_replacement_and_preserves_event_identity() {
        let original = Session::new(Default::default(), None);
        original.set_username("listener");
        let mut reporter = SpotifyReporter::new(original.clone());
        reporter.sequence = 41;
        let identities = (reporter.app_session_id, reporter.sequence_id);
        let (updates, mut sessions) = watch::channel(original.clone());
        original.set_username("another-listener");
        assert!(reporter.report(&report(), &mut sessions).await.unwrap_err().contains("account"));
        assert_eq!(reporter.sequence, 41);
        original.set_username("listener");
        original.shutdown();
        let value = report();
        let mut pending = Box::pin(reporter.report(&value, &mut sessions));
        tokio::select! {
            biased;
            _ = pending.as_mut() => panic!("handover must wait for its replacement"),
            _ = std::future::ready(()) => {}
        }
        let replacement = Session::new(Default::default(), None);
        replacement.set_username("listener");
        updates.send_replace(replacement);
        // The replacement has no stored credential. Reaching its auth failure,
        // rather than losing the report at AP shutdown, proves the handover.
        assert!(pending.await.unwrap_err().contains("authentication"));
        assert_eq!((reporter.app_session_id, reporter.sequence_id), identities);
        assert_eq!(reporter.sequence, 41);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_backoff_prevents_later_network_attempts() {
        let mut attempts = 0;
        let mut retry_attempts = 0;
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            publish_with_retry(Bytes::from_static(b"event"), Duration::from_secs(60), &mut retry_attempts, |_| {
                attempts += 1;
                std::future::ready(Err(Failure::transient("temporary")))
            }),
        ).await;
        assert!(result.is_err());
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn cancelling_an_inflight_publish_drops_its_wait_without_detached_work() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        struct WaitGuard(Arc<AtomicBool>);
        impl Drop for WaitGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let mut retry_attempts = 0;
        let mut pending = Box::pin(publish_with_retry(
            Bytes::from_static(b"event"), Duration::ZERO, &mut retry_attempts, |_| {
                let guard = WaitGuard(dropped.clone());
                async move {
                    let _guard = guard;
                    std::future::pending::<Result<Bytes, Failure>>().await
                }
            },
        ));
        tokio::select! {
            biased;
            _ = pending.as_mut() => panic!("publish should wait"),
            _ = std::future::ready(()) => {}
        }
        assert!(!dropped.load(Ordering::Relaxed));
        drop(pending);
        assert!(dropped.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn missing_ap_replacement_is_bounded_by_the_original_report_deadline() {
        let session = Session::new(Default::default(), None);
        session.set_username("listener");
        session.shutdown();
        let (_updates, mut sessions) = watch::channel(session.clone());
        let mut reporter = SpotifyReporter::new(session);
        let started = tokio::time::Instant::now();
        assert!(reporter.report(&report(), &mut sessions).await.unwrap_err().contains("deadline"));
        assert_eq!(started.elapsed(), REPORT_TIMEOUT);
        assert_eq!(reporter.sequence, 0);
    }

    #[tokio::test]
    async fn changed_account_interrupts_a_pending_handover_without_rebinding() {
        let session = Session::new(Default::default(), None);
        session.set_username("listener");
        session.shutdown();
        let (updates, mut sessions) = watch::channel(session.clone());
        let mut reporter = SpotifyReporter::new(session);
        let value = report();
        let mut pending = Box::pin(reporter.report(&value, &mut sessions));
        tokio::select! {
            biased;
            _ = pending.as_mut() => panic!("handover must wait"),
            _ = std::future::ready(()) => {}
        }
        let other = Session::new(Default::default(), None);
        other.set_username("another-listener");
        updates.send_replace(other);
        assert!(pending.await.unwrap_err().contains("account"));
        assert_eq!(reporter.account, "listener");
        assert_eq!(reporter.sequence, 0);
    }

    #[tokio::test]
    async fn interrupted_http_attempt_keeps_immutable_bytes_and_the_total_retry_budget() {
        let body = gzip(b"immutable event across authentication handover").unwrap();
        let mut attempts = 0;
        let mut pending = Box::pin(publish_with_retry(
            body.clone(), Duration::ZERO, &mut attempts, |sent| {
                assert_eq!(sent, body);
                std::future::pending::<Result<Bytes, Failure>>()
            },
        ));
        tokio::select! {
            biased;
            _ = pending.as_mut() => panic!("HTTP attempt must wait"),
            _ = std::future::ready(()) => {}
        }
        drop(pending);
        assert_eq!(attempts, 1);
        let mut resumed = 0;
        let error = publish_with_retry(body.clone(), Duration::ZERO, &mut attempts, |sent| {
            resumed += 1;
            assert_eq!(sent, body);
            std::future::ready(Err(Failure::transient("temporary")))
        }).await.unwrap_err();
        assert_eq!(resumed, ATTEMPTS - 1);
        assert_eq!(attempts, ATTEMPTS);
        assert!(error.contains("3 attempt(s)"));
    }
}
