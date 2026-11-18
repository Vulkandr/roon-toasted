//! Wrapped's own records: a copy of Roon's play history kept on disk
//! (`wrapped-history.json` in the app's data folder), so the pages and the
//! picking mode (Discover playlists, the radio builder) can count over all
//! of it without reading 26,000 plays from the Core every time.
//!
//! The first sync reads everything (a few minutes for a big history; the
//! page shows the progress); later syncs read the newest pages until they
//! meet a play the copy already holds. Plays are kept newest first, one
//! line each, told apart by their time and track.
//!
//! Listening is a Roon profile's own, so each profile has its copy
//! (`wrapped-history-<profile id>.json`). The one in memory is the copy of
//! the profile in use (`use_profile` swaps it); a sync reads every
//! profile's in one pass, the others' straight to their files.

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
    /// The copy of the profile in use.
    store: Mutex<Store>,
    /// The file `store` was read from, and is saved to.
    at: Mutex<Option<PathBuf>>,
    /// Another profile's copy, while a Discover list is built for it in
    /// the background (`with_plays`), with its file.
    side: Mutex<Option<(PathBuf, Store)>>,
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
    /// Whose plays are being read (a profile's name), while a sync goes
    /// over more profiles than one.
    profile: Option<String>,
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

/// The one copy from before profiles had their own (1.5 and earlier): it
/// becomes the account's first profile's, the one those versions read
/// (`use_profile`).
fn old_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-history.json"))
}

/// A profile's copy, by the profile's id.
fn path_of(app: &AppHandle, profile: &str) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join(format!("wrapped-history-{profile}.json")))
}

/// The copy to go by before the Core has said which profile is in use: the
/// one of the profile last in use, or the old single copy.
fn path(app: &AppHandle) -> Option<PathBuf> {
    match WrappedSettings::load_saved(app).history_profile {
        Some(id) => path_of(app, &id),
        None => old_path(app),
    }
}

/// A copy read from its file (none there: an empty one).
fn read_store(path: &std::path::Path) -> Store {
    if path.exists() { read_json(path).unwrap_or_default() } else { Store::default() }
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

/// The settings on Settings' General page that a profile can have of its
/// own (with Toasted's three colors, kept in settings.rs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralSet {
    pub page_size: String,
    pub side_menu: String,
    pub genres: String,
    pub similar_by: SimilarBy,
    pub similar_scope: SimilarScope,
}

