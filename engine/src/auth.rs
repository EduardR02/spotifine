use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use librespot_core::Session;
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_playback::config::{
    AudioFormat, Bitrate, NormalisationMethod, NormalisationType, PlayerConfig, VolumeCtrl,
};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume};
use librespot_playback::player::{Player, PlayerEventChannel};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use url::Url;

use crate::audio::{OutputError, SinkOpener, SilentSink};

const OAUTH_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const OAUTH_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:5588/login";
const OAUTH_SCOPES: &[&str] = &["streaming", "user-read-private"];

/// Presentation for the callback tab.
///
/// The redirect target is the only surface of this application a stranger ever
/// sees. It used to be served as a bare sentence with no content type, which
/// browsers render as unstyled black-on-white — a successful login that looks
/// exactly like a crash. The style is hardcoded here and the page loads
/// nothing: reaching for `src/styles/app.css` is impossible from a socket that
/// serves one response and closes, and fetching anything remote would put a
/// network dependency on the last step of the flow. The literal values are the
/// app's own ground (`--bg-0`), pane (`--bg-1`), text (`--fg`, `--fg-2`) and
/// type stack, copied rather than referenced for that reason.
const OAUTH_PAGE_STYLE: &str = "\
:root{color-scheme:dark}\
html,body{height:100%}\
body{margin:0;display:grid;place-items:center;background:#080808;color:#f2f2f2;\
font-family:'Segoe UI Variable Text','Segoe UI',ui-sans-serif,system-ui,sans-serif;\
font-size:15px;line-height:1.55}\
main{box-sizing:border-box;width:min(420px,calc(100% - 48px));padding:32px;\
background:#121212;border-radius:14px}\
.rule{width:36px;height:3px;border-radius:2px}\
h1{margin:20px 0 8px;font-size:21px;font-weight:600;letter-spacing:-0.01em}\
p{margin:0;color:#a0a0a0}";
/// The signature foam→rose gradient (`--accent-grad`), for the one outcome
/// that earned it.
const OAUTH_SUCCESS_ACCENT: &str = "linear-gradient(90deg,#9ccfd8,#ebbcba)";
/// `--danger`. A cancelled or malformed callback is not an error the user has
/// to act on, but it is not the same event as success and must not look it.
const OAUTH_FAILED_ACCENT: &str = "#eb6f92";

/// An abandoned login attempt (browser never redirected) must not wedge the
/// engine in `Authenticating` forever; after this long the flow fails and the
/// engine returns to `NeedsLogin` with a fresh URL.
///
/// Twenty minutes, not the five this used to be. Five is generous for the
/// owner re-authorising an account his browser is already signed into, and
/// far too tight for the case this timeout actually exists for: a first login
/// on somebody else's machine, where the browser is signed out, the password
/// lives in a manager behind its own unlock, a 2FA code has to arrive on a
/// phone, and the account may not exist yet. Expiring under a user who is
/// still working through that costs them the whole attempt; waiting an extra
/// quarter of an hour for one that really was abandoned costs a held port.
const OAUTH_LISTENER_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// How long one connection to the callback port may hold the flow open without
/// sending a request line.
///
/// [`OAUTH_LISTENER_TIMEOUT`] bounds how long the *redirect* is waited for;
/// this bounds a single socket that has already connected. They are different
/// waits and only one of them used to exist. An accepted socket that never
/// writes blocks the request-line read, and that read had no clock at all, so
/// one silent connection — a browser holding a speculative socket, a scanner —
/// kept the flow (and the port) for as long as the socket lived, while the
/// engine sat in `Authenticating` believing the deadline still protected it.
///
/// A real redirect sends its request line with the connection: the browser has
/// the URL and opens the socket to use it. Ten seconds is far past that and
/// far short of a wait anyone notices.
const OAUTH_CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a device open may take before the engine treats it as a machine
/// with no usable output device. The engine's probe clock is held for the same
/// span, so one open runs at a time; see `engine::AudioUnavailable::retry_at`.
pub const AUDIO_START_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PlaybackHandles {
    pub player: Arc<Player>,
    pub events: PlayerEventChannel,
    pub mixer: Arc<SoftMixer>,
    pub volume_percent: u8,
}

/// A live Spotify session, and whether it has a player.
///
/// The two answers are separate because they fail separately. A machine with
/// no output device has a session that browses, searches and edits playlists
/// exactly as well as one with a device; the player is all that is missing, and
/// nothing about the account or the network is in question.
pub struct ConnectedSession {
    pub session: Session,
    /// `Ok` is a player. `Err` is a machine with no output device to open —
    /// the string names that and what to do about it, and the engine keeps the
    /// session, reports the message, and retries the device when one can be there.
    /// Every other playback failure arrives as an [`AuthFailure`] instead,
    /// with the session shut down.
    pub playback: Result<PlaybackHandles, String>,
}

/// Why player construction did not produce a player.
///
/// The distinction decides what happens to the session, and that is the whole
/// of it. A missing output device is a fact about the machine that the user can
/// change while the engine runs: the session stays up, browsing keeps working,
/// and the output device is retried until it appears. Everything else is a
/// construction failure that a retry would reproduce, and the session goes with
/// it — which is what a refused login gets too.
#[derive(Debug)]
pub enum PlaybackError {
    /// The output device could not be opened. The string is what the user is
    /// shown, and it names the cause and the way out.
    NoOutputDevice(String),
    /// The machine has a device, but opening it did not return within
    /// [`AUDIO_START_TIMEOUT`].
    ///
    /// Separate from [`Self::NoOutputDevice`] because of what the retry costs.
    /// The librespot player thread that ran that open is still inside cpal and
    /// will not come back, so it is parked for the life of the process: every
    /// further attempt leaves another one behind. The engine treats this as its
    /// own condition and retries it on a much longer clock
    /// (`engine::AUDIO_BLOCKED_PROBE_BACKOFF_MAX`) than an ordinary missing
    /// device, which is retried in seconds. The message is the same kind of
    /// user-facing text as the one above.
    DeviceOpenBlocked(String),
    /// Anything else: the software mixer, the player thread, or librespot
    /// itself failing before the device was ever reached.
    Fatal(String),
}

