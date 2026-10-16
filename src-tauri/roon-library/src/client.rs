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
use crate::graph::{ObjectGraph, PropertyType, RoonObject, Value};
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
const SIG_PLAY_PLAYLIST: &str = "Sooloos.Broker.Api.Transport::PlayPlaylist(Sooloos.Broker.Api.Zone, System.Sooid, Sooloos.Broker.Api.PlayParameters, Sooloos.Broker.Api.Playlist, Sooloos.Broker.Api.PlaylistItem, Base.ResultCallback<Sooloos.Broker.Api.PlayFeedback>)";
const SIG_GET_ALBUM_LITE: &str = "Sooloos.Broker.Api.Library::GetAlbumLite(long, Base.ResultCallback<Sooloos.Broker.Api.AlbumLite>)";
const SIG_GET_PERFORMER_LITE: &str = "Sooloos.Broker.Api.Library::GetPerformerLite(long, Base.ResultCallback<Sooloos.Broker.Api.PerformerLite>)";
const SIG_UNIFIED_SEARCH: &str = "Sooloos.Broker.Api.Library::UnifiedSearch(Sooloos.Broker.Api.SearchParameters, Base.ResultCallback<Sooloos.Broker.Api.UnifiedSearchResults>)";
const SIG_GET_PERFORMER: &str = "Sooloos.Broker.Api.Library::GetPerformer(Sooloos.Broker.Api.PerformerBase, Base.ResultCallback<Sooloos.Broker.Api.Performer>)";
const SIG_SIMILAR_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxSimilarPerformers(Sooloos.Broker.Api.PerformerLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerWithExtras>>)";
const SIG_PERFORMER_RECOMMENDED: &str = "Sooloos.Broker.Api.Library::GetOneBoxPerformerRecommendedAlbums(Sooloos.Broker.Api.PerformerLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.PerformerRecommendedAlbums>)";
const SIG_SIMILAR_ALBUMS: &str = "Sooloos.Broker.Api.Library::GetOneBoxSimilarAlbums(Sooloos.Broker.Api.AlbumLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.OneBox_SimilarAlbums>)";
const SIG_ALBUM_RECOMMENDED: &str = "Sooloos.Broker.Api.Library::GetOneBoxAlbumRecommendedForYou(Sooloos.Broker.Api.AlbumLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.AlbumLite>>)";
const SIG_ALBUM_SIMILAR_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxAlbumSimilarPerformers(Sooloos.Broker.Api.AlbumLite, System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerWithExtras>>)";
const SIG_RECOMMENDED_PERFORMERS: &str = "Sooloos.Broker.Api.Library::GetOneBoxRecommendedPerformers(System.Sooid, Base.ResultCallback<Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.PerformerWithExtras>>)";
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

