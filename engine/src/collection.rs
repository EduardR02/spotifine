//! The podcasts the signed-in account follows ("saved shows").
//!
//! Transport: `POST /collection/v2/paging` over spclient, the collection
//! service the official desktop client syncs every library set through. The
//! session authenticates it with its own bearer and client token, so no
//! developer app is involved — the same constraint the followed-artists read
//! (`follow`) is built under.
//!
//! What the official client says about this service, read from its own
//! binary (Spotify 1.303 for Windows): the endpoints `collection/v2/paging`,
//! `collection/v2/delta` and `collection/v2/write`; the `PageRequest`/
//! `PageResponse` messages of `collection2v2.proto` (the descriptor librespot
//! extracted from Spotify 1.2.52, vendored in librespot-protocol but not
//! compiled by it); and the media type
//! `application/vnd.collection-v2.spotify.proto` for both directions. Saved
//! podcasts are the set named `show`; its rows are `spotify:show:` URIs, each
//! with the second it was added. Verified live against the owner's account
//! (2026-10-06): the set answered with exactly the shows the Web API's
//! `me/shows` had listed. Liked Songs is the `collection` set of the same
//! service, which this engine reads through the context resolver instead.
//!
//! The two messages are four scalar fields each, so they are encoded and
//! decoded here with the protobuf crate's wire streams rather than by
//! vendoring a schema librespot does not build.
//!
//! Order: newest first by `added_at`, the order the Web API's `me/shows`
//! answered in, so the rail reads the same as it did. The whole set is read
//! at once — a few pages for even a large library — because ordering a set
//! by the time each row was added needs every row.

use http::Method;
use http::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use librespot_core::Session;
use protobuf::{CodedInputStream, CodedOutputStream};

use renderer_engine::protocol::ShowRef;

const COLLECTION_PROTO: &str = "application/vnd.collection-v2.spotify.proto";
const SHOW_SET: &str = "show";
const SHOW_URI_PREFIX: &str = "spotify:show:";
const PAGE_LIMIT: i32 = 300;
/// A ceiling on pages, not on shows: the service hands back a token while it
/// has more, and a token that never ends must not loop for ever.
const MAX_PAGES: usize = 20;

// ---------------------------------------------------------------------------
// pure wire shaping (unit-tested)
// ---------------------------------------------------------------------------

/// `spotify.collection.proto.v2.PageRequest`: username = 1, set = 2,
/// pagination_token = 3, limit = 4.
fn page_request(username: &str, set: &str, token: &str, limit: i32) -> Vec<u8> {
    let mut body = Vec::new();
    {
        let mut out = CodedOutputStream::vec(&mut body);
        // Writing into a Vec cannot fail.
        let _ = out.write_string(1, username);
        let _ = out.write_string(2, set);
        if !token.is_empty() {
            let _ = out.write_string(3, token);
        }
        let _ = out.write_int32(4, limit);
        let _ = out.flush();
    }
    body
}

#[derive(Debug, Default, PartialEq)]
struct CollectionItem {
    uri: String,
    added_at: i32,
    is_removed: bool,
}

#[derive(Debug, Default)]
struct PageResponse {
    items: Vec<CollectionItem>,
    next_page_token: String,
}

fn parse_item(payload: &[u8]) -> protobuf::Result<CollectionItem> {
    let mut item = CollectionItem::default();
    let mut input = CodedInputStream::from_bytes(payload);
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            10 => item.uri = input.read_string()?,
            16 => item.added_at = input.read_int32()?,
            24 => item.is_removed = input.read_bool()?,
            other => protobuf::rt::skip_field_for_tag(other, &mut input)?,
        }
    }
    Ok(item)
}

/// `PageResponse`: items = 1 (repeated `CollectionItem`), next_page_token = 2,
/// sync_token = 3 (not needed by a one-shot read).
fn parse_page(payload: &[u8]) -> Result<PageResponse, String> {
    let parse = || -> protobuf::Result<PageResponse> {
        let mut page = PageResponse::default();
        let mut input = CodedInputStream::from_bytes(payload);
        while let Some(tag) = input.read_raw_tag_or_eof()? {
            match tag {
                10 => page.items.push(parse_item(&input.read_bytes()?)?),
                18 => page.next_page_token = input.read_string()?,
                other => protobuf::rt::skip_field_for_tag(other, &mut input)?,
            }
        }
        Ok(page)
    };
    parse().map_err(|error| format!("unparseable saved podcasts page: {error}"))
}

/// The saved show URIs, newest first, without removed rows, repeats or
/// anything that is not a show.
fn saved_show_uris(mut items: Vec<CollectionItem>) -> Vec<String> {
    items.retain(|item| {
        !item.is_removed
            && item
                .uri
                .strip_prefix(SHOW_URI_PREFIX)
                .is_some_and(|id| !id.is_empty())
    });
    items.sort_by(|left, right| right.added_at.cmp(&left.added_at));
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    items
        .into_iter()
        .filter_map(|item| seen.insert(item.uri.clone()).then_some(item.uri))
        .collect()
}

