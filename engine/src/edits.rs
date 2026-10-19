//! Playlist mutation over the spclient playlist4 service, replacing the
//! shared-quota Web API edit calls.
//!
//! Endpoints (spclient mirrors the Hermes `hm://` URIs verbatim):
//! - Playlist content edits (add/remove/reorder/rename) POST an
//!   `OpList`-style delta wrapped in `ListChanges` to
//!   `/playlist/v2/playlist/{id}/changes`. The `base_revision` is the
//!   playlist4 revision (the same bytes `browse_playlist` reports
//!   hex-encoded); it acts as the optimistic-concurrency check — a stale
//!   revision is rejected instead of clobbering concurrent edits.
//! - Create posts a `ListUpdateRequest` (attributes only, no base revision)
//!   to `/playlist/v2/playlist`; the server answers with a `CreateListReply`
//!   carrying the new playlist URI, which is then added to the user's
//!   rootlist via `/playlist/v2/user/{user}/rootlist/changes` (the same
//!   rootlist-changes endpoint the official client uses to place playlists;
//!   cross-verified against mirrorfm's working rootlist reorder client).
//!   That ADD carries `add_first`, so a new playlist lands at the top of the
//!   library server-side — see [`rootlist_add_op`].
//! - Delete (unfollow) is a rootlist REM op keyed by the playlist URI, and
//!   following one back is a rootlist ADD at the top. Captures of the
//!   official desktop client show that this is all following is: there is
//!   no separate follow call, only the rootlist edit.
//! - Folders are two rootlist items, `spotify:start-group:{id}:{name}` and
//!   `spotify:end-group:{id}`, with everything between them inside: `id` is
//!   16 lowercase hex digits and `name` is form-encoded (`+` for a space).
//!   The official client's own edits, as captured from its traffic, are what
//!   the folder ops copy: create is one positional ADD of both markers;
//!   rename is a REM of the start marker by index plus an ADD of the new one
//!   at the same index, keeping its creation timestamp and leaving the end
//!   marker alone; moving a playlist or a whole folder is a positional MOV.
//!   Deleting a folder takes its contents with it — the official client asks
//!   "Do you really want to delete this folder and all playlists inside?"
//!   (`folder.delete-header` in its strings) — so it is one positional REM of
//!   the whole span. Every positional op is computed against a rootlist
//!   snapshot read for it and posted on that snapshot's revision.
//!
//! Wire format: protobuf binary (`application/x-protobuf`), using the
//! official-client `playlist4_external.proto` types from librespot-protocol
//! 0.8.0. Responses parse as `SelectedListContent` (playlist changes) or
//! `CreateListReply` (create). Most edits fetch the latest revision;
//! previewed removals additionally require that it matches the reviewed
//! snapshot, so concurrent changes cannot silently expand a destructive edit.
//!
//! MOV semantics are settled against two independent references: the Web
//! API documents `insert_before` in pre-move coordinates ("first item of a
//! 10-item playlist to the last position" means insert_before=10), and the
//! official web player builds index-based MOV ops the same way (before an
//! item -> its current index; after it -> current index + 1; end ->
//! length). So the server reads `to_index` as an insert-before position in
//! the pre-move list, and `reorder_tracks` converts the caller's
//! final-position target accordingly. REM uses `items_as_key` (remove by
//! URI) which mirrors the Web API remove-by-uri call, including its
//! behavior on duplicate tracks.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use http::Method;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Metadata, Playlist};
use librespot_protocol as protocol;
use protobuf::Message;
use protocol::playlist4_external as p4;
use serde::Deserialize;

use renderer_engine::protocol::PlaylistRef;

// ---------------------------------------------------------------------------
// pure op builders (unit-tested)
// ---------------------------------------------------------------------------

fn item(uri: &str) -> p4::Item {
    let mut item = p4::Item::new();
    item.uri = Some(uri.to_owned());
    item
}

/// ADD op appending every given URI at the end of the playlist.
fn add_tracks_op(uris: &[String]) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::ADD);
    let mut add = p4::Add::new();
    add.add_last = Some(true);
    add.items = uris.iter().map(|uri| item(uri)).collect();
    op.add = protobuf::MessageField::some(add);
    op
}

/// REM op keyed by item URI (`items_as_key`), mirroring the Web API
/// remove-by-uris call.
fn remove_tracks_op(uris: &[String]) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::REM);
    let mut rem = p4::Rem::new();
    rem.items_as_key = Some(true);
    rem.items = uris.iter().map(|uri| item(uri)).collect();
    op.rem = protobuf::MessageField::some(rem);
    op
}

/// MOV op moving the single item at `from` so that it lands at index `to`
/// of the resulting list. Callers speak final positions; the wire's
/// `to_index` is an insert-before slot in the pre-move list — "to reorder
/// the first item to the last position in a playlist with 10 items, set
/// range_start to 0, and insert_before to 10" (Web API reference) — so a
/// downward move encodes one past the target, the end case landing on
/// length ("append").
fn move_op(from: usize, to: usize) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::MOV);
    let mut mov = p4::Mov::new();
    mov.from_index = Some(clamp_index(from));
    mov.length = Some(1);
    mov.to_index = Some(clamp_index(insert_before_index(from, to)));
    op.mov = protobuf::MessageField::some(mov);
    op
}

/// Converts a final-position target (`to` indexes the resulting list) into
/// the MOV wire's insert-before index over the pre-move list: moving down
/// skips the slot the move itself vacates. Moving up needs no adjustment.
fn insert_before_index(from: usize, to: usize) -> usize {
    if to > from { to.saturating_add(1) } else { to }
}

/// UPDATE_LIST_ATTRIBUTES op setting the playlist name.
fn rename_op(name: &str) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::UPDATE_LIST_ATTRIBUTES);
    let mut attributes = p4::ListAttributes::new();
    attributes.name = Some(name.to_owned());
    let mut partial = p4::ListAttributesPartialState::new();
    partial.values = protobuf::MessageField::some(attributes);
    let mut update = p4::UpdateListAttributes::new();
    update.new_attributes = protobuf::MessageField::some(partial);
    op.update_list_attributes = protobuf::MessageField::some(update);
    op
}

