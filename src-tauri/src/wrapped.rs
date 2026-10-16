//! Wrapped's own records: a copy of Roon's play history kept on disk
//! (`wrapped-history.json` in the app's data folder), so the pages and the
//! picking mode (Discover playlists, the radio builder) can count over all
//! of it without reading 26,000 plays from the Core every time.
//!
//! The first sync reads everything (a few minutes for a big history; the
//! page shows the progress); later syncs read the newest pages until they
//! meet a play the copy already holds. Plays are kept newest first, one
//! line each, told apart by their time and track.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use roon_library::Play;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::library::Library;

/// The copy on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Store {
    /// How many plays Roon reported at the last sync.
    total: u32,
    /// When the last sync finished (Unix ms).
    synced_at: Option<i64>,
    /// Whether a full read has been completed (an incremental sync keeps
    /// it so); a partial first read leaves it false.
    complete: bool,
    /// Newest first.
    plays: Vec<Play>,
}

#[derive(Default)]
pub struct Wrapped {
    store: Mutex<Store>,
    loaded: AtomicBool,
    syncing: AtomicBool,
}

/// What the page shows about the copy.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStatus {
    pub plays: usize,
    pub total: u32,
    pub synced_at: Option<i64>,
    pub complete: bool,
    pub syncing: bool,
}

/// A sync's progress, sent to the pages as "wrapped-history".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    loaded: usize,
    /// 0 while only checking for new plays (the end is wherever the copy
    /// starts, so there is no total to show).
    wanted: usize,
    done: bool,
    /// New plays, when done.
    added: Option<usize>,
    error: Option<String>,
    status: Option<HistoryStatus>,
}

/// A most-played row (see `wrapped_history_top`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopRow {
    pub name: String,
    pub artist: String,
    pub plays: u32,
    pub last_played: i64,
    pub album_id: Option<u64>,
    pub performer_id: Option<u64>,
    pub track_id: Option<u64>,
    pub image_key: Option<String>,
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-history.json"))
}

/// A JSON file in the app's data folder, if it is there and readable.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(e) => {
            println!("[wrapped] {} unreadable ({e}); starting over", path.display());
            None
        }
    }
}

/// Writes a JSON file in one go (a temporary file, then renamed over).
pub fn write_json<T: Serialize>(path: &std::path::Path, value: &T) {
    let bytes = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(e) => {
            println!("[wrapped] could not encode {}: {e}", path.display());
            return;
        }
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = fs::write(&tmp, bytes).and_then(|_| fs::rename(&tmp, path)) {
        println!("[wrapped] could not save {}: {e}", path.display());
    }
}

// ----- Wrapped's own settings (its Settings page, not Toasted's) -----

/// How the "Similar by" row under artists and albums starts out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SimilarBy {
    #[default]
    Roon,
    Genre,
    Style,
    Scene,
    Era,
}

/// Where the tag-based "Similar" rows look: inside the library, or only
/// outside it (discovery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SimilarScope {
    #[default]
    Library,
    Discover,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WrappedSettings {
    /// The user's own Last.fm API key (last.fm/api/account/create); without
    /// it only MusicBrainz is asked for tags.
    pub lastfm_key: String,
    pub similar_by: SimilarBy,
    pub similar_scope: SimilarScope,
}

impl WrappedSettings {
    fn path(app: &AppHandle) -> Option<PathBuf> {
        app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-settings.json"))
    }

    pub fn load(app: &AppHandle) -> Self {
        Self::path(app).and_then(|p| read_json(&p)).unwrap_or_default()
    }

    fn save(&self, app: &AppHandle) {
        if let Some(p) = Self::path(app) {
            write_json(&p, self);
        }
    }
}

#[tauri::command]
pub fn wrapped_settings_get(app: AppHandle) -> WrappedSettings {
    WrappedSettings::load(&app)
}

