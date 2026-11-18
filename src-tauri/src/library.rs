// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hearts and add-to-library (experimental).
//!
//! Roon's extension API can't heart a track or add it to the library. The
//! `roon-library` crate (in `roon-library/`) does both the way Roon's own
//! desktop app does, over Roon's internal protocol on port 9332. Roon changes
//! that protocol whenever they like, so this is switched off by default
//! (setting `libraryControls`), labeled Experimental, and built to fail
//! quietly: when the Core turns us down the buttons simply stay hidden and
//! Settings says why.
//!
//! - Command `library_status` returns `{ enabled, state, reason }`; event
//!   `library-status` fires with the same shape whenever it changes. `state`
//!   is "off", "connecting", "ready", "unsupported" (this Roon version) or
//!   "unavailable" (no Core, or the connection dropped; it retries).
//! - Command `library_track({ zoneId })` returns the zone's track with its
//!   heart / library state, or null; event `library-track` fires with
//!   `{ zoneId, track }` whenever that changes (including hearts set in Roon).
//! - Commands `library_heart({ zoneId, favorite })` and `library_add({ zoneId, mode? })`
//!   change things and return the confirmed track. Hearting a track that isn't
//!   in the library adds it first, as a single track or its whole album
//!   (setting `libraryAddMode`, unless `mode` says otherwise).
//! - For the playing album's page: `library_album_heart({ zoneId, favorite })`,
//!   `library_album_tracks({ zoneId })` (every track with its state and a
//!   session `handle`; the whole release, including tracks the library doesn't
//!   hold), `library_track_heart({ handle, favorite })`, `library_track_add({ handle })`
//!   and `library_play_track({ zoneId, handle })`.
//! - For any album by the playing artist: `library_artist_albums({ zoneId })`
//!   (the discography with album handles), then `library_album_tracks_for({ zoneId, handle })`,
//!   `library_album_heart_for({ handle, favorite })` and `library_album_add_for({ handle })`.
//! - For the Wrapper's Wrapped pages (any album or artist, not just the
//!   playing one): `library_album_by_id({ albumId })` and `library_artist_by_id({ performerId })`
//!   (the Core's durable ids, as Roon's log names its pages) give a card with
//!   a handle; `library_album_card({ handle })` / `library_artist_card({ handle })`
//!   read a card again; `library_artist_albums_for({ handle, zoneId? })` is the
//!   discography of any artist; `library_search({ query })` is Roon's own
//!   search (artists, albums, tracks with handles); `library_play_album({ zoneId, handle })`
//!   plays an album now. The album track commands above take any album handle.
//!
//! The connection follows the paired Core: it opens once the Core is
//! connected and its address is known, closes when the Core is lost or the
//! setting is switched off, and reconnects with a backoff otherwise. Once a
//! day it reconnects on purpose, because the Core keeps every object it has
//! sent a session until that session ends (a few per track played).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use roon_library::{AddMode, AlbumCard, AlbumExtras, AlbumTrack, ArtistAlbum, ArtistCard, ArtistExtras, ClientOptions, Facts, GenreInfo, GenrePage, History, LibraryClient, LibraryError, LibraryEvent, MixOptions, MixPlayed, PlaylistCard, QueueSong, RadioSeeds, SearchHits, TrackInfo};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::settings::{self, AppSettings};

/// Planned reconnect, so the Core can drop the objects it kept for us.
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The same reconnect, early: the link keeps every object the Core sends
/// (and the Core keeps them for us), and a read of the whole library or
/// history sends a great many: about 200,000 objects and 450 MB for a
/// library of 14,000 tracks, kept until the daily reconnect (a library
/// three times that had the app at 2.3 GB). So once the link holds this
/// many and none has come for a while (the read that brought them is
/// over), it reconnects and starts again with the thousand or so it needs.
///
/// The numbers: a fresh link is at 16,000 within a minute (the artists'
/// pictures are read on connect) and an evening of playing and browsing
/// adds a few thousand, so 20,000 had it reconnecting about hourly for
/// nothing; 60,000 (about 140 MB) is out of everyday reach and well under
/// what a library read brings. And two minutes of quiet, with no paged
/// read running or just ended: a rescan is several reads with other work
/// between them, and 45 s had it reconnecting between two of them.
const LINK_FULL: usize = 60_000;
const LINK_QUIET: Duration = Duration::from_secs(120);
const LINK_TRICKLE: usize = 2_000;

/// Work that carries handles from one call to the next (a Deep Dive, a
/// Discover list, Home: songs are found, weighed for minutes, then saved
/// as a playlist by their handles) holds the link: no planned reconnect
/// while a hold is out, nor for `LINK_QUIET` after the last one ends (the
/// page then plays what was made, by its handle). A reconnect in the
/// middle left a Deep Dive with handles of the old link, and Roon
/// answered "unknown track handle" when it came to saving the playlist.
static HOLDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HELD_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

pub struct LinkHold;

pub fn hold_link() -> LinkHold {
    HOLDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    LinkHold
}

