//! The artists the signed-in account follows.
//!
//! Transport: `GET /user-profile-view/v3/profile/{username}/following` over
//! spclient, which the session authenticates for us with its own bearer and
//! client token. No developer app, no second credential, no extra login —
//! which is the binding constraint on this whole feature.
//!
//! Two things about this endpoint were established by probing the owner's
//! live account, and both contradict what this module used to say:
//!
//! - The collection holds ARTISTS, not users. An older comment here claimed
//!   `/following` was the set of *users* you follow and used that claim to
//!   justify a different transport entirely. It is wrong: the live response
//!   is `{"profiles": [{"uri": "spotify:artist:…", "name", "image_url",
//!   "followers_count", "is_following"}]}`. The uri is nevertheless the field
//!   the id is derived from, and entries are filtered on the artist prefix,
//!   because a profile collection could plausibly carry `spotify:user:` rows
//!   and a user rendered as an artist tile would open a page that does not
//!   exist.
//! - `image_url` is a plain https URL, not the `spotify:image:` /
//!   `spotify:mosaic:` form the metadata surfaces in this codebase return.
//!   It is passed through as-is.
//!
//! Why not Spotify's public Web API (`api.spotify.com/v1/me/following`),
//! which this module was originally built on: it does not work and cannot be
//! made to. That edge requires a token issued through the accounts OAuth flow
//! and bound to a registered application; the login5 bearer is minted for
//! Spotify's internal services and carries no client registration, so the
//! request is refused. It is refused as **429 Too Many Requests**, which is
//! the trap — the previous implementation mapped that to "Spotify is
//! rate-limiting this account" and so reported a permanent, structural
//! refusal as a transient one. Registering an application is out of scope, so
//! that transport is a dead end for this project.
//!
//! There is no write path here, and adding one is not a small job. Following
//! an artist is not a social-graph operation: `socialgraph/v2/following` and
//! `socialgraphv2.proto` (symbols `followUser`, `followUsers`,
//! `FollowedUsers`) are the *user* follow graph. Following an ARTIST is a
//! collection write — `collection/v2/write`, with
//! `collection/artist_collection_state.proto` — and both services speak
//! protobuf, which is why every `format=json` probe against them answered
//! 400. librespot 0.8 ships neither schema, so a write would mean vendoring
//! and maintaining descriptors for an undocumented internal service. Until
//! that is worth doing, this app reads who you follow and the official client
//! is where you change it.
//!
//! Paging: none. Nine entries came back in one response with no cursor field
//! of any kind, and the request carries no page parameter that was observed
//! to do anything. Inventing a cursor that cannot be verified would be a
//! guess dressed as a contract, so the read is one round trip and the whole
//! collection is whatever that answers with. If a large account turns out to
//! be truncated, the fix is to find out how this service actually pages —
//! not to add an `after=` and hope.

use http::Method;
use librespot_core::Session;
use serde::Deserialize;

use spotifine_common::protocol::ArtistRef;


const ARTIST_URI_PREFIX: &str = "spotify:artist:";

// ---------------------------------------------------------------------------
// pure response shaping (unit-tested)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct FollowingResponse {
    #[serde(default)]
    profiles: Vec<FollowedProfile>,
}

/// One entry as the service returns it. Every field is optional at the parse
/// level: an artist whose name or picture the service omitted is still an
/// artist you follow, and dropping the whole collection over one thin row
/// would be a worse answer than a row with a blank name.
///
/// `followers_count` and `is_following` are deliberately not read. The first
/// has nowhere to go — nothing this list renders is a follower count — and
/// the second is `true` for every row of a collection defined as "who you
/// follow", so testing it would be theatre.
#[derive(Debug, Default, Deserialize)]
struct FollowedProfile {
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    image_url: String,
}

