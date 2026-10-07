//! Tauri command layer: frontend-facing commands plus the background tasks
//! that keep `AppState`, the disk caches, and the frontend events in sync.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::app::{
    carry_local_fields, clear_cache_directory, compute_cache_stats, data_dir,
    insert_created_playlist, is_followed_playlist, load_app_settings, now_secs,
    order_by_last_activity, playlist_detail_from_cache, playlist_qualifies, remove_membership,
    update_app_settings, save_membership, save_playlist_list, save_tracks_cache,
    touch_playlist_activity as stamp_playlist_activity, touch_playlist_played, tracks_cache_bytes,
    upsert_membership, upsert_playlist, upsert_tracks_cache, write_tracks_cache_bytes, AppSettings,
    AppState, MembershipEntry, PlaylistListCache, PlaylistTracksEntry, RefreshCause,
    CACHE_STATS_TTL_SECS, LIBRARY_PAGE_SIZE, LIKED_MEMBERSHIP_ID,
};
use crate::engine_client::{unmark_cached_rows, EngineClient, PositionHeartbeat, RestoreSnapshot, StateLine};
use crate::log;
use crate::media_keys;
use crate::personal_api::{
    Authorization as PersonalAuthorization, Device as PersonalDevice, PersonalApi,
    Status as PersonalStatus,
};
use crate::playback_router::{Action as PlaybackAction, PlaybackRouter};
use crate::types::{
    AlbumDetail, AppState as AppStateSnapshot, Artist, ArtistCataloguePageDetail, ArtistDetail, CacheStats, EpisodeDetail, HistoryPageDetail, LibraryNodeDetail, LikedSongsDetail, PlaybackEvent, PlaybackState, Playlist, PlaylistDetail, PlaylistRecommendationsDetail, ProfileDetail, RadioDetail, SearchResult, Show, ShowDetail, SongwriterPlaylist, Track, TrackCreditsDetail, TrackPlaylistRef, TrackWaveform
};
use parking_lot::Mutex;
use renderer_engine::protocol::LibraryItem;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State};

/// Creates the same updater resource as the plugin's check command, with the
/// engine's durable shutdown included in Windows' pre-process-exit hook.
#[tauri::command]
pub async fn check_for_update(webview: tauri::Webview) -> Result<Option<Value>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let app = webview.app_handle().clone();
    let updater = webview.updater_builder()
        .on_before_exit(move || {
            crate::shutdown_for_exit(&app);
            app.cleanup_before_exit();
        })
        .build().map_err(|error| error.to_string())?;
    let Some(update) = updater.check().await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    // The plugin already parsed/validated pub_date as RFC3339. Retain that
    // spelling for JS instead of adding a second date formatter dependency.
    let mut metadata = json!({
        "currentVersion": update.current_version,
        "version": update.version,
        "date": update.raw_json.get("pub_date").and_then(Value::as_str),
        "body": update.body,
        "rawJson": update.raw_json,
    });
    metadata["rid"] = json!(webview.resources_table().add(update));
    Ok(Some(metadata))
}
// ---------------------------------------------------------------------------
// Playback commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn play(app: AppHandle, router: State<'_, Arc<PlaybackRouter>>) -> Result<(), String> {
    router.run(&app, PlaybackAction::Play).await
}

/// Whether the window can be seen, which decides how often a selected Spotify
/// device is read; `refresh` (on focus) reads it at once.
#[tauri::command]
pub async fn set_window_visible(router: State<'_, Arc<PlaybackRouter>>, visible: bool, refresh: bool) -> Result<(), String> {
    router.set_visible(visible, refresh);
    Ok(())
}

#[tauri::command]
pub async fn pause(app: AppHandle, router: State<'_, Arc<PlaybackRouter>>) -> Result<(), String> {
    router.run(&app, PlaybackAction::Pause).await
}

#[tauri::command]
pub async fn next(app: AppHandle, router: State<'_, Arc<PlaybackRouter>>) -> Result<(), String> {
    router.run(&app, PlaybackAction::Next).await
}

#[tauri::command]
pub async fn previous(app: AppHandle, router: State<'_, Arc<PlaybackRouter>>) -> Result<(), String> {
    router.run(&app, PlaybackAction::Previous).await
}

#[tauri::command]
pub async fn seek(app: AppHandle, router: State<'_, Arc<PlaybackRouter>>, position_ms: u32) -> Result<(), String> {
    router.run(&app, PlaybackAction::Seek(position_ms)).await
}

#[tauri::command]
pub async fn set_volume(app: AppHandle, router: State<'_, Arc<PlaybackRouter>>, percent: u8) -> Result<(), String> {
    router.run(&app, PlaybackAction::Volume(percent)).await
}

#[tauri::command]
pub async fn set_shuffle(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    enabled: bool,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Shuffle(enabled)).await
}

#[tauri::command]
pub async fn set_repeat(app: AppHandle, router: State<'_, Arc<PlaybackRouter>>, mode: String) -> Result<(), String> {
    router.run(&app, PlaybackAction::Repeat(mode)).await
}

#[tauri::command]
pub async fn set_playback_speed(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    speed: f32,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Speed(speed)).await
}

/// `automatic_start` is optional so clients built before the preference
/// existed keep direct-play semantics (`false`) when they omit it.
#[tauri::command]
pub async fn play_queue(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    queue: Vec<Track>,
    index: usize,
    context: String,
    automatic_start: Option<bool>,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Queue(queue, index, context, automatic_start.unwrap_or(false))).await
}

#[tauri::command]
pub async fn preview_track_edit(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    track: Track,
    cuts: Vec<renderer_engine::protocol::TimeRange>,
    loop_range: Option<renderer_engine::protocol::LoopRange>,
    position_ms: u32,
    preview_lease_id: u64,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Preview(track, cuts, loop_range, position_ms, preview_lease_id)).await
}

#[tauri::command]
pub async fn restore_preview(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    preview_lease_id: u64,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::RestorePreview(preview_lease_id)).await
}

#[tauri::command]
pub async fn play_queue_index(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    index: usize,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Index(index)).await
}

#[tauri::command]
pub async fn add_queue(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    track: Track,
    context: String,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Add(track, context)).await
}

#[tauri::command]
pub async fn add_queue_batch(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    tracks: Vec<Track>,
    context: String,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::AddBatch(tracks, context)).await
}

#[tauri::command]
pub async fn remove_queue(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    index: usize,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Remove(index)).await
}

#[tauri::command]
pub async fn move_queue(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    from: usize,
    to: usize,
) -> Result<(), String> {
    router.run(&app, PlaybackAction::Move(from, to)).await
}

/// One page of the listening archive. The filter and the order are the
/// engine's to apply: it holds the archive, and the view holds a window.
#[tauri::command]
pub async fn get_history(
    client: State<'_, Arc<EngineClient>>,
    offset: Option<usize>,
    limit: Option<usize>,
    query: Option<String>,
    sort: Option<String>,
) -> Result<HistoryPageDetail, String> {
    Ok(HistoryPageDetail::from(
        client
            .get_history(
                offset.unwrap_or(0),
                limit.unwrap_or_else(renderer_engine::protocol::default_history_page_size),
                query.as_deref().unwrap_or_default(),
                sort.as_deref().unwrap_or("recent"),
            )
            .await?,
    ))
}

#[tauri::command]
pub async fn clear_history(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.clear_history().await
}

#[tauri::command]
pub async fn get_track_waveform(
    client: State<'_, Arc<EngineClient>>,
    track_id: String,
) -> Result<TrackWaveform, String> {
    client
        .get_track_waveform(&track_id)
        .await
        .map(TrackWaveform::from)
}

#[tauri::command]
pub async fn cancel_track_waveform(
    client: State<'_, Arc<EngineClient>>,
    track_id: String,
) -> Result<(), String> {
    client.cancel_track_waveform(&track_id).await
}

#[tauri::command]
pub async fn get_track_edit(
    client: State<'_, Arc<EngineClient>>,
    track_id: String,
    playlist_id: Option<String>,
) -> Result<renderer_engine::protocol::TrackEditStatus, String> {
    client
        .track_edit_status(&track_id, playlist_id.as_deref())
        .await
}

#[tauri::command]
pub async fn save_track_edit(
    client: State<'_, Arc<EngineClient>>,
    track_id: String,
    duration_ms: u32,
    cuts: Vec<renderer_engine::protocol::TimeRange>,
    loop_range: Option<renderer_engine::protocol::LoopRange>,
) -> Result<renderer_engine::protocol::TrackEditDefinition, String> {
    client
        .save_track_edit(&track_id, duration_ms, &cuts, loop_range)
        .await
}

#[tauri::command]
pub async fn delete_track_edit(
    client: State<'_, Arc<EngineClient>>,
    track_id: String,
) -> Result<(), String> {
    client.delete_track_edit(&track_id).await
}

#[tauri::command]
pub async fn set_playlist_track_edit_enabled(
    client: State<'_, Arc<EngineClient>>,
    playlist_id: String,
    track_id: String,
    enabled: bool,
) -> Result<(), String> {
    client
        .set_playlist_track_edit_enabled(&playlist_id, &track_id, enabled)
        .await
}

