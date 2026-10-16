//! Artist tags from outside Roon, for Wrapped: Roon's own genres stop at
//! "Death Metal, Heavy Metal, Pop/Rock" for a band like Amon Amarth, while
//! Last.fm's listeners tag it "melodic death metal" and "viking metal", and
//! MusicBrainz knows where it is from and since when. One lookup per artist,
//! kept in `wrapped-tags.json` in the app's data folder for 90 days.
//!
//! Sources: Last.fm `artist.getTopTags` (needs the user's own API key, from
//! Wrapped's settings) and MusicBrainz `ws/2/artist` (no key; one request a
//! second, a User-Agent that names the app). Each tag is sorted into one of
//! three kinds, so the pages can offer "similar by genre / style / scene":
//! genre (the sound), style (the themes: viking, pagan, epic...), scene (a
//! country or city). Tags that say nothing about the music ("seen live",
//! "favorites") are dropped.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::wrapped::{read_json, write_json, HistoryArtist, LibraryCopy, Wrapped, WrappedSettings};
use tauri::Emitter;

const KEEP_FOR_MS: i64 = 90 * 86_400_000;
const USER_AGENT: &str = concat!("RoonToasted/", env!("CARGO_PKG_VERSION"), " (https://github.com/Vulkandr/roon-toasted)");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagKind {
    Genre,
    Style,
    Scene,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub name: String,
    /// Last.fm's 0..100 weight (100 for the artist's top tag); 50 for a
    /// MusicBrainz tag.
    pub weight: u32,
    pub kind: TagKind,
}

/// What the outside knows about an artist.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistTags {
    pub name: String,
    pub tags: Vec<Tag>,
    pub mbid: Option<String>,
    pub country: Option<String>,
    pub area: Option<String>,
    pub begin_year: Option<u32>,
    pub fetched_at: i64,
    pub from_lastfm: bool,
    pub from_musicbrainz: bool,
    /// Why a source gave nothing, for the settings page.
    #[serde(default)]
    pub note: Option<String>,
}

/// What Last.fm's listeners call one album (its artist's tags are the
/// fallback when they are few).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumTags {
    pub artist: String,
    pub album: String,
    pub tags: Vec<Tag>,
    pub fetched_at: i64,
    pub from_lastfm: bool,
}

/// Last.fm's neighbours of an artist (artist.getSimilar), with its match
/// score 0..1 each.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarCache {
    pub names: Vec<(String, f64)>,
    pub fetched_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    /// Keyed by the artist's name, lower-cased.
    artists: HashMap<String, ArtistTags>,
    /// Keyed by "artist\u{0}album", lower-cased.
    albums: HashMap<String, AlbumTags>,
    /// Keyed by the artist's name, lower-cased.
    similar: HashMap<String, SimilarCache>,
}

#[derive(Default)]
pub struct Tags {
    store: Mutex<Option<Store>>,
    /// Serializes the lookups (MusicBrainz wants at most one a second).
    gate: tokio::sync::Mutex<Option<Instant>>,
    /// The "tag my artists" job (see `wrapped_tags_fill`).
    filling: std::sync::atomic::AtomicBool,
    stop_filling: std::sync::atomic::AtomicBool,
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-tags.json"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn key_of(name: &str) -> String {
    name.trim().to_lowercase()
}

impl Tags {
    fn with_store<T>(&self, app: &AppHandle, f: impl FnOnce(&mut Store) -> T) -> T {
        let mut guard = self.store.lock().unwrap();
        let store = guard.get_or_insert_with(|| path(app).and_then(|p| read_json(&p)).unwrap_or_default());
        f(store)
    }

    fn save(&self, app: &AppHandle) {
        let Some(path) = path(app) else { return };
        let guard = self.store.lock().unwrap();
        if let Some(store) = guard.as_ref() {
            write_json(&path, store);
        }
    }

    pub fn cached(&self, app: &AppHandle, name: &str) -> Option<ArtistTags> {
        self.with_store(app, |s| s.artists.get(&key_of(name)).cloned())
    }

