// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `LibraryClient`: heart the track a zone is playing, and add it (or its
//! album) to the library.
//!
//! Connect with the Core's address and id (both things every Roon extension
//! already has), then use the zone ids you know from the official extension
//! API: they are the same strings.
//!
//! What this can do is limited on purpose: read what is playing, heart it,
//! un-heart it, add it to the library. No banning, no removing, no deleting.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Notify};

use crate::connection::{self, broker_id_from_core_id, ConnectError, DEFAULT_PORT};
use crate::graph::{InlineStruct, ObjectGraph, PropertyType, RoonObject, Value};
use crate::remoting::{Remoting, RemotingError, StructField};
use crate::wire::{Reader, Writer};

// The root service object every client asks for first; the Core answers by streaming its objects.
const ROOT_SERVICE_GUID: [u8; 16] = [
    0xbc, 0xd3, 0x6e, 0x84, 0x78, 0xa3, 0xe1, 0x11, 0xb2, 0x72, 0x5b, 0x4a, 0x61, 0x88, 0x70, 0x9b,
];

// The exact method declarations, as the Roon client sends them.
const SIG_FAVORITE_TRACK: &str = "Sooloos.Broker.Api.Library::FavoriteOrBan(System.Sooid, Sooloos.Broker.Api.TrackBase, Sooloos.Broker.Api.FavoriteBanState, Base.ResultCallback)";
const SIG_FAVORITE_ALBUMS: &str = "Sooloos.Broker.Api.Library::FavoriteOrBan(System.Sooid, System.Collections.Generic.IEnumerable<Sooloos.Broker.Api.AlbumBase>, Sooloos.Broker.Api.FavoriteBanState, Base.ResultCallback)";
const SIG_ADD_TRACK: &str = "Sooloos.Broker.Api.Library::AddToLibrary(Sooloos.Broker.Api.Profile, Sooloos.Broker.Api.TrackBase)";
const SIG_ADD_ALBUM: &str = "Sooloos.Broker.Api.Library::AddToLibrary(Sooloos.Broker.Api.Profile, Sooloos.Broker.Api.AlbumBase)";
const SIG_GET_TRACK_LITE: &str = "Sooloos.Broker.Api.Library::GetTrackLite(long, Base.ResultCallback<Sooloos.Broker.Api.TrackLite>)";
const SIG_ALBUM_TRACKS: &str = "Sooloos.Broker.Api.Library::GetAlbumTrackLites(Sooloos.Broker.Api.AlbumBase, Base.ResultCallback<System.Collections.Generic.IList<Sooloos.Broker.Api.TrackLite>>)";
const SIG_GET_ALBUM: &str = "Sooloos.Broker.Api.Library::GetAlbum(Sooloos.Broker.Api.AlbumBase, Base.ResultCallback<Sooloos.Broker.Api.Album>)";
const SIG_BROWSE_PERFORMER_ALBUMS: &str = "Sooloos.Broker.Api.Library::BrowsePerformerAlbums(Sooloos.Broker.Api.Profile, Sooloos.Broker.Api.PerformerLite, Sooloos.Broker.Api.AlbumBrowserSpec, Base.ResultCallback<Sooloos.Broker.Api.PerformerAlbumsBrowser>)";
const SIG_QOBUZ_MAIN_ALBUMS: &str = "Sooloos.Broker.Api.Qobuz::GetPerformerMainAlbumLites(long, Base.ResultCallback<System.Collections.Generic.IList<Sooloos.Broker.Api.AlbumLite>>)";
const SIG_TIDAL_MAIN_ALBUMS: &str = "Sooloos.Broker.Api.Tidal::GetPerformerMainAlbumLites(long, Base.ResultCallback<System.Collections.Generic.IList<Sooloos.Broker.Api.AlbumLite>>)";
const SIG_GROUP_LOAD: &str = "Sooloos.Broker.Api.PerformerAlbumsGroup::Load(int, int, Base.ResultCallback)";
const SIG_PLAY_TRACK: &str = "Sooloos.Broker.Api.Transport::PlayTrack(Sooloos.Broker.Api.Zone, System.Sooid, Sooloos.Broker.Api.PlayParameters, Sooloos.Broker.Api.TrackBase, Base.ResultCallback<Sooloos.Broker.Api.PlayFeedback>)";
const SIG_PLAY_ALBUM: &str = "Sooloos.Broker.Api.Transport::PlayAlbum(Sooloos.Broker.Api.Zone, System.Sooid, Sooloos.Broker.Api.PlayParameters, Sooloos.Broker.Api.AlbumBase, bool, bool, Base.ResultCallback<Sooloos.Broker.Api.PlayFeedback>)";
const SIG_PLAY_SWIM: &str = "Sooloos.Broker.Api.Transport::PlaySwim(Sooloos.Broker.Api.Zone, System.Sooid, Sooloos.Broker.Api.SwimParameters, Base.ResultCallback<Sooloos.Broker.Api.PlayFeedback>)";
const SIG_CREATE_PLAYLIST: &str = "Sooloos.Broker.Api.Playlists::CreatePlaylistFromMediaIds(Sooloos.Broker.Api.Profile, string, string, bool, System.Collections.Generic.IEnumerable<long>, Base.ResultCallback<Sooloos.Broker.Api.Playlist>)";
const SIG_DELETE_PLAYLIST: &str = "Sooloos.Broker.Api.Playlists::Delete(Sooloos.Broker.Api.Playlist, Base.ResultCallback)";
const SIG_RENAME_PLAYLIST: &str = "Sooloos.Broker.Api.Playlists::Rename(Sooloos.Broker.Api.Playlist, string, Base.ResultCallback)";
const SIG_PLAY_PLAYLIST: &str = "Sooloos.Broker.Api.Transport::PlayPlaylist(Sooloos.Broker.Api.Zone, System.Sooid, Sooloos.Broker.Api.PlayParameters, Sooloos.Broker.Api.Playlist, Sooloos.Broker.Api.PlaylistItem, Base.ResultCallback<Sooloos.Broker.Api.PlayFeedback>)";
/// Roon's Export: where library tracks' files are (the Core's path, and
/// where its web server gives them out).
const SIG_TRACK_EXPORT_INFO: &str = "Sooloos.Broker.Api.Library::GetTrackExportInfo(System.Collections.Generic.IEnumerable<long>, Base.ResultCallback<System.Collections.Generic.IList<Sooloos.Broker.Api.TrackExportInfo>>)";
const SIG_GET_ALBUM_LITE: &str = "Sooloos.Broker.Api.Library::GetAlbumLite(long, Base.ResultCallback<Sooloos.Broker.Api.AlbumLite>)";
const SIG_GET_PERFORMER_LITE: &str = "Sooloos.Broker.Api.Library::GetPerformerLite(long, Base.ResultCallback<Sooloos.Broker.Api.PerformerLite>)";
const SIG_UNIFIED_SEARCH: &str = "Sooloos.Broker.Api.Library::UnifiedSearch(Sooloos.Broker.Api.SearchParameters, Base.ResultCallback<Sooloos.Broker.Api.UnifiedSearchResults>)";
const SIG_GET_PERFORMER: &str = "Sooloos.Broker.Api.Library::GetPerformer(Sooloos.Broker.Api.PerformerBase, Base.ResultCallback<Sooloos.Broker.Api.Performer>)";
const SIG_SIMILAR_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxSimilarPerformers(Sooloos.Broker.Api.PerformerLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerWithExtras>>)";
const SIG_PERFORMER_TOP_TRACKS: &str = "Sooloos.Broker.Api.Library::GetOneBoxPerformerTopTracks(Sooloos.Broker.Api.PerformerLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.TrackLite>>)";
const SIG_GET_RECENTS: &str = "Sooloos.Broker.Api.Library::GetRecents(Sooloos.Broker.Api.Profile, string, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.RecentItem>>)";
const SIG_QUEUE_ITEMS: &str = "Sooloos.Broker.Api.Queue::GetItems(Base.ResultCallback<System.Collections.Generic.IList<Sooloos.Broker.Api.TransportItem>>)";
const SIG_QUEUE_REMOVE_ALL: &str = "Sooloos.Broker.Api.Queue::RemoveAll()";
const SIG_QUEUE_PLAY_FROM_ID: &str = "Sooloos.Broker.Api.Queue::PlayFromHere(long)";
const SIG_PLAY_TRACKS: &str = "Sooloos.Broker.Api.Transport::PlayTracks(Sooloos.Broker.Api.Zone, System.Sooid, Sooloos.Broker.Api.PlayParameters, System.Collections.Generic.IEnumerable<Sooloos.Broker.Api.TrackBase>, Base.ResultCallback<Sooloos.Broker.Api.PlayFeedback>)";
const SIG_PERFORMER_RECOMMENDED: &str = "Sooloos.Broker.Api.Library::GetOneBoxPerformerRecommendedAlbums(Sooloos.Broker.Api.PerformerLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.PerformerRecommendedAlbums>)";
const SIG_SIMILAR_ALBUMS: &str = "Sooloos.Broker.Api.Library::GetOneBoxSimilarAlbums(Sooloos.Broker.Api.AlbumLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.OneBox_SimilarAlbums>)";
const SIG_ALBUM_RECOMMENDED: &str = "Sooloos.Broker.Api.Library::GetOneBoxAlbumRecommendedForYou(Sooloos.Broker.Api.AlbumLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.AlbumLite>>)";
const SIG_ALBUM_SIMILAR_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxAlbumSimilarPerformers(Sooloos.Broker.Api.AlbumLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerWithExtras>>)";
const SIG_RECOMMENDED_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxRecommendedPerformers(System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerWithExtras>>)";
const SIG_SHARE_PERFORMER: &str = "Sooloos.Broker.Api.Sharing::GetShareLink(System.Sooid, Sooloos.Broker.Api.PerformerBase, Base.ResultCallback<string>)";
const SIG_SHARE_ALBUM: &str = "Sooloos.Broker.Api.Sharing::GetShareLink(System.Sooid, Sooloos.Broker.Api.AlbumBase, Base.ResultCallback<string>)";
const SIG_PERFORMER_QUERY: &str = "Sooloos.Broker.Api.Library::VirtualPerformerQuery(System.Sooid, Sooloos.Broker.Api.PerformerQueryCriteria, Sooloos.Broker.Api.VirtualQueryParameters, Base.ResultCallback<Sooloos.Broker.Api.VirtualPerformerLiteQuery>)";
const SIG_PERFORMER_QUERY_RETAIN: &str = "Sooloos.Broker.Api.VirtualPerformerLiteQuery::RetainPage(int, Base.ResultCallback)";
const SIG_PERFORMER_QUERY_RELEASE: &str = "Sooloos.Broker.Api.VirtualPerformerLiteQuery::ReleasePage(int, Base.ResultCallback)";
const SIG_PERFORMER_QUERY_DISPOSE: &str = "Sooloos.Broker.Api.VirtualPerformerLiteQuery::Dispose()";
const SIG_ALBUM_QUERY: &str = "Sooloos.Broker.Api.Library::VirtualAlbumQuery(System.Sooid, Sooloos.Broker.Api.AlbumQueryCriteria, Sooloos.Broker.Api.VirtualQueryParameters, Base.ResultCallback<Sooloos.Broker.Api.VirtualAlbumLiteQuery>)";
const SIG_ALBUM_QUERY_RETAIN: &str = "Sooloos.Broker.Api.VirtualAlbumLiteQuery::RetainPage(int, Base.ResultCallback)";
const SIG_ALBUM_QUERY_RELEASE: &str = "Sooloos.Broker.Api.VirtualAlbumLiteQuery::ReleasePage(int, Base.ResultCallback)";
const SIG_ALBUM_QUERY_DISPOSE: &str = "Sooloos.Broker.Api.VirtualAlbumLiteQuery::Dispose()";
const SIG_TRACK_QUERY: &str = "Sooloos.Broker.Api.Library::VirtualTrackQuery(System.Sooid, Sooloos.Broker.Api.TrackQueryCriteria, Sooloos.Broker.Api.VirtualQueryParameters, Base.ResultCallback<Sooloos.Broker.Api.VirtualTrackLiteQuery>)";
const SIG_TRACK_QUERY_RETAIN: &str = "Sooloos.Broker.Api.VirtualTrackLiteQuery::RetainPage(int, Base.ResultCallback)";
const SIG_TRACK_QUERY_RELEASE: &str = "Sooloos.Broker.Api.VirtualTrackLiteQuery::ReleasePage(int, Base.ResultCallback)";
const SIG_TRACK_QUERY_DISPOSE: &str = "Sooloos.Broker.Api.VirtualTrackLiteQuery::Dispose()";
const SIG_PLAYLIST_ITEMS: &str = "Sooloos.Broker.Api.Playlist::GetItems(Base.ResultCallback<Sooloos.Broker.Api.Query<Sooloos.Broker.Api.PlaylistItem>>)";
const SIG_PLAYLIST_REMOVE_ITEMS: &str = "Sooloos.Broker.Api.Playlists::RemoveItems(Sooloos.Broker.Api.Playlist, System.Collections.Generic.IEnumerable<Sooloos.Broker.Api.PlaylistItem>)";
const SIG_ALL_TAGS: &str = "Sooloos.Broker.Api.Library::GetAllTagLites(Sooloos.Broker.Api.Profile, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.TagLite>>)";
const SIG_GET_TAG: &str = "Sooloos.Broker.Api.Library::GetTag(Sooloos.Broker.Api.Profile, Sooloos.Broker.Api.TagBase, Base.ResultCallback<Sooloos.Broker.Api.Tag>)";
const SIG_TAG_ITEMS: &str = "Sooloos.Broker.Api.ExplicitTag::GetItems(Sooloos.Broker.Api.TagItemQueryCriteria, Base.ResultCallback<Sooloos.Broker.Api.Query<Sooloos.Broker.Api.TagItem>>)";
const SIG_LISTEN_LATER: &str = "Sooloos.Broker.Api.Library::GetListenLater(Sooloos.Broker.Api.Profile, System.Collections.Generic.IList<string>, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.ListenLaterItem>>)";
const SIG_LISTEN_LATER_TRACK: &str = "Sooloos.Broker.Api.Library::SetListenLaterState(System.Sooid, Sooloos.Broker.Api.TrackBase, Sooloos.Broker.Api.ListenLaterState, Base.ResultCallback)";
const SIG_LISTEN_LATER_ALBUM: &str = "Sooloos.Broker.Api.Library::SetListenLaterState(System.Sooid, Sooloos.Broker.Api.AlbumBase, Sooloos.Broker.Api.ListenLaterState, Base.ResultCallback)";
const SIG_LISTEN_LATER_PERFORMER: &str = "Sooloos.Broker.Api.Library::SetListenLaterState(System.Sooid, Sooloos.Broker.Api.PerformerBase, Sooloos.Broker.Api.ListenLaterState, Base.ResultCallback)";
const SIG_HISTORY_QUERY: &str = "Sooloos.Broker.Api.Library::VirtualHistoryQuery(System.Sooid, Sooloos.Broker.Api.HistoryQueryCriteria, Sooloos.Broker.Api.VirtualQueryParameters, Base.ResultCallback<Sooloos.Broker.Api.VirtualHistoryPlayQuery>)";
const SIG_HISTORY_RETAIN: &str = "Sooloos.Broker.Api.VirtualHistoryPlayQuery::RetainPage(int, Base.ResultCallback)";
const SIG_HISTORY_RELEASE: &str = "Sooloos.Broker.Api.VirtualHistoryPlayQuery::ReleasePage(int, Base.ResultCallback)";
const SIG_HISTORY_DISPOSE: &str = "Sooloos.Broker.Api.VirtualHistoryPlayQuery::Dispose()";
const SIG_GENRE_TOP_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxGenreTopPerformers(string, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerLite>>)";
const SIG_GENRE_RECOMMENDED_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxGenreRecommendedPerformers(string, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerWithExtras>>)";
const SIG_GENRE_TOP_ALBUMS: &str = "Sooloos.Broker.Api.Library::GetOneBoxGenreTopAlbums(string, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.AlbumLite>>)";
const SIG_GENRE_RECOMMENDED_ALBUMS: &str = "Sooloos.Broker.Api.Library::GetOneBoxGenreRecommendedAlbums(string, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.AlbumWithExtras>>)";
const SIG_GENRE_NEW_RELEASES: &str = "Sooloos.Broker.Api.Library::GetOneBoxGenreNewReleases(string, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.AlbumWithExtras>>)";

const FAVORITE: u32 = 1;
const NOT_FAVORITE: u32 = 0;

/// Types whose pushes can change what we report for a zone.
const WATCHED_TYPES: &[&str] = &[
    ".Zone",
    ".TransportItem",
    ".TransportTrack",
    ".TrackLite",
    ".AlbumLite",
    ".Profile",
    ".DataList<Sooloos.Broker.Api.TransportTrack>",
    // The signal path, for the audio quality
    ".SignalPath",
    ".SignalPathElement",
    ".SourceSignalPathElement",
    ".DataList<Sooloos.Broker.Api.SignalPathElement>",
    // The waveform of what's playing
    ".DataList<int>",
];

/// Pushes arrive in bursts; changes are reported once they settle.
const RECOMPUTE_DELAY: Duration = Duration::from_millis(60);

#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// The Core's address.
    pub host: String,
    /// The internal protocol's port. Default 9332.
    pub port: u16,
    /// The Core id, e.g. "fafb763c-9ad0-4f07-887d-44e19b8374e0".
    pub core_id: String,
    /// How long to give the Core to send its objects after connecting. Default 3 s.
    pub settle: Duration,
    /// How long a call may wait for the Core's answer. Default 15 s.
    pub request_timeout: Duration,
    /// How long to wait for the Core to confirm a change (heart, add). Default 8 s.
    pub confirm_timeout: Duration,
}

impl ClientOptions {
    pub fn new(host: impl Into<String>, core_id: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: DEFAULT_PORT,
            core_id: core_id.into(),
            settle: Duration::from_secs(3),
            request_timeout: Duration::from_secs(15),
            confirm_timeout: Duration::from_secs(8),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddMode {
    /// Just this track.
    Track,
    /// The whole album (what Roon's own "+" does).
    Album,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumInfo {
    pub title: String,
    /// Roon's id for the album across the Core ("RoonAlbumId").
    pub roon_album_id: Option<String>,
    pub in_library: bool,
    pub favorite: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    pub zone_id: String,
    pub title: String,
    /// Roon's id for the playing track object ("TrackId").
    pub track_id: Option<String>,
    /// The library's id for this track, once it is in the library.
    pub library_track_id: Option<String>,
    pub in_library: bool,
    pub favorite: bool,
    pub banned: bool,
    /// "local" for files, "streaming" for TIDAL/Qobuz/KKBOX, None if unknown.
    pub source: Option<&'static str>,
    pub album: Option<AlbumInfo>,
    /// What the zone is actually playing, from the first step of Roon's
    /// signal path. None while nothing plays (Roon clears the path when paused).
    pub quality: Option<Box<AudioQuality>>,
    /// The whole track's waveform as Roon draws it in its seek bar: about
    /// 2000 loudness values, 0 to 255, start to end. None if Roon has none
    /// (radio, a track it hasn't analyzed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waveform: Option<Vec<u8>>,
}

/// The source step of a zone's signal path: the stream or file as Roon
/// receives it, before any DSP or conversion.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioQuality {
    /// "low" (lossy), "cd" (lossless up to 16-bit / 48 kHz) or "hires"
    /// (lossless above that, and DSD).
    pub class: &'static str,
    /// As Roon names it: "Flac", "Mp3", "Aac", "Dsf", ...
    pub format: Option<String>,
    pub bits_per_sample: Option<u32>,
    pub sample_rate: Option<u32>,
    /// Lossy streams only, in kbps.
    pub bit_rate: Option<u32>,
    /// 2 for stereo.
    pub channels: Option<u32>,
}

/// One track of the playing album (see `album_tracks`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumTrack {
    /// Names this track to `set_track_favorite` / `add_track`; valid for this session only.
    pub handle: String,
    pub title: String,
    pub track_number: Option<i32>,
    pub media_number: Option<i32>,
    pub length_seconds: Option<i32>,
    pub in_library: bool,
    pub favorite: bool,
    /// True for the track the zone is playing right now.
    pub playing: bool,
    /// The Core's ids (durable), to match against a zone's track later.
    pub track_id: Option<u64>,
    pub library_track_id: Option<u64>,
}

/// How to play something: now (the queue cleared, as Roon's Play Now),
/// next (after the playing song), or at the end of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayHow {
    #[default]
    Now,
    Next,
    Queue,
}

/// One album of the playing artist's discography (see `artist_albums`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistAlbum {
    /// Names this album to `album_tracks_for`, `set_album_favorite_for` and
    /// `add_album_for`; valid for this session only.
    pub handle: String,
    pub title: String,
    /// The album artist as Roon shows it (credits markup stripped).
    pub artist: String,
    /// "main", "other" (singles, EPs, live...) or "appearance", as Roon groups the discography.
    pub group: String,
    pub in_library: bool,
    pub favorite: bool,
    /// True for the album the zone is playing right now.
    pub playing: bool,
    /// The Core's album id (durable, unlike the handle), when known.
    pub album_id: Option<u64>,
    /// The release year, when Roon knows it.
    pub year: Option<u32>,
    /// The release date as "YYYY-MM-DD" (month and day 00 when Roon only
    /// knows the year), for sorting and the cards.
    pub released: Option<String>,
    /// Roon's popularity score for the album, when the Core sends one.
    pub popularity: Option<f64>,
    /// The cover's image key for the Core's image service, when there is one.
    pub image_key: Option<String>,
}

/// An album as a page header or a search hit (see `album_card`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumCard {
    /// Names this album to `album_tracks_for`, `set_album_favorite_for`,
    /// `add_album_for` and `play_album`; valid for this session only.
    pub handle: String,
    pub album_id: Option<u64>,
    pub title: String,
    /// The album artist as Roon shows it (credits markup stripped).
    pub artist: String,
    /// The main performer's object (for `artist_albums_for`), when the Core sent it.
    pub performer_handle: Option<String>,
    pub performer_id: Option<u64>,
    pub year: Option<u32>,
    pub image_key: Option<String>,
    pub in_library: bool,
    pub favorite: bool,
    /// "local", "qobuz", "tidal", "streaming" or "other".
    pub source: String,
}

/// An artist as a page header or a search hit (see `artist_card`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistCard {
    /// Names this artist to `artist_albums_for`; valid for this session only.
    pub handle: String,
    pub performer_id: Option<u64>,
    pub name: String,
    pub image_key: Option<String>,
    pub favorite: bool,
    /// How many albums the artist is the main performer of, as Roon counts them.
    pub album_count: Option<i32>,
}

/// What Roon's artist page shows besides the discography: similar artists
/// (Roon's "Similar to"), and the albums it recommends, the latest ones and
/// the ones the artist appears on (see `artist_extras`).
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArtistExtras {
    pub similar: Vec<ArtistCard>,
    pub recommended: Vec<AlbumCard>,
    pub latest: Vec<AlbumCard>,
    pub appears_on: Vec<AlbumCard>,
}

/// What Roon's album page shows besides the tracks: similar albums (with
/// Roon's word for the likeness), albums recommended from this one, and
/// similar artists (see `album_extras`).
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AlbumExtras {
    pub similar: Vec<AlbumCard>,
    pub similarity: Option<String>,
    pub recommended: Vec<AlbumCard>,
    pub similar_artists: Vec<ArtistCard>,
}

/// What Roon's metadata says about an artist or album beyond the card:
/// its genres (Roon's own list, as deep as its provider goes), and for an
/// artist where it is from (see `artist_facts`, `album_facts`).
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    pub genres: Vec<String>,
    pub country: Option<String>,
    pub from: Option<String>,
    pub active_since: Option<u32>,
}

/// What Roon's genre page shows: its top and recommended artists, its top
/// and recommended albums, and new releases (see `genre_page`).
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenrePage {
    pub name: String,
    pub top_artists: Vec<ArtistCard>,
    pub recommended_artists: Vec<ArtistCard>,
    pub top_albums: Vec<AlbumCard>,
    pub recommended_albums: Vec<AlbumCard>,
    pub new_releases: Vec<AlbumCard>,
}

/// What a "one box" call is about (see `LibraryClient::one_box`).
#[derive(Debug, Clone, Copy)]
enum Subject<'a> {
    /// The profile alone
    None,
    /// An object in the graph (an album or performer handle)
    Oid(u64),
    /// A name (a genre)
    Name(&'a str),
}

/// One play from Roon's history (see `history`). The handles are good for
/// this session; the ids (album, track, performer) are the Core's and keep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Play {
    /// When it was played, as Unix milliseconds.
    pub played_at: i64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_handle: Option<String>,
    pub album_id: Option<u64>,
    pub track_handle: Option<String>,
    pub track_id: Option<u64>,
    pub performer_handle: Option<String>,
    pub performer_id: Option<u64>,
    pub image_key: Option<String>,
    pub seconds_played: Option<i32>,
    pub length_seconds: Option<i32>,
}

/// Roon's play history: how many plays it holds, and the newest `limit`.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub total: u32,
    pub plays: Vec<Play>,
    /// What the read saw, for the history log (counts per step).
    pub debug: String,
}

/// Where a library track's file is, as Roon's Export finds it (see
/// `track_files`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackFile {
    /// The file as the Core knows it: a path on the Core's own machine, or
    /// a share as the Core reaches it
    /// ("/roon/sys/storage/smbmounts/RoonStorage_.../Bölzer/2016 - Hero/03 - Hero.flac").
    pub file_path: String,
    /// Where the Core's web server (the port its extensions connect to,
    /// 9330 as a rule) gives the file out ("/file/<storage id>/<path>.flac").
    pub http_path: String,
}

/// A track found by `search`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackHit {
    /// Names this track to `play_track`, `set_track_favorite` and `add_track`.
    pub handle: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_handle: Option<String>,
    pub image_key: Option<String>,
    pub length_seconds: Option<i32>,
    pub in_library: bool,
    pub favorite: bool,
}

/// What to start Roon Radio from (session handles from the pages), whether
/// to stay inside the library, and whether to play only the seeds' own music
/// (see `play_radio`). Roon's radio (its "swim type") takes one artist, or
/// several only with `limit_to_seed`: then the Core's own radio queues every
/// song of those artists it finds, shuffled (31 artists gave 245 songs).
/// Several artists without it fail on the Core ("Failed to create swim
/// session: InvalidArgument", logged on the Core only; the zone just stops).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RadioSeeds {
    pub tracks: Vec<String>,
    pub albums: Vec<String>,
    pub artists: Vec<String>,
    /// Genre names, as Roon spells them.
    pub genres: Vec<String>,
    pub library_only: bool,
    pub limit_to_seed: bool,
}

/// One of an artist's top songs as Roon ranks them (see `top_tracks`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopTrack {
    pub handle: String,
    /// Roon's track id (as the history and the queue know the song).
    pub track_id: Option<String>,
    pub title: String,
    pub album: String,
    /// Tells the song's albums apart (the album id, else its title).
    pub album_key: String,
    /// The album's artist as Roon credits it ("HEALTH" for a collaboration
    /// on HEALTH's album, whoever else is on the song).
    pub album_artist: String,
    pub in_library: bool,
    /// A live recording: Roon's IsLive on the album, or "live" in the song's
    /// or album's title.
    pub live: bool,
    pub length_seconds: Option<i32>,
    /// The song's place on its album (1 is usually the intro).
    pub track_number: Option<i32>,
    /// The album's (original) release year, when Roon has it.
    pub year: Option<u32>,
    /// The album's handle (for `album_review`).
    pub album_handle: Option<String>,
    /// A compilation, anthology, split, soundtrack or such: Roon's
    /// IsCompilation on the album, or the words in its title.
    pub compilation: bool,
}