#[tauri::command]
pub async fn set_playlist_track_excluded(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    client: State<'_, Arc<EngineClient>>,
    playlist_id: String,
    track_id: String,
    excluded: bool,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.set_playlist_track_excluded(&playlist_id, &track_id, excluded).await?;
    record_playlist_edit(&app, &playlist_id, account_generation)?;
    spawn_refresh_playlist(app.clone(), playlist_id, RefreshCause::Edit);
    // The exclusion is saved. A Spotify device that could not take the new
    // order is the router's to report and retry; it is not a failed save.
    router.eligibility_changed(&app).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Browse commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn search(
    client: State<'_, Arc<EngineClient>>,
    query: String,
    limit: Option<usize>,
) -> Result<SearchResult, String> {
    let limit = limit.unwrap_or(10).clamp(1, 50);
    let browse = client.browse_search(&query, limit).await?;
    Ok(SearchResult::from(browse))
}

fn cached_playlist_detail(state: &AppState, id: &str) -> Option<((u64, u64), PlaylistDetail)> {
    if !is_followed_playlist(&state.playlists, id) { return None; }
    let entry = state.tracks_cache.iter().find(|entry| entry.id == id)?.clone();
    Some(((state.library_generation, state.playlist_generation(id)),
        playlist_detail_from_cache(state, entry)))
}

/// Opens a followed playlist from the disk cache instantly and refreshes it in
/// the background; otherwise fetches from the engine. Public playlist entries
/// deliberately carry tracks only in the bounded cache, not library metadata,
/// so serving one as a followed-cache hit would blank its name/owner/cover.
#[tauri::command]
pub async fn browse_playlist(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<PlaylistDetail, String> {
    let cached = cached_playlist_detail(&state.lock(), &id);
    if let Some(((generation, playlist_generation), detail)) = cached {
        let current = {
            let guard = state.lock();
            library_generation_is_current(&guard, generation)
                && guard.playlist_read_is_current(&id, playlist_generation)
        };
        if !current {
            let exists = state
                .lock()
                .playlists
                .iter()
                .any(|playlist| playlist.id == id);
            return Err(if exists {
                "playlist detail request was superseded by a newer library generation".to_owned()
            } else {
                "playlist not found".to_owned()
            });
        }
        spawn_refresh_playlist(app, id, RefreshCause::Reopen);
        return Ok(detail);
    }
    let mut result = fetch_playlist(&app, &state, &client, &id).await?;
    let current = {
        let guard = state.lock();
        result.applied && library_generation_is_current(&guard, result.generation)
            && guard.playlist_read_is_current(&id, result.playlist_generation)
    };
    if current {
        client.reconcile_cached_rows(&mut result.detail.tracks, result.cache_generation);
        Ok(result.detail)
    } else {
        let exists = state
            .lock()
            .playlists
            .iter()
            .any(|playlist| playlist.id == id);
        Err(if exists {
            "playlist detail request was superseded by a newer library generation".to_owned()
        } else {
            "playlist not found".to_owned()
        })
    }
}

#[tauri::command]
pub async fn browse_radio(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<RadioDetail, String> {
    Ok(client.browse_radio(&id).await?.into())
}

/// Optional and strictly on-demand: this command is never called from
/// `browse_playlist`, cached, emitted, or allowed to affect playback state.
#[tauri::command]
pub async fn browse_playlist_recommendations(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<PlaylistRecommendationsDetail, String> {
    Ok(client.browse_playlist_recommendations(&id).await?.into())
}

#[tauri::command]
pub async fn browse_track(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<Track, String> {
    Ok(client.browse_track(&id).await?.into())
}

#[tauri::command]
pub async fn browse_album(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<AlbumDetail, String> {
    Ok(AlbumDetail::from(client.browse_album(&id).await?))
}

#[tauri::command]
pub async fn browse_artist(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<ArtistDetail, String> {
    Ok(ArtistDetail::from(client.browse_artist(&id).await?))
}

#[tauri::command]
pub async fn browse_artist_songwriter(
    client: State<'_, Arc<EngineClient>>,
    id: String,
    name: String,
) -> Result<Option<SongwriterPlaylist>, String> {
    Ok(client
        .browse_artist_songwriter(&id, &name)
        .await?
        .map(SongwriterPlaylist::from))
}

#[tauri::command]
pub async fn browse_artist_catalogue(
    client: State<'_, Arc<EngineClient>>,
    id: String,
    release_types: Option<Vec<String>>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ArtistCataloguePageDetail, String> {
    Ok(ArtistCataloguePageDetail::from(
        client
            .browse_artist_catalogue(
                &id,
                release_types.as_deref().unwrap_or_default(),
                offset.unwrap_or(0),
                limit.unwrap_or(4).clamp(1, 7),
            )
            .await?,
    ))
}

#[tauri::command]
pub async fn browse_liked_songs(
    client: State<'_, Arc<EngineClient>>,
    cursor: Option<String>,
) -> Result<LikedSongsDetail, String> {
    Ok(LikedSongsDetail::from(
        client.browse_liked_songs(cursor.as_deref()).await?,
    ))
}

#[tauri::command]
pub async fn browse_show(client: State<'_, Arc<EngineClient>>, id: String) -> Result<ShowDetail, String> {
    client.browse_show(&id).await.map(ShowDetail::from)
}

#[tauri::command]
pub async fn browse_episode(client: State<'_, Arc<EngineClient>>, id: String) -> Result<EpisodeDetail, String> {
    client.browse_episode(&id).await.map(EpisodeDetail::from)
}

#[tauri::command]
pub async fn browse_profile(
    client: State<'_, Arc<EngineClient>>,
    state: State<'_, Mutex<AppState>>,
    username: String,
) -> Result<ProfileDetail, String> {
    let known_playlists = ProfileDetail::library_artwork_refs(&state.lock().playlists);
    let mut profile = ProfileDetail::from(client.browse_profile(&username, &known_playlists).await?);
    profile.enrich_library_metadata(&state.lock().playlists);
    Ok(profile)
}

/// Visible rootlist rows reuse the profile header resolver. This needs only the
/// playback login, not a personal Web API grant, and never browses full tracks.
/// The covers are returned only, never emitted: the caller that asked patches
/// the rows itself.
#[tauri::command]
pub async fn hydrate_library_covers(
    client: State<'_, Arc<EngineClient>>,
    state: State<'_, Mutex<AppState>>,
    ids: Vec<String>,
) -> Result<Vec<Playlist>, String> {
    if ids.len() > 32 {
        return Err("at most 32 visible playlist covers may be requested at once".to_owned());
    }
    let (generation, references) = {
        let guard = state.lock();
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let references: Vec<_> = guard.playlists.iter()
            .filter(|playlist| wanted.contains(playlist.id.as_str()))
            .map(Playlist::artwork_ref).collect();
        (guard.library_generation, references)
    };
    if references.is_empty() {
        return Ok(Vec::new());
    }
    let resolved = client.browse_playlist_covers(&references).await?;
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, cache, summaries) = {
        let mut guard = state.lock();
        if !library_generation_is_current(&guard, generation) {
            return Ok(Vec::new());
        }
        let by_id: std::collections::HashMap<_, _> = resolved.iter()
            .map(|playlist| (playlist.id.as_str(), playlist)).collect();
        let mut summaries = Vec::new();
        let mut changed = false;
        for playlist in &mut guard.playlists {
            let Some(metadata) = by_id.get(playlist.id.as_str()) else { continue };
            // A full browse or rootlist update may have supplied newer artwork
            // while this header was in flight. Hydration fills gaps only.
            changed |= playlist.fill_artwork(metadata);
            summaries.push(playlist.clone());
        }
        let cache = changed.then(|| PlaylistListCache {
            version: 1,
            fetched_at: guard.playlists_fetched_at,
            me_id: guard.me_id.clone(),
            playlists: guard.playlists.clone(),
            playlist_tree: guard.playlist_tree.clone(),
        });
        (guard.library_dir(), cache, summaries)
    };
    if let Some(cache) = cache {
        save_playlist_list(&dir, &cache);
    }
    Ok(summaries)
}

/// Songwriter/producer/performer credits for one track.
///
/// Returned only, never emitted: credits are opened for one track at a time
/// from an overflow menu, so the caller that asked is the only consumer and a
/// broadcast would just be a second copy of the payload crossing IPC.
///
/// One ~1 KB request per invocation, and nothing prefetches it — this is the
/// only endpoint here whose cost scales per track rather than per page, so it
/// must stay strictly on demand.
#[tauri::command]
pub async fn browse_track_credits(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<TrackCreditsDetail, String> {
    Ok(TrackCreditsDetail::from(
        client.browse_track_credits(&id).await?,
    ))
}

/// Resolves one official Spotify Canvas video for the currently playing track.
/// The engine caches positive/negative answers in memory; errors stay errors so
/// a later panel open can retry without inventing a fallback URL.
#[tauri::command]
pub async fn browse_canvas(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<Option<renderer_engine::protocol::Canvas>, String> {
    client.browse_canvas(&id).await
}

/// Turns Canvas on for the account, as the official "Videos and Canvas"
/// setting does, when the account has it off. Asked for only when the
/// listener turns Canvas on in Settings; turning it off stays local.
#[tauri::command]
pub async fn enable_account_canvas(client: State<'_, Arc<EngineClient>>) -> Result<bool, String> {
    client.enable_account_canvas().await
}

// ---------------------------------------------------------------------------
// Follow commands
// ---------------------------------------------------------------------------

/// Every artist the signed-in account follows. Read-only: this app has no way
/// to change who you follow — the engine's `follow` module records why — so
/// the rail shows the collection and the official client edits it.
///
/// Deliberately uncached. The rail asks for it the first time the library
/// switches to artists, and following is mutable from every other Spotify
/// client, so a stale list held on disk would be a worse answer than a round
/// trip nobody pays for unless they look.
#[tauri::command]
pub async fn browse_followed_artists(
    client: State<'_, Arc<EngineClient>>,
) -> Result<Vec<Artist>, String> {
    Ok(client
        .browse_followed_artists()
        .await?
        .into_iter()
        .map(Artist::from)
        .collect())
}

/// Every podcast the account follows, newest first. Read through the engine
/// from Spotify's collection service, so it needs no developer app; like the
/// followed artists it is uncached and asked for only when the rail opens its
/// Podcasts tab.
#[tauri::command]
pub async fn browse_saved_shows(
    client: State<'_, Arc<EngineClient>>,
) -> Result<Vec<Show>, String> {
    Ok(client
        .browse_saved_shows()
        .await?
        .into_iter()
        .map(Show::from)
        .collect())
}

// ---------------------------------------------------------------------------
// Playlist edit commands
// ---------------------------------------------------------------------------

/// Creates a playlist and installs it at the head of the library immediately.
///
/// The engine already answers with a fully populated reference — the name is
/// the one we just posted — so nothing here waits on the rootlist to learn
/// it. Installing the row before the refresh is what keeps the caller's
/// optimistic insert and the refresh's answer in agreement: the stamp put on
/// it here survives the refetch via `carry_local_fields`, so the row does not
/// appear on top and then sink a second later when the rootlist event lands.
#[tauri::command]
pub async fn create_playlist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    name: String,
) -> Result<Playlist, String> {
    let account_generation = library_account_generation(&app);
    let reference = client.create_playlist(&name).await?;
    let playlist = commit_library_edit(&app, account_generation, |guard| {
        insert_created_playlist(&mut guard.playlists, Playlist::from(&reference), now_secs())
    })?;
    spawn_refresh_library(app);
    Ok(playlist)
}

#[tauri::command]
pub async fn rename_playlist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    name: String,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.rename_playlist(&id, &name).await?;
    commit_library_edit(&app, account_generation, |guard| {
        guard.record_playlist_rename(&id, &name);
    })?;
    spawn_refresh_library(app);
    Ok(())
}

/// Deletes a playlist you made, or removes someone else's from your library:
/// on the wire both are the same rootlist REM, which is all the official
/// client sends for either.
#[tauri::command]
pub async fn delete_playlist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.delete_playlist(&id).await?;
    forget_library_playlists(&app, account_generation, |tree| {
        remove_tree_playlist(tree, &id);
        vec![id.clone()]
    })?;
    spawn_refresh_library(app);
    Ok(())
}

/// Drops playlists that just left the library from every local store in one
/// critical section, together with the tree edit that removed them (which
/// answers with their ids), and puts all of it on disk before a concurrent
/// older refresh can persist — or the removed rows reappear on the next
/// start.
fn forget_library_playlists(
    app: &AppHandle,
    account_generation: u64,
    edit_tree: impl FnOnce(&mut Vec<LibraryNodeDetail>) -> Vec<String>,
) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, list_cache, tracks_cache, membership) = {
        let mut guard = state.lock();
        guard.check_account_generation(account_generation)?;
        guard.finish_library_import()?;
        guard.library_generation = guard.library_generation.wrapping_add(1);
        let ids = edit_tree(&mut guard.playlist_tree);
        let gone: HashSet<&str> = ids.iter().map(String::as_str).collect();
        guard.playlists.retain(|playlist| !gone.contains(playlist.id.as_str()));
        let before = guard.tracks_cache.len();
        guard.tracks_cache.retain(|entry| !gone.contains(entry.id.as_str()));
        let tracks_changed = before != guard.tracks_cache.len();
        // The index must forget the container in the same critical section,
        // or the next lookup could still light the mark for a playlist that
        // no longer exists.
        let mut removed = false;
        for id in &ids {
            removed |= remove_membership(&mut guard.memberships, id);
        }
        let membership = removed.then(|| guard.memberships.clone());
        (
            guard.library_dir(),
            library_list_cache(&guard),
            tracks_changed.then(|| guard.tracks_cache.clone()),
            membership,
        )
    };
    save_playlist_list(&dir, &list_cache);
    if let Some(tracks_cache) = tracks_cache {
        save_tracks_cache(&dir, &tracks_cache);
    }
    if let Some(entries) = membership {
        save_membership(&dir, &entries);
    }
    Ok(())
}

fn library_list_cache(state: &AppState) -> PlaylistListCache {
    PlaylistListCache {
        version: 1,
        fetched_at: state.playlists_fetched_at,
        me_id: state.me_id.clone(),
        playlists: state.playlists.clone(),
        playlist_tree: state.playlist_tree.clone(),
    }
}

fn record_playlist_edit(app: &AppHandle, id: &str, account_generation: u64) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, bytes) = {
        let mut guard = state.lock();
        guard.check_account_generation(account_generation)?;
        let changed = guard.record_playlist_edit(id);
        (guard.library_dir(), changed.then(|| tracks_cache_bytes(&guard.tracks_cache)).flatten())
    };
    if let Some(bytes) = bytes {
        tokio::task::block_in_place(|| write_tracks_cache_bytes(&dir, &bytes));
    }
    Ok(())
}

fn library_account_generation(app: &AppHandle) -> u64 {
    app.state::<Mutex<AppState>>().lock().account_generation
}

/// Writes a landed library edit into the shell's copy and to disk at once,
/// fenced like a delete: a rootlist read that was already in flight predates
/// the edit and must not install over it. The refresh that follows brings
/// the account's own answer.
fn commit_library_edit<R>(
    app: &AppHandle,
    account_generation: u64,
    edit: impl FnOnce(&mut AppState) -> R,
) -> Result<R, String> {
    let state = app.state::<Mutex<AppState>>();
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, cache, result) = {
        let mut guard = state.lock();
        guard.check_account_generation(account_generation)?;
        guard.finish_library_import()?;
        guard.library_generation = guard.library_generation.wrapping_add(1);
        let result = edit(&mut guard);
        (guard.library_dir(), library_list_cache(&guard), result)
    };
    save_playlist_list(&dir, &cache);
    Ok(result)
}

/// Puts someone else's playlist back in your library, at the top, and
/// answers with the library row. The row is the one the playlist's page
/// already holds; following it is library activity, so it is stamped the way
/// a created playlist is and the next refresh keeps it first.
#[tauri::command]
pub async fn follow_playlist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    playlist: Playlist,
) -> Result<Playlist, String> {
    let account_generation = library_account_generation(&app);
    client.follow_playlist(&playlist.id).await?;
    let row = commit_library_edit(&app, account_generation, |guard| {
        insert_created_playlist(&mut guard.playlists, playlist, now_secs())
    })?;
    spawn_refresh_library(app);
    Ok(row)
}