/// Rootlist ADD op: places an existing playlist at the TOP of the library.
///
/// The wire can say this directly. `Add` carries four mutually exclusive
/// position fields — `from_index` (1), `add_last` (4), `add_first` (5),
/// `add_before_item` (6), `add_after_item` (7) — so "at the top" is one
/// boolean, exactly as "at the end" is. That matters because the rootlist
/// order is the account's, not this client's: the official app also puts a
/// newly created playlist first, and a client-side sort would leave the two
/// disagreeing the moment you opened Spotify proper.
///
/// `add_first` is preferred over `from_index = 0`, which expresses the same
/// intent as an absolute index and so races any concurrent rootlist edit
/// between our revision read and the server's apply; the boolean has no
/// index to go stale. Track ADDs keep `add_last` — appending is what adding
/// a song to a playlist means (see [`add_tracks_op`]).
fn rootlist_add_op(uri: &str) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::ADD);
    let mut add = p4::Add::new();
    add.add_first = Some(true);
    add.items = vec![item(uri)];
    op.add = protobuf::MessageField::some(add);
    op
}

/// Rootlist REM op keyed by the playlist URI (unfollow).
fn rootlist_remove_op(uri: &str) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::REM);
    let mut rem = p4::Rem::new();
    rem.items_as_key = Some(true);
    rem.items = vec![item(uri)];
    op.rem = protobuf::MessageField::some(rem);
    op
}

fn clamp_index(index: usize) -> i32 {
    i32::try_from(index).unwrap_or(i32::MAX)
}

// ---------------------------------------------------------------------------
// rootlist folders: pure planners over a snapshot (unit-tested)
// ---------------------------------------------------------------------------

const START_GROUP: &str = "spotify:start-group:";
const END_GROUP: &str = "spotify:end-group:";

/// One rootlist row as a snapshot reads it: its URI and the creation
/// timestamp its item attributes carry (milliseconds), which a rename keeps.
#[derive(Clone, Debug, PartialEq)]
struct RootItem {
    uri: String,
    timestamp: Option<i64>,
}

fn group_start_id(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix(START_GROUP)?;
    Some(rest.split_once(':').map_or(rest, |(id, _)| id))
}

fn group_end_id(uri: &str) -> Option<&str> {
    uri.strip_prefix(END_GROUP)
}

/// The start marker's URI: the name rides in it, form-encoded the way the
/// official client writes it (`New+Folder`, `Old%3A+80s`).
fn folder_start_uri(id: &str, name: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
    format!("{START_GROUP}{id}:{encoded}")
}

fn folder_end_uri(id: &str) -> String {
    format!("{END_GROUP}{id}")
}

fn valid_folder_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn folder_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        Err("folder name must not be empty".to_owned())
    } else {
        Ok(name)
    }
}

/// The folder's start and end marker indexes (inclusive), matched by depth
/// so a folder nested inside it cannot close it early.
fn folder_span(items: &[RootItem], id: &str) -> Result<(usize, usize), String> {
    let start = items
        .iter()
        .position(|item| group_start_id(&item.uri) == Some(id))
        .ok_or_else(|| "That folder is no longer in your library.".to_owned())?;
    let mut depth = 0usize;
    for (index, item) in items.iter().enumerate().skip(start) {
        if group_start_id(&item.uri).is_some() {
            depth += 1;
        } else if let Some(end) = group_end_id(&item.uri) {
            depth -= 1;
            if depth == 0 {
                return if end == id {
                    Ok((start, index))
                } else {
                    Err("Your library's folders are out of order; reload it and try again.".to_owned())
                };
            }
        }
    }
    Err("Your library's folders are out of order; reload it and try again.".to_owned())
}

fn is_playlist(uri: &str, id: &str) -> bool {
    uri.strip_prefix("spotify:playlist:") == Some(id)
        || uri.rsplit_once(":playlist:").is_some_and(|(_, tail)| tail == id)
}

/// Where a library row sits in the snapshot: its first index and how many
/// items move with it (a folder brings its whole span).
fn item_span(
    items: &[RootItem],
    item: &renderer_engine::protocol::LibraryItem,
) -> Result<(usize, usize), String> {
    use renderer_engine::protocol::LibraryItem;
    match item {
        LibraryItem::Playlist { id } => items
            .iter()
            .position(|row| is_playlist(&row.uri, id))
            .map(|index| (index, 1))
            .ok_or_else(|| "That playlist is no longer in your library.".to_owned()),
        LibraryItem::Folder { id } => {
            folder_span(items, id).map(|(start, end)| (start, end - start + 1))
        }
    }
}

/// The start-marker index of every folder that contains `index`, outermost
/// first; empty at the top level.
fn ancestors(items: &[RootItem], index: usize) -> Vec<usize> {
    let mut open = Vec::new();
    for (position, item) in items.iter().enumerate().take(index) {
        if group_start_id(&item.uri).is_some() {
            open.push(position);
        } else if group_end_id(&item.uri).is_some() {
            open.pop();
        }
    }
    open
}

/// A fresh folder id the snapshot does not use yet: 16 hex digits, as the
/// official client mints them. `RandomState` is seeded from the OS per
/// process, which is all the randomness a per-account id needs.
fn new_folder_id(items: &[RootItem]) -> String {
    use std::hash::{BuildHasher, Hasher};
    loop {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0),
        );
        let id = format!("{:016x}", hasher.finish());
        if !items.iter().any(|item| group_start_id(&item.uri) == Some(id.as_str())) {
            return id;
        }
    }
}

fn root_item(uri: String, timestamp: Option<i64>) -> p4::Item {
    let mut item = p4::Item::new();
    item.uri = Some(uri);
    if let Some(timestamp) = timestamp {
        let mut attributes = p4::ItemAttributes::new();
        attributes.timestamp = Some(timestamp);
        item.attributes = protobuf::MessageField::some(attributes);
    }
    item
}

/// Positional ADD of `items` before index `at`.
fn add_at_op(at: usize, items: Vec<p4::Item>) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::ADD);
    let mut add = p4::Add::new();
    add.from_index = Some(clamp_index(at));
    add.items = items;
    op.add = protobuf::MessageField::some(add);
    op
}

/// Positional REM of `length` items from `from`, naming them when given.
fn remove_range_op(from: usize, length: usize, items: Vec<p4::Item>) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::REM);
    let mut rem = p4::Rem::new();
    rem.from_index = Some(clamp_index(from));
    rem.length = Some(clamp_index(length));
    rem.items = items;
    op.rem = protobuf::MessageField::some(rem);
    op
}

