//! Shared serde types for the Tauri <-> frontend contract.
//!
//! Field names are the exact JSON keys the frontend consumes; they mirror
//! the engine's protocol payloads (see `renderer_engine::protocol`)
//! so the engine's line-JSON state and browse messages deserialize straight
//! into these types.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use renderer_engine::protocol::{
    normalize_canonical_playlist_description, AlbumBrowse, AlbumRef, ArtistBrowse,
    ArtistCataloguePage, ArtistImage, ArtistOverview, ArtistPick, ArtistPickItem, ArtistRef,
    ArtistReleaseCounts, ArtistReleases, ArtistTopCity, CreditArtist, CreditRole, EpisodeRef,
    HistoryItem as EngineHistoryItem, HistoryPage as EngineHistoryPage, LibraryItem, LibraryNode,
    LikedSongsPage, PlaylistBrowse, PlaylistRecommendations, PlaylistRef, RadioBrowse,
    SearchBrowse, SearchTopRef, ShowBrowse, ShowRef, SongwriterPlaylist as EngineSongwriterPlaylist,
    TrackCredits, TrackEdit, TrackRef, TrackWaveform as EngineTrackWaveform, UserProfile,
};

/// The verified songwriter playlist returned by the artist's separate
/// discovery request.
///
/// The playlist remains nested so the frontend can use the same navigation
/// metadata as every other playlist surface, while `tracks` keeps the
/// engine's official playlist order for direct row playback.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SongwriterPlaylist {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

impl From<EngineSongwriterPlaylist> for SongwriterPlaylist {
    fn from(source: EngineSongwriterPlaylist) -> Self {
        Self {
            playlist: Playlist::from(&source.playlist),
            tracks: source.tracks.into_iter().map(Track::from).collect(),
        }
    }
}

/// One playable track. Field-for-field identical to the engine's `TrackRef`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Track {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    /// Artist ids parallel to `artist_names` — same length, same order — so
    /// every credited artist on a track is individually linkable instead of
    /// just the primary.
    ///
    /// The two are always the same length: the engine builds them in one pass
    /// over one list, and [`align_artist_ids`] repairs entries loaded from a
    /// cache written before this field existed. An individual id may still be
    /// empty for an artist with no resolvable id, so render a name as a link
    /// only when its id is non-empty.
    pub artist_ids: Vec<String>,
    /// Primary artist, unchanged and still used on its own in several places.
    pub artist_id: String,
    pub album_id: String,
    pub album_name: String,
    pub cover_url: String,
    pub duration_ms: u32,
    /// Lifetime Spotify play count on surfaces whose official payload carries
    /// it (albums and artist Popular). Missing everywhere else.
    pub play_count: Option<u64>,
    /// Unix timestamp in milliseconds when this playlist item was added.
    /// Absent for tracks sourced from albums, search, or legacy caches.
    pub added_at: Option<i64>,
    /// Stable metadata/account restriction. Runtime player/network failures
    /// never mutate this persisted field.
    pub unavailable: bool,
    pub unavailable_reason: Option<String>,
    /// Whether this track's audio is already in the app-owned librespot cache,
    /// as measured by the engine at browse time.
    ///
    /// This one field is EPHEMERAL where every other field here is durable, so
    /// it is never written to `playlist_tracks_cache.json`: an audio cache
    /// that has been cleared or pruned since the file was written would
    /// otherwise show download marks for tracks that are no longer on disk.
    pub cached: bool,
    /// Compact queue source context used when a play becomes a local history
    /// row. It is intentionally ephemeral metadata, not part of the row.
    pub context: String,
    /// Frozen queue-only edit snapshot. Browse/cache tracks leave it absent.
    pub effective_edit: Option<TrackEdit>,
}

impl From<TrackRef> for Track {
    fn from(track: TrackRef) -> Self {
        Self {
            id: track.id,
            uri: track.uri,
            name: track.name,
            artist_names: track.artist_names,
            artist_ids: track.artist_ids,
            artist_id: track.artist_id,
            album_id: track.album_id,
            album_name: track.album_name,
            cover_url: track.cover_url,
            duration_ms: track.duration_ms,
            play_count: track.play_count,
            added_at: track.added_at,
            unavailable: track.unavailable,
            unavailable_reason: track.unavailable_reason,
            context: track.context,
            cached: track.cached,
            effective_edit: track.effective_edit,
        }
    }
}

impl From<&TrackRef> for Track {
    fn from(track: &TrackRef) -> Self {
        Self {
            id: track.id.clone(),
            uri: track.uri.clone(),
            name: track.name.clone(),
            artist_names: track.artist_names.clone(),
            artist_ids: track.artist_ids.clone(),
            artist_id: track.artist_id.clone(),
            album_id: track.album_id.clone(),
            album_name: track.album_name.clone(),
            cover_url: track.cover_url.clone(),
            duration_ms: track.duration_ms,
            play_count: track.play_count,
            added_at: track.added_at,
            unavailable: track.unavailable,
            unavailable_reason: track.unavailable_reason.clone(),
            context: track.context.clone(),
            cached: track.cached,
            effective_edit: track.effective_edit.clone(),
        }
    }
}

impl From<Track> for TrackRef {
    fn from(track: Track) -> Self {
        Self {
            id: track.id,
            uri: track.uri,
            name: track.name,
            artist_names: track.artist_names,
            artist_ids: track.artist_ids,
            artist_id: track.artist_id,
            album_id: track.album_id,
            album_name: track.album_name,
            cover_url: track.cover_url,
            duration_ms: track.duration_ms,
            play_count: track.play_count,
            added_at: track.added_at,
            unavailable: track.unavailable,
            unavailable_reason: track.unavailable_reason,
            context: track.context,
            cached: track.cached,
            effective_edit: track.effective_edit,
        }
    }
}

/// Frontend-facing copy of the engine waveform payload. Keeping this at the
/// Tauri boundary makes the line-protocol conversion explicit while preserving
/// the shared snake_case field contract.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TrackWaveform {
    pub track_id: String,
    pub duration_ms: u32,
    pub interval_ms: u16,
    pub bin_count: u32,
    pub peaks_base64: String,
}

