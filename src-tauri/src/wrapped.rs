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

use roon_library::{LibraryTrack, Play};
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
    /// Last.fm scrobbles from before the oldest play Roon has (they join
    /// the most-played lists; scrobbles.rs).
    pub scrobbles: usize,
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
    /// Genre and Style together.
    Blend,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WrappedSettings {
    /// The user's own Last.fm API key (last.fm/api/account/create); without
    /// it only MusicBrainz is asked for tags.
    pub lastfm_key: String,
    /// The user's Last.fm username, optional: their scrobbles fill in the
    /// listening from before Roon's history (scrobbles.rs).
    pub lastfm_user: String,
    /// Tier 3's own folder of music files (sound.rs "Manually choose music
    /// folder"), for songs Roon can't hand over; empty for none.
    pub music_folder: String,
    pub similar_by: SimilarBy,
    pub similar_scope: SimilarScope,
    /// Wrapped Discovery (discover.rs): the weekly lists, ratings, the
    /// player's "Suggested because", and the zone watching behind them.
    /// Off, none of it shows or runs; what it kept stays on disk.
    pub discovery: bool,
    /// Fine Tuning's tier switches for Discover Weekly: tier 2 (the
    /// language model) and tier 3 (sound) used when set up.
    pub weekly_tier2: bool,
    pub weekly_tier3: bool,
    /// Deep Dive's tier 3 switch (dive.rs): listening to the songs, when
    /// set up. (Tier 1 always runs, and tier 2 is what makes a Deep Dive.)
    pub dive_tier3: bool,
    /// Tier 2, the language model (judge.rs): which service, its key (or
    /// Ollama's address), the model name (empty: the service's default),
    /// and how much say it gets ("light": keep or drop, pick the song;
    /// "heavy": also reorder and add, later).
    pub llm_provider: String,
    pub llm_key: String,
    pub llm_model: String,
    pub llm_url: String,
    pub llm_mode: String,
    /// The user's own Discogs token (discogs.com/settings/developers),
    /// optional: Discogs' styles per album for tier 2.
    pub discogs_token: String,
    /// "Leave Ollama to me": Wrapped doesn't start or stop Ollama (by
    /// default it does, see ollama.rs).
    pub ollama_manual: bool,
    /// Deep Dive's own model (dive.rs), on the same service: it reads far
    /// more per playlist, so a bigger model pays off there. Empty: the
    /// model above.
    pub dive_model: String,
    /// How big Wrapped's pages are (the top bar, side menu and player stay
    /// Roon's size): a percent ("90", "100", "110", "125", "150") or "auto",
    /// scaled with the window's width.
    pub page_size: String,
    /// Which side menu shows when Wrapped opens: "wrapped" (Wrapped's own)
    /// or "roon" (Roon's sidebar, as it is).
    pub side_menu: String,
}

impl Default for WrappedSettings {
    fn default() -> Self {
        WrappedSettings {
            lastfm_key: String::new(),
            lastfm_user: String::new(),
            music_folder: String::new(),
            similar_by: SimilarBy::default(),
            similar_scope: SimilarScope::default(),
            discovery: true,
            weekly_tier2: true,
            weekly_tier3: true,
            dive_tier3: true,
            llm_provider: "anthropic".into(),
            llm_key: String::new(),
            llm_model: String::new(),
            llm_url: String::new(),
            llm_mode: "light".into(),
            discogs_token: String::new(),
            ollama_manual: false,
            dive_model: String::new(),
            page_size: "100".into(),
            side_menu: "roon".into(),
        }
    }
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
    crate::ollama::settings_changed(&app, &current, &next);
    crate::discover::set_enabled(next.discovery);
    // (Toasted's Settings window shows Wrapped Discovery too)
    let _ = app.emit("wrapped-settings-changed", &next);
    Ok(next)
}