/// Positional MOV of a block, `to` being an insert-before index in the
/// pre-move list (see the module notes on MOV).
fn move_block_op(from: usize, length: usize, to: usize) -> p4::Op {
    let mut op = p4::Op::new();
    op.set_kind(p4::op::Kind::MOV);
    let mut mov = p4::Mov::new();
    mov.from_index = Some(clamp_index(from));
    mov.length = Some(clamp_index(length));
    mov.to_index = Some(clamp_index(to));
    op.mov = protobuf::MessageField::some(mov);
    op
}

/// A new, empty folder at the top of the library or of `parent`.
fn create_folder_ops(
    items: &[RootItem],
    id: &str,
    name: &str,
    parent: Option<&str>,
    now_ms: i64,
) -> Result<Vec<p4::Op>, String> {
    let name = folder_name(name)?;
    if !valid_folder_id(id) || items.iter().any(|item| group_start_id(&item.uri) == Some(id)) {
        return Err("could not mint a new folder id".to_owned());
    }
    let at = match parent {
        Some(parent) => folder_span(items, parent)?.0 + 1,
        None => 0,
    };
    Ok(vec![add_at_op(
        at,
        vec![
            root_item(folder_start_uri(id, name), Some(now_ms)),
            root_item(folder_end_uri(id), Some(now_ms)),
        ],
    )])
}

/// The start marker replaced in place; the end marker is left alone.
fn rename_folder_ops(items: &[RootItem], id: &str, name: &str) -> Result<Vec<p4::Op>, String> {
    let name = folder_name(name)?;
    let (start, _) = folder_span(items, id)?;
    let uri = folder_start_uri(id, name);
    if items[start].uri == uri {
        return Ok(Vec::new());
    }
    Ok(vec![
        remove_range_op(start, 1, Vec::new()),
        add_at_op(start, vec![root_item(uri, items[start].timestamp)]),
    ])
}

/// The folder's whole span, contents included, as one REM that names every
/// item it removes.
fn delete_folder_ops(items: &[RootItem], id: &str) -> Result<Vec<p4::Op>, String> {
    let (start, end) = folder_span(items, id)?;
    let removed = items[start..=end]
        .iter()
        .map(|item| root_item(item.uri.clone(), None))
        .collect();
    Ok(vec![remove_range_op(start, end - start + 1, removed)])
}

/// Moves a playlist or a folder to the top of `folder`, or out of every
/// folder — to just after the top-level folder it was in, so it stays where
/// it was in the account's own order. Already there is no op at all.
fn move_item_ops(
    items: &[RootItem],
    item: &renderer_engine::protocol::LibraryItem,
    folder: Option<&str>,
) -> Result<Vec<p4::Op>, String> {
    let (from, length) = item_span(items, item)?;
    let parents = ancestors(items, from);
    let to = match folder {
        Some(folder) => {
            let (start, _) = folder_span(items, folder)?;
            if (from..from + length).contains(&start) {
                return Err("A folder can't go inside itself.".to_owned());
            }
            if parents.last() == Some(&start) {
                return Ok(Vec::new());
            }
            start + 1
        }
        None => {
            let Some(&outermost) = parents.first() else {
                return Ok(Vec::new());
            };
            let outer = group_start_id(&items[outermost].uri).unwrap_or_default();
            folder_span(items, outer)?.1 + 1
        }
    };
    Ok(vec![move_block_op(from, length, to)])
}

/// Wraps ops into a `ListChanges` request against `base_revision`, asking for
/// the resulting revision (the response's `SelectedListContent` then carries
/// the new checksum).
fn list_changes(base_revision: &[u8], ops: Vec<p4::Op>) -> p4::ListChanges {
    let mut changes = p4::ListChanges::new();
    changes.base_revision = Some(base_revision.to_vec());
    let mut delta = p4::Delta::new();
    delta.ops = ops;
    changes.deltas.push(delta);
    changes.want_resulting_revisions = Some(true);
    changes
}

fn change_info(session: &Session) -> p4::ChangeInfo {
    let mut info = p4::ChangeInfo::new();
    info.user = Some(session.username().to_owned());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    info.timestamp = Some(i64::try_from(now).unwrap_or(i64::MAX));
    let mut source = p4::SourceInfo::new();
    source.set_client(p4::source_info::Client::CLIENT);
    source.app = Some("spotifine".to_owned());
    info.source = protobuf::MessageField::some(source);
    info
}

// ---------------------------------------------------------------------------
// session-backed operations
// ---------------------------------------------------------------------------

fn playlist_uri(id: &str) -> Result<SpotifyUri, String> {
    SpotifyUri::from_uri(&format!("spotify:playlist:{id}"))
        .map_err(|error| format!("invalid playlist id: {error}"))
}

/// Fetches the playlist's current revision bytes (the playlist4 checksum).
async fn playlist_revision(session: &Session, id: &str) -> Result<Vec<u8>, String> {
    let uri = playlist_uri(id)?;
    let playlist = Playlist::get(session, &uri)
        .await
        .map_err(|error| format!("playlist fetch failed: {error}"))?;
    Ok(playlist.revision)
}

/// Raw rootlist response: only the top-level revision (base64 protobuf-JSON
/// bytes mapping) is needed for rootlist change requests.
#[derive(Default, Deserialize)]
struct RootlistJson {
    #[serde(default)]
    revision: Option<String>,
}

/// Fetches the rootlist revision bytes (decoded from the JSON base64).
async fn rootlist_revision(session: &Session) -> Result<Vec<u8>, String> {
    let endpoint = format!(
        "/playlist/v2/user/{user}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from=0&length=1",
        user = session.username(),
    );
    let body = session
        .spclient()
        .request_as_json(&Method::GET, &endpoint, None, None)
        .await
        .map_err(|error| format!("rootlist request failed: {error}"))?;
    let parsed: RootlistJson = serde_json::from_slice(&body)
        .map_err(|error| format!("unparseable rootlist response: {error}"))?;
    let revision = parsed
        .revision
        .ok_or_else(|| "the rootlist response carries no revision".to_owned())?;
    base64::engine::general_purpose::STANDARD
        .decode(revision)
        .map_err(|error| format!("rootlist revision is not valid base64: {error}"))
}