impl PlaybackError {
    pub fn message(&self) -> &str {
        match self {
            Self::NoOutputDevice(message) | Self::DeviceOpenBlocked(message) | Self::Fatal(message) => {
                message
            }
        }
    }
}

impl fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// The way out, appended to every message about a missing output device: the
/// engine retries the device by itself, so the user's only part is to give the
/// machine something to play to.
const NO_OUTPUT_DEVICE_REMEDY: &str = "Plug in or enable headphones, speakers, or a display's \
                                       audio output and playback resumes by itself";

/// What the user is told when there is no output device to open.
///
/// This message is the whole diagnosis they get, so it leads with the cause and
/// ends with the way out. The detail is appended only when it adds something:
/// "no output device is present" says no more than the sentence that already
/// named it, while a device that exists and will not open is worth the exact
/// reason. What this replaced was a panic on librespot's player thread and,
/// where that was caught, a WASAPI stack trace — neither of which says that
/// plugging the headphones back in is the fix.
fn no_output_device_message(error: &OutputError) -> String {
    let detail = match error {
        OutputError::NoDeviceAvailable => String::new(),
        error => format!(" ({error})"),
    };
    format!("no audio output device{detail}. {NO_OUTPUT_DEVICE_REMEDY}")
}

/// Turns the device step's answer into the failure the caller acts on.
///
/// `Ok(None)` is the only success — the sink asked for a device and got one.
/// Every other answer is named for what it costs to *retry*, not for what it
/// looked like: a refused open is an ordinary missing device, retried in
/// seconds, while an open that never returned is a wedged driver whose player
/// thread stays parked inside cpal for the life of the process, and is retried
/// on a much longer clock (see [`PlaybackError::DeviceOpenBlocked`]). The
/// builder never being reached is not about the device at all.
fn device_step_failure(
    answer: Result<Option<OutputError>, std_mpsc::RecvTimeoutError>,
) -> Option<PlaybackError> {
    match answer {
        Ok(None) => None,
        Ok(Some(error)) => Some(PlaybackError::NoOutputDevice(no_output_device_message(
            &error,
        ))),
        Err(std_mpsc::RecvTimeoutError::Timeout) => Some(PlaybackError::DeviceOpenBlocked(
            format!(
                "no audio output device: opening one did not finish within {} s. \
                 {NO_OUTPUT_DEVICE_REMEDY}",
                AUDIO_START_TIMEOUT.as_secs()
            ),
        )),
        // The builder was never reached, which means the player thread ended
        // before it could ask for a device: a librespot or runtime failure, and
        // a retry would meet the same one.
        Err(std_mpsc::RecvTimeoutError::Disconnected) => Some(PlaybackError::Fatal(
            "the audio player terminated before it opened an output device".to_owned(),
        )),
    }
}

/// A prepared OAuth authorization-code + PKCE attempt. The authorize URL is
/// generated up front (fresh CSRF state and PKCE challenge per attempt) so the
/// engine can publish it in its `needs_login` state before the flow runs; the
/// client and verifier are kept so the `login` command completes the exact
/// attempt whose URL the UI opened.
pub struct PendingAuth {
    client: BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>,
    verifier: PkceCodeVerifier,
    pub auth_url: String,
}

/// Builds a fresh OAuth attempt and its authorize URL. Pure local work: no
/// network, no browser, no listener. Each call regenerates the URL (new CSRF
/// state and PKCE challenge), satisfying "regenerated per attempt".
pub fn prepare_oauth() -> Result<PendingAuth, String> {
    let client = basic_client()?;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let request_scopes: Vec<Scope> = OAUTH_SCOPES
        .iter()
        .map(|scope| Scope::new((*scope).to_owned()))
        .collect();
    let (auth_url, _csrf) = client
        .authorize_url(CsrfToken::new_random)
        .add_scopes(request_scopes)
        .set_pkce_challenge(pkce_challenge)
        .url();
    Ok(PendingAuth {
        client,
        verifier: pkce_verifier,
        auth_url: auth_url.to_string(),
    })
}

/// Why a cached-credential connection did not produce a session.
///
/// The distinction decides whether the user is asked to log in again. Waking
/// from sleep reaches `session.connect` before Windows has a working resolver,
/// which fails with `no such host is known (os error 11001)` — a fact about the
/// network, not the credentials. Treating that as a rejection logged the user
/// out of an account that was never actually refused, and restarting the app
/// signed them straight back in with the same cached credentials.
#[derive(Debug)]
pub enum AuthFailure {
    /// Spotify refused these credentials. They will not start working again,
    /// so a fresh login is the only way forward.
    Rejected(String),
    /// Spotify could not be reached: no resolver, no route, a timeout, or an
    /// outage on their side. The credentials are untouched and worth retrying.
    Unreachable(String),
}

