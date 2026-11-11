// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Your Last.fm scrobbles: the listening from before Roon's history
//! starts (Roon's goes back only as far as the Core's library does; a
//! Last.fm account may hold years more). A Roon profile's username is the
//! one it's signed in with in Roon, or one typed on its Profile page; a
//! profile with neither has no scrobbles. Every scrobble of a username is
//! read once (`user.getRecentTracks`, 200 a page, with the user's own API
//! key) into `wrapped-scrobbles-<username>.json`, then only the new ones,
//! daily: two profiles on the same account share the one read. What the
//! pages count are the scrobbles of the profile in use. Home's favorites (discover.rs
//! `home_favorites`) count the scrobbles from before Roon's first play;
//! later ones Roon already has.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::wrapped::{now_ms, read_json, write_json, WrappedSettings};

const USER_AGENT: &str = concat!("RoonToasted/", env!("CARGO_PKG_VERSION"), " (https://github.com/Vulkandr/roon-toasted)");
/// Scrobbles a page (Last.fm's most).
const PAGE: u32 = 200;
/// Between pages (Last.fm asks for no more than about 5 calls a second).
const PAUSE: Duration = Duration::from_millis(300);
/// New scrobbles are looked for after this long.
const SYNC_EVERY: i64 = 24 * 3_600_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Scrobble {
    /// Unix milliseconds.
    pub at: i64,
    pub artist: String,
    pub title: String,
    pub album: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Scrobbles {
    /// Whose they are (a change starts over).
    pub user: String,
    /// Newest first.
    pub plays: Vec<Scrobble>,
    /// The whole history has been read (else `plays` is the newest part
    /// so far, and the next read goes on from the oldest).
    pub complete: bool,
    pub synced_at: i64,
    pub error: String,
}

/// What the page shows ("wrapped-scrobbles" events).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrobblesStatus {
    pub user: String,
    pub plays: usize,
    pub complete: bool,
    pub synced_at: i64,
    pub syncing: bool,
    pub error: String,
    /// The oldest scrobble read, Unix milliseconds (0 for none).
    pub oldest: i64,
}

static SYNCING: AtomicBool = AtomicBool::new(false);
/// Whose scrobbles the running read is on.
static READING: Mutex<String> = Mutex::new(String::new());
/// The scrobbles of the profile in use (an empty set when it has no
/// username), with the username they were read for.
static CACHE: Mutex<Option<(String, Scrobbles)>> = Mutex::new(None);
/// The username of the profile in use, once asked for (`user_changed`
/// asks again).
static USER: Mutex<Option<String>> = Mutex::new(None);

/// The one file of 1.5 and earlier (it names whose it is): it becomes that
/// username's own the first time it's wanted.
fn old_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("wrapped-scrobbles.json"))
}

/// A username's file (Last.fm names are letters, digits, "-" and "_", in
/// any case).
fn path(app: &AppHandle, user: &str) -> Option<PathBuf> {
    let name: String = user.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    app.path().app_data_dir().ok().map(|d| d.join(format!("wrapped-scrobbles-{name}.json")))
}

/// The username of the profile in use (empty for none).
fn current_user(app: &AppHandle) -> String {
    if let Some(user) = USER.lock().unwrap().clone() {
        return user;
    }
    let user = WrappedSettings::load(app).lastfm_user.trim().to_string();
    *USER.lock().unwrap() = Some(user.clone());
    user
}

/// The profile in use, or its username, may have changed: the scrobbles
/// to go by are asked for again, the pages hear, and a username not read
/// yet starts its read.
pub fn user_changed(app: &AppHandle) {
    let user = WrappedSettings::load(app).lastfm_user.trim().to_string();
    let was = USER.lock().unwrap().replace(user.clone());
    if was.as_deref() == Some(user.as_str()) {
        return;
    }
    emit(app);
    if !user.is_empty() && with(app, |s| !s.complete) {
        start(app, vec![user]);
    }
}

