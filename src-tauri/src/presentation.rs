//! Presentation mode: a run for recording the app against a real account
//! that shows none of the account's library, name or listening, and leaves
//! nothing behind.
//!
//! It is chosen at launch and only there: `SPOTIFINE_PRESENTATION` names a
//! JSON file, read once by [`from_environment`]. Without the variable nothing
//! here is active and the app is the ordinary one. The file names who is on
//! screen and what their library holds, in order:
//!
//! ```json
//! { "name": "Alex Rivera",
//!   "library": ["spotify:playlist:…", "spotify:album:…", "spotify:artist:…"] }
//! ```
//!
//! While it is on, this module is where the library and the identity come
//! from, and the rest of the run is read-only:
//! - the library rows are [`Presentation::library`], the configured playlists
//!   and albums read through the same browses that open them; the rail's
//!   artists are [`Presentation::artists`]. Nothing of the account's own
//!   collection is loaded from disk or fetched for display;
//! - the window shows [`Presentation::name`] for the account
//!   (`get_presentation`), and the account's own playlists carry it as their
//!   owner ([`Presentation::mask_owner`]);
//! - every command that would change the account, its library or stored
//!   state is refused at the IPC boundary ([`refuses`]);
//! - nothing is written to the app's data directory (`app::persistent`), the
//!   webview is private so its storage ends with the run, and the engine runs
//!   with `--presentation`: plays are neither recorded nor reported.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use spotifine_common::protocol::PlaylistRead;
use serde::Deserialize;

use crate::engine_client::EngineClient;
use crate::log;
use crate::types::{AlbumDetail, Artist, ArtistDetail, Playlist, PlaylistDetail};

/// The launch variable: the path of the presentation file.
pub const VARIABLE: &str = "SPOTIFINE_PRESENTATION";

/// What a refused command answers. The window shows it where the write was
/// asked for.
pub const REFUSAL: &str = "Not available in presentation mode.";

/// Commands a presentation run refuses. Each would change the account, its
/// library or the app's stored state, or read the account's own collection.
const REFUSED: &[&str] = &[
    // The library and its playlists, local activity stamps included.
    "create_playlist",
    "rename_playlist",
    "delete_playlist",
    "follow_playlist",
    "create_folder",
    "rename_folder",
    "delete_folder",
    "move_library_item",
    "add_playlist_tracks",
    "remove_playlist_tracks",
    "reorder_playlist_tracks",
    "touch_playlist",
    "touch_playlist_activity",
    "browse_liked_songs",
    // The account: likes and follows, its developer app, its Canvas
    // setting, its devices and its session.
    "personal_api_set_saved",
    "personal_api_configure",
    "personal_api_authorize",
    "personal_api_disconnect",
    "personal_api_devices",
    "enable_account_canvas",
    "logout",
    // Stored state, and the settings this run holds fixed.
    "clear_history",
    "clear_cache",
    "set_audio_cache_limit",
    "set_launch_at_login",
    "set_minimize_to_tray",
    "set_spotify_listening_enabled",
    "set_devices_enabled",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Playlist(String),
    Album(String),
    Artist(String),
}

impl Item {
    fn parse(uri: &str) -> Result<Self, String> {
        let mut parts = uri.trim().split(':');
        let (Some("spotify"), Some(kind), Some(id), None) = (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(format!("\"{uri}\" is not a Spotify URI such as spotify:playlist:<id>"));
        };
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(format!("\"{uri}\" has no valid Spotify id"));
        }
        match kind {
            "playlist" => Ok(Self::Playlist(id.to_owned())),
            "album" => Ok(Self::Album(id.to_owned())),
            "artist" => Ok(Self::Artist(id.to_owned())),
            _ => Err(format!("\"{uri}\": the library holds playlists, albums and artists")),
        }
    }
}

#[derive(Debug)]
pub struct Presentation {
    /// Shown wherever the account's name would be.
    pub name: String,
    /// The library, in the file's order, each item once.
    pub library: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    name: String,
    library: Vec<String>,
}