impl AuthFailure {
    pub fn message(&self) -> &str {
        match self {
            Self::Rejected(message) | Self::Unreachable(message) => message,
        }
    }
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// Only an explicit refusal from Spotify invalidates the credentials. Every
/// other kind — including `Unknown`, which is where librespot puts a wrapped
/// `io::Error` such as a DNS failure — is a transport problem, and answering it
/// with a login prompt would be wrong. Misclassifying in this direction costs a
/// retry; misclassifying the other way costs the user their session.
fn classify_connect_error(error: &librespot_core::Error) -> AuthFailure {
    use librespot_core::error::ErrorKind;
    match error.kind {
        ErrorKind::Unauthenticated | ErrorKind::PermissionDenied => {
            AuthFailure::Rejected(format!("cached Spotify credentials were rejected: {error}"))
        }
        _ => AuthFailure::Unreachable(format!("could not reach Spotify: {error}")),
    }
}

/// Connects with the cached credentials (if any) and creates playback. Fails
/// when no credentials are cached, they are rejected, or Spotify cannot be
/// reached; the caller decides which of those means `NeedsLogin` (the flow is
/// never started implicitly).
///
/// A machine with no output device is not one of those failures. It comes back
/// as a [`ConnectedSession`] whose playback is missing and whose session is
/// live, because the audio device is not what browsing uses.
pub async fn connect_cached(
    cache: Cache,
    temporary_directory: PathBuf,
    normalisation: Arc<AtomicBool>,
    audio: SinkOpener,
) -> Result<ConnectedSession, AuthFailure> {
    let credentials = cache.credentials().ok_or_else(|| {
        AuthFailure::Rejected("no cached Spotify credentials".to_owned())
    })?;
    let mut session_config = SessionConfig::default();
    session_config.tmp_dir = temporary_directory;
    let session = Session::new(session_config, Some(cache.clone()));
    match session.connect(credentials, false).await {
        Ok(()) => match create_playback_latest(session.clone(), cache, normalisation, audio).await {
            Ok(playback) => Ok(ConnectedSession {
                session,
                playback: Ok(playback),
            }),
            // The device is the machine's problem, not the account's: the
            // session is left running and the engine retries the device. A
            // device whose open hung is the same answer with a different retry
            // clock, which the engine derives from the probe's own typed
            // result.
            Err(error @ (PlaybackError::NoOutputDevice(_) | PlaybackError::DeviceOpenBlocked(_))) => {
                Ok(ConnectedSession {
                    session,
                    playback: Err(error.message().to_owned()),
                })
            }
            // Unchanged from when playback construction was one string: a
            // player that cannot be built at all is reported as a login
            // problem, session torn down, exactly as a refusal is.
            Err(PlaybackError::Fatal(message)) => {
                session.shutdown();
                Err(AuthFailure::Rejected(message))
            }
        },
        Err(error) => {
            let failure = classify_connect_error(&error);
            eprintln!("{failure}");
            session.shutdown();
            Err(failure)
        }
    }
}

/// The loopback socket Spotify's redirect lands on, already bound.
///
/// This exists as a value so the bind cannot be scheduled: the flow that waits
/// for the callback can only be started by handing it a listener that is
/// already listening, and the caller therefore knows the port is held the
/// moment [`bind_oauth_listener`] returns. The browser must not be opened
/// before that — the redirect goes to a fixed address, and a listener that
/// appears a moment after the user finishes authorising is a callback lost to
/// a closed port, which is exactly the bug this shape exists to make
/// impossible.
#[derive(Debug)]
pub struct OauthListener {
    listener: TcpListener,
}

impl OauthListener {
    #[cfg(test)]
    pub fn address(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound OAuth listener has an address")
    }
}

/// Binds the loopback callback port, synchronously, or explains why it could
/// not.
///
/// The port is not negotiable. `OAUTH_REDIRECT_URI` has to match a redirect
/// URI registered against the client id, and the client id is librespot's own
/// (`SessionConfig::default().client_id`), so this application cannot register
/// a second address and cannot fall back to a free port: Spotify would refuse
/// the authorize request, or — worse — accept it and redirect the browser
/// somewhere nothing is listening. A taken port is therefore a dead end, and
/// the only honest response is to say so here, before the user is sent away to
/// type a password for a flow that cannot finish.
pub fn bind_oauth_listener() -> Result<OauthListener, String> {
    let address = oauth_listener_addr()?;
    let listener = TcpListener::bind(address).map_err(|error| {
        format!(
            "another program is already using port {} on this machine, and Spotify can only send \
             the sign-in back to that exact port. Close whatever is holding it (or restart the \
             computer) and try again. Details: {error}",
            address.port()
        )
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure the OAuth callback listener: {error}"))?;
    Ok(OauthListener { listener })
}

/// Runs the OAuth flow for a prepared attempt on an already-bound listener:
/// waits for the loopback callback, exchanges the code, connects the session,
/// and creates playback. Taking [`OauthListener`] by value is what guarantees
/// the port was held before the browser was opened; see [`bind_oauth_listener`].
pub async fn complete_oauth(
    cache: Cache,
    temporary_directory: PathBuf,
    pending: PendingAuth,
    listener: OauthListener,
    normalisation: Arc<AtomicBool>,
    audio: SinkOpener,
) -> Result<ConnectedSession, String> {
    eprintln!("Browse to: {}", pending.auth_url);
    let access_token = tokio::task::spawn_blocking(move || run_oauth_flow(pending, listener))
        .await
        .map_err(|error| format!("Spotify OAuth worker failed: {error}"))??;

    let mut session_config = SessionConfig::default();
    session_config.tmp_dir = temporary_directory;
    let session = Session::new(session_config, Some(cache.clone()));
    if let Err(error) = session
        .connect(Credentials::with_access_token(access_token), true)
        .await
    {
        session.shutdown();
        return Err(format!("Spotify authentication failed: {error}"));
    }
    match create_playback_latest(session.clone(), cache, normalisation, audio).await {
        Ok(playback) => Ok(ConnectedSession {
            session,
            playback: Ok(playback),
        }),
        // Signing in on a machine with no output device used to fail the whole
        // flow, which is a strange thing to tell someone whose password was
        // accepted: the session is live, and the device is retried. A device
        // that hung instead of refusing is the same story with a longer retry
        // clock.
        Err(error @ (PlaybackError::NoOutputDevice(_) | PlaybackError::DeviceOpenBlocked(_))) => {
            Ok(ConnectedSession {
                session,
                playback: Err(error.message().to_owned()),
            })
        }
        Err(PlaybackError::Fatal(message)) => {
            session.shutdown();
            Err(message)
        }
    }
}

/// Blocking OAuth half of [`complete_oauth`]: loopback listener + token
/// exchange. Runs on the blocking pool so the async engine loop is never
/// stalled by the callback wait.
fn run_oauth_flow(pending: PendingAuth, listener: OauthListener) -> Result<String, String> {
    let code = wait_for_oauth_code(listener)?;
    let http_client = reqwest::blocking::Client::new();
    let response = pending
        .client
        .exchange_code(code)
        .set_pkce_verifier(pending.verifier)
        .request(&http_client)
        .map_err(|error| format!("Spotify OAuth token exchange failed: {error}"))?;
    Ok(response.access_token().secret().to_string())
}

/// One complete HTTP answer for the callback tab, page included.
///
/// A content type is not decoration here: without it the browser has to guess,
/// and every one of them guesses "plain text on white", which is why a
/// successful sign-in used to look like a broken page. `connection: close`
/// says out loud what the socket does anyway — the listener serves exactly one
/// callback and is dropped — so the tab stops waiting for more instead of
/// spinning until it times out.
fn oauth_response(status: &str, accent: &str, heading: &str, message: &str) -> String {
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Spotifine</title><style>{OAUTH_PAGE_STYLE}</style></head><body><main>\
         <div class=\"rule\" style=\"background:{accent}\"></div>\
         <h1>{heading}</h1><p>{message}</p></main></body></html>"
    );
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{page}",
        page.len()
    )
}

fn oauth_success_response() -> String {
    oauth_response(
        "200 OK",
        OAUTH_SUCCESS_ACCENT,
        "Signed in",
        "Spotifine has your Spotify session. You can close this tab and go back to the app.",
    )
}

fn oauth_failed_response() -> String {
    oauth_response(
        "400 Bad Request",
        OAUTH_FAILED_ACCENT,
        "Sign-in was not completed",
        "Spotify did not send an authorisation code back. Close this tab and click Log in again \
         in Spotifine.",
    )
}

/// Waits for the browser redirect to `OAUTH_REDIRECT_URI` and returns the
/// authorization code from its query string. The listener answers the first
/// callback and terminates, mirroring the librespot client; unlike it, a
/// stalled attempt fails after [`OAUTH_LISTENER_TIMEOUT`] instead of blocking
/// forever.
///
/// Every callback is answered, including the ones that carry no code — a user
/// who presses Cancel at Spotify is redirected here with `?error=access_denied`
/// and deserves a page rather than a tab that hangs until the browser gives up
/// on a connection nobody ever wrote to.
fn wait_for_oauth_code(listener: OauthListener) -> Result<AuthorizationCode, String> {
    wait_for_oauth_code_within(
        listener,
        OAUTH_LISTENER_TIMEOUT,
        OAUTH_CALLBACK_READ_TIMEOUT,
    )
}

/// [`wait_for_oauth_code`] with both of its clocks named, so the behaviour
/// that depends on them — a socket that says nothing, a connection that is
/// not the callback, the deadline itself — is reachable from a test without
/// waiting out the production durations.
///
/// A connection arriving is not the callback arriving. `accept` hands over
/// whatever connected to the port: a browser's speculative socket, a scanner,
/// on Windows an aborted pending connection that surfaces as an error rather
/// than as a stream. None of those is the redirect, and none of them may end
/// the sign-in — the wait therefore continues until a request line actually
/// names the redirect path, and the deadline is what ends it either way. The
/// read is bounded separately: without [`OAUTH_CALLBACK_READ_TIMEOUT`] one
/// socket that connects and never writes blocks this blocking task forever,
/// holding the port and leaving the engine in `Authenticating` past the
/// deadline below, which only ever bounded `accept`.
fn wait_for_oauth_code_within(
    listener: OauthListener,
    total: Duration,
    read_timeout: Duration,
) -> Result<AuthorizationCode, String> {
    let deadline = Instant::now() + total;
    loop {
        if Instant::now() >= deadline {
            return Err("Spotify login timed out; click Log in to start again".to_owned());
        }
        let mut stream = match listener.listener.accept() {
            Ok((stream, _)) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err("Spotify login timed out; click Log in to start again".to_owned());
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(error) => return Err(format!("Spotify OAuth callback failed: {error}")),
        };
        // Windows inherits the listener's nonblocking mode. Read timeouts do
        // not change it, so a redirect accepted before its bytes arrive would
        // otherwise be dropped immediately with WouldBlock.
        stream.set_nonblocking(false)
            .map_err(|error| format!("could not configure the Spotify OAuth callback socket: {error}"))?;

        stream
            .set_read_timeout(Some(read_timeout.min(deadline.saturating_duration_since(Instant::now())).max(Duration::from_millis(1))))
            .map_err(|error| {
                format!("could not configure the Spotify OAuth callback socket: {error}")
            })?;
        stream.set_write_timeout(Some(read_timeout))
            .map_err(|error| format!("could not configure the Spotify OAuth callback socket: {error}"))?;
        let mut request_line = String::new();
        match BufReader::new(&stream).read_line(&mut request_line) {
            // Connected and said nothing, went quiet mid-line, or sent bytes
            // that are not a request line at all. The socket is closed by
            // dropping it and the wait goes on: none of those is the redirect
            // arriving, and the deadline — not any one connection — is what
            // ends the wait.
            Ok(0) => continue,
            Err(_) => continue,
            Ok(_) => {}
        }

        let Some(path) = request_line.split_whitespace().nth(1) else {
            continue;
        };
        if path.split(['?', '#']).next() != Some(oauth_callback_path()) {
            // Not the redirect: the port answers to a fixed address, so
            // anything on the machine can knock, and a browser asks for
            // things on its own (a favicon, a preconnect). Answering one of
            // those would end an attempt that has not happened yet.
            continue;
        }
        let outcome = extract_oauth_code(path);

        let response = if outcome.is_ok() {
            oauth_success_response()
        } else {
            oauth_failed_response()
        };
        // A browser that hung up before reading the page cannot take the login
        // back: the code in hand is still redeemable, and discarding it over a
        // dead socket would fail an attempt that actually succeeded. The write
        // is reported only when there is no code to lose.
        if let Err(error) = stream
            .write_all(response.as_bytes())
            .and_then(|()| stream.flush())
        {
            if outcome.is_ok() {
                eprintln!("could not answer the Spotify OAuth callback tab: {error}");
            } else {
                return Err(format!(
                    "could not answer the Spotify OAuth callback: {error}"
                ));
            }
        }
        return outcome;
    }
}

/// Reads the authorization code out of the redirect path, or says what came
/// instead. A cancelled sign-in arrives as `?error=access_denied`; naming it
/// keeps the engine's `NeedsLogin` message honest about whose decision ended
/// the attempt.
fn extract_oauth_code(request_path: &str) -> Result<AuthorizationCode, String> {
    let redirect = format!("http://127.0.0.1{request_path}");
    let url = Url::parse(&redirect)
        .map_err(|error| format!("malformed Spotify OAuth callback: {error}"))?;
    if let Some(code) = url.query_pairs().find(|(key, _)| key == "code") {
        return Ok(AuthorizationCode::new(code.1.into_owned()));
    }
    match url.query_pairs().find(|(key, _)| key == "error") {
        Some((_, reason)) if reason == "access_denied" => {
            Err("Spotify sign-in was cancelled; click Log in to start again".to_owned())
        }
        Some((_, reason)) => Err(format!("Spotify refused the sign-in: {reason}")),
        None => Err("the Spotify OAuth callback carried no authorization code".to_owned()),
    }
}

/// Serialises every test that binds the one fixed callback port.
///
/// The crate's unit tests run as threads in a single process, so the tests
/// here and the engine's login-ordering tests would otherwise race for
/// 127.0.0.1:5588 and fail each other at random. The guard carries no state —
/// only the exclusion — so a panic that poisons it has broken nothing worth
/// propagating.
#[cfg(test)]
pub(crate) fn lock_oauth_port() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn oauth_listener_addr() -> Result<SocketAddr, String> {
    let url = Url::parse(OAUTH_REDIRECT_URI)
        .map_err(|error| format!("invalid OAuth redirect URI: {error}"))?;
    url.socket_addrs(|| None)
        .ok()
        .and_then(|mut addresses| addresses.pop())
        .ok_or_else(|| format!("OAuth redirect URI has no listenable socket: {OAUTH_REDIRECT_URI}"))
}

/// The path part of `OAUTH_REDIRECT_URI`: what a request line has to name to
/// be the redirect rather than something else that reached the port.
///
/// Derived from the redirect URI rather than written out again, because the
/// two have to agree and only one of them is registered with Spotify. Parsed
/// once; the fallback keeps this total without a panic path on a literal this
/// crate owns and the test below pins.
fn oauth_callback_path() -> &'static str {
    static PATH: LazyLock<String> = LazyLock::new(|| {
        Url::parse(OAUTH_REDIRECT_URI)
            .map(|url| url.path().to_owned())
            .unwrap_or_else(|_| "/login".to_owned())
    });
    PATH.as_str()
}