/// Changes one or more of Wrapped's settings (`{ lastfmKey: "..." }`);
/// unknown names are refused. Returns the whole set.
#[tauri::command]
pub fn wrapped_settings_update(app: AppHandle, changes: serde_json::Value) -> Result<WrappedSettings, String> {
    let Some(changes) = changes.as_object() else {
        return Err("Settings changes must be an object.".into());
    };
    let current = WrappedSettings::load(&app);
    let mut value = serde_json::to_value(&current).map_err(|e| e.to_string())?;
    let fields = value.as_object_mut().expect("settings serialize to an object");
    for (name, new) in changes {
        if !fields.contains_key(name) {
            return Err(format!("Unknown Wrapped setting: {name}"));
        }
        fields.insert(name.clone(), new.clone());
    }
    let next: WrappedSettings = serde_json::from_value(value).map_err(|e| format!("Bad value: {e}"))?;
    next.save(&app);
    Ok(next)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What tells two plays apart: the same track at the same moment is the
/// same play, however it was read.
fn play_key(p: &Play) -> (i64, Option<u64>, String) {
    (p.played_at, p.track_id, p.title.clone())
}

impl Wrapped {
    /// Reads the copy from disk once.
    fn ensure_loaded(&self, app: &AppHandle) {
        if self.loaded.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(path) = path(app) else { return };
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Store>(&bytes) {
                Ok(store) => {
                    println!("[wrapped] history copy: {} plays", store.plays.len());
                    *self.store.lock().unwrap() = store;
                }
                Err(e) => println!("[wrapped] history copy unreadable ({e}); starting over"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => println!("[wrapped] history copy: {e}"),
        }
    }

    fn save(&self, app: &AppHandle) {
        let Some(path) = path(app) else { return };
        write_json(&path, &*self.store.lock().unwrap());
    }

    fn status(&self) -> HistoryStatus {
        let store = self.store.lock().unwrap();
        HistoryStatus {
            plays: store.plays.len(),
            total: store.total,
            synced_at: store.synced_at,
            complete: store.complete,
            syncing: self.syncing.load(Ordering::SeqCst),
        }
    }
}

/// The copy's state (loading it from disk on the first call).
#[tauri::command]
pub fn wrapped_history_status(app: AppHandle) -> HistoryStatus {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(&app);
    wrapped.status()
}

/// Brings the copy up to date in the background: everything when it is
/// empty (or `full`), otherwise only what is newer than it. Progress goes
/// out as "wrapped-history" events; the result is the status when done.
#[tauri::command]
pub fn wrapped_history_sync(app: AppHandle, full: Option<bool>) -> Result<HistoryStatus, String> {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(&app);
    if wrapped.syncing.swap(true, Ordering::SeqCst) {
        return Ok(wrapped.status());
    }
    let library = app.state::<Library>();
    let Some(client) = library.client() else {
        wrapped.syncing.store(false, Ordering::SeqCst);
        return Err("Not connected to the Roon Core's library yet.".into());
    };
    let full = full.unwrap_or(false);
    let stop_at = {
        let store = wrapped.store.lock().unwrap();
        if full || !store.complete {
            None
        } else {
            store.plays.first().map(|p| p.played_at)
        }
    };
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let wrapped = app2.state::<Wrapped>();
        let checking = stop_at.is_some();
        let emit = |loaded: usize, wanted: usize, done: bool, added: Option<usize>, error: Option<String>, status: Option<HistoryStatus>| {
            let _ = app2.emit("wrapped-history", Progress { loaded, wanted, done, added, error, status });
        };
        let progress = |loaded: usize, wanted: usize| emit(loaded, if checking { 0 } else { wanted }, false, None, None, None);
        let result = client.history_since(200_000, stop_at, &progress).await;
        match result {
            Ok(history) => {
                let added = {
                    let mut store = wrapped.store.lock().unwrap();
                    let known: std::collections::HashSet<_> = store.plays.iter().map(play_key).collect();
                    let mut fresh: Vec<Play> = history
                        .plays
                        .into_iter()
                        .filter(|p| stop_at.is_none_or(|at| p.played_at > at))
                        .filter(|p| !known.contains(&play_key(p)))
                        .collect();
                    let added = fresh.len();
                    if stop_at.is_none() {
                        // A full read replaces the copy
                        store.plays = fresh;
                    } else {
                        fresh.append(&mut store.plays);
                        store.plays = fresh;
                    }
                    store.plays.sort_by(|a, b| b.played_at.cmp(&a.played_at));
                    store.total = history.total;
                    store.synced_at = Some(now_ms());
                    store.complete = stop_at.is_some() || store.plays.len() as u32 + 50 >= history.total;
                    added
                };
                wrapped.save(&app2);
                println!("[wrapped] history synced: {added} new plays");
                wrapped.syncing.store(false, Ordering::SeqCst);
                let status = wrapped.status();
                emit(status.plays, status.plays, true, Some(added), None, Some(status));
            }
            Err(e) => {
                wrapped.syncing.store(false, Ordering::SeqCst);
                let status = wrapped.status();
                emit(status.plays, status.plays, true, None, Some(format!("Reading the history failed: {e}")), Some(status));
            }
        }
    });
    Ok(wrapped.status())
}