/// Creates an empty folder first in the library, or first in `parent`, and
/// answers with the folder as the tree holds it.
#[tauri::command]
pub async fn create_folder(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    name: String,
    parent: Option<String>,
) -> Result<LibraryNodeDetail, String> {
    let account_generation = library_account_generation(&app);
    let name = name.trim().to_owned();
    let id = client.create_folder(&name, parent.as_deref()).await?;
    let folder = LibraryNodeDetail::Folder {
        id,
        name,
        children: Vec::new(),
        last_activity: Some(now_secs()),
    };
    commit_library_edit(&app, account_generation, |guard| {
        LibraryNodeDetail::insert_first(&mut guard.playlist_tree, parent.as_deref(), folder.clone());
    })?;
    spawn_refresh_library(app);
    Ok(folder)
}

#[tauri::command]
pub async fn rename_folder(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    name: String,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    let name = name.trim().to_owned();
    client.rename_folder(&id, &name).await?;
    commit_library_edit(&app, account_generation, |guard| {
        if let Some(LibraryNodeDetail::Folder { name: current, .. }) =
            LibraryNodeDetail::folder_mut(&mut guard.playlist_tree, &id)
        {
            *current = name;
        }
    })?;
    spawn_refresh_library(app);
    Ok(())
}

/// Deletes a folder and everything in it, as the official client does: the
/// playlists inside leave the library with it.
#[tauri::command]
pub async fn delete_folder(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.delete_folder(&id).await?;
    forget_library_playlists(&app, account_generation, |tree| {
        let mut inside = Vec::new();
        if let Some(folder) = LibraryNodeDetail::take(tree, &LibraryItem::Folder { id }) {
            folder.playlist_ids(&mut inside);
        }
        inside
    })?;
    spawn_refresh_library(app);
    Ok(())
}

/// Moves a playlist or a folder to the top of `folder`, or out of every
/// folder when there is none.
#[tauri::command]
pub async fn move_library_item(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    item: LibraryItem,
    folder: Option<String>,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.move_library_item(&item, folder.as_deref()).await?;
    commit_library_edit(&app, account_generation, |guard| {
        LibraryNodeDetail::move_to(&mut guard.playlist_tree, &item, folder.as_deref());
    })?;
    spawn_refresh_library(app);
    Ok(())
}

#[tauri::command]
pub async fn add_playlist_tracks(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    uris: Vec<String>,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.add_playlist_tracks(&id, &uris).await?;
    record_playlist_edit(&app, &id, account_generation)?;
    spawn_refresh_playlist(app, id, RefreshCause::Edit);
    Ok(())
}

#[tauri::command]
pub async fn remove_playlist_tracks(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    uris: Vec<String>,
    expected_snapshot_id: Option<String>,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.remove_playlist_tracks(&id, &uris, expected_snapshot_id.as_deref()).await?;
    record_playlist_edit(&app, &id, account_generation)?;
    spawn_refresh_playlist(app, id, RefreshCause::Edit);
    Ok(())
}

#[tauri::command]
pub async fn reorder_playlist_tracks(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    from: usize,
    to: usize,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    client.reorder_playlist_tracks(&id, from, to).await?;
    record_playlist_edit(&app, &id, account_generation)?;
    spawn_refresh_playlist(app, id, RefreshCause::Edit);
    Ok(())
}

// ---------------------------------------------------------------------------
// Session commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn login(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.login().await
}

/// Persists the volume-normalisation preference and applies it to the live
/// engine. The preference is saved first: if the engine request fails (engine
/// momentarily down), the supervisor respawns it with the saved flag, so the
/// setting converges either way — but the UI still hears about the failure.
#[tauri::command]
pub async fn set_normalisation(
    client: State<'_, Arc<EngineClient>>,
    enabled: bool,
) -> Result<AppSettings, String> {
    let settings = update_app_settings(|settings| settings.normalisation = enabled)?;
    client.set_normalisation(enabled).await?;
    Ok(settings)
}

#[tauri::command]
pub async fn logout(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    personal: State<'_, Arc<PersonalApi>>,
) -> Result<(), String> {
    app.state::<Arc<PlaybackRouter>>().disconnect(&app).await;
    personal.disconnect(&app).await?;
    client.logout().await?;
    select_library_account(&app, "");
    Ok(())
}

#[tauri::command]
pub async fn personal_api_status(app: AppHandle, personal: State<'_, Arc<PersonalApi>>) -> Result<PersonalStatus, String> {
    personal.status(&app).await
}

#[tauri::command]
pub async fn personal_api_configure(
    app: AppHandle,
    personal: State<'_, Arc<PersonalApi>>,
    client_id: String,
) -> Result<PersonalStatus, String> {
    app.state::<Arc<PlaybackRouter>>().disconnect(&app).await;
    personal.configure(&app, client_id).await
}

#[tauri::command]
pub async fn personal_api_authorize(
    app: AppHandle,
    personal: State<'_, Arc<PersonalApi>>,
    enable_devices: bool,
) -> Result<PersonalAuthorization, String> {
    personal.authorize(app, enable_devices).await
}

#[tauri::command]
pub async fn personal_api_disconnect(app: AppHandle, personal: State<'_, Arc<PersonalApi>>, router: State<'_, Arc<PlaybackRouter>>) -> Result<PersonalStatus, String> {
    router.disconnect(&app).await;
    personal.disconnect(&app).await
}

#[tauri::command]
pub async fn personal_api_contains(
    app: AppHandle,
    personal: State<'_, Arc<PersonalApi>>,
    uris: Vec<String>,
) -> Result<Vec<bool>, String> {
    personal.contains(&app, uris).await
}

#[tauri::command]
pub async fn personal_api_set_saved(
    app: AppHandle,
    personal: State<'_, Arc<PersonalApi>>,
    state: State<'_, Mutex<AppState>>,
    uris: Vec<String>,
    saved: bool,
) -> Result<(), String> {
    let account_generation = library_account_generation(&app);
    personal.set_saved(&app, uris.clone(), saved).await?;
    // The write landed. The membership index answers "is this in Liked
    // Songs" everywhere, so it takes the change now rather than at the next
    // reconcile pass, and the UI hears about it through the one event every
    // other index change uses (which also drops the cached Liked Songs page).
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let written = {
        let mut guard = state.lock();
        guard.check_account_generation(account_generation)?;
        guard.record_liked_write(&uris, saved)
            .then(|| (guard.library_dir(), guard.memberships.clone()))
    };
    if let Some((dir, entries)) = written {
        save_membership(&dir, &entries);
        let _ = app.emit("memberships_changed", json!({"saved_tracks": true, "uris": uris, "saved": saved}));
    }
    Ok(())
}

#[tauri::command]
pub async fn personal_api_devices(
    app: AppHandle,
    personal: State<'_, Arc<PersonalApi>>,
    router: State<'_, Arc<PlaybackRouter>>,
) -> Result<Vec<PersonalDevice>, String> {
    if !router.devices_enabled() { return Err("Spotify devices are turned off in Settings".to_owned()); }
    personal.devices(&app).await
}

#[tauri::command]
pub async fn select_output(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    device_id: Option<String>,
) -> Result<(), String> {
    router.select(&app, device_id).await
}

// ---------------------------------------------------------------------------
// State + covers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_state(state: State<'_, Mutex<AppState>>, router: State<'_, Arc<PlaybackRouter>>) -> Result<AppStateSnapshot, String> {
    let guard = state.lock();
    // A cache selected during held playback restoration must not be paired
    // with the previous account's still-published transport identity.
    let library_visible = !guard.me_id.is_empty()
        && guard.playback.username == guard.me_id
        && !matches!(guard.playback.auth_state.as_str(), "logged_out" | "needs_login");
    Ok(AppStateSnapshot {
        playback: router.snapshot().filter(|remote| remote.username == guard.playback.username
            && !matches!(guard.playback.auth_state.as_str(), "logged_out" | "needs_login"))
            .unwrap_or_else(|| guard.playback.clone()),
        playlists: if library_visible { guard.playlists.clone() } else { Vec::new() },
        playlist_tree: if library_visible { guard.playlist_tree.clone() } else { Vec::new() },
        settings: load_app_settings(),
        library_fresh: library_visible && guard.library_fresh,
        me_id: if library_visible { guard.me_id.clone() } else { String::new() },
    })
}

/// Records that a successful playback started *from* playlist `id`.
///
/// The frontend calls this because only it knows the answer. A play command
/// carries track URIs, and the same track sits in any number of playlists, so
/// the backend cannot infer the source. `touch_playlist` updates both
/// `last_played` (Home listening history) and `last_activity` (library order).
///
/// Browsing or editing a playlist deliberately does not count as playback.
#[tauri::command]
pub async fn touch_playlist(state: State<'_, Mutex<AppState>>, id: String) -> Result<(), String> {
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, cache) = {
        let mut guard = state.lock();
        if !touch_playlist_played(&mut guard.playlists, &id, now_secs()) {
            // Playback started somewhere that is not a followed playlist (an
            // album, an artist page): no local timestamp should be invented.
            return Ok(());
        }
        (
            guard.library_dir(),
            PlaylistListCache {
                version: 1,
                fetched_at: guard.playlists_fetched_at,
                me_id: guard.me_id.clone(),
                playlists: guard.playlists.clone(),
                playlist_tree: guard.playlist_tree.clone(),
            },
        )
    };
    save_playlist_list(&dir, &cache);
    Ok(())
}