/// Parses a presentation file. A misspelt key is an error rather than an
/// empty library, and a repeated item is shown once.
pub fn parse(json: &str) -> Result<Presentation, String> {
    let file: File = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let name = file.name.trim().to_owned();
    if name.is_empty() {
        return Err("\"name\" is empty".to_owned());
    }
    let mut library: Vec<Item> = Vec::with_capacity(file.library.len());
    for uri in &file.library {
        let item = Item::parse(uri)?;
        if !library.contains(&item) {
            library.push(item);
        }
    }
    Ok(Presentation { name, library })
}

/// Reads the file `SPOTIFINE_PRESENTATION` names, once, at launch. An unset or
/// empty variable is an ordinary run; a file that cannot be read or parsed is
/// an error, never an ordinary run in its place.
pub fn from_environment() -> Result<Option<Presentation>, String> {
    let Some(path) = std::env::var_os(VARIABLE).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(&path);
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    parse(&text).map(Some).map_err(|error| format!("{}: {error}", path.display()))
}

static PRESENTATION: RwLock<Option<Arc<Presentation>>> = parking_lot::const_rwlock(None);

/// Sets the run's presentation, once, at launch (tests set and clear it).
pub fn install(presentation: Option<Presentation>) {
    *PRESENTATION.write() = presentation.map(Arc::new);
}

pub fn get() -> Option<Arc<Presentation>> {
    PRESENTATION.read().clone()
}

pub fn active() -> bool {
    PRESENTATION.read().is_some()
}

/// Whether this run refuses `command`: see [`REFUSED`].
pub fn refuses(command: &str) -> bool {
    active() && REFUSED.contains(&command)
}

impl Presentation {
    /// The rail's and Home's rows: the configured playlists and albums, in the
    /// file's order. An item that cannot be read is left out and logged; when
    /// none can be, that is the error, so a failed read never stands in for
    /// the configured library as an empty one.
    pub async fn library(&self, client: &Arc<EngineClient>, account: &str) -> Result<Vec<Playlist>, String> {
        let items: Vec<Item> = self.library.iter()
            .filter(|item| !matches!(item, Item::Artist(_)))
            .cloned()
            .collect();
        let mut rows = read_all(&items, client, |client, item| async move {
            match item {
                Item::Playlist(id) => match client.browse_playlist(&id, None).await? {
                    PlaylistRead::Changed(browse) => Ok(PlaylistDetail::from(browse).playlist),
                    PlaylistRead::Unchanged(_) => Err("the engine vouched for rows that were never offered".to_owned()),
                },
                Item::Album(id) => Ok(album_row(&id, AlbumDetail::from(client.browse_album(&id).await?))),
                Item::Artist(_) => unreachable!("artists are not library rows"),
            }
        })
        .await?;
        for row in &mut rows {
            self.mask_owner(row, account);
        }
        Ok(rows)
    }

    /// The rail's artists: the configured ones, in the file's order.
    pub async fn artists(&self, client: &Arc<EngineClient>) -> Result<Vec<Artist>, String> {
        let items: Vec<Item> = self.library.iter()
            .filter(|item| matches!(item, Item::Artist(_)))
            .cloned()
            .collect();
        read_all(&items, client, |client, item| async move {
            let Item::Artist(id) = item else { unreachable!("only artists are read here") };
            Ok(ArtistDetail::from(client.browse_artist(&id).await?).artist)
        })
        .await
    }

    /// The account's own playlist is shown as the presented person's.
    pub fn mask_owner(&self, playlist: &mut Playlist, account: &str) {
        if !account.is_empty() && playlist.owner_id == account {
            playlist.owner.clone_from(&self.name);
        }
    }
}

/// An album as a library row. Its URI is what tells the window to open it
/// as an album.
fn album_row(id: &str, detail: AlbumDetail) -> Playlist {
    Playlist {
        id: id.to_owned(),
        uri: format!("spotify:album:{id}"),
        name: detail.album.name,
        owner: detail.album.artist_names.join(", "),
        cover_url: detail.album.cover_url,
        tracks_total: detail.tracks.len() as u32,
        ..Playlist::default()
    }
}