/// Applies ops to a playlist via `/playlist/v2/playlist/{id}/changes`.
async fn post_playlist_changes(
    session: &Session,
    id: &str,
    ops: Vec<p4::Op>,
    expected_snapshot_id: Option<&str>,
) -> Result<(), String> {
    let revision = playlist_revision(session, id).await?;
    if let Some(expected) = expected_snapshot_id {
        require_reviewed_revision(&revision, expected)?;
    }
    post_playlist_ops(session, id, &revision, ops).await
}

/// Posts edits against the same source snapshot that was checked.
async fn post_playlist_ops(
    session: &Session,
    id: &str,
    revision: &[u8],
    ops: Vec<p4::Op>,
) -> Result<(), String> {
    let mut changes = list_changes(revision, ops);
    if let Some(delta) = changes.deltas.first_mut() {
        delta.info = protobuf::MessageField::some(change_info(session));
    }
    let endpoint = format!("/playlist/v2/playlist/{id}/changes");
    let body = session
        .spclient()
        .request_with_protobuf(&Method::POST, &endpoint, None, &changes)
        .await
        .map_err(|error| format!("playlist change failed: {error}"))?;
    // The response is the resulting playlist content; parsing it validates
    // that the server answered with a well-formed result (the resulting
    // revision is available here for diagnostics).
    p4::SelectedListContent::parse_from_bytes(&body)
        .map_err(|error| format!("unparseable playlist change response: {error}"))?;
    Ok(())
}

fn require_reviewed_revision(revision: &[u8], expected: &str) -> Result<(), String> {
    let matches = !revision.is_empty()
        && expected.len() == revision.len() * 2
        && expected.as_bytes().chunks_exact(2).zip(revision).all(|(pair, byte)| {
            let digit = |value: u8| (value as char).to_digit(16);
            match (digit(pair[0]), digit(pair[1])) {
                (Some(high), Some(low)) => ((high << 4) | low) as u8 == *byte,
                _ => false,
            }
        });
    if matches {
        Ok(())
    } else {
        Err("This playlist changed since your preview. Reload it and review the matches before removing songs.".to_owned())
    }
}

/// One page of the rootlist as items only: the list revision, its length,
/// and each row's URI and creation timestamp.
#[derive(Deserialize)]
struct RootlistPageJson {
    revision: String,
    #[serde(default)]
    length: usize,
    #[serde(default)]
    contents: RootlistPageContents,
}

#[derive(Default, Deserialize)]
struct RootlistPageContents {
    #[serde(default)]
    items: Vec<RootlistPageItem>,
}

#[derive(Deserialize)]
struct RootlistPageItem {
    uri: String,
    #[serde(default)]
    attributes: Option<serde_json::Value>,
}

const ROOTLIST_PAGE: usize = 1000;
const ROOTLIST_MAX_PAGES: usize = 20;

fn parse_rootlist_page(payload: &[u8]) -> Result<(Vec<u8>, usize, Vec<RootItem>), String> {
    let page: RootlistPageJson = serde_json::from_slice(payload)
        .map_err(|error| format!("unparseable rootlist response: {error}"))?;
    let revision = base64::engine::general_purpose::STANDARD
        .decode(&page.revision)
        .map_err(|error| format!("rootlist revision is not valid base64: {error}"))?;
    let items = page
        .contents
        .items
        .into_iter()
        .map(|item| RootItem {
            timestamp: item.attributes.as_ref().and_then(|attributes| {
                let value = attributes.get("timestamp")?;
                value.as_i64().or_else(|| value.as_str()?.parse().ok())
            }),
            uri: item.uri,
        })
        .collect();
    Ok((revision, page.length, items))
}

/// The whole rootlist as a flat list at one revision. Positional ops are
/// only meaningful against exactly the list they were computed from, so a
/// list that changes between pages is read again rather than stitched.
async fn rootlist_snapshot(session: &Session) -> Result<(Vec<u8>, Vec<RootItem>), String> {
    'read: for _ in 0..3 {
        let mut revision = Vec::new();
        let mut items = Vec::new();
        for _ in 0..ROOTLIST_MAX_PAGES {
            let endpoint = format!(
                "/playlist/v2/user/{user}/rootlist?decorate=revision&from={from}&length={ROOTLIST_PAGE}",
                user = session.username(),
                from = items.len(),
            );
            let body = session
                .spclient()
                .request_as_json(&Method::GET, &endpoint, None, None)
                .await
                .map_err(|error| format!("rootlist request failed: {error}"))?;
            let (page_revision, length, page) = parse_rootlist_page(&body)?;
            if items.is_empty() {
                revision = page_revision;
            } else if page_revision != revision {
                continue 'read;
            }
            let received = page.len();
            items.extend(page);
            if items.len() >= length || received == 0 {
                return Ok((revision, items));
            }
        }
        return Err("your library is too large to edit its folders".to_owned());
    }
    Err("your library kept changing while it was read; try again".to_owned())
}

/// Applies ops to the user's rootlist via
/// `/playlist/v2/user/{user}/rootlist/changes`, on the latest revision.
async fn post_rootlist_changes(session: &Session, ops: Vec<p4::Op>) -> Result<(), String> {
    let revision = rootlist_revision(session).await?;
    post_rootlist_ops(session, &revision, ops).await
}

/// Applies ops computed against the snapshot at `revision`.
async fn post_rootlist_ops(session: &Session, revision: &[u8], ops: Vec<p4::Op>) -> Result<(), String> {
    if ops.is_empty() {
        return Ok(());
    }
    let mut changes = list_changes(revision, ops);
    if let Some(delta) = changes.deltas.first_mut() {
        delta.info = protobuf::MessageField::some(change_info(session));
    }
    let endpoint = format!(
        "/playlist/v2/user/{user}/rootlist/changes",
        user = session.username(),
    );
    let body = session
        .spclient()
        .request_with_protobuf(&Method::POST, &endpoint, None, &changes)
        .await
        .map_err(|error| format!("rootlist change failed: {error}"))?;
    p4::SelectedListContent::parse_from_bytes(&body)
        .map_err(|error| format!("unparseable rootlist change response: {error}"))?;
    Ok(())
}

fn validate_track_uris(uris: &[String]) -> Result<(), String> {
    for uri in uris {
        let parsed = SpotifyUri::from_uri(uri)
            .map_err(|error| format!("invalid Spotify track URI '{uri}': {error}"))?;
        if !matches!(&parsed, SpotifyUri::Track { .. }) {
            return Err(format!(
                "playlist edit item is not a Spotify track URI: {uri}"
            ));
        }
    }
    Ok(())
}