pub fn now_ms() -> i64 {
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

    fn status(&self, app: &AppHandle) -> HistoryStatus {
        let store = self.store.lock().unwrap();
        let oldest = store.plays.last().map(|p| p.played_at).unwrap_or(i64::MAX);
        HistoryStatus {
            plays: store.plays.len(),
            total: store.total,
            synced_at: store.synced_at,
            complete: store.complete,
            syncing: self.syncing.load(Ordering::SeqCst),
            scrobbles: crate::scrobbles::count_before(app, oldest),
        }
    }
}

/// The copy's state (loading it from disk on the first call).
#[tauri::command]
pub fn wrapped_history_status(app: AppHandle) -> HistoryStatus {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(&app);
    wrapped.status(&app)
}

/// One line in `history-log.txt` (app data), the last 50 kept: what each
/// read of Roon's history saw, for when one comes back short.
fn history_log(app: &AppHandle, line: &str) {
    println!("[wrapped] history: {line}");
    let Ok(dir) = app.path().app_data_dir() else { return };
    let path = dir.join("history-log.txt");
    let mut lines: Vec<String> = std::fs::read_to_string(&path).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default();
    let when = chrono_like_now();
    lines.push(format!("{when} {line}"));
    let keep = lines.len().saturating_sub(50);
    let _ = std::fs::write(&path, lines[keep..].join("\n") + "\n");
}