/// A track of the library's Tracks browser (see `library_tracks`): the
/// durable ids only (handles don't outlive the session; `track_by_id`
/// gets one back).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryTrack {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_id: Option<u64>,
    pub album_id: Option<u64>,
    pub performer_id: Option<u64>,
    pub image_key: Option<String>,
    pub length_seconds: Option<i32>,
    pub year: Option<u32>,
    pub favorite: bool,
    /// "local", "qobuz", "tidal", "streaming" or "other".
    pub source: String,
    /// When it came into the library (Roon's import date), Unix ms.
    #[serde(default)]
    pub added_at: Option<i64>,
}

/// A song in a list (a playlist, a tag, Listen Later): its handle for this
/// session and what the Tracks page shows of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEntry {
    pub handle: String,
    #[serde(flatten)]
    pub track: LibraryTrack,
}

/// A playlist's song: the playlist item (to play from or remove) and the
/// song.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistEntry {
    pub item_handle: String,
    #[serde(flatten)]
    pub entry: TrackEntry,
}

/// One of the profile's tags (Roon's Tags page).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCard {
    pub handle: String,
    pub name: String,
    pub count: Option<i32>,
    pub image_key: Option<String>,
}

/// What a tag holds, by kind.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TagContents {
    pub name: String,
    pub albums: Vec<AlbumCard>,
    pub artists: Vec<ArtistCard>,
    pub tracks: Vec<TrackEntry>,
    /// Items of other kinds (works, playlists...), not shown.
    pub other: usize,
    /// A smart tag (rules, no list of its own): not readable yet.
    pub smart: bool,
}

/// One entry of Listen Later: an album, an artist or a song.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenLaterEntry {
    /// "album", "artist" or "track".
    pub kind: String,
    /// When it was added, Unix ms.
    pub added_at: Option<i64>,
    pub album: Option<AlbumCard>,
    pub artist: Option<ArtistCard>,
    pub track: Option<TrackEntry>,
}

/// One entry of Roon's Recent Activity (see `recents`): what you started
/// yourself, an album, an artist, a song, a playlist.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntry {
    /// Roon's Type and DataType, as given ("album", "performer"...).
    pub kind: String,
    pub data_type: String,
    /// When, as Unix milliseconds (0 when Roon gave no time).
    pub at: i64,
    pub count: i32,
    /// The artist it's about: the artist itself, or the album's or song's
    /// artist as Roon credits it. Empty for a playlist, genre, radio.
    pub artist: String,
    /// The album's, song's or playlist's title, or the artist's name.
    pub title: String,
}

/// How to build a mix from artists' top songs (see `play_artist_mix`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MixOptions {
    /// Songs per artist at most, each from a different album where the
    /// artist has enough of them.
    pub per_artist: usize,
    pub include_live: bool,
    pub library_only: bool,
    /// Only songs from albums credited to the artist (Roon's top songs can
    /// be a guest spot on someone else's album: Street Sects' best known
    /// song may be on HEALTH's).
    pub own_albums: bool,
    /// Pick at random among this many of the artist's top songs (0: the
    /// top ones in order), so the same artist gets another song next time.
    pub from_top: usize,
    /// Songs (by Roon track id) never to pick.
    pub never: Vec<String>,
}

impl Default for MixOptions {
    fn default() -> Self {
        MixOptions { per_artist: 3, include_live: false, library_only: false, own_albums: false, from_top: 0, never: Vec::new() }
    }
}

/// A song in the zone's queue, played or to come (see `queue_songs`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSong {
    pub handle: String,
    pub title: String,
    pub artist: String,
    pub image_key: Option<String>,
    pub length_seconds: Option<i32>,
    /// Already played: before the playing song in the queue.
    pub played: bool,
    /// Before the playing song but never played (jumped over with "play
    /// from here"; Roon folds these into "N skipped items").
    pub skipped: bool,
    pub current: bool,
    /// The Core's TransportItemId of the item the song is in (for jumping
    /// back to a played one, Queue::PlayFromHere).
    pub item_id: Option<u64>,
}

/// The queue as a shuffle watches it (see `queue_tracks`).
#[derive(Debug, Clone, Default)]
pub struct QueueTracks {
    /// Songs after the playing one.
    pub upcoming: usize,
    /// The ids (TrackId and LibraryTrackId) of the playing song and the ones
    /// after it.
    pub ahead_ids: Vec<u64>,
}

/// What `play_artist_mix` queued.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MixPlayed {
    pub songs: usize,
    /// Artists that gave at least one song.
    pub artists: usize,
    /// Live recordings left out.
    pub live_skipped: usize,
    /// The songs queued, in order (handles), for saving them as a playlist.
    pub tracks: Vec<String>,
    /// The Core's id for the queue's first item (TransportItemId): where this
    /// queue begins, for the queue panel's played part.
    pub queue_start: Option<u64>,
}

/// One of Roon's playlists (see `playlists`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistCard {
    /// Names this playlist to `play_playlist` and `delete_playlist`; valid for this session only.
    pub handle: String,
    /// Roon's durable id for it (hex).
    pub playlist_id: Option<String>,
    pub name: String,
    /// "roon", "roon-readonly", "qobuz", "tidal", "itunes", "file", "editorial", "smart" or "other".
    pub kind: String,
    pub tracks: Option<i32>,
    pub length_seconds: Option<i32>,
    pub image_key: Option<String>,
    pub read_only: bool,
}

/// What `search` found: Roon's top hits first in each list.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchHits {
    pub query: String,
    pub artists: Vec<ArtistCard>,
    pub albums: Vec<AlbumCard>,
    pub tracks: Vec<TrackHit>,
}

#[derive(Debug, Clone)]
pub enum LibraryEvent {
    /// A zone's current track changed, or its heart / library state did. `None` when nothing is loaded.
    Track { zone_id: String, track: Option<TrackInfo> },
    /// The session ended on its own (network, or the Core hung up).
    Disconnected { reason: String },
}

#[derive(Debug)]
pub enum LibraryError {
    /// The Core turned us down (wrong id, or a Roon version whose protocol we don't speak).
    UnsupportedCore(String),
    /// Network trouble.
    Io(std::io::Error),
    /// The zone has nothing loaded, or the zone id is unknown.
    NothingPlaying(String),
    /// The track isn't in the library and adding it was not allowed.
    NotInLibrary(String),
    /// The Core did not confirm a change in time.
    Unconfirmed(String),
    Remoting(RemotingError),
    Closed,
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LibraryError::UnsupportedCore(m) => write!(f, "this Roon Core is not supported: {m}"),
            LibraryError::Io(e) => write!(f, "network error: {e}"),
            LibraryError::NothingPlaying(z) => write!(f, "nothing is loaded in zone {z}"),
            LibraryError::NotInLibrary(t) => write!(f, "\"{t}\" is not in the library"),
            LibraryError::Unconfirmed(m) => write!(f, "the Core did not confirm: {m}"),
            LibraryError::Remoting(e) => write!(f, "{e}"),
            LibraryError::Closed => f.write_str("not connected to the Core"),
        }
    }
}

impl std::error::Error for LibraryError {}

impl From<RemotingError> for LibraryError {
    fn from(e: RemotingError) -> Self {
        match e {
            RemotingError::Closed => LibraryError::Closed,
            other => LibraryError::Remoting(other),
        }
    }
}

impl From<ConnectError> for LibraryError {
    fn from(e: ConnectError) -> Self {
        match e {
            ConnectError::Unsupported(m) => LibraryError::UnsupportedCore(m),
            ConnectError::Io(e) => LibraryError::Io(e),
            ConnectError::Timeout => LibraryError::UnsupportedCore("the handshake timed out".into()),
        }
    }
}

/// One album of a browsed discography.
#[derive(Debug, Clone)]
struct DiscoEntry {
    /// The object that names the whole release: the streaming service's album
    /// when the library copy came from there, else the library's own album.
    release: u64,
    /// The library's own album object when it differs from `release`.
    library_copy: Option<u64>,
    group: String,
}

struct Inner {
    opts: ClientOptions,
    remoting: Arc<Remoting>,
    graph: Arc<Mutex<ObjectGraph>>,
    /// Streaming tracks come as "metadata" objects whose heart state never
    /// updates; the heart lives on the library's own track object.
    /// LibraryTrackId -> that object's id, looked up via GetTrackLite.
    library_tracks: Mutex<HashMap<u64, u64>>,
    resolving: Mutex<HashSet<u64>>,
    last_reported: Mutex<HashMap<String, Option<TrackInfo>>>,
    /// Performer object id -> its discography, once browsed.
    discographies: Mutex<HashMap<u64, Vec<DiscoEntry>>>,
    recompute_pending: AtomicBool,
    /// Zone id -> which track the zone's current waveform belongs to. Roon
    /// leaves a zone's last waveform in place when the next track has none
    /// (streaming), so a waveform only counts for the track it arrived with.
    waveform_owners: Mutex<HashMap<String, WaveformOwner>>,
    events: broadcast::Sender<LibraryEvent>,
    /// Woken whenever a watched object changes (for confirm waits) and on disconnect.
    changed: Arc<Notify>,
    connected: AtomicBool,
}

/// A connection to one Core's library. Cheap to clone; all clones share the session.
#[derive(Clone)]
pub struct LibraryClient {
    inner: Arc<Inner>,
}

impl LibraryClient {
    /// Connects and waits for the Core's objects. `UnsupportedCore` means the
    /// Core turned us down (wrong id, or a Roon version we don't speak): hide
    /// the buttons and say so.
    pub async fn connect(opts: ClientOptions) -> Result<LibraryClient, LibraryError> {
        let broker_id = broker_id_from_core_id(&opts.core_id)
            .ok_or_else(|| LibraryError::UnsupportedCore(format!("\"{}\" is not a Core id", opts.core_id)))?;
        let handshake = connection::connect(&opts.host, opts.port, broker_id, Duration::from_secs(10)).await?;

        let graph = Arc::new(Mutex::new(ObjectGraph::new()));
        let changed = Arc::new(Notify::new());
        let (events, _) = broadcast::channel(64);

        // The reader task only gets a weak handle: until connect() finishes it
        // just fills the graph, afterwards it also reports changes.
        let inner = Arc::new_cyclic(|weak: &Weak<Inner>| {
            let push_weak = weak.clone();
            let push_graph = Arc::clone(&graph);
            let push_changed = Arc::clone(&changed);
            let close_weak = weak.clone();
            let remoting = Remoting::start(
                handshake.stream,
                handshake.initial,
                opts.request_timeout,
                move |frame| {
                    let mut g = push_graph.lock().unwrap();
                    let Some(oid) = g.ingest(frame) else { return };
                    let watched = g
                        .get(oid)
                        .is_some_and(|o| WATCHED_TYPES.iter().any(|t| o.type_name.ends_with(t)));
                    drop(g);
                    if !watched {
                        return;
                    }
                    push_changed.notify_waiters();
                    if let Some(inner) = push_weak.upgrade() {
                        LibraryClient { inner }.schedule_recompute();
                    }
                },
                move |reason, stats| {
                    if let Some(inner) = close_weak.upgrade() {
                        if inner.connected.swap(false, Ordering::SeqCst) {
                            let reason = format!(
                                "{} ({stats})",
                                reason
                                    .map(|e| e.to_string())
                                    .unwrap_or_else(|| "the Core closed the connection".into())
                            );
                            let _ = inner.events.send(LibraryEvent::Disconnected { reason });
                        }
                        inner.changed.notify_waiters();
                    }
                },
            );
            Inner {
                opts,
                remoting,
                graph: Arc::clone(&graph),
                library_tracks: Mutex::new(HashMap::new()),
                resolving: Mutex::new(HashSet::new()),
                last_reported: Mutex::new(HashMap::new()),
                discographies: Mutex::new(HashMap::new()),
                recompute_pending: AtomicBool::new(false),
                waveform_owners: Mutex::new(HashMap::new()),
                events,
                changed: Arc::clone(&changed),
                connected: AtomicBool::new(false),
            }
        });
        let client = LibraryClient { inner };

        let result: Result<(), LibraryError> = async {
            client.inner.remoting.get_service(&ROOT_SERVICE_GUID).await?;
            tokio::time::sleep(client.inner.opts.settle).await;
            let g = client.inner.graph.lock().unwrap();
            if g.first_of_type("Library").is_none() || client.profile_object(&g).is_none() {
                return Err(LibraryError::UnsupportedCore(
                    "connected, but the Core did not send its Library and Profile objects".into(),
                ));
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            client.inner.remoting.close();
            return Err(e);
        }

        client.inner.connected.store(true, Ordering::SeqCst);
        client.recompute();
        Ok(client)
    }

    /// Ends the session.
    pub fn close(&self) {
        self.inner.connected.store(false, Ordering::SeqCst);
        self.inner.remoting.close();
        self.inner.changed.notify_waiters();
    }

    /// Where the files of library tracks (by handle) are, as Roon's Export
    /// finds them, in the handles' order. None: no file, as for a track
    /// from a streaming service, even one added to the library (an album
    /// you have on disk and from Qobuz is two albums in the library, the
    /// streaming one a hidden duplicate, which a search can still land
    /// on), or a handle that isn't a library track. Asked by the tracks'
    /// library ids, all at once.
    pub async fn track_files(&self, handles: &[String]) -> Result<Vec<Option<TrackFile>>, LibraryError> {
        let (library, ids) = {
            let g = self.inner.graph.lock().unwrap();
            let ids: Vec<Option<u64>> = handles
                .iter()
                .map(|h| {
                    let t = g.get(h.parse().ok()?)?;
                    t.long_field("LibraryTrackId")
                })
                .collect();
            (g.first_of_type("Library").map(|o| o.oid), ids)
        };
        let library = library.ok_or_else(|| LibraryError::UnsupportedCore("no Library service in the graph".into()))?;
        let mut out: Vec<Option<TrackFile>> = vec![None; handles.len()];
        let asked: Vec<u64> = ids.iter().flatten().copied().collect();
        if asked.is_empty() {
            return Ok(out);
        }
        let mut list = Writer::new();
        list.flex_int(asked.len() as u32);
        for id in &asked {
            list.long(*id);
        }
        let mut args = Writer::new();
        args.flex_int(list.len() as u32).bytes(list.as_bytes());
        let res = self.inner.remoting.call(library, SIG_TRACK_EXPORT_INFO, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetTrackExportInfo".into(), status: res.status }.into());
        }
        if res.payload.is_empty() {
            return Ok(out);
        }
        let g = self.inner.graph.lock().unwrap();
        let values = g.decode_return_list(&res.payload).map_err(|_| LibraryError::Unconfirmed("could not read the tracks' files".into()))?;
        let in_order = values.len() == asked.len();
        for (n, value) in values.iter().enumerate() {
            let Value::Inline(s) = value else { continue };
            let text = |name: &str| s.field(name).and_then(Value::as_str).unwrap_or("").to_string();
            let file = TrackFile { file_path: text("FilePath"), http_path: text("HttpPath") };
            if file.file_path.is_empty() && file.http_path.is_empty() {
                continue;
            }
            // (by the track it names, else by its place in the answer)
            let named = g
                .deref(s.field("Track"))
                .and_then(|t| t.long_field("LibraryTrackId").or_else(|| t.long_field("TrackId")))
                .filter(|id| asked.contains(id));
            let id = named.or_else(|| in_order.then(|| asked[n]));
            if let Some(id) = id {
                for (at, mine) in ids.iter().enumerate() {
                    if *mine == Some(id) && out[at].is_none() {
                        out[at] = Some(file.clone());
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst) && !self.inner.remoting.is_closed()
    }

    /// Track changes and disconnects. Each call gets its own receiver.
    pub fn events(&self) -> broadcast::Receiver<LibraryEvent> {
        self.inner.events.subscribe()
    }

    /// How many objects the Core has pushed this session (grows while music
    /// plays; reconnect now and then so the Core can let go of them).
    pub fn object_count(&self) -> usize {
        self.inner.graph.lock().unwrap().objects.len()
    }

    // --- reading ---

    /// The zone ids the Core reports (same strings as the extension API's zone_id).
    pub fn zone_ids(&self) -> Vec<String> {
        let g = self.inner.graph.lock().unwrap();
        g.find_by_type("Zone").filter_map(zone_id_of).collect()
    }

    /// What the zone has loaded right now, or None.
    pub fn track_for_zone(&self, zone_id: &str) -> Option<TrackInfo> {
        let g = self.inner.graph.lock().unwrap();
        let track = self.track_object(&g, zone_id)?;
        Some(self.describe(&g, zone_id, track))
    }

    fn zone_object<'g>(&self, g: &'g ObjectGraph, zone_id: &str) -> Option<&'g RoonObject> {
        let wanted = zone_id.to_ascii_lowercase();
        g.find_by_type("Zone").find(|z| zone_id_of(z).as_deref() == Some(wanted.as_str()))
    }

    /// Zone -> NowPlaying (TransportItem) -> Tracks (DataList) -> current TransportTrack -> Track (TrackLite).
    fn track_object<'g>(&self, g: &'g ObjectGraph, zone_id: &str) -> Option<&'g RoonObject> {
        let zone = self.zone_object(g, zone_id)?;
        let item = g.deref(zone.field("NowPlaying"))?;
        let list = g.deref(item.field("Tracks"))?;
        let items = list.items.as_ref()?;
        let transport_tracks: Vec<&RoonObject> = items.iter().filter_map(|oid| g.get(*oid)).collect();
        let current = transport_tracks
            .iter()
            .find(|t| t.field("IsCurrent").and_then(Value::as_bool) == Some(true))
            .or_else(|| transport_tracks.first())?;
        g.deref(current.field("Track"))
    }

    fn describe(&self, g: &ObjectGraph, zone_id: &str, track: &RoonObject) -> TrackInfo {
        let profile = self.profile_sooid(g);
        let album = g.deref(track.field("Album"));
        let library_track_id = track.long_field("LibraryTrackId");
        // Heart and ban live on the library's track object (the same object for local files).
        let state_obj = self.library_track_object(g, track).unwrap_or(track);
        TrackInfo {
            zone_id: zone_id.to_string(),
            title: track.str_field("Title").unwrap_or_default().to_string(),
            track_id: track.long_field("TrackId").map(|v| v.to_string()),
            library_track_id: library_track_id.map(|v| v.to_string()),
            in_library: library_track_id.is_some(),
            favorite: read_state(state_obj.field("IsFavorite"), profile.as_deref()),
            banned: read_state(state_obj.field("IsBanned"), profile.as_deref()),
            source: match track.field("Source").and_then(Value::as_int) {
                Some(1) => Some("local"),
                Some(_) => Some("streaming"),
                None => None,
            },
            album: album.map(|a| AlbumInfo {
                title: a.str_field("Title").unwrap_or_default().to_string(),
                roon_album_id: a.long_field("RoonAlbumId").map(|v| v.to_string()),
                in_library: a.has_field("LibraryAlbumId"),
                favorite: read_state(a.field("IsFavorite"), profile.as_deref()),
            }),
            quality: self.zone_object(g, zone_id).and_then(|z| source_quality(g, z)).map(Box::new),
            waveform: self.zone_object(g, zone_id).and_then(|z| zone_waveform(g, z)).and_then(|w| self.own_waveform(zone_id, track, w)),
        }
    }