/// Creates a playlist named `name` and adds it to the user's library.
pub async fn create_playlist(session: &Session, name: &str) -> Result<PlaylistRef, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("playlist name must not be empty".to_owned());
    }
    let mut request = p4::ListUpdateRequest::new();
    let mut attributes = p4::ListAttributes::new();
    attributes.name = Some(name.to_owned());
    request.attributes = protobuf::MessageField::some(attributes);
    request.info = protobuf::MessageField::some(change_info(session));
    let body = session
        .spclient()
        .request_with_protobuf(&Method::POST, "/playlist/v2/playlist", None, &request)
        .await
        .map_err(|error| format!("playlist creation failed: {error}"))?;
    let reply = p4::CreateListReply::parse_from_bytes(&body)
        .map_err(|error| format!("unparseable playlist creation response: {error}"))?;
    let uri = reply
        .uri
        .ok_or_else(|| "the server returned no playlist URI".to_owned())?;
    let parsed = SpotifyUri::from_uri(&uri)
        .map_err(|error| format!("the server returned an invalid playlist URI: {error}"))?;
    let SpotifyUri::Playlist { id, .. } = &parsed else {
        return Err(format!("the server returned a non-playlist URI: {uri}"));
    };
    let id = id.to_base62().unwrap_or_default();
    // Place the new playlist at the TOP of the library, which is where the
    // official client puts it and where the person who just named it expects
    // to find it. This is a real rootlist ADD with `add_first`, not a local
    // sort, so Spotify's own app shows the same first row. If it fails the
    // playlist exists server-side but is missing from the rootlist; the
    // error lets the UI refresh instead of silently showing nothing.
    post_rootlist_changes(session, vec![rootlist_add_op(&uri)]).await?;
    Ok(PlaylistRef {
        id,
        uri,
        name: name.to_owned(),
        revision: reply
            .revision
            .as_deref()
            .filter(|bytes| !bytes.is_empty())
            .map(crate::browse::hex),
        description: None,
        owner_id: session.username(),
        owner_name: String::new(),
        cover_url: None,
        cover_urls: Vec::new(),
        track_count: Some(0),
    })
}

/// Renames a playlist via an UPDATE_LIST_ATTRIBUTES change.
pub async fn rename_playlist(session: &Session, id: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("playlist name must not be empty".to_owned());
    }
    post_playlist_changes(session, id, vec![rename_op(name)], None).await
}

/// Unfollows (removes) a playlist from the user's rootlist.
pub async fn delete_playlist(session: &Session, id: &str) -> Result<(), String> {
    let uri = format!("spotify:playlist:{id}");
    post_rootlist_changes(session, vec![rootlist_remove_op(&uri)]).await
}

/// Adds someone else's playlist back to the library, at the top.
pub async fn follow_playlist(session: &Session, id: &str) -> Result<(), String> {
    playlist_uri(id)?;
    post_rootlist_changes(session, vec![rootlist_add_op(&format!("spotify:playlist:{id}"))]).await
}

/// Creates an empty folder and answers with its id.
pub async fn create_folder(
    session: &Session,
    name: &str,
    parent: Option<&str>,
) -> Result<String, String> {
    let (revision, items) = rootlist_snapshot(session).await?;
    let id = new_folder_id(&items);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let ops = create_folder_ops(&items, &id, name, parent, now)?;
    post_rootlist_ops(session, &revision, ops).await?;
    Ok(id)
}

pub async fn rename_folder(session: &Session, id: &str, name: &str) -> Result<(), String> {
    let (revision, items) = rootlist_snapshot(session).await?;
    let ops = rename_folder_ops(&items, id, name)?;
    post_rootlist_ops(session, &revision, ops).await
}

pub async fn delete_folder(session: &Session, id: &str) -> Result<(), String> {
    let (revision, items) = rootlist_snapshot(session).await?;
    let ops = delete_folder_ops(&items, id)?;
    post_rootlist_ops(session, &revision, ops).await
}

pub async fn move_library_item(
    session: &Session,
    item: &renderer_engine::protocol::LibraryItem,
    folder: Option<&str>,
) -> Result<(), String> {
    let (revision, items) = rootlist_snapshot(session).await?;
    let ops = move_item_ops(&items, item, folder)?;
    post_rootlist_ops(session, &revision, ops).await
}

/// Appends tracks to a playlist via an ADD change.
pub async fn add_tracks(session: &Session, id: &str, uris: &[String]) -> Result<(), String> {
    if uris.is_empty() {
        return Err("no tracks to add".to_owned());
    }
    validate_track_uris(uris)?;
    post_playlist_changes(session, id, vec![add_tracks_op(uris)], None).await
}

/// Removes tracks by URI via a REM change (`items_as_key`).
pub async fn remove_tracks(
    session: &Session,
    id: &str,
    uris: &[String],
    expected_snapshot_id: Option<&str>,
) -> Result<(), String> {
    if uris.is_empty() {
        return Err("no tracks to remove".to_owned());
    }
    validate_track_uris(uris)?;
    post_playlist_changes(session, id, vec![remove_tracks_op(uris)], expected_snapshot_id).await
}

/// Moves one track so that it lands at index `to` of the resulting list.
///
/// Callers speak final positions — the same numbers the UI's optimistic
/// splice produces — and [`move_op`] converts once into the wire's
/// insert-before form, so no caller ever sees both coordinate systems.
pub async fn reorder_tracks(
    session: &Session,
    id: &str,
    from: usize,
    to: usize,
    row_count: usize,
) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    let uri = playlist_uri(id)?;
    let playlist = Playlist::get(session, &uri)
        .await
        .map_err(|error| format!("playlist fetch failed: {error}"))?;
    require_complete_reorder(usize::try_from(playlist.length).ok(), row_count, from, to)?;
    post_playlist_ops(session, id, &playlist.revision, vec![move_op(from, to)]).await
}