fn basic_client() -> Result<
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>,
    String,
> {
    let client_id = SessionConfig::default().client_id;
    Ok(BasicClient::new(ClientId::new(client_id))
        .set_auth_uri(
            AuthUrl::new(OAUTH_AUTHORIZE_URL.to_owned())
                .map_err(|_| "invalid Spotify OAuth authorize URL".to_owned())?,
        )
        .set_token_uri(
            TokenUrl::new(OAUTH_TOKEN_URL.to_owned())
                .map_err(|_| "invalid Spotify OAuth token URL".to_owned())?,
        )
        .set_redirect_uri(
            RedirectUrl::new(OAUTH_REDIRECT_URI.to_owned())
                .map_err(|_| "invalid Spotify OAuth redirect URI".to_owned())?,
        ))
}

/// Player configuration for the standalone engine.
///
/// `position_update_interval` is intentionally left at its `None` default: the
/// UI projects position locally between engine events, and the engine emits a
/// single position heartbeat every two seconds while playing. librespot must
/// not stream 250 ms `PositionChanged` events.
///
/// Normalisation, when enabled, is deliberately the *basic* kind: one constant
/// per-track gain from Spotify's embedded ReplayGain-style tags, clamped so it
/// can never push a sample past full scale. That makes it pure attenuation —
/// mathematically transparent in this float pipeline — and it never constructs
/// librespot's dynamic limiter, which would shave transients on boosted quiet
/// material. `Track` type keeps every track at the same perceived level.
fn player_config(normalisation: bool) -> PlayerConfig {
    PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        gapless: true,
        normalisation,
        normalisation_type: NormalisationType::Track,
        normalisation_method: NormalisationMethod::Basic,
        normalisation_pregain_db: 0.0,
        // librespot defaults this to a triangular ditherer, which only ever
        // runs in its fixed-point conversions (`f64_to_s16` and friends). The
        // sink takes the float path, where `Converter::f64_to_f32` is a plain
        // cast and the ditherer is never consulted — but constructing one still
        // logs "Converting with ditherer: tpdf", which reads like the audio is
        // being dithered when nothing of the sort is happening.
        ditherer: None,
        ..PlayerConfig::default()
    }
}