/// The newest plays in the copy, `limit` from `offset`.
#[tauri::command]
pub fn wrapped_history_recent(app: AppHandle, limit: Option<usize>, offset: Option<usize>) -> Vec<Play> {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(&app);
    let store = wrapped.store.lock().unwrap();
    store.plays.iter().skip(offset.unwrap_or(0)).take(limit.unwrap_or(200)).cloned().collect()
}

/// The most played artists, albums or tracks over the last `days` (all of
/// the copy when None). A play counts when at least half of the track was
/// heard (or when Roon didn't say how much).
#[tauri::command]
pub fn wrapped_history_top(app: AppHandle, what: String, days: Option<u32>, limit: Option<usize>) -> Vec<TopRow> {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(&app);
    let store = wrapped.store.lock().unwrap();
    let since = days.map(|d| now_ms() - d as i64 * 86_400_000);
    let mut rows: HashMap<String, TopRow> = HashMap::new();
    for p in &store.plays {
        if since.is_some_and(|at| p.played_at < at) {
            break; // newest first: the rest is older
        }
        if !counts(p) {
            continue;
        }
        let (key, name) = match what.as_str() {
            "artists" => (p.artist.to_lowercase(), p.artist.clone()),
            "albums" => (format!("{}\u{0}{}", p.album.to_lowercase(), p.artist.to_lowercase()), p.album.clone()),
            _ => (format!("{}\u{0}{}", p.title.to_lowercase(), p.artist.to_lowercase()), p.title.clone()),
        };
        if name.is_empty() {
            continue;
        }
        let row = rows.entry(key).or_insert_with(|| TopRow {
            name,
            artist: p.artist.clone(),
            plays: 0,
            last_played: p.played_at,
            album_id: p.album_id,
            performer_id: p.performer_id,
            track_id: p.track_id,
            image_key: p.image_key.clone(),
        });
        row.plays += 1;
        if row.image_key.is_none() {
            row.image_key = p.image_key.clone();
        }
        if row.album_id.is_none() {
            row.album_id = p.album_id;
        }
    }
    let mut rows: Vec<TopRow> = rows.into_values().collect();
    rows.sort_by(|a, b| b.plays.cmp(&a.plays).then(b.last_played.cmp(&a.last_played)));
    rows.truncate(limit.unwrap_or(60));
    rows
}

// ----- The library's artists (a copy, like the history) -----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryArtist {
    pub name: String,
    pub performer_id: Option<u64>,
    pub image_key: Option<String>,
    pub album_count: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LibraryStore {
    synced_at: Option<i64>,
    artists: Vec<LibraryArtist>,
}

#[derive(Default)]
pub struct LibraryCopy {
    store: Mutex<Option<LibraryStore>>,
    syncing: AtomicBool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStatus {
    pub artists: usize,
    pub synced_at: Option<i64>,
    pub syncing: bool,
}

impl LibraryCopy {
    fn path(app: &AppHandle) -> Option<PathBuf> {
        app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-library.json"))
    }

    fn with_store<T>(&self, app: &AppHandle, f: impl FnOnce(&mut LibraryStore) -> T) -> T {
        let mut guard = self.store.lock().unwrap();
        let store = guard.get_or_insert_with(|| Self::path(app).and_then(|p| read_json(&p)).unwrap_or_default());
        f(store)
    }

    pub fn artists(&self, app: &AppHandle) -> Vec<LibraryArtist> {
        self.with_store(app, |s| s.artists.clone())
    }

    /// The library's artist names, lower-cased, for "is this in the library".
    pub fn names(&self, app: &AppHandle) -> std::collections::HashSet<String> {
        self.with_store(app, |s| s.artists.iter().map(|a| a.name.to_lowercase()).collect())
    }

    fn status(&self, app: &AppHandle) -> LibraryStatus {
        let (artists, synced_at) = self.with_store(app, |s| (s.artists.len(), s.synced_at));
        LibraryStatus { artists, synced_at, syncing: self.syncing.load(Ordering::SeqCst) }
    }
}

/// Who an artist page is, from the pictures Roon fetched for it: a library
/// artist by its photo key, or any performer the session has seen.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistMatch {
    pub name: String,
    pub performer_id: Option<u64>,
    pub handle: Option<String>,
}

