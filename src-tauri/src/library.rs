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
//!
//! The connection follows the paired Core: it opens once the Core is
//! connected and its address is known, closes when the Core is lost or the
//! setting is switched off, and reconnects with a backoff otherwise. Once a
//! day it reconnects on purpose, because the Core keeps every object it has
//! sent a session until that session ends (a few per track played).

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use roon_library::{AddMode, AlbumTrack, ArtistAlbum, ClientOptions, LibraryClient, LibraryError, LibraryEvent, TrackInfo};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::settings::{self, AppSettings};

/// Planned reconnect, so the Core can drop the objects it kept for us.
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

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

    fn client(&self) -> Option<LibraryClient> {
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
                library.set_status(LibraryState::Ready, None);
                attempt = 0;

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
                        println!("[library {}] daily reconnect", clock());
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
    loop {
        tokio::select! {
            _ = &mut refresh => return None,
            // A deliberate close (stop()) sends no event; notice it here.
            _ = check.tick() => {
                if !client.is_connected() || app.state::<Library>().inner.lock().unwrap().generation != generation {
                    return Some("stopped".into());
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
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let add = add_mode(&app);
    client.set_favorite(&zone_id, favorite, Some(add)).await.map_err(describe)
}

/// Adds the zone's current track (or its album, per the setting unless `mode`
/// says "track" or "album") to the library.
#[tauri::command]
pub async fn library_add(app: AppHandle, zone_id: String, mode: Option<AddMode>) -> Result<TrackInfo, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    let add = mode.unwrap_or_else(|| add_mode(&app));
    client.add_to_library(&zone_id, add).await.map_err(describe)
}

/// Hearts or un-hearts the album of the zone's current track (it must be in the library).
#[tauri::command]
pub async fn library_album_heart(app: AppHandle, zone_id: String, favorite: bool) -> Result<TrackInfo, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.set_album_favorite(&zone_id, favorite).await.map_err(describe)
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
    client.set_track_favorite(&handle, favorite).await.map_err(describe)
}

/// Adds one album track to the library by its handle.
#[tauri::command]
pub async fn library_track_add(app: AppHandle, handle: String) -> Result<AlbumTrack, String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.add_track(&handle).await.map_err(describe)
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
pub async fn library_play_track(app: AppHandle, zone_id: String, handle: String) -> Result<(), String> {
    let library = app.state::<Library>();
    let client = library.client().ok_or_else(|| not_ready(&library))?;
    client.play_track(&zone_id, &handle).await.map_err(describe)
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