impl From<EngineTrackWaveform> for TrackWaveform {
    fn from(waveform: EngineTrackWaveform) -> Self {
        Self {
            track_id: waveform.track_id,
            duration_ms: waveform.duration_ms,
            interval_ms: waveform.interval_ms,
            bin_count: waveform.bin_count,
            peaks_base64: waveform.peaks_base64,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct HistoryEntry {
    pub track_id: String,
    pub started_at: i64,
    pub ms_played: u64,
    pub completed: bool,
    pub context: String,
    pub track: Track,
}

impl From<EngineHistoryItem> for HistoryEntry {
    fn from(entry: EngineHistoryItem) -> Self {
        Self {
            track_id: entry.row.track_id,
            started_at: entry.row.started_at,
            ms_played: entry.row.ms_played,
            completed: entry.row.completed,
            context: entry.row.context,
            track: Track::from(entry.track),
        }
    }
}

/// One window of the listening archive, as the view consumes it.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct HistoryPageDetail {
    pub entries: Vec<HistoryEntry>,
    pub total: usize,
    pub recorded: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

impl From<EngineHistoryPage> for HistoryPageDetail {
    fn from(page: EngineHistoryPage) -> Self {
        Self {
            entries: page.items.into_iter().map(HistoryEntry::from).collect(),
            total: page.total,
            recorded: page.recorded,
            offset: page.offset,
            next_offset: page.next_offset,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LikedSongsDetail {
    pub tracks: Vec<Track>,
    pub next_cursor: Option<String>,
}

impl From<LikedSongsPage> for LikedSongsDetail {
    fn from(page: LikedSongsPage) -> Self {
        Self {
            tracks: page.tracks.into_iter().map(Track::from).collect(),
            next_cursor: page.next_cursor,
        }
    }
}
/// A playlist in the user's library (rootlist).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// Rich-text markup is removed at the engine/Tauri boundary.
    pub description: String,
    /// Display name of the owning user (falls back to the username).
    pub owner: String,
    pub owner_id: String,
    pub cover_url: String,
    /// Up to [`COVER_URL_CANDIDATES`] distinct track covers, in track order,
    /// derived the last time the playlist was browsed. Spotify's rootlist
    /// returns no artwork at all for most playlists, so this is what the
    /// surfaces that only ever see the library payload — the sidebar, the home
    /// grid — mosaic instead of dropping to a monogram tile. Empty until a
    /// browse fills it in; a real `cover_url` still outranks it.
    pub cover_urls: Vec<String>,
    pub collaborative: bool,
    pub tracks_total: u32,
    /// Playlist4 revision hex (the Web API "snapshot id"); empty when the
    /// library listing did not carry a revision.
    pub snapshot_id: String,
    /// Unix seconds when playback was last started *from* this playlist, or
    /// `None` for one that never has been. Set only after a successful
    /// `play_queue` from this playlist, via the `touch_playlist` command.
    ///
    /// Home's "Recently played" shelf is ordered by this field. It is never
    /// changed merely by opening, adding to, or otherwise editing a playlist.
    /// Local to this app — Spotify has no equivalent and none is sent.
    pub last_played: Option<i64>,
    /// Unix seconds when this playlist was last used in the library, or
    /// `None` for one with no local activity. A successful add-to-playlist or
    /// play stamps it through its corresponding local command.
    ///
    /// Sidebar and library ordering use this field rather than
    /// [`last_played`], so a playlist can be promoted by an add without
    /// appearing in Home's listening history.
    /// Local to this app — Spotify has no equivalent and none is sent.
    pub last_activity: Option<i64>,
}

impl Playlist {
    pub fn artwork_ref(&self) -> PlaylistRef {
        PlaylistRef {
            id: self.id.clone(),
            uri: self.uri.clone(),
            name: self.name.clone(),
            revision: (!self.snapshot_id.is_empty()).then(|| self.snapshot_id.clone()),
            owner_id: self.owner_id.clone(),
            owner_name: self.owner.clone(),
            description: (!self.description.is_empty()).then(|| self.description.clone()),
            cover_url: (!self.cover_url.is_empty()).then(|| self.cover_url.clone()),
            cover_urls: self.cover_urls.clone(),
            track_count: Some(self.tracks_total),
        }
    }

    pub fn fill_artwork(&mut self, metadata: &PlaylistRef) -> bool {
        let mut changed = false;
        if self.cover_url.is_empty() {
            if let Some(url) = metadata.cover_url.as_ref().filter(|url| !url.is_empty()) {
                self.cover_url.clone_from(url);
                changed = true;
            }
        }
        if self.cover_urls.is_empty() && !metadata.cover_urls.is_empty() {
            self.cover_urls.clone_from(&metadata.cover_urls);
            changed = true;
        }
        changed
    }
}

impl From<&PlaylistRef> for Playlist {
    fn from(reference: &PlaylistRef) -> Self {
        Self {
            id: reference.id.clone(),
            uri: reference.uri.clone(),
            name: reference.name.clone(),
            description: normalize_canonical_playlist_description(
                reference.description.as_deref().unwrap_or_default(),
            ),
            owner: if reference.owner_name.is_empty() {
                reference.owner_id.clone()
            } else {
                reference.owner_name.clone()
            },
            owner_id: reference.owner_id.clone(),
            cover_url: reference.cover_url.clone().unwrap_or_default(),
            // Profile artwork samples carry genuine mosaic candidates without
            // a full playlist browse; unknown revisions remain unknown.
            collaborative: false,
            tracks_total: reference.track_count.unwrap_or(0),
            cover_urls: reference.cover_urls.clone(),
            snapshot_id: reference.revision.clone().unwrap_or_default(),
            last_played: None,
            last_activity: None,
        }
    }
}

/// How many distinct track covers a playlist keeps as mosaic candidates: the
/// 2x2 grid the frontend paints for a playlist with no cover of its own.
pub const COVER_URL_CANDIDATES: usize = 4;

/// The first [`COVER_URL_CANDIDATES`] distinct non-empty track covers, in
/// track order.
///
/// Distinctness is the point: a playlist that is one album top to bottom would
/// otherwise mosaic four copies of the same square, which reads as a rendering
/// bug rather than as a cover. Scanning the kept urls beats a set — there are
/// never more than four of them, and the loop stops at the fourth.
pub fn cover_urls_from_tracks(tracks: &[Track]) -> Vec<String> {
    let mut urls: Vec<String> = Vec::with_capacity(COVER_URL_CANDIDATES);
    for track in tracks {
        if track.cover_url.is_empty() || urls.iter().any(|url| url == &track.cover_url) {
            continue;
        }
        urls.push(track.cover_url.clone());
        if urls.len() == COVER_URL_CANDIDATES {
            break;
        }
    }
    urls
}

/// Makes `artist_ids` the same length as `artist_names`, padding with empty
/// ids or dropping any excess.
///
/// This exists for one real case: `playlist_tracks_cache.json` files written
/// before `artist_ids` existed deserialize with names but no ids, and every
/// existing user has one. Without this the two lists would disagree in length
/// for exactly as long as it takes the background refresh to replace them —
/// which is precisely the first paint after an upgrade, when the user is most
/// likely to click. Padding keeps "same length, same order" true
/// unconditionally, so the frontend can zip without a guard, and the padded
/// entries are empty ids, which already means "not linkable".
pub fn align_artist_ids(track: &mut Track) {
    if track.artist_ids.len() != track.artist_names.len() {
        track
            .artist_ids
            .resize(track.artist_names.len(), String::new());
    }
}

/// A playlist opened for browsing: playlist metadata plus its tracks.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaylistDetail {
    #[serde(flatten)]
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
    /// Track ids excluded from automatic playback for this playlist.
    ///
    /// This is playlist-local state supplied by the engine rather than a
    /// property of an individual queue row. Keeping it beside the flattened
    /// playlist payload lets cached detail responses round-trip it without
    /// changing the library-list shape.
    #[serde(default)]
    pub excluded_track_ids: Vec<String>,
}

/// One container holding a track, as answered by `get_track_playlists`.
/// The Liked Songs collection reports the sentinel id `"liked"`; the
/// frontend maps it to the collection route.
#[derive(Clone, Debug, Serialize)]
pub struct TrackPlaylistRef {
    pub id: String,
    pub name: String,
}

impl From<PlaylistBrowse> for PlaylistDetail {
    fn from(browse: PlaylistBrowse) -> Self {
        let revision = browse.revision.unwrap_or_default();
        let excluded_track_ids = browse.excluded_track_ids;
        let tracks: Vec<Track> = browse.tracks.into_iter().map(Track::from).collect();
        Self {
            playlist: Playlist {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                description: normalize_canonical_playlist_description(
                    browse.description.as_deref().unwrap_or_default(),
                ),
                owner: if browse.owner_name.is_empty() {
                    browse.owner_id.clone()
                } else {
                    browse.owner_name
                },
                owner_id: browse.owner_id,
                cover_url: browse.cover_url.unwrap_or_default(),
                // Derived here rather than at the call sites so the candidates
                // and the revision they came from are always written as a pair:
                // a revision bump can replace exactly the tracks they were
                // taken from, so candidates outliving their revision are stale.
                cover_urls: cover_urls_from_tracks(&tracks),
                collaborative: false,
                tracks_total: tracks.len() as u32,
                snapshot_id: revision,
                // A browse cannot know this; the library entry keeps it (see
                // `upsert_playlist`).
                last_played: None,
                last_activity: None,
            },
            tracks,
            excluded_track_ids,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RadioDetail {
    pub seed: Track,
    pub tracks: Vec<Track>,
    pub seed_kind: String,
    pub seed_artist: Option<Artist>,
    pub cover_url: Option<String>,
}

impl From<RadioBrowse> for RadioDetail {
    fn from(browse: RadioBrowse) -> Self {
        Self {
            seed: browse.seed.into(),
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
            seed_kind: browse.seed_kind,
            seed_artist: browse.seed_artist.map(Artist::from),
            cover_url: browse.cover_url,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaylistRecommendationsDetail {
    pub playlist_id: String,
    pub tracks: Vec<Track>,
}

impl From<PlaylistRecommendations> for PlaylistRecommendationsDetail {
    fn from(recommendations: PlaylistRecommendations) -> Self {
        Self {
            playlist_id: recommendations.playlist_id,
            tracks: recommendations
                .tracks
                .into_iter()
                .map(Track::from)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    /// Artist ids parallel to `artist_names`; empty or missing entries are
    /// rendered as plain text rather than navigable links.
    #[serde(default)]
    pub artist_ids: Vec<String>,
    pub cover_url: String,
    /// Release year, or `null` when the release carries no date. An absent
    /// string field is the empty string by this contract's convention, but a
    /// year has no such spelling — 0 would be a date, and a wrong one — so
    /// this one stays an option.
    pub year: Option<u32>,
}

/// Keeps an album's artist ids index-aligned with its names. Album payloads
/// are not currently disk-cached, but applying the same repair as tracks makes
/// older or hand-authored payloads safe to render without false links.
pub fn align_album_artist_ids(album: &mut Album) {
    if album.artist_ids.len() != album.artist_names.len() {
        album
            .artist_ids
            .resize(album.artist_names.len(), String::new());
    }
}

impl From<AlbumRef> for Album {
    fn from(reference: AlbumRef) -> Self {
        let mut album = Self {
            id: reference.id,
            uri: reference.uri,
            name: reference.name,
            artist_names: reference.artist_names,
            artist_ids: reference.artist_ids,
            cover_url: reference.cover_url.unwrap_or_default(),
            year: reference.year,
        };
        align_album_artist_ids(&mut album);
        album
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AlbumDetail {
    #[serde(flatten)]
    pub album: Album,
    pub tracks: Vec<Track>,
}

impl From<AlbumBrowse> for AlbumDetail {
    fn from(browse: AlbumBrowse) -> Self {
        let mut album = Album {
            id: browse.id,
            uri: browse.uri,
            name: browse.name,
            artist_names: browse.artist_names,
            artist_ids: browse.artist_ids,
            cover_url: browse.cover_url.unwrap_or_default(),
            year: browse.year,
        };
        align_album_artist_ids(&mut album);
        Self {
            album,
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Artist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub cover_url: String,
}

impl From<ArtistRef> for Artist {
    fn from(reference: ArtistRef) -> Self {
        Self {
            id: reference.id,
            uri: reference.uri,
            name: reference.name,
            cover_url: reference.portrait_url.unwrap_or_default(),
        }
    }
}

/// An artist's catalogue grouped by release type, mirroring the engine's
/// `ArtistReleases`. A group may be shorter than the artist's true catalogue
/// when the engine's per-browse resolution budget bit; `appears_on` is the
/// one that hits it in practice.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistReleaseGroups {
    pub albums: Vec<Album>,
    pub singles: Vec<Album>,
    pub compilations: Vec<Album>,
    pub appears_on: Vec<Album>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistReleaseTotals {
    pub albums: usize,
    pub singles: usize,
    pub compilations: usize,
    pub appears_on: usize,
}

impl From<ArtistReleaseCounts> for ArtistReleaseTotals {
    fn from(counts: ArtistReleaseCounts) -> Self {
        Self {
            albums: counts.albums,
            singles: counts.singles,
            compilations: counts.compilations,
            appears_on: counts.appears_on,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistCataloguePageDetail {
    pub releases: Vec<AlbumDetail>,
    pub total: usize,
    pub next_offset: Option<usize>,
}

impl From<ArtistCataloguePage> for ArtistCataloguePageDetail {
    fn from(page: ArtistCataloguePage) -> Self {
        Self {
            releases: page.releases.into_iter().map(AlbumDetail::from).collect(),
            total: page.total,
            next_offset: page.next_offset,
        }
    }
}

impl From<ArtistReleases> for ArtistReleaseGroups {
    fn from(releases: ArtistReleases) -> Self {
        let convert = |group: Vec<AlbumRef>| group.into_iter().map(Album::from).collect();
        Self {
            albums: convert(releases.albums),
            singles: convert(releases.singles),
            compilations: convert(releases.compilations),
            appears_on: convert(releases.appears_on),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistTopCityDetail {
    pub city: String,
    pub country: String,
    pub region: String,
    pub listeners: Option<u64>,
}

/// One editorial photograph with the dimensions the service reported for it.
/// The About gallery needs the shape of every picture before any of them has
/// loaded, so the size travels with the URL rather than being discovered on
/// load. Absent dimensions reach the page as null and are read there as a
/// neutral square.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ArtistImageDetail {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl From<ArtistImage> for ArtistImageDetail {
    fn from(image: ArtistImage) -> Self {
        Self {
            url: image.url,
            width: image.width,
            height: image.height,
        }
    }
}

impl From<ArtistTopCity> for ArtistTopCityDetail {
    fn from(city: ArtistTopCity) -> Self {
        Self {
            city: city.city,
            country: city.country,
            region: city.region,
            listeners: city.listeners,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistOverviewDetail {
    pub biography: Option<String>,
    pub header_image_url: Option<String>,
    /// Every editorial gallery picture, in the service's own order. The About
    /// section pages through them, and sizes its frame from all of them at once.
    pub biography_images: Vec<ArtistImageDetail>,
    pub popularity: Option<u32>,
    pub followers: Option<u64>,
    pub monthly_listeners: Option<u64>,
    pub world_rank: Option<u32>,
    pub top_cities: Vec<ArtistTopCityDetail>,
    pub popular_releases: Vec<Album>,
    pub related_artists: Vec<Artist>,
    pub discovered_on: Vec<Playlist>,
    pub artist_playlists: Vec<Playlist>,
    pub artist_pick: Option<ArtistPickDetail>,
}

/// The item pinned to the top of an artist's page, with the artist's note.
/// Mirrors the engine's `ArtistPick`; see it for why this is a tagged union
/// and not an optional playlist.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ArtistPickDetail {
    pub comment: Option<String>,
    pub item: ArtistPickItemDetail,
}

/// Reaches the frontend as `{ "kind": "track", "data": { … } }`, so one card
/// component can branch on `kind` to decide whether clicking it plays or opens.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ArtistPickItemDetail {
    Playlist(Playlist),
    Album(Album),
    Track(Track),
}

impl From<&ArtistPick> for ArtistPickDetail {
    fn from(pick: &ArtistPick) -> Self {
        Self {
            comment: pick.comment.clone(),
            item: match &pick.item {
                ArtistPickItem::Playlist(playlist) => {
                    ArtistPickItemDetail::Playlist(Playlist::from(playlist))
                }
                ArtistPickItem::Album(album) => {
                    ArtistPickItemDetail::Album(Album::from(album.clone()))
                }
                ArtistPickItem::Track(track) => ArtistPickItemDetail::Track(Track::from(track)),
            },
        }
    }
}

impl From<ArtistOverview> for ArtistOverviewDetail {
    fn from(overview: ArtistOverview) -> Self {
        Self {
            biography: overview.biography,
            header_image_url: overview.header_image_url,
            biography_images: overview
                .biography_images
                .into_iter()
                .map(ArtistImageDetail::from)
                .collect(),
            popularity: overview.popularity,
            followers: overview.followers,
            monthly_listeners: overview.monthly_listeners,
            world_rank: overview.world_rank,
            top_cities: overview
                .top_cities
                .into_iter()
                .map(ArtistTopCityDetail::from)
                .collect(),
            popular_releases: overview
                .popular_releases
                .into_iter()
                .map(Album::from)
                .collect(),
            related_artists: overview
                .related_artists
                .into_iter()
                .map(Artist::from)
                .collect(),
            discovered_on: overview.discovered_on.iter().map(Playlist::from).collect(),
            artist_playlists: overview
                .artist_playlists
                .iter()
                .map(Playlist::from)
                .collect(),
            artist_pick: overview.artist_pick.as_ref().map(ArtistPickDetail::from),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistDetail {
    #[serde(flatten)]
    pub artist: Artist,
    pub top_tracks: Vec<Track>,
    pub releases: ArtistReleaseGroups,
    pub release_counts: ArtistReleaseTotals,
    pub releases_next_offset: Option<usize>,
    pub overview: Option<ArtistOverviewDetail>,
}

impl From<ArtistBrowse> for ArtistDetail {
    fn from(browse: ArtistBrowse) -> Self {
        Self {
            artist: Artist {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                cover_url: browse.portrait_url.unwrap_or_default(),
            },
            top_tracks: browse.top_tracks.into_iter().map(Track::from).collect(),
            releases: browse.releases.into(),
            release_counts: browse.release_counts.into(),
            releases_next_offset: browse.releases_next_offset,
            overview: browse.overview.map(ArtistOverviewDetail::from),
        }
    }
}

/// Search result metadata for a podcast show.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Show {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub cover_url: String,
}

impl From<ShowRef> for Show {
    fn from(reference: ShowRef) -> Self {
        Self {
            id: reference.id,
            uri: reference.uri,
            name: reference.name,
            publisher: reference.publisher,
            description: reference.description,
            cover_url: reference.cover_url.unwrap_or_default(),
        }
    }
}

/// An episode retains its audio-only queue row for direct playback.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EpisodeDetail {
    pub show_id: String,
    pub show_name: String,
    pub description: String,
    pub published_at: Option<i64>,
    pub track: Track,
}

impl From<EpisodeRef> for EpisodeDetail {
    fn from(reference: EpisodeRef) -> Self {
        Self {
            show_id: reference.show_id,
            show_name: reference.show_name,
            description: reference.description,
            published_at: reference.published_at,
            track: Track::from(reference.track),
        }
    }
}

/// A show's metadata plus its source-ordered episodes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShowDetail {
    #[serde(flatten)]
    pub show: Show,
    pub episodes: Vec<EpisodeDetail>,
}

impl From<ShowBrowse> for ShowDetail {
    fn from(browse: ShowBrowse) -> Self {
        Self {
            show: Show {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                publisher: browse.publisher,
                description: browse.description,
                cover_url: browse.cover_url.unwrap_or_default(),
            },
            episodes: browse.episodes.into_iter().map(EpisodeDetail::from).collect(),
        }
    }
}

/// Spotify username is the navigation identity; name is only the display label.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProfileDetail {
    pub username: String,
    pub name: String,
    pub image_url: Option<String>,
    pub playlists: Vec<Playlist>,
}

impl From<UserProfile> for ProfileDetail {
    fn from(profile: UserProfile) -> Self {
        Self {
            username: profile.username,
            name: profile.name,
            image_url: profile.image_url,
            playlists: profile.playlists.iter().map(Playlist::from).collect(),
        }
    }
}

impl ProfileDetail {
    /// Fill only absent browse facts. Public-list order/identity/title and
    /// local activity are never replaced by the library's sorted snapshot.
    pub fn enrich_library_metadata(&mut self, library: &[Playlist]) {
        let by_id: std::collections::HashMap<&str, &Playlist> =
            library.iter().map(|playlist| (playlist.id.as_str(), playlist)).collect();
        for playlist in &mut self.playlists {
            let Some(known) = by_id.get(playlist.id.as_str()) else { continue };
            if playlist.cover_url.is_empty() {
                playlist.cover_url.clone_from(&known.cover_url);
            }
            if playlist.cover_urls.is_empty() {
                playlist.cover_urls.clone_from(&known.cover_urls);
            }
            if playlist.description.is_empty() {
                playlist.description.clone_from(&known.description);
            }
            if playlist.tracks_total == 0 {
                playlist.tracks_total = known.tracks_total;
            }
        }
    }

    /// Compact artwork facts for the engine's cache, never full tracks or
    /// activity timestamps. Rows remain in their public-profile source order.
    pub fn library_artwork_refs(library: &[Playlist]) -> Vec<PlaylistRef> {
        library.iter().filter(|playlist|
            !playlist.cover_url.is_empty() || !playlist.cover_urls.is_empty())
            .map(Playlist::artwork_ref).collect()
    }
}

/// The rootlist's folder order and hierarchy. A playlist is named by id: its
/// record lives once, in the library list. A playlist the tree does not name
/// sits at the top level.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LibraryNodeDetail {
    Folder {
        id: String,
        name: String,
        children: Vec<LibraryNodeDetail>,
        /// Unix seconds this app made the folder, the way a playlist's
        /// `last_activity` is stamped: what floats a new, still-empty folder
        /// to the top of an activity-ordered rail. Local to this app, carried
        /// across refreshes by id; Spotify has no such field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_activity: Option<i64>,
    },
    Playlist { id: String },
}

impl LibraryNodeDetail {
    /// Splits the engine's rootlist into its hierarchy and its playlists, the
    /// latter in rootlist order with the standard conversion applied.
    pub fn split(nodes: Vec<LibraryNode>, playlists: &mut Vec<Playlist>) -> Vec<Self> {
        nodes.into_iter().map(|node| match node {
            LibraryNode::Folder { id, name, children } => Self::Folder {
                id,
                name,
                children: Self::split(children, playlists),
                last_activity: None,
            },
            LibraryNode::Playlist { playlist } => {
                let playlist = Playlist::from(&playlist);
                let id = playlist.id.clone();
                playlists.push(playlist);
                Self::Playlist { id }
            }
        }).collect()
    }

    fn is(&self, item: &LibraryItem) -> bool {
        match (self, item) {
            (Self::Folder { id, .. }, LibraryItem::Folder { id: wanted })
            | (Self::Playlist { id }, LibraryItem::Playlist { id: wanted }) => id == wanted,
            _ => false,
        }
    }

    /// Takes the row out of the tree, wherever it is nested.
    pub fn take(tree: &mut Vec<Self>, item: &LibraryItem) -> Option<Self> {
        if let Some(index) = tree.iter().position(|node| node.is(item)) {
            return Some(tree.remove(index));
        }
        tree.iter_mut().find_map(|node| match node {
            Self::Folder { children, .. } => Self::take(children, item),
            Self::Playlist { .. } => None,
        })
    }

    pub fn folder_mut<'a>(tree: &'a mut [Self], wanted: &str) -> Option<&'a mut Self> {
        for node in tree {
            if matches!(node, Self::Folder { id, .. } if id == wanted) {
                return Some(node);
            }
            if let Self::Folder { children, .. } = node {
                if let Some(found) = Self::folder_mut(children, wanted) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Puts a row first in `folder`, or first at the top level.
    pub fn insert_first(tree: &mut Vec<Self>, folder: Option<&str>, node: Self) {
        match folder.and_then(|folder| Self::folder_mut(tree, folder)) {
            Some(Self::Folder { children, .. }) => children.insert(0, node),
            _ => tree.insert(0, node),
        }
    }

    fn holds(&self, item: &LibraryItem) -> bool {
        match self {
            Self::Folder { children, .. } => children.iter().any(|child| child.is(item) || child.holds(item)),
            Self::Playlist { .. } => false,
        }
    }

    /// Moves a row where the rootlist MOV that was just written puts it: to
    /// the top of `folder`, or — none — out of every folder to just after the
    /// top-level folder it was in. A playlist the tree did not name was at
    /// the top level already; a folder it does not name is left alone.
    pub fn move_to(tree: &mut Vec<Self>, item: &LibraryItem, folder: Option<&str>) {
        let outermost = match folder {
            Some(_) => None,
            None => tree.iter().position(|node| node.holds(item)),
        };
        let node = match Self::take(tree, item) {
            Some(node) => node,
            None => match item {
                LibraryItem::Playlist { id } => Self::Playlist { id: id.clone() },
                LibraryItem::Folder { .. } => return,
            },
        };
        match outermost {
            Some(index) => tree.insert(index + 1, node),
            None => Self::insert_first(tree, folder, node),
        }
    }

    /// Every playlist id at or under this row.
    pub fn playlist_ids(&self, out: &mut Vec<String>) {
        match self {
            Self::Playlist { id } => out.push(id.clone()),
            Self::Folder { children, .. } => {
                for child in children {
                    child.playlist_ids(out);
                }
            }
        }
    }

    /// Copies the local folder stamps of `previous` onto the same folders in
    /// a freshly read tree; the rootlist knows nothing about them.
    pub fn carry_folder_activity(previous: &[Self], fresh: &mut [Self]) {
        fn stamps(nodes: &[LibraryNodeDetail], out: &mut std::collections::HashMap<String, i64>) {
            for node in nodes {
                if let LibraryNodeDetail::Folder { id, children, last_activity, .. } = node {
                    if let Some(at) = last_activity {
                        out.insert(id.clone(), *at);
                    }
                    stamps(children, out);
                }
            }
        }
        fn apply(nodes: &mut [LibraryNodeDetail], stamps: &std::collections::HashMap<String, i64>) {
            for node in nodes {
                if let LibraryNodeDetail::Folder { id, children, last_activity, .. } = node {
                    if last_activity.is_none() {
                        *last_activity = stamps.get(id.as_str()).copied();
                    }
                    apply(children, stamps);
                }
            }
        }
        let mut known = std::collections::HashMap::new();
        stamps(previous, &mut known);
        if !known.is_empty() {
            apply(fresh, &known);
        }
    }
}

/// The one best answer to a query, of whatever kind. Serialized internally
/// tagged, so the frontend reads `top.kind` and then the entity's own fields
/// off the same object.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SearchTop {
    Track(Track),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
    Show(Show),
    Episode { uri: String },
}

impl From<SearchTopRef> for SearchTop {
    fn from(top: SearchTopRef) -> Self {
        match top {
            SearchTopRef::Track(reference) => Self::Track(Track::from(reference)),
            SearchTopRef::Album(reference) => Self::Album(Album::from(reference)),
            SearchTopRef::Artist(reference) => Self::Artist(Artist::from(reference)),
            SearchTopRef::Playlist(reference) => Self::Playlist(Playlist::from(&reference)),
            SearchTopRef::Show(reference) => Self::Show(Show::from(reference)),
            SearchTopRef::Episode { uri } => Self::Episode { uri },
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<SearchTop>,
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    #[serde(default)]
    pub playlists: Vec<Playlist>,
    pub shows: Vec<Show>,
    pub episodes: Vec<EpisodeDetail>,
}

impl From<SearchBrowse> for SearchResult {
    fn from(browse: SearchBrowse) -> Self {
        Self {
            top: browse.top.map(SearchTop::from),
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
            albums: browse.albums.into_iter().map(Album::from).collect(),
            artists: browse.artists.into_iter().map(Artist::from).collect(),
            playlists: browse.playlists.iter().map(Playlist::from).collect(),
            shows: browse.shows.into_iter().map(Show::from).collect(),
            episodes: browse.episodes.into_iter().map(EpisodeDetail::from).collect(),
        }
    }
}

/// One contributor in a track's credits.
///
/// `id` is the validated Spotify artist id from the source credit URI. The
/// frontend uses it only to open the external
/// `https://artists.spotify.com/songwriter/{id}` page; it must never route to
/// the in-app artist view. An empty id means the contributor remains plain
/// text. `subroles` are the service's own labels (`composer`, `lyricist`,
/// `producer`, `main artist`, ...), possibly empty.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Contributor {
    pub id: String,
    pub uri: String,
    /// External page for this contributor, supplied finished by the service.
    /// The frontend opens it verbatim and never builds one; empty means the
    /// name renders as plain text.
    pub url: String,
    pub name: String,
    /// The service's own labels, kept verbatim for truthful per-person detail.
    pub subroles: Vec<String>,
}

impl From<CreditArtist> for Contributor {
    fn from(artist: CreditArtist) -> Self {
        Self {
            id: artist.id,
            uri: artist.uri,
            url: artist.url,
            name: artist.name,
            subroles: artist.subroles,
        }
    }
}
/// One source-provided role group of a track's credits, e.g. Artist or
/// Composition & Lyrics. The title is passed through exactly as the service
/// called it; an unrecognised group is still worth showing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CreditGroup {
    pub title: String,
    pub contributors: Vec<Contributor>,
}

impl From<CreditRole> for CreditGroup {
    fn from(role: CreditRole) -> Self {
        Self {
            title: role.title,
            contributors: role.artists.into_iter().map(Contributor::from).collect(),
        }
    }
}

/// Songwriter/producer/performer credits for one track, fetched on demand.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TrackCreditsDetail {
    pub track_uri: String,
    pub track_name: String,
    pub groups: Vec<CreditGroup>,
    /// The licensor, shown under the groups exactly as the official client
    /// does. Empty when the service does not supply one.
    pub source: String,
}

impl From<TrackCredits> for TrackCreditsDetail {
    fn from(credits: TrackCredits) -> Self {
        Self {
            track_uri: credits.track_uri,
            track_name: credits.track_name,
            groups: credits.roles.into_iter().map(CreditGroup::from).collect(),
            source: credits.source,
        }
    }
}

/// Deserializes a nullable/missing string field into an owned `String`
/// (the engine serializes some optional state fields as `null`).
fn string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn cache_ids_or_none<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if !value.is_array() {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some).map_err(serde::de::Error::custom)
}

/// Mirror of the engine's `state` line, with a locally projected playhead
/// between its scalar transport updates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaybackState {
    pub ready: bool,
    /// One of `authenticating`, `needs_login`, `ready`, `error`.
    pub auth_state: String,
    #[serde(deserialize_with = "string_or_default")]
    pub auth_url: String,
    pub playing: bool,
    pub buffering: bool,
    /// Empty for local audio; remote output is selected only for this app session.
    pub output_device_id: String,
    pub output_device_name: String,
    /// True only while the engine queue is an editor draft preview. Preview
    /// state is forwarded to the window but excluded from durable restore.
    pub preview: bool,
    #[serde(deserialize_with = "string_or_default")]
    pub username: String,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    /// One of `off`, `context`, `track`.
    pub repeat: String,
    /// The current item's speed: its kind's, of the two below.
    pub playback_speed: f32,
    /// Remembered apart for songs and podcast episodes, and persisted.
    pub track_speed: f32,
    pub episode_speed: f32,
    pub audible_playback_speed: f32,
    pub current_index: Option<usize>,
    #[serde(deserialize_with = "string_or_default")]
    pub current_uri: String,
    pub context: String,
    /// Shared: the delta base, the retained state, every line subscriber and
    /// `AppState` each hold the same rows, so passing a state on copies a
    /// pointer. Writers replace the rows, or copy on write (`Arc::make_mut`).
    pub queue: Arc<[Track]>,
    /// Generation of the canonical queue rows. Omission retains those rows;
    /// an explicit empty array clears them. Zero means an unnamed snapshot.
    #[serde(default)]
    pub queue_revision: u64,
    /// Generation of the upcoming order, independent of row edits.
    #[serde(default)]
    pub order_revision: u64,
    /// Queue indexes in the order automatic playback will actually reach them,
    /// current row excluded. Derived by the engine from shuffle's live bag or
    /// the sequential walk, with exclusions and unavailable rows removed, so it
    /// is never restored or persisted — only forwarded. It may be omitted
    /// independently of `queue` when `order_revision` is unchanged.
    pub upcoming: Vec<usize>,
    /// Additive cache marks, independent of queue/order revisions. Omission
    /// retains known marks; these IDs can refer to visible off-queue rows.
    #[serde(deserialize_with = "cache_ids_or_none", skip_serializing_if = "Option::is_none")]
    pub cached_ids: Option<Vec<String>>,
    #[serde(deserialize_with = "string_or_default")]
    pub error: String,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            ready: false,
            auth_state: "needs_login".to_owned(),
            auth_url: String::new(),
            playing: false,
            buffering: false,
            output_device_id: String::new(),
            output_device_name: String::new(),
            preview: false,
            username: String::new(),
            position_ms: 0,
            duration_ms: 0,
            // The engine starts at 50% volume; mirror it so the first paint
            // matches the first heartbeat.
            volume: 50,
            shuffle: false,
            repeat: "off".to_owned(),
            playback_speed: 1.0,
            track_speed: 1.0,
            episode_speed: 1.0,
            audible_playback_speed: 1.0,
            current_index: None,
            current_uri: String::new(),
            context: String::new(),
            queue: Arc::default(),
            queue_revision: 0,
            order_revision: 0,
            upcoming: Vec::new(),
            cached_ids: None,
            error: String::new(),
        }
    }
}

/// Borrowed event projection: do not clone or serialize unchanged queue rows.
#[derive(Clone, Copy)]
pub struct PlaybackEvent<'a> {
    state: &'a PlaybackState,
    include_queue: bool,
    include_order: bool,
}

impl<'a> PlaybackEvent<'a> {
    pub fn new(state: &'a PlaybackState, include_queue: bool, include_order: bool) -> Self {
        Self { state, include_queue, include_order }
    }
}

impl Serialize for PlaybackEvent<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        let state = self.state;
        macro_rules! fields {
            ($($field:ident),* $(,)?) => {
                $(map.serialize_entry(stringify!($field), &state.$field)?;)*
            };
        }
        fields!(ready, auth_state, auth_url, playing, buffering, output_device_id,
            output_device_name, preview, username, position_ms, duration_ms, volume,
            shuffle, repeat, playback_speed, audible_playback_speed, current_index,
            current_uri, context, queue_revision, order_revision, error);
        if self.include_queue {
            map.serialize_entry("queue", &state.queue)?;
        }
        if self.include_order {
            map.serialize_entry("upcoming", &state.upcoming)?;
        }
        if let Some(ids) = &state.cached_ids {
            map.serialize_entry("cached_ids", ids)?;
        }
        map.end()
    }
}

/// Full snapshot served to the frontend for the initial render.
#[derive(Clone, Debug, Serialize)]
pub struct AppState {
    pub playback: PlaybackState,
    pub playlists: Vec<Playlist>,
    pub playlist_tree: Vec<LibraryNodeDetail>,
    pub settings: crate::app::AppSettings,
    pub library_fresh: bool,
    pub me_id: String,
}

/// What one cache directory is costing on disk.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheUsage {
    pub files: u64,
    pub bytes: u64,
}

/// Disk usage of both on-disk caches, for the Settings page.
///
/// `audio` is librespot's own audio cache under the engine state dir — the
/// songs kept for offline replay — and `covers` is the artwork the shell
/// downloads for `cover://`. They are reported separately because they are
/// bounded separately: the audio cache has a 1 GiB ceiling enforced by
/// librespot, the cover cache has none.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheStats {
    pub audio: CacheUsage,
    pub covers: CacheUsage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_events_preserve_plan_and_current_context_without_queue_rows() {
        let state = PlaybackState { queue: vec![Track { id: "song".into(), ..Track::default() }].into(),
            current_index: Some(0), context: "playlist:p".into(), queue_revision: 17,
            upcoming: vec![3, 2], playing: true, ..PlaybackState::default() };
        let compact = serde_json::to_value(PlaybackEvent::new(&state, false, true)).unwrap();
        assert!(compact.get("queue").is_none());
        assert_eq!(compact["queue_revision"], 17);
        assert_eq!(compact["context"], "playlist:p");
        assert_eq!(compact["upcoming"], serde_json::json!([3, 2]));
        assert_eq!(compact["current_index"], 0);
        let full = serde_json::to_value(PlaybackEvent::new(&state, true, true)).unwrap();
        assert_eq!(full["queue"][0]["id"], "song");
        let empty = PlaybackState { queue_revision: 18, ..PlaybackState::default() };
        let cleared = serde_json::to_value(PlaybackEvent::new(&empty, true, true)).unwrap();
        assert_eq!(cleared["queue"], serde_json::json!([]));
    }

    fn track(cover_url: &str) -> Track {
        Track {
            cover_url: cover_url.to_owned(),
            ..Track::default()
        }
    }

    fn browse(revision: &str, covers: &[&str]) -> PlaylistBrowse {
        PlaylistBrowse {
            id: "p1".to_owned(),
            uri: "spotify:playlist:p1".to_owned(),
            name: "Mixtape".to_owned(),
            description: Some("<p>Road&nbsp;music</p>".to_owned()),
            revision: Some(revision.to_owned()),
            owner_id: "me".to_owned(),
            owner_name: String::new(),
            cover_url: None,
            tracks: covers.iter().map(|url| track(url).into()).collect(),
            excluded_track_ids: Vec::new(),
        }
    }

    #[test]
    fn artwork_hydration_preserves_existing_covers_and_local_ordering_facts() {
        let metadata = PlaylistRef {
            id: "p1".into(), uri: "spotify:playlist:p1".into(), name: "Old title".into(),
            revision: None,
            description: None, owner_id: "owner".into(), owner_name: String::new(),
            cover_url: Some("remote-custom".into()), cover_urls: vec!["sample".into()],
            track_count: Some(400),
        };
        let mut playlist = Playlist {
            id: "p1".into(), name: "Renamed".into(), cover_url: "current-custom".into(),
            cover_urls: vec!["current-mosaic".into()], tracks_total: 9,
            last_activity: Some(800), last_played: Some(700), snapshot_id: "new".into(),
            ..Playlist::default()
        };
        let before = playlist.clone();
        assert!(!playlist.fill_artwork(&metadata));
        assert_eq!(playlist, before);
        playlist.cover_url.clear();
        playlist.cover_urls.clear();
        assert!(playlist.fill_artwork(&metadata));
        assert_eq!(playlist.cover_url, "remote-custom");
        assert_eq!(playlist.cover_urls, vec!["sample"]);
        assert_eq!(playlist.name, "Renamed");
        assert_eq!(playlist.tracks_total, 9);
        assert_eq!(playlist.last_activity, Some(800));
        assert_eq!(playlist.last_played, Some(700));
        assert_eq!(playlist.snapshot_id, "new");
    }

    #[test]
    fn show_and_episode_detail_keep_playback_and_availability_fields() {
        let episode = EpisodeRef {
            show_id: "show".into(),
            show_name: "The Show".into(),
            description: "Audio episode".into(),
            published_at: Some(1_726_000_000),
            track: TrackRef {
                id: "ep".into(),
                uri: "spotify:episode:ep".into(),
                context: "show:show".into(),
                name: "Interview".into(),
                cover_url: "episode-cover".into(),
                duration_ms: 83_000,
                unavailable: true,
                unavailable_reason: Some("Region restricted".into()),
                ..TrackRef::default()
            },
        };
        let show = ShowDetail::from(ShowBrowse {
            id: "show".into(),
            uri: "spotify:show:show".into(),
            name: "The Show".into(),
            publisher: "Publisher".into(),
            description: "About the show".into(),
            cover_url: Some("show-cover".into()),
            episodes: vec![episode],
        });
        let json = serde_json::to_value(show).unwrap();
        assert_eq!(json["name"], "The Show");
        assert_eq!(json["publisher"], "Publisher");
        assert_eq!(json["cover_url"], "show-cover");
        assert_eq!(json["episodes"][0]["track"]["uri"], "spotify:episode:ep");
        assert_eq!(json["episodes"][0]["show_name"], "The Show");
        assert_eq!(json["episodes"][0]["published_at"], 1_726_000_000_i64);
        assert_eq!(json["episodes"][0]["track"]["duration_ms"], 83_000);
        assert_eq!(json["episodes"][0]["track"]["unavailable"], true);
        assert_eq!(json["episodes"][0]["track"]["unavailable_reason"], "Region restricted");
        assert_eq!(json["episodes"][0]["track"]["context"], "show:show");
    }

    #[test]
    fn profile_library_enrichment_fills_mosaics_without_sorting_or_activity_mutation() {
        let mut profile = ProfileDetail {
            playlists: vec![
                Playlist { id: "b".into(), name: "Public B".into(), owner: "Owner B".into(),
                    cover_url: "fresh-cover".into(), ..Playlist::default() },
                Playlist { id: "a".into(), name: "Public A".into(), owner: "Owner A".into(),
                    ..Playlist::default() },
            ],
            ..ProfileDetail::default()
        };
        let library = vec![
            Playlist { id: "a".into(), name: "Old A".into(), owner: "Wrong owner".into(),
                cover_urls: vec!["album-one".into(), "album-two".into()],
                description: "Known description".into(), tracks_total: 77,
                last_activity: Some(999), last_played: Some(888), ..Playlist::default() },
            Playlist { id: "b".into(), cover_url: "old-cover".into(), ..Playlist::default() },
        ];
        let unchanged = library.clone();
        profile.enrich_library_metadata(&library);
        assert_eq!(profile.playlists.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(profile.playlists[0].cover_url, "fresh-cover");
        assert_eq!(profile.playlists[1].cover_urls, ["album-one", "album-two"]);
        assert_eq!(profile.playlists[1].description, "Known description");
        assert_eq!(profile.playlists[1].tracks_total, 77);
        assert_eq!(profile.playlists[1].name, "Public A");
        assert_eq!(profile.playlists[1].owner, "Owner A");
        assert!(profile.playlists[1].last_activity.is_none());
        assert!(profile.playlists[1].last_played.is_none());
        assert_eq!(library, unchanged);
    }

    #[test]
    fn profile_and_nested_rootlist_keep_playlist_metadata_and_order() {
        let playlist = PlaylistRef {
            id: "mix".into(),
            uri: "spotify:playlist:mix".into(),
            name: "Mixtape".into(),
            revision: None,
            description: Some("<p>Mixed&nbsp;tapes</p>".into()),
            owner_id: "dj".into(),
            owner_name: "DJ Name".into(),
            cover_url: Some("cover".into()),
            cover_urls: Vec::new(),
            track_count: Some(12),
        };
        let profile = ProfileDetail::from(UserProfile {
            username: "dj".into(),
            name: "DJ Name".into(),
            image_url: Some("portrait".into()),
            playlists: vec![playlist.clone()],
        });
        assert_eq!(profile.username, "dj");
        assert_eq!(profile.name, "DJ Name");
        assert_eq!(profile.image_url.as_deref(), Some("portrait"));
        assert_eq!(profile.playlists[0].owner, "DJ Name");
        assert_eq!(profile.playlists[0].description, "Mixed tapes");
        assert_eq!(profile.playlists[0].cover_url, "cover");

        let mut playlists = Vec::new();
        let tree = LibraryNodeDetail::split(vec![LibraryNode::Folder {
            id: "parent".into(),
            name: "Archive".into(),
            children: vec![LibraryNode::Folder {
                id: "child".into(),
                name: "2026".into(),
                children: vec![LibraryNode::Playlist { playlist }],
            }],
        }], &mut playlists);
        let json = serde_json::to_value(&tree[0]).unwrap();
        assert_eq!(json["kind"], "folder");
        assert_eq!(json["children"][0]["kind"], "folder");
        assert_eq!(json["children"][0]["children"][0], serde_json::json!({"kind": "playlist", "id": "mix"}));
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].owner, "DJ Name");
        assert_eq!(playlists[0].tracks_total, 12);
    }

    #[test]
    fn library_tree_edits_move_take_and_carry_folder_stamps() {
        let playlist = |id: &str| LibraryNodeDetail::Playlist { id: id.into() };
        let folder = |id: &str, children, last_activity| LibraryNodeDetail::Folder {
            id: id.into(), name: id.to_uppercase(), children, last_activity,
        };
        let mut tree = vec![
            playlist("a"),
            folder("outer", vec![playlist("b"), folder("inner", vec![playlist("c")], None)], Some(5)),
        ];
        // Into a nested folder, first.
        LibraryNodeDetail::move_to(&mut tree, &LibraryItem::Playlist { id: "a".into() }, Some("inner"));
        assert_eq!(tree, vec![folder("outer", vec![playlist("b"), folder("inner", vec![playlist("a"), playlist("c")], None)], Some(5))]);
        // A folder out of its parent, contents and all: just after the
        // top-level folder it was in, where the rootlist MOV puts it.
        LibraryNodeDetail::move_to(&mut tree, &LibraryItem::Folder { id: "inner".into() }, None);
        assert_eq!(tree[0], folder("outer", vec![playlist("b")], Some(5)));
        assert_eq!(tree[1], folder("inner", vec![playlist("a"), playlist("c")], None));
        // A playlist the tree never named was at the top level already.
        LibraryNodeDetail::move_to(&mut tree, &LibraryItem::Playlist { id: "z".into() }, Some("outer"));
        assert_eq!(tree[0], folder("outer", vec![playlist("z"), playlist("b")], Some(5)));
        // An unknown folder is not invented.
        LibraryNodeDetail::move_to(&mut tree, &LibraryItem::Folder { id: "ghost".into() }, None);
        assert_eq!(tree.len(), 2);
        tree.swap(0, 1);

        let taken = LibraryNodeDetail::take(&mut tree, &LibraryItem::Folder { id: "outer".into() }).unwrap();
        let mut ids = Vec::new();
        taken.playlist_ids(&mut ids);
        assert_eq!(ids, vec!["z", "b"]);
        assert_eq!(tree.len(), 1);
        assert!(LibraryNodeDetail::take(&mut tree, &LibraryItem::Folder { id: "outer".into() }).is_none());

        LibraryNodeDetail::insert_first(&mut tree, Some("inner"), folder("new", Vec::new(), Some(9)));
        assert!(matches!(&tree[0], LibraryNodeDetail::Folder { children, .. } if children[0] == folder("new", Vec::new(), Some(9))));

        // A refresh answers without stamps; the folders this app made keep theirs.
        let mut fresh = vec![folder("inner", vec![folder("new", Vec::new(), None)], None)];
        LibraryNodeDetail::carry_folder_activity(&tree, &mut fresh);
        assert_eq!(fresh, vec![folder("inner", vec![folder("new", Vec::new(), Some(9))], None)]);
        // The stamp is local and travels only when there is one.
        let json = serde_json::to_value(&fresh[0]).unwrap();
        assert!(json.get("last_activity").is_none());
        assert_eq!(json["children"][0]["last_activity"], 9);
        let old: LibraryNodeDetail = serde_json::from_value(serde_json::json!({"kind": "folder", "id": "x", "name": "X", "children": []})).unwrap();
        assert_eq!(old, folder("x", Vec::new(), None));
    }

    #[test]
    fn search_keeps_podcast_groups_and_ranked_top() {
        let show = ShowRef {
            id: "show".into(),
            name: "The Show".into(),
            publisher: "Publisher".into(),
            ..ShowRef::default()
        };
        let episode = EpisodeRef {
            show_id: "show".into(),
            track: TrackRef {
                id: "ep".into(),
                name: "Interview".into(),
                uri: "spotify:episode:ep".into(),
                ..TrackRef::default()
            },
            ..EpisodeRef::default()
        };
        let results = SearchResult::from(SearchBrowse {
            top: Some(SearchTopRef::Episode { uri: episode.track.uri.clone() }),
            shows: vec![show.clone()],
            episodes: vec![episode],
            ..SearchBrowse::default()
        });
        let json = serde_json::to_value(results).unwrap();
        assert_eq!(json["top"]["kind"], "episode");
        assert_eq!(json["top"]["uri"], "spotify:episode:ep");
        assert_eq!(json["shows"][0]["publisher"], "Publisher");
        assert_eq!(json["episodes"][0]["track"]["name"], "Interview");
        let top_show = serde_json::to_value(SearchTop::from(SearchTopRef::Show(show))).unwrap();
        assert_eq!(top_show["kind"], "show");
        assert_eq!(top_show["name"], "The Show");
    }

    #[test]
    fn history_item_conversion_preserves_the_flat_snapshot_contract() {
        let item = EngineHistoryItem {
            row: renderer_engine::protocol::HistoryRow {
                track_id: "track-1".into(),
                started_at: 1_725_000_123_456,
                ms_played: 91_000,
                completed: true,
                context: "playlist:mix".into(),
            },
            track: TrackRef {
                id: "track-1".into(),
                uri: "spotify:track:track-1".into(),
                name: "Song".into(),
                artist_names: vec!["Artist".into()],
                ..TrackRef::default()
            },
        };

        let entry = HistoryEntry::from(item);
        assert_eq!(entry.track_id, "track-1");
        assert_eq!(entry.started_at, 1_725_000_123_456);
        assert_eq!(entry.ms_played, 91_000);
        assert!(entry.completed);
        assert_eq!(entry.context, "playlist:mix");
        assert_eq!(entry.track.name, "Song");
    }

    /// The page's counts are what the view sizes its scrollbar and its
    /// "showing N of M" line from, so they have to survive the crossing
    /// distinct from each other and from the number of rows carried.
    #[test]
    fn history_page_conversion_keeps_the_window_and_both_counts() {
        let item = |id: &str| EngineHistoryItem {
            row: renderer_engine::protocol::HistoryRow {
                track_id: id.to_owned(),
                started_at: 1_725_000_000_000,
                ms_played: 91_000,
                completed: true,
                context: String::new(),
            },
            track: TrackRef {
                id: id.to_owned(),
                name: format!("Song {id}"),
                ..TrackRef::default()
            },
        };
        let detail = HistoryPageDetail::from(EngineHistoryPage {
            items: vec![item("a"), item("b")],
            total: 340,
            recorded: 1_204,
            offset: 200,
            next_offset: Some(202),
        });
        assert_eq!(detail.entries.len(), 2);
        assert_eq!(detail.entries[1].track.name, "Song b");
        assert_eq!(detail.total, 340, "the filtered list being paged");
        assert_eq!(detail.recorded, 1_204, "the archive behind it");
        assert_eq!(detail.offset, 200);
        assert_eq!(detail.next_offset, Some(202));
    }

    #[test]
    fn cover_candidates_take_the_first_four_distinct_covers() {
        let covers = ["a", "a", "b", "c", "b", "d", "e"];
        let tracks: Vec<Track> = covers.iter().map(|url| track(url)).collect();
        assert_eq!(cover_urls_from_tracks(&tracks), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn cover_candidates_skip_tracks_without_a_cover() {
        let tracks: Vec<Track> = ["", "a", "", "b"].iter().map(|url| track(url)).collect();
        assert_eq!(cover_urls_from_tracks(&tracks), vec!["a", "b"]);
        // Fewer than four distinct covers is normal (short playlists, one
        // album); the frontend renders whatever it is given.
        assert!(cover_urls_from_tracks(&[]).is_empty());
        assert!(cover_urls_from_tracks(&[track("")]).is_empty());
    }

    #[test]
    fn browsing_derives_the_candidates_alongside_the_revision() {
        let detail = PlaylistDetail::from(browse("rev-a", &["a", "b"]));
        assert_eq!(detail.playlist.description, "Road music");
        assert_eq!(detail.playlist.cover_urls, vec!["a", "b"]);
        assert_eq!(detail.playlist.snapshot_id, "rev-a");
        assert_eq!(detail.playlist.tracks_total, 2);
        let mut excluded = browse("rev-excluded", &["a", "b"]);
        excluded.excluded_track_ids = vec!["track-a".into(), "track-b".into()];
        let detail = PlaylistDetail::from(excluded);
        assert_eq!(
            detail.excluded_track_ids,
            vec!["track-a".to_owned(), "track-b".to_owned()]
        );

        let legacy: PlaylistDetail = serde_json::from_str(
            r#"{"id":"p1","uri":"spotify:playlist:p1","name":"Mixtape","tracks":[]}"#,
        )
        .expect("detail caches without exclusions remain readable");
        assert!(legacy.excluded_track_ids.is_empty());

        // A revision bump can replace exactly the tracks the old candidates
        // came from, so a browse never reuses them.
        let next = PlaylistDetail::from(browse("rev-b", &["c", "d"]));
        assert_eq!(next.playlist.cover_urls, vec!["c", "d"]);
        assert_eq!(next.playlist.snapshot_id, "rev-b");
    }

    #[test]
    fn artist_ids_stay_parallel_to_artist_names() {
        // A track cached before the field existed: names but no ids.
        let mut legacy: Track = serde_json::from_str(
            r#"{"id":"t1","name":"Track","artist_names":["A","B","C"],"artist_id":"a1"}"#,
        )
        .unwrap();
        assert!(legacy.artist_ids.is_empty(), "old caches carry no ids");
        align_artist_ids(&mut legacy);
        assert_eq!(
            legacy.artist_ids,
            vec!["", "", ""],
            "padded so the two lists zip, with every name unlinkable"
        );
        assert_eq!(legacy.artist_id, "a1", "the primary is untouched");

        // A track from the engine already matches and is left alone.
        let mut fresh = Track {
            artist_names: vec!["A".into(), "B".into()],
            artist_ids: vec!["a1".into(), "b1".into()],
            ..Track::default()
        };
        align_artist_ids(&mut fresh);
        assert_eq!(fresh.artist_ids, vec!["a1", "b1"]);

        // Excess ids (a hand-edited or future-written cache) are trimmed
        // rather than left to mispair with the names.
        let mut extra = Track {
            artist_names: vec!["A".into()],
            artist_ids: vec!["a1".into(), "stale".into()],
            ..Track::default()
        };
        align_artist_ids(&mut extra);
        assert_eq!(extra.artist_ids, vec!["a1"]);
    }

    #[test]
    fn artist_ids_survive_the_round_trip_through_the_engine_type() {
        let track = Track {
            id: "t1".into(),
            artist_names: vec!["A".into(), "B".into()],
            artist_ids: vec!["a1".into(), "b1".into()],
            artist_id: "a1".into(),
            added_at: Some(1_725_000_123_456),
            ..Track::default()
        };
        let reference: TrackRef = track.clone().into();
        assert_eq!(reference.artist_ids, vec!["a1", "b1"]);
        assert_eq!(reference.added_at, Some(1_725_000_123_456));
        assert_eq!(Track::from(&reference), track);
        assert_eq!(Track::from(reference), track);
    }

    #[test]
    fn a_missing_cover_urls_field_deserializes_to_no_candidates() {
        // Caches written before the field existed must still load.
        let playlist: Playlist =
            serde_json::from_str(r#"{"id":"p1","name":"Mixtape","cover_url":""}"#).unwrap();
        assert_eq!(playlist.id, "p1");
        assert!(playlist.cover_urls.is_empty());
    }

    #[test]
    fn search_result_maps_playlist_references_and_defaults_legacy_payloads() {
        let result = SearchResult::from(SearchBrowse {
            playlists: vec![PlaylistRef {
                id: "0123456789ABCDEFGHIJKL".into(),
                uri: "spotify:playlist:0123456789ABCDEFGHIJKL".into(),
                name: "Public Mix".into(),
                revision: None,
                description: Some("A public playlist.".into()),
                owner_id: "alice".into(),
                owner_name: "Alice Example".into(),
                cover_url: Some("https://i.scdn.co/image/cover".into()),
                cover_urls: Vec::new(),
                track_count: Some(42),
            }],
            ..SearchBrowse::default()
        });
        assert_eq!(
            result.playlists,
            vec![Playlist {
                id: "0123456789ABCDEFGHIJKL".into(),
                uri: "spotify:playlist:0123456789ABCDEFGHIJKL".into(),
                name: "Public Mix".into(),
                description: "A public playlist.".into(),
                owner: "Alice Example".into(),
                owner_id: "alice".into(),
                cover_url: "https://i.scdn.co/image/cover".into(),
                tracks_total: 42,
                ..Playlist::default()
            }]
        );

        let legacy: SearchResult =
            serde_json::from_str(r#"{"tracks":[],"albums":[],"artists":[]}"#).unwrap();
        assert!(legacy.playlists.is_empty());
    }

    #[test]
    fn playlist_conversion_does_not_decode_canonical_entities_twice() {
        let reference = PlaylistRef {
            id: "p1".into(),
            uri: "spotify:playlist:p1".into(),
            name: "Literal tags".into(),
            revision: None,
            description: Some("&lt;b&gt;".into()),
            owner_id: String::new(),
            owner_name: String::new(),
            cover_url: None,
            cover_urls: Vec::new(),
            track_count: None,
        };
        assert_eq!(Playlist::from(&reference).description, "&lt;b&gt;");
    }

    #[test]
    fn playlist_conversion_preserves_canonical_comparison_brackets() {
        let reference = PlaylistRef {
            id: "p1".into(),
            uri: "spotify:playlist:p1".into(),
            name: "Short songs".into(),
            revision: None,
            description: Some("Songs under < 3 min > classics".into()),
            owner_id: String::new(),
            owner_name: String::new(),
            cover_url: None,
            cover_urls: Vec::new(),
            track_count: None,
        };
        assert_eq!(
            Playlist::from(&reference).description,
            "Songs under < 3 min > classics"
        );
    }

    #[test]
    fn followed_artists_carry_their_portraits_to_the_frontend() {
        let artists: Vec<Artist> = vec![
            ArtistRef {
                id: "ar1".into(),
                uri: "spotify:artist:ar1".into(),
                name: "Artist One".into(),
                portrait_url: Some("portrait".into()),
            },
            ArtistRef {
                id: "ar2".into(),
                uri: "spotify:artist:ar2".into(),
                name: "Artist Two".into(),
                portrait_url: None,
            },
        ]
        .into_iter()
        .map(Artist::from)
        .collect();
        assert_eq!(artists[0].cover_url, "portrait");
        // The frontend `Artist` has no optional cover; a missing portrait is
        // the empty string the identity tile already stands in for.
        assert_eq!(artists[1].cover_url, "");
    }

    #[test]
    fn artist_overview_survives_the_engine_to_frontend_conversion() {
        let overview = ArtistOverview {
            biography: Some("Biography".into()),
            header_image_url: Some("header".into()),
            biography_images: vec![
                ArtistImage {
                    url: "biography-image".into(),
                    width: Some(1600),
                    height: Some(1072),
                },
                ArtistImage {
                    url: "biography-image-2".into(),
                    ..ArtistImage::default()
                },
            ],
            popularity: Some(77),
            followers: Some(1_200),
            monthly_listeners: Some(3_400),
            world_rank: Some(42),
            top_cities: vec![ArtistTopCity {
                city: "London".into(),
                country: "GB".into(),
                region: "England".into(),
                listeners: Some(900),
            }],
            popular_releases: vec![AlbumRef {
                id: "album".into(),
                uri: "spotify:album:album".into(),
                name: "Popular".into(),
                artist_names: vec!["Artist".into()],
                artist_ids: vec!["artist".into()],
                cover_url: Some("cover".into()),
                year: Some(2024),
            }],
            related_artists: vec![ArtistRef {
                id: "related".into(),
                uri: "spotify:artist:related".into(),
                name: "Related".into(),
                portrait_url: Some("portrait".into()),
            }],
            discovered_on: Vec::new(),
            artist_playlists: Vec::new(),
            artist_pick: None,
        };

        let frontend = ArtistOverviewDetail::from(overview);
        assert_eq!(frontend.biography.as_deref(), Some("Biography"));
        assert_eq!(frontend.header_image_url.as_deref(), Some("header"));
        // Dimensions cross the boundary with the URL, and a picture the service
        // did not measure crosses as unmeasured — the page reads that as a
        // neutral shape rather than as a photograph one pixel square.
        assert_eq!(
            frontend.biography_images,
            vec![
                ArtistImageDetail {
                    url: "biography-image".to_owned(),
                    width: Some(1600),
                    height: Some(1072),
                },
                ArtistImageDetail {
                    url: "biography-image-2".to_owned(),
                    width: None,
                    height: None,
                },
            ]
        );
        assert_eq!(frontend.top_cities[0].listeners, Some(900));
        assert_eq!(frontend.popular_releases[0].artist_ids, vec!["artist"]);
        assert_eq!(frontend.related_artists[0].cover_url, "portrait");
        let json = serde_json::to_value(frontend).unwrap();
        assert_eq!(json["monthly_listeners"], 3_400);
        assert_eq!(json["popular_releases"][0]["name"], "Popular");
        assert_eq!(json["related_artists"][0]["name"], "Related");
    }
    #[test]
    fn songwriter_playlist_defaults_when_missing_from_standalone_payload() {
        let empty: SongwriterPlaylist = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.playlist, Playlist::default());
        assert!(empty.tracks.is_empty());
    }

    #[test]
    fn songwriter_playlist_conversion_preserves_metadata_and_track_order() {
        let source = EngineSongwriterPlaylist {
            playlist: PlaylistRef {
                id: "writers".into(),
                uri: "spotify:playlist:writers".into(),
                name: "Written by Artist".into(),
                revision: None,
                description: Some("Official writers playlist".into()),
                owner_id: "spotify".into(),
                owner_name: "Spotify".into(),
                cover_url: Some("https://i.scdn.co/image/writers".into()),
                cover_urls: Vec::new(),
                track_count: Some(37),
            },
            tracks: (0..10)
                .map(|index| TrackRef {
                    id: format!("track-{index}"),
                    uri: format!("spotify:track:track-{index}"),
                    name: format!("Song {index}"),
                    artist_names: vec!["Artist".into(), "Guest".into()],
                    artist_ids: vec!["artist-id".into(), "guest-id".into()],
                    artist_id: "artist-id".into(),
                    album_id: format!("album-{index}"),
                    album_name: format!("Album {index}"),
                    cover_url: format!("cover-{index}"),
                    ..TrackRef::default()
                })
                .collect(),
        };

        let songwriter = SongwriterPlaylist::from(source);
        assert_eq!(songwriter.playlist.id, "writers");
        assert_eq!(songwriter.playlist.name, "Written by Artist");
        assert_eq!(songwriter.playlist.owner, "Spotify");
        assert_eq!(songwriter.playlist.owner_id, "spotify");
        assert_eq!(
            songwriter.playlist.cover_url,
            "https://i.scdn.co/image/writers"
        );
        assert_eq!(songwriter.playlist.tracks_total, 37);
        assert_eq!(
            songwriter
                .tracks
                .iter()
                .map(|track| track.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Song 0", "Song 1", "Song 2", "Song 3", "Song 4", "Song 5", "Song 6", "Song 7",
                "Song 8", "Song 9",
            ]
        );
        assert_eq!(songwriter.tracks[0].artist_names, vec!["Artist", "Guest"]);
        assert_eq!(
            songwriter.tracks[0].artist_ids,
            vec!["artist-id", "guest-id"]
        );
        assert_eq!(songwriter.tracks[0].album_id, "album-0");
        assert_eq!(songwriter.tracks[0].album_name, "Album 0");

        let json = serde_json::to_value(songwriter).unwrap();
        assert_eq!(json["playlist"]["id"], "writers");
        assert_eq!(json["playlist"]["tracks_total"], 37);
        assert_eq!(json["tracks"].as_array().unwrap().len(), 10);
        assert_eq!(json["tracks"][9]["name"], "Song 9");
    }

    #[test]
    fn waveform_conversion_preserves_packed_payload_and_wire_names() {
        let waveform = TrackWaveform::from(EngineTrackWaveform {
            track_id: "0123456789ABCDEFGHIJKL".into(),
            duration_ms: 25,
            interval_ms: 1,
            bin_count: 3,
            peaks_base64: "AAAAAAAAAAAAAAAA".into(),
        });
        let json = serde_json::to_value(waveform).unwrap();
        assert_eq!(json["track_id"], "0123456789ABCDEFGHIJKL");
        assert_eq!(json["duration_ms"], 25);
        assert_eq!(json["interval_ms"], 1);
        assert_eq!(json["bin_count"], 3);
        assert_eq!(json["peaks_base64"], "AAAAAAAAAAAAAAAA");
    }

    /// The shell's half of the revision contract, which is about the number
    /// surviving the wire rather than about anything this struct computes.
    ///
    /// The engine's seed is an epoch in milliseconds, and the webview reads the
    /// value out of JSON as a JavaScript number; an integer has to come out of
    /// this type as the same integer, or two generations compare equal at the
    /// far end and the webview keeps the rows of the first. A payload that does
    /// not speak the field at all — a pre-revision engine, or a snapshot from
    /// before it existed — reads as `0`, the "assume changed" value, and still
    /// says `0` when it is written back out.
    #[test]
    fn a_queue_revision_round_trips_exactly_and_a_missing_one_reads_as_zero() {
        let state: PlaybackState =
            serde_json::from_str(r#"{"queue_revision":1789402727699}"#).unwrap();
        assert_eq!(state.queue_revision, 1_789_402_727_699);
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["queue_revision"], 1_789_402_727_699u64);

        let legacy: PlaybackState = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.queue_revision, 0);
        let json = serde_json::to_value(&legacy).unwrap();
        assert_eq!(json["queue_revision"], 0);
    }
}