/// Their names as the pages send them.
const GENERAL_NAMES: [&str; 5] = ["pageSize", "sideMenu", "genres", "similarBy", "similarScope"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WrappedSettings {
    /// The user's own Last.fm API key (last.fm/api/account/create); without
    /// it only MusicBrainz is asked for tags.
    pub lastfm_key: String,
    /// The Last.fm username of the profile in use, optional: its scrobbles
    /// fill in the listening from before Roon's history (scrobbles.rs).
    /// As `load` gives it, this is worked out from the two maps below;
    /// what's saved here is only the one username of 1.5 and earlier,
    /// until it's handed to the first profile (`profile_in_use`).
    pub lastfm_user: String,
    /// A Last.fm username typed for a profile (the Profile page), by
    /// profile id; empty for "none", whatever Roon has.
    pub lastfm_users: HashMap<String, String>,
    /// The Last.fm username each profile is signed in with in Roon (empty
    /// for none), by profile id, as the Core last gave them: what a
    /// profile goes by when nothing was typed for it.
    pub lastfm_roon: HashMap<String, String>,
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
    /// Live albums and songs in what Wrapped picks for you (Home's rows,
    /// Discover's lists, Deep Dive, the radio); off by default.
    pub live_albums: bool,
    /// Whose genres the Genres page shows: "wrapped" (Wrapped's own tags)
    /// or "roon" (Roon's genres).
    pub genres: String,
    /// A Custom profile's own General settings, by profile id (kept when
    /// it goes back to Default, for the next time).
    pub profile_general: HashMap<String, GeneralSet>,
    /// The account's profiles as the Core last gave them (id to name), and
    /// which one comes first. Kept by `profile_in_use`.
    pub profile_names: HashMap<String, String>,
    pub first_profile: Option<String>,
    /// Discover's weekly lists switched on or off for a profile (the
    /// Profile page), by profile id. A profile not in here: on for the
    /// account's first profile, off for the others (`discover_on`).
    pub discover_profiles: HashMap<String, bool>,
    /// Whether the profile in use gets Discover's lists (worked out by
    /// `load`, never saved).
    pub discover_here: bool,
    /// "Rescan daily" switched off (or back on) for a profile, by profile
    /// id; a profile not in here is rescanned.
    pub rescan_profiles: HashMap<String, bool>,
    /// Whether the profile in use is rescanned daily (worked out by
    /// `load`, never saved).
    pub rescan_here: bool,
    /// When the daily rescan last ran (Unix ms).
    pub rescanned_at: i64,
    /// The profile whose listening history is the one in use (its id),
    /// for reading its copy before the Core is reached. Kept by
    /// `Wrapped::use_profile`, not a setting to change.
    pub history_profile: Option<String>,
    /// The shared General settings, when the ones above are a Custom
    /// profile's own (`load`): for the General page's Edit Default. Never
    /// saved.
    pub shared_general: Option<GeneralSet>,
}

impl Default for WrappedSettings {
    fn default() -> Self {
        WrappedSettings {
            lastfm_key: String::new(),
            lastfm_user: String::new(),
            lastfm_users: HashMap::new(),
            lastfm_roon: HashMap::new(),
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
            live_albums: false,
            genres: "wrapped".into(),
            profile_general: HashMap::new(),
            profile_names: HashMap::new(),
            first_profile: None,
            discover_profiles: HashMap::new(),
            discover_here: true,
            rescan_profiles: HashMap::new(),
            rescan_here: true,
            rescanned_at: 0,
            history_profile: None,
            shared_general: None,
        }
    }
}

impl WrappedSettings {
    fn path(app: &AppHandle) -> Option<PathBuf> {
        app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-settings.json"))
    }

    /// The settings as saved: the General ones are the shared ones.
    pub fn load_saved(app: &AppHandle) -> Self {
        let mut saved: Self = Self::path(app).and_then(|p| read_json(&p)).unwrap_or_default();
        saved.shared_general = None;
        saved
    }

    /// The settings to go by: with the General ones of the profile in use,
    /// if it's Custom (the shared ones are then in `shared_general`).
    pub fn load(app: &AppHandle) -> Self {
        Self::load_saved(app).for_profile(app)
    }

    fn general(&self) -> GeneralSet {
        GeneralSet {
            page_size: self.page_size.clone(),
            side_menu: self.side_menu.clone(),
            genres: self.genres.clone(),
            similar_by: self.similar_by,
            similar_scope: self.similar_scope,
        }
    }

    /// The profile in use, if its General settings are its own.
    fn custom_profile(app: &AppHandle) -> Option<String> {
        let toasted = crate::settings::path(app).map(|p| crate::settings::load(&p)).unwrap_or_default();
        crate::settings::custom_profile(app, &toasted)
    }

    /// A profile's Last.fm username: the one typed for it, else the one
    /// it's signed in with in Roon, else none. No profile shares another's
    /// unless both come to the same name.
    fn lastfm_of(&self, profile: &str) -> String {
        self.lastfm_users.get(profile).or_else(|| self.lastfm_roon.get(profile)).map(|u| u.trim().to_string()).unwrap_or_default()
    }

    /// Every Last.fm username in use, each once (two profiles on the same
    /// account share one read of it).
    pub fn lastfm_all(app: &AppHandle) -> Vec<String> {
        let saved = Self::load_saved(app);
        let mut users: Vec<String> = Vec::new();
        let ids: std::collections::BTreeSet<&String> = saved.lastfm_roon.keys().chain(saved.lastfm_users.keys()).collect();
        for user in ids.into_iter().map(|id| saved.lastfm_of(id)).chain([saved.lastfm_user.trim().to_string()]) {
            if !user.is_empty() && !users.iter().any(|u| u.eq_ignore_ascii_case(&user)) {
                users.push(user);
            }
        }
        users
    }

    /// Whether a profile gets Discover's weekly lists: as switched on its
    /// Profile page, else only the account's first profile does.
    pub fn discover_on(&self, profile: &str) -> bool {
        self.discover_profiles.get(profile).copied().unwrap_or(self.first_profile.as_deref().is_none_or(|first| first == profile))
    }

    /// The profiles that get Discover's lists, the first profile first
    /// and the rest by name.
    pub fn discover_owners(&self) -> Vec<String> {
        let mut ids: Vec<&String> = self.profile_names.keys().filter(|id| self.discover_on(id)).collect();
        ids.sort_by_key(|id| (Some(*id) != self.first_profile.as_ref(), self.profile_names[*id].to_lowercase()));
        ids.into_iter().cloned().collect()
    }

    fn for_profile(mut self, app: &AppHandle) -> Self {
        // (the profile in use, as the history copy has it: known before
        // the Core is reached too)
        if let Some(id) = self.history_profile.clone() {
            self.lastfm_user = self.lastfm_of(&id);
            self.discover_here = self.discover_on(&id);
            self.rescan_here = self.rescan_profiles.get(&id).copied().unwrap_or(true);
        }
        let Some(own) = Self::custom_profile(app).and_then(|id| self.profile_general.get(&id).cloned()) else {
            return self;
        };
        self.shared_general = Some(self.general());
        self.page_size = own.page_size;
        self.side_menu = own.side_menu;
        self.genres = own.genres;
        self.similar_by = own.similar_by;
        self.similar_scope = own.similar_scope;
        self
    }

    /// (only ever what `load_saved` gave, changed)
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

/// Makes a profile's General settings its own (Custom) or the shared ones
/// again (Default). Its own start as a copy of the shared ones, colors
/// included; going back to Default keeps them for the next time. Every
/// window hears, since the settings to go by may just have changed.
#[tauri::command]
pub fn wrapped_profile_custom(app: AppHandle, id: String, custom: bool) -> Result<(), String> {
    let path = crate::settings::path(&app).ok_or("Couldn't find the app data folder.")?;
    let mut toasted = crate::settings::load(&path);
    toasted.custom_profiles.retain(|p| p != &id);
    if custom {
        toasted.custom_profiles.push(id.clone());
        toasted.profile_colors.entry(id.clone()).or_insert_with(|| crate::settings::ProfileColors {
            colors: toasted.toaster_colors,
            accent: toasted.toaster_accent.clone(),
            background: toasted.toaster_background.clone(),
        });
        let mut saved = WrappedSettings::load_saved(&app);
        if !saved.profile_general.contains_key(&id) {
            let copy = saved.general();
            saved.profile_general.insert(id.clone(), copy);
            saved.save(&app);
        }
    }
    crate::settings::save(&path, &toasted)?;
    crate::settings::announce(&app);
    let _ = app.emit("wrapped-settings-changed", WrappedSettings::load(&app));
    Ok(())
}

/// Changes one or more of Wrapped's settings (`{ lastfmKey: "..." }`);
/// unknown names are refused. Returns the whole set (as `load` gives it).
///
/// A General setting (`GENERAL_NAMES`) goes to the own copy of the profile
/// in use while that profile is Custom, unless `shared` asks for the
/// shared ones (the General page's Edit Default).
#[tauri::command]
pub fn wrapped_settings_update(app: AppHandle, changes: serde_json::Value, shared: Option<bool>) -> Result<WrappedSettings, String> {
    let Some(changes) = changes.as_object() else {
        return Err("Settings changes must be an object.".into());
    };
    let current = WrappedSettings::load(&app);
    let saved = WrappedSettings::load_saved(&app);
    let own = if shared == Some(true) { None } else { WrappedSettings::custom_profile(&app) };
    let mut value = serde_json::to_value(&saved).map_err(|e| e.to_string())?;
    let fields = value.as_object_mut().expect("settings serialize to an object");
    for (name, new) in changes {
        if ["profileGeneral", "sharedGeneral", "historyProfile", "lastfmUsers", "lastfmRoon", "profileNames", "firstProfile", "discoverProfiles", "discoverHere", "rescanProfiles", "rescanHere", "rescannedAt"].contains(&name.as_str()) {
            return Err(format!("{name} isn't changed directly."));
        }
        if !fields.contains_key(name) {
            return Err(format!("Unknown Wrapped setting: {name}"));
        }
        // (a Last.fm username is the profile in use's own)
        if let ("lastfmUser", Some(id)) = (name.as_str(), &saved.history_profile) {
            if let Some(all) = fields.get_mut("lastfmUsers").and_then(|all| all.as_object_mut()) {
                all.insert(id.clone(), new.clone());
            }
            continue;
        }
        match &own {
            Some(id) if GENERAL_NAMES.contains(&name.as_str()) => {
                let copy = serde_json::to_value(saved.profile_general.get(id).cloned().unwrap_or_else(|| saved.general())).map_err(|e| e.to_string())?;
                let all = fields.get_mut("profileGeneral").and_then(|all| all.as_object_mut()).ok_or("The profiles' settings are damaged.")?;
                all.entry(id.clone()).or_insert(copy)[name.as_str()] = new.clone();
            }
            _ => {
                fields.insert(name.clone(), new.clone());
            }
        }
    }
    let next: WrappedSettings = serde_json::from_value(value).map_err(|e| format!("Bad value: {e}"))?;
    next.save(&app);
    let next = next.for_profile(&app);
    crate::ollama::settings_changed(&app, &current, &next);
    crate::discover::set_enabled(next.discovery);
    if next.lastfm_user != current.lastfm_user {
        crate::scrobbles::user_changed(&app);
    }
    // (Toasted's Settings window shows Wrapped Discovery too)
    let _ = app.emit("wrapped-settings-changed", &next);
    Ok(next)
}

/// Whether live albums and songs may be in what Wrapped picks (Settings,
/// Wrapped Discovery's Live albums).
pub fn live_albums(app: &AppHandle) -> bool {
    WrappedSettings::load(app).live_albums
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
        let store = read_store(&path);
        println!("[wrapped] history copy: {} plays", store.plays.len());
        *self.store.lock().unwrap() = store;
        *self.at.lock().unwrap() = Some(path);
    }

    fn save(&self, app: &AppHandle) {
        let Some(path) = self.at.lock().unwrap().clone().or_else(|| path(app)) else { return };
        write_json(&path, &*self.store.lock().unwrap());
    }

    /// The profile in use, as the Core has it (`first`: the account's
    /// first profile): its copy becomes the one in memory, if it wasn't.
    /// The single copy of 1.5 and earlier is handed to the first profile
    /// the first time through. Returns whether the copy changed.
    pub fn use_profile(&self, app: &AppHandle, first: &str, id: &str) -> bool {
        self.ensure_loaded(app);
        if let (Some(old), Some(firsts)) = (old_path(app), path_of(app, first)) {
            if old.exists() && !firsts.exists() {
                match fs::rename(&old, &firsts) {
                    Ok(()) => {
                        println!("[wrapped] the history copy is the first profile's now");
                        let mut at = self.at.lock().unwrap();
                        if at.as_deref() == Some(old.as_path()) {
                            *at = Some(firsts);
                        }
                    }
                    Err(e) => println!("[wrapped] could not hand the history copy to the first profile: {e}"),
                }
            }
        }
        let Some(target) = path_of(app, id) else { return false };
        let mut saved = WrappedSettings::load_saved(app);
        if saved.history_profile.as_deref() != Some(id) {
            saved.history_profile = Some(id.to_string());
            saved.save(app);
        }
        let mut at = self.at.lock().unwrap();
        if at.as_deref() == Some(target.as_path()) {
            return false;
        }
        let store = read_store(&target);
        println!("[wrapped] history copy of the profile in use: {} plays", store.plays.len());
        *self.store.lock().unwrap() = store;
        *at = Some(target);
        true
    }

    /// Looks at a profile's copy (in memory if that profile is in use,
    /// otherwise from its file) without saving anything.
    fn read_copy<R>(&self, app: &AppHandle, profile: &str, look: impl FnOnce(&Store) -> R) -> Option<R> {
        let path = path_of(app, profile)?;
        if self.at.lock().unwrap().as_deref() == Some(path.as_path()) {
            return Some(look(&self.store.lock().unwrap()));
        }
        Some(look(&read_store(&path)))
    }

    /// Changes a profile's copy: the one in memory if that profile is in
    /// use (saved after), otherwise its file.
    fn with_copy<R>(&self, app: &AppHandle, profile: &str, change: impl FnOnce(&mut Store) -> R) -> Option<R> {
        let path = path_of(app, profile)?;
        if self.at.lock().unwrap().as_deref() == Some(path.as_path()) {
            let out = change(&mut self.store.lock().unwrap());
            self.save(app);
            return Some(out);
        }
        let mut store = read_store(&path);
        let out = change(&mut store);
        write_json(&path, &store);
        // (a copy of it kept for a build is read again when next wanted)
        let mut side = self.side.lock().unwrap();
        if side.as_ref().is_some_and(|(at, _)| *at == path) {
            *side = None;
        }
        Some(out)
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

/// Discover's weekly lists switched on or off for a profile (the Profile
/// page).
#[tauri::command]
pub fn wrapped_profile_discover(app: AppHandle, id: String, on: bool) -> WrappedSettings {
    let mut saved = WrappedSettings::load_saved(&app);
    saved.discover_profiles.insert(id, on);
    saved.save(&app);
    let now = WrappedSettings::load(&app);
    let _ = app.emit("wrapped-settings-changed", &now);
    now
}

/// Something banned as History's Banned lists it: a song, an album
/// (`title` its title) or an artist (`title` the name).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BannedSong {
    /// "song" (also when empty: the first records had no kind), "album"
    /// or "artist".
    pub kind: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub image_key: Option<String>,
    pub album_id: Option<u64>,
    /// When it was banned with Toasted's button (Unix ms); 0 for a ban
    /// only known from the library copy (made in Roon).
    pub at: i64,
    pub in_library: bool,
}

fn ban_key(title: &str, artist: &str) -> String {
    format!("{}\u{0}{}", title.trim().to_lowercase(), artist.trim().to_lowercase())
}

fn ban_kind(song: &BannedSong) -> &str {
    if song.kind.is_empty() { "song" } else { &song.kind }
}

/// The bans made with Toasted's button, by profile
/// (`wrapped-bans-<profile id>.json`): Roon gives no list of the songs
/// banned outside the library, so these are kept here.
fn bans_path(app: &AppHandle) -> Option<PathBuf> {
    let id = crate::discover::profile_in_use(app)?;
    app.path().app_data_dir().ok().map(|dir| dir.join(format!("wrapped-bans-{id}.json")))
}

/// A ban made (or lifted) with Toasted's button, noted for the list; the
/// library copy's own mark is kept in step.
pub fn ban_note(app: &AppHandle, mut song: BannedSong, banned: bool) {
    let Some(path) = bans_path(app) else { return };
    let mut kept: Vec<BannedSong> = read_json(&path).unwrap_or_default();
    let key = ban_key(&song.title, &song.artist);
    let kind = ban_kind(&song).to_string();
    kept.retain(|s| ban_kind(s) != kind || ban_key(&s.title, &s.artist) != key);
    if banned {
        song.at = now_ms();
        kept.insert(0, song.clone());
    }
    write_json(&path, &kept);
    if kind != "song" {
        return;
    }
    if let Some(id) = crate::discover::profile_in_use(app) {
        app.state::<LibraryCopy>().with_store(app, |s| {
            for t in s.tracks.iter_mut().filter(|t| ban_key(&t.title, &t.artist) == key && (song.album.is_empty() || t.album.eq_ignore_ascii_case(&song.album))) {
                t.banned_by.retain(|by| *by != id);
                if banned {
                    t.banned_by.push(id.clone());
                }
            }
        });
    }
}

/// The songs banned for the profile in use: the ones banned with Toasted's
/// button (newest first), then the library's songs Roon has as banned.
/// Songs banned in Roon that aren't in the library can't be listed: Roon
/// has no list of those.
#[tauri::command]
pub fn wrapped_banned_songs(app: AppHandle, kind: Option<String>) -> Vec<BannedSong> {
    let kind = kind.unwrap_or_else(|| "song".into());
    let kept: Vec<BannedSong> = bans_path(&app).and_then(|p| read_json(&p)).unwrap_or_default();
    let mut out: Vec<BannedSong> = kept.into_iter().filter(|s| ban_kind(s) == kind).collect();
    let mut seen: std::collections::HashSet<String> = out.iter().map(|s| ban_key(&s.title, &s.artist)).collect();
    if kind == "artist" {
        // (and the artists on Wrapped's own ban list: a thumbs down in
        // Discover, or one added in Settings)
        for banned in crate::discover::wrapped_ban_list(app.clone()) {
            if seen.insert(ban_key(&banned.name, "")) {
                out.push(BannedSong { kind: "artist".into(), title: banned.name, at: banned.at, ..Default::default() });
            }
        }
        return out;
    }
    if kind != "song" {
        return out;
    }
    if let Some(id) = crate::discover::profile_in_use(&app) {
        app.state::<LibraryCopy>().with_store(&app, |s| {
            for t in s.tracks.iter().filter(|t| t.banned_by.contains(&id)) {
                if seen.insert(ban_key(&t.title, &t.artist)) {
                    out.push(BannedSong { kind: "song".into(), title: t.title.clone(), artist: t.artist.clone(), album: t.album.clone(), image_key: t.image_key.clone(), album_id: t.album_id, at: 0, in_library: true });
                }
            }
        });
    }
    out
}

/// A library song with how often the profile in use has played it (an
/// artist's page, My Library).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayedTrack {
    #[serde(flatten)]
    pub track: LibraryTrack,
    pub plays: usize,
}

/// The artist's songs in the library, the most played first (by the
/// profile in use, from its history), then the rest by album and number.
#[tauri::command]
pub fn wrapped_artist_library_tracks(app: AppHandle, name: String, performer_id: Option<u64>) -> Vec<PlayedTrack> {
    let wanted = name.trim().to_lowercase();
    let theirs = |t: &LibraryTrack| {
        (performer_id.is_some() && t.performer_id == performer_id)
            || t.artist.to_lowercase().split(" / ").flat_map(|a| a.split(", ")).any(|a| a.trim() == wanted)
    };
    let tracks: Vec<LibraryTrack> = app.state::<LibraryCopy>().with_store(&app, |s| s.tracks.iter().filter(|t| theirs(t)).cloned().collect());
    if tracks.is_empty() {
        return Vec::new();
    }
    // (plays by Roon's track id, and by title for a play of another copy
    // of the song)
    let ids: std::collections::HashSet<u64> = tracks.iter().filter_map(|t| t.track_id).collect();
    let titles: std::collections::HashSet<String> = tracks.iter().map(|t| t.title.to_lowercase()).collect();
    let (mut by_id, mut by_title): (HashMap<u64, usize>, HashMap<String, usize>) = (HashMap::new(), HashMap::new());
    app.state::<Wrapped>().with_plays(&app, |plays| {
        for p in plays {
            match p.track_id.filter(|id| ids.contains(id)) {
                Some(id) => *by_id.entry(id).or_default() += 1,
                None => {
                    let title = p.title.to_lowercase();
                    if titles.contains(&title) && p.artist.to_lowercase().contains(&wanted) {
                        *by_title.entry(title).or_default() += 1;
                    }
                }
            }
        }
    });
    let mut out: Vec<PlayedTrack> = tracks
        .into_iter()
        .map(|track| {
            let plays = track.track_id.and_then(|id| by_id.get(&id)).copied().unwrap_or(0) + by_title.get(&track.title.to_lowercase()).copied().unwrap_or(0);
            PlayedTrack { track, plays }
        })
        .collect();
    out.sort_by(|a, b| {
        b.plays
            .cmp(&a.plays)
            .then_with(|| a.track.album.to_lowercase().cmp(&b.track.album.to_lowercase()))
            .then_with(|| (a.track.media_number, a.track.track_number).cmp(&(b.track.media_number, b.track.track_number)))
    });
    out.truncate(60);
    out
}

/// "Rescan daily" switched on or off for a profile (the Profile page).
#[tauri::command]
pub fn wrapped_profile_rescan_daily(app: AppHandle, id: String, on: bool) -> WrappedSettings {
    let mut saved = WrappedSettings::load_saved(&app);
    saved.rescan_profiles.insert(id, on);
    saved.save(&app);
    let now = WrappedSettings::load(&app);
    let _ = app.emit("wrapped-settings-changed", &now);
    now
}

/// Rescans the profile in use now (the Profile page's Rescan): its new
/// plays and scrobbles, then the library with its hearts, and the tags of
/// artists new to either.
#[tauri::command]
pub async fn wrapped_rescan(app: AppHandle) -> Result<(), String> {
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    let profile = client.profile().ok_or("Roon hasn't said which profile is in use yet.")?;
    rescan(&app, &client, vec![profile.id]).await;
    Ok(())
}

/// One rescan: these profiles' new plays (each into its own copy) and
/// their Last.fm scrobbles, then what every profile shares, the library
/// with its hearts (the tags of new artists follow it, tags.rs). The
/// pages hear of each part as they do of a read started by hand.
async fn rescan(app: &AppHandle, client: &roon_library::LibraryClient, ids: Vec<String>) {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(app);
    let profiles: Vec<roon_library::ProfileInfo> = client.profiles().into_iter().filter(|p| ids.contains(&p.id)).collect();
    if !wrapped.syncing.swap(true, Ordering::SeqCst) {
        for profile in &profiles {
            let name = profile.name.clone();
            let progress = |loaded: usize, wanted: usize| {
                let _ = app.emit("wrapped-history", Progress { loaded, wanted, done: false, added: None, error: None, status: None, profile: Some(name.clone()) });
            };
            match sync_profile(app, client, profile, false, &progress).await {
                Ok(n) => println!("[wrapped] rescan, {}: {n} new plays", profile.name),
                Err(e) => println!("[wrapped] rescan, {}: {e}", profile.name),
            }
        }
        wrapped.syncing.store(false, Ordering::SeqCst);
        let status = wrapped.status(app);
        let _ = app.emit("wrapped-history", Progress { loaded: status.plays, wanted: status.plays, done: true, added: None, error: None, status: Some(status), profile: None });
    }
    let saved = WrappedSettings::load_saved(app);
    let mut users: Vec<String> = Vec::new();
    for user in profiles.iter().map(|p| saved.lastfm_of(&p.id)) {
        if !user.is_empty() && !users.iter().any(|u| u.eq_ignore_ascii_case(&user)) {
            users.push(user);
        }
    }
    crate::scrobbles::rescan(app, users);
    let _ = wrapped_library_sync(app.clone());
}

/// The daily rescan: once a day, every profile that has "Rescan daily" on
/// (they all do until it's switched off on a Profile page) gets its new
/// plays and scrobbles read, and the library is read again, so nobody has
/// to come back and press the buttons. Looks a few minutes after start,
/// then every hour; not without the library link.
pub fn schedule_rescan(app: &AppHandle) {
    const DAY: i64 = 24 * 3_600_000;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(180)).await;
        loop {
            let saved = WrappedSettings::load_saved(&app);
            if now_ms() - saved.rescanned_at >= DAY {
                if let Some(client) = app.state::<Library>().client() {
                    let ids: Vec<String> = client.profiles().into_iter().map(|p| p.id).filter(|id| saved.rescan_profiles.get(id).copied().unwrap_or(true)).collect();
                    if !ids.is_empty() {
                        println!("[wrapped] daily rescan: {} profiles", ids.len());
                        rescan(&app, &client, ids).await;
                        let mut saved = WrappedSettings::load_saved(&app);
                        saved.rescanned_at = now_ms();
                        saved.save(&app);
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3_600)).await;
        }
    });
}