/// Now as "YYYY-MM-DD HH:MM:SS" UTC (no date crate needed for a log line).
fn chrono_like_now() -> String {
    let secs = (now_ms() / 1000) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // (civil date from days since 1970-01-01, Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Brings the copy up to date in the background: everything when it is
/// empty (or `full`), otherwise only what is newer than it. Progress goes
/// out as "wrapped-history" events; the result is the status when done.
#[tauri::command]
pub fn wrapped_history_sync(app: AppHandle, full: Option<bool>) -> Result<HistoryStatus, String> {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(&app);
    if wrapped.syncing.swap(true, Ordering::SeqCst) {
        return Ok(wrapped.status(&app));
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
        // (what the read saw, kept in history-log.txt next to the copy)
        history_log(&app2, &match &result {
            Ok(h) => format!("{} read: {} plays back; {}", if stop_at.is_some() { "new plays" } else { "full" }, h.plays.len(), h.debug),
            Err(e) => format!("read failed: {e}"),
        });
        // (Roon said it has plays but none came back: the copy is kept as
        // it is, rather than emptied)
        let result = result.and_then(|h| {
            if h.plays.is_empty() && h.total > 0 && stop_at.is_none() {
                Err(roon_library::LibraryError::Unconfirmed(format!("Roon has {} plays but none came back", h.total)))
            } else {
                Ok(h)
            }
        });
        match result {
            Ok(history) => {
                let added = {
                    let mut store = wrapped.store.lock().unwrap();
                    let known: std::collections::HashSet<_> = store.plays.iter().map(play_key).collect();
                    let read: Vec<Play> = history.plays.into_iter().filter(|p| stop_at.is_none_or(|at| p.played_at > at)).collect();
                    let added = read.iter().filter(|p| !known.contains(&play_key(p))).count();
                    if stop_at.is_none() {
                        // A full read replaces the copy with every play it
                        // read; one that came back short keeps the copy's
                        // other plays too
                        let short = read.len() as u32 + 50 < history.total;
                        let mut seen = std::collections::HashSet::new();
                        let mut plays: Vec<Play> = read.into_iter().filter(|p| seen.insert(play_key(p))).collect();
                        if short {
                            plays.extend(store.plays.drain(..).filter(|p| seen.insert(play_key(p))));
                        }
                        store.plays = plays;
                    } else {
                        let mut fresh: Vec<Play> = read.into_iter().filter(|p| !known.contains(&play_key(p))).collect();
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
                let status = wrapped.status(&app2);
                emit(status.plays, status.plays, true, Some(added), None, Some(status));
            }
            Err(e) => {
                wrapped.syncing.store(false, Ordering::SeqCst);
                let status = wrapped.status(&app2);
                emit(status.plays, status.plays, true, None, Some(format!("Reading the history failed: {e}")), Some(status));
            }
        }
    });
    Ok(wrapped.status(&app))
}

/// Reads the plays newer than the copy from the Core and keeps them,
/// waiting for it (for a build that needs the latest plays). A full read
/// isn't done here; without a complete copy it just returns.
pub async fn history_catch_up(app: &AppHandle) -> Result<usize, String> {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(app);
    let stop_at = {
        let store = wrapped.store.lock().unwrap();
        if !store.complete {
            return Ok(0);
        }
        store.plays.first().map(|p| p.played_at)
    };
    if wrapped.syncing.swap(true, Ordering::SeqCst) {
        return Ok(0);
    }
    let Some(client) = app.state::<Library>().client() else {
        wrapped.syncing.store(false, Ordering::SeqCst);
        return Err("Not connected to the Roon Core's library yet.".into());
    };
    let quiet = |_: usize, _: usize| {};
    let result = client.history_since(200_000, stop_at, &quiet).await;
    wrapped.syncing.store(false, Ordering::SeqCst);
    let history = result.map_err(|e| format!("Reading the history failed: {e}"))?;
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
        fresh.append(&mut store.plays);
        store.plays = fresh;
        store.plays.sort_by(|a, b| b.played_at.cmp(&a.played_at));
        store.total = history.total;
        store.synced_at = Some(now_ms());
        added
    };
    wrapped.save(app);
    println!("[wrapped] history caught up: {added} new plays");
    Ok(added)
}

/// The newest plays in the copy, `limit` from `offset`.
#[tauri::command]
pub fn wrapped_history_recent(app: AppHandle, limit: Option<usize>, offset: Option<usize>, oldest: Option<bool>) -> Vec<PlayRow> {
    let plays: Vec<Play> = {
        let wrapped = app.state::<Wrapped>();
        wrapped.ensure_loaded(&app);
        let store = wrapped.store.lock().unwrap();
        let (skip, take) = (offset.unwrap_or(0), limit.unwrap_or(200));
        // (newest first as kept; `oldest`: from the other end)
        if oldest.unwrap_or(false) {
            store.plays.iter().rev().skip(skip).take(take).cloned().collect()
        } else {
            store.plays.iter().skip(skip).take(take).cloned().collect()
        }
    };
    // Each play's song in the library copy, for its heart: by the Core's
    // track id, else by artist, title and album
    let tracks = app.state::<LibraryCopy>().tracks(&app);
    let by_id: HashMap<u64, &LibraryTrack> = tracks.iter().filter_map(|t| t.track_id.map(|id| (id, t))).collect();
    let key = |artist: &str, title: &str, album: &str| (artist.to_lowercase(), title.to_lowercase(), album.to_lowercase());
    let by_name: HashMap<(String, String, String), &LibraryTrack> = tracks.iter().map(|t| (key(&t.artist, &t.title, &t.album), t)).collect();
    plays
        .into_iter()
        .map(|play| {
            let found = play.track_id.and_then(|id| by_id.get(&id)).or_else(|| by_name.get(&key(&play.artist, &play.title, &play.album))).copied();
            PlayRow { library_track_id: found.and_then(|t| t.track_id), favorite: found.is_some_and(|t| t.favorite), in_library: found.is_some(), play }
        })
        .collect()
}

/// A play as the History page shows it: with its song's heart, when the
/// song is in the library (and the library copy's id for it).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayRow {
    #[serde(flatten)]
    pub play: Play,
    pub in_library: bool,
    pub favorite: bool,
    pub library_track_id: Option<u64>,
}