// ---------------------------------------------------------------------------
// transport
// ---------------------------------------------------------------------------

async fn read_set(session: &Session, set: &str) -> Result<Vec<CollectionItem>, String> {
    let username = session.username();
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(COLLECTION_PROTO));
    headers.insert(ACCEPT, HeaderValue::from_static(COLLECTION_PROTO));
    let mut items = Vec::new();
    let mut token = String::new();
    for _ in 0..MAX_PAGES {
        let body = page_request(&username, set, &token, PAGE_LIMIT);
        let payload = crate::browse::spclient_read(|| async {
            session
                .spclient()
                .request(&Method::POST, "/collection/v2/paging", Some(headers.clone()), Some(&body))
                .await
        })
        .await
        .map_err(|error| format!("saved podcasts request failed: {error}"))?;
        let page = parse_page(&payload)?;
        items.extend(page.items);
        if page.next_page_token.is_empty() {
            return Ok(items);
        }
        token = page.next_page_token;
    }
    Ok(items)
}

/// Every podcast the account follows, newest first, with name, publisher and
/// artwork resolved in metadata batches.
pub async fn saved_shows(session: &Session) -> Result<Vec<ShowRef>, String> {
    let uris = saved_show_uris(read_set(session, SHOW_SET).await?);
    crate::browse::library_shows(session, &uris).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_item(uri: &str, added_at: i32, removed: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut out = CodedOutputStream::vec(&mut bytes);
        out.write_string(1, uri).unwrap();
        out.write_int32(2, added_at).unwrap();
        if removed {
            out.write_bool(3, true).unwrap();
        }
        // An optional context_uri the read does not need.
        out.write_string(4, "spotify:app:collection").unwrap();
        out.flush().unwrap();
        drop(out);
        bytes
    }

    #[test]
    fn the_page_request_carries_the_collection_v2_fields() {
        let body = page_request("owner", "show", "", 300);
        let mut input = CodedInputStream::from_bytes(&body);
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(10));
        assert_eq!(input.read_string().unwrap(), "owner");
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(18));
        assert_eq!(input.read_string().unwrap(), "show");
        // No token on the first page: field 3 is absent, not empty.
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(32));
        assert_eq!(input.read_int32().unwrap(), 300);
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), None);

        let next = page_request("owner", "show", "cursor", 300);
        let mut input = CodedInputStream::from_bytes(&next);
        input.read_raw_tag_or_eof().unwrap();
        input.read_string().unwrap();
        input.read_raw_tag_or_eof().unwrap();
        input.read_string().unwrap();
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(26));
        assert_eq!(input.read_string().unwrap(), "cursor");
    }

    #[test]
    fn a_page_parses_items_and_the_next_token_and_skips_unknown_fields() {
        let mut payload = Vec::new();
        {
            let mut out = CodedOutputStream::vec(&mut payload);
            out.write_bytes(1, &encode_item("spotify:show:aaaaaaaaaaaaaaaaaaaaaa", 100, false)).unwrap();
            out.write_bytes(1, &encode_item("spotify:show:bbbbbbbbbbbbbbbbbbbbbb", 200, true)).unwrap();
            out.write_string(2, "next").unwrap();
            out.write_string(3, "sync").unwrap();
            out.flush().unwrap();
        }
        let page = parse_page(&payload).unwrap();
        assert_eq!(page.next_page_token, "next");
        assert_eq!(
            page.items,
            vec![
                CollectionItem { uri: "spotify:show:aaaaaaaaaaaaaaaaaaaaaa".into(), added_at: 100, is_removed: false },
                CollectionItem { uri: "spotify:show:bbbbbbbbbbbbbbbbbbbbbb".into(), added_at: 200, is_removed: true },
            ]
        );
        assert!(parse_page(&[0x0a, 0x05, 0x01]).is_err(), "a truncated frame is an error");
        assert!(parse_page(&[]).unwrap().items.is_empty(), "an empty set is an empty page");
    }

    #[test]
    fn saved_shows_are_newest_first_without_removed_repeated_or_foreign_rows() {
        let item = |uri: &str, added_at, is_removed| CollectionItem { uri: uri.into(), added_at, is_removed };
        let uris = saved_show_uris(vec![
            item("spotify:show:old", 10, false),
            item("spotify:show:new", 30, false),
            item("spotify:show:gone", 40, true),
            item("spotify:episode:notashow", 50, false),
            item("spotify:show:", 60, false),
            item("spotify:show:new", 20, false),
            item("spotify:show:mid", 20, false),
        ]);
        assert_eq!(uris, vec!["spotify:show:new", "spotify:show:mid", "spotify:show:old"]);
    }
}