/// Records a successful add-to-playlist for playlist `id`.
///
/// This is intentionally separate from [`touch_playlist`]: adding a track
/// promotes sidebar/library activity but must not create a Home
/// "Recently played" item. Failed add commands never reach this command.
#[tauri::command]
pub async fn touch_playlist_activity(
    state: State<'_, Mutex<AppState>>,
    id: String,
) -> Result<(), String> {
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, cache) = {
        let mut guard = state.lock();
        if !stamp_playlist_activity(&mut guard.playlists, &id, now_secs()) {
            return Ok(());
        }
        (
            guard.library_dir(),
            PlaylistListCache {
                version: 1,
                fetched_at: guard.playlists_fetched_at,
                me_id: guard.me_id.clone(),
                playlists: guard.playlists.clone(),
                playlist_tree: guard.playlist_tree.clone(),
            },
        )
    };
    save_playlist_list(&dir, &cache);
    Ok(())
}

/// Which of the user's own containers hold `uri` — the data behind the
/// player bar's saved mark. Pure in-memory lookup: the index is maintained
/// by [`fetch_playlist`] and the reconciliation chain, so a track change
/// costs one IPC round trip and no network. Liked Songs leads the list;
/// playlists follow in current library order with names resolved live from
/// that library, so a rename is reflected without touching the index.
#[tauri::command]
pub fn get_track_playlists(
    state: State<'_, Mutex<AppState>>,
    uri: String,
) -> Result<Vec<TrackPlaylistRef>, String> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Ok(Vec::new());
    }
    let guard = state.lock();
    let mut refs = Vec::new();
    let containing: HashSet<&str> = guard.memberships.iter()
        .filter(|entry| entry.contains(uri)).map(|entry| entry.id.as_str()).collect();
    if containing.contains(LIKED_MEMBERSHIP_ID) {
        refs.push(TrackPlaylistRef {
            id: LIKED_MEMBERSHIP_ID.to_owned(),
            name: "Liked Songs".to_owned(),
        });
    }
    for playlist in &guard.playlists {
        if containing.contains(playlist.id.as_str()) {
            refs.push(TrackPlaylistRef {
                id: playlist.id.clone(),
                name: playlist.name.clone(),
            });
        }
    }
    Ok(refs)
}

/// Ensures the OS registration reflects `enabled`, returning the prior
/// registration state so a later disk-write failure can be rolled back.
fn set_autostart_registration(app: &AppHandle, enabled: bool) -> Result<bool, String> {
    let was_enabled = crate::autostart::is_enabled(app)
        .map_err(|error| format!("could not read launch-at-login registration: {error}"))?;
    if was_enabled == enabled {
        return Ok(was_enabled);
    }

    crate::autostart::set_enabled(app, enabled).map_err(|error| {
        let action = if enabled { "enable" } else { "disable" };
        format!("could not {action} launch at login: {error}")
    })?;
    Ok(was_enabled)
}

fn restore_autostart_registration(app: &AppHandle, enabled: bool) -> Result<(), String> {
    crate::autostart::set_enabled(app, enabled).map_err(|error| {
        let action = if enabled {
            "restore enabled"
        } else {
            "restore disabled"
        };
        format!("could not {action} launch-at-login registration: {error}")
    })
}

#[tauri::command]
pub fn set_audio_cache_limit(mb: u64) -> Result<AppSettings, String> {
    if !matches!(mb, 0 | 1024 | 2048 | 4096 | 8192) {
        return Err("audio cache limit must be 1, 2, 4, or 8 GiB, or unlimited".to_owned());
    }
    update_app_settings(|settings| settings.audio_cache_limit_mb = mb)
}

/// Updates the OS registration before persisting the preference. If writing
/// the preference fails, restore the registration to its previous state.
#[tauri::command]
pub fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<AppSettings, String> {
    let was_enabled = set_autostart_registration(&app, enabled)?;

    let updated = update_app_settings(|settings| settings.launch_at_login = enabled);
    if let Err(error) = updated.as_ref() {
        if was_enabled != enabled {
            if let Err(rollback_error) = restore_autostart_registration(&app, was_enabled) {
                return Err(format!(
                    "{error}; could not roll back launch-at-login registration: {rollback_error}"
                ));
            }
        }
        return Err(error.clone());
    }
    updated
}

#[tauri::command]
pub fn set_start_minimized(enabled: bool) -> Result<AppSettings, String> {
    update_app_settings(|settings| settings.start_minimized = enabled)
}

#[tauri::command]
pub fn set_animated_canvas(enabled: bool) -> Result<AppSettings, String> {
    update_app_settings(|settings| settings.animated_canvas = enabled)
}

/// Spotify devices on or off, locally: the grant stays. Saved first, so a
/// failed write changes nothing; then the router follows, and off brings
/// playback on a device back here, paused, before it goes quiet.
#[tauri::command]
pub async fn set_devices_enabled(
    app: AppHandle,
    router: State<'_, Arc<PlaybackRouter>>,
    enabled: bool,
) -> Result<AppSettings, String> {
    let settings = update_app_settings(|settings| settings.devices_enabled = enabled)?;
    router.set_devices_enabled(&app, enabled).await;
    Ok(settings)
}

/// File count and total bytes of the audio cache and the cover cache.
///
/// Two guards keep a Settings visit from costing anything noticeable. The
/// walk runs on the blocking pool, so counting thousands of files never
/// occupies an async worker (and never the UI thread, which only ever awaits
/// the IPC reply); and the result is memoised for [`CACHE_STATS_TTL_SECS`],
/// so a Settings page that re-invokes on every render still walks the disk at
/// most once a minute.
#[tauri::command]
pub async fn get_cache_stats(state: State<'_, Mutex<AppState>>) -> Result<CacheStats, String> {
    let now = now_secs();
    {
        let guard = state.lock();
        if let Some((computed_at, stats)) = guard.cache_stats {
            if now.saturating_sub(computed_at) < CACHE_STATS_TTL_SECS {
                return Ok(stats);
            }
        }
    }
    let stats = tauri::async_runtime::spawn_blocking(compute_cache_stats)
        .await
        .map_err(|error| format!("could not measure the caches: {error}"))?;
    state.lock().cache_stats = Some((now, stats));
    Ok(stats)
}

/// Clears one cache after an explicit Settings confirmation. Clearing audio
/// first stops playback and empties the queue so no decoder/download task can
/// keep a cache file open while Windows removes it. Credentials, volume,
/// playlist metadata, and diagnostic logs are outside both target directories.
#[tauri::command]
pub async fn clear_cache(
    kind: String,
    state: State<'_, Mutex<AppState>>,
    client: State<'_, Arc<EngineClient>>,
) -> Result<CacheStats, String> {
    match kind.as_str() {
        "audio" => {
            // Playback retirement, producer fencing and disk deletion complete
            // inside the serialized native command before it acknowledges.
            client.clear_audio_cache().await?;
        }
        "covers" => {
            let root = data_dir().join("covers");
            tauri::async_runtime::spawn_blocking(move || clear_cache_directory(&root, &[]))
                .await
                .map_err(|error| format!("could not clear the covers cache: {error}"))??;
        }
        _ => return Err("cache kind must be 'audio' or 'covers'".to_owned()),
    }

    let stats = tauri::async_runtime::spawn_blocking(compute_cache_stats)
        .await
        .map_err(|error| format!("could not measure caches after clearing: {error}"))?;
    state.lock().cache_stats = Some((now_secs(), stats));
    Ok(stats)
}

// ---------------------------------------------------------------------------
// Background tasks
// ---------------------------------------------------------------------------

fn apply_cached_ids(snapshot: &mut AppState, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    // The rows are shared with the engine client's copies; only a mark that
    // actually changes pays for a private copy.
    let queue = &mut snapshot.playback.queue;
    if queue.iter().any(|track| !track.cached && ids.contains(track.id.as_str())) {
        for track in Arc::make_mut(queue) {
            track.cached |= ids.contains(track.id.as_str());
        }
    }
    for entry in &mut snapshot.tracks_cache {
        for track in &mut entry.tracks {
            track.cached |= ids.contains(track.id.as_str());
        }
    }
}

fn apply_uncached_ids(snapshot: &mut AppState, ids: &[String]) {
    if ids.is_empty() { return; }
    let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let queue = &mut snapshot.playback.queue;
    if queue.iter().any(|track| track.cached && ids.contains(track.id.as_str())) {
        for track in Arc::make_mut(queue) {
            if ids.contains(track.id.as_str()) { track.cached = false; }
        }
    }
    for entry in &mut snapshot.tracks_cache {
        for track in &mut entry.tracks {
            if ids.contains(track.id.as_str()) { track.cached = false; }
        }
    }
}

fn reset_cache_marks(snapshot: &mut AppState) {
    snapshot.playback.cached_ids = None;
    snapshot.playback.uncached_ids = None;
    unmark_cached_rows(&mut snapshot.playback.queue);
    for entry in &mut snapshot.tracks_cache {
        for track in &mut entry.tracks { track.cached = false; }
    }
}

/// Applies a scalar position heartbeat to the shared snapshot: only the two
/// playhead scalars change, in place. The engine already distinguishes
/// heartbeats from real changes, so this never clones or compares the
/// queue — the cost is O(1) in queue length. Full states replace the whole
/// snapshot as before.
fn apply_position_heartbeat(snapshot: &mut AppState, heartbeat: PositionHeartbeat) {
    snapshot.playback.position_ms = heartbeat.position_ms;
    snapshot.playback.duration_ms = heartbeat.duration_ms;
}

/// Applies a scalar volume line to the shared snapshot: the one number, in
/// place, with the queue and the playhead untouched — `get_state`/`status`
/// then report the volume the engine actually has.
fn apply_volume_line(snapshot: &mut AppState, volume: u8) {
    snapshot.playback.volume = volume;
}

/// The window's event for a scalar volume line: a partial `state` carrying the
/// one key that changed.
///
/// The frontend's `applyPlayback` applies whatever keys are present, so this
/// reaches the slider's confirmed value without a queue payload, without
/// touching the playing authority and without re-anchoring the playhead —
/// which is what the full-state path there is for. A full state remains what a
/// real transport change emits.
fn volume_state_payload(volume: u8) -> Value {
    json!({ "volume": volume })
}

/// A failed restore may re-arm its plan immediately, but a ready state must
/// not start the next attempt until the returned backoff expires. The
/// consumer continues to receive scalar and lifecycle lines in the meantime.
#[derive(Default)]
struct RestoreRetry {
    deadline: Option<tokio::time::Instant>,
    generation: u64,
    username: String,
}

impl RestoreRetry {
    fn schedule(
        &mut self,
        delay: Duration,
        now: tokio::time::Instant,
        generation: u64,
        username: &str,
    ) {
        self.deadline = Some(now + delay);
        self.generation = generation;
        self.username.clear();
        self.username.push_str(username);
    }

    fn waiting(&self, now: tokio::time::Instant) -> bool {
        self.deadline.is_some_and(|deadline| now < deadline)
    }

    fn take_due(&mut self, now: tokio::time::Instant) -> Option<(u64, String)> {
        if self.waiting(now) {
            return None;
        }
        self.deadline.take()?;
        Some((self.generation, std::mem::take(&mut self.username)))
    }

    fn cancel(&mut self) {
        self.deadline = None;
        self.username.clear();
    }
}