impl Drop for LinkHold {
    fn drop(&mut self) {
        *HELD_UNTIL.lock().unwrap() = Some(Instant::now());
        HOLDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn link_held() -> bool {
    HOLDS.load(std::sync::atomic::Ordering::SeqCst) > 0 || HELD_UNTIL.lock().unwrap().is_some_and(|at| at.elapsed() < LINK_QUIET)
}

/// Retry spacing after a failed connection (not for "unsupported").
const RETRY_DELAYS: [u64; 4] = [5, 15, 60, 300];

#[derive(Clone, Copy, Serialize, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum LibraryState {
    /// The setting is off.
    Off,
    Connecting,
    Ready,
    /// The Core rejected us: this Roon version isn't supported (no retries
    /// until the Core reconnects or the setting is toggled).
    Unsupported,
    /// No Core, or the connection dropped; retrying.
    Unavailable,
}

#[derive(Clone, Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStatus {
    pub enabled: bool,
    pub state: LibraryState,
    /// Plain-language detail for Settings when not ready.
    pub reason: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrackEvent {
    zone_id: String,
    track: Option<TrackInfo>,
}

struct Inner {
    client: Option<LibraryClient>,
    status: LibraryStatus,
    /// Bumped every time the connection is (re)started or stopped, so an old
    /// connection task that wakes up late knows to stand down.
    generation: u64,
    last_sent: Option<LibraryStatus>,
}

/// The library connection, stored in Tauri's managed state.
pub struct Library {
    inner: Mutex<Inner>,
    app: OnceLock<AppHandle>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                client: None,
                status: LibraryStatus {
                    enabled: false,
                    state: LibraryState::Off,
                    reason: None,
                },
                generation: 0,
                last_sent: None,
            }),
            app: OnceLock::new(),
        }
    }
}

impl Library {
    pub fn status(&self) -> LibraryStatus {
        self.inner.lock().unwrap().status.clone()
    }

    pub(crate) fn client(&self) -> Option<LibraryClient> {
        let inner = self.inner.lock().unwrap();
        inner.client.clone().filter(|c| c.is_connected())
    }

    /// Records a new status and tells the pages (and the terminal) if it changed.
    fn set_status(&self, state: LibraryState, reason: Option<String>) {
        let status = {
            let mut inner = self.inner.lock().unwrap();
            inner.status.state = state;
            inner.status.reason = reason;
            let status = inner.status.clone();
            if inner.last_sent.as_ref() == Some(&status) {
                return;
            }
            inner.last_sent = Some(status.clone());
            status
        };
        match &status.reason {
            Some(r) => println!("[library {}] {:?}: {r}", clock(), status.state),
            None => println!("[library {}] {:?}", clock(), status.state),
        }
        if let Some(app) = self.app.get() {
            let _ = app.emit("library-status", &status);
        }
    }