fn mixer_config() -> MixerConfig {
    MixerConfig {
        volume_ctrl: VolumeCtrl::Cubic(60.0),
        ..MixerConfig::default()
    }
}

async fn create_playback_latest(
    session: Session,
    cache: Cache,
    normalisation: Arc<AtomicBool>,
    audio: SinkOpener,
) -> Result<PlaybackHandles, PlaybackError> {
    loop {
        let enabled = normalisation.load(Ordering::Acquire);
        // The session is deliberately not shut down on a failure here. Whether
        // a failure ends the session is the caller's decision, and the one
        // failure this actually happens with — a machine with no output device
        // — is not about the session at all. Shutting it down here is what once
        // took browsing out together with the audio device.
        let handles =
            create_playback(session.clone(), cache.clone(), enabled, audio.clone()).await?;
        if normalisation.load(Ordering::Acquire) == enabled {
            return Ok(handles);
        }
        // The preference changed while the player was opening. Keep the
        // authenticated session and retry only player construction.
        handles.player.stop();
    }
}

pub async fn create_playback(
    session: Session,
    cache: Cache,
    normalisation: bool,
    audio: SinkOpener,
) -> Result<PlaybackHandles, PlaybackError> {
    let mixer = Arc::new(SoftMixer::open(mixer_config()).map_err(|error| {
        PlaybackError::Fatal(format!("could not initialize software volume: {error}"))
    })?);
    let cached_volume = stored_volume(&cache);
    mixer.set_volume(cached_volume);

    // Construction runs in librespot's sink builder, which cannot return an
    // error. Its typed result travels over this channel. Production only checks
    // device presence and builds an unstarted sink here; actual native opening
    // is lazy and independently deadline-bounded in audio::OutputSink::start.
    let (device_tx, device_rx) = std_mpsc::sync_channel(1);
    let config = player_config(normalisation);
    // Volume is applied by the output callback (see audio::set_sink_volume), not
    // per decoded packet, so a transport volume change is audible on the
    // next output callback instead of after the write-ahead buffer plays
    // out. The player's volume getter is therefore a no-op (always 1.0);
    // the SoftMixer is kept purely as the volume store/persistence.
    let player = Player::new(config, session.clone(), Box::new(NoOpVolume), move || {
        // Custom immediate-stop output sink: pause/stop must not drain the
        // buffered audio before silencing the output.
        // F32, not S16: WASAPI's shared-mode engine is float internally, and
        // the sink's own path is float end to end, so asking cpal for 16-bit
        // would insert an undithered quantisation that nothing downstream
        // wants. The device's mix format is what actually gets opened.
        match audio(AudioFormat::F32) {
            Ok(sink) => {
                crate::audio::set_sink_volume(cached_volume);
                let _ = device_tx.send(None);
                sink
            }
            Err(error) => {
                let _ = device_tx.send(Some(error));
                Box::new(SilentSink)
            }
        }
    });

    let device = tokio::task::spawn_blocking(move || device_rx.recv_timeout(AUDIO_START_TIMEOUT))
        .await
        .map_err(|error| {
            PlaybackError::Fatal(format!("audio initialization worker failed: {error}"))
        })?;
    if let Some(failure) = device_step_failure(device) {
        // A stuck injected/construction opener must not turn its timeout into
        // a synchronous Player::drop join on the authentication task.
        player.stop();
        std::thread::spawn(move || drop(player));
        return Err(failure);
    }
    if player.is_invalid() {
        return Err(PlaybackError::Fatal(
            "the audio player terminated during initialization".to_owned(),
        ));
    }

    let events = player.get_player_event_channel();
    Ok(PlaybackHandles {
        player,
        events,
        mixer,
        volume_percent: volume_to_percent(cached_volume),
    })
}

