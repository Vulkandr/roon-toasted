// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Wrapped's Home page: artists for you (new to you, sounding like your
//! favorites over all time, by tags: tier 1 without the songs), new albums
//! from the artists you listen to, and new albums from those suggested
//! artists. Built in the background when Toasted starts and kept in
//! `wrapped-home.json`: the artists again every ARTISTS_EVERY (listening
//! habits don't shift much in a fortnight), the albums looked for daily,
//! new finds added at the front (`schedule`); Refresh builds it all.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use roon_library::LibraryClient;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::discover::{fold, TasteArtist};
use crate::library::Library;
use crate::wrapped::{now_ms, read_json, write_json, WrappedSettings};

/// The artists are looked for again after this long.
const ARTISTS_EVERY: i64 = 14 * 86_400_000;
/// New albums are looked for again after this long.
const ALBUMS_EVERY: i64 = 24 * 3_600_000;
/// How often the schedule looks at the clock.
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
/// Artists suggested on the page.
const ARTISTS: usize = 48;
/// Albums kept in each "new" row.
const ALBUMS: usize = 60;
/// How far back a release counts as new.
const NEW_DAYS: i64 = 365;
/// Your most played artists looked at for new albums.
const FAVORITES: usize = 80;
/// Roon lookups at once.
const AT_ONCE: usize = 4;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HomeArtist {
    pub name: String,
    /// Roon's performer id (durable; the page opens the artist by it).
    pub performer_id: Option<u64>,
    pub image_key: Option<String>,
    pub because: String,
    /// The artists you play that it came from.
    pub seeds: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HomeAlbum {
    pub title: String,
    pub artist: String,
    /// Roon's album id (durable).
    pub album_id: Option<u64>,
    pub performer_id: Option<u64>,
    pub image_key: Option<String>,
    /// "2026-09-12"
    pub released: String,
    pub in_library: bool,
    /// Why the artist is here ("you played them 40 times this year", or
    /// the suggestion's because line).
    pub why: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HomeData {
    pub artists: Vec<HomeArtist>,
    pub new_from_favorites: Vec<HomeAlbum>,
    pub new_from_suggested: Vec<HomeAlbum>,
    /// When the artists were last looked for (the whole build).
    pub built_at: i64,
    /// When new albums were last looked for.
    pub albums_at: i64,
    /// What stopped the last build, if anything.
    pub error: String,
}

/// What `wrapped_home` returns and the "wrapped-home" event carries.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeStatus {
    #[serde(flatten)]
    pub data: HomeData,
    pub building: bool,
    pub progress: String,
}

static BUILDING: AtomicBool = AtomicBool::new(false);
static PROGRESS: Mutex<String> = Mutex::new(String::new());

fn path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("wrapped-home.json"))
}

fn load(app: &AppHandle) -> HomeData {
    path(app).and_then(|p| read_json(&p)).unwrap_or_default()
}

fn status(app: &AppHandle) -> HomeStatus {
    HomeStatus { data: load(app), building: BUILDING.load(Ordering::SeqCst), progress: PROGRESS.lock().unwrap().clone() }
}

fn emit(app: &AppHandle) {
    let _ = app.emit("wrapped-home", status(app));
}

/// The Home page's data (kept from the last build); `refresh` starts a
/// whole build in the background (else `schedule` keeps it current).
#[tauri::command]
pub fn wrapped_home(app: AppHandle, refresh: Option<bool>) -> HomeStatus {
    if refresh.unwrap_or(false) {
        start(&app, true);
    } else {
        consider(&app);
    }
    status(&app)
}