/// Parses the followed-profiles response into artists.
///
/// The id comes from the uri rather than a field of its own, because the
/// service supplies no separate id — and that is also the filter: anything
/// that is not `spotify:artist:<id>` is dropped rather than rendered as an
/// artist tile that opens nothing.
fn parse_following(payload: &[u8]) -> Result<Vec<ArtistRef>, String> {
    let parsed: FollowingResponse = serde_json::from_slice(payload)
        .map_err(|error| format!("unparseable followed-artists response: {error}"))?;
    Ok(parsed
        .profiles
        .into_iter()
        .filter_map(|profile| {
            let id = profile.uri.strip_prefix(ARTIST_URI_PREFIX)?.trim();
            if id.is_empty() {
                return None;
            }
            Some(ArtistRef {
                id: id.to_owned(),
                uri: profile.uri.clone(),
                name: profile.name,
                // Already an https URL at this endpoint, unlike the
                // `spotify:image:` ids the metadata surfaces return.
                portrait_url: Some(profile.image_url).filter(|url| !url.is_empty()),
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// transport
// ---------------------------------------------------------------------------

/// Every artist the signed-in account follows, in the service's own order.
pub async fn followed_artists(session: &Session) -> Result<Vec<ArtistRef>, String> {
    let endpoint = format!(
        "/user-profile-view/v3/profile/{user}/following",
        user = session.username(),
    );
    let payload = crate::browse::spclient_read(|| async {
        session.spclient().request(&Method::GET, &endpoint, None, None).await
    }).await
        .map_err(|error| format!("followed-artists request failed: {error}"))?;
    parse_following(&payload)
}

#[cfg(test)]
mod tests {
    use super::parse_following;

    /// The exact shape the live account returned, trimmed to two entries.
    #[test]
    fn the_live_response_shape_parses_into_artists() {
        let payload = br#"{
          "profiles": [
            {
              "uri": "spotify:artist:788qKGMEh4hfYUTy8yANRC",
              "name": "Conrad.",
              "image_url": "https://i.scdn.co/image/ab676161000051741cee0c15e2748fdabccab112",
              "followers_count": 14954,
              "is_following": true
            },
            {
              "uri": "spotify:artist:0TnOYISbd1XYRBk9myaseg",
              "name": "Pitbull",
              "image_url": "https://i.scdn.co/image/ab6761610000517412345678",
              "followers_count": 12000000,
              "is_following": true
            }
          ]
        }"#;
        let artists = parse_following(payload).expect("the live shape parses");
        assert_eq!(artists.len(), 2);
        assert_eq!(artists[0].id, "788qKGMEh4hfYUTy8yANRC");
        assert_eq!(artists[0].uri, "spotify:artist:788qKGMEh4hfYUTy8yANRC");
        assert_eq!(artists[0].name, "Conrad.");
        assert_eq!(
            artists[0].portrait_url.as_deref(),
            Some("https://i.scdn.co/image/ab676161000051741cee0c15e2748fdabccab112"),
            "image_url is already an https URL at this endpoint",
        );
        assert_eq!(artists[1].id, "0TnOYISbd1XYRBk9myaseg");
    }

    #[test]
    fn non_artist_entries_are_dropped_rather_than_rendered() {
        // A user profile in this collection would open an artist page that
        // does not exist, so the uri prefix is the filter.
        let payload = br#"{
          "profiles": [
            {"uri": "spotify:user:someone", "name": "A Person", "image_url": ""},
            {"uri": "spotify:artist:ar1", "name": "Artist One", "image_url": "https://art"},
            {"uri": "", "name": "Nothing", "image_url": ""},
            {"uri": "spotify:artist:", "name": "Prefix only", "image_url": ""}
          ]
        }"#;
        let artists = parse_following(payload).expect("a mixed collection parses");
        assert_eq!(artists.len(), 1, "only the artist row survives");
        assert_eq!(artists[0].id, "ar1");
    }

    #[test]
    fn thin_entries_keep_their_row() {
        // Name and picture missing entirely: still an artist you follow, and
        // still navigable, which is the whole point of the row.
        let payload = br#"{"profiles":[{"uri":"spotify:artist:ar2"}]}"#;
        let artists = parse_following(payload).expect("a field-less entry parses");
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].id, "ar2");
        assert!(artists[0].name.is_empty());
        assert!(
            artists[0].portrait_url.is_none(),
            "an empty image_url is no picture, not an empty one",
        );
    }

    #[test]
    fn an_account_following_nobody_parses_as_an_empty_collection() {
        assert!(parse_following(br#"{"profiles":[]}"#).unwrap().is_empty());
        // The field itself has been observed absent on other profile routes.
        assert!(parse_following(br#"{}"#).unwrap().is_empty());
        // An error document is not a collection.
        assert!(parse_following(br#"not json"#).is_err());
    }
}