pub fn percent_to_volume(percent: u8) -> u16 {
    ((u32::from(percent) * u32::from(u16::MAX) + 50) / 100) as u16
}

fn volume_to_percent(volume: u16) -> u8 {
    ((u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u8
}

/// The transport volume stored in the cache, in librespot's u16 scale: what the
/// mixer, the sink and the reported state all start from. Half of full scale
/// until something has been stored, which is where a fresh install starts.
fn stored_volume(cache: &Cache) -> u16 {
    cache.volume().unwrap_or(u16::MAX / 2)
}

/// [`stored_volume`] as the percentage the engine's state and the UI both
/// speak. The engine reports this from the moment it starts, so a machine that
/// could not open an output device at startup still shows the volume its
/// recovered playback will use — otherwise the first state event would claim
/// half volume and the rebuilt player would be set to it.
pub fn stored_volume_percent(cache: &Cache) -> u8 {
    volume_to_percent(stored_volume(cache))
}

#[cfg(test)]
mod tests {
    use super::{
        AUDIO_START_TIMEOUT, OAUTH_LISTENER_TIMEOUT, OAUTH_REDIRECT_URI, PlaybackError,
        bind_oauth_listener, device_step_failure, extract_oauth_code, lock_oauth_port,
        mixer_config, oauth_callback_path, oauth_failed_response, oauth_listener_addr,
        oauth_success_response, player_config, prepare_oauth, std_mpsc, wait_for_oauth_code_within,
    };
    use crate::audio::OutputError;
    use librespot_playback::config::VolumeCtrl;
    use librespot_playback::mixer::Mixer;
    use librespot_playback::mixer::softmixer::SoftMixer;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    #[test]
    fn mixer_uses_the_same_cubic_sixty_db_curve_as_the_sink() {
        let config = mixer_config();
        assert!(
            matches!(config.volume_ctrl, VolumeCtrl::Cubic(db_range) if db_range == 60.0),
            "unexpected volume control: {:?}",
            config.volume_ctrl
        );

        let mixer = SoftMixer::open(config).expect("soft mixer opens with cubic control");
        for volume in [0u16, 1, 32768, 49151, u16::MAX] {
            mixer.set_volume(volume);
            let audible = mixer.get_soft_volume().attenuation_factor();
            let normalized = f64::from(volume) / f64::from(u16::MAX);
            let expected = if volume == 0 {
                0.0
            } else {
                (0.1 + 0.9 * normalized).powi(3)
            };
            assert!(
                (audible - expected).abs() <= 1e-12,
                "mixer mapping mismatch at raw volume {volume}: {audible} != {expected}"
            );
        }
    }
    #[test]
    fn player_config_does_not_poll_position() {
        // Event cadence: the engine emits state on transitions plus a 2 s
        // position heartbeat while playing. librespot must not be configured
        // to stream a 250 ms PositionChanged event for the UI to poll.
        assert!(player_config(false).position_update_interval.is_none());
    }

    #[test]
    fn normalisation_is_attenuation_only_when_enabled_and_absent_when_disabled() {
        let enabled = player_config(true);
        assert!(enabled.normalisation, "the flag must reach the player");
        assert_eq!(
            enabled.normalisation_method,
            librespot_playback::config::NormalisationMethod::Basic,
            "basic mode is constant gain only; the dynamic limiter must never run"
        );
        assert_eq!(
            enabled.normalisation_type,
            librespot_playback::config::NormalisationType::Track,
            "track gain keeps every track at the same perceived level"
        );
        assert_eq!(
            enabled.normalisation_pregain_db, 0.0,
            "no pregain: quiet tracks stay at their natural level"
        );

        let disabled = player_config(false);
        assert!(!disabled.normalisation, "off stays fully off");
        assert!(
            disabled.gapless,
            "enabling normalisation must not disturb the other player settings"
        );
    }

    #[test]
    fn prepared_oauth_attempt_carries_a_spotify_authorize_url() {
        let attempt = prepare_oauth().expect("oauth attempt prepares without network");
        assert!(
            attempt
                .auth_url
                .starts_with("https://accounts.spotify.com/authorize?"),
            "unexpected authorize URL: {}",
            attempt.auth_url
        );
        assert!(attempt.auth_url.contains("code_challenge="));
        assert!(attempt.auth_url.contains("state="));
        assert!(attempt.auth_url.contains("redirect_uri="));
        assert!(attempt.auth_url.contains("&scope=streaming"));
    }

    #[test]
    fn each_prepared_attempt_regenerates_the_authorize_url() {
        // A fresh CSRF state and PKCE challenge per attempt: re-login must
        // never reuse a URL that a previous attempt (or its browser tab) saw.
        let first = prepare_oauth().expect("first attempt");
        let second = prepare_oauth().expect("second attempt");
        assert_ne!(first.auth_url, second.auth_url);
    }

    #[test]
    fn oauth_listener_addr_is_the_loopback_callback_port() {
        let address = oauth_listener_addr().expect("redirect URI has a socket");
        assert_eq!(address.to_string(), "127.0.0.1:5588");
        assert!(OAUTH_REDIRECT_URI.contains("127.0.0.1:5588"));
        // The flow recognises the redirect by this path, and the redirect is
        // the address registered against the client id: a mismatch would make
        // the sign-in arrive as a connection that is not the callback.
        assert_eq!(oauth_callback_path(), "/login");
    }

    #[test]
    fn oauth_callback_code_extracts_from_the_redirect_path() {
        let code = extract_oauth_code("/login?code=abc123&state=xyz").expect("code present");
        assert_eq!(code.secret(), "abc123");

        // Cancelling at Spotify is a redirect, not a silence: it must be named
        // as the user's own decision rather than as a missing code.
        let cancelled = extract_oauth_code("/login?error=access_denied&state=xyz")
            .expect_err("a denied callback carries no code");
        assert!(
            cancelled.contains("cancelled"),
            "unexpected cancel message: {cancelled}"
        );
        let refused =
            extract_oauth_code("/login?error=server_error").expect_err("an error is not a code");
        assert!(
            refused.contains("server_error"),
            "unexpected refusal message: {refused}"
        );

        assert!(extract_oauth_code("/login").is_err());
        assert!(
            extract_oauth_code("not a path").is_err(),
            "a malformed request line must not parse as a callback"
        );
    }

    /// The port is fixed by the registered redirect URI, so a taken one is a
    /// dead end rather than something to route around. What matters is that
    /// the failure names the port and says what to do, while the user is still
    /// looking at the Log in button.
    #[test]
    fn a_taken_callback_port_fails_with_the_port_and_a_remedy() {
        let _serialized = lock_oauth_port();
        let address = oauth_listener_addr().expect("redirect URI has a socket");
        let squatter = TcpListener::bind(address).expect("the test may hold the port first");

        let error = bind_oauth_listener().expect_err("the port is already held");
        assert!(error.contains("5588"), "the port must be named: {error}");
        assert!(
            error.contains("Close whatever is holding it"),
            "the message must say what to do: {error}"
        );

        drop(squatter);
        bind_oauth_listener().expect("the port is free once the squatter lets go");
    }

    /// The ordering guarantee, at its source: `bind_oauth_listener` returns
    /// only after the socket is accepting connections, so a caller that opens
    /// the browser afterwards cannot lose the redirect to a closed port.
    #[test]
    fn the_callback_port_is_accepting_before_the_bind_returns() {
        let _serialized = lock_oauth_port();
        let listener = bind_oauth_listener().expect("the callback port is free");
        assert_eq!(listener.address().to_string(), "127.0.0.1:5588");
        TcpStream::connect("127.0.0.1:5588")
            .expect("the browser's redirect would connect at this instant");
    }

    /// A connection is not a callback. The port answers to a fixed address, so
    /// whatever is on the machine can knock, and `accept` hands it over all the
    /// same: a socket that says nothing, something asking for a path that is
    /// not the redirect, a connection that vanishes before it can be read.
    /// Each of those used to end the sign-in — the first two as a fatal read
    /// error, the third by consuming the one callback the flow reads — and a
    /// socket that never wrote could hold the flow (and the port) open behind
    /// a deadline that only ever bounded `accept`. The wait has to keep its
    /// patience for exactly one thing: the request line naming the redirect.
    #[test]
    fn a_connection_that_is_not_the_callback_does_not_end_the_sign_in() {
        let _serialized = lock_oauth_port();
        let listener = bind_oauth_listener().expect("the callback port is free");
        let flow = std::thread::spawn(move || {
            // Short clocks, same behaviour: the redirect still has to be found
            // behind two connections that are not it.
            wait_for_oauth_code_within(
                listener,
                Duration::from_secs(10),
                Duration::from_millis(150),
            )
        });

        // First a socket that connects and says nothing at all.
        let silent = TcpStream::connect("127.0.0.1:5588").expect("connect");
        // Then one asking for something the callback port never serves.
        let mut stray = TcpStream::connect("127.0.0.1:5588").expect("connect");
        let _ = stray.write_all(b"GET /favicon.ico HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n");

        let mut callback = TcpStream::connect("127.0.0.1:5588").expect("connect");
        callback
            .write_all(b"GET /login?code=abc123&state=xyz HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n")
            .expect("write the redirect");
        let mut response = String::new();
        callback
            .read_to_string(&mut response)
            .expect("the callback tab is answered");

        let code = flow
            .join()
            .expect("the flow thread finishes")
            .expect("the redirect still carries its code");
        assert_eq!(code.secret(), "abc123");
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "the tab must be answered with the success page: {response}"
        );
        drop(silent);
    }

    #[test]
    fn the_callback_tab_gets_a_typed_html_page_either_way() {
        for (response, status, accent, needle) in [
            (oauth_success_response(), "200 OK", "#9ccfd8", "Signed in"),
            (
                oauth_failed_response(),
                "400 Bad Request",
                "#eb6f92",
                "Sign-in was not completed",
            ),
        ] {
            let (head, body) = response
                .split_once("\r\n\r\n")
                .expect("headers are separated from the page");
            assert!(
                head.starts_with(&format!("HTTP/1.1 {status}\r\n")),
                "unexpected status line: {head}"
            );
            assert!(
                head.contains("content-type: text/html; charset=utf-8\r\n"),
                "without a content type the browser renders the page as plain text: {head}"
            );
            assert!(
                head.contains(&format!("content-length: {}\r\n", body.len())),
                "the length must match the page exactly, or the tab hangs: {head}"
            );
            assert!(body.starts_with("<!doctype html>"), "not a page: {body}");
            assert!(body.contains(accent) && body.contains(needle), "{body}");
        }
    }

    #[test]
    fn the_callback_wait_is_forgiving_enough_for_a_first_login() {
        // A first login on a new machine runs through a password manager, a
        // 2FA prompt on another device, and possibly signing up. Five minutes
        // expired under people who were still typing.
        assert!(OAUTH_LISTENER_TIMEOUT >= Duration::from_secs(15 * 60));
    }

    /// The device step's answer decides how hard the engine retries it, so it
    /// has to be classified for what the retry costs rather than for what it
    /// looked like. A refused open is a missing device, seconds away from
    /// working; an open that never returned is a wedged driver whose player
    /// thread is parked inside cpal for the life of the process. Treated as a
    /// missing device, that second answer would leave a thread behind every ten
    /// seconds.
    #[test]
    fn a_device_open_that_never_returned_is_its_own_failure() {
        assert!(
            device_step_failure(Ok(None)).is_none(),
            "a device that opened is not a failure"
        );

        let refused = device_step_failure(Ok(Some(OutputError::NoDeviceAvailable)))
            .expect("a refused open is a failure");
        assert!(
            matches!(refused, PlaybackError::NoOutputDevice(_)),
            "a device that is simply gone retries in seconds: {refused:?}"
        );
        assert!(refused.message().contains("no audio output device"));
        assert!(
            refused.message().contains("Plug in or enable"),
            "and still tells the user the way out: {}",
            refused.message()
        );

        let blocked = device_step_failure(Err(std_mpsc::RecvTimeoutError::Timeout))
            .expect("a hung open is a failure");
        assert!(
            matches!(blocked, PlaybackError::DeviceOpenBlocked(_)),
            "the retry clock is chosen by this variant: {blocked:?}"
        );
        assert!(
            blocked
                .message()
                .contains(&AUDIO_START_TIMEOUT.as_secs().to_string()),
            "and the message still says how long the open was given: {}",
            blocked.message()
        );

        let disconnected =
            device_step_failure(Err(std_mpsc::RecvTimeoutError::Disconnected))
                .expect("a player thread that never asked for a device is a failure");
        assert!(
            matches!(disconnected, PlaybackError::Fatal(_)),
            "a thread that ended before the device step is not the device's: {disconnected:?}"
        );
    }
}