/// Keeps the page current from Toasted's start: a look at the clock every
/// CHECK_EVERY (and once the library link is up).
pub fn schedule(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            consider(&app);
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// Starts what's due: the whole build when the artists are ARTISTS_EVERY
/// old (or there are none), else the albums when they're ALBUMS_EVERY old.
fn consider(app: &AppHandle) {
    if app.state::<Library>().client().is_none() {
        return;
    }
    let data = load(app);
    let now = now_ms();
    if data.built_at == 0 || now - data.built_at > ARTISTS_EVERY {
        start(app, true);
    } else if now - data.albums_at > ALBUMS_EVERY {
        start(app, false);
    }
}

fn start(app: &AppHandle, whole: bool) {
    if BUILDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let before = load(&app);
        let result = if whole { build(&app).await } else { albums_again(&app, before.clone()).await };
        let data = match result {
            Ok(data) => data,
            Err(e) => {
                println!("[home] not built: {e}");
                HomeData { error: e, ..before }
            }
        };
        if let Some(p) = path(&app) {
            write_json(&p, &data);
        }
        BUILDING.store(false, Ordering::SeqCst);
        PROGRESS.lock().unwrap().clear();
        emit(&app);
    });
}

fn progress(app: &AppHandle, text: String) {
    *PROGRESS.lock().unwrap() = text;
    emit(app);
}

/// The whole build: your favorites, the suggested artists, and every
/// new album from both.
async fn build(app: &AppHandle) -> Result<HomeData, String> {
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    let key = WrappedSettings::load(app).lastfm_key;
    let started = std::time::Instant::now();
    let tick = |text: String| progress(app, text);
    let (favorites, suggested) = crate::discover::home_taste(app, &client, &key, ARTISTS + 10, &tick).await?;
    let by_name = library_by_name(app, &client).await;
    let like = your_genres(&client, &by_name, &favorites).await;
    let new_from_favorites = favorite_albums(app, &client, &by_name, favorites).await;
    // The suggested artists, found in Roon, and their new albums
    progress(app, "Finding the suggested artists in Roon...".into());
    let mut artists: Vec<HomeArtist> = Vec::new();
    let mut suggested_albums: Vec<(f64, HomeAlbum)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for chunk in suggested.chunks(AT_ONCE) {
        let mut set = tokio::task::JoinSet::new();
        for (i, t) in chunk.iter().cloned().enumerate() {
            let (client, like) = (client.clone(), like.clone());
            set.spawn(async move {
                let card = find_artist(&client, &t.name, &like).await?;
                let albums = new_albums(&client, &card, &t.name, 1.0).await;
                Some((i, t, card, albums))
            });
        }
        let mut got: Vec<(usize, TasteArtist, roon_library::ArtistCard, Vec<(f64, HomeAlbum)>)> = Vec::new();
        while let Some(found) = set.join_next().await {
            if let Ok(Some(row)) = found {
                got.push(row);
            }
        }
        got.sort_by_key(|r| r.0);
        for (_, t, card, albums) in got {
            if !seen.insert(fold(&card.name)) || artists.len() >= ARTISTS {
                continue;
            }
            artists.push(HomeArtist { name: card.name.clone(), performer_id: card.performer_id, image_key: card.image_key.clone(), because: t.because.clone(), seeds: t.seeds.clone() });
            suggested_albums.extend(albums.into_iter().map(|(w, mut a)| {
                a.why = t.because.clone();
                (w, a)
            }));
        }
    }
    let new_from_suggested = newest(suggested_albums, &since(), |a, _| a.why.clone());
    println!(
        "[home] {} artists, {} new albums from your artists, {} from the suggested, in {} s",
        artists.len(),
        new_from_favorites.len(),
        new_from_suggested.len(),
        started.elapsed().as_secs()
    );
    let now = now_ms();
    Ok(HomeData { artists, new_from_favorites, new_from_suggested, built_at: now, albums_at: now, error: String::new() })
}