#[tauri::command]
pub fn wrapped_artist_by_images(app: AppHandle, keys: Vec<String>) -> Option<ArtistMatch> {
    if keys.is_empty() {
        return None;
    }
    let copy = app.state::<LibraryCopy>();
    let from_library = copy.artists(&app).into_iter().find(|a| a.image_key.as_ref().is_some_and(|k| keys.contains(k)));
    if let Some(a) = from_library {
        return Some(ArtistMatch { name: a.name, performer_id: a.performer_id, handle: None });
    }
    let client = app.state::<Library>().client()?;
    client.artist_by_image(&keys).map(|c| ArtistMatch { name: c.name, performer_id: c.performer_id, handle: Some(c.handle) })
}

#[tauri::command]
pub fn wrapped_library_status(app: AppHandle) -> LibraryStatus {
    app.state::<LibraryCopy>().status(&app)
}

/// Reads the library's artists from the Core in the background (progress
/// as "wrapped-library" events) and keeps them beside the history copy.
#[tauri::command]
pub fn wrapped_library_sync(app: AppHandle) -> Result<LibraryStatus, String> {
    let copy = app.state::<LibraryCopy>();
    if copy.syncing.swap(true, Ordering::SeqCst) {
        return Ok(copy.status(&app));
    }
    let library = app.state::<Library>();
    let Some(client) = library.client() else {
        copy.syncing.store(false, Ordering::SeqCst);
        return Err("Not connected to the Roon Core's library yet.".into());
    };
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let copy = app2.state::<LibraryCopy>();
        let progress = |read: usize, total: usize| {
            let _ = app2.emit("wrapped-library", serde_json::json!({ "read": read, "total": total, "done": false }));
        };
        match client.library_artists(&progress).await {
            Ok(cards) => {
                let artists: Vec<LibraryArtist> = cards
                    .into_iter()
                    .map(|c| LibraryArtist { name: c.name, performer_id: c.performer_id, image_key: c.image_key, album_count: c.album_count })
                    .collect();
                let n = artists.len();
                copy.with_store(&app2, |s| {
                    s.artists = artists;
                    s.synced_at = Some(now_ms());
                });
                if let Some(path) = LibraryCopy::path(&app2) {
                    copy.with_store(&app2, |s| write_json(&path, s));
                }
                println!("[wrapped] library copy: {n} artists");
                copy.syncing.store(false, Ordering::SeqCst);
                let _ = app2.emit("wrapped-library", serde_json::json!({ "read": n, "total": n, "done": true }));
            }
            Err(e) => {
                copy.syncing.store(false, Ordering::SeqCst);
                let _ = app2.emit("wrapped-library", serde_json::json!({ "read": 0, "total": 0, "done": true, "error": format!("Reading the library failed: {e}") }));
            }
        }
    });
    Ok(copy.status(&app))
}

/// An artist as the history copy knows it: plays, the Core's id, and an
/// album cover to stand in for a photo (see `Wrapped::artists`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryArtist {
    pub name: String,
    pub plays: u32,
    pub performer_id: Option<u64>,
    pub album_id: Option<u64>,
    pub image_key: Option<String>,
    pub last_played: i64,
}

impl Wrapped {
    /// Every artist in the copy, most played first (listens of at least
    /// half a track).
    pub fn artists(&self, app: &AppHandle) -> Vec<HistoryArtist> {
        self.ensure_loaded(app);
        let store = self.store.lock().unwrap();
        let mut rows: HashMap<String, HistoryArtist> = HashMap::new();
        for p in &store.plays {
            if p.artist.is_empty() || !counts(p) {
                continue;
            }
            let row = rows.entry(p.artist.to_lowercase()).or_insert_with(|| HistoryArtist {
                name: p.artist.clone(),
                plays: 0,
                performer_id: p.performer_id,
                album_id: p.album_id,
                image_key: p.image_key.clone(),
                last_played: p.played_at,
            });
            row.plays += 1;
            if row.performer_id.is_none() {
                row.performer_id = p.performer_id;
            }
            if row.image_key.is_none() {
                row.image_key = p.image_key.clone();
                row.album_id = p.album_id;
            }
        }
        let mut rows: Vec<HistoryArtist> = rows.into_values().collect();
        rows.sort_by(|a, b| b.plays.cmp(&a.plays).then(b.last_played.cmp(&a.last_played)));
        rows
    }
}

/// Whether a play counts as a listen: at least half the track.
pub fn counts(p: &Play) -> bool {
    match (p.seconds_played, p.length_seconds) {
        (Some(heard), Some(len)) if len > 0 => heard as f64 / len as f64 >= 0.5,
        _ => true,
    }
}