/// A read of Roon's history put into a copy: a full one (`stop_at` None)
/// replaces the copy with every play read (one that came back short keeps
/// the copy's other plays too), otherwise the new plays join it. Returns
/// how many plays are new.
fn merge(store: &mut Store, history: roon_library::History, stop_at: Option<i64>) -> usize {
    let known: std::collections::HashSet<_> = store.plays.iter().map(play_key).collect();
    let read: Vec<Play> = history.plays.into_iter().filter(|p| stop_at.is_none_or(|at| p.played_at > at)).collect();
    let added = read.iter().filter(|p| !known.contains(&play_key(p))).count();
    // (whether a full read came back short of what Roon said it has: by
    // the plays read, before the ones Roon lists twice are dropped)
    let short = read.len() as u32 + 50 < history.total;
    if stop_at.is_none() {
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
    store.complete = stop_at.is_some() || !short;
    added
}

/// The Core's word on which profile is in use, taken up by the history
/// copy (library.rs: on connect, on a switch, and when the account's
/// profiles change). The pages hear when the copy is another one now.
pub fn profile_in_use(app: &AppHandle, client: &roon_library::LibraryClient) {
    let profiles = client.profiles();
    let (Some(first), Some(now)) = (profiles.first(), client.profile()) else { return };
    let wrapped = app.state::<Wrapped>();
    let swapped = wrapped.use_profile(app, &first.id, &now.id);
    // Last.fm: who each profile is in Roon, and the one username of 1.5
    // and earlier handed to the first profile, like its history
    let mut saved = WrappedSettings::load_saved(app);
    let roon: HashMap<String, String> = profiles.iter().map(|p| (p.id.clone(), p.lastfm_user.clone().unwrap_or_default())).collect();
    let names: HashMap<String, String> = profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
    let old = saved.lastfm_user.trim().to_string();
    if roon != saved.lastfm_roon || !old.is_empty() || names != saved.profile_names || saved.first_profile.as_ref() != Some(&first.id) {
        saved.lastfm_roon = roon;
        saved.profile_names = names;
        saved.first_profile = Some(first.id.clone());
        if !old.is_empty() {
            saved.lastfm_users.entry(first.id.clone()).or_insert(old);
            saved.lastfm_user.clear();
        }
        saved.save(app);
    }
    crate::scrobbles::user_changed(app);
    // (Discover's ratings and lists are the profile's own too)
    crate::discover::use_profile(app, &first.id, &now.id);
    // The hearts in the library copy are this profile's now. A copy from
    // before they were kept per profile is read again, once another
    // profile than the one it was read for is in use
    if !app.state::<LibraryCopy>().use_profile(app, &now.id) && swapped {
        println!("[wrapped] library copy: read again for the hearts of each profile");
        // (until it's read, the pages show the scan and not the copy)
        if wrapped_library_sync(app.clone()).is_ok() {
            set_stale(app, true);
        }
    }
    if swapped {
        let status = wrapped.status(app);
        let _ = app.emit("wrapped-history", Progress { loaded: status.plays, wanted: status.plays, done: true, added: None, error: None, status: Some(status), profile: None });
    }
}

/// One profile's plays read from the Core into its copy: everything when
/// the copy isn't complete (or `full`), otherwise what is newer than it.
/// Returns how many plays are new.
async fn sync_profile(
    app: &AppHandle,
    client: &roon_library::LibraryClient,
    profile: &roon_library::ProfileInfo,
    full: bool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<usize, String> {
    let wrapped = app.state::<Wrapped>();
    let stop_at = wrapped
        .read_copy(app, &profile.id, |store| if full || !store.complete { None } else { store.plays.first().map(|p| p.played_at) })
        .flatten();
    let checking = stop_at.is_some();
    let report = |loaded: usize, wanted: usize| progress(loaded, if checking { 0 } else { wanted });
    let result = client.history_of(Some(&profile.id), 200_000, stop_at, &report).await;
    // A profile that has played nothing yet: Roon gives its history no
    // count at all. With no plays in its copy either, that's an empty
    // history read in full (a copy that has plays is left as it is)
    let unread = wrapped.read_copy(app, &profile.id, |store| store.plays.is_empty()).unwrap_or(false);
    let result = match result {
        Err(roon_library::LibraryError::Unconfirmed(why)) if unread && why.contains("how many plays") => {
            Ok(roon_library::History { total: 0, plays: Vec::new(), debug: "no count from Roon: nothing played yet".into() })
        }
        other => other,
    };
    // (what the read saw, kept in history-log.txt next to the copies)
    history_log(app, &match &result {
        Ok(h) => format!("{}, {} read: {} plays back; {}", profile.name, if checking { "new plays" } else { "full" }, h.plays.len(), h.debug),
        Err(e) => format!("{}, read failed: {e}", profile.name),
    });
    let history = result.map_err(|e| format!("Reading the history failed: {e}"))?;
    // (Roon said it has plays but none came back: the copy is kept as it
    // is, rather than emptied)
    if history.plays.is_empty() && history.total > 0 && !checking {
        return Err(format!("Reading the history failed: Roon has {} plays but none came back", history.total));
    }
    wrapped.with_copy(app, &profile.id, |store| merge(store, history, stop_at)).ok_or_else(|| "Couldn't find the app data folder.".to_string())
}

/// Brings the copies up to date in the background, every profile's in one
/// pass (the one in use first): everything for a copy that is empty (or
/// `full`), otherwise only what is newer than it. Progress goes out as
/// "wrapped-history" events; the result is the status when done.
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
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let wrapped = app2.state::<Wrapped>();
        let emit = |loaded: usize, wanted: usize, done: bool, added: Option<usize>, error: Option<String>, status: Option<HistoryStatus>, profile: Option<String>| {
            let _ = app2.emit("wrapped-history", Progress { loaded, wanted, done, added, error, status, profile });
        };
        // (the profile in use first: its plays are the ones on the pages)
        let mut profiles = client.profiles();
        let using = client.profile().map(|p| p.id);
        profiles.sort_by_key(|p| Some(&p.id) != using.as_ref());
        let several = profiles.len() > 1;
        let (mut added, mut error) = (None, None);
        for profile in &profiles {
            let mine = Some(&profile.id) == using.as_ref();
            let name = several.then(|| profile.name.clone());
            let progress = |loaded: usize, wanted: usize| emit(loaded, wanted, false, None, None, None, name.clone());
            match sync_profile(&app2, &client, profile, full, &progress).await {
                Ok(n) => {
                    println!("[wrapped] history synced, {}: {n} new plays", profile.name);
                    if mine {
                        added = Some(n);
                    }
                }
                // (the page says what went wrong with the profile in use;
                // another profile's read is tried again with the next sync)
                Err(e) if mine => error = Some(e),
                Err(e) => println!("[wrapped] history, {}: {e}", profile.name),
            }
        }
        if profiles.is_empty() {
            error = Some("Roon hasn't said which profiles this account has yet.".into());
        }
        wrapped.syncing.store(false, Ordering::SeqCst);
        let status = wrapped.status(&app2);
        emit(status.plays, status.plays, true, if error.is_some() { None } else { added }, error, Some(status), None);
        // (artists new to the history get their tags)
        crate::tags::fill_missing(&app2);
    });
    Ok(wrapped.status(&app))
}