/// The daily look for new albums: your favorites' and the suggested
/// artists' discographies again; albums not on the page yet go in at the
/// front, the rest keep their place, ones past NEW_DAYS drop off.
async fn albums_again(app: &AppHandle, mut data: HomeData) -> Result<HomeData, String> {
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    let started = std::time::Instant::now();
    let (favorites, _) = crate::discover::home_favorites(app, &client).await;
    let by_name = library_by_name(app, &client).await;
    let like = your_genres(&client, &by_name, &favorites).await;
    let fresh = favorite_albums(app, &client, &by_name, favorites).await;
    let (added_favorites, new_from_favorites) = merge(data.new_from_favorites, fresh);
    progress(app, "Looking for new albums from the suggested artists...".into());
    let mut rows: Vec<(f64, HomeAlbum)> = Vec::new();
    for chunk in data.artists.chunks(AT_ONCE) {
        let mut set = tokio::task::JoinSet::new();
        for a in chunk.iter().cloned() {
            let (client, like) = (client.clone(), like.clone());
            set.spawn(async move {
                let card = find_artist(&client, &a.name, &like).await?;
                let mut albums = new_albums(&client, &card, &a.name, 1.0).await;
                for (_, album) in albums.iter_mut() {
                    album.why = a.because.clone();
                }
                Some(albums)
            });
        }
        while let Some(found) = set.join_next().await {
            if let Ok(Some(list)) = found {
                rows.extend(list);
            }
        }
    }
    let (added_suggested, new_from_suggested) = merge(data.new_from_suggested, newest(rows, &since(), |a, _| a.why.clone()));
    println!("[home] new albums looked for: {added_favorites} new from your artists, {added_suggested} from the suggested, in {} s", started.elapsed().as_secs());
    data.new_from_favorites = new_from_favorites;
    data.new_from_suggested = new_from_suggested;
    data.albums_at = now_ms();
    data.error.clear();
    Ok(data)
}

/// `fresh` albums not among `kept` go in at the front (newest first among
/// themselves); the kept ones follow in their order, the ones past
/// NEW_DAYS dropped. How many came in, and the row.
fn merge(kept: Vec<HomeAlbum>, fresh: Vec<HomeAlbum>) -> (usize, Vec<HomeAlbum>) {
    let since = since();
    let known: HashSet<String> = kept.iter().map(album_key).collect();
    let added: Vec<HomeAlbum> = fresh.into_iter().filter(|a| !known.contains(&album_key(a))).collect();
    let n = added.len();
    let mut out = added;
    out.extend(kept.into_iter().filter(|a| a.released.as_str() >= since.as_str()));
    out.truncate(ALBUMS);
    (n, out)
}

fn album_key(a: &HomeAlbum) -> String {
    match a.album_id {
        Some(id) => format!("id:{id}"),
        None => format!("{}|{}", fold(&a.artist), fold(&a.title)),
    }
}

/// The library's artists by folded name.
async fn library_by_name(app: &AppHandle, client: &LibraryClient) -> HashMap<String, roon_library::ArtistCard> {
    progress(app, "Reading your library...".into());
    let no_progress = |_: usize, _: usize| {};
    let cards = client.library_artists(&no_progress).await.unwrap_or_default();
    cards.into_iter().map(|c| (fold(&c.name), c)).collect()
}

/// New albums from your FAVORITES most played artists, newest first.
async fn favorite_albums(app: &AppHandle, client: &LibraryClient, by_name: &HashMap<String, roon_library::ArtistCard>, favorites: Vec<(String, f64)>) -> Vec<HomeAlbum> {
    progress(app, "Looking for new albums from the artists you play...".into());
    let mut rows: Vec<(f64, HomeAlbum)> = Vec::new();
    let top: Vec<(String, f64)> = favorites.into_iter().take(FAVORITES).collect();
    let total = top.len();
    let mut done = 0;
    for chunk in top.chunks(AT_ONCE) {
        let mut set = tokio::task::JoinSet::new();
        for (name, weight) in chunk.iter().cloned() {
            let (client, card) = (client.clone(), by_name.get(&fold(&name)).cloned());
            set.spawn(async move {
                let card = match card {
                    Some(c) => Some(c),
                    None => find_artist(&client, &name, &HashSet::new()).await,
                };
                let Some(card) = card else { return Vec::new() };
                new_albums(&client, &card, &name, weight).await
            });
        }
        while let Some(found) = set.join_next().await {
            if let Ok(list) = found {
                rows.extend(list);
            }
        }
        done += chunk.len();
        progress(app, format!("Looking for new albums from the artists you play... {done} of {total}"));
    }
    newest(rows, &since(), |a, w| format!("You listen to {} ({})", a.artist, listens_text(w)))
}