    /// The artist's tags, fetched when missing or stale.
    pub async fn lookup(&self, app: &AppHandle, name: &str, lastfm_key: &str) -> ArtistTags {
        let name = name.trim();
        if let Some(have) = self.cached(app, name) {
            let fresh = now_ms() - have.fetched_at < KEEP_FOR_MS;
            // A lookup made without a Last.fm key is retried once there is one
            let complete = have.from_lastfm || lastfm_key.is_empty();
            if fresh && complete {
                return have;
            }
        }
        self.turn().await;
        let fetched = fetch(name, lastfm_key).await;
        self.with_store(app, |s| {
            s.artists.insert(key_of(name), fetched.clone());
        });
        self.save(app);
        fetched
    }

    pub fn count(&self, app: &AppHandle) -> usize {
        self.with_store(app, |s| s.artists.len())
    }

    /// Waits for the lookup gate (one outside request a second).
    async fn turn(&self) {
        let mut gate = self.gate.lock().await;
        if let Some(last) = *gate {
            let since = last.elapsed();
            if since < Duration::from_millis(1100) {
                tokio::time::sleep(Duration::from_millis(1100) - since).await;
            }
        }
        *gate = Some(Instant::now());
    }

    /// An album's own tags, fetched when missing or stale.
    pub async fn lookup_album(&self, app: &AppHandle, artist: &str, album: &str, lastfm_key: &str) -> AlbumTags {
        let key = format!("{}\u{0}{}", key_of(artist), key_of(album));
        if let Some(have) = self.with_store(app, |s| s.albums.get(&key).cloned()) {
            if now_ms() - have.fetched_at < KEEP_FOR_MS && (have.from_lastfm || lastfm_key.is_empty()) {
                return have;
            }
        }
        let mut out = AlbumTags { artist: artist.to_string(), album: album.to_string(), fetched_at: now_ms(), ..Default::default() };
        if !lastfm_key.is_empty() {
            self.turn().await;
            if let Ok(client) = client() {
                if let Ok(tags) = lastfm_album_top_tags(&client, artist, album, lastfm_key).await {
                    out.from_lastfm = true;
                    out.tags = tags;
                }
            }
        }
        self.with_store(app, |s| {
            s.albums.insert(key, out.clone());
        });
        self.save(app);
        out
    }

