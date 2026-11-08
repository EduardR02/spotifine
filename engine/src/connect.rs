//! On-demand Spotify Connect ZeroConf controller, not a receiver/advertiser.
//!
//! Wire formats follow Spotify's commercial-hardware ZeroConf guide and
//! spotifywebapipython/zeroconfapi/{zeroconfconnect,blobbuilder}.py. The default
//! blob is the inverse of librespot-core 0.8 Credentials::with_blob; hardware
//! advertising `accesstoken` instead expects a streaming token in the form body.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes::cipher::{BlockEncrypt, KeyInit, KeyIvInit, StreamCipher};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::{StreamExt, stream};
use hmac::{Hmac, Mac};
use librespot_core::{Session, authentication::Credentials, diffie_hellman::DhLocalKeys};
use librespot_protocol::authentication::AuthenticationType;
use mdns_sd::{DaemonEvent, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent};
use protobuf::Enum;
use rand::RngCore;
use renderer_engine::protocol::ConnectDevice;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;

const SERVICE_TYPE: &str = "_spotify-connect._tcp.local.";
const BROWSE_TIME: Duration = Duration::from_secs(3);
const PROBE_TIME: Duration = Duration::from_secs(5);
const ACTIVATION_TIME: Duration = Duration::from_secs(20);
const ENDPOINT_LIFETIME: Duration = Duration::from_secs(300);
const MAX_ENDPOINTS: usize = 32;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const RECEIVER_HOST: &str = "spotify-connect.invalid";
const CLIENT_VERSION: &str = "2.10.0";

type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

#[derive(Clone, Default)]
pub struct ConnectService {
    registry: Arc<Mutex<Registry>>,
}

#[derive(Default)]
struct Registry {
    owner: Option<Session>,
    discovered_at: Option<Instant>,
    endpoints: HashMap<String, Endpoint>,
}

impl Registry {
    fn endpoint(&self, session: &Session, id: &str) -> Result<Endpoint, String> {
        require_session(session)?;
        let owner = self.owner.as_ref().ok_or("Discover local Spotify devices first")?;
        if owner.is_invalid()
            || owner.device_id() != session.device_id()
            || owner.username() != session.username()
        {
            return Err("Spotify account changed; discover local devices again".into());
        }
        if self.discovered_at.is_none_or(|at| at.elapsed() > ENDPOINT_LIFETIME) {
            return Err("Local device discovery expired; refresh the device list".into());
        }
        self.endpoints.get(id).cloned()
            .ok_or_else(|| "The selected device was not discovered on this network".into())
    }
}

impl ConnectService {
    pub async fn discover(&self, session: &Session) -> Result<Vec<ConnectDevice>, String> {
        require_session(session)?;
        let mut registry = self.registry.try_lock()
            .map_err(|_| "Local Spotify device discovery or activation is already running")?;
        *registry = Registry::default();
        // mDNS owns its own thread. Both its wait and shutdown acknowledgement
        // belong on the blocking pool, never on either engine runtime worker.
        let endpoints = tokio::task::spawn_blocking(browse_endpoints).await
            .map_err(|_| "Local Spotify discovery worker failed")??;
        require_session(session)?;
        let mut probes = stream::iter(endpoints.into_iter().map(|endpoint| async move {
            let client = endpoint.client()?;
            let info = get_info(&client, &endpoint).await?;
            Ok::<_, String>((endpoint, info))
        })).buffer_unordered(8);
        let deadline = tokio::time::Instant::now() + PROBE_TIME;
        let mut devices = Vec::new();
        let mut last_error = None;
        loop {
            match tokio::time::timeout_at(deadline, probes.next()).await {
                Ok(Some(Ok((endpoint, info)))) => {
                    if !registry.endpoints.contains_key(&info.device_id) {
                        devices.push(ConnectDevice {
                            id: info.device_id.clone(), name: info.remote_name, kind: info.device_type,
                        });
                        registry.endpoints.insert(info.device_id, endpoint);
                    }
                }
                Ok(Some(Err(error))) => last_error = Some(error),
                Ok(None) => break,
                Err(_) => {
                    last_error = Some("Local Spotify receiver inspection timed out".into());
                    break;
                }
            }
        }
        if let Err(error) = require_session(session) {
            *registry = Registry::default();
            return Err(error);
        }
        // One broken advertisement (including the desktop launcher) must not
        // erase healthy receivers. If none can be inspected, explain why.
        if devices.is_empty() {
            if let Some(error) = last_error { return Err(error); }
        }
        registry.owner = Some(session.clone());
        registry.discovered_at = Some(Instant::now());
        devices.sort_unstable_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(devices)
    }