/// A username's scrobbles from disk (none kept: an empty set of its own).
fn read(app: &AppHandle, user: &str) -> Scrobbles {
    let empty = || Scrobbles { user: user.to_string(), ..Default::default() };
    if user.is_empty() {
        return empty();
    }
    let Some(own) = path(app, user) else { return empty() };
    if !own.exists() {
        // (the one file from before, if it is this username's)
        if let Some(old) = old_path(app).filter(|p| p.exists()) {
            let whose: Option<Scrobbles> = read_json(&old);
            if whose.as_ref().is_some_and(|s| s.user.eq_ignore_ascii_case(user)) {
                let _ = std::fs::rename(&old, &own);
                return whose.unwrap();
            }
        }
        return empty();
    }
    read_json(&own).unwrap_or_else(empty)
}

/// Runs `f` over the scrobbles of the profile in use (read from disk the
/// first time; a quarter million of them aren't cloned around).
pub fn with<T>(app: &AppHandle, f: impl FnOnce(&Scrobbles) -> T) -> T {
    let user = current_user(app);
    let mut cache = CACHE.lock().unwrap();
    if !cache.as_ref().is_some_and(|(whose, _)| whose.eq_ignore_ascii_case(&user)) {
        *cache = Some((user.clone(), read(app, &user)));
    }
    f(&cache.as_ref().unwrap().1)
}

/// A username's scrobbles: the ones in memory if they are the ones in
/// use, else from its file.
fn load(app: &AppHandle, user: &str) -> Scrobbles {
    if let Some((whose, s)) = CACHE.lock().unwrap().as_ref() {
        if whose.eq_ignore_ascii_case(user) {
            return s.clone();
        }
    }
    read(app, user)
}

fn save(app: &AppHandle, s: &Scrobbles) {
    {
        let mut cache = CACHE.lock().unwrap();
        if cache.as_ref().is_some_and(|(whose, _)| whose.eq_ignore_ascii_case(&s.user)) {
            *cache = Some((s.user.clone(), s.clone()));
        }
    }
    if let Some(p) = path(app, &s.user).filter(|_| !s.user.is_empty()) {
        write_json(&p, s);
    }
}

pub fn status(app: &AppHandle) -> ScrobblesStatus {
    with(app, |s| ScrobblesStatus {
        user: s.user.clone(),
        plays: s.plays.len(),
        complete: s.complete,
        synced_at: s.synced_at,
        // (a read of another profile's username isn't this one's)
        syncing: SYNCING.load(Ordering::SeqCst) && READING.lock().unwrap().eq_ignore_ascii_case(&s.user),
        error: s.error.clone(),
        oldest: s.plays.last().map(|p| p.at).unwrap_or(0),
    })
}

/// Listens per artist (lower-cased name -> (name, count)) from the
/// scrobbles before `before` (Unix ms): what Roon's history doesn't hold.
pub fn listens_before(app: &AppHandle, before: i64) -> HashMap<String, (String, u32)> {
    with(app, |s| {
        let mut out: HashMap<String, (String, u32)> = HashMap::new();
        for p in s.plays.iter().filter(|p| p.at < before && !p.artist.is_empty()) {
            let row = out.entry(p.artist.to_lowercase()).or_insert_with(|| (p.artist.clone(), 0));
            row.1 += 1;
        }
        out
    })
}

/// How many scrobbles are from before `before` (Unix ms).
pub fn count_before(app: &AppHandle, before: i64) -> usize {
    with(app, |s| s.plays.iter().filter(|p| p.at < before).count())
}

#[tauri::command]
pub fn wrapped_scrobbles_status(app: AppHandle) -> ScrobblesStatus {
    status(&app)
}

/// Reads the scrobbles of the profile in use now (the rest of the
/// history, or the new ones); `again` starts over from nothing.
#[tauri::command]
pub fn wrapped_scrobbles_sync(app: AppHandle, again: Option<bool>) -> ScrobblesStatus {
    let user = current_user(&app);
    if user.is_empty() {
        return status(&app);
    }
    if again.unwrap_or(false) && !SYNCING.load(Ordering::SeqCst) {
        save(&app, &Scrobbles { user: user.clone(), ..Default::default() });
    }
    start(&app, vec![user]);
    status(&app)
}