    /// Last.fm's neighbours of an artist, fetched when missing or stale
    /// (empty without a key).
    pub async fn lookup_similar(&self, app: &AppHandle, name: &str, lastfm_key: &str) -> Vec<(String, f64)> {
        if lastfm_key.is_empty() {
            return Vec::new();
        }
        let key = key_of(name);
        if let Some(have) = self.with_store(app, |s| s.similar.get(&key).cloned()) {
            if now_ms() - have.fetched_at < KEEP_FOR_MS {
                return have.names;
            }
        }
        self.turn().await;
        let names = match client() {
            Ok(client) => lastfm_similar(&client, name, lastfm_key).await.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        self.with_store(app, |s| {
            s.similar.insert(key, SimilarCache { names: names.clone(), fetched_at: now_ms() });
        });
        self.save(app);
        names
    }

    /// Everything tagged so far, for the similarity scores.
    fn all_artists(&self, app: &AppHandle) -> Vec<ArtistTags> {
        self.with_store(app, |s| s.artists.values().cloned().collect())
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch(name: &str, lastfm_key: &str) -> ArtistTags {
    let mut out = ArtistTags { name: name.to_string(), fetched_at: now_ms(), ..Default::default() };
    let mut notes = Vec::new();
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            out.note = Some(e);
            return out;
        }
    };
    if !lastfm_key.is_empty() {
        match lastfm_top_tags(&client, name, lastfm_key).await {
            Ok(tags) => {
                out.from_lastfm = true;
                out.tags = tags;
            }
            Err(e) => notes.push(format!("Last.fm: {e}")),
        }
    }
    match musicbrainz(&client, name).await {
        Ok(mb) => {
            out.from_musicbrainz = true;
            out.mbid = mb.mbid;
            out.country = mb.country;
            out.area = mb.area;
            out.begin_year = mb.begin_year;
            for tag in mb.tags {
                if !out.tags.iter().any(|t| t.name == tag.name) {
                    out.tags.push(tag);
                }
            }
        }
        Err(e) => notes.push(format!("MusicBrainz: {e}")),
    }
    // A country tag from the artist's own country, so "scene" works even
    // when nobody tagged it
    if let Some(country) = out.country.as_deref().and_then(country_name) {
        if !out.tags.iter().any(|t| t.kind == TagKind::Scene) {
            out.tags.push(Tag { name: country.to_string(), weight: 40, kind: TagKind::Scene });
        }
    }
    out.tags.sort_by(|a, b| b.weight.cmp(&a.weight));
    if !notes.is_empty() {
        out.note = Some(notes.join("; "));
    }
    out
}

// ----- Last.fm -----

#[derive(Deserialize)]
struct LastfmTopTags {
    toptags: Option<LastfmTags>,
    error: Option<i32>,
    message: Option<String>,
}
#[derive(Deserialize)]
struct LastfmTags {
    tag: Vec<LastfmTag>,
}
#[derive(Deserialize)]
struct LastfmTag {
    name: String,
    count: u32,
}

async fn lastfm_top_tags(client: &reqwest::Client, name: &str, key: &str) -> Result<Vec<Tag>, String> {
    let res = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&[("method", "artist.gettoptags"), ("artist", name), ("api_key", key), ("format", "json"), ("autocorrect", "1")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let body: LastfmTopTags = res.json().await.map_err(|e| e.to_string())?;
    if let Some(code) = body.error {
        return Err(body.message.unwrap_or_else(|| format!("error {code}")));
    }
    let tags = body.toptags.map(|t| t.tag).unwrap_or_default();
    Ok(tags
        .into_iter()
        .filter(|t| t.count >= 5)
        .filter_map(|t| classify(&t.name).map(|(name, kind)| Tag { name, weight: t.count.min(100), kind }))
        .take(12)
        .collect())
}

async fn lastfm_album_top_tags(client: &reqwest::Client, artist: &str, album: &str, key: &str) -> Result<Vec<Tag>, String> {
    let res = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&[
            ("method", "album.gettoptags"),
            ("artist", artist),
            ("album", album),
            ("api_key", key),
            ("format", "json"),
            ("autocorrect", "1"),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let body: LastfmTopTags = res.json().await.map_err(|e| e.to_string())?;
    if let Some(code) = body.error {
        return Err(body.message.unwrap_or_else(|| format!("error {code}")));
    }
    let tags = body.toptags.map(|t| t.tag).unwrap_or_default();
    Ok(tags
        .into_iter()
        .filter(|t| t.count >= 5)
        .filter_map(|t| classify(&t.name).map(|(name, kind)| Tag { name, weight: t.count.min(100), kind }))
        .take(12)
        .collect())
}

#[derive(Deserialize)]
struct LastfmSimilar {
    similarartists: Option<LastfmSimilarList>,
    error: Option<i32>,
    message: Option<String>,
}
#[derive(Deserialize)]
struct LastfmSimilarList {
    artist: Vec<LastfmSimilarArtist>,
}
#[derive(Deserialize)]
struct LastfmSimilarArtist {
    name: String,
    #[serde(rename = "match")]
    score: Option<String>,
}

async fn lastfm_similar(client: &reqwest::Client, name: &str, key: &str) -> Result<Vec<(String, f64)>, String> {
    let res = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&[("method", "artist.getsimilar"), ("artist", name), ("api_key", key), ("format", "json"), ("autocorrect", "1"), ("limit", "60")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let body: LastfmSimilar = res.json().await.map_err(|e| e.to_string())?;
    if let Some(code) = body.error {
        return Err(body.message.unwrap_or_else(|| format!("error {code}")));
    }
    Ok(body
        .similarartists
        .map(|l| l.artist)
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.name, a.score.and_then(|m| m.parse().ok()).unwrap_or(0.0)))
        .collect())
}

// ----- MusicBrainz -----

#[derive(Default)]
struct MbArtist {
    mbid: Option<String>,
    country: Option<String>,
    area: Option<String>,
    begin_year: Option<u32>,
    tags: Vec<Tag>,
}

#[derive(Deserialize)]
struct MbSearch {
    artists: Vec<MbArtistJson>,
}
#[derive(Deserialize)]
struct MbArtistJson {
    id: String,
    name: String,
    score: Option<u32>,
    country: Option<String>,
    area: Option<MbArea>,
    #[serde(rename = "begin-area")]
    begin_area: Option<MbArea>,
    #[serde(rename = "life-span")]
    life_span: Option<MbLifeSpan>,
    tags: Option<Vec<MbTag>>,
}
#[derive(Deserialize)]
struct MbArea {
    name: String,
}
#[derive(Deserialize)]
struct MbLifeSpan {
    begin: Option<String>,
}
#[derive(Deserialize)]
struct MbTag {
    name: String,
    count: Option<i32>,
}

async fn musicbrainz(client: &reqwest::Client, name: &str) -> Result<MbArtist, String> {
    let query = format!("artist:\"{}\"", name.replace('"', ""));
    let res = client
        .get("https://musicbrainz.org/ws/2/artist/")
        .query(&[("query", query.as_str()), ("fmt", "json"), ("limit", "3")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("status {}", res.status()));
    }
    let body: MbSearch = res.json().await.map_err(|e| e.to_string())?;
    let wanted = key_of(name);
    let best = body
        .artists
        .into_iter()
        .filter(|a| a.score.unwrap_or(0) >= 80)
        .max_by_key(|a| (key_of(&a.name) == wanted, a.score.unwrap_or(0)))
        .ok_or_else(|| "no match".to_string())?;
    let begin_year = best.life_span.as_ref().and_then(|l| l.begin.as_deref()).and_then(|b| b.get(0..4)?.parse().ok());
    let area = best.begin_area.map(|a| a.name).or_else(|| best.area.map(|a| a.name));
    let tags = best
        .tags
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.count.unwrap_or(0) > 0)
        .filter_map(|t| classify(&t.name).map(|(name, kind)| Tag { name, weight: 50, kind }))
        .take(8)
        .collect();
    Ok(MbArtist { mbid: Some(best.id), country: best.country, area, begin_year, tags })
}

// ----- Sorting tags into genre, style and scene -----

/// Tags about themes, mood and image rather than sound, taken whole.
const STYLE_TAGS: &[&str] = &[
    "viking", "pagan", "epic", "satanic", "occult", "christian", "fantasy", "medieval", "pirate", "horror", "gore",
    "political", "atmospheric", "melancholic", "melancholy", "dark", "brutal", "raw", "cold", "trve", "kvlt",
    "cinematic", "chill", "party", "summer", "winter", "christmas", "instrumental", "female vocalists",
    "female fronted", "male vocalists", "acoustic", "live", "cover", "covers", "soundtrack", "80s", "90s", "00s",
    "70s", "60s", "oldies", "classic", "retro", "underground", "mainstream", "cult", "heavy", "fast", "slow",
    "beautiful", "sad", "happy", "angry", "relaxing", "energetic", "mellow", "anime", "video game", "lyrics",
    "guitar", "guitar virtuoso", "shred", "growl", "clean vocals", "harsh vocals", "screamo vocals",
];

/// Themes that make "<theme> metal" or "<theme> rock" a style rather than
/// a genre (viking metal is a theme on melodic death metal).
const STYLE_THEMES: &[&str] = &[
    "viking", "pagan", "pirate", "war", "battle", "epic", "fantasy", "medieval", "horror", "christian", "satanic",
    "occult", "political", "anime", "christmas", "party", "comedy", "nerd", "geek", "cowboy", "space",
];

/// Tags that say nothing about the music.
const JUNK: &[&str] = &[
    "seen live", "favorites", "favourites", "favorite", "favourite", "awesome", "love", "loved", "best", "good",
    "great", "my favorite", "all", "music", "albums i own", "under 2000 listeners", "check out", "spotify",
    "bands", "artists", "band", "male", "female", "singer", "songwriter", "legend", "legends", "rip", "overrated",
    "shit", "crap", "fuck", "number", "numbers", "misc", "other", "unknown", "test",
];

/// Country and city tags: the scene.
const SCENES: &[&str] = &[
    "swedish", "sweden", "finnish", "finland", "norwegian", "norway", "danish", "denmark", "icelandic", "iceland",
    "german", "germany", "british", "uk", "english", "england", "scottish", "scotland", "irish", "ireland", "welsh",
    "american", "usa", "us", "canadian", "canada", "australian", "australia", "japanese", "japan", "french", "france",
    "dutch", "netherlands", "belgian", "belgium", "polish", "poland", "russian", "russia", "ukrainian", "italian",
    "italy", "spanish", "spain", "portuguese", "portugal", "brazilian", "brazil", "mexican", "argentinian",
    "chilean", "greek", "greece", "turkish", "czech", "austrian", "austria", "swiss", "switzerland", "hungarian",
    "romanian", "estonian", "latvian", "lithuanian", "chinese", "korean", "k-pop", "j-pop", "indian", "israeli",
    "south african", "new zealand", "gothenburg", "stockholm", "oslo", "bergen", "helsinki", "tampere", "berlin",
    "hamburg", "london", "manchester", "birmingham", "seattle", "bay area", "florida", "new york", "los angeles",
    "chicago", "texas", "detroit", "philadelphia", "boston", "atlanta", "nashville", "portland", "scandinavian",
    "nordic", "european", "latin", "balkan", "celtic", "nwobhm", "nwoahm", "new wave of british heavy metal",
];

fn classify(raw: &str) -> Option<(String, TagKind)> {
    let name = raw.trim().to_lowercase();
    if name.is_empty() || name.len() > 40 || JUNK.contains(&name.as_str()) {
        return None;
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if SCENES.contains(&name.as_str()) {
        return Some((name, TagKind::Scene));
    }
    if STYLE_TAGS.contains(&name.as_str()) {
        return Some((name, TagKind::Style));
    }
    for suffix in [" metal", " rock", " punk"] {
        if let Some(theme) = name.strip_suffix(suffix) {
            if STYLE_THEMES.contains(&theme) {
                return Some((name, TagKind::Style));
            }
        }
    }
    Some((name, TagKind::Genre))
}

/// A country's name for its ISO code, for the scene tag.
fn country_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "SE" => "swedish",
        "FI" => "finnish",
        "NO" => "norwegian",
        "DK" => "danish",
        "IS" => "icelandic",
        "DE" => "german",
        "GB" => "british",
        "IE" => "irish",
        "US" => "american",
        "CA" => "canadian",
        "AU" => "australian",
        "NZ" => "new zealand",
        "JP" => "japanese",
        "FR" => "french",
        "NL" => "dutch",
        "BE" => "belgian",
        "PL" => "polish",
        "RU" => "russian",
        "UA" => "ukrainian",
        "IT" => "italian",
        "ES" => "spanish",
        "PT" => "portuguese",
        "BR" => "brazilian",
        "MX" => "mexican",
        "AR" => "argentinian",
        "CL" => "chilean",
        "GR" => "greek",
        "TR" => "turkish",
        "CZ" => "czech",
        "AT" => "austrian",
        "CH" => "swiss",
        "HU" => "hungarian",
        "RO" => "romanian",
        "EE" => "estonian",
        "LV" => "latvian",
        "LT" => "lithuanian",
        "CN" => "chinese",
        "KR" => "korean",
        "IN" => "indian",
        "IL" => "israeli",
        "ZA" => "south african",
        _ => return None,
    })
}