    /// Drops the current connection (if any) and invalidates its task.
    fn stop(&self) -> u64 {
        let (client, generation) = {
            let mut inner = self.inner.lock().unwrap();
            inner.generation += 1;
            (inner.client.take(), inner.generation)
        };
        if let Some(client) = client {
            client.close();
        }
        generation
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Connects the state to the app (in setup) and starts if the setting is on.
pub fn start(app: &AppHandle) {
    let library = app.state::<Library>();
    let _ = library.app.set(app.clone());
    let enabled = settings::get_settings(app.clone()).library_controls;
    library.inner.lock().unwrap().status.enabled = enabled;
    reconsider(app);
}

/// Called whenever the Roon connection status changes (roon.rs) or the
/// setting is toggled: (re)connects or stops as appropriate.
pub fn reconsider(app: &AppHandle) {
    let library = app.state::<Library>();
    let enabled = library.inner.lock().unwrap().status.enabled;
    if !enabled {
        library.stop();
        library.set_status(LibraryState::Off, None);
        return;
    }
    let roon = app.state::<crate::roon::Roon>().status();
    let core = match (roon.state, roon.connected) {
        (crate::roon::ConnectionState::Connected, Some(core)) if !core.host.is_empty() => core,
        _ => {
            library.stop();
            library.set_status(LibraryState::Unavailable, Some("Waiting for the Roon Core.".into()));
            return;
        }
    };
    // Already connected to this Core: nothing to do.
    {
        let inner = library.inner.lock().unwrap();
        if inner.client.as_ref().is_some_and(|c| c.is_connected())
            || inner.status.state == LibraryState::Connecting
            || inner.status.state == LibraryState::Unsupported
        {
            return;
        }
    }
    let generation = library.stop();
    library.set_status(LibraryState::Connecting, None);
    let app = app.clone();
    tauri::async_runtime::spawn(run(app, generation, core.host, core.core_id));
}

/// Called from settings.rs when either library setting changes.
pub fn settings_changed(app: &AppHandle, before: &AppSettings, after: &AppSettings) {
    if before.library_controls != after.library_controls {
        let library = app.state::<Library>();
        {
            let mut inner = library.inner.lock().unwrap();
            inner.status.enabled = after.library_controls;
        }
        // A toggle also clears an "unsupported" verdict, so it can be retried.
        if after.library_controls {
            library.set_status(LibraryState::Unavailable, None);
        }
        reconsider(app);
    }
}

/// One connection's life: connect, forward events, reconnect on drops, stand
/// down when a newer generation took over.
async fn run(app: AppHandle, generation: u64, host: String, core_id: String) {
    let library = app.state::<Library>();
    let mut attempt = 0usize;
    loop {
        if library.inner.lock().unwrap().generation != generation {
            return; // stopped or restarted meanwhile
        }
        let mut opts = ClientOptions::new(host.clone(), core_id.clone());
        opts.settle = Duration::from_secs(3);
        match LibraryClient::connect(opts).await {
            Ok(client) => {
                {
                    let mut inner = library.inner.lock().unwrap();
                    if inner.generation != generation {
                        client.close();
                        return;
                    }
                    inner.client = Some(client.clone());
                }
                println!("[library {}] connected to the Core at {host} ({} objects)", clock(), client.object_count());
                // The profile picked in Settings (the account's first if none
                // was, or it's gone), before anything reads a heart
                client.set_profile(crate::settings::get_settings(app.clone()).library_profile.as_deref());
                library.set_status(LibraryState::Ready, None);
                let _ = app.emit("library-profiles", profiles_now(&client));
                // (the profile in use is known for sure now: its colors, and
                // its own General settings if it has them)
                crate::settings::announce(&app);
                // (and its listening history and Last.fm name are the ones
                // Wrapped goes by)
                crate::wrapped::profile_in_use(&app, &client);
                let _ = app.emit("wrapped-settings-changed", crate::wrapped::WrappedSettings::load(&app));
                // (artists that came in since the last tagging get their
                // tags, a little after the start so nothing waits on it)
                let app2 = app.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    crate::tags::fill_missing(&app2);
                });
                attempt = 0;
                crate::wrapped::refresh_artists(app.clone(), client.clone());

                let reason = forward_events(&app, &client, generation).await;
                if library.inner.lock().unwrap().generation != generation {
                    return;
                }
                match reason {
                    Some(reason) => {
                        library.set_status(LibraryState::Unavailable, Some(format!("Lost the connection: {reason}")));
                    }
                    None => {
                        // Planned refresh: reconnect right away, quietly.
                        client.close();
                        library.inner.lock().unwrap().client = None;
                        continue;
                    }
                }
            }
            Err(LibraryError::UnsupportedCore(reason)) => {
                library.set_status(
                    LibraryState::Unsupported,
                    Some(format!("Not supported with this Roon version ({reason}).")),
                );
                return;
            }
            Err(e) => {
                if attempt == 0 {
                    eprintln!("[library {}] couldn't connect to the Core at {host}: {e} (will keep trying)", clock());
                }
                library.set_status(LibraryState::Unavailable, Some(format!("Couldn't connect: {e}")));
            }
        }
        let delay = RETRY_DELAYS[attempt.min(RETRY_DELAYS.len() - 1)];
        attempt += 1;
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

/// Forwards track events to the pages until the connection ends. Returns the
/// reason it ended, or None for the planned daily refresh.
async fn forward_events(app: &AppHandle, client: &LibraryClient, generation: u64) -> Option<String> {
    let mut events = client.events();
    let refresh = tokio::time::sleep(REFRESH_AFTER);
    tokio::pin!(refresh);
    let mut check = tokio::time::interval(Duration::from_secs(5));
    let mut profiles = profiles_now(client);
    // (how many objects the link held when it last grew, and when)
    let mut grown = (client.object_count(), Instant::now());
    loop {
        tokio::select! {
            _ = &mut refresh => {
                // (not under work that holds the link: a minute later, then)
                if link_held() || client.reading(LINK_QUIET) {
                    refresh.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(60));
                    continue;
                }
                println!("[library {}] daily reconnect", clock());
                return None;
            }
            // A deliberate close (stop()) sends no event; notice it here.
            _ = check.tick() => {
                if !client.is_connected() || app.state::<Library>().inner.lock().unwrap().generation != generation {
                    return Some("stopped".into());
                }
                // (still growing in earnest, that is: a song starting brings
                // a few dozen objects, and with music on there would never
                // be a quiet two minutes if those counted)
                let held = client.object_count();
                if held > grown.0 + LINK_TRICKLE {
                    grown = (held, Instant::now());
                } else if held >= LINK_FULL && grown.1.elapsed() >= LINK_QUIET && !client.reading(LINK_QUIET) && !link_held() {
                    println!("[library {}] reconnecting to let go of {held} objects (quiet for {} s)", clock(), LINK_QUIET.as_secs());
                    return None;
                }
                // (a profile added, renamed or removed in Roon: the pages hear)
                let now = profiles_now(client);
                if now != profiles {
                    // (the list can come in after the link is up; the
                    // profile in use is then known, and its history)
                    crate::wrapped::profile_in_use(app, client);
                    let _ = app.emit("wrapped-settings-changed", crate::wrapped::WrappedSettings::load(app));
                    let _ = app.emit("library-profiles", &now);
                    profiles = now;
                }
            }
            event = events.recv() => match event {
                Ok(LibraryEvent::Track { zone_id, track }) => {
                    if app.state::<Library>().inner.lock().unwrap().generation != generation {
                        return Some("stopped".into());
                    }
                    let _ = app.emit("library-track", &TrackEvent { zone_id, track });
                }
                Ok(LibraryEvent::Disconnected { reason }) => return Some(reason),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Some("connection closed".into()),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Commands the UI (and status.rs, for the Stream Deck plugin) can call
// ---------------------------------------------------------------------------

/// The Roon account's profiles and the one in use (`current`, its id).
/// Hearts, bans and listening history are kept per profile; the library
/// is shared. Empty while the library link is off or not connected.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profiles {
    pub profiles: Vec<roon_library::ProfileInfo>,
    pub current: Option<String>,
}

/// The id of the profile in use: the connected Core's word on it, or,
/// while there's no link to it, the one picked last (so that profile's
/// colors are there from the start).
pub fn profile_id(app: &AppHandle, settings: &crate::settings::AppSettings) -> Option<String> {
    match app.try_state::<Library>().and_then(|library| library.client()) {
        Some(client) => client.profile().map(|p| p.id),
        None => settings.library_profile.clone().filter(|_| settings.library_controls),
    }
}

fn profiles_now(client: &LibraryClient) -> Profiles {
    Profiles { profiles: client.profiles(), current: client.profile().map(|p| p.id) }
}

#[tauri::command]
pub fn library_profiles(library: State<'_, Library>) -> Profiles {
    library.client().map(|c| profiles_now(&c)).unwrap_or_default()
}

/// Picks the profile hearts, bans and history go to (saved, so it's the
/// same one next time), and tells every page.
/// (async, so it runs on the async runtime and not on the main thread)
#[tauri::command]
pub async fn library_set_profile(app: AppHandle, id: String) -> Result<Profiles, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    if !client.profiles().iter().any(|p| p.id == id) {
        return Err("That profile isn't in this Roon account any more.".into());
    }
    // (the profile first: the settings then go out with its colors)
    client.set_profile(Some(&id));
    crate::settings::update_settings(app.clone(), serde_json::json!({ "libraryProfile": id }), None)?;
    let now = profiles_now(&client);
    println!("[library {}] profile: {}", clock(), client.profile().map(|p| p.name).unwrap_or_default());
    // (Wrapped's General settings may be this profile's own)
    // (its listening history and Last.fm name are the ones Wrapped goes by
    // now)
    crate::wrapped::profile_in_use(&app, &client);
    let _ = app.emit("wrapped-settings-changed", crate::wrapped::WrappedSettings::load(&app));
    let _ = app.emit("library-profiles", &now);
    Ok(now)
}

/// Time of day (UTC, HH:MM:SS) for the log lines, to see how far apart events are.
fn clock() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}

fn not_ready(library: &Library) -> String {
    match library.status().state {
        LibraryState::Off => "Hearts and library controls are switched off in Settings.".into(),
        LibraryState::Unsupported => "Hearts and library controls aren't supported with this Roon version.".into(),
        _ => "Not connected to the Roon Core's library yet.".into(),
    }
}

fn describe(e: LibraryError) -> String {
    match e {
        LibraryError::NothingPlaying(_) => "Nothing is playing in that zone.".into(),
        LibraryError::NotInLibrary(t) => format!("\"{t}\" isn't in the library."),
        LibraryError::Unconfirmed(m) => format!("Roon didn't confirm the change ({m})."),
        LibraryError::UnsupportedCore(m) => format!("Not supported with this Roon version ({m})."),
        LibraryError::Closed => "Lost the connection to the Roon Core.".into(),
        other => other.to_string(),
    }
}

/// How long an action on the playing song waits for a link that's
/// reconnecting, and how long it lets the new link settle before trying.
const ACTION_WAIT: Duration = Duration::from_secs(12);
const ACTION_SETTLE: Duration = Duration::from_millis(1500);

/// Runs an action on a zone's playing song (a heart, a ban, add to
/// library), holding on to it over a reconnect: clicked while the link is
/// away (the planned reconnects take about three seconds) or cut off by
/// one, it's tried again a moment after the link is back, instead of
/// failing. Only for actions named by the zone: what a page names by a
/// handle means nothing on the next link.
async fn over_reconnect<T, F, Fut>(app: &AppHandle, action: F) -> Result<T, String>
where
    F: Fn(LibraryClient) -> Fut,
    Fut: std::future::Future<Output = Result<T, LibraryError>>,
{
    let library = app.state::<Library>();
    let started = Instant::now();
    let mut waited = false;
    loop {
        let mut failure = None;
        if let Some(client) = library.client().filter(|c| c.is_connected()) {
            if waited {
                tokio::time::sleep(ACTION_SETTLE).await;
            }
            match action(client).await {
                Err(e) if e.is_closed() => failure = Some(e),
                // (the new link may not have heard what the zone plays yet)
                Err(e @ LibraryError::NothingPlaying(_)) if waited => failure = Some(e),
                other => return other.map_err(describe),
            }
        }
        let state = library.status().state;
        if matches!(state, LibraryState::Off | LibraryState::Unsupported) || started.elapsed() >= ACTION_WAIT {
            return Err(failure.map(describe).unwrap_or_else(|| not_ready(&library)));
        }
        if !waited {
            println!("[library {}] an action waits for the link to come back", clock());
        }
        waited = true;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Whether the feature is on and connected, with a reason when it isn't.
#[tauri::command]
pub fn library_status(library: State<'_, Library>) -> LibraryStatus {
    library.status()
}

/// The zone's current track with its heart / library state (null when nothing is loaded).
#[tauri::command]
pub fn library_track(library: State<'_, Library>, zone_id: String) -> Result<Option<TrackInfo>, String> {
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    Ok(client.track_for_zone(&zone_id))
}

/// Hearts or un-hearts the zone's current track (adding it to the library
/// first if needed, per the `libraryAddMode` setting).
#[tauri::command]
pub async fn library_heart(app: AppHandle, zone_id: String, favorite: bool) -> Result<TrackInfo, String> {
    let add = add_mode(&app);
    let track = over_reconnect(&app, |client| {
        let zone_id = zone_id.clone();
        async move { client.set_favorite(&zone_id, favorite, Some(add)).await }
    })
    .await?;
    tell_copy_of(&app, track.library_track_id.as_ref()).await;
    Ok(track)
}

/// Bans the zone's current track for the profile in use, or lifts its ban
/// (the Ban button, when it's switched on in Settings).
#[tauri::command]
pub async fn library_ban(app: AppHandle, zone_id: String, banned: bool, song: Option<crate::wrapped::BannedSong>) -> Result<TrackInfo, String> {
    let result = over_reconnect(&app, |client| {
        let zone_id = zone_id.clone();
        async move { client.set_banned(&zone_id, banned).await }
    })
    .await;
    // (Roon has no list of bans outside the library: Wrapped keeps the
    // ones made here, for History's Banned)
    if let (Ok(now), Some(song)) = (&result, song) {
        crate::wrapped::ban_note(&app, song, now.banned);
    }
    result
}

/// Roon's genres and the user's Roon tags for the zone's current track
/// (Now Playing's Wrapped piece, when its Genres or Tags are switched on).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlayingLabels {
    pub genres: Vec<String>,
    pub tags: Vec<String>,
}

#[tauri::command]
pub async fn library_playing_labels(app: AppHandle, zone_id: String) -> Result<PlayingLabels, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let (genres, tags, _) = client.playing_labels(&zone_id).await.map_err(describe)?;
    Ok(PlayingLabels { genres, tags })
}

/// Bans an artist for the profile in use, or lifts the ban (the Ban button
/// on an artist's page): Roon's own artist ban, and Wrapped's ban list
/// with it, so neither Roon nor Discover, Home or Deep Dive picks them.
/// Returns whether the artist is banned now.
#[tauri::command]
pub async fn library_artist_ban(app: AppHandle, handle: String, name: String, banned: bool, image_key: Option<String>) -> Result<bool, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.set_artist_banned(&handle, banned).await.map_err(describe)?;
    crate::wrapped::ban_note(&app, crate::wrapped::BannedSong { kind: "artist".into(), title: name.clone(), image_key, ..Default::default() }, banned);
    if banned {
        crate::discover::wrapped_ban_add(app.clone(), name);
    } else {
        crate::discover::wrapped_ban_remove(app.clone(), name);
    }
    Ok(banned)
}

/// Bans an album for the profile in use, or lifts the ban (the Ban button
/// on an album's page): Roon's own album ban. Returns whether the album is
/// banned now.
#[tauri::command]
pub async fn library_album_ban(app: AppHandle, handle: String, banned: bool, album: crate::wrapped::BannedSong) -> Result<bool, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let (now, _) = client.set_album_banned(&handle, banned).await.map_err(describe)?;
    crate::wrapped::ban_note(&app, crate::wrapped::BannedSong { kind: "album".into(), ..album }, now);
    Ok(now)
}