/// A heart changed on a song (Tracks, History): the library copy follows,
/// so the pages show it until the next read from Roon.
#[tauri::command]
pub fn wrapped_library_favorite(app: AppHandle, track_id: u64, favorite: bool) {
    let copy = app.state::<LibraryCopy>();
    let changed = copy.with_store(&app, |s| {
        let mut changed = false;
        for t in s.tracks.iter_mut().filter(|t| t.track_id == Some(track_id)) {
            changed |= t.favorite != favorite;
            t.favorite = favorite;
        }
        changed
    });
    if changed {
        if let Some(path) = LibraryCopy::path(&app) {
            copy.with_store(&app, |s| write_json(&path, s));
        }
    }
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
    // The Last.fm scrobbles from before Roon's history starts, when the
    // window reaches back that far (nothing Roon knows is counted twice)
    let oldest = store.plays.last().map(|p| p.played_at).unwrap_or(i64::MAX);
    if since.is_none_or(|at| at < oldest) {
        crate::scrobbles::with(&app, |s| {
            for p in s.plays.iter().filter(|p| p.at < oldest && since.is_none_or(|at| p.at >= at)) {
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
                    last_played: p.at,
                    album_id: None,
                    performer_id: None,
                    track_id: None,
                    image_key: None,
                });
                row.plays += 1;
            }
        });
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryAlbum {
    pub title: String,
    pub artist: String,
    pub album_id: Option<u64>,
    pub performer_id: Option<u64>,
    pub image_key: Option<String>,
    pub year: Option<u32>,
    /// "local", "qobuz", "tidal", "streaming" or "other".
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LibraryStore {
    synced_at: Option<i64>,
    artists: Vec<LibraryArtist>,
    albums: Vec<LibraryAlbum>,
    tracks: Vec<LibraryTrack>,
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
    pub albums: usize,
    pub tracks: usize,
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

    pub fn albums(&self, app: &AppHandle) -> Vec<LibraryAlbum> {
        self.with_store(app, |s| s.albums.clone())
    }

    pub fn tracks(&self, app: &AppHandle) -> Vec<LibraryTrack> {
        self.with_store(app, |s| s.tracks.clone())
    }

    fn status(&self, app: &AppHandle) -> LibraryStatus {
        let (artists, albums, tracks, synced_at) = self.with_store(app, |s| (s.artists.len(), s.albums.len(), s.tracks.len(), s.synced_at));
        LibraryStatus { artists, albums, tracks, synced_at, syncing: self.syncing.load(Ordering::SeqCst) }
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

/// The library's artists, as last read (the Artists page).
#[tauri::command]
pub fn wrapped_library_artists(app: AppHandle) -> Vec<LibraryArtist> {
    app.state::<LibraryCopy>().artists(&app)
}

/// The library's albums, as last read (the Albums page).
#[tauri::command]
pub fn wrapped_library_albums(app: AppHandle) -> Vec<LibraryAlbum> {
    app.state::<LibraryCopy>().albums(&app)
}

/// A Tracks page row: the track, and when it was added: Roon's import
/// date, or the first Last.fm scrobble of the song when that is earlier (a
/// library moved into Roon in one go has one import date for everything).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackRow {
    #[serde(flatten)]
    pub track: LibraryTrack,
    /// Where `added_at` comes from: "roon", "lastfm" or "" (no date known).
    pub added_from: String,
}

/// The library's tracks, as last read (the Tracks page).
#[tauri::command]
pub fn wrapped_library_tracks(app: AppHandle) -> Vec<TrackRow> {
    let tracks = app.state::<LibraryCopy>().tracks(&app);
    // The first scrobble of each song (artist and title, lower-cased)
    let wanted: std::collections::HashSet<(String, String)> = tracks.iter().map(|t| (t.artist.to_lowercase(), t.title.to_lowercase())).collect();
    let mut first: HashMap<(String, String), i64> = HashMap::new();
    crate::scrobbles::with(&app, |s| {
        // (newest first: the last one seen is the first scrobble)
        for p in &s.plays {
            let key = (p.artist.to_lowercase(), p.title.to_lowercase());
            if wanted.contains(&key) {
                first.insert(key, p.at);
            }
        }
    });
    tracks
        .into_iter()
        .map(|mut track| {
            let scrobbled = first.get(&(track.artist.to_lowercase(), track.title.to_lowercase())).copied();
            let added_from = match (track.added_at, scrobbled) {
                (Some(roon), Some(lastfm)) if lastfm < roon => {
                    track.added_at = Some(lastfm);
                    "lastfm"
                }
                (Some(_), _) => "roon",
                (None, Some(lastfm)) => {
                    track.added_at = Some(lastfm);
                    "lastfm"
                }
                (None, None) => "",
            };
            TrackRow { track, added_from: added_from.into() }
        })
        .collect()
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
            let _ = app2.emit("wrapped-library", serde_json::json!({ "read": read, "total": total, "what": "artists", "done": false }));
        };
        let albums_progress = |read: usize, total: usize| {
            let _ = app2.emit("wrapped-library", serde_json::json!({ "read": read, "total": total, "what": "albums", "done": false }));
        };
        match client.library_artists(&progress).await {
            Ok(cards) => {
                let artists: Vec<LibraryArtist> = cards
                    .into_iter()
                    .map(|c| LibraryArtist { name: c.name, performer_id: c.performer_id, image_key: c.image_key, album_count: c.album_count })
                    .collect();
                let n = artists.len();
                // The albums too (a failure keeps the ones read before)
                let albums: Option<Vec<LibraryAlbum>> = match client.library_albums(&albums_progress).await {
                    Ok(cards) => Some(
                        cards
                            .into_iter()
                            .map(|c| LibraryAlbum {
                                title: c.title,
                                artist: c.artist,
                                album_id: c.album_id,
                                performer_id: c.performer_id,
                                image_key: c.image_key,
                                year: c.year,
                                source: c.source,
                            })
                            .collect(),
                    ),
                    Err(e) => {
                        println!("[wrapped] library copy: albums unreadable ({e})");
                        None
                    }
                };
                let albums_n = albums.as_ref().map(|a| a.len());
                let tracks_progress = |read: usize, total: usize| {
                    let _ = app2.emit("wrapped-library", serde_json::json!({ "read": read, "total": total, "what": "tracks", "done": false }));
                };
                let tracks = match client.library_tracks(&tracks_progress).await {
                    Ok(tracks) => Some(tracks),
                    Err(e) => {
                        println!("[wrapped] library copy: tracks unreadable ({e})");
                        None
                    }
                };
                let tracks_n = tracks.as_ref().map(|t| t.len());
                copy.with_store(&app2, |s| {
                    s.artists = artists;
                    if let Some(albums) = albums {
                        s.albums = albums;
                    }
                    if let Some(tracks) = tracks {
                        s.tracks = tracks;
                    }
                    s.synced_at = Some(now_ms());
                });
                if let Some(path) = LibraryCopy::path(&app2) {
                    copy.with_store(&app2, |s| write_json(&path, s));
                }
                println!(
                    "[wrapped] library copy: {n} artists, {} albums, {} tracks",
                    albums_n.map(|n| n.to_string()).unwrap_or_else(|| "no".into()),
                    tracks_n.map(|n| n.to_string()).unwrap_or_else(|| "no".into())
                );
                copy.syncing.store(false, Ordering::SeqCst);
                let _ = app2.emit("wrapped-library", serde_json::json!({ "read": n, "total": n, "what": "artists", "done": true }));
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
    /// Runs `f` over the copy's plays (newest first).
    pub fn with_plays<T>(&self, app: &AppHandle, f: impl FnOnce(&[Play]) -> T) -> T {
        self.ensure_loaded(app);
        let store = self.store.lock().unwrap();
        f(&store.plays)
    }

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