// ----- Commands -----

/// What the outside says about an artist (cached; fetched when missing).
#[tauri::command]
pub async fn wrapped_artist_tags(app: AppHandle, name: String) -> Result<ArtistTags, String> {
    if name.trim().is_empty() {
        return Err("No artist name.".into());
    }
    let key = WrappedSettings::load(&app).lastfm_key;
    let tags = app.state::<Tags>();
    Ok(tags.lookup(&app, &name, &key).await)
}

/// An album's tags (its artist's when Last.fm has fewer than three for it).
#[tauri::command]
pub async fn wrapped_album_tags(app: AppHandle, artist: String, album: String) -> Result<AlbumTags, String> {
    if artist.trim().is_empty() || album.trim().is_empty() {
        return Err("No album.".into());
    }
    let key = WrappedSettings::load(&app).lastfm_key;
    let tags = app.state::<Tags>();
    let mut own = tags.lookup_album(&app, &artist, &album, &key).await;
    if own.tags.len() < 3 {
        let of_artist = tags.lookup(&app, &artist, &key).await;
        for tag in of_artist.tags {
            if !own.tags.iter().any(|t| t.name == tag.name) {
                own.tags.push(tag);
            }
        }
    }
    Ok(own)
}

/// How many artists are in the tag cache.
#[tauri::command]
pub fn wrapped_tags_count(app: AppHandle) -> usize {
    app.state::<Tags>().count(&app)
}