fn request_restore_state(client: Arc<EngineClient>) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = client.status().await {
            log::warn(&format!(
                "could not re-request engine state for restore: {error}"
            ));
        }
    });
}

fn restore_frame_is_publishable(settled: &mut Option<PlaybackState>, state: &PlaybackState) -> bool {
    let Some(target) = settled.as_ref() else { return true; };
    if state.auth_state == "ready" && state != target { return false; }
    *settled = None;
    true
}

async fn run_restore_attempt(
    app: &AppHandle,
    client: &Arc<EngineClient>,
    snapshot: &RestoreSnapshot,
    generation: u64,
    username: &str,
    restore_retry: &mut RestoreRetry,
    restore_error: &mut Option<(String, String)>,
) -> Option<PlaybackState> {
    let result = async {
        restore_playback(client, snapshot).await?;
        // Replies precede their state lines. A subsequent scalar-free command
        // is the wire barrier proving every restore state has reached the reader.
        client.queue_metadata(None).await?;
        client.complete_pending_restore(generation)
            .ok_or_else(|| "the engine changed session before restoration settled".to_owned())
    }.await;
    let error = match result {
        Ok(state) => return Some(state),
        Err(error) => error,
    };
    log::warn(&format!("could not restore playback: {error}"));
    match client.retry_pending_restore_for_generation(generation) {
        Ok(Some(delay)) => {
            restore_retry.schedule(delay, tokio::time::Instant::now(), generation, username);
        }
        Ok(None) => {
            let message = format!("could not restore the previous session: {error}");
            *restore_error = Some((username.to_owned(), message.clone()));
            let _ = app.emit(
                "session",
                json!({
                    "auth_state": "ready",
                    "username": username,
                    "error": message,
                }),
            );
            // With the plan disarmed, publish the engine's actual state.
            request_restore_state(client.clone());
        }
        Err(()) => {} // An exited engine or cleared plan owns no retry or error.
    }
    None
}

/// Consumes engine state lines, mirrors them into `AppState`, and emits the
/// `state`/`position`/`session` events. The scalar lanes are forwarded in the
/// shape the window already applies: a position heartbeat goes out as the
/// `position` number, and a volume step as a partial `state` carrying only the
/// volume — the playhead is projected in the frontend between heartbeats and a
/// drag is optimistic there, so neither needs the queue.
pub async fn consume_states(app: AppHandle) {
    // Owned handle so spawned tasks do not borrow the AppHandle.
    let client = app.state::<Arc<EngineClient>>().inner().clone();
    let mut lines = client.subscribe_lines();
    if let Err(error) = client.status().await {
        log::warn(&format!("could not request initial engine state: {error}"));
    }


    let mut previous_identity: Option<(String, String)> = None;
    let mut last_error = String::new();
    let mut restore_error: Option<(String, String)> = None;
    let mut published_queue: Option<u64> = None;
    let mut published_order: Option<u64> = None;
    // The first authenticated line may be held while the playback queue is
    // restored. Rootlist browse is independent of those transport setters.
    let mut library_refresh_during_restore = false;
    let mut restore_retry = RestoreRetry::default();
    let mut restore_started_at = None;
    let mut settled_restore: Option<PlaybackState> = None;

    loop {
        let retry_deadline = restore_retry.deadline;
        let state = match tokio::select! {
            line = lines.recv() => line,
            _ = async {
                tokio::time::sleep_until(retry_deadline.expect("retry is scheduled")).await
            }, if retry_deadline.is_some() => {
                if let Some((generation, username)) =
                    restore_retry.take_due(tokio::time::Instant::now())
                {
                    if let Some(snapshot) = client.begin_pending_retry(generation) {
                        settled_restore = run_restore_attempt(
                            &app,
                            &client,
                            &snapshot,
                            generation,
                            &username,
                            &mut restore_retry,
                            &mut restore_error,
                        )
                        .await;
                    }
                }
                continue;
            }
        } {
            Ok(StateLine::State(state)) => state,
            Ok(StateLine::CacheMarksReset) => {
                reset_cache_marks(&mut app.state::<Mutex<AppState>>().lock());
                app.state::<Arc<PlaybackRouter>>().apply_cache_marks(true, &[], &[]);
                let _ = app.emit("state", json!({"cache_marks_reset": true}));
                continue;
            }
            Ok(StateLine::UncachedIds(ids)) => {
                apply_uncached_ids(&mut app.state::<Mutex<AppState>>().lock(), &ids);
                app.state::<Arc<PlaybackRouter>>().apply_cache_marks(false, &ids, &[]);
                let _ = app.emit("state", json!({"uncached_ids": ids}));
                continue;
            }
            Ok(StateLine::CachedIds(ids)) => {
                let state = app.state::<Mutex<AppState>>();
                apply_cached_ids(&mut state.lock(), &ids);
                app.state::<Arc<PlaybackRouter>>().apply_cache_marks(false, &[], &ids);
                let _ = app.emit("state", json!({"cached_ids": ids}));
                continue;
            }
            Ok(StateLine::Position(heartbeat)) => {
                if client.restore_is_pending() || settled_restore.is_some() { continue; }
                // A heartbeat only moved the playhead: freshen the snapshot
                // scalars in place and forward the existing scalar `position`
                // event unchanged (a number, no queue payload).
                let managed = app.state::<Mutex<AppState>>();
                let mut guard = managed.lock();
                apply_position_heartbeat(&mut guard, heartbeat);
                drop(guard);
                if !app.state::<Arc<PlaybackRouter>>().is_remote() {
                    let _ = app.emit("position", heartbeat.position_ms);
                    media_keys::update_position(heartbeat.position_ms);
                }
                continue;
            }
            Ok(StateLine::Volume(volume)) => {
                if client.restore_is_pending() || settled_restore.is_some() { continue; }
                // A volume step is one number: freshen the cached snapshot in
                // place — never the queue — and forward it as a partial state,
                // which `applyPlayback` applies as a volume-only update.
                let managed = app.state::<Mutex<AppState>>();
                let mut guard = managed.lock();
                apply_volume_line(&mut guard, volume);
                drop(guard);
                if !app.state::<Arc<PlaybackRouter>>().is_remote() {
                    let _ = app.emit("state", volume_state_payload(volume));
                }
                continue;
            }
            Ok(StateLine::Disconnected) => {
                settled_restore = None;
                restore_retry.cancel();
                restore_error = None;
                let disconnected = {
                    let managed = app.state::<Mutex<AppState>>();
                    let persistence = managed.lock().playlist_persistence.clone();
                    let _serialize = persistence.lock();
                    let mut guard = managed.lock();
                    guard.fence_library_reads();
                    guard.playback.playing = false;
                    guard.playback.auth_state = "disconnected".to_owned();
                    guard.playback.error = "playback engine disconnected".to_owned();
                    guard.playback.clone()
                };
                let router = app.state::<Arc<PlaybackRouter>>();
                if router.is_remote() {
                    router.engine_disconnected(&app, &disconnected.error);
                } else {
                    let _ = app.emit("state", PlaybackEvent::new(&disconnected, false, false));
                    media_keys::update_disconnected();
                }
                let _ = app.emit(
                    "session",
                    json!({
                        "auth_state": "disconnected",
                        "username": &disconnected.username,
                        "error": &disconnected.error,
                    }),
                );
                previous_identity =
                    Some(("disconnected".to_owned(), disconnected.username.clone()));
                last_error = disconnected.error;
                continue;
            }
            // The engine out-ran this consumer. A skipped full state re-emits
            // itself with the next real change and a skipped heartbeat is one
            // projection step, but a skipped *volume* line is that change gone:
            // the lane is change-driven, the window reads the volume from it
            // and nowhere else, and a slider drag writes twenty of them a
            // second while this loop is awaiting inside a state arm. Ask for a
            // full state instead of hoping some later line carries the number.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                settled_restore = None;
                log::warn(&format!(
                    "engine lines skipped ({skipped}); re-requesting state"
                ));
                // Lifecycle and negative lanes may have been skipped too.
                reset_cache_marks(&mut app.state::<Mutex<AppState>>().lock());
                app.state::<Arc<PlaybackRouter>>().apply_cache_marks(true, &[], &[]);
                let _ = app.emit("state", json!({"cache_marks_reset": true}));
                if let Err(error) = client.status().await {
                    log::warn(&format!("could not re-request engine state: {error}"));
                }
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        app.state::<Arc<PlaybackRouter>>().apply_cache_marks(false,
            state.uncached_ids.as_deref().unwrap_or_default(),
            state.cached_ids.as_deref().unwrap_or_default());
        if let Some(ids) = &state.uncached_ids {
            apply_uncached_ids(&mut app.state::<Mutex<AppState>>().lock(), ids);
            // Invalidations also reach off-queue pages while transport is hidden.
            let _ = app.emit("state", json!({"uncached_ids": ids}));
        }
        if let Some(ids) = &state.cached_ids {
            let managed = app.state::<Mutex<AppState>>();
            apply_cached_ids(&mut managed.lock(), ids);
            if client.restore_is_pending() || app.state::<Arc<PlaybackRouter>>().is_remote() {
                // Suppressing a local transport frame must not suppress a
                // cache addition for a visible row outside that queue.
                let _ = app.emit("state", json!({"cached_ids": ids}));
            }
        }
        // The consumer awaited the commands while the reader kept running.
        // Publish only after reaching the exact settled frame in wire order.
        if !restore_frame_is_publishable(&mut settled_restore, &state) { continue; }

        // A fresh child reports an authenticated but empty state before its
        // queue/settings are restored. Start restoration once and suppress
        // that blank state plus every intermediate setter state. EngineClient
        // clears the plan on a matching frame or the successful wire barrier.
        if state.auth_state == "ready" && !state.username.is_empty() {
            select_library_account(&app, &state.username);
        } else if matches!(state.auth_state.as_str(), "logged_out" | "needs_login") {
            select_library_account(&app, "");
        }
        if state.auth_state != "ready"
            && app.state::<Mutex<AppState>>().lock().playback.auth_state == "ready"
        {
            let managed = app.state::<Mutex<AppState>>();
            let persistence = managed.lock().playlist_persistence.clone();
            let _serialize = persistence.lock();
            managed.lock().fence_library_reads();
        }
        if state.auth_state != "ready" {
            library_refresh_during_restore = false;
            restore_started_at = None;
            restore_retry.cancel();
            restore_error = None;
            if matches!(state.auth_state.as_str(), "logged_out" | "needs_login") {
                app.state::<Mutex<AppState>>().lock().library_fresh = false;
            }
        }
        if state.auth_state == "ready" && client.restore_is_pending() {
            if !library_refresh_during_restore {
                // Playback restoration hides intermediate transport states,
                // but rootlist browse can run beside those sequential setters.
                spawn_refresh_library(app.clone());
                library_refresh_during_restore = true;
                restore_started_at = Some(std::time::Instant::now());
            }
        }
        if !client.restore_is_pending() {
            restore_retry.cancel();
        }
        if state.auth_state == "ready" && restore_retry.waiting(tokio::time::Instant::now()) {
            continue;
        }
        restore_retry.cancel();
        if let Some((snapshot, generation)) = client.begin_pending_restore_with_generation(&state) {
            settled_restore = run_restore_attempt(
                &app,
                &client,
                &snapshot,
                generation,
                &state.username,
                &mut restore_retry,
                &mut restore_error,
            )
            .await;
            continue;
        }
        if client.restore_is_pending() && state.auth_state == "ready" {
            continue;
        }

        let (became_ready, session_changed, auth_changed) = match &previous_identity {
            Some((auth_state, username)) => (
                state.auth_state == "ready"
                    && (auth_state.as_str() != "ready" || state.username != username.as_str()),
                state.auth_state != auth_state.as_str() || state.username != username.as_str(),
                state.auth_state != auth_state.as_str(),
            ),
            None => (state.auth_state == "ready", true, true),
        };

        if auth_changed {
            log::info(&format!("engine auth_state -> {}", state.auth_state));
        }
        if became_ready {
            log::info(&format!("engine ready; username={}", state.username));
        }
        if !state.error.is_empty() && state.error != last_error {
            last_error = state.error.clone();
            log::error(&format!("engine error: {}", state.error));
        }

        // Restoration is handled before identity and AppState projection
        // above, so no blank-ready state reaches either consumer.

        {
            let guard = app.state::<Mutex<AppState>>();
            let mut guard = guard.lock();
            guard.playback = state.clone();
        }
        if session_changed && (state.auth_state == "ready" || state.auth_state == "logged_out") {
            let router = app.state::<Arc<PlaybackRouter>>();
            if router.snapshot().is_some_and(|remote| state.auth_state == "logged_out" || remote.username != state.username) {
                router.disconnect(&app).await;
            }
            app.state::<Arc<PersonalApi>>()
                .account_changed(&app, if state.auth_state == "ready" { &state.username } else { "" })
                .await;
        }
        if state.auth_state == "ready" {
            app.state::<Arc<PlaybackRouter>>().engine_recovered(&app, &state).await;
        }


        // Full states are reserved for real changes; the scalar lanes were
        // already forwarded above as their own events.
        if !app.state::<Arc<PlaybackRouter>>().is_remote() {
            let include_queue = state.queue_revision == 0 || published_queue != Some(state.queue_revision);
            let include_order = state.order_revision == 0 || published_order != Some(state.order_revision);
            let _ = app.emit("state", PlaybackEvent::new(&state, include_queue, include_order));
            published_queue = Some(state.queue_revision);
            published_order = Some(state.order_revision);
            media_keys::update_state(&state);
        } else if state.playing {
            // An engine recovery or an un-routed internal action must not
            // restore local audio underneath an external output.
            if let Err(error) = client.pause().await {
                app.state::<Arc<PlaybackRouter>>().report_error(&app, &error);
            }
        }
        if session_changed {
            let _ = app.emit(
                "session",
                json!({
                    "auth_state": state.auth_state,
                    "username": state.username,
                    "error": restore_error.as_ref()
                        .filter(|(username, _)| username == &state.username)
                        .map_or(state.error.as_str(), |(_, error)| error.as_str()),
                }),
            );
        }
        publish_library_account(&app);
        if became_ready {
            if let Some(started) = restore_started_at.take() {
                log::info(&format!(
                    "playback restore held ready state for {} ms",
                    started.elapsed().as_millis()
                ));
            }
            if !std::mem::take(&mut library_refresh_during_restore) {
                spawn_refresh_library(app.clone());
            }
        }

        previous_identity = Some((state.auth_state, state.username));
    }
}

/// Applies settings, then restores queue and crash-only resume intent in one
/// engine command. A normal startup remains paused.
async fn restore_playback(client: &EngineClient, snapshot: &RestoreSnapshot) -> Result<(), String> {
    client.set_volume(snapshot.volume).await?;
    client.set_shuffle(snapshot.shuffle).await?;
    client.set_repeat(&snapshot.repeat).await?;
    client.set_playback_speeds(snapshot.track_speed, snapshot.episode_speed).await?;
    let index = snapshot.current_index.unwrap_or(0);
    client
        .restore_queue(&snapshot.queue, index, snapshot.position_ms, "", false,
            snapshot.resume_playing && snapshot.current_index.is_some())
        .await?;
    Ok(())
}

/// Switches only after an authenticated identity is known. Disk data from a
/// different account is never exposed during startup or a failed login.
fn select_library_account(app: &AppHandle, account: &str) {
    let managed = app.state::<Mutex<AppState>>();
    if managed.lock().me_id == account { return; }
    let persistence = managed.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    managed.lock().select_library_account(account);
}

/// Restore may fetch the library before its first ready state is publishable.
/// Deliver that account's latest cache only after the UI has seen its identity.
fn publish_library_account(app: &AppHandle) {
    let managed = app.state::<Mutex<AppState>>();
    if managed.lock().library_identity_published { return; }
    let persistence = managed.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let payload = {
        let mut guard = managed.lock();
        if guard.library_identity_published { return; }
        guard.library_identity_published = true;
        (guard.library_fresh,
            json!({"playlists": guard.playlists, "playlist_tree": guard.playlist_tree}))
    };
    let event = if payload.0 { "library" } else { "library_cached" };
    let _ = app.emit(event, payload.1);
    let _ = app.emit("memberships_changed", json!({"saved_tracks": true}));
}

/// One coalesced background library refresh. The engine owns classified read
/// retries; the shell must not multiply them with another retry stack.
fn spawn_refresh_library(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<Arc<EngineClient>>();
        let state = app.state::<Mutex<AppState>>();
        {
            let mut guard = state.lock();
            if guard.library_fetching {
                // Keep a trigger that landed during the fetch: it may describe
                // a committed edit newer than that fetch's snapshot.
                guard.library_refresh_queued = true;
                return;
            }
            guard.library_fetching = true;
        }
        loop {
            if let Err(error) = refresh_library(&state, &client, &app).await {
                log::error(&format!("library refresh failed: {error}"));
            }
            let mut guard = state.lock();
            if !std::mem::take(&mut guard.library_refresh_queued) {
                guard.library_fetching = false;
                return;
            }
        }
    });
}