fn require_complete_reorder(
    source_count: Option<usize>,
    row_count: usize,
    from: usize,
    to: usize,
) -> Result<(), String> {
    if source_count != Some(row_count) || from >= row_count || to >= row_count {
        return Err("Reordering is unavailable because not every playlist item is shown. Refresh the playlist and try again.".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use protobuf::Message;

    use super::*;

    #[test]
    fn positional_reorder_rejects_omitted_source_rows_before_building_a_move() {
        assert!(require_complete_reorder(Some(4), 3, 1, 0).is_err());
        assert!(require_complete_reorder(None, 3, 1, 0).is_err());
        assert!(require_complete_reorder(Some(3), 3, 3, 0).is_err());
        assert!(require_complete_reorder(Some(3), 3, 1, 3).is_err());
        assert!(require_complete_reorder(Some(3), 3, 1, 0).is_ok());
        assert!(require_complete_reorder(Some(3), 3, 0, 2).is_ok());
    }

    #[test]
    fn reviewed_removal_rejects_changed_or_missing_revision() {
        let revision = [0x01, 0xab, 0xff];
        assert!(require_reviewed_revision(&revision, "01abff").is_ok());
        assert!(require_reviewed_revision(&revision, "01ABFF").is_ok());
        for stale in ["01abfe", "", "01ab", "01abffff", "01abzz"] {
            assert!(require_reviewed_revision(&revision, stale).is_err());
        }
        assert!(require_reviewed_revision(&[], "").is_err());
    }

    /// Round-trips an op through protobuf encoding and asserts the interesting
    /// fields, proving the builder wires the official-client field numbers.
    fn round_trip(op: &p4::Op) -> p4::Op {
        p4::Op::parse_from_bytes(&op.write_to_bytes().unwrap()).unwrap()
    }

    #[test]
    fn add_tracks_op_appends_uris_at_the_end() {
        let op = round_trip(&add_tracks_op(&[
            "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            "spotify:track:1abcdefghijklmnopqrstu".to_owned(),
        ]));
        assert_eq!(op.kind(), p4::op::Kind::ADD);
        let add = op.add.unwrap();
        assert_eq!(add.add_last, Some(true));
        let uris: Vec<&str> = add
            .items
            .iter()
            .map(|item| item.uri.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            uris,
            vec![
                "spotify:track:0123456789ABCDEFGHIJKL",
                "spotify:track:1abcdefghijklmnopqrstu"
            ]
        );
    }

    #[test]
    fn remove_tracks_op_keys_items_by_uri() {
        let op = round_trip(&remove_tracks_op(&[
            "spotify:track:0123456789ABCDEFGHIJKL".to_owned()
        ]));
        assert_eq!(op.kind(), p4::op::Kind::REM);
        let rem = op.rem.unwrap();
        assert_eq!(rem.items_as_key, Some(true));
        assert_eq!(rem.items.len(), 1);
        assert_eq!(
            rem.items[0].uri.as_deref(),
            Some("spotify:track:0123456789ABCDEFGHIJKL")
        );
    }

    #[test]
    fn move_op_carries_web_api_insert_before_semantics() {
        // Moving up passes the final index through untouched.
        let op = round_trip(&move_op(3, 1));
        assert_eq!(op.kind(), p4::op::Kind::MOV);
        let mov = op.mov.unwrap();
        assert_eq!(mov.from_index, Some(3));
        assert_eq!(mov.length, Some(1));
        assert_eq!(mov.to_index, Some(1));

        // Moving down encodes one past the target: item 2 to final index 5
        // inserts before pre-move index 6, skipping the vacated slot.
        let down = round_trip(&move_op(2, 5));
        let mov = down.mov.unwrap();
        assert_eq!(mov.from_index, Some(2));
        assert_eq!(mov.to_index, Some(6));
    }

    #[test]
    fn downward_moves_convert_to_insert_before_positions() {
        // [A,B,C] dragged to the end posts to_index 3 == length ("append").
        assert_eq!(insert_before_index(0, 2), 3);
        // Landing mid-list skips the vacated slot: A to final index 1 of
        // [A,B,C] inserts before C.
        assert_eq!(insert_before_index(0, 1), 2);
        // Moving up passes through untouched.
        assert_eq!(insert_before_index(3, 1), 1);
        assert_eq!(insert_before_index(1, 0), 0);
    }

    #[test]
    fn rename_op_sets_the_name_attribute() {
        let op = round_trip(&rename_op("Road Trip"));
        assert_eq!(op.kind(), p4::op::Kind::UPDATE_LIST_ATTRIBUTES);
        let update = op.update_list_attributes.unwrap();
        let partial = update.new_attributes.unwrap();
        assert!(update.old_attributes.is_none());
        let values = partial.values.unwrap();
        assert_eq!(values.name.as_deref(), Some("Road Trip"));
        assert!(partial.no_value.is_empty());
    }

    #[test]
    fn rootlist_ops_add_and_remove_by_uri() {
        let add = round_trip(&rootlist_add_op("spotify:playlist:0123456789ABCDEFGHIJKL"));
        assert_eq!(add.kind(), p4::op::Kind::ADD);
        let add_body = add.add.unwrap();
        assert_eq!(
            add_body.items[0].uri.as_deref(),
            Some("spotify:playlist:0123456789ABCDEFGHIJKL")
        );
        assert_eq!(add_body.add_first, Some(true));

        let rem = round_trip(&rootlist_remove_op(
            "spotify:playlist:0123456789ABCDEFGHIJKL",
        ));
        assert_eq!(rem.kind(), p4::op::Kind::REM);
        let rem_body = rem.rem.unwrap();
        assert_eq!(rem_body.items_as_key, Some(true));
        assert_eq!(
            rem_body.items[0].uri.as_deref(),
            Some("spotify:playlist:0123456789ABCDEFGHIJKL")
        );
    }

    /// A new playlist belongs at the top of the library, and the two ADD
    /// builders must not drift into each other: the rootlist one says
    /// "first", the track one says "last", and neither may carry both
    /// position flags — the server would have to pick one, and which one is
    /// not ours to guess.
    #[test]
    fn rootlist_add_places_the_playlist_first_while_track_add_appends() {
        let rootlist = round_trip(&rootlist_add_op("spotify:playlist:0123456789ABCDEFGHIJKL"))
            .add
            .unwrap();
        assert_eq!(rootlist.add_first, Some(true));
        assert!(
            rootlist.add_last.is_none(),
            "a rootlist ADD asking for both ends is ambiguous on the wire"
        );
        // Unset, not zero: an absolute index would be read against whatever
        // revision the server applies, which is not the one we fetched.
        assert!(rootlist.from_index.is_none());
        assert!(rootlist.add_before_item.is_none());
        assert!(rootlist.add_after_item.is_none());

        let tracks = round_trip(&add_tracks_op(&[
            "spotify:track:0123456789ABCDEFGHIJKL".to_owned()
        ]))
        .add
        .unwrap();
        assert_eq!(tracks.add_last, Some(true));
        assert!(tracks.add_first.is_none());
    }

    #[test]
    fn list_changes_wraps_ops_with_the_base_revision() {
        let base = vec![0xde, 0xad, 0xbe, 0xef];
        let changes = list_changes(
            &base,
            vec![add_tracks_op(&[
                "spotify:track:2abcdefghijklmnopqrstu".to_owned()
            ])],
        );
        assert_eq!(changes.base_revision.as_deref(), Some(&base[..]));
        assert_eq!(changes.want_resulting_revisions, Some(true));
        assert_eq!(changes.deltas.len(), 1);
        assert_eq!(changes.deltas[0].ops.len(), 1);
        assert_eq!(changes.deltas[0].ops[0].kind(), p4::op::Kind::ADD);
        assert!(
            changes.deltas[0].info.is_none(),
            "info is attached per-session"
        );
    }

    #[test]
    fn track_uri_validation_rejects_non_tracks() {
        assert!(validate_track_uris(&["spotify:track:0123456789ABCDEFGHIJKL".to_owned()]).is_ok());
        assert!(validate_track_uris(&["spotify:album:0123456789ABCDEFGHIJKL".to_owned()]).is_err());
        assert!(validate_track_uris(&["not-a-uri".to_owned()]).is_err());
        assert!(validate_track_uris(&[]).is_ok());
    }

    // -- rootlist folders -------------------------------------------------

    use renderer_engine::protocol::LibraryItem;

    const OUTER: &str = "0123456789abcdef";
    const INNER: &str = "fedcba9876543210";
    const OTHER: &str = "00000000000000aa";

    fn row(uri: &str) -> RootItem {
        RootItem { uri: uri.to_owned(), timestamp: Some(1_665_495_078_416) }
    }

    fn playlist(id: &str) -> String {
        format!("spotify:playlist:{id}")
    }

    /// [0] A
    /// [1] start OUTER "Road Trips"
    /// [2]   B
    /// [3]   start INNER "Old: 80s"
    /// [4]     C
    /// [5]   end INNER
    /// [6] end OUTER
    /// [7] start OTHER "Empty"
    /// [8] end OTHER
    /// [9] D
    fn library() -> Vec<RootItem> {
        vec![
            row(&playlist("A")),
            row(&format!("spotify:start-group:{OUTER}:Road+Trips")),
            row(&playlist("B")),
            row(&format!("spotify:start-group:{INNER}:Old%3A+80s")),
            row(&playlist("C")),
            row(&format!("spotify:end-group:{INNER}")),
            row(&format!("spotify:end-group:{OUTER}")),
            row(&format!("spotify:start-group:{OTHER}:Empty")),
            row(&format!("spotify:end-group:{OTHER}")),
            row(&playlist("D")),
        ]
    }

    fn uris(items: &[p4::Item]) -> Vec<&str> {
        items.iter().map(|item| item.uri.as_deref().unwrap_or_default()).collect()
    }

    fn folder(id: &str) -> LibraryItem {
        LibraryItem::Folder { id: id.to_owned() }
    }

    fn list(id: &str) -> LibraryItem {
        LibraryItem::Playlist { id: id.to_owned() }
    }

    fn only(ops: Vec<p4::Op>) -> p4::Op {
        assert_eq!(ops.len(), 1, "one op expected");
        round_trip(&ops[0])
    }

    #[test]
    fn folder_markers_encode_the_name_as_the_official_client_does() {
        assert_eq!(folder_start_uri(OUTER, "New Folder"), format!("spotify:start-group:{OUTER}:New+Folder"));
        assert_eq!(folder_start_uri(OUTER, "Old: 80s & more"), format!("spotify:start-group:{OUTER}:Old%3A+80s+%26+more"));
        assert_eq!(folder_end_uri(OUTER), format!("spotify:end-group:{OUTER}"));
        assert_eq!(group_start_id(&folder_start_uri(OUTER, "x")), Some(OUTER));
        assert_eq!(group_end_id(&folder_end_uri(OUTER)), Some(OUTER));
    }

    #[test]
    fn folder_spans_match_by_depth_and_reject_broken_hierarchies() {
        let items = library();
        assert_eq!(folder_span(&items, OUTER), Ok((1, 6)));
        assert_eq!(folder_span(&items, INNER), Ok((3, 5)));
        assert_eq!(folder_span(&items, OTHER), Ok((7, 8)));
        assert!(folder_span(&items, "ffffffffffffffff").is_err());
        let mut broken = items.clone();
        broken.remove(5);
        assert!(folder_span(&broken, INNER).is_err(), "an end marker that is not this folder's");
        assert_eq!(ancestors(&items, 4), vec![1, 3]);
        assert_eq!(ancestors(&items, 2), vec![1]);
        assert!(ancestors(&items, 9).is_empty());
    }

    #[test]
    fn a_new_folder_is_both_markers_in_one_positional_add() {
        let items = library();
        let at_top = only(create_folder_ops(&items, "1111111111111111", " Gym ", None, 42).unwrap());
        assert_eq!(at_top.kind(), p4::op::Kind::ADD);
        let add = at_top.add.unwrap();
        assert_eq!(add.from_index, Some(0));
        assert!(add.add_first.is_none() && add.add_last.is_none());
        assert_eq!(
            uris(&add.items),
            vec!["spotify:start-group:1111111111111111:Gym", "spotify:end-group:1111111111111111"]
        );
        assert!(add.items.iter().all(|item| item.attributes.timestamp == Some(42)));

        // Nested: right after the parent's start marker, at the top of it.
        let nested = only(create_folder_ops(&items, "1111111111111111", "Gym", Some(INNER), 42).unwrap());
        assert_eq!(nested.add.unwrap().from_index, Some(4));

        assert!(create_folder_ops(&items, "1111111111111111", "  ", None, 42).is_err());
        assert!(create_folder_ops(&items, OUTER, "Again", None, 42).is_err(), "an id already in use");
        assert!(create_folder_ops(&items, "not-hex", "Gym", None, 42).is_err());
        assert!(create_folder_ops(&items, "1111111111111111", "Gym", Some("ffffffffffffffff"), 42).is_err());
    }

    #[test]
    fn minted_folder_ids_are_sixteen_hex_digits_and_unused() {
        let items = library();
        let id = new_folder_id(&items);
        assert!(valid_folder_id(&id), "{id}");
        assert!(folder_span(&items, &id).is_err());
    }

    #[test]
    fn a_rename_replaces_only_the_start_marker_and_keeps_its_timestamp() {
        let items = library();
        let ops = rename_folder_ops(&items, INNER, "Synthwave").unwrap();
        assert_eq!(ops.len(), 2, "one delta, REM then ADD");
        let rem = round_trip(&ops[0]);
        assert_eq!(rem.kind(), p4::op::Kind::REM);
        let rem = rem.rem.unwrap();
        assert_eq!((rem.from_index, rem.length), (Some(3), Some(1)));
        assert!(rem.items.is_empty() && rem.items_as_key.is_none());
        let add = round_trip(&ops[1]);
        assert_eq!(add.kind(), p4::op::Kind::ADD);
        let add = add.add.unwrap();
        assert_eq!(add.from_index, Some(3));
        assert_eq!(uris(&add.items), vec![format!("spotify:start-group:{INNER}:Synthwave").as_str()]);
        assert_eq!(add.items[0].attributes.timestamp, Some(1_665_495_078_416));

        assert!(rename_folder_ops(&items, INNER, "Old: 80s").unwrap().is_empty(), "same name, no write");
        assert!(rename_folder_ops(&items, INNER, "").is_err());
        assert!(rename_folder_ops(&items, "ffffffffffffffff", "x").is_err());
    }

    #[test]
    fn deleting_a_folder_removes_its_whole_span_by_index() {
        let items = library();
        let op = only(delete_folder_ops(&items, OUTER).unwrap());
        assert_eq!(op.kind(), p4::op::Kind::REM);
        let rem = op.rem.unwrap();
        assert_eq!((rem.from_index, rem.length), (Some(1), Some(6)));
        assert!(rem.items_as_key.is_none(), "positional, not keyed");
        assert_eq!(
            uris(&rem.items),
            items[1..=6].iter().map(|item| item.uri.as_str()).collect::<Vec<_>>()
        );
        let empty = only(delete_folder_ops(&items, OTHER).unwrap()).rem.unwrap();
        assert_eq!((empty.from_index, empty.length), (Some(7), Some(2)));
    }

    #[test]
    fn moves_into_folders_land_at_the_top_of_them() {
        let items = library();
        let mov = |ops: Vec<p4::Op>| {
            let op = only(ops);
            assert_eq!(op.kind(), p4::op::Kind::MOV);
            let mov = op.mov.unwrap();
            (mov.from_index.unwrap(), mov.length.unwrap(), mov.to_index.unwrap())
        };
        // D (9) up into OUTER: insert before 2.
        assert_eq!(mov(move_item_ops(&items, &list("D"), Some(OUTER)).unwrap()), (9, 1, 2));
        // A (0) down into the empty folder: insert before 8, its end marker.
        assert_eq!(mov(move_item_ops(&items, &list("A"), Some(OTHER)).unwrap()), (0, 1, 8));
        // A nested move: B into INNER.
        assert_eq!(mov(move_item_ops(&items, &list("B"), Some(INNER)).unwrap()), (2, 1, 4));
        // A whole folder, span and all: OTHER into INNER.
        assert_eq!(mov(move_item_ops(&items, &folder(OTHER), Some(INNER)).unwrap()), (7, 2, 4));
        // INNER (3..=5) into OTHER, moving down past its old parent's end.
        assert_eq!(mov(move_item_ops(&items, &folder(INNER), Some(OTHER)).unwrap()), (3, 3, 8));
    }

    #[test]
    fn moving_out_of_folders_lands_after_the_top_level_folder() {
        let items = library();
        let op = only(move_item_ops(&items, &list("C"), None).unwrap()).mov.unwrap();
        assert_eq!((op.from_index, op.length, op.to_index), (Some(4), Some(1), Some(7)));
        let op = only(move_item_ops(&items, &folder(INNER), None).unwrap()).mov.unwrap();
        assert_eq!((op.from_index, op.length, op.to_index), (Some(3), Some(3), Some(7)));
    }

    #[test]
    fn moves_that_change_nothing_or_cannot_exist_are_refused_before_any_write() {
        let items = library();
        assert!(move_item_ops(&items, &list("A"), None).unwrap().is_empty(), "already at the top level");
        assert!(move_item_ops(&items, &list("C"), Some(INNER)).unwrap().is_empty(), "already in it");
        assert!(move_item_ops(&items, &folder(OUTER), Some(OUTER)).is_err(), "into itself");
        assert!(move_item_ops(&items, &folder(OUTER), Some(INNER)).is_err(), "into its own child");
        assert!(move_item_ops(&items, &list("Z"), Some(OUTER)).is_err(), "not in the library");
        assert!(move_item_ops(&items, &list("A"), Some("ffffffffffffffff")).is_err());
    }

    #[test]
    fn a_rootlist_page_reads_revision_length_uris_and_timestamps() {
        let body = br#"{"revision":"AAAAELqqrKuzaoeUKYP7gEzCzrx3h0rD","length":4,"attributes":{},"contents":{"pos":0,"truncated":false,"items":[
            {"uri":"spotify:start-group:123456789abcdefa:Abablagan","attributes":{"timestamp":"1665495078416","seenAt":"0","public":false}},
            {"uri":"spotify:end-group:123456789abcdefa","attributes":{"timestamp":"1665495078416"}},
            {"uri":"spotify:playlist:5aNzxEEkRE9MgNkiuXmpOR"},
            {"uri":"spotify:playlist:3FKTkhbClLGgKdPpbx3aHy","attributes":{"timestamp":1665486908663}}],
            "metaItems":[{},{},{"length":0},{"length":1}]}}"#;
        let (revision, length, items) = parse_rootlist_page(body).unwrap();
        assert_eq!(revision.len(), 24);
        assert_eq!(length, 4);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].timestamp, Some(1_665_495_078_416));
        assert_eq!(items[2].timestamp, None);
        assert_eq!(items[3].timestamp, Some(1_665_486_908_663));
        assert_eq!(folder_span(&items, "123456789abcdefa"), Ok((0, 1)));
    }

}