/// The tag cache against the history: how many of the artists you have
/// played are tagged, and whether the job is running.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FillStatus {
    pub tagged: usize,
    pub artists: usize,
    pub filling: bool,
    /// Whether the library's artists have been read (Library / Discover
    /// need them).
    pub library: bool,
}

/// Every artist Wrapped knows: the history's (most played first), then
/// the library's that were never played (most albums first).
fn known_artists(app: &AppHandle) -> Vec<HistoryArtist> {
    let mut out = app.state::<Wrapped>().artists(app);
    let mut seen: std::collections::HashSet<String> = out.iter().map(|a| key_of(&a.name)).collect();
    let mut library = app.state::<LibraryCopy>().artists(app);
    library.sort_by(|a, b| b.album_count.unwrap_or(0).cmp(&a.album_count.unwrap_or(0)));
    for a in library {
        if seen.insert(key_of(&a.name)) {
            out.push(HistoryArtist { name: a.name, plays: 0, performer_id: a.performer_id, album_id: None, image_key: a.image_key, last_played: 0 });
        }
    }
    out
}

fn fill_status(app: &AppHandle) -> FillStatus {
    let tags = app.state::<Tags>();
    let artists = known_artists(app);
    let tagged = tags.with_store(app, |s| artists.iter().filter(|a| s.artists.contains_key(&key_of(&a.name))).count());
    let library = !app.state::<LibraryCopy>().artists(app).is_empty();
    FillStatus { tagged, artists: artists.len(), filling: tags.filling.load(std::sync::atomic::Ordering::SeqCst), library }
}