/// Reads the plays newer than the copy from the Core and keeps them,
/// waiting for it (for a build that needs the latest plays). A full read
/// isn't done here; without a complete copy it just returns.
pub async fn history_catch_up(app: &AppHandle) -> Result<usize, String> {
    let wrapped = app.state::<Wrapped>();
    wrapped.ensure_loaded(app);
    let Some(client) = app.state::<Library>().client() else {
        return Err("Not connected to the Roon Core's library yet.".into());
    };
    // (the profile a list is being built for, else the one in use; its
    // own copy, even if another profile is picked meanwhile)
    let profile = match crate::discover::acting() {
        Some(id) => client.profiles().into_iter().find(|p| p.id == id),
        None => client.profile(),
    };
    let Some(profile) = profile else { return Ok(0) };
    if !wrapped.read_copy(app, &profile.id, |store| store.complete).unwrap_or(false) {
        return Ok(0);
    }
    if wrapped.syncing.swap(true, Ordering::SeqCst) {
        return Ok(0);
    }
    let result = sync_profile(app, &client, &profile, false, &|_, _| {}).await;
    wrapped.syncing.store(false, Ordering::SeqCst);
    let added = result?;
    println!("[wrapped] history caught up, {}: {added} new plays", profile.name);
    Ok(added)
}