/// Keep cached rows on a failed read; publish only a fenced, complete rootlist.
async fn refresh_library(
    state: &Mutex<AppState>,
    client: &EngineClient,
    app: &AppHandle,
) -> Result<(), String> {
    let result = fetch_library(state, client).await?;
    if !result.applied { return Ok(()); }
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    if !library_generation_is_current(&state.lock(), result.generation) { return Ok(()); }
    if state.lock().library_identity_published {
        let _ = app.emit("library", json!({"playlists": result.playlists, "playlist_tree": result.playlist_tree}));
    }
    spawn_membership_reconcile(app.clone());
    Ok(())
}

/// Pause between sequential reconciliation fetches. The chain runs after the
/// user-facing library refresh has finished; pacing keeps it background
/// traffic even for a large owned library.
const MEMBERSHIP_RECONCILE_GAP: Duration = Duration::from_millis(300);

/// One background membership-reconciliation chain, mirroring the library
/// chain's coalescing shape: triggers arriving mid-pass re-run once instead
/// of being dropped.
fn spawn_membership_reconcile(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<Arc<EngineClient>>();
        let state = app.state::<Mutex<AppState>>();
        {
            let mut guard = state.lock();
            if guard.membership_fetching {
                guard.membership_refresh_queued = true;
                return;
            }
            guard.membership_fetching = true;
        }
        loop {
            reconcile_memberships(&app, &state, &client).await;
            let mut guard = state.lock();
            if !std::mem::take(&mut guard.membership_refresh_queued) {
                guard.membership_fetching = false;
                return;
            }
        }
    });
}

fn membership_refresh_work(qualifying: Vec<(String, String)>, memberships: &[MembershipEntry]) -> Vec<String> {
    qualifying.into_iter().filter(|(id, revision)| {
        memberships.iter().find(|entry| &entry.id == id)
            .is_none_or(|entry| revision.is_empty() || entry.revision != *revision)
    }).map(|(id, _)| id).collect()
}

/// Brings the membership index in line with the current library: drops
/// containers that no longer qualify (deleted, unfollowed, or no longer
/// owned), refetches stale ones sequentially, and re-walks Liked Songs —
/// which has no revision and must always be re-asked to see external
/// likes and unlikes. Failures keep the previous entry rather than
/// corrupting a good index with an empty one. Whatever the pass changed is
/// saved and announced once, when it ends.
async fn reconcile_memberships(app: &AppHandle, state: &Mutex<AppState>, client: &EngineClient) {
    let (mut playlists, mut liked) = (false, false);
    refresh_memberships(state, client, &mut playlists, &mut liked).await;
    if !playlists && !liked {
        return;
    }
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, entries) = {
        let guard = state.lock();
        (guard.library_dir(), guard.memberships.clone())
    };
    save_membership(&dir, &entries);
    let _ = app.emit("memberships_changed", json!({"saved_tracks": liked}));
}

/// One pass over the index, in memory: `playlists` and `liked` report what it
/// changed, also when a newer library generation cuts it short.
async fn refresh_memberships(state: &Mutex<AppState>, client: &EngineClient, playlists: &mut bool, liked: &mut bool) {
    // Snapshot the work list and its fence before any network request. The
    // persistence lock keeps this snapshot ordered with a successful delete.
    let persistence = state.lock().playlist_persistence.clone();
    let (work, generation) = {
        let _serialize = persistence.lock();
        let mut guard = state.lock();
        let qualifying: Vec<(String, String)> = guard
            .playlists
            .iter()
            .filter(|playlist| playlist_qualifies(playlist, &guard.me_id))
            .map(|playlist| (playlist.id.clone(), playlist.snapshot_id.clone()))
            .collect();
        let qualifying_ids: HashSet<&str> = qualifying.iter().map(|(id, _)| id.as_str()).collect();
        let before = guard.memberships.len();
        guard.memberships.retain(|entry| {
            entry.id == LIKED_MEMBERSHIP_ID || qualifying_ids.contains(entry.id.as_str())
        });
        *playlists = before != guard.memberships.len();
        // Unknown is not unchanged. Revalidate source URI membership only,
        // sequentially paced below; never resolve full playlist track metadata.
        let work = membership_refresh_work(qualifying, &guard.memberships);
        (work, guard.library_generation)
    };

    for id in &work {
        let playlist_generation = {
            let guard = state.lock();
            if !library_generation_is_current(&guard, generation) { return; }
            if guard.playlists.iter().find(|playlist| playlist.id == *id).is_some_and(|playlist| {
                !playlist.snapshot_id.is_empty() && guard.memberships.iter().any(|entry| {
                    entry.id == *id && entry.revision == playlist.snapshot_id
                })
            }) {
                continue;
            }
            guard.playlist_generation(id)
        };
        match client.browse_playlist_membership(id).await {
            Ok(membership) => {
                let mut guard = state.lock();
                if !library_generation_is_current(&guard, generation)
                    || !guard.playlist_read_is_current(id, playlist_generation) {
                    guard.membership_refresh_queued = true;
                    return;
                }
                *playlists |= upsert_membership(
                    &mut guard.memberships,
                    MembershipEntry {
                        id: membership.id,
                        revision: membership.revision,
                        uris: membership.uris.into_iter().collect(),
                    },
                );
            }
            Err(error) => log::warn(&format!(
                "membership reconcile of playlist {id} failed: {error}"
            )),
        }
        if !library_generation_is_current(&state.lock(), generation) {
            return;
        }
        tokio::time::sleep(MEMBERSHIP_RECONCILE_GAP).await;
    }
    // Liked Songs: one context walk per pass. A page failure mid-walk keeps
    // the previously indexed set — a partial liked list would silently
    // unmark tracks that are still saved.
    if !library_generation_is_current(&state.lock(), generation) {
        return;
    }
    let liked_generation = state.lock().liked_generation;
    match browse_all_liked_uris(client).await {
        Ok(uris) => {
            let mut guard = state.lock();
            if !library_generation_is_current(&guard, generation) {
                return;
            }
            *liked = guard.commit_liked_read(liked_generation, uris).unwrap_or(false);
        }
        Err(error) => log::warn(&format!(
            "membership reconcile of liked songs failed: {error}"
        )),
    }
}