    /// The zone's waveform if it belongs to `track`. A waveform that's new
    /// (by its values) is the playing track's; the same one still there
    /// after the track changed is the last track's, left behind. (Within a
    /// couple of seconds of arriving it may still move to the next track, in
    /// case it came just before the track change did.)
    fn own_waveform(&self, zone_id: &str, track: &RoonObject, waveform: Vec<u8>) -> Option<Vec<u8>> {
        let key = format!("{:?}|{}", track.long_field("TrackId"), track.str_field("Title").unwrap_or_default());
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&waveform, &mut hasher);
        let hash = std::hash::Hasher::finish(&hasher);
        let mut owners = self.inner.waveform_owners.lock().unwrap();
        match owners.get_mut(zone_id) {
            Some(owner) if owner.hash == hash => {
                if owner.track == key {
                    return Some(waveform);
                }
                if owner.since.elapsed() < Duration::from_secs(3) {
                    owner.track = key;
                    return Some(waveform);
                }
                None
            }
            _ => {
                owners.insert(zone_id.to_string(), WaveformOwner { track: key, hash, since: Instant::now() });
                Some(waveform)
            }
        }
    }

    fn profile_object<'g>(&self, g: &'g ObjectGraph) -> Option<&'g RoonObject> {
        g.find_by_type("Profile").find(|p| p.field("ProfileId").and_then(Value::as_bytes).is_some())
    }

    fn profile_sooid(&self, g: &ObjectGraph) -> Option<Vec<u8>> {
        self.profile_object(g)?.field("ProfileId")?.as_bytes().map(<[u8]>::to_vec)
    }

    /// The library's own object for a track, if we have it. Local tracks are
    /// their own library object. Streaming tracks are "metadata" objects whose
    /// TrackId differs from their LibraryTrackId; for those the library object
    /// is fetched with GetTrackLite and the Core keeps it updated.
    fn library_track_object<'g>(&self, g: &'g ObjectGraph, track: &'g RoonObject) -> Option<&'g RoonObject> {
        let library_id = track.long_field("LibraryTrackId")?;
        if track.long_field("TrackId") == Some(library_id) {
            return Some(track);
        }
        let oid = self.inner.library_tracks.lock().unwrap().get(&library_id).copied();
        let obj = oid.and_then(|oid| g.get(oid));
        if obj.is_none() {
            self.resolve_library_track(library_id);
        }
        obj
    }

    /// Asks the Core for the library's track object; the answer lands in the graph and triggers a Track event.
    fn resolve_library_track(&self, library_track_id: u64) {
        if !self.is_connected() || !self.inner.resolving.lock().unwrap().insert(library_track_id) {
            return;
        }
        let client = self.clone();
        tokio::spawn(async move {
            let library = client.inner.graph.lock().unwrap().first_of_type("Library").map(|o| o.oid);
            if let Some(library) = library {
                let mut args = Writer::new();
                args.long(library_track_id);
                match client.inner.remoting.call(library, SIG_GET_TRACK_LITE, args.as_bytes()).await {
                    Ok(res) if res.success() => {
                        if let Ok(oid) = Reader::new(&res.payload).flex_long() {
                            client.inner.library_tracks.lock().unwrap().insert(library_track_id, oid);
                            // The object itself usually arrives right after; report either way.
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            client.inner.changed.notify_waiters();
                            client.schedule_recompute();
                        }
                    }
                    _ => {}
                }
            }
            client.inner.resolving.lock().unwrap().remove(&library_track_id);
        });
    }

    /// Waits for the library's track object (resolving it if needed).
    async fn await_library_track(&self, zone_id: &str) -> Result<u64, LibraryError> {
        let deadline = Instant::now() + self.inner.opts.confirm_timeout;
        loop {
            {
                let g = self.inner.graph.lock().unwrap();
                let track = self.require_track(&g, zone_id)?;
                if let Some(obj) = self.library_track_object(&g, track) {
                    return Ok(obj.oid);
                }
                if Instant::now() > deadline {
                    return Err(LibraryError::Unconfirmed(format!(
                        "no library object for \"{}\"",
                        track.str_field("Title").unwrap_or_default()
                    )));
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    // --- changing ---

    /// Hearts (or un-hearts) the track the zone has loaded. Roon only hearts
    /// library tracks, so when the track isn't in the library yet it is added
    /// first, as `add` says (a single track, or the whole album); `None` gives
    /// `NotInLibrary` instead. Resolves with the confirmed state.
    pub async fn set_favorite(&self, zone_id: &str, favorite: bool, add: Option<AddMode>) -> Result<TrackInfo, LibraryError> {
        let (in_library, before) = {
            let g = self.inner.graph.lock().unwrap();
            let track = self.require_track(&g, zone_id)?;
            (track.has_field("LibraryTrackId"), self.describe(&g, zone_id, track))
        };
        if !in_library {
            if !favorite {
                return Ok(before); // nothing to un-heart
            }
            let mode = add.ok_or_else(|| LibraryError::NotInLibrary(before.title.clone()))?;
            self.add_to_library(zone_id, mode).await?;
        }

        let target = self.await_library_track(zone_id).await?;
        let (profile, library, current) = {
            let g = self.inner.graph.lock().unwrap();
            let track = self.require_track(&g, zone_id)?;
            (
                self.profile_sooid(&g),
                g.first_of_type("Library").map(|o| o.oid),
                self.describe(&g, zone_id, track),
            )
        };
        if current.favorite == favorite {
            return Ok(current);
        }
        let (Some(profile), Some(library)) = (profile, library) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };

        let mut args = Writer::new();
        args.sooid(&profile).long(target).flex_int(if favorite { FAVORITE } else { NOT_FAVORITE });
        let res = self.inner.remoting.call(library, SIG_FAVORITE_TRACK, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "FavoriteOrBan".into(), status: res.status }.into());
        }
        // The Core pushes the new IsFavorite onto the library track object.
        match self.wait_for_track(zone_id, |t| t.favorite == favorite).await {
            Some(confirmed) => Ok(confirmed),
            None => self.track_for_zone(zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into())),
        }
    }

    /// Adds the loaded track (or its whole album) to the library. Resolves
    /// once the Core confirms the track is in the library.
    pub async fn add_to_library(&self, zone_id: &str, mode: AddMode) -> Result<TrackInfo, LibraryError> {
        let (library, profile_oid, track_oid, album_oid, before) = {
            let g = self.inner.graph.lock().unwrap();
            let track = self.require_track(&g, zone_id)?;
            (
                g.first_of_type("Library").map(|o| o.oid),
                self.profile_object(&g).map(|o| o.oid),
                track.oid,
                track.ref_field("Album"),
                self.describe(&g, zone_id, track),
            )
        };
        if before.in_library {
            return Ok(before);
        }
        let (Some(library), Some(profile)) = (library, profile_oid) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };

        let mut args = Writer::new();
        match mode {
            AddMode::Album => {
                let album = album_oid.ok_or_else(|| LibraryError::Unconfirmed(format!("\"{}\" has no album object", before.title)))?;
                args.long(profile).long(album);
                self.inner.remoting.call_no_reply(library, SIG_ADD_ALBUM, args.as_bytes())?;
            }
            AddMode::Track => {
                args.long(profile).long(track_oid);
                self.inner.remoting.call_no_reply(library, SIG_ADD_TRACK, args.as_bytes())?;
            }
        }
        // No reply to this call; the Core confirms by pushing a LibraryTrackId.
        self.wait_for_track(zone_id, |t| t.in_library)
            .await
            .ok_or_else(|| LibraryError::Unconfirmed(format!("adding \"{}\" to the library", before.title)))
    }

    /// Hearts (or un-hearts) the album of the track the zone has loaded. The
    /// album must already be in the library.
    pub async fn set_album_favorite(&self, zone_id: &str, favorite: bool) -> Result<TrackInfo, LibraryError> {
        let (album_oid, before) = {
            let g = self.inner.graph.lock().unwrap();
            let track = self.require_track(&g, zone_id)?;
            (track.ref_field("Album"), self.describe(&g, zone_id, track))
        };
        if before.album.as_ref().map(|a| a.favorite) == Some(favorite) {
            return Ok(before);
        }
        let album = album_oid.ok_or_else(|| LibraryError::Unconfirmed("the track has no album object".into()))?;
        self.favorite_album_object(album, favorite).await?;
        match self.wait_for_track(zone_id, |t| t.album.as_ref().map(|a| a.favorite) == Some(favorite)).await {
            Some(confirmed) => Ok(confirmed),
            None => self.track_for_zone(zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into())),
        }
    }

    /// Sends FavoriteOrBan for one album object (which must be in the library).
    async fn favorite_album_object(&self, album_oid: u64, favorite: bool) -> Result<(), LibraryError> {
        let (profile, library, broker, album_id) = {
            let g = self.inner.graph.lock().unwrap();
            let album = g.get(album_oid).ok_or_else(|| LibraryError::Unconfirmed("unknown album handle".into()))?;
            if !album_in_library(album) {
                return Err(LibraryError::NotInLibrary(album.str_field("Title").unwrap_or_default().to_string()));
            }
            (
                self.profile_sooid(&g),
                g.first_of_type("Library").map(|o| o.oid),
                g.first_of_type("Broker").map(|o| o.oid),
                album.long_field("AlbumId"),
            )
        };
        let (Some(profile), Some(library), Some(broker), Some(album_id)) = (profile, library, broker, album_id) else {
            return Err(LibraryError::UnsupportedCore("no Library/Broker service, Profile or AlbumId in the graph".into()));
        };

        // The official client sends the IEnumerable<AlbumBase> form with inline AlbumLink structs.
        let mut album_id_bytes = Writer::new();
        album_id_bytes.long(album_id);
        let mut broker_bytes = Writer::new();
        broker_bytes.long(broker);
        let link = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.AlbumLink",
            &[
                StructField {
                    name: "long Sooloos.Broker.Api.AlbumLink::AlbumId",
                    prop_type: PropertyType::Long,
                    value: album_id_bytes.into_bytes(),
                },
                StructField {
                    name: "Sooloos.Broker.Api.Broker Sooloos.Broker.Api.AlbumLink::Broker",
                    prop_type: PropertyType::Object,
                    value: broker_bytes.into_bytes(),
                },
            ],
        )?;
        let mut list = Writer::new();
        list.flex_int(1).bytes(&link);
        let mut args = Writer::new();
        args.sooid(&profile)
            .flex_int(list.len() as u32)
            .bytes(list.as_bytes())
            .flex_int(if favorite { FAVORITE } else { NOT_FAVORITE });
        let res = self.inner.remoting.call(library, SIG_FAVORITE_ALBUMS, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "FavoriteOrBan(albums)".into(), status: res.status }.into());
        }
        Ok(())
    }

    // --- the playing artist's discography ---

    /// The albums of the artist the zone is playing, as the Core's artist page
    /// lists them (main albums, other releases, appearances), whether or not
    /// they are in the library. Each comes with a session handle for
    /// `album_tracks_for`, `set_album_favorite_for` and `add_album_for`.
    /// The discography is fetched once per artist and session.
    pub async fn artist_albums(&self, zone_id: &str) -> Result<Vec<ArtistAlbum>, LibraryError> {
        let (library, profile, performer, playing_album) = {
            let g = self.inner.graph.lock().unwrap();
            let track = self.require_track(&g, zone_id)?;
            let album = g
                .deref(track.field("Album"))
                .ok_or_else(|| LibraryError::Unconfirmed("the track has no album object".into()))?;
            let performer = g
                .deref(album.field("MainPerformers"))
                .and_then(|list| list.items.as_ref()?.first().copied())
                .ok_or_else(|| LibraryError::Unconfirmed("the album has no main performer".into()))?;
            (
                g.first_of_type("Library").map(|o| o.oid),
                self.profile_object(&g).map(|o| o.oid),
                performer,
                album.oid,
            )
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        self.albums_of(library, profile, performer, Some(playing_album)).await
    }

    /// The discography of any performer object in the graph (by its handle,
    /// from an album card, an artist card or a search hit), like
    /// `artist_albums`. `zone_id` marks the album that zone is playing.
    pub async fn artist_albums_for(&self, handle: &str, zone_id: Option<&str>) -> Result<Vec<ArtistAlbum>, LibraryError> {
        let performer: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad artist handle".into()))?;
        let (library, profile, playing_album) = {
            let g = self.inner.graph.lock().unwrap();
            if g.get(performer).is_none() {
                return Err(LibraryError::Unconfirmed("unknown artist handle".into()));
            }
            let playing = zone_id.and_then(|z| self.track_object(&g, z)).and_then(|t| t.ref_field("Album"));
            (g.first_of_type("Library").map(|o| o.oid), self.profile_object(&g).map(|o| o.oid), playing)
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        self.albums_of(library, profile, performer, playing_album).await
    }

    /// The discography (cached per performer and session), described.
    async fn albums_of(&self, library: u64, profile: u64, performer: u64, playing_album: Option<u64>) -> Result<Vec<ArtistAlbum>, LibraryError> {
        let cached = self.inner.discographies.lock().unwrap().get(&performer).cloned();
        let albums = match cached {
            Some(list) => list,
            None => {
                let list = self.browse_performer_albums(library, profile, performer).await?;
                self.inner.discographies.lock().unwrap().insert(performer, list.clone());
                list
            }
        };

        let g = self.inner.graph.lock().unwrap();
        let profile_sooid = self.profile_sooid(&g);
        let playing_ids: Vec<u64> = playing_album
            .and_then(|oid| g.get(oid))
            .map(|a| [a.long_field("AlbumId"), a.long_field("RoonAlbumId")].into_iter().flatten().collect())
            .unwrap_or_default();
        Ok(albums
            .iter()
            .filter_map(|entry| {
                let a = g.get(entry.release)?;
                let ids: Vec<u64> = [entry.release, entry.library_copy.unwrap_or(entry.release)]
                    .iter()
                    .filter_map(|oid| g.get(*oid))
                    .flat_map(|o| [o.long_field("AlbumId"), o.long_field("RoonAlbumId")])
                    .flatten()
                    .collect();
                let playing = playing_album.is_some_and(|p| entry.release == p || entry.library_copy == Some(p))
                    || ids.iter().any(|id| playing_ids.contains(id));
                Some(describe_album(&g, a, &entry.group, profile_sooid.as_deref(), playing))
            })
            .collect())
    }

    // --- Wrapped pages: albums and artists by id, search, playing an album ---

    /// Asks the Core for an object by its durable id (GetAlbumLite / GetPerformerLite)
    /// and waits for it to arrive; its session handle.
    async fn fetch_by_id(&self, sig: &str, id: u64, what: &str) -> Result<u64, LibraryError> {
        let library = self.inner.graph.lock().unwrap().first_of_type("Library").map(|o| o.oid);
        let library = library.ok_or_else(|| LibraryError::UnsupportedCore("no Library service in the graph".into()))?;
        let mut args = Writer::new();
        args.long(id);
        let res = self.inner.remoting.call(library, sig, args.as_bytes()).await?;
        if !res.success() || res.payload.is_empty() {
            return Err(LibraryError::Unconfirmed(format!("the Core has no {what} {id}")));
        }
        let oid = Reader::new(&res.payload)
            .flex_long()
            .ok()
            .filter(|oid| *oid != 0)
            .ok_or_else(|| LibraryError::Unconfirmed(format!("the Core has no {what} {id}")))?;
        let arrived = self
            .wait_until(Duration::from_secs(3), |g| g.get(oid).is_some_and(|o| o.has_field("Title") || o.has_field("Name")))
            .await;
        if !arrived {
            return Err(LibraryError::Unconfirmed(format!("{what} {id} did not arrive")));
        }
        Ok(oid)
    }

    /// An album by the Core's album id (as Roon's own log names album pages).
    pub async fn album_by_id(&self, album_id: u64) -> Result<AlbumCard, LibraryError> {
        let oid = self.fetch_by_id(SIG_GET_ALBUM_LITE, album_id, "album").await?;
        self.album_card(&oid.to_string()).ok_or_else(|| LibraryError::Unconfirmed(format!("album {album_id} vanished")))
    }

    /// A track by the Core's track id (as the history knows it): its handle.
    pub async fn track_by_id(&self, track_id: u64) -> Result<String, LibraryError> {
        self.fetch_by_id(SIG_GET_TRACK_LITE, track_id, "track").await.map(|oid| oid.to_string())
    }

    /// An artist by the Core's performer id.
    pub async fn artist_by_id(&self, performer_id: u64) -> Result<ArtistCard, LibraryError> {
        let oid = self.fetch_by_id(SIG_GET_PERFORMER_LITE, performer_id, "artist").await?;
        self.artist_card(&oid.to_string()).ok_or_else(|| LibraryError::Unconfirmed(format!("artist {performer_id} vanished")))
    }

    /// An album object's current state by its handle.
    pub fn album_card(&self, handle: &str) -> Option<AlbumCard> {
        let oid: u64 = handle.parse().ok()?;
        let g = self.inner.graph.lock().unwrap();
        let a = g.get(oid)?;
        let profile = self.profile_sooid(&g);
        Some(describe_album_card(&g, a, profile.as_deref()))
    }

    /// A performer object's current state by its handle.
    pub fn artist_card(&self, handle: &str) -> Option<ArtistCard> {
        let oid: u64 = handle.parse().ok()?;
        let g = self.inner.graph.lock().unwrap();
        let p = g.get(oid)?;
        let profile = self.profile_sooid(&g);
        Some(describe_artist_card(&g, p, profile.as_deref()))
    }

    /// Roon's metadata on an artist: the full Performer object (GetPerformer
    /// on the lite one): genres, country, birthplace, years active.
    pub async fn artist_facts(&self, handle: &str) -> Result<Facts, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad artist handle {handle}")))?;
        let library = self.inner.graph.lock().unwrap().first_of_type("Library").map(|o| o.oid);
        let library = library.ok_or_else(|| LibraryError::UnsupportedCore("no Library service in the graph".into()))?;
        let mut args = Writer::new();
        args.long(oid);
        let res = self.inner.remoting.call(library, SIG_GET_PERFORMER, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetPerformer".into(), status: res.status }.into());
        }
        let full = Reader::new(&res.payload).flex_long().ok().filter(|oid| *oid != 0);
        let Some(full) = full else {
            return Ok(Facts::default());
        };
        self.wait_until(Duration::from_secs(3), |g| g.get(full).is_some_and(|o| o.has_field("Name"))).await;
        let g = self.inner.graph.lock().unwrap();
        let Some(p) = g.get(full) else {
            return Ok(Facts::default());
        };
        Ok(Facts {
            genres: strings_of(&g, p.field("Genres")),
            country: p.str_field("Country").map(str::to_string),
            from: p.str_field("BirthPlace").map(str::to_string),
            active_since: year_of(p.field("YearsActive")),
        })
    }

    /// Roon's biography text for an artist (the full Performer's Biography),
    /// None when Roon has none.
    pub async fn artist_bio(&self, handle: &str) -> Result<Option<String>, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad artist handle {handle}")))?;
        let library = self.inner.graph.lock().unwrap().first_of_type("Library").map(|o| o.oid);
        let library = library.ok_or_else(|| LibraryError::UnsupportedCore("no Library service in the graph".into()))?;
        let mut args = Writer::new();
        args.long(oid);
        let res = self.inner.remoting.call(library, SIG_GET_PERFORMER, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetPerformer".into(), status: res.status }.into());
        }
        let Some(full) = Reader::new(&res.payload).flex_long().ok().filter(|oid| *oid != 0) else {
            return Ok(None);
        };
        self.wait_until(Duration::from_secs(3), |g| g.get(full).is_some_and(|o| o.has_field("Name"))).await;
        let g = self.inner.graph.lock().unwrap();
        Ok(g.get(full).and_then(|p| text_of(&g, p.field("Biography"))))
    }

    /// Roon's review text for an album (the full Album's Review), None when
    /// Roon has none.
    pub async fn album_review(&self, handle: &str) -> Result<Option<String>, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad album handle {handle}")))?;
        let library = self.inner.graph.lock().unwrap().first_of_type("Library").map(|o| o.oid);
        let library = library.ok_or_else(|| LibraryError::UnsupportedCore("no Library service in the graph".into()))?;
        let Some(full) = self.get_album(library, oid).await? else {
            return Ok(None);
        };
        self.wait_until(Duration::from_secs(3), |g| g.get(full).is_some_and(|o| o.has_field("Title"))).await;
        let g = self.inner.graph.lock().unwrap();
        Ok(g.get(full).and_then(|a| text_of(&g, a.field("Review"))))
    }

    /// Roon's metadata on an album: its genres, from the AlbumLite when it
    /// carries them, the full Album otherwise.
    pub async fn album_facts(&self, handle: &str) -> Result<Facts, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad album handle {handle}")))?;
        let (library, genres) = {
            let g = self.inner.graph.lock().unwrap();
            (
                g.first_of_type("Library").map(|o| o.oid),
                g.get(oid).map(|a| strings_of(&g, a.field("Genres"))).unwrap_or_default(),
            )
        };
        if !genres.is_empty() {
            return Ok(Facts { genres, ..Facts::default() });
        }
        let library = library.ok_or_else(|| LibraryError::UnsupportedCore("no Library service in the graph".into()))?;
        let Some(full) = self.get_album(library, oid).await? else {
            return Ok(Facts::default());
        };
        self.wait_until(Duration::from_secs(3), |g| g.get(full).is_some_and(|o| o.has_field("Title"))).await;
        let g = self.inner.graph.lock().unwrap();
        Ok(Facts {
            genres: g.get(full).map(|a| strings_of(&g, a.field("Genres"))).unwrap_or_default(),
            ..Facts::default()
        })
    }

    /// Every main artist in the library (Roon's Artists browser with its
    /// default focus): a VirtualPerformerQuery read page by page, like the
    /// history. `progress` gets (read so far, total) per page.
    pub async fn library_artists(&self, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<Vec<ArtistCard>, LibraryError> {
        const PAGE: usize = 100;
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_sooid(&g))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let field = |name: &'static str, prop_type: PropertyType, value: Vec<u8>| StructField { name, prop_type, value };
        let mut lang = Writer::new();
        lang.string(Some("en"));
        let mut yes = Writer::new();
        yes.boolean(true);
        let criteria = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.PerformerQueryCriteria",
            &[
                field("string Sooloos.Broker.Api.PerformerQueryCriteria::UiLanguage", PropertyType::String, lang.into_bytes()),
                field("bool Sooloos.Broker.Api.PerformerQueryCriteria::RequireIsMainPerformer", PropertyType::Bool, yes.into_bytes()),
            ],
        )?;
        let mut page_size = Writer::new();
        page_size.integer(PAGE as i32);
        let params = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.VirtualQueryParameters",
            &[field("int Sooloos.Broker.Api.VirtualQueryParameters::PageSize", PropertyType::Int, page_size.into_bytes())],
        )?;
        let mut args = Writer::new();
        args.sooid(&profile).bytes(&criteria).bytes(&params);
        let res = self.inner.remoting.call(library, SIG_PERFORMER_QUERY, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "VirtualPerformerQuery".into(), status: res.status }.into());
        }
        let query = Reader::new(&res.payload)
            .flex_long()
            .ok()
            .filter(|oid| *oid != 0)
            .ok_or_else(|| LibraryError::Unconfirmed("the artist query did not open".into()))?;
        // The page's rows come as fresh VirtualQueryElement objects whose
        // Data points at the PerformerLite; the ones already in the graph
        // belong to earlier queries
        let before: HashSet<u64> = self.inner.graph.lock().unwrap().find_by_type_prefix("VirtualQueryElement<").map(|o| o.oid).collect();
        let result = self.query_pages(query, PAGE, &before, SIG_PERFORMER_QUERY_RETAIN, SIG_PERFORMER_QUERY_RELEASE, progress).await;
        let _ = self.inner.remoting.call_no_reply(query, SIG_PERFORMER_QUERY_DISPOSE, &[]);
        let oids = result?;
        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        let mut seen = HashSet::new();
        Ok(oids
            .into_iter()
            .filter_map(|oid| g.get(oid))
            .filter(|p| p.has_field("Name"))
            .map(|p| describe_artist_card(&g, p, profile.as_deref()))
            .filter(|p| seen.insert(p.performer_id.map(|id| id.to_string()).unwrap_or_else(|| p.handle.clone())))
            .collect())
    }

    /// Every album in the library (Roon's Albums browser, in its default order):
    /// a VirtualAlbumQuery read page by page, like the artists.
    pub async fn library_albums(&self, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<Vec<AlbumCard>, LibraryError> {
        const PAGE: usize = 100;
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_sooid(&g))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let field = |name: &'static str, prop_type: PropertyType, value: Vec<u8>| StructField { name, prop_type, value };
        let mut lang = Writer::new();
        lang.string(Some("en"));
        let criteria = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.AlbumQueryCriteria",
            &[field("string Sooloos.Broker.Api.AlbumQueryCriteria::UiLanguage", PropertyType::String, lang.into_bytes())],
        )?;
        let mut page_size = Writer::new();
        page_size.integer(PAGE as i32);
        let params = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.VirtualQueryParameters",
            &[field("int Sooloos.Broker.Api.VirtualQueryParameters::PageSize", PropertyType::Int, page_size.into_bytes())],
        )?;
        let mut args = Writer::new();
        args.sooid(&profile).bytes(&criteria).bytes(&params);
        let res = self.inner.remoting.call(library, SIG_ALBUM_QUERY, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "VirtualAlbumQuery".into(), status: res.status }.into());
        }
        let query = Reader::new(&res.payload)
            .flex_long()
            .ok()
            .filter(|oid| *oid != 0)
            .ok_or_else(|| LibraryError::Unconfirmed("the album query did not open".into()))?;
        let before: HashSet<u64> = self.inner.graph.lock().unwrap().find_by_type_prefix("VirtualQueryElement<").map(|o| o.oid).collect();
        let result = self.query_pages(query, PAGE, &before, SIG_ALBUM_QUERY_RETAIN, SIG_ALBUM_QUERY_RELEASE, progress).await;
        let _ = self.inner.remoting.call_no_reply(query, SIG_ALBUM_QUERY_DISPOSE, &[]);
        let oids = result?;
        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        let mut seen = HashSet::new();
        Ok(oids
            .into_iter()
            .filter_map(|oid| g.get(oid))
            .filter(|a| a.has_field("Title"))
            .map(|a| describe_album_card(&g, a, profile.as_deref()))
            .filter(|a| seen.insert(a.album_id.map(|id| id.to_string()).unwrap_or_else(|| a.handle.clone())))
            .collect())
    }

    /// Every track in the library (Roon's Tracks browser): a
    /// VirtualTrackQuery read page by page, like the artists and albums.
    pub async fn library_tracks(&self, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<Vec<LibraryTrack>, LibraryError> {
        const PAGE: usize = 200;
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_sooid(&g))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let field = |name: &'static str, prop_type: PropertyType, value: Vec<u8>| StructField { name, prop_type, value };
        let mut lang = Writer::new();
        lang.string(Some("en"));
        let criteria = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.TrackQueryCriteria",
            &[field("string Sooloos.Broker.Api.TrackQueryCriteria::UiLanguage", PropertyType::String, lang.into_bytes())],
        )?;
        let mut page_size = Writer::new();
        page_size.integer(PAGE as i32);
        let params = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.VirtualQueryParameters",
            &[field("int Sooloos.Broker.Api.VirtualQueryParameters::PageSize", PropertyType::Int, page_size.into_bytes())],
        )?;
        let mut args = Writer::new();
        args.sooid(&profile).bytes(&criteria).bytes(&params);
        let res = self.inner.remoting.call(library, SIG_TRACK_QUERY, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "VirtualTrackQuery".into(), status: res.status }.into());
        }
        let query = Reader::new(&res.payload)
            .flex_long()
            .ok()
            .filter(|oid| *oid != 0)
            .ok_or_else(|| LibraryError::Unconfirmed("the track query did not open".into()))?;
        let before: HashSet<u64> = self.inner.graph.lock().unwrap().find_by_type_prefix("VirtualQueryElement<").map(|o| o.oid).collect();
        let result = self.query_pages(query, PAGE, &before, SIG_TRACK_QUERY_RETAIN, SIG_TRACK_QUERY_RELEASE, progress).await;
        let _ = self.inner.remoting.call_no_reply(query, SIG_TRACK_QUERY_DISPOSE, &[]);
        let oids = result?;
        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        // (once, what a TrackLite carries, for the import date's name)
        if let Some(first) = oids.first().and_then(|oid| g.get(*oid)) {
            let mut names: Vec<&str> = first.fields.keys().map(|k| k.rsplit("::").next().unwrap_or(k)).collect();
            names.sort_unstable();
            println!("[library] TrackLite fields: {}", names.join(", "));
        }
        let mut seen = HashSet::new();
        Ok(oids
            .into_iter()
            .filter_map(|oid| g.get(oid))
            .filter(|t| t.has_field("Title"))
            .map(|t| describe_library_track(&g, t, profile.as_deref()))
            .filter(|t| seen.insert(t.track_id.map(|id| id.to_string()).unwrap_or_else(|| format!("{}\u{0}{}", t.title, t.album))))
            .collect())
    }

    /// Reads every page of a virtual query: the object ids its elements'
    /// Data point at, in query order.
    async fn query_pages(
        &self,
        query: u64,
        page_size: usize,
        before: &HashSet<u64>,
        retain: &str,
        release: &str,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<Vec<u64>, LibraryError> {
        let count = |g: &ObjectGraph| g.get(query).and_then(|q| q.field("Count")).and_then(Value::as_int);
        if !self.wait_until(Duration::from_secs(5), |g| count(g).is_some()).await {
            return Err(LibraryError::Unconfirmed("Roon did not say how many rows the query has".into()));
        }
        let total = count(&self.inner.graph.lock().unwrap()).unwrap_or(0).max(0) as usize;
        let elements = |g: &ObjectGraph| -> Vec<(u64, Option<u64>)> {
            g.find_by_type_prefix("VirtualQueryElement<")
                .filter(|o| !before.contains(&o.oid))
                .map(|o| (o.oid, o.ref_field("Data")))
                .collect()
        };
        for page in 0..total.div_ceil(page_size) {
            let mut arg = Writer::new();
            arg.integer(page as i32);
            let res = self.inner.remoting.call(query, retain, arg.as_bytes()).await?;
            if !res.success() {
                return Err(RemotingError::Failed { method: format!("RetainPage({page})"), status: res.status }.into());
            }
            let want = total.min((page + 1) * page_size);
            self.wait_until(Duration::from_secs(5), |g| elements(g).len() >= want).await;
            self.wait_until(Duration::from_secs(3), |g| {
                elements(g).iter().all(|(_, data)| data.and_then(|d| g.get(d)).is_some_and(|o| o.has_field("Name") || o.has_field("Title")))
            })
            .await;
            let _ = self.inner.remoting.call_no_reply(query, release, arg.as_bytes());
            progress(want, total);
        }
        let g = self.inner.graph.lock().unwrap();
        let mut rows = elements(&g);
        rows.sort_by_key(|(oid, _)| *oid);
        Ok(rows.into_iter().filter_map(|(_, data)| data).collect())
    }

    /// Roon's play history for the profile, newest first, up to `limit`
    /// plays: the same VirtualHistoryQuery Roon's History page uses (pages of
    /// 100 retained one after the other; the HistoryPlay objects arrive as
    /// pushes; the query is disposed afterwards).
    pub async fn history(&self, limit: usize) -> Result<History, LibraryError> {
        self.history_since(limit, None, &|_, _| {}).await
    }

    /// `history`, stopping early once a page holds a play from `stop_at`
    /// (Unix ms) or before (the plays already known), and reporting each
    /// page as (plays read so far, plays wanted) to `progress`.
    pub async fn history_since(
        &self,
        limit: usize,
        stop_at: Option<i64>,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<History, LibraryError> {
        const PAGE: usize = 100;
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_sooid(&g))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let criteria = self.inner.remoting.inline_struct("Sooloos.Broker.Api.HistoryQueryCriteria", &[])?;
        let mut page_size = Writer::new();
        page_size.integer(PAGE as i32);
        let params = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.VirtualQueryParameters",
            &[StructField {
                name: "int Sooloos.Broker.Api.VirtualQueryParameters::PageSize",
                prop_type: PropertyType::Int,
                value: page_size.into_bytes(),
            }],
        )?;
        let mut args = Writer::new();
        args.sooid(&profile).bytes(&criteria).bytes(&params);
        let res = self.inner.remoting.call(library, SIG_HISTORY_QUERY, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "VirtualHistoryQuery".into(), status: res.status }.into());
        }
        let query = Reader::new(&res.payload)
            .flex_long()
            .ok()
            .filter(|oid| *oid != 0)
            .ok_or_else(|| LibraryError::Unconfirmed("the history query did not open".into()))?;

        // What an earlier query left in the graph (its rows and plays): this
        // query's rows are the new ones, and they can point at plays that
        // were already there (a second read in the same session)
        let before: HashSet<u64> = {
            let g = self.inner.graph.lock().unwrap();
            g.find_by_type("HistoryPlay").chain(history_rows(&g)).map(|o| o.oid).collect()
        };
        let result = self.history_pages(query, limit, PAGE, &before, stop_at, progress).await;
        let _ = self.inner.remoting.call_no_reply(query, SIG_HISTORY_DISPOSE, &[]);
        result
    }

    async fn history_pages(
        &self,
        query: u64,
        limit: usize,
        page_size: usize,
        before: &HashSet<u64>,
        stop_at: Option<i64>,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<History, LibraryError> {
        let count = |g: &ObjectGraph| g.get(query).and_then(|q| q.field("Count")).and_then(Value::as_int);
        if !self.wait_until(Duration::from_secs(5), |g| count(g).is_some()).await {
            return Err(LibraryError::Unconfirmed("Roon did not say how many plays it has".into()));
        }
        let total = count(&self.inner.graph.lock().unwrap()).unwrap_or(0).max(0) as usize;
        let target = total.min(limit);
        // This query's plays: what its new rows point at, and any play that
        // is new to the graph (oids, each once)
        let mine = |g: &ObjectGraph| -> Vec<u64> {
            let mut oids: Vec<u64> = history_rows(g).filter(|r| !before.contains(&r.oid)).filter_map(|r| r.ref_field("Data")).collect();
            oids.extend(g.find_by_type("HistoryPlay").filter(|p| !before.contains(&p.oid)).map(|p| p.oid));
            oids.sort_unstable();
            oids.dedup();
            oids
        };
        let new_rows = |g: &ObjectGraph| history_rows(g).filter(|r| !before.contains(&r.oid)).count();
        let arrived = |g: &ObjectGraph| new_rows(g).max(mine(g).len());
        let pages = target.div_ceil(page_size);
        // (for the log: what had arrived after each page)
        let mut seen_after: Vec<usize> = Vec::new();
        let started = std::time::Instant::now();
        for page in 0..pages {
            let mut arg = Writer::new();
            arg.integer(page as i32);
            let res = self.inner.remoting.call(query, SIG_HISTORY_RETAIN, arg.as_bytes()).await?;
            if !res.success() {
                return Err(RemotingError::Failed { method: format!("RetainPage({page})"), status: res.status }.into());
            }
            let want = total.min((page + 1) * page_size);
            // The page's rows, then their plays' tracks and albums (names),
            // up to 5 s
            self.wait_until(Duration::from_secs(5), |g| arrived(g) >= want).await;
            self.wait_until(Duration::from_secs(3), |g| {
                mine(g)
                    .into_iter()
                    .all(|oid| g.get(oid).is_some_and(|p| p.has_field("Time") && g.deref(p.field("Track")).is_some_and(|t| t.has_field("Title"))))
            })
            .await;
            let _ = self.inner.remoting.call_no_reply(query, SIG_HISTORY_RELEASE, arg.as_bytes());
            seen_after.push(arrived(&self.inner.graph.lock().unwrap()));
            progress(want, target);
            // Reached what the caller already has: no need for older pages
            if let Some(stop_at) = stop_at {
                let g = self.inner.graph.lock().unwrap();
                let reached = mine(&g).into_iter().filter_map(|oid| g.get(oid)).filter_map(|p| describe_play(&g, p)).any(|p| p.played_at <= stop_at);
                if reached {
                    break;
                }
            }
        }
        let g = self.inner.graph.lock().unwrap();
        let ours = mine(&g);
        let mut seen = HashSet::new();
        let mut plays: Vec<Play> = ours
            .iter()
            .filter_map(|oid| g.get(*oid))
            .filter(|p| seen.insert(play_identity(p)))
            .filter_map(|p| describe_play(&g, p))
            .collect();
        plays.sort_by(|a, b| b.played_at.cmp(&a.played_at));
        plays.truncate(target);
        let all = g.find_by_type("HistoryPlay").count();
        let fresh = g.find_by_type("HistoryPlay").filter(|o| !before.contains(&o.oid)).count();
        let rows = new_rows(&g);
        let row_fields: Vec<String> = history_rows(&g)
            .find(|r| !before.contains(&r.oid))
            .map(|r| r.fields.keys().map(|k| k.rsplit("::").next().unwrap_or(k).to_string()).collect())
            .unwrap_or_default();
        let shown = |list: &[usize]| {
            if list.len() <= 8 {
                format!("{list:?}")
            } else {
                format!("{:?} ... {:?}", &list[..4], &list[list.len() - 3..])
            }
        };
        let types: Vec<String> = g.type_counts("History").into_iter().map(|(t, n)| format!("{t} {n}")).collect();
        let query_fields: Vec<String> = g.get(query).map(|q| q.fields.keys().map(|k| k.rsplit("::").next().unwrap_or(k).to_string()).collect()).unwrap_or_default();
        let debug = format!(
            "total {total}, target {target}, pages read {} of {pages} in {} s, stop at {stop_at:?}; objects in the graph before {}; plays after {all} ({fresh} new to the graph), this read's rows {rows} pointing at {} plays, described {}; arrived after each page {}; types {}; query fields {}; row fields {}",
            seen_after.len(),
            started.elapsed().as_secs(),
            before.len(),
            ours.len(),
            plays.len(),
            shown(&seen_after),
            types.join(", "),
            query_fields.join(", "),
            row_fields.join(", "),
        );
        Ok(History { total: total as u32, plays, debug })
    }

    /// The performer in the graph whose picture has one of these Core image
    /// keys (how Roon's artist page, which names no performer in the log,
    /// is told from the pictures it fetches). The first match, as a card.
    pub fn artist_by_image(&self, keys: &[String]) -> Option<ArtistCard> {
        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        let found = g
            .find_by_type_prefix("Performer")
            .filter(|o| o.has_field("Name"))
            .find(|p| {
                ["Photo", "Avatar", "Image", "HeroImage", "BannerImage"]
                    .iter()
                    .filter_map(|name| image_key(&g, p, name))
                    .any(|k| keys.contains(&k))
            })
            .map(|p| describe_artist_card(&g, p, profile.as_deref()));
        found
    }

    /// Roon's share link for an artist or album (what its Share button
    /// copies), by handle; the Roon app opens such links.
    pub async fn share_link(&self, handle: &str) -> Result<String, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad handle {handle}")))?;
        let (sharing, profile, kind) = {
            let g = self.inner.graph.lock().unwrap();
            let kind = g.get(oid).map(|o| o.short_type().to_string());
            (g.first_of_type("Sharing").map(|o| o.oid), self.profile_sooid(&g), kind)
        };
        let (Some(sharing), Some(profile)) = (sharing, profile) else {
            return Err(LibraryError::UnsupportedCore("no Sharing service or Profile in the graph".into()));
        };
        let sig = match kind.as_deref() {
            Some(k) if k.contains("Performer") => SIG_SHARE_PERFORMER,
            Some(k) if k.contains("Album") => SIG_SHARE_ALBUM,
            other => return Err(LibraryError::Unconfirmed(format!("not an artist or album: {other:?}"))),
        };
        let mut args = Writer::new();
        args.sooid(&profile).long(oid);
        let res = self.inner.remoting.call(sharing, sig, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetShareLink".into(), status: res.status }.into());
        }
        Reader::new(&res.payload)
            .string()
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| LibraryError::Unconfirmed("the Core gave no share link".into()))
    }

    /// Roon's genre page, by the genre's name as Roon spells it: top and
    /// recommended artists, top and recommended albums, new releases.
    pub async fn genre_page(&self, name: &str) -> Result<GenrePage, LibraryError> {
        let subject = Subject::Name(name);
        let top_artists = self.one_box(SIG_GENRE_TOP_PERFORMERS, "GetOneBoxGenreTopPerformers", subject).await?;
        let rec_artists = self.one_box(SIG_GENRE_RECOMMENDED_PERFORMERS, "GetOneBoxGenreRecommendedPerformers", subject).await?;
        let top_albums = self.one_box(SIG_GENRE_TOP_ALBUMS, "GetOneBoxGenreTopAlbums", subject).await?;
        let rec_albums = self.one_box(SIG_GENRE_RECOMMENDED_ALBUMS, "GetOneBoxGenreRecommendedAlbums", subject).await?;
        let new_releases = self.one_box(SIG_GENRE_NEW_RELEASES, "GetOneBoxGenreNewReleases", subject).await?;
        self.wait_for_lists(&[top_artists.clone(), rec_artists.clone()], Some("Performer")).await;
        self.wait_for_lists(&[top_albums.clone(), rec_albums.clone(), new_releases.clone()], Some("Album")).await;
        let g = self.inner.graph.lock().unwrap();
        Ok(GenrePage {
            name: name.to_string(),
            top_artists: self.artist_cards(&g, Some(&top_artists), None),
            recommended_artists: self.artist_cards(&g, Some(&rec_artists), None),
            top_albums: self.album_cards(&g, Some(&top_albums), None),
            recommended_albums: self.album_cards(&g, Some(&rec_albums), None),
            new_releases: self.album_cards(&g, Some(&new_releases), None),
        })
    }

    /// One of the Library's "one box" calls (what Roon's own pages are built
    /// from): `subject` (an album or performer handle) and the profile, and
    /// the decoded return value: a reference to a list, or an inline struct
    /// whose members reference lists. The lists and their items arrive as
    /// pushes afterwards; see `wait_for_lists`.
    async fn one_box(&self, sig: &str, method: &str, subject: Subject<'_>) -> Result<Value, LibraryError> {
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_sooid(&g))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let mut args = Writer::new();
        match subject {
            Subject::None => {}
            Subject::Oid(oid) => {
                args.long(oid);
            }
            Subject::Name(name) => {
                args.string(Some(name));
            }
        }
        args.sooid(&profile);
        let res = self.inner.remoting.call(library, sig, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: method.into(), status: res.status }.into());
        }
        if res.payload.is_empty() {
            return Ok(Value::Null);
        }
        let value = self
            .inner
            .graph
            .lock()
            .unwrap()
            .decode_return_value(&res.payload)
            .map_err(|_| LibraryError::Unconfirmed(format!("could not read what {method} returned")))?;
        Ok(value)
    }

    /// Waits (up to 3 s) until every list in `lists` is in the graph and each
    /// of its members has its name; `via` names the member of an inline item
    /// (PerformerWithExtras::Performer) that points at the real object.
    async fn wait_for_lists(&self, lists: &[Value], via: Option<&str>) {
        let settled = |g: &ObjectGraph| {
            lists.iter().all(|list| {
                list_members(g, Some(list), via)
                    .is_some_and(|members| members.iter().all(|oid| g.get(*oid).is_some_and(|o| o.has_field("Title") || o.has_field("Name"))))
            })
        };
        self.wait_until(Duration::from_secs(3), settled).await;
    }

    fn album_cards(&self, g: &ObjectGraph, list: Option<&Value>, skip: Option<u64>) -> Vec<AlbumCard> {
        let profile = self.profile_sooid(g);
        let mut seen = HashSet::new();
        list_members(g, list, Some("Album"))
            .unwrap_or_default()
            .into_iter()
            .filter(|oid| Some(*oid) != skip)
            .filter_map(|oid| g.get(oid))
            .filter(|a| a.has_field("Title"))
            .map(|a| describe_album_card(g, a, profile.as_deref()))
            .filter(|a| seen.insert(a.album_id.map(|id| id.to_string()).unwrap_or_else(|| a.handle.clone())))
            .collect()
    }

    fn artist_cards(&self, g: &ObjectGraph, list: Option<&Value>, skip: Option<u64>) -> Vec<ArtistCard> {
        let profile = self.profile_sooid(g);
        let mut seen = HashSet::new();
        list_members(g, list, Some("Performer"))
            .unwrap_or_default()
            .into_iter()
            .filter(|oid| Some(*oid) != skip)
            .filter_map(|oid| g.get(oid))
            .filter(|p| p.has_field("Name"))
            .map(|p| describe_artist_card(g, p, profile.as_deref()))
            .filter(|p| seen.insert(p.performer_id.map(|id| id.to_string()).unwrap_or_else(|| p.handle.clone())))
            .collect()
    }

    /// Roon's picks around an artist: similar artists, and the albums it
    /// recommends, the latest ones and the ones the artist appears on. Lists
    /// Roon has nothing for come back empty.
    pub async fn artist_extras(&self, handle: &str) -> Result<ArtistExtras, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad artist handle {handle}")))?;
        let similar = self.one_box(SIG_SIMILAR_PERFORMERS, "GetOneBoxSimilarPerformers", Subject::Oid(oid)).await?;
        let recommended = self.one_box(SIG_PERFORMER_RECOMMENDED, "GetOneBoxPerformerRecommendedAlbums", Subject::Oid(oid)).await?;
        let album_list = |name: &str| -> Value {
            match &recommended {
                Value::Inline(s) => s.field(name).cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            }
        };
        let (rec, latest, appears) = (album_list("RecommendedAlbums"), album_list("LatestAlbums"), album_list("AppearsOnAlbums"));
        self.wait_for_lists(std::slice::from_ref(&similar), Some("Performer")).await;
        self.wait_for_lists(&[rec.clone(), latest.clone(), appears.clone()], Some("Album")).await;
        let g = self.inner.graph.lock().unwrap();
        Ok(ArtistExtras {
            similar: self.artist_cards(&g, Some(&similar), Some(oid)),
            recommended: self.album_cards(&g, Some(&rec), None),
            latest: self.album_cards(&g, Some(&latest), None),
            appears_on: self.album_cards(&g, Some(&appears), None),
        })
    }

    /// Roon's picks around an album: similar albums (and Roon's word for the
    /// likeness), albums recommended from this one, and similar artists.
    pub async fn album_extras(&self, handle: &str) -> Result<AlbumExtras, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad album handle {handle}")))?;
        let first = self.album_extras_of(oid).await;
        if !first.similar.is_empty() || !first.recommended.is_empty() || !first.similar_artists.is_empty() {
            return Ok(first);
        }
        // Nothing for this one: a library album that holds part of a
        // release (a single track added, say) has no picks of its own, but
        // its full streaming version does
        let Some(version) = self.full_version_of(oid).await else {
            return Ok(first);
        };
        Ok(self.album_extras_of(version).await)
    }

    /// The picks for one album object; a call the Core turns down
    /// (NotFound for an album it has no picks for) is an empty list, not a
    /// failure of the whole.
    async fn album_extras_of(&self, oid: u64) -> AlbumExtras {
        let quiet = |r: Result<Value, LibraryError>, what: &str| match r {
            Ok(v) => v,
            Err(e) => {
                println!("[library] {what} for album {oid}: {e}");
                Value::Null
            }
        };
        let similar = quiet(self.one_box(SIG_SIMILAR_ALBUMS, "GetOneBoxSimilarAlbums", Subject::Oid(oid)).await, "similar albums");
        let recommended =
            quiet(self.one_box(SIG_ALBUM_RECOMMENDED, "GetOneBoxAlbumRecommendedForYou", Subject::Oid(oid)).await, "recommended albums");
        let artists =
            quiet(self.one_box(SIG_ALBUM_SIMILAR_PERFORMERS, "GetOneBoxAlbumSimilarPerformers", Subject::Oid(oid)).await, "similar artists");
        let (similar_list, similarity) = match &similar {
            Value::Inline(s) => (
                s.field("Albums").cloned().unwrap_or(Value::Null),
                s.field("SimilarityType").and_then(Value::as_str).map(str::to_string),
            ),
            other => (other.clone(), None),
        };
        self.wait_for_lists(&[similar_list.clone(), recommended.clone()], Some("Album")).await;
        self.wait_for_lists(std::slice::from_ref(&artists), Some("Performer")).await;
        let g = self.inner.graph.lock().unwrap();
        AlbumExtras {
            similar: self.album_cards(&g, Some(&similar_list), Some(oid)),
            similarity,
            recommended: self.album_cards(&g, Some(&recommended), Some(oid)),
            similar_artists: self.artist_cards(&g, Some(&artists), None),
        }
    }

    /// The streaming version of an album with the most tracks (the whole
    /// release, for a library album that holds part of it), if Roon knows one.
    async fn full_version_of(&self, album: u64) -> Option<u64> {
        let library = self.inner.graph.lock().unwrap().first_of_type("Library").map(|o| o.oid)?;
        let full = self.get_album(library, album).await.ok()??;
        self.wait_until(Duration::from_secs(3), |g| {
            g.get(full).is_some_and(|a| a.field("AlternateVersionsReady").and_then(Value::as_bool) == Some(true))
        })
        .await;
        let g = self.inner.graph.lock().unwrap();
        largest_streaming_version(&g, full).map(|(oid, _)| oid).filter(|oid| *oid != album)
    }

    /// The artists Roon recommends to this profile (its Home page's
    /// "Recommended for you").
    pub async fn recommended_artists(&self) -> Result<Vec<ArtistCard>, LibraryError> {
        let list = self.one_box(SIG_RECOMMENDED_PERFORMERS, "GetOneBoxRecommendedPerformers", Subject::None).await?;
        self.wait_for_lists(std::slice::from_ref(&list), Some("Performer")).await;
        let g = self.inner.graph.lock().unwrap();
        Ok(self.artist_cards(&g, Some(&list), None))
    }

    /// Roon's own search (what its search box uses): artists, albums and
    /// tracks for `query`, Roon's top hits first in each list.
    pub async fn search(&self, query: &str, limit: usize) -> Result<SearchHits, LibraryError> {
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_sooid(&g))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let field = |name: &'static str, prop_type: PropertyType, value: Vec<u8>| StructField { name, prop_type, value };
        let mut profile_bytes = Writer::new();
        profile_bytes.sooid(&profile);
        let mut terms = Writer::new();
        terms.string(Some(query));
        let mut max_count = Writer::new();
        max_count.integer(limit.clamp(1, 200) as i32);
        let mut max_top = Writer::new();
        max_top.integer(10);
        let params = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.SearchParameters",
            &[
                field("System.Sooid Sooloos.Broker.Api.SearchParameters::ProfileId", PropertyType::Sooid, profile_bytes.into_bytes()),
                field("string Sooloos.Broker.Api.SearchParameters::Terms", PropertyType::String, terms.into_bytes()),
                field("int Sooloos.Broker.Api.SearchParameters::MaxCount", PropertyType::Int, max_count.into_bytes()),
                field("int Sooloos.Broker.Api.SearchParameters::MaxTopResultCount", PropertyType::Int, max_top.into_bytes()),
            ],
        )?;
        let res = self.inner.remoting.call(library, SIG_UNIFIED_SEARCH, &params).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "UnifiedSearch".into(), status: res.status }.into());
        }
        let root = Reader::new(&res.payload)
            .flex_long()
            .map_err(|_| LibraryError::Unconfirmed("could not read the search results".into()))?;

        // The results object and its lists arrive as pushes; wait until the
        // lists are here and their objects have their names (up to 3 s), then
        // take what there is.
        let settled = |g: &ObjectGraph| -> bool {
            let Some(r) = g.get(root) else { return false };
            if !r.has_field("Performers") && !r.has_field("Albums") && !r.has_field("Tracks") {
                return false;
            }
            let (artists, albums, tracks) = search_lists(g, r);
            artists.iter().chain(&albums).chain(&tracks).all(|oid| {
                g.get(*oid).is_some_and(|o| o.has_field("Title") || o.has_field("Name"))
            })
        };
        self.wait_until(Duration::from_secs(3), settled).await;

        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        let Some(r) = g.get(root) else {
            return Err(LibraryError::Unconfirmed("the search results did not arrive".into()));
        };
        let (artists, albums, tracks) = search_lists(&g, r);
        Ok(SearchHits {
            query: query.to_string(),
            artists: artists
                .iter()
                .filter_map(|oid| g.get(*oid))
                .filter(|o| o.has_field("Name"))
                .map(|o| describe_artist_card(&g, o, profile.as_deref()))
                .collect(),
            albums: albums
                .iter()
                .filter_map(|oid| g.get(*oid))
                .filter(|o| o.has_field("Title"))
                .map(|o| describe_album_card(&g, o, profile.as_deref()))
                .collect(),
            tracks: tracks
                .iter()
                .filter_map(|oid| g.get(*oid))
                .filter(|o| o.has_field("Title"))
                .map(|t| {
                    let album = g.deref(t.field("Album"));
                    let state_obj = self.library_track_object(&g, t).unwrap_or(t);
                    TrackHit {
                        handle: t.oid.to_string(),
                        title: t.str_field("Title").unwrap_or_default().to_string(),
                        artist: strip_credit_markup(album.and_then(|a| a.str_field("PerformedBy")).unwrap_or_default()),
                        album: album.and_then(|a| a.str_field("Title")).unwrap_or_default().to_string(),
                        album_handle: album.map(|a| a.oid.to_string()),
                        image_key: album.and_then(|a| image_key(&g, a, "Image")),
                        length_seconds: t.field("LengthSeconds").and_then(Value::as_int),
                        in_library: t.has_field("LibraryTrackId"),
                        favorite: read_state(state_obj.field("IsFavorite"), profile.as_deref()),
                    }
                })
                .collect(),
        })
    }

    /// The album of the track the zone is playing, as a card (its artist's
    /// handle included).
    pub fn playing_album(&self, zone_id: &str) -> Result<AlbumCard, LibraryError> {
        let g = self.inner.graph.lock().unwrap();
        let track = self.require_track(&g, zone_id)?;
        let album = g
            .deref(track.field("Album"))
            .ok_or_else(|| LibraryError::Unconfirmed("the track has no album object".into()))?;
        Ok(describe_album_card(&g, album, self.profile_sooid(&g).as_deref()))
    }

    /// A new playlist "Wrapped: <title>" holding the track the zone is playing.
    pub async fn playlist_from_playing(&self, zone_id: &str) -> Result<PlaylistCard, LibraryError> {
        let (oid, title) = {
            let g = self.inner.graph.lock().unwrap();
            let track = self.require_track(&g, zone_id)?;
            (track.oid, track.str_field("Title").unwrap_or_default().to_string())
        };
        self.create_playlist(&format!("Wrapped: {title}"), &[oid.to_string()]).await
    }

    /// The zone's Queue object.
    fn queue_object(&self, zone_id: &str) -> Result<u64, LibraryError> {
        let g = self.inner.graph.lock().unwrap();
        let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
        zone.ref_field("Queue").ok_or_else(|| LibraryError::UnsupportedCore("the zone has no queue".into()))
    }

    /// The queue's items (Queue::GetItems), the played ones too.
    async fn queue_items(&self, queue: u64) -> Result<Vec<u64>, LibraryError> {
        let res = self.inner.remoting.call(queue, SIG_QUEUE_ITEMS, &[]).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "Queue::GetItems".into(), status: res.status }.into());
        }
        let g = self.inner.graph.lock().unwrap();
        Ok(g.decode_return_list(&res.payload)
            .map_err(|_| LibraryError::Unconfirmed("could not read the queue".into()))?
            .iter()
            .filter_map(Value::as_ref_id)
            .collect())
    }

    /// The Core's id (TransportItemId) of the queue's first item, and the
    /// track id of its first song (right after a clear and a new queue, the
    /// playing one).
    async fn queue_first_id(&self, queue: u64) -> Option<(u64, Option<u64>)> {
        let first = *self.queue_items(queue).await.ok()?.first()?;
        let song = |g: &ObjectGraph| -> Option<u64> {
            let list = g.deref(g.get(first)?.field("Tracks"))?;
            let tt = g.get(*list.items.as_ref()?.first()?)?;
            g.get(tt.ref_field("Track")?)?.long_field("TrackId")
        };
        self.wait_until(Duration::from_secs(1), |g| g.get(first).is_some_and(|o| o.has_field("TransportItemId")) && song(g).is_some()).await;
        let g = self.inner.graph.lock().unwrap();
        Some((g.get(first)?.long_field("TransportItemId")?, song(&g)))
    }

    /// The zone's queue in order: the songs already played since `start`
    /// (the queue item the current queue began with; without one, or with
    /// one the queue no longer has, only the last `recent` items) and the
    /// ones to come. Queue::GetItems gives the whole queue, the played items
    /// too (Queue.CurrentItemIndex points at the playing one; the zone's
    /// NowPlaying is that item). An item is a song or a whole album queued;
    /// its TransportTracks point at the songs, one of them IsCurrent.
    pub async fn queue_songs(&self, zone_id: &str, start: Option<u64>, recent: usize) -> Result<Vec<QueueSong>, LibraryError> {
        Ok(self.queue_songs_report(zone_id, start, recent, false).await?.0)
    }

    /// How many songs come after the playing one, and which songs those
    /// are (with the playing one): a cheap look for a shuffle that keeps the
    /// queue topped up.
    pub async fn queue_tracks(&self, zone_id: &str) -> Result<QueueTracks, LibraryError> {
        let queue = self.queue_object(zone_id)?;
        let listed = self.queue_items(queue).await?;
        let now_item = {
            let g = self.inner.graph.lock().unwrap();
            self.zone_object(&g, zone_id).and_then(|z| z.ref_field("NowPlaying"))
        };
        // (the items' songs come a moment after the list)
        let songs_of = |g: &ObjectGraph, i: u64| -> Option<Vec<u64>> {
            let item = g.get(i)?;
            let list = g.deref(item.field("Tracks"))?.items.clone()?;
            Some(list.iter().filter_map(|tt| g.get(*tt)?.ref_field("Track")).collect())
        };
        self.wait_until(Duration::from_secs(3), |g| {
            listed.iter().all(|i| g.get(*i).is_some_and(|o| o.has_field("TransportItemId")) && songs_of(g, *i).is_some_and(|t| t.iter().all(|t| g.get(*t).is_some_and(|o| o.has_field("TrackId")))))
        })
        .await;
        let g = self.inner.graph.lock().unwrap();
        let id_of = |i: &u64| g.get(*i).and_then(|o| o.long_field("TransportItemId"));
        let now_id = now_item.as_ref().and_then(id_of);
        let k = listed
            .iter()
            .position(|i| Some(*i) == now_item || (now_id.is_some() && id_of(i) == now_id))
            .or_else(|| listed.iter().position(|i| g.get(*i).and_then(|o| o.field("IsCurrent")).and_then(Value::as_bool) == Some(true)))
            .unwrap_or(0);
        let mut out = QueueTracks::default();
        for (n, i) in listed.iter().enumerate().skip(k) {
            let songs = songs_of(&g, *i).unwrap_or_default();
            if n > k {
                out.upcoming += songs.len().max(1);
            }
            for t in songs.iter().filter_map(|t| g.get(*t)) {
                out.ahead_ids.extend(t.long_field("TrackId"));
                out.ahead_ids.extend(t.long_field("LibraryTrackId"));
            }
        }
        Ok(out)
    }

    /// Plays songs (handles) on the zone: now (the queue as it is, the
    /// songs first), next, or at the end of the queue.
    pub async fn play_tracks_how(&self, zone_id: &str, handles: &[String], how: PlayHow) -> Result<(), LibraryError> {
        let oids: Vec<u64> = handles.iter().filter_map(|h| h.parse().ok()).filter(|o| *o != 0).collect();
        if oids.is_empty() {
            return Err(LibraryError::Unconfirmed("no songs to play".into()));
        }
        let (transport, zone, profile) = {
            let g = self.inner.graph.lock().unwrap();
            let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
            (g.first_of_type("Transport").map(|o| o.oid), zone.oid, self.profile_sooid(&g))
        };
        let (Some(transport), Some(profile)) = (transport, profile) else {
            return Err(LibraryError::UnsupportedCore("no Transport service or Profile in the graph".into()));
        };
        let params = self.play_parameters(how)?;
        let mut list = Writer::new();
        list.flex_int(oids.len() as u32);
        for oid in &oids {
            list.long(*oid);
        }
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params).flex_int(list.len() as u32).bytes(list.as_bytes());
        let res = self.inner.remoting.call(transport, SIG_PLAY_TRACKS, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlayTracks".into(), status: res.status }.into());
        }
        Ok(())
    }

    /// `queue_songs`, with a report of what the Core gave (the list, where
    /// the playing item and the start were found, how long each wait took)
    /// for working out a queue that comes out wrong. `played_only` leaves
    /// out the songs to come after the playing item (the queue panel shows
    /// those from the Roon API's queue already), so only the played ones
    /// are waited for.
    pub async fn queue_songs_report(
        &self,
        zone_id: &str,
        start: Option<u64>,
        recent: usize,
        played_only: bool,
    ) -> Result<(Vec<QueueSong>, String), LibraryError> {
        let began = Instant::now();
        let queue = self.queue_object(zone_id)?;
        let listed = self.queue_items(queue).await?;
        let listed_after = began.elapsed();
        let now_item = {
            let g = self.inner.graph.lock().unwrap();
            self.zone_object(&g, zone_id).and_then(|z| z.ref_field("NowPlaying"))
        };
        // The items get their fields after the list itself; the ids are what
        // they're matched by
        self.wait_until(Duration::from_secs(3), |g| {
            let has_id = |i: &u64| g.get(*i).is_some_and(|o| o.has_field("TransportItemId"));
            listed.iter().all(has_id) && now_item.map_or(true, |i| has_id(&i))
        })
        .await;
        let ids_after = began.elapsed();

        let mut report = String::new();
        let (played, upcoming) = {
            let g = self.inner.graph.lock().unwrap();
            let id_of = |i: &u64| g.get(*i).and_then(|o| o.long_field("TransportItemId"));
            let now_id = now_item.as_ref().and_then(id_of);
            // The playing item: the zone's NowPlaying, else the one marked
            // IsCurrent, else the first
            let k = listed
                .iter()
                .position(|i| Some(*i) == now_item || (now_id.is_some() && id_of(i) == now_id))
                .or_else(|| listed.iter().position(|i| g.get(*i).and_then(|o| o.field("IsCurrent")).and_then(Value::as_bool) == Some(true)))
                .unwrap_or(0);
            let start_at = start.and_then(|s| listed.iter().position(|i| id_of(i) == Some(s)));
            let played: Vec<u64> = match start_at {
                Some(from) if from <= k => listed[from..k].to_vec(),
                _ => listed[k.saturating_sub(recent)..k].to_vec(),
            };
            let upcoming: Vec<u64> = if played_only { listed[k..].iter().take(1).copied().collect() } else { listed[k..].to_vec() };

            // What the Core gave, for queue-debug.txt
            use std::fmt::Write as _;
            let item_line = |i: &u64| -> String {
                let Some(o) = g.get(*i) else { return format!("#{i} (not in the graph)") };
                let tracks = g
                    .deref(o.field("Tracks"))
                    .and_then(|l| l.items.clone())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|t| g.get(*t))
                    .map(|tt| {
                        let title = tt.ref_field("Track").and_then(|t| g.get(t)).and_then(|t| t.str_field("Title")).unwrap_or("?");
                        format!(
                            "{title} (current {:?}, seconds played {:?}, last played {})",
                            tt.field("IsCurrent").and_then(Value::as_bool),
                            tt.field("SecondsPlayed").and_then(Value::as_int),
                            tt.has_field("LastPlayed")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(
                    "#{i} id {:?} current {:?} swim {:?}: {tracks}",
                    o.long_field("TransportItemId"),
                    o.field("IsCurrent").and_then(Value::as_bool),
                    o.field("IsFromSwim").and_then(Value::as_bool)
                )
            };
            let _ = writeln!(report, "zone {zone_id}, start {start:?}, recent {recent}, played only {played_only}");
            if let Some(q) = g.get(queue) {
                let mut fields: Vec<String> = q.fields.iter().map(|(k, v)| format!("{k} = {v:?}")).collect();
                fields.sort();
                let _ = writeln!(report, "Queue #{queue} {}: {}", q.type_name, fields.join(", "));
            }
            let _ = writeln!(report, "NowPlaying {now_item:?} id {now_id:?}");
            let _ = writeln!(report, "GetItems: {} items, the playing one at {k}, the start at {start_at:?}", listed.len());
            let mut shown: Vec<usize> = (0..listed.len().min(3)).chain(k.saturating_sub(8)..(k + 3).min(listed.len())).collect();
            if let Some(s) = start_at {
                shown.push(s);
            }
            shown.sort_unstable();
            shown.dedup();
            for n in shown {
                let _ = writeln!(report, "  [{n}] {}", item_line(&listed[n]));
            }
            (played, upcoming)
        };

        // An item's songs: (its TransportTracks' tracks, whether one is the
        // playing one, whether it was ever played)
        fn songs_of(g: &ObjectGraph, item: u64) -> Option<Vec<(u64, bool, bool)>> {
            let list = g.deref(g.get(item)?.field("Tracks"))?;
            list.items
                .as_ref()?
                .iter()
                .map(|t| {
                    let t = g.get(*t)?;
                    let heard = t.field("SecondsPlayed").and_then(Value::as_int).is_some_and(|s| s > 0) || t.has_field("LastPlayed");
                    Some((t.ref_field("Track")?, t.field("IsCurrent").and_then(Value::as_bool) == Some(true), heard))
                })
                .collect()
        }
        let items: Vec<u64> = played.iter().chain(upcoming.iter()).copied().collect();
        let complete = self
            .wait_until(Duration::from_secs(3), |g| {
                items.iter().all(|i| songs_of(g, *i).is_some_and(|songs| songs.iter().all(|(t, _, _)| g.get(*t).is_some_and(|o| o.has_field("Title")))))
            })
            .await;
        let songs_after = began.elapsed();

        let g = self.inner.graph.lock().unwrap();
        let item_id_of = |item: u64| g.get(item).and_then(|o| o.long_field("TransportItemId"));
        let describe = |track: u64, played: bool, skipped: bool, current: bool, item: u64| -> Option<QueueSong> {
            let t = g.get(track).filter(|t| t.has_field("Title"))?;
            let album = g.deref(t.field("Album"));
            Some(QueueSong {
                handle: t.oid.to_string(),
                title: t.str_field("Title").unwrap_or_default().to_string(),
                artist: strip_credit_markup(album.and_then(|a| a.str_field("PerformedBy")).unwrap_or_default()),
                image_key: album.and_then(|a| image_key(&g, a, "Image")),
                length_seconds: t.field("LengthSeconds").and_then(Value::as_int),
                played,
                skipped,
                current,
                item_id: item_id_of(item),
            })
        };
        let mut out = Vec::new();
        for item in &played {
            out.extend(songs_of(&g, *item).unwrap_or_default().into_iter().filter_map(|(t, _, heard)| describe(t, true, !heard, false, *item)));
        }
        for (n, item) in upcoming.iter().enumerate() {
            let songs = songs_of(&g, *item).unwrap_or_default();
            // In the playing item (an album, say): the songs before the
            // playing one are played too
            let now = if n == 0 { songs.iter().position(|(_, now, _)| *now).unwrap_or(0) } else { usize::MAX };
            for (i, (t, _, heard)) in songs.into_iter().enumerate() {
                let (played, current) = if n == 0 { (i < now, i == now) } else { (false, false) };
                out.extend(describe(t, played, played && !heard, current, *item));
            }
        }
        let summary = format!(
            "[queue] {} played and {} upcoming items, {} songs, {} of them played; the list after {} ms, its ids after {} ms, the songs after {} ms{}",
            played.len(),
            upcoming.len(),
            out.len(),
            out.iter().filter(|s| s.played).count(),
            listed_after.as_millis(),
            ids_after.as_millis(),
            songs_after.as_millis(),
            if complete { "" } else { " (gave up waiting for some songs)" }
        );
        println!("{summary}");
        report.push_str(&summary);
        report.push('\n');
        Ok((out, report))
    }

    // --- playlists ---

    /// Roon's playlists, as its Playlists page lists them.
    pub fn playlists(&self) -> Result<Vec<PlaylistCard>, LibraryError> {
        let g = self.inner.graph.lock().unwrap();
        let service = g
            .first_of_type("Playlists")
            .ok_or_else(|| LibraryError::UnsupportedCore("no Playlists service in the graph".into()))?;
        let list = g.deref(service.field("AllPlaylists")).and_then(|l| l.items.clone()).unwrap_or_default();
        Ok(list.iter().filter_map(|oid| g.get(*oid)).filter(|p| p.has_field("Name")).map(|p| describe_playlist(&g, p)).collect())
    }

    /// A track's Roon id ("TrackId", as `TrackInfo.track_id` gives it) by
    /// its handle.
    pub fn track_id_of(&self, handle: &str) -> Option<String> {
        let oid: u64 = handle.parse().ok()?;
        let g = self.inner.graph.lock().unwrap();
        g.get(oid)?.long_field("TrackId").map(|id| id.to_string())
    }

    /// A track's title by its handle.
    pub fn track_title_of(&self, handle: &str) -> Option<String> {
        let oid: u64 = handle.parse().ok()?;
        let g = self.inner.graph.lock().unwrap();
        g.get(oid)?.str_field("Title").map(str::to_string)
    }

    /// A track's album cover (its image key), by the track's handle.
    pub fn track_image_of(&self, handle: &str) -> Option<String> {
        let oid: u64 = handle.parse().ok()?;
        let g = self.inner.graph.lock().unwrap();
        let track = g.get(oid)?;
        g.deref(track.field("Album")).and_then(|album| image_key(&g, album, "Image")).or_else(|| image_key(&g, track, "Image"))
    }

    /// One playlist's current state by its handle.
    pub fn playlist_card(&self, handle: &str) -> Option<PlaylistCard> {
        let oid: u64 = handle.parse().ok()?;
        let g = self.inner.graph.lock().unwrap();
        let p = g.get(oid)?;
        Some(describe_playlist(&g, p))
    }

    /// A new playlist of the given tracks (handles), in that order, named
    /// `name` (Roon allows duplicate names; this doesn't overwrite). Waits
    /// for the Core to list it.
    pub async fn create_playlist(&self, name: &str, track_handles: &[String]) -> Result<PlaylistCard, LibraryError> {
        self.create_playlist_described(name, "", track_handles).await
    }

    /// `create_playlist` with a description (shown under the name on
    /// Roon's playlist page).
    pub async fn create_playlist_described(&self, name: &str, description: &str, track_handles: &[String]) -> Result<PlaylistCard, LibraryError> {
        let (service, profile, media_ids) = {
            let g = self.inner.graph.lock().unwrap();
            let service = g.first_of_type("Playlists").map(|o| o.oid);
            let profile = self.profile_object(&g).map(|o| o.oid);
            let mut ids = Vec::new();
            for handle in track_handles {
                let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad track handle".into()))?;
                let track = g.get(oid).ok_or_else(|| LibraryError::Unconfirmed("unknown track handle".into()))?;
                let id = track
                    .long_field("TrackId")
                    .ok_or_else(|| LibraryError::Unconfirmed(format!("\"{}\" has no track id", track.str_field("Title").unwrap_or_default())))?;
                ids.push(id);
            }
            (service, profile, ids)
        };
        let (Some(service), Some(profile)) = (service, profile) else {
            return Err(LibraryError::UnsupportedCore("no Playlists service or Profile in the graph".into()));
        };
        if media_ids.is_empty() {
            return Err(LibraryError::Unconfirmed("no tracks for the playlist".into()));
        }
        let before: Vec<u64> = self.playlists()?.iter().filter_map(|p| p.handle.parse().ok()).collect();
        // Profile, name, description, overwrite on a duplicate name, the
        // media ids (a length-prefixed collection of longs)
        let mut ids = Writer::new();
        ids.flex_int(media_ids.len() as u32);
        for id in &media_ids {
            ids.long(*id);
        }
        let mut args = Writer::new();
        args.long(profile).string(Some(name)).string(Some(description)).boolean(false).flex_int(ids.len() as u32).bytes(ids.as_bytes());
        let res = self.inner.remoting.call(service, SIG_CREATE_PLAYLIST, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "CreatePlaylistFromMediaIds".into(), status: res.status }.into());
        }
        // The answer names the playlist object; it's listed (with its count)
        // a moment later
        let created = Reader::new(&res.payload).flex_long().ok().filter(|oid| *oid != 0);
        let listed = |g: &ObjectGraph| -> Option<u64> {
            let service = g.first_of_type("Playlists")?;
            let list = g.deref(service.field("AllPlaylists"))?.items.clone()?;
            list.iter()
                .copied()
                .find(|oid| {
                    created == Some(*oid)
                        || (!before.contains(oid) && g.get(*oid).is_some_and(|p| p.str_field("Name") == Some(name)))
                })
                .filter(|oid| g.get(*oid).is_some_and(|p| p.has_field("Name")))
        };
        self.wait_until(Duration::from_secs(5), |g| listed(g).is_some()).await;
        let g = self.inner.graph.lock().unwrap();
        let oid = listed(&g).ok_or_else(|| LibraryError::Unconfirmed(format!("Roon said ok, but \"{name}\" isn't listed")))?;
        Ok(describe_playlist(&g, g.get(oid).unwrap()))
    }

    /// Renames a playlist (by handle). Waits for the new name to show.
    pub async fn rename_playlist(&self, handle: &str, name: &str) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad playlist handle".into()))?;
        let service = {
            let g = self.inner.graph.lock().unwrap();
            if g.get(oid).is_none() {
                return Err(LibraryError::Unconfirmed("unknown playlist handle".into()));
            }
            g.first_of_type("Playlists").map(|o| o.oid)
        };
        let service = service.ok_or_else(|| LibraryError::UnsupportedCore("no Playlists service in the graph".into()))?;
        let mut args = Writer::new();
        args.long(oid).string(Some(name));
        let res = self.inner.remoting.call(service, SIG_RENAME_PLAYLIST, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "Playlists.Rename".into(), status: res.status }.into());
        }
        self.wait_until(Duration::from_secs(5), |g| g.get(oid).is_some_and(|p| p.str_field("Name") == Some(name))).await;
        Ok(())
    }

    /// Deletes a playlist (by handle). Waits for it to leave the list.
    pub async fn delete_playlist(&self, handle: &str) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad playlist handle".into()))?;
        let service = {
            let g = self.inner.graph.lock().unwrap();
            if g.get(oid).is_none() {
                return Err(LibraryError::Unconfirmed("unknown playlist handle".into()));
            }
            g.first_of_type("Playlists").map(|o| o.oid)
        };
        let service = service.ok_or_else(|| LibraryError::UnsupportedCore("no Playlists service in the graph".into()))?;
        let mut args = Writer::new();
        args.long(oid);
        let res = self.inner.remoting.call(service, SIG_DELETE_PLAYLIST, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "Playlists.Delete".into(), status: res.status }.into());
        }
        self.wait_until(Duration::from_secs(5), |g| {
            g.first_of_type("Playlists")
                .and_then(|s| g.deref(s.field("AllPlaylists")))
                .and_then(|l| l.items.as_ref())
                .is_some_and(|items| !items.contains(&oid))
        })
        .await;
        Ok(())
    }

    /// Plays a playlist (by handle) on the zone from its start, right now.
    pub async fn play_playlist(&self, zone_id: &str, handle: &str) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad playlist handle".into()))?;
        let (transport, zone, profile) = {
            let g = self.inner.graph.lock().unwrap();
            let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
            if g.get(oid).is_none() {
                return Err(LibraryError::Unconfirmed("unknown playlist handle".into()));
            }
            (g.first_of_type("Transport").map(|o| o.oid), zone.oid, self.profile_sooid(&g))
        };
        let (Some(transport), Some(profile)) = (transport, profile) else {
            return Err(LibraryError::UnsupportedCore("no Transport service or Profile in the graph".into()));
        };
        // Zone, profile, default PlayParameters, the playlist, from no particular item
        let params = self.inner.remoting.inline_struct("Sooloos.Broker.Api.PlayParameters", &[])?;
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params).long(oid).long(0);
        let res = self.inner.remoting.call(transport, SIG_PLAY_PLAYLIST, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlayPlaylist".into(), status: res.status }.into());
        }
        Ok(())
    }

    /// Plays a playlist from one of its items (`item`: a PlaylistEntry's
    /// item handle), or from the start.
    pub async fn play_playlist_from(&self, zone_id: &str, handle: &str, item: Option<&str>) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad playlist handle".into()))?;
        let from: u64 = match item {
            Some(i) => i.parse().map_err(|_| LibraryError::Unconfirmed("bad playlist item handle".into()))?,
            None => 0,
        };
        let (transport, zone, profile) = {
            let g = self.inner.graph.lock().unwrap();
            let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
            if g.get(oid).is_none() {
                return Err(LibraryError::Unconfirmed("unknown playlist handle".into()));
            }
            (g.first_of_type("Transport").map(|o| o.oid), zone.oid, self.profile_sooid(&g))
        };
        let (Some(transport), Some(profile)) = (transport, profile) else {
            return Err(LibraryError::UnsupportedCore("no Transport service or Profile in the graph".into()));
        };
        let params = self.inner.remoting.inline_struct("Sooloos.Broker.Api.PlayParameters", &[])?;
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params).long(oid).long(from);
        let res = self.inner.remoting.call(transport, SIG_PLAY_PLAYLIST, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlayPlaylist".into(), status: res.status }.into());
        }
        Ok(())
    }

    /// A playlist's songs, in order (Playlist::GetItems: a Query of
    /// PlaylistItems, each with its Track).
    pub async fn playlist_tracks(&self, handle: &str) -> Result<Vec<PlaylistEntry>, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad playlist handle".into()))?;
        if self.inner.graph.lock().unwrap().get(oid).is_none() {
            return Err(LibraryError::Unconfirmed("unknown playlist handle".into()));
        }
        let res = self.inner.remoting.call(oid, SIG_PLAYLIST_ITEMS, &[]).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "Playlist::GetItems".into(), status: res.status }.into());
        }
        let query = Reader::new(&res.payload)
            .flex_long()
            .ok()
            .filter(|q| *q != 0)
            .ok_or_else(|| LibraryError::Unconfirmed("the playlist's items did not come".into()))?;
        // The list, then every item's song with its title
        let ready = |g: &ObjectGraph| -> bool {
            let Some(q) = g.get(query) else { return false };
            let Some(items) = q.items.as_ref() else { return false };
            let want = q.field("Count").and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            items.len() >= want
                && items.iter().all(|i| g.get(*i).and_then(|it| g.deref(it.field("Track"))).is_some_and(|t| t.has_field("Title")))
        };
        self.wait_until(Duration::from_secs(6), ready).await;
        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        let items = g.get(query).and_then(|q| q.items.clone()).unwrap_or_default();
        Ok(items
            .into_iter()
            .filter_map(|i| {
                let item = g.get(i)?;
                let track = g.deref(item.field("Track"))?;
                Some(PlaylistEntry {
                    item_handle: item.oid.to_string(),
                    entry: TrackEntry { handle: track.oid.to_string(), track: describe_library_track(&g, track, profile.as_deref()) },
                })
            })
            .collect())
    }

    /// Takes songs out of a playlist (Playlists::RemoveItems; no answer
    /// comes, so the playlist is read again to see them gone).
    pub async fn remove_playlist_items(&self, handle: &str, items: &[String]) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad playlist handle".into()))?;
        let items: Vec<u64> = items.iter().filter_map(|i| i.parse().ok()).collect();
        if items.is_empty() {
            return Ok(());
        }
        let service = self
            .inner
            .graph
            .lock()
            .unwrap()
            .first_of_type("Playlists")
            .map(|o| o.oid)
            .ok_or_else(|| LibraryError::UnsupportedCore("no Playlists service in the graph".into()))?;
        let mut list = Writer::new();
        list.flex_int(items.len() as u32);
        for i in &items {
            list.long(*i);
        }
        let mut args = Writer::new();
        args.long(oid).flex_int(list.len() as u32).bytes(list.as_bytes());
        self.inner.remoting.call_no_reply(service, SIG_PLAYLIST_REMOVE_ITEMS, args.as_bytes())?;
        Ok(())
    }

    /// The profile's tags (Roon's Tags page).
    pub async fn tags(&self) -> Result<Vec<TagCard>, LibraryError> {
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_object(&g).map(|o| o.oid))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let mut args = Writer::new();
        args.long(profile);
        let res = self.inner.remoting.call(library, SIG_ALL_TAGS, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetAllTagLites".into(), status: res.status }.into());
        }
        let list = Reader::new(&res.payload).flex_long().ok().filter(|l| *l != 0).ok_or_else(|| LibraryError::Unconfirmed("the tags did not come".into()))?;
        let ready = |g: &ObjectGraph| g.get(list).and_then(|l| l.items.as_ref()).is_some_and(|items| items.iter().all(|i| g.get(*i).is_some_and(|t| t.has_field("Name"))));
        self.wait_until(Duration::from_secs(5), ready).await;
        let g = self.inner.graph.lock().unwrap();
        let items = g.get(list).and_then(|l| l.items.clone()).unwrap_or_default();
        if let Some(first) = items.first().and_then(|i| g.get(*i)) {
            let mut names: Vec<&str> = first.fields.keys().map(|k| k.rsplit("::").next().unwrap_or(k)).collect();
            names.sort_unstable();
            println!("[library] {} fields: {}", first.short_type(), names.join(", "));
        }
        let mut tags: Vec<TagCard> = items
            .into_iter()
            .filter_map(|i| g.get(i))
            .filter(|t| t.has_field("Name"))
            .map(|t| TagCard {
                handle: t.oid.to_string(),
                name: t.str_field("Name").unwrap_or_default().to_string(),
                count: ["ItemCount", "Count", "TotalCount"].iter().find_map(|n| t.field(n).and_then(Value::as_int)),
                image_key: image_key(&g, t, "Image"),
            })
            .collect();
        tags.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(tags)
    }

    /// What a tag holds: its albums, artists and songs (Library::GetTag,
    /// then ExplicitTag::GetItems; a smart tag says so instead).
    pub async fn tag_contents(&self, handle: &str) -> Result<TagContents, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad tag handle".into()))?;
        let (library, profile, name) = {
            let g = self.inner.graph.lock().unwrap();
            let name = g.get(oid).and_then(|t| t.str_field("Name")).unwrap_or_default().to_string();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_object(&g).map(|o| o.oid), name)
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let mut args = Writer::new();
        args.long(profile).long(oid);
        let res = self.inner.remoting.call(library, SIG_GET_TAG, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetTag".into(), status: res.status }.into());
        }
        let tag = Reader::new(&res.payload).flex_long().ok().filter(|t| *t != 0).ok_or_else(|| LibraryError::Unconfirmed("the tag did not come".into()))?;
        self.wait_until(Duration::from_secs(3), |g| g.get(tag).is_some()).await;
        let kind = self.inner.graph.lock().unwrap().get(tag).map(|t| t.short_type().to_string()).unwrap_or_default();
        println!("[library] tag {name}: {kind}");
        if kind.contains("Smart") {
            return Ok(TagContents { name, smart: true, ..Default::default() });
        }
        let field = |n: &'static str, prop_type: PropertyType, value: Vec<u8>| StructField { name: n, prop_type, value };
        let mut lang = Writer::new();
        lang.string(Some("en"));
        let criteria = self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.TagItemQueryCriteria",
            &[field("string Sooloos.Broker.Api.TagItemQueryCriteria::UiLanguage", PropertyType::String, lang.into_bytes())],
        )?;
        let res = self.inner.remoting.call(tag, SIG_TAG_ITEMS, &criteria).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "ExplicitTag::GetItems".into(), status: res.status }.into());
        }
        let query = Reader::new(&res.payload).flex_long().ok().filter(|q| *q != 0).ok_or_else(|| LibraryError::Unconfirmed("the tag's items did not come".into()))?;
        // What an item points at (by reference or inline)
        const KINDS: [&str; 3] = ["Album", "Performer", "Track"];
        let targets = |g: &ObjectGraph| -> Vec<(&'static str, u64)> {
            let Some(q) = g.get(query) else { return Vec::new() };
            let mut out = Vec::new();
            for i in q.items.iter().flatten() {
                if let Some(item) = g.get(*i) {
                    out.push(KINDS.iter().find_map(|k| item.ref_field(k).map(|r| (*k, r))).unwrap_or(("", 0)));
                }
            }
            for s in q.structs.iter().flatten() {
                out.push(KINDS.iter().find_map(|k| s.field(k).and_then(Value::as_ref_id).map(|r| (*k, r))).unwrap_or(("", 0)));
            }
            out
        };
        let ready = |g: &ObjectGraph| -> bool {
            let Some(q) = g.get(query) else { return false };
            let want = q.field("Count").and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let t = targets(g);
            t.len() >= want && t.iter().all(|(k, r)| k.is_empty() || g.get(*r).is_some_and(|o| o.has_field("Title") || o.has_field("Name")))
        };
        self.wait_until(Duration::from_secs(6), ready).await;
        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        if let Some(first) = g.get(query).and_then(|q| q.items.as_ref()?.first().copied()).and_then(|i| g.get(i)) {
            let mut names: Vec<&str> = first.fields.keys().map(|k| k.rsplit("::").next().unwrap_or(k)).collect();
            names.sort_unstable();
            println!("[library] {} fields: {}", first.short_type(), names.join(", "));
        }
        let mut out = TagContents { name, ..Default::default() };
        for (kind, r) in targets(&g) {
            let Some(o) = g.get(r).filter(|_| !kind.is_empty()) else {
                out.other += 1;
                continue;
            };
            match kind {
                "Album" => out.albums.push(describe_album_card(&g, o, profile.as_deref())),
                "Performer" => out.artists.push(describe_artist_card(&g, o, profile.as_deref())),
                _ => out.tracks.push(TrackEntry { handle: o.oid.to_string(), track: describe_library_track(&g, o, profile.as_deref()) }),
            }
        }
        Ok(out)
    }

    /// Listen Later: the albums, artists and songs the profile put aside,
    /// newest first (the ones still waiting).
    pub async fn listen_later(&self) -> Result<Vec<ListenLaterEntry>, LibraryError> {
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_object(&g).map(|o| o.oid))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        // Profile, the language preferences (a list of strings: "en")
        let mut langs = Writer::new();
        langs.flex_int(1).string(Some("en"));
        let mut args = Writer::new();
        args.long(profile).flex_int(langs.len() as u32).bytes(langs.as_bytes());
        let res = self.inner.remoting.call(library, SIG_LISTEN_LATER, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetListenLater".into(), status: res.status }.into());
        }
        let list = Reader::new(&res.payload).flex_long().ok().filter(|l| *l != 0).ok_or_else(|| LibraryError::Unconfirmed("Listen Later did not come".into()))?;
        let points = |s: &InlineStruct| -> Option<(&'static str, u64)> {
            [("album", "Album"), ("artist", "Performer"), ("track", "Track")]
                .iter()
                .find_map(|(kind, field)| s.field(field).and_then(Value::as_ref_id).map(|r| (*kind, r)))
        };
        let ready = |g: &ObjectGraph| -> bool {
            let Some(l) = g.get(list) else { return false };
            let (Some(_), Some(structs)) = (l.items.as_ref(), l.structs.as_ref()) else { return false };
            structs.iter().filter_map(points).all(|(_, r)| g.get(r).is_some_and(|o| o.has_field("Title") || o.has_field("Name")))
        };
        self.wait_until(Duration::from_secs(6), ready).await;
        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        let structs = g.get(list).and_then(|l| l.structs.clone()).unwrap_or_default();
        if let Some(first) = structs.first() {
            let mut names: Vec<&str> = first.fields.keys().map(|k| k.rsplit("::").next().unwrap_or(k)).collect();
            names.sort_unstable();
            println!("[library] ListenLaterItem fields: {}", names.join(", "));
        }
        let mut out: Vec<ListenLaterEntry> = structs
            .iter()
            // (Pending, or no state given; the played ones are done)
            .filter(|s| s.field("State").and_then(Value::as_int).is_none_or(|st| st == 1))
            .filter_map(|s| {
                let (kind, r) = points(s)?;
                let o = g.get(r)?;
                let added_at = s.field("Timestamp").and_then(ticks_to_ms);
                Some(ListenLaterEntry {
                    kind: kind.into(),
                    added_at,
                    album: (kind == "album").then(|| describe_album_card(&g, o, profile.as_deref())),
                    artist: (kind == "artist").then(|| describe_artist_card(&g, o, profile.as_deref())),
                    track: (kind == "track").then(|| TrackEntry { handle: o.oid.to_string(), track: describe_library_track(&g, o, profile.as_deref()) }),
                })
            })
            .collect();
        out.sort_by(|a, b| b.added_at.cmp(&a.added_at));
        Ok(out)
    }

    /// Takes something out of Listen Later (its state set to None):
    /// `kind` "album", "artist" or "track", `handle` its object.
    pub async fn listen_later_remove(&self, kind: &str, handle: &str) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad handle".into()))?;
        let sig = match kind {
            "album" => SIG_LISTEN_LATER_ALBUM,
            "artist" => SIG_LISTEN_LATER_PERFORMER,
            _ => SIG_LISTEN_LATER_TRACK,
        };
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_sooid(&g))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let mut args = Writer::new();
        args.sooid(&profile).long(oid).flex_int(0); // ListenLaterState::None
        let res = self.inner.remoting.call(library, sig, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "SetListenLaterState".into(), status: res.status }.into());
        }
        Ok(())
    }

    /// Plays the zone's queue from one of its items (the Core's
    /// TransportItemId), a played one too.
    pub async fn queue_play_from(&self, zone_id: &str, item_id: u64) -> Result<(), LibraryError> {
        let queue = self.queue_object(zone_id)?;
        let mut args = Writer::new();
        args.long(item_id);
        self.inner.remoting.call_no_reply(queue, SIG_QUEUE_PLAY_FROM_ID, args.as_bytes()).map_err(LibraryError::from)?;
        Ok(())
    }

    /// Empties the zone's queue (Queue::RemoveAll), played items too.
    pub async fn queue_clear(&self, zone_id: &str) -> Result<(), LibraryError> {
        let queue = self.queue_object(zone_id)?;
        self.inner.remoting.call_no_reply(queue, SIG_QUEUE_REMOVE_ALL, &[]).map_err(LibraryError::from)?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        Ok(())
    }

    /// Starts Roon Radio on the zone from the seeds (Roon's own "Start
    /// Radio", which the app offers on one song, album or artist), optionally
    /// staying inside the library; see `RadioSeeds` for which mixes the Core
    /// takes. Answers how many songs the Core queued, when it says.
    pub async fn play_radio(&self, zone_id: &str, seeds: &RadioSeeds) -> Result<Option<i32>, LibraryError> {
        let handles = |list: &[String], what: &str| -> Result<Vec<u64>, LibraryError> {
            list.iter()
                .map(|h| h.parse::<u64>().map_err(|_| LibraryError::Unconfirmed(format!("bad {what} handle"))))
                .collect()
        };
        let tracks = handles(&seeds.tracks, "track")?;
        let albums = handles(&seeds.albums, "album")?;
        let artists = handles(&seeds.artists, "artist")?;
        if tracks.is_empty() && albums.is_empty() && artists.is_empty() && seeds.genres.is_empty() {
            return Err(LibraryError::Unconfirmed("nothing to start the radio from".into()));
        }
        let (transport, zone, profile) = {
            let g = self.inner.graph.lock().unwrap();
            let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
            if tracks.iter().chain(&albums).chain(&artists).any(|oid| g.get(*oid).is_none()) {
                return Err(LibraryError::Unconfirmed("unknown seed handle".into()));
            }
            (g.first_of_type("Transport").map(|o| o.oid), zone.oid, self.profile_sooid(&g))
        };
        let (Some(transport), Some(profile)) = (transport, profile) else {
            return Err(LibraryError::UnsupportedCore("no Transport service or Profile in the graph".into()));
        };

        // List members of a struct: a length prefix around the count and the
        // items (object references as longs, strings as strings)
        let ref_list = |oids: &[u64]| -> Vec<u8> {
            let mut items = Writer::new();
            items.flex_int(oids.len() as u32);
            for oid in oids {
                items.long(*oid);
            }
            let mut out = Writer::new();
            out.integer(items.len() as i32).bytes(items.as_bytes());
            out.into_bytes()
        };
        let text_list = |texts: &[String]| -> Vec<u8> {
            let mut items = Writer::new();
            items.flex_int(texts.len() as u32);
            for text in texts {
                items.string(Some(text));
            }
            let mut out = Writer::new();
            out.integer(items.len() as i32).bytes(items.as_bytes());
            out.into_bytes()
        };
        let mut priority = Writer::new();
        priority.flex_int(0); // SwimPriority::Now
        let mut yes = Writer::new();
        yes.boolean(true);
        let mut fields = vec![StructField {
            name: "Sooloos.Broker.Api.SwimPriority Sooloos.Broker.Api.SwimParameters::Priority",
            prop_type: PropertyType::Enum,
            value: priority.into_bytes(),
        }];
        // The Core's own broker id (the Core id in .NET GUID byte order), as
        // the Roon app sends it
        if let Some(broker_id) = broker_id_from_core_id(&self.inner.opts.core_id) {
            let mut broker = Writer::new();
            broker.guid(&broker_id);
            fields.push(StructField {
                name: "System.Guid Sooloos.Broker.Api.SwimParameters::BrokerId",
                prop_type: PropertyType::Guid,
                value: broker.into_bytes(),
            });
        }
        if !tracks.is_empty() {
            fields.push(StructField {
                name: "System.Collections.Generic.IList<Sooloos.Broker.Api.TrackBase> Sooloos.Broker.Api.SwimParameters::Tracks",
                prop_type: PropertyType::Object,
                value: ref_list(&tracks),
            });
        }
        if !albums.is_empty() {
            fields.push(StructField {
                name: "System.Collections.Generic.IList<Sooloos.Broker.Api.AlbumBase> Sooloos.Broker.Api.SwimParameters::Albums",
                prop_type: PropertyType::Object,
                value: ref_list(&albums),
            });
        }
        if !artists.is_empty() {
            fields.push(StructField {
                name: "System.Collections.Generic.IList<Sooloos.Broker.Api.PerformerBase> Sooloos.Broker.Api.SwimParameters::Performers",
                prop_type: PropertyType::Object,
                value: ref_list(&artists),
            });
        }
        if !seeds.genres.is_empty() {
            fields.push(StructField {
                name: "System.Collections.Generic.IList<string> Sooloos.Broker.Api.SwimParameters::Genres",
                prop_type: PropertyType::Object,
                value: text_list(&seeds.genres),
            });
        }
        if seeds.library_only {
            fields.push(StructField {
                name: "bool Sooloos.Broker.Api.SwimParameters::LimitToLibrary",
                prop_type: PropertyType::Bool,
                value: yes.as_bytes().to_vec(),
            });
        }
        if seeds.limit_to_seed {
            fields.push(StructField {
                name: "bool Sooloos.Broker.Api.SwimParameters::LimitToSeed",
                prop_type: PropertyType::Bool,
                value: yes.as_bytes().to_vec(),
            });
        }
        // Start on a song from the seeds right away, as Roon's own "Start
        // radio" does: with only artists or albums as seeds and no song to
        // play first, the zone stopped and waited (2026-10-16)
        if tracks.is_empty() {
            fields.push(StructField {
                name: "bool Sooloos.Broker.Api.SwimParameters::PreplayTrackFromSeed",
                prop_type: PropertyType::Bool,
                value: yes.as_bytes().to_vec(),
            });
        }
        let params = self.inner.remoting.inline_struct("Sooloos.Broker.Api.SwimParameters", &fields)?;
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params);
        let res = self.inner.remoting.call(transport, SIG_PLAY_SWIM, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlaySwim".into(), status: res.status }.into());
        }
        // The Core's PlayFeedback says how many songs it queued (with
        // LimitToSeed: every song of the seeds it found, shuffled)
        let queued = {
            let g = self.inner.graph.lock().unwrap();
            match g.decode_return_value(&res.payload) {
                Ok(Value::Inline(feedback)) => feedback.field("ItemCount").and_then(Value::as_int),
                _ => None,
            }
        };
        println!(
            "[radio] PlaySwim: {} tracks, {} albums, {} artists, library only {}, only seeds {}; queued {queued:?}",
            tracks.len(),
            albums.len(),
            artists.len(),
            seeds.library_only,
            seeds.limit_to_seed
        );
        Ok(queued)
    }

    /// Roon's Recent Activity (Library::GetRecents) for `mode`, newest
    /// first. The mode strings aren't known; see the Discover builder, which
    /// tries a few. Also gives a line describing what came back, for the log.
    pub async fn recents(&self, mode: &str) -> Result<(Vec<RecentEntry>, String), LibraryError> {
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_object(&g).map(|o| o.oid))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let mut args = Writer::new();
        args.long(profile).string(Some(mode));
        let res = self.inner.remoting.call(library, SIG_GET_RECENTS, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "GetRecents".into(), status: res.status }.into());
        }
        if res.payload.is_empty() {
            return Ok((Vec::new(), "nothing returned".into()));
        }
        let value = self
            .inner
            .graph
            .lock()
            .unwrap()
            .decode_return_value(&res.payload)
            .map_err(|_| LibraryError::Unconfirmed("could not read what GetRecents returned".into()))?;
        // (a list pushed by reference: wait for its items, then for the
        // albums, artists and songs they point at)
        if let Value::Ref(list) = value {
            self.wait_until(Duration::from_secs(3), |g| g.get(list).is_some_and(|l| l.items.is_some() || l.structs.is_some())).await;
            let pointed: Vec<u64> = {
                let g = self.inner.graph.lock().unwrap();
                let mut out = Vec::new();
                if let Some(l) = g.get(list) {
                    for s in l.structs.iter().flatten() {
                        for name in ["Album", "Performer", "Track", "Playlist"] {
                            out.extend(s.field(name).and_then(Value::as_ref_id));
                        }
                    }
                    out.extend(l.items.iter().flatten().copied());
                }
                out
            };
            self.wait_until(Duration::from_secs(3), |g| pointed.iter().all(|oid| g.get(*oid).is_some_and(|o| !o.fields.is_empty()))).await;
        }
        let g = self.inner.graph.lock().unwrap();
        let read = |get: &dyn Fn(&str) -> Option<Value>| -> RecentEntry {
            const TICKS_MASK: u64 = (1 << 62) - 1;
            const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
            let at = get("Time")
                .and_then(|v| v.as_long())
                .map(|t| ((t & TICKS_MASK) as i64 - UNIX_EPOCH_TICKS) / 10_000)
                .filter(|ms| *ms > 0)
                .unwrap_or(0);
            let obj = |name: &str| get(name).and_then(|v| g.deref(Some(&v)).cloned());
            let (album, performer, track, playlist) = (obj("Album"), obj("Performer"), obj("Track"), obj("Playlist"));
            let track_album = track.as_ref().and_then(|t| g.deref(t.field("Album")).cloned());
            let artist = performer
                .as_ref()
                .and_then(|p| p.str_field("Name").map(str::to_string))
                .or_else(|| album.as_ref().or(track_album.as_ref()).and_then(|a| a.str_field("PerformedBy")).map(strip_credit_markup))
                .unwrap_or_default();
            let title = [&album, &track, &playlist, &performer]
                .iter()
                .find_map(|o| o.as_ref().and_then(|o| o.str_field("Title").or_else(|| o.str_field("Name")).map(str::to_string)))
                .or_else(|| get("Text").and_then(|v| v.as_str().map(str::to_string)))
                .unwrap_or_default();
            RecentEntry {
                kind: get("Type").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
                data_type: get("DataType").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
                at,
                count: get("Count").and_then(|v| v.as_int()).unwrap_or(0),
                artist,
                title,
            }
        };
        let mut out = Vec::new();
        let shape;
        match &value {
            Value::Ref(list) => {
                let l = g.get(*list);
                let structs = l.and_then(|l| l.structs.clone()).unwrap_or_default();
                let items = l.and_then(|l| l.items.clone()).unwrap_or_default();
                shape = format!("list by reference: {} inline items, {} referenced", structs.len(), items.len());
                for s in &structs {
                    out.push(read(&|name| s.field(name).cloned()));
                }
                for oid in &items {
                    if let Some(o) = g.get(*oid) {
                        out.push(read(&|name| o.field(name).cloned()));
                    }
                }
            }
            Value::Inline(list) => {
                shape = format!("inline list: {} items", list.items.len());
                for item in &list.items {
                    match item {
                        Value::Inline(s) => out.push(read(&|name| s.field(name).cloned())),
                        Value::Ref(oid) => {
                            if let Some(o) = g.get(*oid) {
                                out.push(read(&|name| o.field(name).cloned()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            other => shape = format!("unexpected answer {other:?}"),
        }
        out.sort_by(|a, b| b.at.cmp(&a.at));
        Ok((out, shape))
    }

    /// An artist's top songs as Roon ranks them (GetOneBoxPerformerTopTracks),
    /// with their albums' titles and live flags.
    pub async fn top_tracks(&self, performer_handle: &str) -> Result<Vec<TopTrack>, LibraryError> {
        let oid: u64 = performer_handle.parse().map_err(|_| LibraryError::Unconfirmed(format!("bad artist handle {performer_handle}")))?;
        let list = self.one_box(SIG_PERFORMER_TOP_TRACKS, "GetOneBoxPerformerTopTracks", Subject::Oid(oid)).await?;
        self.wait_for_lists(std::slice::from_ref(&list), None).await;
        // (and the songs' albums, for their titles and live flags)
        let albums: Vec<u64> = {
            let g = self.inner.graph.lock().unwrap();
            list_members(&g, Some(&list), None)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|t| g.get(t)?.ref_field("Album"))
                .collect()
        };
        self.wait_until(Duration::from_secs(2), |g| albums.iter().all(|a| g.get(*a).is_some_and(|o| o.has_field("Title")))).await;
        let g = self.inner.graph.lock().unwrap();
        Ok(list_members(&g, Some(&list), None)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|oid| g.get(oid))
            .filter(|t| t.has_field("Title"))
            .map(|t| {
                let album = g.deref(t.field("Album"));
                let title = t.str_field("Title").unwrap_or_default().to_string();
                let album_title = album.and_then(|a| a.str_field("Title")).unwrap_or_default().to_string();
                let flagged = album.and_then(|a| a.field("IsLive")).and_then(Value::as_bool) == Some(true);
                TopTrack {
                    handle: t.oid.to_string(),
                    track_id: t.long_field("TrackId").map(|id| id.to_string()),
                    album_key: album
                        .and_then(|a| a.long_field("AlbumId"))
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| album_title.to_lowercase()),
                    live: flagged || looks_live(&title) || looks_live(&album_title),
                    length_seconds: t.field("LengthSeconds").and_then(Value::as_int),
                    track_number: t.field("TrackNumber").and_then(Value::as_int),
                    year: album.and_then(|a| year_of(a.field("OriginalReleaseDate")).or_else(|| year_of(a.field("ReleaseDate")))),
                    album_handle: album.map(|a| a.oid.to_string()),
                    compilation: album.and_then(|a| a.field("IsCompilation")).and_then(Value::as_bool) == Some(true) || looks_compilation(&album_title),
                    album_artist: strip_credit_markup(album.and_then(|a| a.str_field("PerformedBy")).unwrap_or_default()),
                    in_library: t.has_field("LibraryTrackId"),
                    title,
                    album: album_title,
                }
            })
            .collect())
    }

    /// Songs from an artist's own albums released from `from` to `to`
    /// (Roon's discography, its main albums: no live albums, compilations
    /// or demos), the albums nearest the middle of those years first, up to
    /// `albums` albums and `per_album` songs each (no intros, interludes,
    /// skits or the like). For when the artist's best-known songs all come
    /// from other years than the ones wanted.
    pub async fn songs_from_years(&self, performer_handle: &str, from: u32, to: u32, albums: usize, per_album: usize) -> Vec<TopTrack> {
        let Ok(disco) = self.artist_albums_for(performer_handle, None).await else {
            return Vec::new();
        };
        let middle = f64::from(from + to) / 2.0;
        let mut chosen: Vec<&ArtistAlbum> = disco
            .iter()
            .filter(|a| a.group == "main" && a.year.is_some_and(|y| y >= from && y <= to))
            .filter(|a| !looks_live(&a.title) && !looks_compilation(&a.title) && !looks_rough(&a.title))
            .collect();
        let distance = |a: &ArtistAlbum| (f64::from(a.year.unwrap_or(from)) - middle).abs();
        chosen.sort_by(|a, b| distance(a).total_cmp(&distance(b)));
        // (one edition of each album)
        let mut titles: HashSet<String> = HashSet::new();
        chosen.retain(|a| titles.insert(normalized_title(&a.title)));
        let mut out = Vec::new();
        for album in chosen.into_iter().take(albums) {
            let Ok(tracks) = self.album_tracks_for("", &album.handle).await else {
                continue;
            };
            let mut lengths: Vec<i32> = tracks.iter().filter_map(|t| t.length_seconds).collect();
            lengths.sort_unstable();
            let median = lengths.get(lengths.len() / 2).copied().unwrap_or(0);
            let proper = |t: &&AlbumTrack| {
                !looks_rough(&t.title)
                    && !looks_live(&t.title)
                    && t.length_seconds.is_none_or(|l| l >= 105 && !(median > 0 && l * 5 < median * 2) && !(t.track_number == Some(1) && l < 120))
            };
            for t in tracks.iter().filter(proper).take(per_album) {
                out.push(TopTrack {
                    handle: t.handle.clone(),
                    track_id: t.track_id.map(|id| id.to_string()),
                    title: t.title.clone(),
                    album: album.title.clone(),
                    album_key: album.album_id.map(|id| id.to_string()).unwrap_or_else(|| album.title.to_lowercase()),
                    album_artist: album.artist.clone(),
                    in_library: t.in_library,
                    live: false,
                    length_seconds: t.length_seconds,
                    track_number: t.track_number,
                    year: album.year,
                    album_handle: Some(album.handle.clone()),
                    compilation: false,
                });
            }
        }
        out
    }

    /// Deep Dive: an artist's albums to choose songs from, newest first, one
    /// edition of each (the library's copy when there is one): the main
    /// albums (the rest when there are none), no live, compilation or rough
    /// ones, appearances on others' albums left out; only the library's
    /// albums when `library_only`.
    pub async fn albums_to_choose(&self, performer_handle: &str, library_only: bool) -> Vec<ArtistAlbum> {
        let Ok(disco) = self.artist_albums_for(performer_handle, None).await else {
            return Vec::new();
        };
        let fine = |a: &&ArtistAlbum| {
            a.group != "appearance" && !looks_live(&a.title) && !looks_compilation(&a.title) && !looks_rough(&a.title) && (!library_only || a.in_library)
        };
        let mut chosen: Vec<&ArtistAlbum> = disco.iter().filter(fine).filter(|a| a.group == "main").collect();
        if chosen.is_empty() {
            chosen = disco.iter().filter(fine).collect();
        }
        chosen.sort_by(|a, b| b.in_library.cmp(&a.in_library));
        let mut titles: HashSet<String> = HashSet::new();
        chosen.retain(|a| titles.insert(normalized_title(&a.title)));
        chosen.sort_by(|a, b| b.year.cmp(&a.year));
        chosen.into_iter().cloned().collect()
    }

    /// Deep Dive: an album's proper songs in album order (no intros,
    /// interludes, demos or live takes, as `songs_from_years` leaves out;
    /// all of them when that would leave none).
    pub async fn album_songs(&self, album: &ArtistAlbum) -> Vec<TopTrack> {
        let Ok(tracks) = self.album_tracks_for("", &album.handle).await else {
            return Vec::new();
        };
        let mut lengths: Vec<i32> = tracks.iter().filter_map(|t| t.length_seconds).collect();
        lengths.sort_unstable();
        let median = lengths.get(lengths.len() / 2).copied().unwrap_or(0);
        let proper = |t: &&AlbumTrack| {
            !looks_rough(&t.title)
                && !looks_live(&t.title)
                && t.length_seconds.is_none_or(|l| l >= 105 && !(median > 0 && l * 5 < median * 2) && !(t.track_number == Some(1) && l < 120))
        };
        let mut usable: Vec<&AlbumTrack> = tracks.iter().filter(proper).collect();
        if usable.is_empty() {
            usable = tracks.iter().collect();
        }
        usable
            .into_iter()
            .map(|t| TopTrack {
                handle: t.handle.clone(),
                track_id: t.track_id.map(|id| id.to_string()),
                title: t.title.clone(),
                album: album.title.clone(),
                album_key: album.album_id.map(|id| id.to_string()).unwrap_or_else(|| album.title.to_lowercase()),
                album_artist: album.artist.clone(),
                in_library: t.in_library,
                live: false,
                length_seconds: t.length_seconds,
                track_number: t.track_number,
                year: album.year,
                album_handle: Some(album.handle.clone()),
                compilation: false,
            })
            .collect()
    }

    /// Each artist's songs for a mix, in the artists' order: up to
    /// `per_artist` of their top songs (Roon's ranking), one per album
    /// first, the same song (by title) once, live recordings left out
    /// unless asked, only library tracks when asked. Top songs are fetched
    /// eight artists at a time. Also gives how many live recordings were
    /// left out.
    pub async fn pick_artist_songs(&self, artists: &[String], options: &MixOptions) -> (Vec<Vec<String>>, usize) {
        let (picks, live) = self.pick_artist_tracks(artists, options).await;
        (picks.into_iter().map(|v| v.into_iter().map(|t| t.handle).collect()).collect(), live)
    }

    /// `pick_artist_songs` with the songs themselves (title, album, album
    /// artist), for a caller that wants to choose among them.
    pub async fn pick_artist_tracks(&self, artists: &[String], options: &MixOptions) -> (Vec<Vec<TopTrack>>, usize) {
        let per_artist = options.per_artist.clamp(1, 10);
        // Every artist's top songs, eight lookups at a time
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut jobs = tokio::task::JoinSet::new();
        for (i, handle) in artists.iter().enumerate() {
            let (client, handle, gate) = (self.clone(), handle.clone(), gate.clone());
            jobs.spawn(async move {
                let _turn = gate.acquire_owned().await;
                (i, client.top_tracks(&handle).await.unwrap_or_default())
            });
        }
        let mut found: Vec<(usize, Vec<TopTrack>)> = Vec::new();
        while let Some(done) = jobs.join_next().await {
            if let Ok(pair) = done {
                found.push(pair);
            }
        }
        found.sort_by_key(|(i, _)| *i);

        // Each artist's picks: one song per album first, then a second from
        // an album already used if there's room; the same song (by title) once
        // (one entry per artist, in their order, empty when Roon gave none)
        let mut live_skipped = 0;
        let mut picks: Vec<Vec<TopTrack>> = vec![Vec::new(); artists.len()];
        let names: Vec<String> = {
            let g = self.inner.graph.lock().unwrap();
            artists
                .iter()
                .map(|h| h.parse::<u64>().ok().and_then(|oid| g.get(oid)).and_then(|p| p.str_field("Name")).unwrap_or_default().to_lowercase())
                .collect()
        };
        let never: HashSet<&str> = options.never.iter().map(String::as_str).collect();
        let mut rng = Shuffle::new();
        for (i, tracks) in &found {
            let mut seen_titles: HashSet<String> = HashSet::new();
            let name = &names[*i];
            let mut usable: Vec<&TopTrack> = tracks
                .iter()
                .filter(|t| {
                    if t.live && !options.include_live {
                        live_skipped += 1;
                        return false;
                    }
                    if options.own_albums && !name.is_empty() {
                        let credit = t.album_artist.to_lowercase();
                        // (the artist's own album: credited to it alone, not
                        // a split or a compilation it shares)
                        let shared = [" / ", " & ", ", ", " and ", " feat", " with "].iter().any(|sep| credit.contains(sep));
                        if !credit.contains(name.as_str()) || shared {
                            return false;
                        }
                    }
                    if never.contains(t.handle.as_str()) || t.track_id.as_deref().is_some_and(|id| never.contains(id)) {
                        return false;
                    }
                    !options.library_only || t.in_library
                })
                .collect();
            // (demos, rough mixes, remixes, intros, skits and such, and songs
            // off compilations and splits, behind the proper songs)
            let rough = |t: &TopTrack| looks_rough(&t.title) || looks_rough(&t.album) || t.compilation;
            if usable.iter().any(|t| !rough(t)) {
                usable.retain(|t| !rough(t));
            }
            // (intros, interludes, noise pieces: under 1:45, a short track 1,
            // or far shorter than the artist's usual song, when there are
            // proper ones)
            let mut lengths: Vec<i32> = usable.iter().filter_map(|t| t.length_seconds).collect();
            lengths.sort_unstable();
            let median = lengths.get(lengths.len() / 2).copied().unwrap_or(0);
            let short = |t: &TopTrack| {
                t.length_seconds.is_some_and(|l| l < 105 || (median > 0 && l * 5 < median * 2) || (t.track_number == Some(1) && l < 120))
            };
            if usable.iter().any(|t| !short(t)) {
                usable.retain(|t| !short(t));
            }
            // (the same song on two albums, a remaster or an anthology: the
            // earliest release only, so the original album's copy is the one)
            let mut first_release: HashMap<String, (u32, &str)> = HashMap::new();
            for t in &usable {
                let year = t.year.unwrap_or(u32::MAX);
                let e = first_release.entry(normalized_title(&t.title)).or_insert((year, t.handle.as_str()));
                if year < e.0 {
                    *e = (year, t.handle.as_str());
                }
            }
            usable.retain(|t| first_release.get(&normalized_title(&t.title)).map_or(true, |(_, h)| *h == t.handle));
            // (a shuffle within the top few: not always the best known song)
            if options.from_top > 1 && usable.len() > 1 {
                let n = usable.len().min(options.from_top);
                let mut head: Vec<&TopTrack> = usable[..n].to_vec();
                rng.shuffle(&mut head);
                usable.splice(..n, head);
            }
            let mut mine: Vec<&TopTrack> = Vec::new();
            let mut albums: HashMap<&str, usize> = HashMap::new();
            for pass in 1..=2 {
                for t in &usable {
                    if mine.len() >= per_artist {
                        break;
                    }
                    let used = albums.get(t.album_key.as_str()).copied().unwrap_or(0);
                    let song = normalized_title(&t.title);
                    if used >= pass || mine.iter().any(|m| m.handle == t.handle) || seen_titles.contains(&song) {
                        continue;
                    }
                    albums.insert(t.album_key.as_str(), used + 1);
                    seen_titles.insert(song);
                    mine.push(t);
                }
            }
            picks[*i] = mine.into_iter().cloned().collect();
        }
        (picks, live_skipped)
    }

    /// The Wrapped radio: up to `per_artist` of each artist's top songs (one
    /// per album first, no live recordings unless asked), the first
    /// artist's best song first, then round by round in a shuffled artist
    /// order so no artist plays twice in a row, queued on the zone at once
    /// (Roon Radio carries on after it, if the zone has it on). Roon's own
    /// radio takes several artists only as "these artists only", and then
    /// queues every song it has of theirs, whole albums at a time.
    pub async fn play_artist_mix(&self, zone_id: &str, artists: &[String], options: &MixOptions) -> Result<MixPlayed, LibraryError> {
        let per_artist = options.per_artist.clamp(1, 10);
        let (picks, live_skipped) = self.pick_artist_songs(artists, options).await;
        let artists_with_songs = picks.iter().filter(|p| !p.is_empty()).count();
        let mut order: Vec<u64> = order_mix(&picks, per_artist).iter().filter_map(|h| h.parse().ok()).collect();
        order.retain(|oid| *oid != 0);
        if order.is_empty() {
            return Err(LibraryError::Unconfirmed("Roon found no songs by these artists".into()));
        }

        let (transport, zone, profile) = {
            let g = self.inner.graph.lock().unwrap();
            let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
            (g.first_of_type("Transport").map(|o| o.oid), zone.oid, self.profile_sooid(&g))
        };
        let (Some(transport), Some(profile)) = (transport, profile) else {
            return Err(LibraryError::UnsupportedCore("no Transport service or Profile in the graph".into()));
        };
        let params = self.inner.remoting.inline_struct("Sooloos.Broker.Api.PlayParameters", &[])?;
        let mut list = Writer::new();
        list.flex_int(order.len() as u32);
        for oid in &order {
            list.long(*oid);
        }
        // A clean queue first (Queue::RemoveAll), so the radio is a queue of
        // its own: Roon keeps every song ever queued before the playing one
        let queue = self.queue_object(zone_id).ok();
        let mut kept = None;
        if let Some(queue) = queue {
            let before = self.queue_items(queue).await.ok().map(|a| a.len());
            let _ = self.inner.remoting.call_no_reply(queue, SIG_QUEUE_REMOVE_ALL, &[]);
            tokio::time::sleep(Duration::from_millis(400)).await;
            kept = Some((before, self.queue_items(queue).await.ok().map(|a| a.len())));
        }
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params).flex_int(list.len() as u32).bytes(list.as_bytes());
        let res = self.inner.remoting.call(transport, SIG_PLAY_TRACKS, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlayTracks".into(), status: res.status }.into());
        }
        // Where this queue begins (its first item, once it's the mix's first
        // song), for the queue panel's played part
        let first_song = self.inner.graph.lock().unwrap().get(order[0]).and_then(|t| t.long_field("TrackId"));
        let mut queue_start = None;
        if let Some(queue) = queue {
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if let Some((item, song)) = self.queue_first_id(queue).await {
                    if first_song.is_none() || song == first_song {
                        queue_start = Some(item);
                        break;
                    }
                }
            }
        }
        println!(
            "[radio] mix: {} songs from {} of {} artists ({} live left out); queue items before and after clearing: {:?}; the new queue starts at item {:?}",
            order.len(),
            artists_with_songs,
            artists.len(),
            live_skipped,
            kept,
            queue_start
        );
        Ok(MixPlayed {
            songs: order.len(),
            artists: artists_with_songs,
            live_skipped,
            tracks: order.iter().map(|oid| oid.to_string()).collect(),
            queue_start,
        })
    }

    /// PlayParameters for a play: empty for now (Roon's default), else its
    /// Priority (PlayPriority: Now 0, Interrupt 1, PlayBefore 2, Next 3,
    /// Queue 4).
    fn play_parameters(&self, how: PlayHow) -> Result<Vec<u8>, LibraryError> {
        let priority = match how {
            PlayHow::Now => return Ok(self.inner.remoting.inline_struct("Sooloos.Broker.Api.PlayParameters", &[])?),
            PlayHow::Next => 3,
            PlayHow::Queue => 4,
        };
        let mut value = Writer::new();
        value.flex_int(priority);
        Ok(self.inner.remoting.inline_struct(
            "Sooloos.Broker.Api.PlayParameters",
            &[StructField { name: "Sooloos.Broker.Api.PlayPriority Sooloos.Broker.Api.PlayParameters::Priority", prop_type: PropertyType::Enum, value: value.into_bytes() }],
        )?)
    }

    /// Plays an album (by its handle) on the zone, right now, the way the Roon
    /// app's Play Now does.
    pub async fn play_album(&self, zone_id: &str, handle: &str) -> Result<(), LibraryError> {
        self.play_album_how(zone_id, handle, PlayHow::Now).await
    }

    /// Plays an album (by its handle) on the zone: now, next, or at the
    /// end of the queue, as the Roon app's Play Now, Add Next and Queue.
    pub async fn play_album_how(&self, zone_id: &str, handle: &str, how: PlayHow) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad album handle".into()))?;
        let (transport, zone, profile) = {
            let g = self.inner.graph.lock().unwrap();
            let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
            if g.get(oid).is_none() {
                return Err(LibraryError::Unconfirmed("unknown album handle".into()));
            }
            (g.first_of_type("Transport").map(|o| o.oid), zone.oid, self.profile_sooid(&g))
        };
        let (Some(transport), Some(profile)) = (transport, profile) else {
            return Err(LibraryError::UnsupportedCore("no Transport service or Profile in the graph".into()));
        };
        // Zone, profile, PlayParameters (its priority), the album, favorites only: no, include hidden: no.
        let params = self.play_parameters(how)?;
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params).long(oid).boolean(false).boolean(false);
        let res = self.inner.remoting.call(transport, SIG_PLAY_ALBUM, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlayAlbum".into(), status: res.status }.into());
        }
        Ok(())
    }

    /// BrowsePerformerAlbums, then one Load per group so every item points at
    /// its album object, in Roon's order. Roon hands out the library's copy of
    /// an album it holds, which only lists the library's tracks; the streaming
    /// services' own performer listings give the full release objects for the
    /// same albums (with LibraryAlbumId set), so those take over as `release`.
    async fn browse_performer_albums(&self, library: u64, profile: u64, performer: u64) -> Result<Vec<DiscoEntry>, LibraryError> {
        let spec = self.inner.remoting.inline_struct("Sooloos.Broker.Api.AlbumBrowserSpec", &[])?;
        let mut args = Writer::new();
        args.long(profile).long(performer).bytes(&spec);
        let res = self.inner.remoting.call(library, SIG_BROWSE_PERFORMER_ALBUMS, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "BrowsePerformerAlbums".into(), status: res.status }.into());
        }
        let browser = Reader::new(&res.payload)
            .flex_long()
            .map_err(|_| LibraryError::Unconfirmed("could not read the discography browser".into()))?;

        // The browser, its groups and their item lists arrive as pushes.
        let groups: Vec<(u64, String, u64)> = {
            let read = |g: &ObjectGraph| -> Option<Vec<(u64, String, u64)>> {
                let list = g.deref(g.get(browser)?.field("Groups"))?;
                list.items
                    .as_ref()?
                    .iter()
                    .map(|goid| {
                        let grp = g.get(*goid)?;
                        let items = g.deref(grp.field("Items"))?;
                        items.items.as_ref()?;
                        Some((*goid, grp.str_field("Type").unwrap_or_default().to_string(), items.oid))
                    })
                    .collect()
            };
            if !self.wait_until(Duration::from_secs(5), |g| read(g).is_some()).await {
                return Err(LibraryError::Unconfirmed("the discography did not arrive".into()));
            }
            read(&self.inner.graph.lock().unwrap()).unwrap_or_default()
        };

        let mut out = Vec::new();
        for (group_oid, group_type, items_oid) in groups {
            let count = self.inner.graph.lock().unwrap().get(items_oid).and_then(|l| l.items.as_ref().map(Vec::len)).unwrap_or(0);
            if count == 0 {
                continue;
            }
            let mut args = Writer::new();
            args.integer(0).integer(count as i32);
            let res = self.inner.remoting.call(group_oid, SIG_GROUP_LOAD, args.as_bytes()).await?;
            if !res.success() {
                return Err(RemotingError::Failed { method: "PerformerAlbumsGroup.Load".into(), status: res.status }.into());
            }
            // Loaded items gain an Album reference; wait until every item has one and the album object is here.
            let albums_of = |g: &ObjectGraph| -> Option<Vec<u64>> {
                g.get(items_oid)?
                    .items
                    .as_ref()?
                    .iter()
                    .map(|ioid| {
                        let album = g.get(*ioid)?.ref_field("Album")?;
                        g.get(album)?;
                        Some(album)
                    })
                    .collect()
            };
            self.wait_until(Duration::from_secs(5), |g| albums_of(g).is_some()).await;
            let g = self.inner.graph.lock().unwrap();
            // Take what did load even if some items never did.
            if let Some(list) = g.get(items_oid).and_then(|l| l.items.as_ref()) {
                for ioid in list {
                    if let Some(album) = g.get(*ioid).and_then(|i| i.ref_field("Album")) {
                        if g.get(album).is_some() {
                            out.push(DiscoEntry { release: album, library_copy: None, group: group_name(&group_type) });
                        }
                    }
                }
            }
        }

        // The streaming services' full release objects, by the library album they match.
        let releases = self.streaming_releases(performer).await;
        let g = self.inner.graph.lock().unwrap();
        for entry in &mut out {
            let Some(album) = g.get(entry.release) else { continue };
            if album.field("Source").and_then(Value::as_int) != Some(1) {
                continue; // already a streaming object (or a local-only album)
            }
            if let Some(release) = album.long_field("AlbumId").and_then(|id| releases.get(&id)) {
                entry.library_copy = Some(entry.release);
                entry.release = *release;
            }
        }
        Ok(out)
    }

    /// LibraryAlbumId -> the streaming service's album object, from each
    /// service's main-albums listing for the performer (Qobuz, TIDAL).
    async fn streaming_releases(&self, performer: u64) -> HashMap<u64, u64> {
        let (performer_id, services) = {
            let g = self.inner.graph.lock().unwrap();
            let id = g.get(performer).and_then(|p| p.long_field("PerformerId"));
            let services: Vec<(u64, &'static str)> = [("Qobuz", SIG_QOBUZ_MAIN_ALBUMS), ("Tidal", SIG_TIDAL_MAIN_ALBUMS)]
                .into_iter()
                .filter_map(|(name, sig)| Some((g.first_of_type(name)?.oid, sig)))
                .collect();
            (id, services)
        };
        let mut map = HashMap::new();
        let Some(performer_id) = performer_id else { return map };
        for (service, sig) in services {
            let mut args = Writer::new();
            args.long(performer_id);
            let Ok(res) = self.inner.remoting.call(service, sig, args.as_bytes()).await else { continue };
            if !res.success() {
                continue; // not logged in to that service, or nothing there
            }
            let oids: Vec<u64> = {
                let g = self.inner.graph.lock().unwrap();
                g.decode_return_list(&res.payload)
                    .map(|refs| refs.iter().filter_map(Value::as_ref_id).collect())
                    .unwrap_or_default()
            };
            self.wait_until(Duration::from_secs(3), |g| oids.iter().all(|oid| g.get(*oid).is_some())).await;
            let g = self.inner.graph.lock().unwrap();
            for oid in oids {
                if let Some(library_id) = g.get(oid).and_then(|a| a.long_field("LibraryAlbumId")) {
                    map.entry(library_id).or_insert(oid);
                }
            }
        }
        map
    }

    /// The discography entry whose release object is `oid`.
    fn disco_entry(&self, oid: u64) -> Option<DiscoEntry> {
        self.inner.discographies.lock().unwrap().values().flatten().find(|e| e.release == oid).cloned()
    }

    /// One discography album's current state by its handle.
    pub fn artist_album(&self, handle: &str) -> Option<ArtistAlbum> {
        let oid: u64 = handle.parse().ok()?;
        let g = self.inner.graph.lock().unwrap();
        let a = g.get(oid)?;
        let group = self.disco_entry(oid).map(|e| e.group).unwrap_or_default();
        Some(describe_album(&g, a, &group, self.profile_sooid(&g).as_deref(), false))
    }

    /// The complete release for one discography album (by its handle), with
    /// track handles like `album_tracks`. `zone_id` marks the playing track.
    pub async fn album_tracks_for(&self, zone_id: &str, handle: &str) -> Result<Vec<AlbumTrack>, LibraryError> {
        let album: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad album handle".into()))?;
        let (library, playing) = {
            let g = self.inner.graph.lock().unwrap();
            if g.get(album).is_none() {
                return Err(LibraryError::Unconfirmed("unknown album handle".into()));
            }
            (g.first_of_type("Library").map(|o| o.oid), self.track_object(&g, zone_id).map(|t| t.oid))
        };
        let library = library.ok_or_else(|| LibraryError::UnsupportedCore("no Library service in the graph".into()))?;
        let library_copy = self.disco_entry(album).and_then(|e| e.library_copy);
        self.tracks_of_album(library, album, playing, library_copy).await
    }

    /// Hearts (or un-hearts) a discography album by its handle. The album must
    /// be in the library.
    pub async fn set_album_favorite_for(&self, handle: &str, favorite: bool) -> Result<ArtistAlbum, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad album handle".into()))?;
        let current = self.artist_album(handle).ok_or_else(|| LibraryError::Unconfirmed("unknown album handle".into()))?;
        if current.favorite == favorite {
            return Ok(current);
        }
        self.favorite_album_object(oid, favorite).await?;
        let _ = self
            .wait_for(|g| g.get(oid).is_some_and(|a| read_state(a.field("IsFavorite"), self.profile_sooid(g).as_deref()) == favorite))
            .await;
        self.artist_album(handle).ok_or_else(|| LibraryError::Unconfirmed(format!("\"{}\" vanished", current.title)))
    }

    /// Adds a discography album to the library by its handle.
    pub async fn add_album_for(&self, handle: &str) -> Result<ArtistAlbum, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad album handle".into()))?;
        let current = self.artist_album(handle).ok_or_else(|| LibraryError::Unconfirmed("unknown album handle".into()))?;
        if current.in_library {
            return Ok(current);
        }
        let (library, profile) = {
            let g = self.inner.graph.lock().unwrap();
            (g.first_of_type("Library").map(|o| o.oid), self.profile_object(&g).map(|o| o.oid))
        };
        let (Some(library), Some(profile)) = (library, profile) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        let mut args = Writer::new();
        args.long(profile).long(oid);
        self.inner.remoting.call_no_reply(library, SIG_ADD_ALBUM, args.as_bytes())?;
        if !self.wait_for(|g| g.get(oid).is_some_and(album_in_library)).await {
            return Err(LibraryError::Unconfirmed(format!("adding \"{}\" to the library", current.title)));
        }
        self.artist_album(handle).ok_or_else(|| LibraryError::Unconfirmed(format!("\"{}\" vanished", current.title)))
    }

    /// GetAlbum: the full Album object's id (None when the Core has none).
    async fn get_album(&self, library: u64, album: u64) -> Result<Option<u64>, LibraryError> {
        let mut args = Writer::new();
        args.long(album);
        let res = self.inner.remoting.call(library, SIG_GET_ALBUM, args.as_bytes()).await?;
        if !res.success() || res.payload.is_empty() {
            return Ok(None);
        }
        Ok(Reader::new(&res.payload).flex_long().ok().filter(|oid| *oid != 0))
    }

    /// The track object ids of a full Album once they have all arrived (up to 3 s).
    async fn wait_for_album_tracks(&self, full_oid: u64) -> Vec<u64> {
        let tracks_of = |g: &ObjectGraph| -> Option<Vec<u64>> {
            let list = g.deref(g.get(full_oid)?.field("Tracks"))?;
            let items = list.items.as_ref()?;
            items.iter().all(|oid| g.get(*oid).is_some()).then(|| items.clone())
        };
        self.wait_until(Duration::from_secs(3), |g| tracks_of(g).is_some()).await;
        tracks_of(&self.inner.graph.lock().unwrap()).unwrap_or_default()
    }

    /// Waits up to `limit` until `predicate(graph)` holds (polling the pushes).
    async fn wait_until(&self, limit: Duration, predicate: impl Fn(&ObjectGraph) -> bool) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if predicate(&self.inner.graph.lock().unwrap()) {
                return true;
            }
            if Instant::now() > deadline || !self.is_connected() {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // --- the playing album's tracks ---

    /// The tracks of the album the zone is playing: the complete release as
    /// the Core knows it (also the tracks that aren't in the library, for a
    /// library album that only holds some of them). Each comes with a session
    /// handle for `set_track_favorite`, `add_track` and `play_track`.
    pub async fn album_tracks(&self, zone_id: &str) -> Result<Vec<AlbumTrack>, LibraryError> {
        let (library, album_oid, playing_oid) = {
            let g = self.inner.graph.lock().unwrap();
            let track = self.require_track(&g, zone_id)?;
            (
                g.first_of_type("Library").map(|o| o.oid),
                track.ref_field("Album"),
                track.oid,
            )
        };
        let (Some(library), Some(album)) = (library, album_oid) else {
            return Err(LibraryError::NothingPlaying(zone_id.into()));
        };
        self.tracks_of_album(library, album, Some(playing_oid), None).await
    }

    /// GetAlbum for the full release (falling back to the library's own track
    /// list), then each track's state; `playing` is the zone's track object.
    ///
    /// A library album only lists the tracks the library holds; its full
    /// Album names the streaming versions of the same release, and GetAlbum
    /// on the largest of those lists every track (with LibraryTrackId on the
    /// ones the library has). That is the list used when it is longer.
    ///
    /// `library_copy` is the library's own album object when `album` is a
    /// streaming service's release of it; its tracks are matched in by title.
    async fn tracks_of_album(&self, library: u64, album: u64, playing: Option<u64>, library_copy: Option<u64>) -> Result<Vec<AlbumTrack>, LibraryError> {
        let mut args = Writer::new();
        args.long(album);

        // The full Album object lists the release in its Tracks list.
        let mut track_oids: Vec<u64> = Vec::new();
        // The library's own tracks, to recognise them in a streaming version's list.
        let mut library_tracks: Vec<u64> = Vec::new();
        if let Some(full_oid) = self.get_album(library, album).await? {
            track_oids = self.wait_for_album_tracks(full_oid).await;
            // A streaming version with more tracks knows the whole release
            // (the versions are looked up after the album itself arrives).
            self.wait_until(Duration::from_secs(3), |g| {
                g.get(full_oid).is_some_and(|a| a.field("AlternateVersionsReady").and_then(Value::as_bool) == Some(true))
            })
            .await;
            let streaming = {
                let g = self.inner.graph.lock().unwrap();
                let largest = largest_streaming_version(&g, full_oid);
                largest.filter(|(_, count)| *count as usize > track_oids.len()).map(|(oid, _)| oid)
            };
            if let Some(version) = streaming {
                if let Some(version_full) = self.get_album(library, version).await? {
                    let more = self.wait_for_album_tracks(version_full).await;
                    if more.len() > track_oids.len() {
                        library_tracks = track_oids;
                        track_oids = more;
                    }
                }
            }
        }
        // The library's own tracks of the release, to recognise them by title.
        if let Some(copy) = library_copy {
            if let Some(copy_full) = self.get_album(library, copy).await? {
                let held = self.wait_for_album_tracks(copy_full).await;
                if !held.is_empty() {
                    library_tracks = held;
                }
            }
        }
        // Fallback: the library's own list for the album.
        if track_oids.is_empty() {
            let res = self.inner.remoting.call(library, SIG_ALBUM_TRACKS, args.as_bytes()).await?;
            if !res.success() {
                return Err(RemotingError::Failed { method: "GetAlbumTrackLites".into(), status: res.status }.into());
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            let g = self.inner.graph.lock().unwrap();
            let refs = g
                .decode_return_list(&res.payload)
                .map_err(|_| LibraryError::Unconfirmed("could not read the album's track list".into()))?;
            track_oids = refs.iter().filter_map(Value::as_ref_id).collect();
        }

        // Streaming tracks carry their heart on the library's own track object;
        // have those fetched (GetTrackLite) before reading the states.
        let unresolved: Vec<u64> = {
            let g = self.inner.graph.lock().unwrap();
            track_oids
                .iter()
                .filter_map(|oid| g.get(*oid))
                .filter(|t| self.library_track_object(&g, t).is_none())
                .filter_map(|t| t.long_field("LibraryTrackId"))
                .collect()
        };
        if !unresolved.is_empty() {
            self.wait_until(Duration::from_secs(2), |g| {
                let known = self.inner.library_tracks.lock().unwrap();
                unresolved.iter().all(|id| known.get(id).is_some_and(|oid| g.get(*oid).is_some()))
            })
            .await;
        }

        let g = self.inner.graph.lock().unwrap();
        let profile = self.profile_sooid(&g);
        let playing_ids: Vec<u64> = playing
            .and_then(|oid| g.get(oid))
            .map(|t| [t.long_field("TrackId"), t.long_field("LibraryTrackId")].into_iter().flatten().collect())
            .unwrap_or_default();
        // A different edition of the release doesn't link the library's tracks;
        // match those by title so they show as in the library (and heartable).
        let by_title: HashMap<String, u64> = library_tracks
            .iter()
            .filter_map(|oid| Some((title_key(g.get(*oid)?.str_field("Title")?), *oid)))
            .collect();
        let by_library_id: HashMap<u64, u64> = library_tracks
            .iter()
            .filter_map(|oid| Some((g.get(*oid)?.long_field("LibraryTrackId")?, *oid)))
            .collect();
        let mut out = Vec::new();
        for oid in track_oids {
            let Some(mut t) = g.get(oid) else { continue };
            let held = match t.long_field("LibraryTrackId") {
                Some(id) => by_library_id.get(&id),
                None => t.str_field("Title").and_then(|title| by_title.get(&title_key(title))),
            };
            if let Some(held) = held.and_then(|o| g.get(*o)) {
                t = held;
            }
            let oid = t.oid;
            let ids = [t.long_field("TrackId"), t.long_field("LibraryTrackId")];
            let state_obj = self.library_track_object(&g, t).unwrap_or(t);
            out.push(AlbumTrack {
                handle: oid.to_string(),
                title: t.str_field("Title").unwrap_or_default().to_string(),
                track_number: t.field("TrackNumber").and_then(Value::as_int),
                media_number: t.field("MediaNumber").and_then(Value::as_int),
                length_seconds: t.field("LengthSeconds").and_then(Value::as_int),
                in_library: t.has_field("LibraryTrackId"),
                favorite: read_state(state_obj.field("IsFavorite"), profile.as_deref()),
                playing: ids.iter().flatten().any(|id| playing_ids.contains(id)),
                track_id: ids[0],
                library_track_id: ids[1],
            });
        }
        Ok(out)
    }

    /// Plays one of the album's tracks (by its handle from `album_tracks`) on
    /// the zone, right now, the way the Roon app does.
    pub async fn play_track(&self, zone_id: &str, handle: &str) -> Result<(), LibraryError> {
        self.play_track_how(zone_id, handle, PlayHow::Now).await
    }

    /// Plays a track (by its handle) on the zone: now, next, or at the end
    /// of the queue.
    pub async fn play_track_how(&self, zone_id: &str, handle: &str, how: PlayHow) -> Result<(), LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad track handle".into()))?;
        let (transport, zone, profile) = {
            let g = self.inner.graph.lock().unwrap();
            let zone = self.zone_object(&g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.into()))?;
            if g.get(oid).is_none() {
                return Err(LibraryError::Unconfirmed("unknown track handle".into()));
            }
            (g.first_of_type("Transport").map(|o| o.oid), zone.oid, self.profile_sooid(&g))
        };
        let (Some(transport), Some(profile)) = (transport, profile) else {
            return Err(LibraryError::UnsupportedCore("no Transport service or Profile in the graph".into()));
        };
        // Zone, profile, PlayParameters (its priority), the track.
        let params = self.play_parameters(how)?;
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params).long(oid);
        let res = self.inner.remoting.call(transport, SIG_PLAY_TRACK, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlayTrack".into(), status: res.status }.into());
        }
        Ok(())
    }

    /// Hearts (or un-hearts) one of the album's tracks by its handle from
    /// `album_tracks`. The track must be in the library.
    pub async fn set_track_favorite(&self, handle: &str, favorite: bool) -> Result<AlbumTrack, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad track handle".into()))?;
        let (profile, library, target, title, current) = {
            let g = self.inner.graph.lock().unwrap();
            let t = g.get(oid).ok_or_else(|| LibraryError::Unconfirmed("unknown track handle".into()))?;
            if !t.has_field("LibraryTrackId") {
                return Err(LibraryError::NotInLibrary(t.str_field("Title").unwrap_or_default().to_string()));
            }
            let state_obj = self.library_track_object(&g, t).unwrap_or(t);
            (
                self.profile_sooid(&g),
                g.first_of_type("Library").map(|o| o.oid),
                state_obj.oid,
                t.str_field("Title").unwrap_or_default().to_string(),
                read_state(state_obj.field("IsFavorite"), self.profile_sooid(&g).as_deref()),
            )
        };
        let (Some(profile), Some(library)) = (profile, library) else {
            return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
        };
        if current != favorite {
            let mut args = Writer::new();
            args.sooid(&profile).long(target).flex_int(if favorite { FAVORITE } else { NOT_FAVORITE });
            let res = self.inner.remoting.call(library, SIG_FAVORITE_TRACK, args.as_bytes()).await?;
            if !res.success() {
                return Err(RemotingError::Failed { method: "FavoriteOrBan".into(), status: res.status }.into());
            }
            let _ = self.wait_for(|g| {
                g.get(target).is_some_and(|o| read_state(o.field("IsFavorite"), self.profile_sooid(g).as_deref()) == favorite)
            }).await;
        }
        self.album_track(oid).ok_or_else(|| LibraryError::Unconfirmed(format!("\"{title}\" vanished")))
    }

    /// Adds one of the album's tracks to the library by its handle from `album_tracks`.
    pub async fn add_track(&self, handle: &str) -> Result<AlbumTrack, LibraryError> {
        let oid: u64 = handle.parse().map_err(|_| LibraryError::Unconfirmed("bad track handle".into()))?;
        let (library, profile, title, in_library) = {
            let g = self.inner.graph.lock().unwrap();
            let t = g.get(oid).ok_or_else(|| LibraryError::Unconfirmed("unknown track handle".into()))?;
            (
                g.first_of_type("Library").map(|o| o.oid),
                self.profile_object(&g).map(|o| o.oid),
                t.str_field("Title").unwrap_or_default().to_string(),
                t.has_field("LibraryTrackId"),
            )
        };
        if !in_library {
            let (Some(library), Some(profile)) = (library, profile) else {
                return Err(LibraryError::UnsupportedCore("no Library service or Profile in the graph".into()));
            };
            let mut args = Writer::new();
            args.long(profile).long(oid);
            self.inner.remoting.call_no_reply(library, SIG_ADD_TRACK, args.as_bytes())?;
            if !self.wait_for(|g| g.get(oid).is_some_and(|o| o.has_field("LibraryTrackId"))).await {
                return Err(LibraryError::Unconfirmed(format!("adding \"{title}\" to the library")));
            }
        }
        self.album_track(oid).ok_or_else(|| LibraryError::Unconfirmed(format!("\"{title}\" vanished")))
    }

    /// One album track's current state by object id.
    fn album_track(&self, oid: u64) -> Option<AlbumTrack> {
        let g = self.inner.graph.lock().unwrap();
        let t = g.get(oid)?;
        let profile = self.profile_sooid(&g);
        let state_obj = self.library_track_object(&g, t).unwrap_or(t);
        Some(AlbumTrack {
            handle: oid.to_string(),
            title: t.str_field("Title").unwrap_or_default().to_string(),
            track_number: t.field("TrackNumber").and_then(Value::as_int),
            media_number: t.field("MediaNumber").and_then(Value::as_int),
            length_seconds: t.field("LengthSeconds").and_then(Value::as_int),
            in_library: t.has_field("LibraryTrackId"),
            favorite: read_state(state_obj.field("IsFavorite"), profile.as_deref()),
            playing: false,
            track_id: t.long_field("TrackId"),
            library_track_id: t.long_field("LibraryTrackId"),
        })
    }

    /// Waits until `predicate(graph)` holds; false on timeout / disconnect.
    async fn wait_for(&self, predicate: impl Fn(&ObjectGraph) -> bool) -> bool {
        let deadline = Instant::now() + self.inner.opts.confirm_timeout;
        loop {
            if !self.is_connected() {
                return false;
            }
            if predicate(&self.inner.graph.lock().unwrap()) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let _ = tokio::time::timeout(left.min(Duration::from_millis(500)), self.inner.changed.notified()).await;
        }
    }

    // --- internals ---

    fn require_track<'g>(&self, g: &'g ObjectGraph, zone_id: &str) -> Result<&'g RoonObject, LibraryError> {
        self.track_object(g, zone_id).ok_or_else(|| LibraryError::NothingPlaying(zone_id.to_string()))
    }

    fn schedule_recompute(&self) {
        if !self.is_connected() || self.inner.recompute_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let client = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RECOMPUTE_DELAY).await;
            client.inner.recompute_pending.store(false, Ordering::SeqCst);
            client.recompute();
        });
    }

    fn recompute(&self) {
        if !self.is_connected() {
            return;
        }
        let zone_ids = self.zone_ids();
        let mut reports = Vec::new();
        {
            let mut last = self.inner.last_reported.lock().unwrap();
            for zone_id in &zone_ids {
                let info = self.track_for_zone(zone_id);
                if last.get(zone_id) != Some(&info) {
                    last.insert(zone_id.clone(), info.clone());
                    reports.push((zone_id.clone(), info));
                }
            }
            let gone: Vec<String> = last.keys().filter(|z| !zone_ids.contains(z)).cloned().collect();
            for zone_id in gone {
                last.remove(&zone_id);
                reports.push((zone_id, None));
            }
        }
        for (zone_id, track) in reports {
            let _ = self.inner.events.send(LibraryEvent::Track { zone_id, track });
        }
    }

    /// Resolves with the zone's track once `predicate` holds, or None on timeout / disconnect.
    async fn wait_for_track(&self, zone_id: &str, predicate: impl Fn(&TrackInfo) -> bool) -> Option<TrackInfo> {
        let deadline = Instant::now() + self.inner.opts.confirm_timeout;
        loop {
            if !self.is_connected() {
                return None;
            }
            if let Some(info) = self.track_for_zone(zone_id) {
                if predicate(&info) {
                    return Some(info);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let _ = tokio::time::timeout(left.min(Duration::from_millis(500)), self.inner.changed.notified()).await;
        }
    }
}

/// From a full Album's AlternateAlbumVersions (an inline struct whose
/// StreamingVersions list holds AlternateAlbumVersion structs by value): the
/// streaming version's album object with the most tracks, and that count.
fn largest_streaming_version(g: &ObjectGraph, full_album: u64) -> Option<(u64, i32)> {
    let album = g.get(full_album)?;
    let Value::Inline(versions) = album.field("AlternateAlbumVersions")? else { return None };
    let list = g.deref(versions.field("StreamingVersions"))?;
    list.structs
        .as_ref()?
        .iter()
        .filter_map(|v| Some((v.field("Album")?.as_ref_id()?, v.field("TrackCount").and_then(Value::as_int).unwrap_or(0))))
        .max_by_key(|(_, count)| *count)
}

/// Titles compared loosely: case, punctuation and spacing ignored.
fn title_key(title: &str) -> String {
    title.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Whether an album object is in the library (library albums carry a
/// LibraryAlbumId; the library's own objects also report Source 1).
fn album_in_library(album: &RoonObject) -> bool {
    album.has_field("LibraryAlbumId") || album.field("Source").and_then(Value::as_int) == Some(1)
}

/// Roon's group names for a discography, shortened.
fn group_name(group_type: &str) -> String {
    match group_type {
        "MainAlbums" => "main".into(),
        "OtherAlbums" => "other".into(),
        "AppearanceAlbums" => "appearance".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// "[[20602|Amon Amarth]] & [[123|Someone]]" -> "Amon Amarth & Someone".
fn strip_credit_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                let inner = &after[..end];
                out.push_str(inner.rsplit('|').next().unwrap_or(inner));
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn describe_album(g: &ObjectGraph, a: &RoonObject, group: &str, profile: Option<&[u8]>, playing: bool) -> ArtistAlbum {
    ArtistAlbum {
        handle: a.oid.to_string(),
        title: a.str_field("Title").unwrap_or_default().to_string(),
        artist: strip_credit_markup(a.str_field("PerformedBy").unwrap_or_default()),
        group: group.to_string(),
        in_library: album_in_library(a),
        favorite: read_state(a.field("IsFavorite"), profile),
        playing,
        album_id: a.long_field("AlbumId"),
        year: year_of(a.field("OriginalReleaseDate")).or_else(|| year_of(a.field("ReleaseDate"))),
        released: date_of(a.field("OriginalReleaseDate")).or_else(|| date_of(a.field("ReleaseDate"))),
        popularity: number_of(a.field("Popularity")),
        image_key: image_key(g, a, "Image"),
    }
}

/// The rows of history queries (VirtualQueryElement<HistoryPlay>), each
/// pointing at its play by Data.
fn history_rows(g: &ObjectGraph) -> impl Iterator<Item = &RoonObject> {
    g.objects.values().filter(|r| r.type_name.contains("VirtualQueryElement<") && r.type_name.contains("HistoryPlay"))
}

/// What tells one HistoryPlay from another (its id, or failing that its
/// time and track), so a play pushed twice counts once.
fn play_identity(p: &RoonObject) -> String {
    match p.field("HistoryPlayId") {
        Some(Value::Long(id)) => format!("id:{id}"),
        Some(Value::Int(id)) => format!("id:{id}"),
        Some(Value::Str(id)) => format!("id:{id}"),
        Some(Value::Bytes(id)) => format!("id:{id:?}"),
        _ => format!("oid:{}", p.oid),
    }
}

/// A HistoryPlay as a `Play`: its time (.NET ticks, the kind bits masked
/// off) and what its track and album say. None without a time or a track.
fn describe_play(g: &ObjectGraph, p: &RoonObject) -> Option<Play> {
    const TICKS_MASK: u64 = (1 << 62) - 1;
    const UNIX_EPOCH_TICKS: u64 = 621_355_968_000_000_000;
    let ticks = p.long_field("Time")? & TICKS_MASK;
    let played_at = (ticks as i64 - UNIX_EPOCH_TICKS as i64) / 10_000;
    let track = g.deref(p.field("Track"))?;
    let album = g.deref(track.field("Album"));
    let performer = album.and_then(|a| g.deref(a.field("MainPerformers"))).and_then(|list| list.items.as_ref()?.first().copied());
    let track_id = match p.field("TrackBase") {
        Some(Value::Inline(base)) => base.field("TrackId").and_then(Value::as_long),
        _ => None,
    }
    .or_else(|| track.long_field("TrackId"));
    Some(Play {
        played_at,
        title: track.str_field("Title").unwrap_or_default().to_string(),
        artist: strip_credit_markup(album.and_then(|a| a.str_field("PerformedBy")).unwrap_or_default()),
        album: album.and_then(|a| a.str_field("Title")).unwrap_or_default().to_string(),
        album_handle: album.map(|a| a.oid.to_string()),
        album_id: album.and_then(|a| a.long_field("AlbumId")),
        track_handle: Some(track.oid.to_string()),
        track_id,
        performer_handle: performer.map(|oid| oid.to_string()),
        performer_id: performer.and_then(|oid| g.get(oid)).and_then(|p| p.long_field("PerformerId")),
        image_key: album.and_then(|a| image_key(g, a, "Image")),
        seconds_played: p.field("SecondsPlayed").and_then(Value::as_int),
        length_seconds: track.field("LengthSeconds").and_then(Value::as_int),
    })
}

/// The Text of a Review or Biography member: an inline struct, or a
/// reference to a pushed one. None when empty.
fn text_of(g: &ObjectGraph, value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::Inline(s) => s.field("Text").and_then(Value::as_str).map(str::to_string),
        Value::Ref(oid) => g.get(*oid).and_then(|o| o.str_field("Text")).map(str::to_string),
        Value::Str(s) => Some(s.clone()),
        _ => None,
    }?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The strings of a `DataList<string>` member (Genres): the list came by
/// reference (a pushed object with `strings`) or by value (an inline list).
fn strings_of(g: &ObjectGraph, list: Option<&Value>) -> Vec<String> {
    match list {
        Some(Value::Ref(oid)) => g.get(*oid).and_then(|l| l.strings.clone()).unwrap_or_default(),
        Some(Value::Inline(l)) => l.items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

/// The object ids a DataList holds: its items when they came by reference,
/// or, for items that came by value (PerformerWithExtras), the object their
/// `via` member points at. None while the list itself hasn't arrived.
fn list_members(g: &ObjectGraph, list: Option<&Value>, via: Option<&str>) -> Option<Vec<u64>> {
    let list = match list? {
        Value::Null => return Some(Vec::new()),
        Value::Ref(oid) => g.get(*oid)?,
        _ => return Some(Vec::new()),
    };
    // The list's contents haven't been pushed yet
    if list.items.is_none() && list.structs.is_none() {
        return None;
    }
    // (A list of inline items has an empty `items` beside its `structs`)
    if let Some(items) = list.items.as_ref().filter(|items| !items.is_empty()) {
        return Some(items.clone());
    }
    Some(
        list.structs
            .iter()
            .flatten()
            .filter_map(|s| via.and_then(|name| s.field(name)).and_then(Value::as_ref_id))
            .collect(),
    )
}

fn describe_album_card(g: &ObjectGraph, a: &RoonObject, profile: Option<&[u8]>) -> AlbumCard {
    let performer = g.deref(a.field("MainPerformers")).and_then(|list| list.items.as_ref()?.first().copied());
    AlbumCard {
        handle: a.oid.to_string(),
        album_id: a.long_field("AlbumId"),
        title: a.str_field("Title").unwrap_or_default().to_string(),
        artist: strip_credit_markup(a.str_field("PerformedBy").unwrap_or_default()),
        performer_handle: performer.map(|oid| oid.to_string()),
        performer_id: performer.and_then(|oid| g.get(oid)).and_then(|p| p.long_field("PerformerId")),
        year: year_of(a.field("OriginalReleaseDate")).or_else(|| year_of(a.field("ReleaseDate"))),
        image_key: image_key(g, a, "Image"),
        in_library: album_in_library(a),
        favorite: read_state(a.field("IsFavorite"), profile),
        source: source_name(a.field("Source").and_then(Value::as_int)),
    }
}

fn describe_library_track(g: &ObjectGraph, t: &RoonObject, profile: Option<&[u8]>) -> LibraryTrack {
    let album = g.deref(t.field("Album"));
    let performer = album.and_then(|a| g.deref(a.field("MainPerformers"))).and_then(|list| list.items.as_ref()?.first().copied());
    let artist = t
        .str_field("PerformedBy")
        .filter(|s| !s.is_empty())
        .or_else(|| album.and_then(|a| a.str_field("PerformedBy")))
        .unwrap_or_default();
    LibraryTrack {
        title: t.str_field("Title").unwrap_or_default().to_string(),
        artist: strip_credit_markup(artist),
        album: album.and_then(|a| a.str_field("Title")).unwrap_or_default().to_string(),
        track_id: t.long_field("LibraryTrackId").or_else(|| t.long_field("TrackId")),
        album_id: album.and_then(|a| a.long_field("AlbumId")),
        performer_id: performer.and_then(|oid| g.get(oid)).and_then(|p| p.long_field("PerformerId")),
        image_key: album.and_then(|a| image_key(g, a, "Image")),
        length_seconds: t.field("LengthSeconds").and_then(Value::as_int),
        year: album.and_then(|a| year_of(a.field("OriginalReleaseDate")).or_else(|| year_of(a.field("ReleaseDate")))),
        favorite: read_state(t.field("IsFavorite"), profile),
        source: source_name(t.field("Source").and_then(Value::as_int)),
        added_at: ["ImportDate", "DateAdded", "AddedDate", "ImportTime"]
            .iter()
            .find_map(|name| t.field(name).or_else(|| album.and_then(|a| a.field(name))).and_then(ticks_to_ms)),
    }
}

/// A .NET DateTime (ticks since year 1, the top two bits its kind) as
/// Unix milliseconds.
fn ticks_to_ms(v: &Value) -> Option<i64> {
    const TICKS_MASK: u64 = (1 << 62) - 1;
    const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    let t = v.as_long()?;
    let ms = ((t & TICKS_MASK) as i64 - UNIX_EPOCH_TICKS) / 10_000;
    (ms > 0).then_some(ms)
}

fn describe_playlist(g: &ObjectGraph, p: &RoonObject) -> PlaylistCard {
    let kind = match p.field("Type").and_then(Value::as_int) {
        Some(1) => "itunes",
        Some(5) => "roon",
        Some(6) => "roon-readonly",
        Some(10) => "tidal",
        Some(11) => "file",
        Some(12) => "qobuz",
        Some(13) => "editorial",
        Some(15) => "smart",
        _ => "other",
    };
    // The first of the playlist's cover images
    let image_key = g
        .deref(p.field("Images"))
        .and_then(|list| list.items.as_ref()?.first().copied())
        .and_then(|oid| g.get(oid))
        .and_then(|image| image.str_field("Url"))
        .and_then(|url| url.strip_prefix("broker:///image/"))
        .and_then(|rest| rest.split('.').next())
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    PlaylistCard {
        handle: p.oid.to_string(),
        playlist_id: p.field("PlaylistId").and_then(Value::as_bytes).map(|b| b.iter().map(|x| format!("{x:02x}")).collect()),
        name: p.str_field("Name").unwrap_or_default().to_string(),
        kind: kind.to_string(),
        tracks: p.field("TrackCount").and_then(Value::as_int).or_else(|| p.field("ItemCount").and_then(Value::as_int)),
        length_seconds: p.field("LengthSeconds").and_then(Value::as_int),
        image_key,
        read_only: p.field("IsReadOnly").and_then(Value::as_bool) == Some(true),
    }
}

fn describe_artist_card(g: &ObjectGraph, p: &RoonObject, profile: Option<&[u8]>) -> ArtistCard {
    ArtistCard {
        handle: p.oid.to_string(),
        performer_id: p.long_field("PerformerId"),
        name: p.str_field("Name").unwrap_or_default().to_string(),
        image_key: image_key(g, p, "Photo").or_else(|| image_key(g, p, "Avatar")),
        favorite: read_state(p.field("IsFavorite"), profile),
        album_count: p.field("MainPerformerAlbumCount").and_then(Value::as_int),
    }
}

/// The lists of a UnifiedSearchResults object: artists, albums and tracks,
/// Roon's top hits first. Albums and tracks may come wrapped in
/// AlbumLiteVersions / TrackLiteVersions (the versions of one release).
fn search_lists(g: &ObjectGraph, r: &RoonObject) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
    let unwrap = |oid: u64, wrapper: &str, member: &str| -> Vec<u64> {
        match g.get(oid) {
            Some(o) if o.short_type().ends_with(wrapper) => {
                g.deref(o.field(member)).and_then(|l| l.items.clone()).unwrap_or_default()
            }
            _ => vec![oid],
        }
    };
    let list = |name: &str| -> Vec<u64> { g.deref(r.field(name)).and_then(|l| l.items.clone()).unwrap_or_default() };
    let mut artists: Vec<u64> = ["TopLibraryPerformer", "TopPerformer"].iter().filter_map(|n| r.ref_field(n)).collect();
    artists.extend(list("Performers"));
    // (the top album or track is a versions wrapper, without a title, when
    // the library has several copies of it: its versions instead)
    let mut albums: Vec<u64> = ["TopLibraryAlbum", "TopAlbum"].iter().filter_map(|n| r.ref_field(n)).flat_map(|oid| unwrap(oid, "AlbumLiteVersions", "Albums")).collect();
    albums.extend(list("Albums").into_iter().flat_map(|oid| unwrap(oid, "AlbumLiteVersions", "Albums")));
    let mut tracks: Vec<u64> = ["TopLibraryTrack", "TopTrack"].iter().filter_map(|n| r.ref_field(n)).flat_map(|oid| unwrap(oid, "TrackLiteVersions", "Tracks")).collect();
    tracks.extend(list("Tracks").into_iter().flat_map(|oid| unwrap(oid, "TrackLiteVersions", "Tracks")));
    let dedupe = |v: Vec<u64>| -> Vec<u64> {
        let mut seen = HashSet::new();
        v.into_iter().filter(|oid| seen.insert(*oid)).collect()
    };
    (dedupe(artists), dedupe(albums), dedupe(tracks))
}

/// The image key (for the Core's image service) of an object's `Image`,
/// `Photo` or `Avatar`: the Image object's Url is `broker:///image/<key>.jpg`.
fn image_key(g: &ObjectGraph, o: &RoonObject, name: &str) -> Option<String> {
    // The Image comes by reference or inline, depending on the object
    let url = match o.field(name)? {
        Value::Ref(oid) => g.get(*oid)?.str_field("Url")?,
        Value::Inline(image) => image.field("Url")?.as_str()?,
        _ => return None,
    };
    let key = url.strip_prefix("broker:///image/")?.split('.').next()?;
    (!key.is_empty()).then(|| key.to_string())
}

/// Roon's dates are one number, (year << 16) | (month << 8) | day, sent as a
/// small blob (a flex int) or a plain number.
fn year_of(value: Option<&Value>) -> Option<u32> {
    let n = match value? {
        Value::Bytes(b) => Reader::new(b).flex_int().ok()?,
        Value::Int(i) => *i as u32,
        Value::Long(l) => *l as u32,
        _ => return None,
    };
    let year = n >> 16;
    (1000..3000).contains(&year).then_some(year)
}

/// A release date (year << 16 | month << 8 | day) as "YYYY-MM-DD".
fn date_of(value: Option<&Value>) -> Option<String> {
    let n = match value? {
        Value::Bytes(b) => Reader::new(b).flex_int().ok()?,
        Value::Int(i) => *i as u32,
        Value::Long(l) => *l as u32,
        _ => return None,
    };
    let (year, month, day) = (n >> 16, (n >> 8) & 0xff, n & 0xff);
    ((1000..3000).contains(&year) && month <= 12 && day <= 31).then(|| format!("{year:04}-{month:02}-{day:02}"))
}

/// Any number the Core sends (popularity comes as a double or an int).
fn number_of(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Double(d) => Some(*d),
        Value::Float(f) => Some(*f as f64),
        Value::Int(i) => Some(*i as f64),
        Value::Long(l) => Some(*l as f64),
        _ => None,
    }
}

/// A live recording by its title: "(Live)", "[Live at ...]", "- Live",
/// "Live at/in/from ...", "Unplugged", "In Concert".
/// A mix's order from each artist's songs (see `pick_artist_songs`): the
/// first artist's first song, then round by round (each artist's next
/// song) in a shuffled artist order, never the same artist twice in a row
/// where it can be helped.
pub fn order_mix(picks: &[Vec<String>], per_artist: usize) -> Vec<String> {
    // The order: the first artist's best song, then rounds
    let mut rng = Shuffle::new();
    let mut order: Vec<String> = Vec::new();
    let mut last_artist: Option<usize> = None;
    if let Some(first) = picks.first().and_then(|p| p.first()) {
        order.push(first.clone());
        last_artist = Some(0);
    }
    for round in 0..per_artist {
        let mut who: Vec<usize> = (0..picks.len()).filter(|a| !(round == 0 && *a == 0)).filter(|a| picks[*a].len() > round).collect();
        rng.shuffle(&mut who);
        // (not the artist that just played)
        if who.len() > 1 && who.first().copied() == last_artist {
            let n = who.len();
            who.swap(0, n - 1);
        }
        for a in who {
            order.push(picks[a][round].clone());
            last_artist = Some(a);
        }
    }
    order
}

/// A title's words: lowercased, punctuation gone, one space between and a
/// space either side, so " word " finds whole words ("Introspection" is not
/// an intro, "Pre-Production" is " pre production ").
fn words_of(title: &str) -> String {
    let mut out = String::from(" ");
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// A demo, rough mix, remix, instrumental, alternate take or other version
/// of a song, or an intro, outro, interlude, skit or medley: not the song
/// to introduce an artist with. Works on a song's title or an album's
/// ("Demos 1991"). Along the lines of MusicBrainz's release types (Demo,
/// Remix, Interview, Spokenword...) and Spotify's speechiness filter.
fn looks_rough(title: &str) -> bool {
    let w = words_of(title);
    [
        " demo ",
        " demos ",
        " rough mix ",
        " rough mixes ",
        " remix ",
        " remixes ",
        " remixed ",
        " instrumental ",
        " instrumentals ",
        " rehearsal ",
        " rehearsals ",
        " pre production ",
        " radio edit ",
        " single edit ",
        " single version ",
        " single mix ",
        " edit ",
        " karaoke ",
        " acoustic ",
        " alternate ",
        " alternative version ",
        " alt version ",
        " alt mix ",
        " alt take ",
        " outtake ",
        " outtakes ",
        " early version ",
        " early mix ",
        " session ",
        " sessions ",
        " cover ",
        " commentary ",
        " bonus ",
        " mono ",
        " intro ",
        " outro ",
        " interlude ",
        " skit ",
        " medley ",
        " intermission ",
        " overture ",
        " prelude ",
        " prologue ",
        " interview ",
        " spoken word ",
        " spokenword ",
    ]
    .iter()
    .any(|m| w.contains(m))
        // ("Take 2", "Take 7")
        || w.split(" take ").skip(1).any(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

/// A compilation, anthology, split, tribute, soundtrack or other album
/// that isn't the artist's own record, by its title (MusicBrainz's
/// Compilation, Soundtrack, Mixtape and DJ-mix types, give or take).
fn looks_compilation(album: &str) -> bool {
    let w = words_of(album);
    [
        " compilation ",
        " anthology ",
        " best of ",
        " the best ",
        " greatest ",
        " collection ",
        " collected ",
        " tribute ",
        " split ",
        " box set ",
        " boxset ",
        " soundtrack ",
        " ost ",
        " rarities ",
        " b sides ",
        " singles ",
        " essential ",
        " retrospective ",
        " discography ",
        " complete ",
        " mixtape ",
        " dj mix ",
        " various artists ",
        " sampler ",
    ]
    .iter()
    .any(|m| w.contains(m))
}

fn looks_live(title: &str) -> bool {
    let t = title.to_lowercase();
    ["(live", "[live", "- live", "live at ", "live in ", "live from ", "unplugged", "in concert", "live session", "live version"]
        .iter()
        .any(|mark| t.contains(mark))
        || t.ends_with(" live")
        || t == "live"
}

/// A song's title for telling the same song apart on other albums ("Song
/// (Remastered 2019)" is "song").
fn normalized_title(title: &str) -> String {
    let t = title.to_lowercase();
    let cut = t.find(" (").or_else(|| t.find(" [")).or_else(|| t.find(" - ")).unwrap_or(t.len());
    t[..cut].chars().filter(|c| c.is_alphanumeric()).collect()
}

/// A small shuffler (xorshift, seeded by the clock): the mix's order needn't
/// be any more random than that.
struct Shuffle(u64);

impl Shuffle {
    fn new() -> Self {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        Shuffle(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

fn source_name(source: Option<i32>) -> String {
    match source {
        Some(1) => "local",
        Some(256) => "qobuz",
        Some(128) => "tidal",
        Some(512) => "streaming",
        _ => "other",
    }
    .to_string()
}

/// The zone id string (the extension API's zone_id) from a Zone object.
/// Zone -> SignalPath -> Elements (DataList<SignalPathElement>) -> the element
/// with a Source (SourceSignalPathElement): format, bit depth, sample rate.
fn source_quality(g: &ObjectGraph, zone: &RoonObject) -> Option<AudioQuality> {
    let path = g.deref(zone.field("SignalPath"))?;
    let elements = g.deref(path.field("Elements"))?;
    let (element, source) = elements.items.as_ref()?.iter().find_map(|oid| {
        let element = g.get(*oid)?;
        Some((element, g.deref(element.field("Source"))?))
    })?;
    let format = source.str_field("Format").map(str::to_string);
    let positive = |name: &str| source.field(name).and_then(Value::as_int).filter(|v| *v > 0).map(|v| v as u32);
    let bits_per_sample = positive("BitsPerSample");
    let sample_rate = positive("SampleRate");
    let bit_rate = positive("BitRate");
    let channels = positive("Channels");
    // Roon's own verdict for this step: 1 LowQuality, 2 HighQuality (both lossy), 3 Lossless, 4 Enhanced
    let roon = element.field("Quality").and_then(Value::as_int);
    let name = format.as_deref().unwrap_or_default().to_ascii_lowercase();
    let lossy = matches!(roon, Some(1 | 2)) || ["mp3", "aac", "ogg", "vorbis", "opus", "wma"].iter().any(|f| name.starts_with(f));
    let dsd = name == "dsf" || name == "dff" || name.contains("dsd");
    let class = if lossy {
        "low"
    } else if dsd || bits_per_sample.is_some_and(|b| b > 16) || sample_rate.is_some_and(|r| r > 48_000) {
        "hires"
    } else if bits_per_sample.is_some() || sample_rate.is_some() {
        "cd"
    } else {
        return None;
    };
    Some(AudioQuality { class, format, bits_per_sample, sample_rate, bit_rate, channels })
}

struct WaveformOwner {
    track: String,
    hash: u64,
    since: Instant,
}

/// Zone -> WaveformData (DataList<int>): Roon's waveform of the playing
/// track, the same one its own seek bar shows.
fn zone_waveform(g: &ObjectGraph, zone: &RoonObject) -> Option<Vec<u8>> {
    let values = g.deref(zone.field("WaveformData"))?.ints.as_ref()?;
    if values.len() < 16 || values.iter().all(|v| *v == 0) {
        return None;
    }
    // Scaled to 0..255 by the track's own top when Roon's numbers go past
    // 255 (capping them flattened every loud part to one height)
    let top = values.iter().copied().max().unwrap_or(0);
    if top > 255 {
        Some(values.iter().map(|v| ((*v as u64 * 255) / top as u64) as u8).collect())
    } else {
        Some(values.iter().map(|v| (*v).min(255) as u8).collect())
    }
}

fn zone_id_of(zone: &RoonObject) -> Option<String> {
    zone.field("ZoneId")
        .and_then(Value::as_bytes)
        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
}

/// IsFavorite / IsBanned are per-profile maps: flexInt(count), then for each
/// entry a Sooid (the profile) and a flexInt state (0 = none, 1 = set).
/// A bare 00 means nobody has set it.
pub fn read_state(raw: Option<&Value>, profile: Option<&[u8]>) -> bool {
    let Some(bytes) = raw.and_then(Value::as_bytes) else { return false };
    if bytes.is_empty() {
        return false;
    }
    let mut r = Reader::new(bytes);
    let Ok(count) = r.flex_int() else { return false };
    let mut anyone = false;
    for _ in 0..count {
        let (Ok(who), Ok(state)) = (r.sooid(), r.flex_int()) else { return false };
        if state == 1 {
            match profile {
                Some(p) if who == p => return true,
                Some(_) => {}
                None => anyone = true,
            }
        }
    }
    profile.is_none() && anyone
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexbytes(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn favorite_state_is_per_profile() {
        let profile = hexbytes("3f019657409adb84814f872dab89ff0b7f2f");
        let set = Value::Bytes(hexbytes("01123f019657409adb84814f872dab89ff0b7f2f01"));
        assert!(read_state(Some(&set), Some(&profile)));
        assert!(!read_state(Some(&set), Some(&[0xaa])));
        assert!(read_state(Some(&set), None));
        assert!(!read_state(Some(&Value::Bytes(vec![0])), Some(&profile)));
        assert!(!read_state(None, Some(&profile)));
    }

    #[test]
    fn favorite_args_match_the_typescript_package() {
        let profile = hexbytes("3f019657409adb84814f872dab89ff0b7f2f");
        let mut args = Writer::new();
        args.sooid(&profile).long(2375439).flex_int(FAVORITE);
        let hex: String = args.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "123f019657409adb84814f872dab89ff0b7f2f8190fe0f01");
    }

    #[test]
    fn rough_titles_are_whole_words() {
        for t in ["Intro", "Malus Vivendi (Intro)", "Song - Radio Edit", "Pre-Production Demo", "Hymn (Take 3)", "Skit", "Peel Session", "Demos 1991", "Song (Acoustic)", "Hero [Alternate Mix]"] {
            assert!(looks_rough(t), "{t}");
        }
        for t in ["Introspection", "Control", "Demonic", "Undertaker", "Hero", "Monolith", "Covered in Blood", "Editorial"] {
            assert!(!looks_rough(t), "{t}");
        }
    }

    #[test]
    fn compilation_albums_by_title() {
        for a in ["The Best of Pitchshifter", "Anthology 1986-1996", "Split", "Rarities & B-Sides", "Original Soundtrack", "Greatest Hits"] {
            assert!(looks_compilation(a), "{a}");
        }
        for a in ["Infect", "Dark Matter", "Collective Unconscious", "Splitting the Atom"] {
            assert!(!looks_compilation(a), "{a}");
        }
    }
}

#[cfg(test)]
mod discography_tests {
    use super::*;

    #[test]
    fn credit_markup_is_stripped() {
        assert_eq!(strip_credit_markup("[[20602|Amon Amarth]]"), "Amon Amarth");
        assert_eq!(strip_credit_markup("[[1|A]] & [[2|B]]"), "A & B");
        assert_eq!(strip_credit_markup("plain"), "plain");
        assert_eq!(strip_credit_markup("[[broken"), "broken");
    }

    #[test]
    fn groups_are_shortened() {
        assert_eq!(group_name("MainAlbums"), "main");
        assert_eq!(group_name("OtherAlbums"), "other");
        assert_eq!(group_name("AppearanceAlbums"), "appearance");
        assert_eq!(group_name("LiveAlbums"), "livealbums");
    }
}

#[cfg(test)]
mod mix_tests {
    use super::*;

    #[test]
    fn live_titles() {
        for live in ["Diaken (Live)", "Live at Roadburn 2019", "Wear My Crown - Live", "MTV Unplugged", "Live", "Sarin [Live in Berlin]"] {
            assert!(looks_live(live), "{live}");
        }
        for studio in ["Deliverance", "Oliver", "Alive", "Lives", "Delivery", "Mass VI"] {
            assert!(!looks_live(studio), "{studio}");
        }
    }

    #[test]
    fn same_song_titles() {
        assert_eq!(normalized_title("Diaken (Remastered 2019)"), normalized_title("Diaken"));
        assert_eq!(normalized_title("Sarin - Single Version"), "sarin");
        assert_ne!(normalized_title("Sarin"), normalized_title("Saturn"));
    }

    #[test]
    fn shuffle_keeps_items() {
        let mut items: Vec<u32> = (0..50).collect();
        Shuffle::new().shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
    }
}