/// Keeps the copy current while music plays: Roon writes a play down when a song ends or is skipped, so
/// whenever a zone moves off a song (to the next one, or to nothing) the plays newer than the copy are
/// read 4 s later, and again at 10 s and 25 s while none has come (the Core has been seen to take up to a
/// second after the song changed here, and a zone the living queue reloads changes song with no play to
/// find). The pages hear of new plays the way they hear of a sync ("wrapped-history", done), and show
/// them.
pub fn watch_plays(app: &AppHandle) {
    use std::time::{Duration, Instant};
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // (zone -> the song it was on)
        let mut on: HashMap<String, String> = HashMap::new();
        // (when to read, and which try that is)
        const TRIES: [u64; 3] = [4, 10, 25];
        let mut due: Option<(Instant, usize)> = None;
        let mut left = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let zones = app.state::<crate::zones::Zones>().payload_now().zones;
            let mut now: HashMap<String, String> = HashMap::new();
            for z in zones {
                if let Some(np) = &z.now_playing {
                    now.insert(z.zone_id.clone(), format!("{}|{}|{}", np.title, np.artist_full, np.album));
                }
            }
            // (a zone that left a song: for another, or for none)
            if on.iter().any(|(zone, song)| now.get(zone) != Some(song)) {
                left = Instant::now();
                due = Some((left + Duration::from_secs(TRIES[0]), 0));
            }
            on = now;
            let Some((at, n)) = due else { continue };
            if Instant::now() < at {
                continue;
            }
            due = None;
            match history_catch_up(&app).await {
                Ok(0) => due = TRIES.get(n + 1).map(|after| (left + Duration::from_secs(*after), n + 1)),
                Ok(_) => {
                    let status = app.state::<Wrapped>().status(&app);
                    let _ = app.emit("wrapped-history", Progress { loaded: status.plays, wanted: status.plays, done: true, added: None, error: None, status: Some(status), profile: None });
                    // (artists new to the history get their tags)
                    crate::tags::fill_missing(&app);
                }
                Err(e) => println!("[wrapped] history after a song: {e}"),
            }
        }
    });
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
/// A song hearted, un-hearted or added somewhere in the app, as the Core has it now: the copy's entry
/// takes its hearts, and a song the copy doesn't have yet (added since the library was last read) joins
/// it, so the Tracks page and its hearts filter show it without waiting for the next read.
pub fn library_copy_put(app: &AppHandle, track: roon_library::LibraryTrack) {
    if track.track_id.is_none() {
        return;
    }
    let copy = app.state::<LibraryCopy>();
    let changed = copy.with_store(app, |s| {
        // (no copy read yet: the first read brings everything)
        if s.synced_at.is_none() {
            return false;
        }
        let mut found = false;
        let mut changed = false;
        for t in s.tracks.iter_mut().filter(|t| t.track_id == track.track_id) {
            found = true;
            changed |= t.favorite != track.favorite || t.hearted_by != track.hearted_by;
            t.favorite = track.favorite;
            t.hearted_by = track.hearted_by.clone();
        }
        if !found {
            s.tracks.push(track);
            changed = true;
        }
        changed
    });
    if changed {
        if let Some(path) = LibraryCopy::path(app) {
            copy.with_store(app, |s| write_json(&path, s));
        }
    }
}

