// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Your Last.fm scrobbles: the listening from before Roon's history
//! starts (Roon's goes back only as far as the Core's library does; a
//! Last.fm account may hold years more). With a username in Wrapped's
//! Settings, every scrobble is read once (`user.getRecentTracks`, 200 a
//! page, with the user's own API key) into `wrapped-scrobbles.json`, then
//! only the new ones, daily. Home's favorites (discover.rs
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
static CACHE: Mutex<Option<Scrobbles>> = Mutex::new(None);

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("wrapped-scrobbles.json"))
}

/// Runs `f` over the scrobbles (read from disk the first time; a quarter
/// million of them aren't cloned around).
pub fn with<T>(app: &AppHandle, f: impl FnOnce(&Scrobbles) -> T) -> T {
    let mut cache = CACHE.lock().unwrap();
    let s = cache.get_or_insert_with(|| path(app).and_then(|p| read_json(&p)).unwrap_or_default());
    f(s)
}

fn load(app: &AppHandle) -> Scrobbles {
    with(app, |s| s.clone())
}

fn save(app: &AppHandle, s: &Scrobbles) {
    *CACHE.lock().unwrap() = Some(s.clone());
    if let Some(p) = path(app) {
        write_json(&p, s);
    }
}

pub fn status(app: &AppHandle) -> ScrobblesStatus {
    with(app, |s| ScrobblesStatus {
        user: s.user.clone(),
        plays: s.plays.len(),
        complete: s.complete,
        synced_at: s.synced_at,
        syncing: SYNCING.load(Ordering::SeqCst),
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

/// Reads the scrobbles now (the rest of the history, or the new ones);
/// `again` starts over from nothing.
#[tauri::command]
pub fn wrapped_scrobbles_sync(app: AppHandle, again: Option<bool>) -> ScrobblesStatus {
    if again.unwrap_or(false) && !SYNCING.load(Ordering::SeqCst) {
        let user = WrappedSettings::load(&app).lastfm_user.trim().to_string();
        save(&app, &Scrobbles { user, ..Default::default() });
    }
    start(&app);
    status(&app)
}

/// From Toasted's start: the history read once, new scrobbles daily.
pub fn schedule(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let settings = WrappedSettings::load(&app);
            if !settings.lastfm_user.trim().is_empty() && !settings.lastfm_key.is_empty() {
                let due = with(&app, |s| !s.complete || s.user != settings.lastfm_user.trim() || now_ms() - s.synced_at > SYNC_EVERY);
                if due {
                    start(&app);
                }
            }
            tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        }
    });
}

fn emit(app: &AppHandle) {
    let _ = app.emit("wrapped-scrobbles", status(app));
}

fn start(app: &AppHandle) {
    if SYNCING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = sync(&app).await;
        if let Err(e) = result {
            println!("[scrobbles] {e}");
            let mut s = load(&app);
            s.error = e;
            save(&app, &s);
        }
        SYNCING.store(false, Ordering::SeqCst);
        emit(&app);
    });
}

async fn sync(app: &AppHandle) -> Result<(), String> {
    let settings = WrappedSettings::load(app);
    let user = settings.lastfm_user.trim().to_string();
    if user.is_empty() {
        return Err("No Last.fm username in Wrapped's Settings.".into());
    }
    if settings.lastfm_key.is_empty() {
        return Err("Reading scrobbles needs the Last.fm API key (Wrapped's Settings).".into());
    }
    let mut s = load(app);
    if s.user != user {
        s = Scrobbles { user: user.clone(), ..Default::default() };
    }
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

/// One page of `user.getRecentTracks` (newest first), how many pages
/// there are and how many scrobbles in all (in the range asked); a track
/// playing right now is left out.
async fn fetch(client: &reqwest::Client, user: &str, key: &str, page: u32, from: Option<i64>, to: Option<i64>) -> Result<(Vec<Scrobble>, u32, usize), String> {
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
        .map_err(|e| format!("Last.fm couldn't be reached: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Last.fm's answer couldn't be read: {e}"))?;
    if let Some(message) = v["message"].as_str() {
        return Err(format!("Last.fm said: {message}"));
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
