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
        let playing_ids: Vec<u64> = g
            .get(playing_album)
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
                let playing = entry.release == playing_album
                    || entry.library_copy == Some(playing_album)
                    || ids.iter().any(|id| playing_ids.contains(id));
                Some(describe_album(a, &entry.group, profile_sooid.as_deref(), playing))
            })
            .collect())
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
        Some(describe_album(a, &group, self.profile_sooid(&g).as_deref(), false))
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

fn describe_album(a: &RoonObject, group: &str, profile: Option<&[u8]>, playing: bool) -> ArtistAlbum {
    ArtistAlbum {
        handle: a.oid.to_string(),
        title: a.str_field("Title").unwrap_or_default().to_string(),
        artist: strip_credit_markup(a.str_field("PerformedBy").unwrap_or_default()),
        group: group.to_string(),
        in_library: album_in_library(a),
        favorite: read_state(a.field("IsFavorite"), profile),
        playing,
    }
}

/// The zone id string (the extension API's zone_id) from a Zone object.
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