#[tauri::command]
pub fn wrapped_library_favorite(app: AppHandle, track_id: u64, favorite: bool) {
    let copy = app.state::<LibraryCopy>();
    // (the profile in use: the heart is its own)
    let profile = WrappedSettings::load_saved(&app).history_profile;
    let changed = copy.with_store(&app, |s| {
        let mut changed = false;
        for t in s.tracks.iter_mut().filter(|t| t.track_id == Some(track_id)) {
            changed |= t.favorite != favorite;
            t.favorite = favorite;
            if let Some(id) = &profile {
                t.hearted_by.retain(|by| by != id);
                if favorite {
                    t.hearted_by.push(id.clone());
                }
            }
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
    /// Roon's pictures of the artist to try in turn (avatar, photo); copies
    /// read before 1.5.0 have just `image_key`.
    #[serde(default)]
    pub image_keys: Vec<String>,
    /// The name as Roon sorts it ("Beatles, The"); not in older copies.
    #[serde(default)]
    pub sort_key: Option<String>,
    pub album_count: Option<i32>,
    /// When the artist was born or the band formed, as Roon has it (copies
    /// read before 1.4.1 don't have it).
    #[serde(default)]
    pub begin_year: Option<u32>,
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
    /// Its record labels (copies read before 1.4.1 don't have them).
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LibraryStore {
    synced_at: Option<i64>,
    /// The tracks say which profiles hearted them (`hearted_by`; a copy
    /// read before 1.6 only has the hearts of the profile it was read
    /// for).
    hearts_known: bool,
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
    /// The copy is being read again because what it holds isn't right for
    /// the profile in use (its hearts are another profile's): the pages
    /// show a scan in progress instead of it.
    pub stale: bool,
}

/// See `LibraryStatus::stale`; the pages hear a change as
/// "wrapped-library-stale".
static STALE: AtomicBool = AtomicBool::new(false);

fn set_stale(app: &AppHandle, stale: bool) {
    if STALE.swap(stale, Ordering::SeqCst) != stale {
        let _ = app.emit("wrapped-library-stale", stale);
    }
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

    /// The hearts on the tracks become those of this profile (they are
    /// kept for every profile, `hearted_by`). Returns false for a copy
    /// that doesn't know them per profile yet: it wants reading again.
    fn use_profile(&self, app: &AppHandle, profile: &str) -> bool {
        self.with_store(app, |s| {
            if !s.hearts_known {
                return s.tracks.is_empty();
            }
            for t in &mut s.tracks {
                t.favorite = t.hearted_by.iter().any(|id| id == profile);
            }
            true
        })
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
        LibraryStatus { artists, albums, tracks, synced_at, syncing: self.syncing.load(Ordering::SeqCst), stale: STALE.load(Ordering::SeqCst) }
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
pub fn wrapped_library_artists(app: AppHandle) -> Vec<ArtistRow> {
    let copy = app.state::<LibraryCopy>();
    let artists = copy.artists(&app);
    let labels = main_labels(&copy.albums(&app));
    let tags = app.state::<crate::tags::Tags>();
    artists
        .into_iter()
        .map(|artist| {
            let label = artist
                .performer_id
                .and_then(|id| labels.get(&LabelKey::Performer(id)))
                .or_else(|| labels.get(&LabelKey::Name(artist.name.to_lowercase())))
                .cloned();
            let (began, began_from) = match artist.begin_year {
                Some(year) => (Some(year), "roon"),
                None => match tags.cached(&app, &artist.name).and_then(|t| t.begin_year) {
                    Some(year) => (Some(year), "musicbrainz"),
                    None => (None, ""),
                },
            };
            ArtistRow { artist, label, began, began_from: began_from.to_string() }
        })
        .collect()
}

/// An Artists page row: the artist, its record label (from its newest
/// library album) and when it started.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRow {
    #[serde(flatten)]
    pub artist: LibraryArtist,
    pub label: Option<String>,
    /// Roon's birth or formed year, else MusicBrainz's (from the tagging).
    pub began: Option<u32>,
    /// Where `began` comes from: "roon", "musicbrainz" or "" (not known).
    pub began_from: String,
}

/// Whose albums a label count is for: the main performer when the Core
/// named it, else the album artist's name (lower-cased).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LabelKey {
    Performer(u64),
    Name(String),
}

/// Each artist's record label: the first one on its newest library album
/// that has a label (albums without a year count as oldest; between two
/// albums of the same year, the label first in A to Z order, so the pick
/// doesn't change from one read to the next).
fn main_labels(albums: &[LibraryAlbum]) -> HashMap<LabelKey, String> {
    let mut best: HashMap<LabelKey, (u32, String)> = HashMap::new();
    for album in albums {
        let Some(label) = album.labels.iter().map(|l| l.trim()).find(|l| !l.is_empty()) else {
            continue;
        };
        let key = match album.performer_id {
            Some(id) => LabelKey::Performer(id),
            None => LabelKey::Name(album.artist.to_lowercase()),
        };
        let year = album.year.unwrap_or(0);
        let newer = match best.get(&key) {
            None => true,
            Some((y, l)) => year > *y || (year == *y && label.to_lowercase() < l.to_lowercase()),
        };
        if newer {
            best.insert(key, (year, label.to_string()));
        }
    }
    best.into_iter().map(|(key, (_, label))| (key, label)).collect()
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
    /// How often it was scrobbled to Last.fm, and when last (Unix ms).
    pub plays: u32,
    pub last_played: Option<i64>,
}

/// The library's tracks, as last read (the Tracks page).
#[tauri::command]
pub fn wrapped_library_tracks(app: AppHandle) -> Vec<TrackRow> {
    let tracks = app.state::<LibraryCopy>().tracks(&app);
    // The first scrobble of each song (artist and title, lower-cased)
    let wanted: std::collections::HashSet<(String, String)> = tracks.iter().map(|t| (t.artist.to_lowercase(), t.title.to_lowercase())).collect();
    let mut first: HashMap<(String, String), i64> = HashMap::new();
    // (and how many scrobbles each has, and the latest)
    let mut played: HashMap<(String, String), (u32, i64)> = HashMap::new();
    crate::scrobbles::with(&app, |s| {
        // (newest first: the last one seen is the first scrobble)
        for p in &s.plays {
            let key = (p.artist.to_lowercase(), p.title.to_lowercase());
            if wanted.contains(&key) {
                let seen = played.entry(key.clone()).or_insert((0, p.at));
                seen.0 += 1;
                seen.1 = seen.1.max(p.at);
                first.insert(key, p.at);
            }
        }
    });
    tracks
        .into_iter()
        .map(|mut track| {
            let key = (track.artist.to_lowercase(), track.title.to_lowercase());
            let scrobbled = first.get(&key).copied();
            let (plays, last_played) = played.get(&key).map(|(n, at)| (*n, Some(*at))).unwrap_or((0, None));
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
            TrackRow { track, added_from: added_from.into(), plays, last_played }
        })
        .collect()
}

fn library_artist(c: roon_library::ArtistCard) -> LibraryArtist {
    LibraryArtist {
        name: c.name,
        performer_id: c.performer_id,
        image_key: c.image_key,
        image_keys: c.image_keys,
        sort_key: c.sort_key,
        album_count: c.album_count,
        begin_year: c.begin_year,
    }
}

/// Fresh artists from the Core each time the Library link connects: not
/// all of Roon's picture keys last from one session to the next, and copies
/// from before 1.5.0 lack the second picture and the sort names. Just the
/// artists (a few seconds); albums and tracks wait for a full read. Not
/// while a full read runs, nor before there's a copy at all.
pub fn refresh_artists(app: AppHandle, client: roon_library::LibraryClient) {
    tauri::async_runtime::spawn(async move {
        let copy = app.state::<LibraryCopy>();
        if copy.syncing.load(Ordering::SeqCst) || copy.artists(&app).is_empty() {
            return;
        }
        match client.library_artists(&|_, _| {}).await {
            Ok(cards) if !cards.is_empty() => {
                let artists: Vec<LibraryArtist> = cards.into_iter().map(library_artist).collect();
                let n = artists.len();
                let pictured = artists.iter().filter(|a| a.image_keys.len() > 1).count();
                let sorted = artists.iter().filter(|a| a.sort_key.is_some()).count();
                copy.with_store(&app, |s| s.artists = artists);
                if let Some(path) = LibraryCopy::path(&app) {
                    copy.with_store(&app, |s| write_json(&path, s));
                }
                println!("[wrapped] library copy: artists refreshed ({n}; {pictured} with two pictures, {sorted} with a sort name)");
                let _ = app.emit("wrapped-artists-refreshed", n);
            }
            Ok(_) => {}
            Err(e) => println!("[wrapped] library copy: artists not refreshed ({e})"),
        }
    });
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
                let artists: Vec<LibraryArtist> = cards.into_iter().map(library_artist).collect();
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
                                labels: c.labels,
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
                        s.hearts_known = true;
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
                // (how much of the labels and start years the Core sent)
                let (labelled, dated) = copy.with_store(&app2, |s| {
                    (s.albums.iter().filter(|a| !a.labels.is_empty()).count(), s.artists.iter().filter(|a| a.begin_year.is_some()).count())
                });
                println!("[wrapped] library copy: {labelled} albums with a label, {dated} artists with a start year");
                let (numbered, composed, formatted, ranged, pathed) = copy.with_store(&app2, |s| {
                    let count = |f: &dyn Fn(&LibraryTrack) -> bool| s.tracks.iter().filter(|t| f(t)).count();
                    (
                        count(&|t| t.track_number.is_some()),
                        count(&|t| !t.composer.is_empty()),
                        count(&|t| !t.format.is_empty()),
                        count(&|t| t.dynamic_range.is_some()),
                        count(&|t| !t.path.is_empty()),
                    )
                });
                println!(
                    "[wrapped] library copy: tracks with a number {numbered}, composer {composed}, format {formatted}, dynamic range {ranged}, path {pathed}"
                );
                copy.syncing.store(false, Ordering::SeqCst);
                set_stale(&app2, false);
                let _ = app2.emit("wrapped-library", serde_json::json!({ "read": n, "total": n, "what": "artists", "done": true }));
                // (artists new to the library get their tags)
                crate::tags::fill_missing(&app2);
            }
            Err(e) => {
                copy.syncing.store(false, Ordering::SeqCst);
                set_stale(&app2, false);
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
        // A Discover list being built for another profile than the one in
        // use reads that profile's plays, from its file
        if let Some(path) = crate::discover::acting().and_then(|id| path_of(app, &id)) {
            if self.at.lock().unwrap().as_deref() != Some(path.as_path()) {
                let mut side = self.side.lock().unwrap();
                if !side.as_ref().is_some_and(|(at, _)| *at == path) {
                    *side = Some((path.clone(), read_store(&path)));
                }
                return f(&side.as_ref().unwrap().1.plays);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn album(artist: &str, performer_id: Option<u64>, year: Option<u32>, labels: &[&str]) -> LibraryAlbum {
        LibraryAlbum {
            title: String::new(),
            artist: artist.into(),
            album_id: None,
            performer_id,
            image_key: None,
            year,
            source: "local".into(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    #[test]
    fn label_comes_from_the_newest_album() {
        let albums = vec![
            album("Amon Amarth", Some(1), Some(2001), &["Metal Blade"]),
            album("Amon Amarth", Some(1), Some(2022), &["Metal Blade", "Sony"]),
            album("Amon Amarth", Some(1), Some(2013), &["Sony"]),
            // (the newest album has no label: the next newest one decides)
            album("Amon Amarth", Some(1), Some(2025), &[]),
            album("Burial", None, Some(2007), &[" ", "Hyperdub"]),
            album("Burial", None, None, &["Undated"]),
            album("Nobody", Some(3), Some(2020), &[]),
        ];
        let labels = main_labels(&albums);
        assert_eq!(labels.get(&LabelKey::Performer(1)).map(String::as_str), Some("Metal Blade"));
        assert_eq!(labels.get(&LabelKey::Name("burial".into())).map(String::as_str), Some("Hyperdub"));
        assert_eq!(labels.get(&LabelKey::Performer(3)), None);
    }

    #[test]
    fn same_year_labels_go_a_to_z() {
        let albums = vec![album("X", Some(7), Some(2010), &["Warp"]), album("X", Some(7), Some(2010), &["Ninja Tune"])];
        assert_eq!(main_labels(&albums).get(&LabelKey::Performer(7)).map(String::as_str), Some("Ninja Tune"));
    }

    #[test]
    fn old_library_copies_still_load() {
        let artist: LibraryArtist = serde_json::from_str(r#"{"name":"Air","performerId":5,"imageKey":null,"albumCount":3}"#).unwrap();
        assert_eq!(artist.begin_year, None);
        let album: LibraryAlbum = serde_json::from_str(r#"{"title":"Moon Safari","artist":"Air","albumId":1,"performerId":5,"imageKey":null,"year":1998,"source":"local"}"#).unwrap();
        assert!(album.labels.is_empty());
    }
}