/// What to start Roon Radio from (session handles from the pages), and
/// whether to stay inside the library (see `play_radio`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RadioSeeds {
    pub tracks: Vec<String>,
    pub albums: Vec<String>,
    pub artists: Vec<String>,
    /// Genre names, as Roon spells them.
    pub genres: Vec<String>,
    pub library_only: bool,
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

        // The plays already in the graph are an earlier query's: only the
        // ones this query pushes count
        let before: HashSet<u64> = self.inner.graph.lock().unwrap().find_by_type("HistoryPlay").map(|o| o.oid).collect();
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
        let new_plays = |g: &ObjectGraph| g.find_by_type("HistoryPlay").filter(|o| !before.contains(&o.oid)).count();
        let pages = target.div_ceil(page_size);
        for page in 0..pages {
            let mut arg = Writer::new();
            arg.integer(page as i32);
            let res = self.inner.remoting.call(query, SIG_HISTORY_RETAIN, arg.as_bytes()).await?;
            if !res.success() {
                return Err(RemotingError::Failed { method: format!("RetainPage({page})"), status: res.status }.into());
            }
            let want = total.min((page + 1) * page_size);
            // The page's plays, then their tracks and albums (names), up to 5 s
            self.wait_until(Duration::from_secs(5), |g| new_plays(g) >= want).await;
            self.wait_until(Duration::from_secs(3), |g| {
                g.find_by_type("HistoryPlay")
                    .filter(|o| !before.contains(&o.oid))
                    .all(|p| p.has_field("Time") && g.deref(p.field("Track")).is_some_and(|t| t.has_field("Title")))
            })
            .await;
            let _ = self.inner.remoting.call_no_reply(query, SIG_HISTORY_RELEASE, arg.as_bytes());
            progress(want, target);
            // Reached what the caller already has: no need for older pages
            if let Some(stop_at) = stop_at {
                let g = self.inner.graph.lock().unwrap();
                let reached = g
                    .find_by_type("HistoryPlay")
                    .filter(|o| !before.contains(&o.oid))
                    .filter_map(|p| describe_play(&g, p))
                    .any(|p| p.played_at <= stop_at);
                if reached {
                    break;
                }
            }
        }
        let g = self.inner.graph.lock().unwrap();
        let mut seen = HashSet::new();
        let mut plays: Vec<Play> = g
            .find_by_type("HistoryPlay")
            .filter(|o| !before.contains(&o.oid))
            .filter(|p| seen.insert(play_identity(p)))
            .filter_map(|p| describe_play(&g, p))
            .collect();
        plays.sort_by(|a, b| b.played_at.cmp(&a.played_at));
        plays.truncate(target);
        Ok(History { total: total as u32, plays })
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
        let similar = self.one_box(SIG_SIMILAR_ALBUMS, "GetOneBoxSimilarAlbums", Subject::Oid(oid)).await?;
        let recommended = self.one_box(SIG_ALBUM_RECOMMENDED, "GetOneBoxAlbumRecommendedForYou", Subject::Oid(oid)).await?;
        let artists = self.one_box(SIG_ALBUM_SIMILAR_PERFORMERS, "GetOneBoxAlbumSimilarPerformers", Subject::Oid(oid)).await?;
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
        Ok(AlbumExtras {
            similar: self.album_cards(&g, Some(&similar_list), Some(oid)),
            similarity,
            recommended: self.album_cards(&g, Some(&recommended), Some(oid)),
            similar_artists: self.artist_cards(&g, Some(&artists), None),
        })
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
        args.long(profile).string(Some(name)).string(Some("")).boolean(false).flex_int(ids.len() as u32).bytes(ids.as_bytes());
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

    /// Starts Roon Radio on the zone from the seeds (Roon's own "Start
    /// Radio", which the app offers on one song, album or artist; this takes
    /// any mix of them, and genres), optionally staying inside the library.
    pub async fn play_radio(&self, zone_id: &str, seeds: &RadioSeeds) -> Result<(), LibraryError> {
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
                value: yes.into_bytes(),
            });
        }
        let params = self.inner.remoting.inline_struct("Sooloos.Broker.Api.SwimParameters", &fields)?;
        let mut args = Writer::new();
        args.long(zone).sooid(&profile).bytes(&params);
        let res = self.inner.remoting.call(transport, SIG_PLAY_SWIM, args.as_bytes()).await?;
        if !res.success() {
            return Err(RemotingError::Failed { method: "PlaySwim".into(), status: res.status }.into());
        }
        Ok(())
    }

    /// Plays an album (by its handle) on the zone, right now, the way the Roon
    /// app's Play Now does.
    pub async fn play_album(&self, zone_id: &str, handle: &str) -> Result<(), LibraryError> {
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
        // Zone, profile, default PlayParameters, the album, favorites only: no, include hidden: no.
        let params = self.inner.remoting.inline_struct("Sooloos.Broker.Api.PlayParameters", &[])?;
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
                largest_streaming_version(&g, full_oid)
                    .filter(|(_, count)| *count as usize > track_oids.len())
                    .map(|(oid, _)| oid)
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
        // Zone, profile, default PlayParameters (an empty inline struct), the track.
        let params = self.inner.remoting.inline_struct("Sooloos.Broker.Api.PlayParameters", &[])?;
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
        image_key: image_key(g, a, "Image"),
    }
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
    let mut albums: Vec<u64> = ["TopLibraryAlbum", "TopAlbum"].iter().filter_map(|n| r.ref_field(n)).collect();
    albums.extend(list("Albums").into_iter().flat_map(|oid| unwrap(oid, "AlbumLiteVersions", "Albums")));
    let mut tracks: Vec<u64> = ["TopLibraryTrack", "TopTrack"].iter().filter_map(|n| r.ref_field(n)).collect();
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
    Some(values.iter().map(|v| (*v).min(255) as u8).collect())
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