/// Whether a username's scrobbles want reading: never read through, or
/// not looked at for a day. (For a username not in use, by its file's
/// age: a read that stopped halfway goes on within the day, or as soon as
/// a profile with that username is in use.)
fn due(app: &AppHandle, user: &str) -> bool {
    if user.eq_ignore_ascii_case(&current_user(app)) {
        return with(app, |s| !s.complete || now_ms() - s.synced_at > SYNC_EVERY);
    }
    let Some(file) = path(app, user) else { return false };
    let age = std::fs::metadata(&file).and_then(|m| m.modified()).ok().and_then(|at| at.elapsed().ok());
    age.is_none_or(|age| age.as_millis() as i64 > SYNC_EVERY)
}

/// From Toasted's start: each username's history read once, new scrobbles
/// daily.
pub fn schedule(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            if !WrappedSettings::load(&app).lastfm_key.is_empty() {
                let users: Vec<String> = WrappedSettings::lastfm_all(&app).into_iter().filter(|user| due(&app, user)).collect();
                if !users.is_empty() {
                    start(&app, users);
                }
            }
            tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        }
    });
}

fn emit(app: &AppHandle) {
    let _ = app.emit("wrapped-scrobbles", status(app));
}

/// A rescan (wrapped.rs): these usernames' new scrobbles now, whenever
/// they were last looked at. Nothing without the Last.fm key.
pub fn rescan(app: &AppHandle, users: Vec<String>) {
    if !users.is_empty() && !WrappedSettings::load(app).lastfm_key.is_empty() {
        start(app, users);
    }
}

/// Reads these usernames' scrobbles in the background, one after the
/// other (nothing while a read is running already).
fn start(app: &AppHandle, users: Vec<String>) {
    if SYNCING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for user in users {
            *READING.lock().unwrap() = user.clone();
            emit(&app);
            if let Err(e) = sync(&app, &user).await {
                println!("[scrobbles] {user}: {e}");
                let mut s = load(&app, &user);
                s.error = e;
                save(&app, &s);
            }
        }
        READING.lock().unwrap().clear();
        SYNCING.store(false, Ordering::SeqCst);
        emit(&app);
    });
}

async fn sync(app: &AppHandle, user: &str) -> Result<(), String> {
    let settings = WrappedSettings::load(app);
    let user = user.trim().to_string();
    if user.is_empty() {
        return Err("No Last.fm username for this profile.".into());
    }
    if settings.lastfm_key.is_empty() {
        return Err("Reading scrobbles needs the Last.fm API key (Wrapped's Settings).".into());
    }
    let mut s = load(app, &user);
    s.user = user.clone();
    s.error.clear();
    let client = reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    // New ones since the newest kept, newest first: pages from the top
    // until one's all known
    let newest = s.plays.first().map(|p| p.at / 1000).unwrap_or(0);
    if newest > 0 {
        let mut fresh: Vec<Scrobble> = Vec::new();
        let mut page = 1;
        loop {
            let (list, pages, _) = fetch(&client, &user, &settings.lastfm_key, page, Some(newest + 1), None).await?;
            let n = list.len();
            fresh.extend(list);
            if n < PAGE as usize || page >= pages {
                break;
            }
            page += 1;
            tokio::time::sleep(PAUSE).await;
        }
        if !fresh.is_empty() {
            println!("[scrobbles] {} new since the last read", fresh.len());
            fresh.extend(std::mem::take(&mut s.plays));
            s.plays = fresh;
        }
    }
    // The rest of the history, from the oldest kept back to the start: a
    // page at a time, each from before the oldest so far (page 1 again
    // with a new `to`), until a page comes back empty or the count Last.fm
    // gives for the whole history is reached
    if !s.complete {
        let (_, _, total) = fetch(&client, &user, &settings.lastfm_key, 1, None, None).await?;
        let mut page = 0;
        loop {
            let to = s.plays.last().map(|p| p.at / 1000 - 1);
            let (list, _, _) = fetch(&client, &user, &settings.lastfm_key, 1, None, to).await?;
            let n = list.len();
            s.plays.extend(list);
            page += 1;
            if n == 0 || (total > 0 && s.plays.len() >= total) {
                s.complete = true;
                break;
            }
            if page % 25 == 0 {
                s.synced_at = now_ms();
                save(app, &s);
                emit(app);
            }
            tokio::time::sleep(PAUSE).await;
        }
    }
    s.synced_at = now_ms();
    save(app, &s);
    println!("[scrobbles] {} scrobbles of {user} kept ({}complete) in {} s", s.plays.len(), if s.complete { "" } else { "not " }, started.elapsed().as_secs());
    Ok(())
}