/// The day NEW_DAYS ago, as "YYYY-MM-DD".
fn since() -> String {
    date_string(now_ms() - NEW_DAYS * 86_400_000)
}

/// An artist banned: off the page, its albums too.
pub fn banned(app: &AppHandle, name: &str) {
    let k = fold(name);
    let mut data = load(app);
    data.artists.retain(|a| fold(&a.name) != k);
    data.new_from_favorites.retain(|a| fold(&a.artist) != k);
    data.new_from_suggested.retain(|a| fold(&a.artist) != k);
    if let Some(p) = path(app) {
        write_json(&p, &data);
    }
    emit(app);
}

/// The Roon genres of your most played library artists: what a same-name
/// artist is told apart by (the metal Master, not the Russian one).
async fn your_genres(client: &LibraryClient, by_name: &HashMap<String, roon_library::ArtistCard>, favorites: &[(String, f64)]) -> HashSet<String> {
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut out: HashSet<String> = HashSet::new();
    for (name, _) in favorites.iter().take(12) {
        if let Some(card) = by_name.get(&fold(name)) {
            out.extend(crate::discover::genres_of(client, &mut cache, &card.handle).await);
        }
    }
    out
}

/// An artist in Roon by exact name (folded); with several of that name,
/// the one sharing a genre with `like`, else one Roon has no genres for.
async fn find_artist(client: &LibraryClient, name: &str, like: &HashSet<String>) -> Option<roon_library::ArtistCard> {
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let handle = crate::discover::find_by_name(client, &mut cache, name, like).await?;
    client.artist_card(&handle)
}

/// The artist's own albums with a release date (not appearances on
/// others', not collaborations or various-artists releases), each with
/// the weight given: its main albums, and its singles, EPs and live
/// albums only when it has no main album from the last year (a new
/// album's singles would otherwise crowd the row).
async fn new_albums(client: &LibraryClient, card: &roon_library::ArtistCard, artist: &str, weight: f64) -> Vec<(f64, HomeAlbum)> {
    let Ok(albums) = client.artist_albums_for(&card.handle, None).await else { return Vec::new() };
    let mine = fold(artist);
    let since = since();
    let own: Vec<roon_library::ArtistAlbum> = albums
        .into_iter()
        .filter(|a| a.group != "appearance")
        .filter(|a| {
            let by = fold(&a.artist);
            by.is_empty() || by == mine
        })
        .collect();
    let has_main = own.iter().any(|a| a.group == "main" && a.released.as_deref().is_some_and(|d| d >= since.as_str()));
    own.into_iter()
        .filter(|a| a.group == "main" || !has_main)
        .filter_map(|a| {
            let released = a.released.clone()?;
            Some((
                weight,
                HomeAlbum {
                    title: a.title,
                    artist: if a.artist.is_empty() { artist.to_string() } else { a.artist },
                    album_id: a.album_id,
                    performer_id: card.performer_id,
                    image_key: a.image_key,
                    released,
                    in_library: a.in_library,
                    why: String::new(),
                },
            ))
        })
        .collect()
}

/// The albums released since `since`, newest first, then by weight; one
/// entry per album, ALBUMS at most, each with its why line.
fn newest(mut rows: Vec<(f64, HomeAlbum)>, since: &str, why: impl Fn(&HomeAlbum, f64) -> String) -> Vec<HomeAlbum> {
    let today = date_string(now_ms() + 86_400_000);
    rows.retain(|(_, a)| a.released.as_str() >= since && a.released.as_str() <= today.as_str());
    rows.sort_by(|a, b| b.1.released.cmp(&a.1.released).then(b.0.total_cmp(&a.0)));
    // (one entry per album, and one edition of it: "Sad Wings of Destiny
    // (50th Anniversary)" and "(Original 1976 Mix)" are one)
    let mut seen: HashSet<String> = HashSet::new();
    let mut titles: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (w, mut a) in rows {
        if !seen.insert(album_key(&a)) || !titles.insert(format!("{}|{}", fold(&a.artist), crate::sound::base_title(&a.title))) {
            continue;
        }
        a.why = why(&a, w);
        out.push(a);
        if out.len() >= ALBUMS {
            break;
        }
    }
    out
}