/// Bans one album track for the profile in use, or lifts its ban, by its
/// handle (the ban mark on an album page's rows).
#[tauri::command]
pub async fn library_track_ban(app: AppHandle, handle: String, banned: bool, song: Option<crate::wrapped::BannedSong>) -> Result<AlbumTrack, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let now = client.set_track_banned(&handle, banned).await.map_err(describe)?;
    if let Some(song) = song {
        crate::wrapped::ban_note(&app, song, now.banned);
    }
    Ok(now)
}

/// Adds the zone's current track (or its album, per the setting unless `mode`
/// says "track" or "album") to the library.
#[tauri::command]
pub async fn library_add(app: AppHandle, zone_id: String, mode: Option<AddMode>) -> Result<TrackInfo, String> {
    let add = mode.unwrap_or_else(|| add_mode(&app));
    let track = over_reconnect(&app, |client| {
        let zone_id = zone_id.clone();
        async move { client.add_to_library(&zone_id, add).await }
    })
    .await?;
    tell_copy_of(&app, track.library_track_id.as_ref()).await;
    Ok(track)
}

/// Hearts or un-hearts the album of the zone's current track (it must be in the library).
#[tauri::command]
pub async fn library_album_heart(app: AppHandle, zone_id: String, favorite: bool) -> Result<TrackInfo, String> {
    over_reconnect(&app, |client| {
        let zone_id = zone_id.clone();
        async move { client.set_album_favorite(&zone_id, favorite).await }
    })
    .await
}