/// `fetch_once`, asked again when Last.fm stumbles (its "Operation
/// failed, please try again", a busy service, a dropped connection): up
/// to four more times, waiting longer each time, so one bad answer in a
/// thousand pages doesn't end the read. What Last.fm means (no such user,
/// a bad key) is given back at once.
async fn fetch(client: &reqwest::Client, user: &str, key: &str, page: u32, from: Option<i64>, to: Option<i64>) -> Result<(Vec<Scrobble>, u32, usize), String> {
    const WAITS: [u64; 4] = [2, 5, 15, 30];
    let mut tries = 0;
    loop {
        match fetch_once(client, user, key, page, from, to).await {
            Ok(page) => return Ok(page),
            Err((e, again)) if again && tries < WAITS.len() => {
                println!("[scrobbles] {e} (asking again in {} s)", WAITS[tries]);
                tokio::time::sleep(Duration::from_secs(WAITS[tries])).await;
                tries += 1;
            }
            Err((e, _)) => return Err(e),
        }
    }
}

/// One page of `user.getRecentTracks` (newest first), how many pages
/// there are and how many scrobbles in all (in the range asked); a track
/// playing right now is left out. An error comes with whether asking
/// again may help.
async fn fetch_once(client: &reqwest::Client, user: &str, key: &str, page: u32, from: Option<i64>, to: Option<i64>) -> Result<(Vec<Scrobble>, u32, usize), (String, bool)> {
    let page_s = page.to_string();
    let limit = PAGE.to_string();
    let mut query: Vec<(&str, String)> = vec![
        ("method", "user.getrecenttracks".into()),
        ("user", user.into()),
        ("api_key", key.into()),
        ("format", "json".into()),
        ("limit", limit),
        ("page", page_s),
    ];
    if let Some(from) = from {
        query.push(("from", from.to_string()));
    }
    if let Some(to) = to {
        query.push(("to", to.to_string()));
    }
    let v: serde_json::Value = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&query)
        .send()
        .await
        .map_err(|e| (format!("Last.fm couldn't be reached: {e}"), true))?
        .json()
        .await
        .map_err(|e| (format!("Last.fm's answer couldn't be read: {e}"), true))?;
    if let Some(message) = v["message"].as_str() {
        // (Last.fm's own codes: 8 operation failed, 11 service offline,
        // 16 temporarily unavailable, 29 too many requests)
        let again = matches!(v["error"].as_i64(), Some(8 | 11 | 16 | 29));
        return Err((format!("Last.fm said: {message}"), again));
    }
    let pages = v["recenttracks"]["@attr"]["totalPages"].as_str().and_then(|p| p.parse().ok()).unwrap_or(1);
    let total = v["recenttracks"]["@attr"]["total"].as_str().and_then(|p| p.parse().ok()).unwrap_or(0);
    let tracks = v["recenttracks"]["track"].as_array().cloned().unwrap_or_default();
    let list = tracks
        .iter()
        .filter(|t| t["@attr"]["nowplaying"].as_str() != Some("true"))
        .filter_map(|t| {
            let at: i64 = t["date"]["uts"].as_str()?.parse().ok()?;
            Some(Scrobble {
                at: at * 1000,
                artist: t["artist"]["#text"].as_str().unwrap_or("").to_string(),
                title: t["name"].as_str().unwrap_or("").to_string(),
                album: t["album"]["#text"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();
    Ok((list, pages, total))
}