// ----- Rows from your own listening: forgotten favorites, your week, on
// this day (worked out on the spot from the history copy, the Last.fm
// scrobbles from before it and the library copy) -----

/// An album remembered from your listening.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAlbum {
    pub title: String,
    pub artist: String,
    pub album_id: Option<u64>,
    pub performer_id: Option<u64>,
    pub image_key: Option<String>,
    /// Songs of it played (in the window the row is about).
    pub plays: u32,
    /// When it was last played (Unix ms).
    pub last_played: i64,
    /// For "on this day": the year.
    pub year: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YourWeek {
    pub plays: u32,
    pub seconds: i64,
    pub artists: usize,
    pub albums: Vec<MemoryAlbum>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeMemories {
    pub forgotten: Vec<MemoryAlbum>,
    pub week: YourWeek,
    pub on_this_day: Vec<MemoryAlbum>,
}

/// An album played this many songs' worth or more counts as a favorite.
const FORGOTTEN_PLAYS: u32 = 20;
/// ...and not played for this long makes it forgotten.
const FORGOTTEN_AFTER: i64 = 365 * 86_400_000;
const FORGOTTEN_MAX: usize = 60;
/// On this day: albums per past year.
const ON_THIS_DAY_PER_YEAR: usize = 3;

/// An album's key across the sources: the first credited artist and the
/// title without its edition, both folded.
fn memory_key(artist: &str, album: &str) -> String {
    let first = artist.split(" / ").next().unwrap_or(artist).split(", ").next().unwrap_or(artist);
    format!("{}|{}", fold(first), fold(&crate::sound::base_title(album)))
}

/// One play of an album, from Roon's history or a scrobble.
struct Listen {
    at: i64,
    key: String,
    title: String,
    artist: String,
    album_id: Option<u64>,
    performer_id: Option<u64>,
    image_key: Option<String>,
    seconds: i64,
}

/// Every album play: Roon's history (plays of at least half a song), and the
/// scrobbles from before Roon's oldest play.
fn all_listens(app: &AppHandle) -> Vec<Listen> {
    let mut out: Vec<Listen> = app.state::<crate::wrapped::Wrapped>().with_plays(app, |plays| {
        plays
            .iter()
            .filter(|p| crate::wrapped::counts(p) && !p.album.is_empty())
            .map(|p| Listen {
                at: p.played_at,
                key: memory_key(&p.artist, &p.album),
                title: p.album.clone(),
                artist: p.artist.clone(),
                album_id: p.album_id,
                performer_id: p.performer_id,
                image_key: p.image_key.clone(),
                seconds: p.seconds_played.or(p.length_seconds).unwrap_or(0).max(0) as i64,
            })
            .collect()
    });
    let oldest = out.iter().map(|l| l.at).min().unwrap_or(i64::MAX);
    crate::scrobbles::with(app, |s| {
        for p in s.plays.iter().filter(|p| p.at < oldest && !p.album.is_empty()) {
            out.push(Listen {
                at: p.at,
                key: memory_key(&p.artist, &p.album),
                title: p.album.clone(),
                artist: p.artist.clone(),
                album_id: None,
                performer_id: None,
                image_key: None,
                seconds: 0,
            });
        }
    });
    out
}

/// Adds Roon's ids and cover to albums known only by name: from Roon's
/// history (the same album played in Roon), else the library copy.
fn fill_ids(app: &AppHandle, albums: &mut [MemoryAlbum], known: &HashMap<String, (Option<u64>, Option<u64>, Option<String>)>) {
    let library: HashMap<String, crate::wrapped::LibraryAlbum> =
        app.state::<crate::wrapped::LibraryCopy>().albums(app).into_iter().map(|a| (memory_key(&a.artist, &a.title), a)).collect();
    for a in albums.iter_mut().filter(|a| a.album_id.is_none()) {
        let key = memory_key(&a.artist, &a.title);
        if let Some((album_id, performer_id, image_key)) = known.get(&key).filter(|k| k.0.is_some()) {
            a.album_id = *album_id;
            a.performer_id = a.performer_id.or(*performer_id);
            a.image_key = a.image_key.clone().or_else(|| image_key.clone());
        } else if let Some(l) = library.get(&key) {
            a.album_id = l.album_id;
            a.performer_id = a.performer_id.or(l.performer_id);
            a.image_key = a.image_key.clone().or_else(|| l.image_key.clone());
            a.title = l.title.clone();
        }
    }
}

/// The local calendar day of a moment ("YYYY-MM-DD"), `offset_min` as
/// JavaScript's getTimezoneOffset (UTC minus local, in minutes).
fn local_day(ms: i64, offset_min: i32) -> String {
    date_string(ms - offset_min as i64 * 60_000)
}

fn memories(app: &AppHandle, offset_min: i32, now: i64) -> HomeMemories {
    let listens = all_listens(app);
    // What Roon's history knows of each album (ids, cover), for the
    // scrobbled ones
    let mut known: HashMap<String, (Option<u64>, Option<u64>, Option<String>)> = HashMap::new();
    for l in listens.iter().filter(|l| l.album_id.is_some()) {
        known.entry(l.key.clone()).or_insert((l.album_id, l.performer_id, l.image_key.clone()));
    }
    let tally = |pick: &dyn Fn(&Listen) -> bool| -> HashMap<String, MemoryAlbum> {
        let mut rows: HashMap<String, MemoryAlbum> = HashMap::new();
        for l in listens.iter().filter(|l| pick(l)) {
            let row = rows.entry(l.key.clone()).or_insert_with(|| MemoryAlbum { title: l.title.clone(), artist: l.artist.clone(), ..Default::default() });
            row.plays += 1;
            if l.at > row.last_played {
                row.last_played = l.at;
            }
            if row.album_id.is_none() && l.album_id.is_some() {
                row.album_id = l.album_id;
                row.title = l.title.clone();
            }
            row.performer_id = row.performer_id.or(l.performer_id);
            if row.image_key.is_none() {
                row.image_key = l.image_key.clone();
            }
        }
        rows
    };

    // Forgotten favorites: much played, not for a year
    let mut forgotten: Vec<MemoryAlbum> = tally(&|_| true)
        .into_values()
        .filter(|a| a.plays >= FORGOTTEN_PLAYS && now - a.last_played >= FORGOTTEN_AFTER)
        .collect();
    fill_ids(app, &mut forgotten, &known);
    forgotten.retain(|a| a.album_id.is_some());
    forgotten.sort_by(|a, b| b.plays.cmp(&a.plays).then(b.last_played.cmp(&a.last_played)));
    forgotten.truncate(FORGOTTEN_MAX);

    // Your week: the last seven days
    let week_from = now - 7 * 86_400_000;
    let recent: Vec<&Listen> = listens.iter().filter(|l| l.at >= week_from).collect();
    let mut week = YourWeek {
        plays: recent.len() as u32,
        seconds: recent.iter().map(|l| l.seconds).sum(),
        artists: recent.iter().map(|l| fold(&l.artist)).collect::<HashSet<_>>().len(),
        albums: tally(&|l| l.at >= week_from).into_values().collect(),
    };
    week.albums.sort_by(|a, b| b.plays.cmp(&a.plays).then(b.last_played.cmp(&a.last_played)));
    week.albums.truncate(24);
    fill_ids(app, &mut week.albums, &known);

    // On this day: the same calendar day in the years before
    let today = local_day(now, offset_min);
    let (this_year, month_day) = (today[..4].parse::<i32>().unwrap_or(0), today[4..].to_string());
    let mut by_year: HashMap<i32, HashMap<String, MemoryAlbum>> = HashMap::new();
    for l in &listens {
        let day = local_day(l.at, offset_min);
        if day[4..] != month_day {
            continue;
        }
        let year = day[..4].parse::<i32>().unwrap_or(0);
        if year >= this_year {
            continue;
        }
        let row = by_year.entry(year).or_default().entry(l.key.clone()).or_insert_with(|| MemoryAlbum {
            title: l.title.clone(),
            artist: l.artist.clone(),
            year: Some(year),
            ..Default::default()
        });
        row.plays += 1;
        row.last_played = row.last_played.max(l.at);
        row.album_id = row.album_id.or(l.album_id);
        row.performer_id = row.performer_id.or(l.performer_id);
        if row.image_key.is_none() {
            row.image_key = l.image_key.clone();
        }
    }
    let mut years: Vec<i32> = by_year.keys().copied().collect();
    years.sort_unstable_by(|a, b| b.cmp(a));
    let mut on_this_day = Vec::new();
    for year in years {
        let mut albums: Vec<MemoryAlbum> = by_year.remove(&year).unwrap_or_default().into_values().collect();
        albums.sort_by(|a, b| b.plays.cmp(&a.plays));
        on_this_day.extend(albums.into_iter().take(ON_THIS_DAY_PER_YEAR));
    }
    fill_ids(app, &mut on_this_day, &known);

    HomeMemories { forgotten, week, on_this_day }
}

/// Home's rows from your own listening, worked out now. `tz_offset_min`:
/// the page's getTimezoneOffset (for "on this day").
#[tauri::command]
pub async fn wrapped_home_memories(app: AppHandle, tz_offset_min: Option<i32>) -> HomeMemories {
    let offset = tz_offset_min.unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || memories(&app, offset, now_ms())).await.unwrap_or_default()
}