    pub async fn activate(&self, session: &Session, id: &str) -> Result<(), String> {
        let registry = self.registry.try_lock()
            .map_err(|_| "Local Spotify device discovery or activation is already running")?;
        let endpoint = registry.endpoint(session, id)?;
        tokio::time::timeout(ACTIVATION_TIME, activate_endpoint(session, id, &endpoint)).await
            .map_err(|_| "Local Spotify receiver activation timed out".to_owned())?
    }
}

fn require_session(session: &Session) -> Result<(), String> {
    if session.is_invalid() || session.username().is_empty() {
        Err("An authenticated Spotify session is required for local devices".into())
    } else {
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Endpoint {
    address: SocketAddr,
    path: String,
}

impl Endpoint {
    fn new(address: SocketAddr, path: &str) -> Result<Self, String> {
        let local = match address.ip() {
            IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
            IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
        };
        if !local || address.port() == 0 {
            return Err("Spotify discovery advertised a non-local receiver address".into());
        }
        if !path.starts_with('/') || path.starts_with("//") || path.len() > 1024
            || path.chars().any(|c| c.is_control() || matches!(c, '\\' | '?' | '#'))
        {
            return Err("Spotify discovery advertised an invalid receiver path".into());
        }
        Ok(Self { address, path: path.into() })
    }

    fn url(&self) -> url::Url {
        // A fixed, non-resolving host lets reqwest pin even scoped IPv6 sockets.
        // No hostname or URL supplied by a peer ever reaches the DNS resolver.
        let mut url = url::Url::parse("http://spotify-connect.invalid/").unwrap();
        url.set_port(Some(self.address.port())).unwrap();
        url.set_path(&self.path);
        url
    }

    fn client(&self) -> Result<reqwest::Client, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::HOST, self.address.to_string().parse()
            .map_err(|_| "Invalid local Spotify receiver address")?);
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .default_headers(headers)
            .resolve(RECEIVER_HOST, self.address)
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|_| "Could not initialize local Spotify receiver communication".into())
    }
}

struct BrowseDaemon(Option<ServiceDaemon>);

impl Drop for BrowseDaemon {
    fn drop(&mut self) {
        if let Some(daemon) = self.0.take() { let _ = daemon.shutdown(); }
    }
}

fn browse_endpoints() -> Result<Vec<Endpoint>, String> {
    let daemon = ServiceDaemon::new().map_err(|_| "Could not open local Spotify mDNS discovery")?;
    let mut guard = BrowseDaemon(Some(daemon));
    let daemon = guard.0.as_ref().unwrap();
    let monitor = daemon.monitor().map_err(|_| "Could not monitor local Spotify discovery")?;
    let receiver = daemon.browse(SERVICE_TYPE).map_err(|_| "Could not browse local Spotify receivers")?;
    let deadline = Instant::now() + BROWSE_TIME;
    let mut services = HashMap::new();
    let mut discovery_error = None;
    while Instant::now() < deadline {
        while let Ok(event) = monitor.try_recv() {
            if matches!(event, DaemonEvent::Error(_)) {
                discovery_error = Some("Local Spotify mDNS discovery reported a network error".to_owned());
            }
        }
        if let Ok(event) = receiver.recv_timeout(Duration::from_millis(100)) {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    if services.len() < MAX_ENDPOINTS || services.contains_key(info.get_fullname()) {
                        match service_endpoints(&info) {
                            Ok(endpoints) => { services.insert(info.get_fullname().to_owned(), endpoints); }
                            Err(error) => discovery_error = Some(error),
                        }
                    }
                }
                ServiceEvent::ServiceRemoved(_, name) => { services.remove(&name); }
                _ => {}
            }
        } else if receiver.is_disconnected() {
            discovery_error = Some("Local Spotify mDNS discovery stopped unexpectedly".into());
            break;
        }
    }
    let _ = daemon.stop_browse(SERVICE_TYPE);
    let stopped = daemon.shutdown().map_err(|_| "Could not stop local Spotify discovery")?;
    stopped.recv_timeout(Duration::from_secs(1))
        .map_err(|_| "Local Spotify discovery did not release its listener in time")?;
    guard.0 = None;
    let mut seen = HashSet::new();
    let endpoints: Vec<_> = services.into_values().flatten()
        .filter(|endpoint| seen.insert(endpoint.clone())).take(MAX_ENDPOINTS).collect();
    if endpoints.is_empty() {
        if let Some(error) = discovery_error { return Err(error); }
    }
    Ok(endpoints)
}

