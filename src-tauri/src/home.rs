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
use std::sync::{Arc, Mutex};
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
/// Your most played artists looked at for albums you may have missed.
const MISSED_ARTISTS: usize = 100;
/// Albums you may have missed: released from this many days ago...
const MISSED_FROM_DAYS: i64 = 1095;
/// ...to this many (newer ones are new albums).
const MISSED_TO_DAYS: i64 = 60;
/// At most this many of one artist's albums in that row.
const MISSED_PER_ARTIST: usize = 3;

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
    /// Albums from your favorite artists you may have missed: from the
    /// last three years but not the last two months, not played at all and
    /// not in the library, the artists you play most first (an unplayed one
    /// from the last year shows in the new albums rows too).
    pub missed: Vec<HomeAlbum>,
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
    let (new_from_favorites, missed) = favorite_albums(app, &client, &by_name, favorites).await;
    let live = crate::wrapped::live_albums(app);
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
                let albums = new_albums(&client, &card, &t.name, 1.0, live, &Window::new_albums()).await;
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
        "[home] {} artists, {} new albums from your artists, {} from the suggested, {} you may have missed, in {} s",
        artists.len(),
        new_from_favorites.len(),
        new_from_suggested.len(),
        missed.len(),
        started.elapsed().as_secs()
    );
    let now = now_ms();
    Ok(HomeData { artists, new_from_favorites, new_from_suggested, missed, built_at: now, albums_at: now, error: String::new() })
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
    let (fresh, missed) = favorite_albums(app, &client, &by_name, favorites).await;
    let live = crate::wrapped::live_albums(app);
    let kept = |list: Vec<HomeAlbum>| -> Vec<HomeAlbum> { without_left_out(list).into_iter().filter(|a| live || !roon_library::looks_live(&a.title)).collect() };
    let (added_favorites, new_from_favorites) = merge(kept(data.new_from_favorites), fresh);
    progress(app, "Looking for new albums from the suggested artists...".into());
    let mut rows: Vec<(f64, HomeAlbum)> = Vec::new();
    for chunk in data.artists.chunks(AT_ONCE) {
        let mut set = tokio::task::JoinSet::new();
        for a in chunk.iter().cloned() {
            let (client, like) = (client.clone(), like.clone());
            set.spawn(async move {
                let card = find_artist(&client, &a.name, &like).await?;
                let mut albums = new_albums(&client, &card, &a.name, 1.0, live, &Window::new_albums()).await;
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
    let (added_suggested, new_from_suggested) = merge(kept(data.new_from_suggested), newest(rows, &since(), |a, _| a.why.clone()));
    println!(
        "[home] new albums looked for: {added_favorites} new from your artists, {added_suggested} from the suggested, {} you may have missed, in {} s",
        missed.len(),
        started.elapsed().as_secs()
    );
    data.new_from_favorites = new_from_favorites;
    data.new_from_suggested = new_from_suggested;
    data.missed = missed;
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

/// New albums from your FAVORITES most played artists, newest first, and
/// the albums your MISSED_ARTISTS most played may have slipped past you
/// (see `Window::missed`), the artists you play most first.
async fn favorite_albums(app: &AppHandle, client: &LibraryClient, by_name: &HashMap<String, roon_library::ArtistCard>, favorites: Vec<(String, f64)>) -> (Vec<HomeAlbum>, Vec<HomeAlbum>) {
    progress(app, "Looking for new albums from the artists you play...".into());
    let mut rows: Vec<(f64, HomeAlbum)> = Vec::new();
    let mut missed_rows: Vec<(f64, HomeAlbum)> = Vec::new();
    let live = crate::wrapped::live_albums(app);
    let played = {
        let app = app.clone();
        Arc::new(tauri::async_runtime::spawn_blocking(move || played_albums(&app)).await.unwrap_or_default())
    };
    let (fresh, missed) = (Window::new_albums(), Window::missed(played));
    let top: Vec<(usize, String, f64)> = favorites.into_iter().take(FAVORITES.max(MISSED_ARTISTS)).enumerate().map(|(i, (n, w))| (i, n, w)).collect();
    let total = top.len();
    let mut done = 0;
    for chunk in top.chunks(AT_ONCE) {
        let mut set = tokio::task::JoinSet::new();
        for (rank, name, weight) in chunk.iter().cloned() {
            let (client, card) = (client.clone(), by_name.get(&fold(&name)).cloned());
            let (fresh, missed) = (fresh.clone(), missed.clone());
            set.spawn(async move {
                let card = match card {
                    Some(c) => Some(c),
                    None => find_artist(&client, &name, &HashSet::new()).await,
                };
                let Some(card) = card else { return (Vec::new(), Vec::new()) };
                let new = if rank < FAVORITES { new_albums(&client, &card, &name, weight, live, &fresh).await } else { Vec::new() };
                let old = if rank < MISSED_ARTISTS { new_albums(&client, &card, &name, weight, live, &missed).await } else { Vec::new() };
                (new, old)
            });
        }
        while let Some(found) = set.join_next().await {
            if let Ok((new, old)) = found {
                rows.extend(new);
                missed_rows.extend(old);
            }
        }
        done += chunk.len();
        progress(app, format!("Looking for new albums from the artists you play... {done} of {total}"));
    }
    let why = |a: &HomeAlbum, w: f64| format!("You listen to {} ({})", a.artist, listens_text(w));
    (newest(rows, &since(), why), by_artist_rank(missed_rows, why))
}

/// The releases an album row looks at: from and to (inclusive,
/// "YYYY-MM-DD"), and for the albums you may have missed, only the ones
/// you haven't played or added to the library.
#[derive(Clone)]
struct Window {
    from: String,
    to: String,
    unplayed: Option<Arc<Played>>,
}

impl Window {
    /// New albums: the last NEW_DAYS (and anything dated up to tomorrow).
    fn new_albums() -> Self {
        Window { from: since(), to: date_string(now_ms() + 86_400_000), unplayed: None }
    }

    /// Albums you may have missed: MISSED_FROM_DAYS to MISSED_TO_DAYS old,
    /// none of their songs played, not in the library.
    fn missed(played: Arc<Played>) -> Self {
        let now = now_ms();
        Window { from: date_string(now - MISSED_FROM_DAYS * 86_400_000), to: date_string(now - MISSED_TO_DAYS * 86_400_000), unplayed: Some(played) }
    }
}

/// The albums you've played any song of: in Roon's history (any play at
/// all) or your Last.fm scrobbles, by Roon's album id and by artist and
/// title.
#[derive(Default)]
struct Played {
    ids: HashSet<u64>,
    keys: HashSet<String>,
}

impl Played {
    fn has(&self, a: &HomeAlbum) -> bool {
        a.album_id.is_some_and(|id| self.ids.contains(&id)) || self.keys.contains(&memory_key(&a.artist, &a.title))
    }
}

fn played_albums(app: &AppHandle) -> Played {
    let mut played = Played::default();
    app.state::<crate::wrapped::Wrapped>().with_plays(app, |plays| {
        for p in plays.iter().filter(|p| !p.album.is_empty()) {
            if let Some(id) = p.album_id {
                played.ids.insert(id);
            }
            played.keys.insert(memory_key(&p.artist, &p.album));
        }
    });
    crate::scrobbles::with(app, |s| {
        for p in s.plays.iter().filter(|p| !p.album.is_empty()) {
            played.keys.insert(memory_key(&p.artist, &p.album));
        }
    });
    played
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
    data.missed.retain(|a| fold(&a.artist) != k);
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

/// Albums left out as another artist's of the same name (see
/// `new_albums`), by `album_key`: the ones already on the page go too.
static LEFT_OUT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The artist's own albums from the last NEW_DAYS (not appearances on
/// others', not collaborations or various-artists releases), each with
/// the weight given: its main albums only. No singles or EPs: the row is
/// for albums, and Roon's discography for a name like Samael holds a
/// dozen other artists' singles, all linked to the one artist.
///
/// Roon's discographies come from Qobuz, which lumps artists of one name
/// together (Samael's page had other Samaels' albums): an album whose Roon
/// genres share none with the artist's is someone else's and is left out.
/// Without genres on either side there's nothing to tell, and it stays.
/// Roon's umbrella genres (Pop/Rock, over nearly all rock and metal) don't
/// count either way: a Latin pop single tagged Pop/Rock shared that with
/// a black metal band. Live albums only with `live` (Settings). Only the
/// ones released in `window` (and unplayed, when it says so).
async fn new_albums(client: &LibraryClient, card: &roon_library::ArtistCard, artist: &str, weight: f64, live: bool, window: &Window) -> Vec<(f64, HomeAlbum)> {
    let Ok(albums) = client.artist_albums_for(&card.handle, None).await else { return Vec::new() };
    let mine = fold(artist);
    let own: Vec<roon_library::ArtistAlbum> = albums
        .into_iter()
        .filter(|a| a.group != "appearance")
        .filter(|a| live || !a.live)
        .filter(|a| {
            let by = fold(&a.artist);
            by.is_empty() || by == mine
        })
        .collect();
    let recent: Vec<roon_library::ArtistAlbum> = own
        .into_iter()
        .filter(|a| a.group == "main")
        .filter(|a| !looks_episode(&a.title))
        .filter(|a| a.released.as_deref().is_some_and(|d| d >= window.from.as_str() && d <= window.to.as_str()))
        .collect();
    if recent.is_empty() {
        return Vec::new();
    }
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let theirs: HashSet<String> = crate::discover::genres_of(client, &mut cache, &card.handle).await.into_iter().filter(|g| !umbrella(g)).collect();
    let mut out = Vec::new();
    for a in recent {
        let Some(released) = a.released.clone() else { continue };
        let album = HomeAlbum {
            title: a.title.clone(),
            artist: if a.artist.is_empty() { artist.to_string() } else { a.artist.clone() },
            album_id: a.album_id,
            performer_id: card.performer_id,
            image_key: a.image_key.clone(),
            released,
            in_library: a.in_library,
            why: String::new(),
        };
        // (for albums you may have missed: not ones you've heard or have)
        if window.unplayed.as_ref().is_some_and(|played| a.in_library || played.has(&album)) {
            continue;
        }
        // (Roon linking it to another artist of that name: someone else's)
        if let (Some(theirs_id), Some(card_id)) = (a.performer_id, card.performer_id) {
            if theirs_id != card_id {
                println!("[home] left out \"{}\" by {}: Roon links it to another artist of that name (id {theirs_id}, not {card_id})", album.title, album.artist);
                LEFT_OUT.lock().unwrap().get_or_insert_with(HashSet::new).insert(album_key(&album));
                continue;
            }
        }
        if !theirs.is_empty() {
            let all: Vec<String> = client.album_facts(&a.handle).await.map(|f| f.genres).unwrap_or_default().into_iter().map(|g| g.to_lowercase()).collect();
            // (a podcast's episodes or an audiobook, filed with the albums)
            if all.iter().any(|g| spoken(g)) {
                println!("[home] left out \"{}\" by {}: spoken word ({})", album.title, album.artist, all.join(", "));
                continue;
            }
            let genres: Vec<String> = all.into_iter().filter(|g| !umbrella(g)).collect();
            if !genres.is_empty() && !genres.iter().any(|g| theirs.contains(g)) {
                println!("[home] left out \"{}\" by {}: its genres ({}) aren't the artist's ({})", album.title, album.artist, genres.join(", "), theirs.iter().cloned().collect::<Vec<_>>().join(", "));
                LEFT_OUT.lock().unwrap().get_or_insert_with(HashSet::new).insert(album_key(&album));
                continue;
            }
        }
        out.push((weight, album));
    }
    out
}

/// A podcast episode by its title ("Episode 114: ...", "Ep. 12", "#45",
/// "... Podcast"): Qobuz files some bands' podcasts with their albums
/// (Metallica's The Metallica Report, an album an episode).
fn looks_episode(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    let numbered = |rest: &str| rest.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit());
    t.contains("podcast") || ["episode ", "ep. ", "ep "].iter().any(|p| t.strip_prefix(p).is_some_and(numbered)) || t.strip_prefix('#').is_some_and(numbered)
}

/// A genre for talk, not music (not comedy rock and the like).
fn spoken(genre: &str) -> bool {
    genre == "comedy/spoken" || ["spoken word", "podcast", "audiobook"].iter().any(|w| genre.contains(w))
}

/// One of Roon's umbrella genres, too wide to tell artists apart by.
fn umbrella(genre: &str) -> bool {
    matches!(genre, "pop/rock" | "rock" | "pop")
}

/// `list` without the albums `new_albums` has left out as another artist's.
fn without_left_out(list: Vec<HomeAlbum>) -> Vec<HomeAlbum> {
    let left_out = LEFT_OUT.lock().unwrap();
    let Some(left_out) = left_out.as_ref() else { return list };
    list.into_iter().filter(|a| !left_out.contains(&album_key(a))).collect()
}

/// The albums released since `since`, newest first, then by weight; one
/// entry per album, ALBUMS at most, each with its why line.
fn newest(mut rows: Vec<(f64, HomeAlbum)>, since: &str, why: impl Fn(&HomeAlbum, f64) -> String) -> Vec<HomeAlbum> {
    let today = date_string(now_ms() + 86_400_000);
    rows.retain(|(_, a)| a.released.as_str() >= since && a.released.as_str() <= today.as_str());
    rows.sort_by(|a, b| b.1.released.cmp(&a.1.released).then(b.0.total_cmp(&a.0)));
    one_each(rows, why, None)
}

/// The albums by artist, the most played artist first (each one's newest
/// first, MISSED_PER_ARTIST at most); one entry per album, ALBUMS at most,
/// each with its why line.
fn by_artist_rank(mut rows: Vec<(f64, HomeAlbum)>, why: impl Fn(&HomeAlbum, f64) -> String) -> Vec<HomeAlbum> {
    rows.sort_by(|a, b| b.0.total_cmp(&a.0).then(fold(&a.1.artist).cmp(&fold(&b.1.artist))).then(b.1.released.cmp(&a.1.released)));
    one_each(rows, why, Some(MISSED_PER_ARTIST))
}

/// One entry per album (and one edition of it), ALBUMS at most (and
/// `per_artist` of one artist's), in the order given, each with its why
/// line.
fn one_each(rows: Vec<(f64, HomeAlbum)>, why: impl Fn(&HomeAlbum, f64) -> String, per_artist: Option<usize>) -> Vec<HomeAlbum> {
    let mut each: HashMap<String, usize> = HashMap::new();
    // (one entry per album, and one edition of it: "Sad Wings of Destiny
    // (50th Anniversary)" and "(Original 1976 Mix)" are one)
    let mut seen: HashSet<String> = HashSet::new();
    let mut titles: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (w, mut a) in rows {
        if !seen.insert(album_key(&a)) || !titles.insert(format!("{}|{}", fold(&a.artist), crate::sound::base_title(&a.title))) {
            continue;
        }
        if let Some(most) = per_artist {
            let n = each.entry(fold(&a.artist)).or_default();
            if *n >= most {
                continue;
            }
            *n += 1;
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
    // (Forgotten favorites and On this day suggest albums: no live ones
    // while Settings' Live albums is off; Your week is what you played)
    let live = crate::wrapped::live_albums(app);
    let suggestable = |a: &MemoryAlbum| live || !roon_library::looks_live(&a.title);
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
        .filter(suggestable)
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
        let mut albums: Vec<MemoryAlbum> = by_year.remove(&year).unwrap_or_default().into_values().filter(suggestable).collect();
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
    fn podcast_episodes() {
        assert!(looks_episode("Episode 114: Robert Trujillo's Radio Roots"));
        assert!(looks_episode("Ep. 12 - Live Talk"));
        assert!(looks_episode("#45: The Interview"));
        assert!(looks_episode("The Metallica Podcast"));
        assert!(!looks_episode("72 Seasons"));
        assert!(!looks_episode("Episode"));
        assert!(!looks_episode("Epitaph"));
        assert!(spoken("comedy/spoken") && spoken("spoken word") && spoken("podcasts"));
        assert!(!spoken("heavy metal") && !spoken("comedy rock"));
    }

    #[test]
    fn missed_albums_by_artist_rank() {
        let album = |artist: &str, title: &str, released: &str| HomeAlbum { title: title.into(), artist: artist.into(), released: released.into(), ..Default::default() };
        let rows = vec![
            (10.0, album("Beta", "b1", "2025-01-01")),
            (50.0, album("Alpha", "a1", "2024-06-01")),
            (50.0, album("Alpha", "a2", "2025-03-01")),
            (10.0, album("Beta", "b2", "2025-05-01")),
        ];
        let out = by_artist_rank(rows, |_, _| String::new());
        let titles: Vec<&str> = out.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["a2", "a1", "b2", "b1"]);
        // (one artist fills MISSED_PER_ARTIST at most)
        let many: Vec<(f64, HomeAlbum)> = (1..=6).map(|i| (90.0, album("Gamma", &format!("g{i}"), &format!("2025-0{i}-01")))).collect();
        assert_eq!(by_artist_rank(many, |_, _| String::new()).len(), MISSED_PER_ARTIST);
    }

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