/// The tracks of the album the zone is playing, each with a session handle.
#[tauri::command]
pub async fn library_album_tracks(app: AppHandle, zone_id: String) -> Result<Vec<AlbumTrack>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.album_tracks(&zone_id).await.map_err(describe)
}

/// Hearts or un-hearts one album track by its handle (adds it to the library first if needed).
#[tauri::command]
pub async fn library_track_heart(app: AppHandle, handle: String, favorite: bool) -> Result<AlbumTrack, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    if favorite {
        client.add_track(&handle).await.map_err(describe)?;
    }
    let track = client.set_track_favorite(&handle, favorite).await.map_err(describe)?;
    tell_copy(&app, &client, &handle);
    Ok(track)
}

/// Wrapped's copy of the library, which the Tracks page and its hearts filter read, hears of a song
/// hearted, un-hearted or added, wherever that was done (an album page, a playlist, the player, the
/// Toaster): its hearts, and the song itself when it came into the library after the copy was read.
fn tell_copy(app: &AppHandle, client: &LibraryClient, handle: &str) {
    if let Some(track) = client.library_track(handle) {
        crate::wrapped::library_copy_put(app, track);
    }
}

/// The same for the zone's playing song, known by the library's id for it.
async fn tell_copy_of(app: &AppHandle, library_track_id: Option<&String>) {
    let Some(id) = library_track_id.and_then(|id| id.parse::<u64>().ok()) else { return };
    let Some(client) = app.state::<Library>().client() else { return };
    if let Ok(handle) = client.track_by_id(id).await {
        tell_copy(app, &client, &handle);
    }
}

/// Adds one album track to the library by its handle.
#[tauri::command]
pub async fn library_track_add(app: AppHandle, handle: String) -> Result<AlbumTrack, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let track = client.add_track(&handle).await.map_err(describe)?;
    tell_copy(&app, &client, &handle);
    Ok(track)
}

/// The playing artist's discography, each album with a session handle.
#[tauri::command]
pub async fn library_artist_albums(app: AppHandle, zone_id: String) -> Result<Vec<ArtistAlbum>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.artist_albums(&zone_id).await.map_err(describe)
}

/// The complete release of a discography album (by handle), with track handles.
#[tauri::command]
pub async fn library_album_tracks_for(app: AppHandle, zone_id: String, handle: String) -> Result<Vec<AlbumTrack>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.album_tracks_for(&zone_id, &handle).await.map_err(describe)
}