fn listens_text(weight: f64) -> String {
    if weight >= 60.0 {
        "a lot".into()
    } else if weight >= 15.0 {
        "often".into()
    } else {
        "now and then".into()
    }
}

/// Unix milliseconds as "YYYY-MM-DD" (UTC).
fn date_string(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // (civil-from-days, Howard Hinnant's)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_keys_and_days() {
        assert_eq!(memory_key("Bölzer", "Hero (Remastered)"), memory_key("Bolzer / Someone", "Hero"));
        assert_ne!(memory_key("Bölzer", "Hero"), memory_key("Bölzer", "Soma"));
        // 2025-10-21 03:00 UTC is still the 20th in Chicago (UTC-5: offset 300)
        let at = 1_761_015_600_000;
        assert_eq!(local_day(at, 0), "2025-10-21");
        assert_eq!(local_day(at, 300), "2025-10-20");
    }

    #[test]
    fn dates_and_newest() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(date_string(1_761_000_000_000), "2025-10-20");
        let album = |title: &str, released: &str| HomeAlbum { title: title.into(), artist: "A".into(), released: released.into(), ..Default::default() };
        let rows = vec![(1.0, album("old", "2020-01-01")), (1.0, album("b", "2026-03-01")), (5.0, album("a", "2026-03-01")), (1.0, album("c", "2026-09-01")), (1.0, album("c", "2026-09-01"))];
        let out = newest(rows, "2025-10-21", |a, w| format!("{} {w}", a.title));
        assert_eq!(out.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), vec!["c", "a", "b"]);
        assert_eq!(out[1].why, "a 5");
        // (a daily look: the new one in front, the kept ones in their order)
        let (n, merged) = merge(out.clone(), vec![album("c", "2026-09-01"), album("d", "2026-10-01")]);
        assert_eq!(n, 1);
        assert_eq!(merged.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), vec!["d", "c", "a", "b"]);
    }
}