/// Walks every Saved Tracks page into one deduplicated URI set. Pages are
/// bare URI payloads; only a truncated context needs more than one request.
async fn browse_all_liked_uris(client: &EngineClient) -> Result<HashSet<String>, String> {
    let mut all = HashSet::new();
    let mut cursor = None;
    loop {
        let page = client.browse_liked_uris(cursor.as_deref()).await?;
        all.extend(
            page.uris
                .into_iter()
                .filter(|uri| uri.starts_with("spotify:track:")),
        );
        match page.next_cursor.filter(|next| !next.is_empty()) {
            Some(next) => cursor = Some(next),
            None => return Ok(all),
        }
    }
}

/// Refreshes one playlist in the background, repeating once when an edit
/// landed behind the fetch that just finished. `cause` describes why the
/// caller is asking; a re-open that meets a fetch already out is dropped by
/// [`AppState::start_playlist_refresh`] rather than queued, because that fetch
/// is about to emit exactly the payload a second pass would fetch.
fn spawn_refresh_playlist(app: AppHandle, id: String, cause: RefreshCause) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<Arc<EngineClient>>();
        let state = app.state::<Mutex<AppState>>();
        loop {
            {
                let mut guard = state.lock();
                if !guard.start_playlist_refresh(&id, cause) {
                    return;
                }
            }

            let result = fetch_playlist(&app, &state, &client, &id).await;
            let again = {
                let mut guard = state.lock();
                guard.finish_playlist_refresh(&id);
                guard.take_playlist_refresh_queued(&id)
            };

            match result {
                /* Tell the window, do not just warm the cache.
                This refresh used to update the caches and stop there, so the
                fresh payload was only ever seen the NEXT time the playlist was
                opened. Download marks made that obvious: `cached` is stripped
                when the track cache is loaded from disk, deliberately, since
                a pruned audio cache must not leave phantom marks behind — so
                a cache-served open showed none, the refresh quietly learned
                the real ones, and they appeared on the second open. */
                Ok(mut result) if result.applied => {
                    // A trigger landed while this fetch was out, so what we
                    // just read may predate the newest committed edit; the
                    // queued pass below speaks instead of this payload.
                    let persistence = state.lock().playlist_persistence.clone();
                    let _serialize = persistence.lock();
                    let current = {
                        let guard = state.lock();
                        library_generation_is_current(&guard, result.generation)
                            && guard.playlist_read_is_current(&id, result.playlist_generation)
                    };
                    if !again && current {
                        client.reconcile_cached_rows(&mut result.detail.tracks, result.cache_generation);
                        let _ = app.emit("playlist", &result.detail);
                    }
                }
                Ok(_) => {}
                Err(error) => log::error(&format!(
                    "background refresh of playlist {id} failed: {error}"
                )),
            }

            // An edit committed while this fetch was out queued one more
            // pass above; running it keeps the emitted payload from standing
            // stale until the next open.
            if !again {
                return;
            }
        }
    });
}

fn remove_tree_playlist(tree: &mut Vec<LibraryNodeDetail>, id: &str) {
    tree.retain_mut(|node| match node {
        LibraryNodeDetail::Folder { children, .. } => { remove_tree_playlist(children, id); true }
        LibraryNodeDetail::Playlist { id: playlist } => playlist != id,
    });
}

struct LibraryFetchResult {
    playlists: Vec<Playlist>,
    playlist_tree: Vec<LibraryNodeDetail>,
    generation: u64,
    applied: bool,
}

struct PlaylistFetchResult {
    detail: PlaylistDetail,
    generation: u64,
    playlist_generation: u64,
    cache_generation: u64,
    applied: bool,
}

fn library_generation_is_current(state: &AppState, generation: u64) -> bool {
    state.library_generation == generation
}

async fn fetch_library(
    state: &Mutex<AppState>,
    client: &EngineClient,
) -> Result<LibraryFetchResult, String> {
    let mut generation = state.lock().library_generation;
    let mut playlists = Vec::new();
    let mut playlist_tree = LibraryNodeDetail::split(client.browse_playlist_tree(LIBRARY_PAGE_SIZE).await?, &mut playlists);
    let fetched_at = now_secs();
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, cache) = {
        let mut guard = state.lock();
        if !library_generation_is_current(&guard, generation) {
            return Ok(LibraryFetchResult {
                playlists,
                playlist_tree,
                generation,
                applied: false,
            });
        }
        guard.finish_library_import()?;
        // A detail refresh may have enriched the current row while the
        // rootlist request was in flight. Carry and install in the same
        // serialized completion so sparse metadata cannot overwrite it.
        carry_local_fields(&guard.playlists, &mut playlists);
        LibraryNodeDetail::carry_folder_activity(&guard.playlist_tree, &mut playlist_tree);
        order_by_last_activity(&mut playlists);
        let fresh_ids: HashSet<_> = playlists.iter().map(|playlist| playlist.id.as_str()).collect();
        if guard.playlists.iter().any(|old| !fresh_ids.contains(old.id.as_str())) {
            guard.library_generation = guard.library_generation.wrapping_add(1);
            generation = guard.library_generation;
        }
        let changed = guard.playlists != playlists || guard.playlist_tree != playlist_tree;
        guard.playlist_tree = playlist_tree.clone();
        guard.playlists = playlists.clone();
        guard.playlists_fetched_at = Some(fetched_at);
        guard.library_fresh = true;
        (
            guard.library_dir(),
            changed.then(|| PlaylistListCache {
                version: 1,
                fetched_at: Some(fetched_at),
                me_id: guard.me_id.clone(),
                playlists: playlists.clone(),
                playlist_tree: playlist_tree.clone(),
            }),
        )
    };
    if let Some(cache) = cache { save_playlist_list(&dir, &cache); }
    Ok(LibraryFetchResult {
        playlists,
        playlist_tree,
        generation,
        applied: true,
    })
}