/// Hearts or un-hearts a discography album (adding it to the library first when hearting).
#[tauri::command]
pub async fn library_album_heart_for(app: AppHandle, handle: String, favorite: bool) -> Result<ArtistAlbum, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    if favorite {
        client.add_album_for(&handle).await.map_err(describe)?;
    }
    client.set_album_favorite_for(&handle, favorite).await.map_err(describe)
}

/// Adds a discography album to the library.
#[tauri::command]
pub async fn library_album_add_for(app: AppHandle, handle: String) -> Result<ArtistAlbum, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.add_album_for(&handle).await.map_err(describe)
}

/// Plays one album track (by its handle) on the zone right now. Used for the
/// tracks of a release that Roon's browse pages leave out.
#[tauri::command]
pub async fn library_play_track(app: AppHandle, zone_id: String, handle: String, how: Option<roon_library::PlayHow>) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.play_track_how(&zone_id, &handle, how.unwrap_or_default()).await.map_err(describe)
}

// --- Wrapped pages ---

/// An album by the Core's album id, as a card with a session handle.
/// A track by the Core's track id: its handle for this session (the
/// Tracks page keeps ids only).
#[tauri::command]
pub async fn library_track_handle(app: AppHandle, track_id: u64) -> Result<String, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.track_by_id(track_id).await.map_err(describe)
}

#[tauri::command]
pub async fn library_album_by_id(app: AppHandle, album_id: u64) -> Result<AlbumCard, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.album_by_id(album_id).await.map_err(describe)
}

/// An artist by the Core's performer id, as a card with a session handle.
#[tauri::command]
pub async fn library_artist_by_id(app: AppHandle, performer_id: u64) -> Result<ArtistCard, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.artist_by_id(performer_id).await.map_err(describe)
}

/// An album card again, by handle (its heart / library state may have changed).
#[tauri::command]
pub fn library_album_card(library: State<'_, Library>, handle: String) -> Result<AlbumCard, String> {
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.album_card(&handle).ok_or_else(|| "That album is no longer known (the link reconnected).".to_string())
}

/// An artist card again, by handle.
#[tauri::command]
pub fn library_artist_card(library: State<'_, Library>, handle: String) -> Result<ArtistCard, String> {
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.artist_card(&handle).ok_or_else(|| "That artist is no longer known (the link reconnected).".to_string())
}

/// Any artist's discography (by the artist's handle), each album with a handle.
#[tauri::command]
pub async fn library_artist_albums_for(app: AppHandle, handle: String, zone_id: Option<String>) -> Result<Vec<ArtistAlbum>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.artist_albums_for(&handle, zone_id.as_deref()).await.map_err(describe)
}

/// Roon's picks around an artist: similar artists, recommended, latest and
/// appears-on albums.
#[tauri::command]
pub async fn library_artist_extras(app: AppHandle, handle: String) -> Result<ArtistExtras, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.artist_extras(&handle).await.map_err(describe)
}

/// An artist's popular songs for their page: the songs Roon offers for the
/// artist (which can be played), put in the order of Last.fm's most played
/// when Last.fm knows them (`by` then says "lastfm"), in Roon's own order
/// otherwise.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PopularTracks {
    pub tracks: Vec<roon_library::TopTrack>,
    pub by: &'static str,
}

#[tauri::command]
pub async fn library_artist_popular(app: AppHandle, handle: String, name: String) -> Result<PopularTracks, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let (roon, lastfm) = tokio::join!(client.top_tracks(&handle), crate::tags::lastfm_top_tracks(&app, &name));
    let mut tracks = roon.map_err(describe)?;
    // (a song's title as both name it: no case, no "(Remastered)" and the
    // like, no punctuation)
    let plain = |title: &str| -> String {
        let mut out = String::new();
        let mut depth = 0;
        for c in title.chars() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = (depth - 1).max(0),
                c if depth == 0 && c.is_alphanumeric() => out.extend(c.to_lowercase()),
                _ => {}
            }
        }
        out
    };
    let rank: std::collections::HashMap<String, usize> = lastfm.iter().enumerate().rev().map(|(i, title)| (plain(title), i)).collect();
    let matched = tracks.iter().filter(|t| rank.contains_key(&plain(&t.title))).count();
    println!("[library {}] popular, {name}: {} from Roon, {} from Last.fm, {matched} matched", clock(), tracks.len(), lastfm.len());
    // (Last.fm's order when it knows a fair share of them; a song it
    // doesn't list keeps Roon's place after the ones it does)
    let by = if matched >= 3 || (matched > 0 && matched * 2 >= tracks.len()) { "lastfm" } else { "roon" };
    if by == "lastfm" {
        tracks.sort_by_key(|t| rank.get(&plain(&t.title)).copied().unwrap_or(usize::MAX));
    }
    Ok(PopularTracks { tracks, by })
}

/// Roon's picks around an album: similar albums, recommended albums, similar artists.
#[tauri::command]
pub async fn library_album_extras(app: AppHandle, handle: String) -> Result<AlbumExtras, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.album_extras(&handle).await.map_err(describe)
}

/// The artists Roon recommends to the profile (Home's "Recommended for you").
#[tauri::command]
pub async fn library_recommended_artists(app: AppHandle) -> Result<Vec<ArtistCard>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.recommended_artists().await.map_err(describe)
}

/// Roon's metadata on an artist: genres, country, birthplace, active since.
#[tauri::command]
pub async fn library_artist_facts(app: AppHandle, handle: String) -> Result<Facts, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.artist_facts(&handle).await.map_err(describe)
}

/// Roon's metadata on an album: its genres.
#[tauri::command]
pub async fn library_album_facts(app: AppHandle, handle: String) -> Result<Facts, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.album_facts(&handle).await.map_err(describe)
}