#[tauri::command]
pub fn wrapped_tags_status(app: AppHandle) -> FillStatus {
    fill_status(&app)
}

/// Tags every artist in the history copy, most played first, one lookup a
/// second in the background; "wrapped-tags" events carry the progress.
/// `stop` ends a running job.
#[tauri::command]
pub fn wrapped_tags_fill(app: AppHandle, stop: Option<bool>) -> FillStatus {
    use std::sync::atomic::Ordering;
    let tags = app.state::<Tags>();
    if stop.unwrap_or(false) {
        tags.stop_filling.store(true, Ordering::SeqCst);
        return fill_status(&app);
    }
    if tags.filling.swap(true, Ordering::SeqCst) {
        return fill_status(&app);
    }
    tags.stop_filling.store(false, Ordering::SeqCst);
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let tags = app2.state::<Tags>();
        let key = WrappedSettings::load(&app2).lastfm_key;
        let artists = known_artists(&app2);
        let total = artists.len();
        let mut done = 0;
        for artist in artists {
            if tags.stop_filling.load(Ordering::SeqCst) {
                break;
            }
            let have = tags.cached(&app2, &artist.name).is_some_and(|t| t.from_lastfm || key.is_empty());
            if !have {
                tags.lookup(&app2, &artist.name, &key).await;
            }
            done += 1;
            if done % 5 == 0 || !have {
                let _ = app2.emit("wrapped-tags", serde_json::json!({ "done": done, "total": total, "finished": false }));
            }
        }
        tags.filling.store(false, Ordering::SeqCst);
        let _ = app2.emit("wrapped-tags", serde_json::json!({ "done": done, "total": total, "finished": true }));
    });
    fill_status(&app)
}

// ----- Similar artists by the tags -----