fn service_endpoints(info: &ResolvedService) -> Result<Vec<Endpoint>, String> {
    let path = info.get_property_val_str("CPath")
        .ok_or("Spotify discovery did not advertise a receiver path")?;
    let mut addresses = Vec::new();
    for ip in info.get_addresses() {
        let address = match ip {
            ScopedIp::V4(ip) => SocketAddr::new((*ip.addr()).into(), info.get_port()),
            ScopedIp::V6(ip) => SocketAddr::V6(SocketAddrV6::new(
                *ip.addr(), info.get_port(), 0, ip.scope_id().index,
            )),
            _ => continue,
        };
        if let Ok(endpoint) = Endpoint::new(address, path) { addresses.push(endpoint); }
    }
    // Prefer IPv4 on dual-stack receivers, without excluding scoped IPv6.
    addresses.sort_unstable_by_key(|endpoint| (endpoint.address.is_ipv6(), endpoint.address));
    addresses.truncate(4);
    if addresses.is_empty() {
        Err("Spotify discovery advertised no usable local receiver address or path".into())
    } else {
        Ok(addresses)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    status: i32,
    spotify_error: i32,
}

impl Status {
    fn check(&self) -> Result<(), String> {
        if self.status == 101 && self.spotify_error == 0 {
            Ok(())
        } else {
            // Do not echo statusString or the response body: a peer may echo
            // submitted credentials there. Numeric protocol errors suffice.
            Err(format!("Spotify receiver rejected the request (status {}, Spotify error {})",
                self.status, self.spotify_error))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    #[serde(flatten)]
    status: Status,
    #[serde(rename = "deviceID")]
    device_id: String,
    remote_name: String,
    #[serde(default = "unknown_device_type")]
    device_type: String,
    #[serde(default)]
    public_key: String,
    #[serde(default)]
    token_type: String,
    #[serde(default, rename = "clientID")]
    client_id: String,
    #[serde(default)]
    availability: String,
    #[serde(default)]
    active_user: Option<String>,
    #[serde(default)]
    version: String,
}

fn unknown_device_type() -> String { "Unknown".into() }

impl Info {
    fn validate(&self, selected_id: Option<&str>) -> Result<(), String> {
        self.status.check()?;
        if self.device_id.is_empty() || self.device_id.len() > 128
            || self.device_id.chars().any(|c| c.is_control() || c.is_whitespace())
            || self.remote_name.is_empty() || self.remote_name.len() > 1024
            || self.remote_name.chars().any(char::is_control)
            || self.device_type.len() > 64 || self.device_type.chars().any(char::is_control)
            || self.version.len() > 32 || self.version.chars().any(char::is_control)
        {
            return Err("Spotify receiver returned invalid device information".into());
        }
        if selected_id.is_some_and(|id| id != self.device_id) {
            return Err("The local receiver identity changed; refresh the device list".into());
        }
        Ok(())
    }

    fn version(&self) -> &str {
        if self.version.is_empty() { CLIENT_VERSION } else { &self.version }
    }
}

async fn response_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, String> {
    let mut response = request.send().await.map_err(|error| {
        if error.is_timeout() { "Local Spotify receiver request timed out" }
        else { "Could not communicate with the local Spotify receiver" }.to_owned()
    })?;
    if !response.status().is_success() {
        return Err(format!("Local Spotify receiver returned HTTP {}", response.status().as_u16()));
    }
    if response.content_length().is_some_and(|length| length > MAX_RESPONSE_BYTES as u64) {
        return Err("Local Spotify receiver response exceeded the size limit".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "Could not read the local Spotify receiver response")? {
        if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
            return Err("Local Spotify receiver response exceeded the size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "Local Spotify receiver returned invalid protocol data".into())
}

async fn get_info(client: &reqwest::Client, endpoint: &Endpoint) -> Result<Info, String> {
    let info: Info = response_json(client.get(endpoint.url())
        .query(&[("action", "getInfo"), ("version", CLIENT_VERSION)])).await?;
    info.validate(None)?;
    Ok(info)
}

async fn add_user(
    client: &reqwest::Client, endpoint: &Endpoint, info: &Info,
    username: &str, blob: &str, client_key: &str, token_type: &str,
) -> Result<Status, String> {
    response_json(client.post(endpoint.url()).form(&[
        ("action", "addUser"), ("version", info.version()), ("userName", username),
        ("loginId", username), ("blob", blob), ("clientKey", client_key), ("tokenType", token_type),
    ])).await
}

async fn activate_endpoint(session: &Session, id: &str, endpoint: &Endpoint) -> Result<(), String> {
    let client = endpoint.client()?;
    let mut info = get_info(&client, endpoint).await?;
    info.validate(Some(id))?;
    require_session(session)?;
    let username = session.username();
    match info.token_type.as_str() {
        "accesstoken" => {
            if info.client_id.len() != 32 || !info.client_id.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err("Spotify receiver did not supply a valid streaming client id".into());
            }
            // Hardware such as Yamaha uses the ordinary desktop streaming
            // access token, with the receiver's clientID as clientKey. This is
            // deliberately NOT an OAuth app token from the shell and NOT a DH
            // blob. login5 uses the engine's already authenticated credential.
            let token = session.login5().auth_token().await
                .map_err(|_| "Could not obtain the Spotify session token for receiver activation")?;
            require_session(session)?;
            add_user(&client, endpoint, &info, &username, &token.access_token,
                &info.client_id, "accesstoken").await?.check()?;
        }
        "" | "default" => {
            // A sleeping receiver may need an empty addUser before it can
            // create its DH key. No credential is sent during this wake step.
            if info.availability == "NOT-LOADED" && info.public_key == "SU5WQUxJRA==" {
                let status = add_user(&client, endpoint, &info, &username, "", "", "default").await?;
                if status.status != 203 { status.check()?; }
                for _ in 0..12 {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    require_session(session)?;
                    info = get_info(&client, endpoint).await?;
                    info.validate(Some(id))?;
                    if info.availability != "NOT-LOADED" { break; }
                }
                if !matches!(info.token_type.as_str(), "" | "default") {
                    return Err("Spotify receiver authentication changed; refresh the device list".into());
                }
            }
            let mut credentials = session.cache().and_then(|cache| cache.credentials())
                .ok_or("Reusable Spotify credentials are unavailable")?;
            if credentials.username.as_deref() != Some(username.as_str())
                || !matches!(credentials.auth_type,
                    AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS
                    | AuthenticationType::AUTHENTICATION_STORED_FACEBOOK_CREDENTIALS)
            {
                return Err("Reusable credentials do not belong to the active Spotify session".into());
            }
            credentials.auth_data = session.auth_data();
            let (blob, client_key) = encrypted_blob(&credentials, id, &info.public_key)?;
            require_session(session)?;
            add_user(&client, endpoint, &info, &username, &blob, &client_key, "default").await?.check()?;
        }
        _ => return Err("This receiver requires an unsupported Spotify authentication method".into()),
    }
    // A successful ZeroConf status confirms pairing (the SDK must wait for
    // LoggedIn). Also wait for the account to be reported by implementations
    // that acknowledge addUser before their asynchronous login has completed.
    for _ in 0..12 {
        require_session(session)?;
        let info = get_info(&client, endpoint).await?;
        info.validate(Some(id))?;
        require_session(session)?;
        if info.active_user.as_deref().is_none_or(|user| user == username) { return Ok(()); }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("The Spotify receiver did not confirm pairing with the signed-in account".into())
}

fn encrypted_blob(credentials: &Credentials, device_id: &str, public_key: &str) -> Result<(String, String), String> {
    let remote = decode_public_key(public_key)?;
    let mut rng = rand::rng();
    let keys = DhLocalKeys::random(&mut rng);
    let mut iv = [0u8; 16];
    rng.fill_bytes(&mut iv);
    let inner = credential_blob(credentials, device_id)?;
    let blob = seal_blob(&inner, &keys.shared_secret(&remote), &iv);
    Ok((blob, BASE64.encode(keys.public_key())))
}

fn decode_public_key(encoded: &str) -> Result<Vec<u8>, String> {
    const PRIME: &str = concat!(
        "ffffffffffffffffc90fdaa22168c234c4c6628b80dc1cd129024e088a67cc74",
        "020bbea63b139b22514a08798e3404ddef9519b3cd3a431b302b0a6df25f1437",
        "4fe1356d6d51c245e485b576625e7ec6f44c42e9a63a3620ffffffffffffffff",
    );
    let invalid = || "Spotify receiver supplied an invalid Diffie-Hellman public key".to_owned();
    if encoded.len() > 128 { return Err(invalid()); }
    let key = BASE64.decode(encoded).map_err(|_| invalid())?;
    if key.is_empty() || key.len() > 96 { return Err(invalid()); }
    let mut padded = [0u8; 96];
    padded[96 - key.len()..].copy_from_slice(&key);
    let mut prime_minus_one = [0u8; 96];
    hex::decode_to_slice(PRIME, &mut prime_minus_one).unwrap();
    prime_minus_one[95] -= 1;
    let mut one = [0u8; 96];
    one[95] = 1;
    if padded <= one || padded >= prime_minus_one { return Err(invalid()); }
    Ok(key)
}

fn credential_blob(credentials: &Credentials, device_id: &str) -> Result<String, String> {
    let username = credentials.username.as_deref().ok_or("Spotify credentials have no username")?;
    // The receiver's credential format uses at most two bytes per length, and
    // the SDK's complete (base64) encrypted blob has a 2047-byte limit.
    if username.is_empty() || username.len() > 256 || credentials.auth_data.is_empty()
        || credentials.auth_data.len() > 1024
    {
        return Err("Spotify reusable credentials have an unsupported size".into());
    }
    let mut blob = Vec::with_capacity(username.len() + credentials.auth_data.len() + 32);
    blob.push(0x49);
    write_bytes(&mut blob, username.as_bytes());
    blob.extend_from_slice(&[0x50, credentials.auth_type.value() as u8, 0x51]);
    write_bytes(&mut blob, &credentials.auth_data);
    let padding = 16 - blob.len() % 16;
    blob.resize(blob.len() + padding - 1, 0);
    blob.push(padding as u8);
    for index in 16..blob.len() { blob[index] ^= blob[index - 16]; }

    let secret = Sha1::digest(device_id.as_bytes());
    let mut derived = [0u8; 20];
    pbkdf2::pbkdf2_hmac::<Sha1>(&secret, username.as_bytes(), 256, &mut derived);
    let mut key = [0u8; 24];
    key[..20].copy_from_slice(&Sha1::digest(derived));
    key[20..].copy_from_slice(&20u32.to_be_bytes());
    let cipher = aes::Aes192::new_from_slice(&key).unwrap();
    for block in blob.chunks_exact_mut(16) { cipher.encrypt_block(block.into()); }
    let inner = BASE64.encode(blob);
    if (16 + inner.len() + 20).div_ceil(3) * 4 > 2047 {
        return Err("Spotify reusable credentials exceed the receiver blob limit".into());
    }
    Ok(inner)
}

fn write_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    let length = bytes.len();
    if length < 128 { output.push(length as u8); }
    else { output.extend_from_slice(&[(length as u8 & 0x7f) | 0x80, (length >> 7) as u8]); }
    output.extend_from_slice(bytes);
}

fn hmac_sha1(key: &[u8], value: &[u8]) -> [u8; 20] {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).unwrap();
    mac.update(value);
    mac.finalize().into_bytes().into()
}

fn seal_blob(inner: &str, shared_secret: &[u8], iv: &[u8; 16]) -> String {
    let digest = Sha1::digest(shared_secret);
    let base_key = &digest[..16];
    let encryption_key = hmac_sha1(base_key, b"encryption");
    let checksum_key = hmac_sha1(base_key, b"checksum");
    let mut packet = Vec::with_capacity(16 + inner.len() + 20);
    packet.extend_from_slice(iv);
    packet.extend_from_slice(inner.as_bytes());
    let mut cipher = Aes128Ctr::new_from_slices(&encryption_key[..16], iv).unwrap();
    cipher.apply_keystream(&mut packet[16..]);
    let checksum = hmac_sha1(&checksum_key, &packet[16..]);
    packet.extend_from_slice(&checksum);
    BASE64.encode(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver_info() -> Info {
        serde_json::from_value(serde_json::json!({
            "status": 101, "spotifyError": 0, "deviceID": "receiver",
            "remoteName": "Living room", "deviceType": "AVR", "tokenType": "accesstoken",
            "clientID": "59c7bf046992472ab0689f5fc7fa08e2", "version": "2.9.0",
        })).unwrap()
    }

    #[test]
    fn connect_rejects_nonlocal_addresses_and_url_injection() {
        for address in ["127.0.0.1:80", "8.8.8.8:80", "0.0.0.0:80", "224.0.0.251:80",
            "[::1]:80", "[::ffff:127.0.0.1]:80", "192.168.1.2:0"] {
            assert!(Endpoint::new(address.parse().unwrap(), "/zc").is_err());
        }
        let address = "192.168.1.2:80".parse().unwrap();
        for path in ["http://evil/", "//evil/", "/\\evil", "/zc?target=evil", "/zc#evil", "/\r\n"] {
            assert!(Endpoint::new(address, path).is_err());
        }
        let endpoint = Endpoint::new(address, "/goform/spotifyConfig").unwrap();
        assert_eq!(endpoint.url().host_str(), Some(RECEIVER_HOST));
        assert_eq!(endpoint.url().path(), "/goform/spotifyConfig");
        assert!(Endpoint::new("[fe80::1234%5]:80".parse().unwrap(), "/zc").is_ok());
    }

    #[test]
    fn connect_requires_matching_identity_and_successful_protocol_status() {
        let mut info = receiver_info();
        assert!(info.validate(Some("receiver")).is_ok());
        assert!(info.validate(Some("different")).is_err());
        info.status.spotify_error = -119;
        assert!(info.validate(Some("receiver")).is_err());
        info.status.spotify_error = 0;
        info.status.status = 202;
        assert!(info.validate(None).is_err());
        info.status.status = 101;
        info.remote_name.push('\n');
        assert!(info.validate(None).is_err());
    }

    #[test]
    fn connect_rejects_degenerate_or_oversized_dh_keys() {
        for key in [vec![], vec![0], vec![1], vec![0; 96], vec![255; 96], vec![2; 97]] {
            assert!(decode_public_key(&BASE64.encode(key)).is_err());
        }
        assert!(decode_public_key("not base64!").is_err());
        let keys = DhLocalKeys::random(&mut rand::rng());
        assert_eq!(decode_public_key(&BASE64.encode(keys.public_key())).unwrap(), keys.public_key());
    }

    #[test]
    fn connect_blob_is_accepted_by_librespot_receiver_with_multibyte_lengths() {
        let credentials = Credentials {
            username: Some("connect-test-user".into()),
            auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
            auth_data: (0..180).collect(),
        };
        let receiver = DhLocalKeys::random(&mut rand::rng());
        let (blob, client_key) = encrypted_blob(&credentials, "receiver", &BASE64.encode(receiver.public_key())).unwrap();
        let packet = BASE64.decode(blob).unwrap();
        let shared = receiver.shared_secret(&BASE64.decode(client_key).unwrap());
        let digest = Sha1::digest(shared);
        let checksum_key = hmac_sha1(&digest[..16], b"checksum");
        let encryption_key = hmac_sha1(&digest[..16], b"encryption");
        let end = packet.len() - 20;
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&checksum_key).unwrap();
        mac.update(&packet[16..end]);
        mac.verify_slice(&packet[end..]).unwrap();
        let mut plaintext = packet[16..end].to_vec();
        Aes128Ctr::new_from_slices(&encryption_key[..16], &packet[..16]).unwrap()
            .apply_keystream(&mut plaintext);
        let decoded = Credentials::with_blob("connect-test-user", plaintext, "receiver").unwrap();
        assert_eq!(decoded, credentials);
        let mut changed = packet[16..end].to_vec();
        changed[0] ^= 1;
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&checksum_key).unwrap();
        mac.update(&changed);
        assert!(mac.verify_slice(&packet[end..]).is_err());
    }

    #[tokio::test]
    async fn connect_discovered_endpoints_cannot_cross_session_or_expiry() {
        let session = Session::new(Default::default(), None);
        session.set_username("connect-test-user");
        let endpoint = Endpoint::new("192.168.1.2:80".parse().unwrap(), "/zc").unwrap();
        let mut registry = Registry {
            owner: Some(session.clone()), discovered_at: Some(Instant::now()),
            endpoints: HashMap::from([("receiver".into(), endpoint)]),
        };
        assert!(registry.endpoint(&session, "receiver").is_ok());
        assert!(registry.endpoint(&session, "http://192.168.1.3/zc").is_err());
        registry.discovered_at = Some(Instant::now() - ENDPOINT_LIFETIME - Duration::from_secs(1));
        assert!(registry.endpoint(&session, "receiver").is_err());
        registry.discovered_at = Some(Instant::now());
        let other = Session::new(Default::default(), None);
        other.set_username("another-test-user");
        assert!(registry.endpoint(&other, "receiver").is_err());
        session.shutdown();
        assert!(registry.endpoint(&session, "receiver").is_err());
    }

    async fn serve_response(response: String) -> Result<Status, String> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("HTTP fixture did not receive a connection: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request);
            let _ = socket.write_all(response.as_bytes());
        });
        // Bypass only the mDNS boundary for this loopback HTTP fixture.
        let endpoint = Endpoint { address, path: "/zc".into() };
        let result = response_json(endpoint.client().unwrap().get(endpoint.url())).await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn connect_lan_http_refuses_redirects_and_oversized_or_echoed_responses() {
        let error = serve_response("HTTP/1.1 302 Found\r\nLocation: http://example.com/secret\r\nContent-Length: 0\r\n\r\n".into()).await.err().unwrap();
        assert_eq!(error, "Local Spotify receiver returned HTTP 302");
        let error = serve_response(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", MAX_RESPONSE_BYTES + 1)).await.err().unwrap();
        assert!(error.contains("size limit"));
        let error = serve_response(format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}", "x".repeat(MAX_RESPONSE_BYTES + 1))).await.err().unwrap();
        assert!(error.contains("size limit"));
        let body = "credential-that-must-not-appear-in-errors";
        let error = serve_response(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())).await.err().unwrap();
        assert!(!error.contains(body));
        assert!(error.contains("invalid protocol data"));
        let body = r#"{"status":202,"spotifyError":-119,"statusString":"synthetic-secret"}"#;
        let status = serve_response(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())).await.unwrap();
        let error = status.check().unwrap_err();
        assert!(!error.contains("synthetic-secret"));
        assert!(error.contains("202"));
    }
}