/// Roon's genre page by name: top and recommended artists and albums, new releases.
#[tauri::command]
pub async fn library_genre_page(app: AppHandle, name: String) -> Result<GenrePage, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.genre_page(&name).await.map_err(describe)
}

/// What Roon knows about these genres (by name), for Roon's Genres page in
/// Wrapped: picture, artist and album counts, sub-genres. Logs each one.
#[tauri::command]
pub async fn library_genre_info(app: AppHandle, names: Vec<String>) -> Result<Vec<GenreInfo>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let found = client.genre_info(&names).await.map_err(describe)?;
    for g in &found {
        println!(
            "[genres {}] {}: {} artists, {} albums, {} sub-genres, {}",
            clock(),
            g.name,
            g.artists.map_or("?".into(), |n| n.to_string()),
            g.albums.map_or("?".into(), |n| n.to_string()),
            g.subgenres.len(),
            if g.image_key.is_some() { "picture" } else { "no picture" },
        );
    }
    Ok(found)
}

/// The library's main artists in a genre (by name), its sub-genres' included.
#[tauri::command]
pub async fn library_genre_artists(app: AppHandle, name: String) -> Result<Vec<ArtistCard>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let found = client.genre_artists(&name).await.map_err(describe)?;
    println!("[genres {}] {name}: {} artists in the library", clock(), found.len());
    Ok(found)
}

/// The library's albums in a genre (by name), its sub-genres' included.
#[tauri::command]
pub async fn library_genre_albums(app: AppHandle, name: String) -> Result<Vec<AlbumCard>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let found = client.genre_albums(&name).await.map_err(describe)?;
    println!("[genres {}] {name}: {} albums in the library", clock(), found.len());
    Ok(found)
}

/// Roon's play history, newest first, up to `limit` plays.
#[tauri::command]
pub async fn library_history(app: AppHandle, limit: Option<usize>) -> Result<History, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.history(limit.unwrap_or(500).clamp(1, 20_000)).await.map_err(describe)
}

/// Every main artist in the library.
#[tauri::command]
pub async fn library_artists(app: AppHandle) -> Result<Vec<ArtistCard>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.library_artists(&|_, _| {}).await.map_err(describe)
}

/// Roon's share link for an artist or album (the Roon app opens it).
#[tauri::command]
pub async fn library_share_link(app: AppHandle, handle: String) -> Result<String, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.share_link(&handle).await.map_err(describe)
}

/// Roon's own search: artists, albums and tracks, with handles.
#[tauri::command]
pub async fn library_search(app: AppHandle, query: String) -> Result<SearchHits, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let query = query.trim().to_string();
    if query.is_empty() {
        return Ok(SearchHits { query, ..Default::default() });
    }
    client.search(&query, 60).await.map_err(describe)
}

/// The playing track's album as a card (for Now Playing's Wrapped buttons).
#[tauri::command]
pub fn library_playing_album(library: State<'_, Library>, zone_id: String) -> Result<AlbumCard, String> {
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.playing_album(&zone_id).map_err(describe)
}

/// A new playlist "Wrapped: <title>" with the playing track.
#[tauri::command]
pub async fn library_playlist_from_playing(app: AppHandle, zone_id: String) -> Result<PlaylistCard, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.playlist_from_playing(&zone_id).await.map_err(describe)
}

/// Roon's playlists.
#[tauri::command]
pub fn library_playlists(library: State<'_, Library>) -> Result<Vec<PlaylistCard>, String> {
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.playlists().map_err(describe)
}

/// A new Roon playlist of the given tracks (handles), named `name`.
#[tauri::command]
pub async fn library_create_playlist(app: AppHandle, name: String, tracks: Vec<String>) -> Result<PlaylistCard, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.create_playlist(name.trim(), &tracks).await.map_err(describe)
}

/// Adds tracks (handles) to the end of a playlist (by handle); how many
/// went in.
#[tauri::command]
pub async fn library_playlist_add(app: AppHandle, handle: String, tracks: Vec<String>) -> Result<usize, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let result = client.add_to_playlist(&handle, &tracks).await;
    // (what Roon made of it, while adding to a playlist is new)
    println!("[library {}] add to playlist, {} tracks: {}", clock(), tracks.len(), match &result { Ok(n) => format!("{n} in"), Err(e) => format!("failed: {e}") });
    result.map_err(describe)
}

/// The zone's current track as a handle (for adding it to a playlist).
#[tauri::command]
pub fn library_playing_handle(library: State<'_, Library>, zone_id: String) -> Result<String, String> {
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.playing_handle(&zone_id).map_err(describe)
}

/// Deletes a playlist (by handle).
#[tauri::command]
pub async fn library_delete_playlist(app: AppHandle, handle: String) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.delete_playlist(&handle).await.map_err(describe)
}

/// Plays a playlist (by handle) on the zone from its start.
#[tauri::command]
pub async fn library_play_playlist(app: AppHandle, zone_id: String, handle: String, clear: Option<bool>) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    // (a fresh queue first when asked: the Discover lists play as a queue
    // of their own, like the radio)
    if clear.unwrap_or(false) {
        let _ = client.queue_clear(&zone_id).await;
    }
    client.play_playlist(&zone_id, &handle).await.map_err(describe)?;
    // Where this queue begins, for the queue panel's played part
    if clear.unwrap_or(false) {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        if let Ok(songs) = client.queue_songs(&zone_id, None, 1).await {
            if let Some(first) = songs.iter().find(|s| s.current).and_then(|s| s.item_id) {
                crate::queue::set_start(&app, &zone_id, first);
            }
        }
    }
    Ok(())
}