/// One artist like another (see `wrapped_similar`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarRow {
    pub name: String,
    pub score: f64,
    /// The tags they share (or the year, for Era).
    pub why: Vec<String>,
    pub performer_id: Option<u64>,
    pub album_id: Option<u64>,
    pub image_key: Option<String>,
    pub plays: u32,
    pub in_library: bool,
}

fn overlap(a: &[Tag], b: &[Tag], kind: TagKind) -> (f64, Vec<String>) {
    let total: u32 = a.iter().filter(|t| t.kind == kind).map(|t| t.weight).sum();
    if total == 0 {
        return (0.0, Vec::new());
    }
    let mut shared = 0u32;
    let mut why = Vec::new();
    for t in a.iter().filter(|t| t.kind == kind) {
        if let Some(o) = b.iter().find(|o| o.kind == kind && o.name == t.name) {
            shared += t.weight.min(o.weight);
            why.push(t.name.clone());
        }
    }
    (shared as f64 / total as f64, why)
}

/// What `wrapped_similar` answers: the rows so far, and how many Discover
/// neighbours are still being tagged (more rows follow as "wrapped-similar"
/// events carrying the same `key`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarAnswer {
    pub key: String,
    pub rows: Vec<SimilarRow>,
    pub pending: usize,
}

/// Everything a ranking needs, so it can be redone as tags arrive.
struct SimilarQuery {
    me: String,
    subject_tags: Vec<Tag>,
    subject_year: Option<u32>,
    kind: Option<TagKind>,
    discover: bool,
    matches: HashMap<String, f64>,
    limit: usize,
}

fn rank_similar(app: &AppHandle, q: &SimilarQuery) -> Vec<SimilarRow> {
    let tags = app.state::<Tags>();
    let history: HashMap<String, HistoryArtist> = known_artists(app).into_iter().map(|a| (key_of(&a.name), a)).collect();
    let in_library = app.state::<LibraryCopy>().names(app);
    let mut rows: Vec<SimilarRow> = tags
        .all_artists(app)
        .into_iter()
        .filter(|c| key_of(&c.name) != q.me)
        .filter(|c| in_library.contains(&key_of(&c.name)) != q.discover)
        .filter_map(|c| {
            let (score, why) = match q.kind {
                Some(TagKind::Scene) => {
                    let (scene, mut why) = overlap(&q.subject_tags, &c.tags, TagKind::Scene);
                    if scene == 0.0 {
                        return None;
                    }
                    let (genre, shared) = overlap(&q.subject_tags, &c.tags, TagKind::Genre);
                    why.extend(shared);
                    (scene * 0.3 + genre, why)
                }
                Some(kind) => {
                    let (score, why) = overlap(&q.subject_tags, &c.tags, kind);
                    if score == 0.0 {
                        return None;
                    }
                    (score, why)
                }
                None => {
                    let (year, mine) = (c.begin_year?, q.subject_year?);
                    let gap = year.abs_diff(mine);
                    if gap > 6 {
                        return None;
                    }
                    let (genre, mut why) = overlap(&q.subject_tags, &c.tags, TagKind::Genre);
                    if genre == 0.0 {
                        return None;
                    }
                    why.insert(0, format!("formed {year}"));
                    (genre + (6 - gap) as f64 / 6.0, why)
                }
            };
            let k = key_of(&c.name);
            let h = history.get(&k);
            let mut why = why;
            let m = q.matches.get(&k).copied();
            if let Some(m) = m {
                why.insert(0, format!("{}% match", (m * 100.0).round() as u32));
            }
            Some(SimilarRow {
                name: c.name.clone(),
                score: score + m.unwrap_or(0.0) * 0.6,
                why,
                performer_id: h.and_then(|h| h.performer_id),
                album_id: h.and_then(|h| h.album_id),
                image_key: h.and_then(|h| h.image_key.clone()),
                plays: h.map(|h| h.plays).unwrap_or(0),
                in_library: in_library.contains(&k),
            })
        })
        .collect();
    rows.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then(b.plays.cmp(&a.plays)));
    rows.truncate(q.limit);
    rows
}