fn adopt_playlist_detail(
    state: &mut AppState,
    id: &str,
    generation: u64,
    playlist_generation: u64,
    detail: &PlaylistDetail,
    fetched_at: i64,
) -> Option<(bool, bool, bool)> {
    if !library_generation_is_current(state, generation)
        || !state.playlist_read_is_current(id, playlist_generation) { return None; }
    let tracks_changed = upsert_tracks_cache(&mut state.tracks_cache, PlaylistTracksEntry {
        id: detail.playlist.id.clone(),
        fetched_at: Some(fetched_at),
        revision: detail.playlist.snapshot_id.clone(),
        tracks: detail.tracks.clone(),
        excluded_track_ids: detail.excluded_track_ids.clone(),
    });
    let library_changed = is_followed_playlist(&state.playlists, id)
        && upsert_playlist(&mut state.playlists, detail.playlist.clone());
    let membership = playlist_qualifies(&detail.playlist, &state.me_id)
        && upsert_membership(&mut state.memberships, MembershipEntry {
            id: detail.playlist.id.clone(),
            revision: detail.playlist.snapshot_id.clone(),
            uris: detail.tracks.iter().map(|track| track.uri.clone()).collect(),
        });
    Some((tracks_changed, library_changed, membership))
}
/// Engine round-trip for one playlist; always updates the bounded tracks
/// cache, and — for a playlist the user created — the membership index the
/// saved mark reads. Only a playlist already present in the rootlist-backed
/// library also updates and persists the library entry.
async fn fetch_playlist(
    app: &AppHandle,
    state: &Mutex<AppState>,
    client: &EngineClient,
    id: &str,
) -> Result<PlaylistFetchResult, String> {
    let (generation, playlist_generation) = {
        let guard = state.lock();
        (guard.library_generation, guard.playlist_generation(id))
    };
    let cache_generation = client.cache_generation();
    let mut detail = PlaylistDetail::from(client.browse_playlist(id).await?);
    let fetched_at = now_secs();
    let persistence = state.lock().playlist_persistence.clone();
    let _serialize = persistence.lock();
    let (dir, list_cache, tracks_bytes, membership) = {
        let mut guard = state.lock();
        guard.finish_library_import()?;
        client.reconcile_cached_rows(&mut detail.tracks, cache_generation);
        let Some((tracks_changed, should_persist_library, membership_changed)) =
            adopt_playlist_detail(&mut guard, id, generation, playlist_generation, &detail, fetched_at)
        else {
            return Ok(PlaylistFetchResult {
                detail,
                generation,
                playlist_generation,
                cache_generation,
                applied: false,
            });
        };
        let membership = membership_changed.then(|| guard.memberships.clone());
        // A browse that only confirmed what the cache already held owes the
        // disk nothing: this file is 1.5 MB of rows, and re-serializing it
        // under the lock for an unchanged entry is pure churn. The bytes are
        // built here because they borrow the rows — a clone to serialize them
        // elsewhere would cost more than the serialization.
        let tracks_bytes = tracks_changed
            .then(|| tracks_cache_bytes(&guard.tracks_cache))
            .flatten();
        (
            guard.library_dir(),
            should_persist_library.then(|| PlaylistListCache {
                version: 1,
                fetched_at: guard.playlists_fetched_at,
                me_id: guard.me_id.clone(),
                playlists: guard.playlists.clone(),
                playlist_tree: guard.playlist_tree.clone(),
            }),
            tracks_bytes,
            membership,
        )
    };
    if let Some(bytes) = tracks_bytes {
        // A large cache write does not belong on an async worker. `block_in_place`
        // rather than `spawn_blocking` because the persistence guard above has
        // to stay held until the bytes land: the unfollow path serializes its
        // own removal under the same guard, and a refresh that captured the
        // removed row before it must not overtake it on disk.
        tokio::task::block_in_place(|| write_tracks_cache_bytes(&dir, &bytes));
    }
    if let Some(list_cache) = list_cache {
        save_playlist_list(&dir, &list_cache);
    }
    if let Some(entries) = membership {
        save_membership(&dir, &entries);
        // Hovering the mark must not wait for the next track change: an add
        // to the playing playlist lights it up within one event round trip.
        let _ = app.emit("memberships_changed", json!({"saved_tracks": false}));
    }
    let _ = app.emit("playlist_summary", &detail.playlist);
    Ok(PlaylistFetchResult {
        detail,
        generation,
        playlist_generation,
        cache_generation,
        applied: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PlaybackState;

    fn playlist_payload(name: &str, revision: &str, ids: &[&str]) -> PlaylistDetail {
        PlaylistDetail {
            playlist: Playlist { id: "p".into(), name: name.into(), owner_id: "owner".into(),
                snapshot_id: revision.into(), ..Default::default() },
            tracks: ids.iter().map(|id| Track { id: (*id).into(), uri: format!("spotify:track:{id}"), ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn landed_track_edit_refuses_cached_old_queue_until_new_detail_arrives() {
        let mut state = AppState::new(std::path::PathBuf::new());
        state.me_id = "owner".into();
        state.playlists.push(playlist_payload("Playlist", "before", &[]).playlist);
        let generation = state.library_generation;
        assert!(adopt_playlist_detail(&mut state, "p", generation, 0,
            &playlist_payload("Playlist", "before", &["a", "b"]), 1).is_some());
        assert_eq!(cached_playlist_detail(&state, "p").unwrap().1.tracks.len(), 2);
        state.record_playlist_edit("p");
        assert!(cached_playlist_detail(&state, "p").is_none(), "card play must not queue removed rows");
        assert!(adopt_playlist_detail(&mut state, "p", generation, 0,
            &playlist_payload("Playlist", "before", &["a", "b"]), 2).is_none());
        let edited = state.playlist_generation("p");
        assert!(adopt_playlist_detail(&mut state, "p", generation, edited,
            &playlist_payload("Playlist", "after", &["a"]), 3).is_some());
        let cached = cached_playlist_detail(&state, "p").unwrap().1;
        assert_eq!(cached.playlist.snapshot_id, "after");
        assert_eq!(cached.tracks.iter().map(|track| track.id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn rename_is_visible_immediately_and_older_detail_cannot_undo_it() {
        let mut state = AppState::new(std::path::PathBuf::new());
        state.me_id = "owner".into();
        state.playlists.push(playlist_payload("Old name", "r", &[]).playlist);
        let generation = state.library_generation;
        adopt_playlist_detail(&mut state, "p", generation, 0,
            &playlist_payload("Old name", "r", &["a"]), 1).unwrap();
        state.record_playlist_rename("p", " New name ");
        let cached = cached_playlist_detail(&state, "p").unwrap().1;
        assert_eq!(cached.playlist.name, "New name");
        assert_eq!(cached.tracks[0].id, "a", "rename does not invalidate unchanged tracks");
        assert!(adopt_playlist_detail(&mut state, "p", generation, 0,
            &playlist_payload("Old name", "r", &["a"]), 2).is_none());
        assert_eq!(state.playlists[0].name, "New name");
    }

    #[test]
    fn auth_departure_rejects_old_payload_but_keeps_same_account_offline_detail() {
        let mut state = AppState::new(std::path::PathBuf::new());
        state.me_id = "owner".into();
        state.playlists.push(playlist_payload("Offline", "r", &[]).playlist);
        let generation = state.library_generation;
        adopt_playlist_detail(&mut state, "p", generation, 0,
            &playlist_payload("Offline", "r", &["a"]), 1).unwrap();
        state.fence_library_reads();
        assert_eq!(cached_playlist_detail(&state, "p").unwrap().1.tracks[0].id, "a");
        assert!(adopt_playlist_detail(&mut state, "p", generation, 0,
            &playlist_payload("Stale", "old", &["b"]), 2).is_none());
        state.select_library_account("");
        assert!(cached_playlist_detail(&state, "p").is_none());
    }

    #[test]
    fn identical_playlist_detail_does_not_request_any_cache_rewrite() {
        let mut state = AppState::new(std::path::PathBuf::new());
        state.me_id = "owner".into();
        state.playlists.push(playlist_payload("Playlist", "r", &[]).playlist);
        let generation = state.library_generation;
        let detail = playlist_payload("Playlist", "r", &["a"]);
        adopt_playlist_detail(&mut state, "p", generation, 0, &detail, 1).unwrap();
        assert_eq!(adopt_playlist_detail(&mut state, "p", generation, 0, &detail, 2), Some((false, false, false)));
        assert_eq!(cached_playlist_detail(&state, "p").unwrap().1.tracks[0].id, "a");
    }

    #[test]
    fn restore_publication_skips_buffered_intermediates_but_accepts_settled_concurrent_pause() {
        let final_state = playing_state(0);
        let mut final_state = PlaybackState { playing: false, ..final_state };
        let mut settled = Some(final_state.clone());
        let blank = PlaybackState { auth_state: "ready".into(), ..Default::default() };
        assert!(!restore_frame_is_publishable(&mut settled, &blank));
        let old_end = playing_state(200_000);
        assert!(!restore_frame_is_publishable(&mut settled, &old_end));
        assert!(restore_frame_is_publishable(&mut settled, &final_state));
        assert!(settled.is_none());
        final_state.position_ms = 500;
        assert!(restore_frame_is_publishable(&mut settled, &final_state));
        settled = Some(final_state);
        assert!(restore_frame_is_publishable(&mut settled, &PlaybackState::default()));
        assert!(settled.is_none());
    }

    #[test]
    fn direct_pre_edit_read_cannot_replace_landed_tracks_summary_or_membership() {
        let mut state = AppState::new(std::path::PathBuf::new());
        state.me_id = "owner".into();
        state.playlists.push(crate::types::Playlist { id: "p".into(), owner_id: "owner".into(), ..Default::default() });
        let generation = state.library_generation;
        let old_generation = state.playlist_generation("p");
        let detail = |revision: &str, ids: &[&str]| PlaylistDetail {
            playlist: crate::types::Playlist { id: "p".into(), name: revision.into(), owner_id: "owner".into(),
                snapshot_id: revision.into(), ..Default::default() },
            tracks: ids.iter().map(|id| Track { id: (*id).into(), uri: format!("spotify:track:{id}"), ..Default::default() }).collect(),
            ..Default::default()
        };
        state.record_playlist_edit("p");
        let current = state.playlist_generation("p");
        assert!(adopt_playlist_detail(&mut state, "p", generation, current, &detail("new", &["a", "b"]), 2).is_some());
        assert!(adopt_playlist_detail(&mut state, "p", generation, old_generation, &detail("old", &["a"]), 1).is_none());
        assert_eq!(state.tracks_cache[0].revision, "new");
        assert_eq!(state.playlists[0].name, "new");
        assert!(state.memberships[0].uris.contains("spotify:track:b"));
    }

    #[test]
    fn negative_cache_marks_and_session_reset_reach_off_queue_tracks() {
        let mut state = AppState::new(std::path::PathBuf::new());
        state.playback.queue = vec![Track { id: "a".into(), cached: true, ..Default::default() }].into();
        state.tracks_cache.push(PlaylistTracksEntry { id: "p".into(), fetched_at: None, revision: String::new(),
            tracks: vec![Track { id: "b".into(), cached: true, ..Default::default() }], excluded_track_ids: Vec::new() });
        apply_uncached_ids(&mut state, &["b".into()]);
        assert!(!state.tracks_cache[0].tracks[0].cached);
        assert!(state.playback.queue[0].cached);
        apply_cached_ids(&mut state, &["b".into()]);
        assert!(state.tracks_cache[0].tracks[0].cached);
        reset_cache_marks(&mut state);
        assert!(!state.playback.queue[0].cached);
        assert!(!state.tracks_cache[0].tracks[0].cached);
    }

    #[test]
    fn membership_refresh_revalidates_unknown_versions_and_skips_only_proven_unchanged_rows() {
        let memberships = vec![
            MembershipEntry { id: "unchanged".into(), revision: "r1".into(), uris: HashSet::new() },
            MembershipEntry { id: "unknown".into(), revision: "r2".into(), uris: HashSet::new() },
            MembershipEntry { id: "changed".into(), revision: "r3".into(), uris: HashSet::new() },
        ];
        let rows = [("unchanged", "r1"), ("unknown", ""), ("changed", "r4"), ("new", "")]
            .into_iter().map(|(id, revision)| (id.into(), revision.into())).collect();
        assert_eq!(membership_refresh_work(rows, &memberships), ["unknown", "changed", "new"]);
    }

    #[test]
    fn a_deleted_playlist_leaves_every_folder_of_the_tree() {
        let node = |id: &str| LibraryNodeDetail::Playlist { id: id.into() };
        let folder = |id: &str, children| LibraryNodeDetail::Folder { id: id.into(), name: id.into(), children, last_activity: None };
        let mut tree = vec![folder("f", vec![node("a"), folder("nested", vec![node("b"), node("a")])]), node("a")];
        remove_tree_playlist(&mut tree, "a");
        assert_eq!(tree, [folder("f", vec![folder("nested", vec![node("b")])])]);
    }

    #[test]
    fn failed_restore_waits_for_deadline_and_disconnect_cancels_old_retry() {
        let started = tokio::time::Instant::now();
        let mut retry = RestoreRetry::default();
        assert!(!retry.waiting(started), "initial restore has no backoff");

        retry.schedule(Duration::from_secs(5), started, 7, "listener");
        assert!(retry.waiting(started));
        assert!(retry.waiting(started + Duration::from_secs(4)));
        assert_eq!(retry.take_due(started + Duration::from_secs(4)), None);
        assert_eq!(retry.deadline, Some(started + Duration::from_secs(5)));
        assert_eq!(
            retry.take_due(started + Duration::from_secs(5)),
            Some((7, "listener".to_owned()))
        );
        assert_eq!(retry.deadline, None);

        retry.schedule(
            Duration::from_secs(10),
            started + Duration::from_secs(5),
            8,
            "new",
        );
        assert!(retry.waiting(started + Duration::from_secs(14)));
        retry.cancel(); // Disconnect or a matched/cleared plan.
        assert_eq!(retry.take_due(started + Duration::from_secs(15)), None);
    }

    #[test]
    fn library_generation_predicate_rejects_pre_delete_results() {
        let mut state = AppState::new(std::path::PathBuf::new());
        let captured = state.library_generation;
        assert!(library_generation_is_current(&state, captured));

        state.library_generation = state.library_generation.wrapping_add(1);
        assert!(
            !library_generation_is_current(&state, captured),
            "a response captured before delete must be fenced"
        );
        assert!(library_generation_is_current(
            &state,
            state.library_generation
        ));
    }

    fn playing_state(position_ms: u32) -> PlaybackState {
        PlaybackState {
            auth_state: "ready".to_owned(),
            playing: true,
            position_ms,
            duration_ms: 200_000,
            current_index: Some(0),
            current_uri: "spotify:track:a".to_owned(),
            queue: vec![Track::default()].into(),
            ..PlaybackState::default()
        }
    }

    #[test]
    fn position_heartbeats_update_only_the_playhead_scalars_in_place() {
        let mut snapshot = AppState::new(std::path::PathBuf::new());
        snapshot.playback = playing_state(1_000);
        let queue_ptr = snapshot.playback.queue.as_ptr();

        apply_position_heartbeat(
            &mut snapshot,
            PositionHeartbeat {
                position_ms: 3_000,
                duration_ms: 250_000,
            },
        );

        assert_eq!(snapshot.playback.position_ms, 3_000);
        assert_eq!(snapshot.playback.duration_ms, 250_000);
        // The queue is never cloned, compared, or rebuilt: the scalars are
        // written in place over the existing snapshot.
        assert_eq!(
            snapshot.playback.queue.as_ptr(),
            queue_ptr,
            "heartbeat must not touch the queue"
        );
        assert_eq!(snapshot.playback.playing, true);
        assert_eq!(snapshot.playback.current_uri, "spotify:track:a");
        assert_eq!(snapshot.playback.volume, 50);
        assert_eq!(snapshot.playback.queue, vec![Track::default()].into());
    }

    /// A volume step reaches the window as a partial `state`: the one key that
    /// changed. That is the shape `applyPlayback` already applies — it copies
    /// whichever keys are present — so the slider's confirmed value updates
    /// without a queue payload, without touching the playing authority and
    /// without re-anchoring the playhead, all of which a full state would do
    /// twenty times a second during a drag.
    #[test]
    fn volume_lines_are_partial_states_and_update_only_the_snapshot_volume() {
        let mut snapshot = AppState::new(std::path::PathBuf::new());
        snapshot.playback = playing_state(1_000);
        let queue_ptr = snapshot.playback.queue.as_ptr();

        apply_volume_line(&mut snapshot, 37);

        assert_eq!(snapshot.playback.volume, 37);
        assert_eq!(
            snapshot.playback.queue.as_ptr(),
            queue_ptr,
            "a volume step must not touch the queue"
        );
        assert_eq!(
            snapshot.playback.position_ms, 1_000,
            "nor the playhead: a volume step carries no position"
        );
        assert_eq!(snapshot.playback.playing, true, "nor the transport intent");
        assert_eq!(
            volume_state_payload(37),
            serde_json::json!({ "volume": 37 }),
            "the event carries one key and nothing else"
        );
    }
}