/// Reads every item at once and answers in their order, leaving out (and
/// logging) the ones that failed; all of them failing is the error.
async fn read_all<T, F, Fut>(items: &[Item], client: &Arc<EngineClient>, read: F) -> Result<Vec<T>, String>
where
    T: Send + 'static,
    F: Fn(Arc<EngineClient>, Item) -> Fut,
    Fut: Future<Output = Result<T, String>> + Send + 'static,
{
    let mut reads = tokio::task::JoinSet::new();
    for (index, item) in items.iter().enumerate() {
        let read = read(client.clone(), item.clone());
        reads.spawn(async move { (index, read.await) });
    }
    let mut answers: Vec<Option<T>> = items.iter().map(|_| None).collect();
    let mut last_error = None;
    while let Some(joined) = reads.join_next().await {
        match joined {
            Ok((index, Ok(value))) => answers[index] = Some(value),
            Ok((index, Err(error))) => {
                log::warn(&format!("presentation item {:?} could not be read: {error}", items[index]));
                last_error = Some(error);
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    let answers: Vec<T> = answers.into_iter().flatten().collect();
    match last_error {
        Some(error) if answers.is_empty() => Err(error),
        _ => Ok(answers),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Installs a presentation for one test and clears it however the test
    /// ends. Native tests run serially, so no other test sees it.
    pub(crate) struct Installed;

    impl Installed {
        pub(crate) fn new(name: &str, library: Vec<Item>) -> Self {
            install(Some(Presentation { name: name.to_owned(), library }));
            Installed
        }
    }

    impl Drop for Installed {
        fn drop(&mut self) {
            install(None);
        }
    }

    #[test]
    fn a_presentation_file_names_the_person_and_the_library_in_order() {
        let presentation = parse(r#"{
            "name": "  Alex Rivera ",
            "library": [
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
                "spotify:album:4LH4d3cOWNNsVw41Gqt2kv",
                "spotify:artist:0OdUWJ0sBjDrqHygGUXeCF",
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"
            ]
        }"#).expect("valid file");
        assert_eq!(presentation.name, "Alex Rivera");
        assert_eq!(presentation.library, vec![
            Item::Playlist("37i9dQZF1DXcBWIGoYBM5M".into()),
            Item::Album("4LH4d3cOWNNsVw41Gqt2kv".into()),
            Item::Artist("0OdUWJ0sBjDrqHygGUXeCF".into()),
        ], "in the file's order, a repeat shown once");
    }

    #[test]
    fn a_presentation_file_that_cannot_be_trusted_is_refused() {
        for (file, why) in [
            (r#"{"name": "A", "libary": []}"#, "a misspelt key"),
            (r#"{"library": []}"#, "no name"),
            (r#"{"name": " ", "library": []}"#, "an empty name"),
            (r#"{"name": "A", "library": ["spotify:track:4uLU6hMCjMI75M1A2tKUQC"]}"#, "a kind the library cannot hold"),
            (r#"{"name": "A", "library": ["https://open.spotify.com/album/4LH4d3cOWNNsVw41Gqt2kv"]}"#, "a link, not a URI"),
            (r#"{"name": "A", "library": ["spotify:album:"]}"#, "no id"),
            (r#"{"name": "A", "library": ["spotify:album:4LH4/../x"]}"#, "a malformed id"),
        ] {
            assert!(parse(file).is_err(), "{why} is an error, not an ordinary run");
        }
    }

    #[test]
    fn writes_are_refused_only_while_presenting() {
        assert!(!refuses("create_playlist"), "an ordinary run refuses nothing");
        let _presenting = Installed::new("Alex", Vec::new());
        for command in ["create_playlist", "personal_api_set_saved", "logout", "clear_history", "touch_playlist"] {
            assert!(refuses(command), "{command} is refused");
        }
        for command in ["play_queue", "browse_playlist", "search", "get_history", "set_volume"] {
            assert!(!refuses(command), "{command} is answered");
        }
    }

    /// Reviewed as safe to answer in a presentation run: playback, reads, and
    /// writes that stay in memory (the engine keeps a presentation run's track
    /// edits to itself). The library reads among them answer the configured
    /// library (`browse_followed_artists`, `browse_saved_shows`) or refuse the
    /// account's own profile (`browse_profile`).
    const ANSWERED: &[&str] = &[
        "check_for_update", "relaunch_app", "play", "set_window_visible", "pause", "next", "previous", "seek",
        "set_volume", "set_shuffle", "set_repeat", "set_playback_speed", "play_queue",
        "play_playlist_row",
        "preview_track_edit", "restore_preview", "play_queue_index", "add_queue", "add_queue_batch",
        "remove_queue", "move_queue", "get_history", "get_track_waveform", "cancel_track_waveform",
        "get_track_edit", "save_track_edit", "delete_track_edit", "set_playlist_track_edit_enabled",
        "set_playlist_track_excluded", "search", "browse_playlist", "hydrate_library_covers",
        "browse_radio", "browse_playlist_recommendations", "browse_track", "browse_album",
        "browse_artist", "browse_artist_songwriter", "browse_artist_catalogue", "browse_show",
        "show_episode_index", "show_episode_rows", "search_show_episodes", "play_show_episodes", "close_show",
        "browse_episode", "browse_profile", "browse_track_credits", "browse_canvas",
        "browse_followed_artists", "browse_saved_shows", "login", "set_normalisation",
        "personal_api_status", "personal_api_contains", "select_output", "get_state",
        "get_presentation", "get_cache_stats", "set_start_minimized", "set_animated_canvas",
        "get_track_playlists",
    ];

    #[test]
    fn every_command_the_window_can_call_is_refused_or_reviewed() {
        let source = include_str!("lib.rs");
        let start = source.find("generate_handler![").expect("the command list") + "generate_handler![".len();
        let list = &source[start..start + source[start..].find(']').expect("the list ends")];
        let registered: Vec<&str> = list.split(',')
            .map(|entry| entry.trim().trim_start_matches("commands::"))
            .filter(|name| !name.is_empty())
            .collect();
        assert!(registered.len() > 50, "the list was read");
        for command in &registered {
            assert!(
                REFUSED.contains(command) != ANSWERED.contains(command),
                "{command} must be in exactly one of REFUSED and ANSWERED"
            );
        }
        for command in REFUSED.iter().chain(ANSWERED) {
            assert!(registered.contains(command), "{command} is not a registered command");
        }
    }

    #[test]
    fn the_accounts_own_playlist_carries_the_presented_name() {
        let presentation = Presentation { name: "Alex Rivera".into(), library: Vec::new() };
        let mut own = Playlist { owner: "Real Name".into(), owner_id: "real-account".into(), ..Playlist::default() };
        let mut other = Playlist { owner: "Spotify".into(), owner_id: "spotify".into(), ..Playlist::default() };
        presentation.mask_owner(&mut own, "real-account");
        presentation.mask_owner(&mut other, "real-account");
        assert_eq!(own.owner, "Alex Rivera");
        assert_eq!(other.owner, "Spotify");
        let mut unowned = Playlist { owner: "Someone".into(), ..Playlist::default() };
        presentation.mask_owner(&mut unowned, "");
        assert_eq!(unowned.owner, "Someone", "no account, nothing is anyone's");
    }

    #[test]
    fn an_album_row_opens_as_an_album() {
        let detail = AlbumDetail {
            album: crate::types::Album {
                id: "browse-id".into(),
                name: "Random Access Memories".into(),
                artist_names: vec!["Daft Punk".into()],
                cover_url: "https://i.scdn.co/image/cover".into(),
                ..Default::default()
            },
            tracks: vec![Default::default(); 13],
        };
        let row = album_row("4m2880jivSbbyEGAKfITCa", detail);
        assert_eq!(row.uri, "spotify:album:4m2880jivSbbyEGAKfITCa");
        assert_eq!(row.id, "4m2880jivSbbyEGAKfITCa");
        assert_eq!(row.owner, "Daft Punk");
        assert_eq!(row.tracks_total, 13);
        assert_eq!((row.last_played, row.last_activity), (None, None), "no listening of its own");
    }
}