/// Artists like `artist` by one of the tag kinds ("genre", "style",
/// "scene") or by "era" (formed within six years, sharing a genre).
/// Library: among the tagged artists in the library. Discover: Last.fm's
/// neighbours of the artist that aren't in the library (the best-matched
/// twenty tagged in the background, the ranking sent again every five as
/// "wrapped-similar" events), beside anything else tagged and not in the
/// library. `album` names an album whose own tags stand in for the
/// artist's.
#[tauri::command]
pub async fn wrapped_similar(
    app: AppHandle,
    artist: String,
    album: Option<String>,
    by: String,
    scope: Option<String>,
    limit: Option<usize>,
) -> Result<SimilarAnswer, String> {
    let key = WrappedSettings::load(&app).lastfm_key;
    let tags = app.state::<Tags>();
    let subject = tags.lookup(&app, &artist, &key).await;
    let mut subject_tags = subject.tags.clone();
    if let Some(album) = album.as_deref().filter(|a| !a.trim().is_empty()) {
        let own = tags.lookup_album(&app, &artist, album, &key).await;
        if own.tags.len() >= 3 {
            // The album's sound, with the artist's scene and year behind it
            subject_tags = own.tags;
            subject_tags.extend(subject.tags.iter().filter(|t| t.kind == TagKind::Scene).cloned());
        }
    }
    let kind = match by.as_str() {
        "genre" => Some(TagKind::Genre),
        "style" => Some(TagKind::Style),
        "scene" => Some(TagKind::Scene),
        "era" => None,
        other => return Err(format!("Unknown similarity: {other}")),
    };
    let discover = scope.as_deref() == Some("discover");
    let me = key_of(&artist);
    let mut matches: HashMap<String, f64> = HashMap::new();
    let mut pending: Vec<String> = Vec::new();
    if discover {
        let in_library = app.state::<LibraryCopy>().names(&app);
        for (name, score) in tags.lookup_similar(&app, &artist, &key).await {
            let k = key_of(&name);
            if k == me || in_library.contains(&k) {
                continue;
            }
            matches.insert(k, score);
            let tagged = tags.cached(&app, &name).is_some_and(|t| t.from_lastfm || key.is_empty());
            if !tagged && pending.len() < 20 {
                pending.push(name);
            }
        }
    }
    let query = SimilarQuery { me, subject_tags, subject_year: subject.begin_year, kind, discover, matches, limit: limit.unwrap_or(30) };
    let answer_key = format!("{}|{}|{}|{}", key_of(&artist), album.as_deref().map(key_of).unwrap_or_default(), by, if discover { "discover" } else { "library" });
    let rows = rank_similar(&app, &query);
    let still = pending.len();
    if still > 0 {
        // Tag the neighbours in the background; every five, rank again
        let app2 = app.clone();
        let answer_key2 = answer_key.clone();
        tauri::async_runtime::spawn(async move {
            let tags = app2.state::<Tags>();
            let total = pending.len();
            for (i, name) in pending.into_iter().enumerate() {
                tags.lookup(&app2, &name, &key).await;
                let left = total - i - 1;
                if left == 0 || (i + 1) % 5 == 0 {
                    let rows = rank_similar(&app2, &query);
                    let _ = app2.emit("wrapped-similar", SimilarAnswer { key: answer_key2.clone(), rows, pending: left });
                }
            }
        });
    }
    Ok(SimilarAnswer { key: answer_key, rows, pending: still })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_tags() {
        assert_eq!(classify("Melodic Death Metal"), Some(("melodic death metal".into(), TagKind::Genre)));
        assert_eq!(classify("viking metal"), Some(("viking metal".into(), TagKind::Style)));
        assert_eq!(classify("swedish"), Some(("swedish".into(), TagKind::Scene)));
        assert_eq!(classify("swedish death metal"), Some(("swedish death metal".into(), TagKind::Genre)));
        assert_eq!(classify("epic"), Some(("epic".into(), TagKind::Style)));
        assert_eq!(classify("atmospheric black metal"), Some(("atmospheric black metal".into(), TagKind::Genre)));
        assert_eq!(classify("war metal"), Some(("war metal".into(), TagKind::Style)));
        assert_eq!(classify("seen live"), None);
        assert_eq!(classify("2008"), None);
    }
}
