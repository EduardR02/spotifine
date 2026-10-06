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
//! - Delete (unfollow) is a rootlist REM op keyed by the playlist URI.
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
    let mut changes = list_changes(&revision, ops);
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

/// Applies ops to the user's rootlist via
/// `/playlist/v2/user/{user}/rootlist/changes`.
async fn post_rootlist_changes(session: &Session, ops: Vec<p4::Op>) -> Result<(), String> {
    let revision = rootlist_revision(session).await?;
    let mut changes = list_changes(&revision, ops);
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
) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    post_playlist_changes(session, id, vec![move_op(from, to)], None).await
}

#[cfg(test)]
mod tests {
    use protobuf::Message;

    use super::*;

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

    #[test]
    fn reorder_index_validation_allows_only_real_moves() {
        assert_eq!(move_op(0, 0).mov.unwrap().to_index, Some(0));
    }
}
