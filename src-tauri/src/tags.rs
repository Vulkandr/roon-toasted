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

use crate::wrapped::{read_json, write_json, WrappedSettings};

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

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    /// Keyed by the artist's name, lower-cased.
    artists: HashMap<String, ArtistTags>,
}

#[derive(Default)]
pub struct Tags {
    store: Mutex<Option<Store>>,
    /// Serializes the lookups (MusicBrainz wants at most one a second).
    gate: tokio::sync::Mutex<Option<Instant>>,
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
        let mut gate = self.gate.lock().await;
        if let Some(last) = *gate {
            let since = last.elapsed();
            if since < Duration::from_millis(1100) {
                tokio::time::sleep(Duration::from_millis(1100) - since).await;
            }
        }
        let fetched = fetch(name, lastfm_key).await;
        *gate = Some(Instant::now());
        drop(gate);
        self.with_store(app, |s| {
            s.artists.insert(key_of(name), fetched.clone());
        });
        self.save(app);
        fetched
    }

    pub fn count(&self, app: &AppHandle) -> usize {
        self.with_store(app, |s| s.artists.len())
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

/// How many artists are in the tag cache.
#[tauri::command]
pub fn wrapped_tags_count(app: AppHandle) -> usize {
    app.state::<Tags>().count(&app)
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