/// A playlist's songs, in order (the playlist page).
#[tauri::command]
pub async fn library_playlist_tracks(app: AppHandle, handle: String) -> Result<Vec<roon_library::PlaylistEntry>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.playlist_tracks(&handle).await.map_err(describe)
}

/// Plays a playlist from one of its songs (`item`, a PlaylistEntry's item
/// handle) on.
#[tauri::command]
pub async fn library_play_playlist_from(app: AppHandle, zone_id: String, handle: String, item: Option<String>) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.play_playlist_from(&zone_id, &handle, item.as_deref()).await.map_err(describe)
}

/// Takes songs out of a playlist; the songs left, read again.
#[tauri::command]
pub async fn library_playlist_remove(app: AppHandle, handle: String, items: Vec<String>) -> Result<Vec<roon_library::PlaylistEntry>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.remove_playlist_items(&handle, &items).await.map_err(describe)?;
    // (no answer comes: read it again until they're gone, a few seconds at most)
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let left = client.playlist_tracks(&handle).await.map_err(describe)?;
        if !left.iter().any(|e| items.contains(&e.item_handle)) {
            return Ok(left);
        }
    }
    Err("Roon didn't take the songs out.".into())
}

/// The profile's tags.
#[tauri::command]
pub async fn library_tags(app: AppHandle) -> Result<Vec<roon_library::TagCard>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.tags().await.map_err(describe)
}

/// What a tag holds.
#[tauri::command]
pub async fn library_tag_contents(app: AppHandle, handle: String) -> Result<roon_library::TagContents, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.tag_contents(&handle).await.map_err(describe)
}

/// Listen Later, newest first.
#[tauri::command]
pub async fn library_listen_later(app: AppHandle) -> Result<Vec<roon_library::ListenLaterEntry>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.listen_later().await.map_err(describe)
}

/// Takes an album, artist or song out of Listen Later.
#[tauri::command]
pub async fn library_listen_later_remove(app: AppHandle, kind: String, handle: String) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.listen_later_remove(&kind, &handle).await.map_err(describe)
}

/// Plays the zone's queue from one of its items, a played one included
/// (the Core's item id, from `library_queue_songs`).
#[tauri::command]
pub async fn library_queue_play_from(app: AppHandle, zone_id: String, item_id: u64) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.queue_play_from(&zone_id, item_id).await.map_err(describe)
}

/// Starts Roon Radio on the zone from songs, albums, artists and genres
/// (`seeds: { tracks, albums, artists, genres, libraryOnly }`, handles from
/// the pages).
#[tauri::command]
pub async fn library_play_radio(app: AppHandle, zone_id: String, seeds: RadioSeeds) -> Result<Option<i32>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.play_radio(&zone_id, &seeds).await.map_err(describe)
}

/// The songs the zone's queue has played since `start` (the current
/// queue's first item, from queue.rs; without one, or one the queue no
/// longer has, the last `recent` items) and the playing one, for the queue
/// panel's played part (the songs to come are the Roon API's queue).
#[tauri::command]
pub async fn library_queue_songs(app: AppHandle, zone_id: String, start: Option<u64>, recent: Option<usize>) -> Result<Vec<QueueSong>, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let (songs, report) = client.queue_songs_report(&zone_id, start, recent.unwrap_or(0), true).await.map_err(describe)?;
    // (what the Core gave, while the played part is being worked out)
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::write(dir.join("queue-debug.txt"), report);
    }
    Ok(songs)
}

/// The zone's current queue (played since `start` and to come) saved as a
/// playlist named `name`.
#[tauri::command]
pub async fn library_save_queue(app: AppHandle, zone_id: String, name: String, start: Option<u64>) -> Result<PlaylistCard, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let songs = client.queue_songs(&zone_id, start, 0).await.map_err(describe)?;
    if songs.is_empty() {
        return Err("The queue is empty.".into());
    }
    let tracks: Vec<String> = songs.into_iter().map(|s| s.handle).collect();
    client.create_playlist(&name, &tracks).await.map_err(describe)
}

/// The Wrapped radio: a mix of the artists' top songs (by handle), queued
/// on the zone at once (see `play_artist_mix`).
#[tauri::command]
pub async fn library_play_artist_mix(app: AppHandle, zone_id: String, artists: Vec<String>, mut options: MixOptions) -> Result<MixPlayed, String> {
    // (no live recordings while Settings' Live albums is off, whatever the
    // radio's own switch says)
    if !crate::wrapped::live_albums(&app) {
        options.include_live = false;
    }
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let played = client.play_artist_mix(&zone_id, &artists, &options).await.map_err(describe)?;
    // The queue panel's played part starts here
    if let Some(item) = played.queue_start {
        crate::queue::set_start(&app, &zone_id, item);
    }
    Ok(played)
}

/// Plays an album (by handle) on the zone right now.
#[tauri::command]
pub async fn library_play_album(app: AppHandle, zone_id: String, handle: String, how: Option<roon_library::PlayHow>) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.play_album_how(&zone_id, &handle, how.unwrap_or_default()).await.map_err(describe)
}

fn add_mode(app: &AppHandle) -> AddMode {
    settings::get_settings(app.clone()).library_add_mode
}

/// For status.rs: the state as a word for the plugin's `library` field.
pub fn state_word(app: &AppHandle) -> &'static str {
    match app.state::<Library>().status().state {
        LibraryState::Off => "off",
        LibraryState::Connecting => "connecting",
        LibraryState::Ready => "ready",
        LibraryState::Unsupported => "unsupported",
        LibraryState::Unavailable => "unavailable",
    }
}
