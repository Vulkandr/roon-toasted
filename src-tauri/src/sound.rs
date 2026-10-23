// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tier 3: how songs actually sound. Two models from the Music Technology
//! Group at Universitat Pompeu Fabra (Discogs-EffNet, essentia.upf.edu/
//! models; CC BY-NC-SA 4.0, free to use while Toasted is), trained on
//! Discogs: one knows 400 styles ("Atmospheric Black Metal"), the other
//! places songs near the artists they sound like, which is what "close"
//! means here. They're a one-time download (37 MB) into the app data's
//! `sound` folder and run on this PC (audio.rs). A song is heard from your
//! own file when Roon has one, all of it: the Core says where its file is
//! and gives it out from its web server, as for Roon's Export, wherever
//! the music is stored (the Core's own drive, a USB drive, a network
//! share). Else through its 30-second preview (Deezer's, else Apple's).
//! Neither is kept; its print is (the artist model's 512 numbers and the
//! 400 styles, about a kilobyte), in `wrapped-sound.json`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use roon_library::{LibraryClient, TrackFile};

use crate::audio::{Heard, Models, ARTIST_DIMS, STYLES};
use crate::discover::fold;
use crate::dive::DivePick;
use crate::library::Library;
use crate::wrapped::{now_ms, read_json, write_json};

/// Where the models come from (the makers' own model page).
const BASE: &str = "https://essentia.upf.edu/models/feature-extractors/discogs-effnet/";

/// One of the model files: its name, SHA-256 and size.
struct ModelFile {
    name: &'static str,
    sha256: &'static str,
    bytes: u64,
}

const STYLE: ModelFile = ModelFile {
    name: "discogs-effnet-bsdynamic-1.onnx",
    sha256: "a280825b334797cf677939db8cd5762c0392aedd0ca6415dbc1cd083f045e43c",
    bytes: 18_027_718,
};
/// The style model's notes, with its 400 style names.
const NAMES: ModelFile = ModelFile {
    name: "discogs-effnet-bsdynamic-1.json",
    sha256: "a2e85b2e7372d5f8e0f35bdd6aeae1139f101087d183d0b2fb60b0ea0f01a0ff",
    bytes: 14_986,
};
const ARTIST: ModelFile = ModelFile {
    name: "discogs_artist_embeddings-effnet-bs64-1.onnx",
    sha256: "30ddb02faa963ad57fe3020482fad081e167d1ad751625455233012691c0b438",
    bytes: 18_621_951,
};
const FILES: [&ModelFile; 3] = [&STYLE, &NAMES, &ARTIST];

/// Prints made another way (other models, other steps) are dropped when
/// this changes.
const PRINTS_VERSION: u32 = 1;
/// A song without a preview anywhere is looked for again after 30 days.
const RETRY_MS: i64 = 30 * 86_400_000;
/// The models are let go after this long unused (they hold about 150 MB).
const KEEP_MODELS: Duration = Duration::from_secs(120);
/// Songs looked up and fetched at once (heard in groups when an ear is
/// free).
const FETCHING: usize = 6;
/// Your files fetched from the Core and decoded at once (each holds its
/// song at 16 kHz while it's decoded, about 4 MB a minute).
const FILES_AT_ONCE: usize = 3;
/// A file bigger than this (a long hi-res one) is heard from its preview.
const MOST_FILE_BYTES: u64 = 700_000_000;
/// Albums looked for in other copies at most, in one go (see `twins`).
const TWIN_ALBUMS: usize = 16;
const USER_AGENT: &str = concat!("RoonToasted/", env!("CARGO_PKG_VERSION"), " (https://github.com/Vulkandr/roon-toasted)");
const STOPPED: &str = "Stopped.";
const NOT_SET_UP: &str = "Tier 3 isn't set up: its models download from Wrapped's Settings.";

/// A song as kept: its print packed small, or that it has no preview.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Kept {
    /// The artist model's embedding: 512 signed bytes (times `scale`), base64.
    artist: String,
    scale: f32,
    /// The 400 styles, a byte each (the square root, times 255), base64.
    styles: String,
    /// What was heard: a preview ("deezer:136994518", "itunes:1440870245")
    /// or your file, all of it ("core:Bölzer/2016 - Hero/03 - Hero.flac"
    /// from Roon, "file:..." from your music folder).
    from: String,
    /// No preview was found, or it couldn't be heard (looked for again
    /// after RETRY_MS).
    missing: bool,
    at: i64,
    /// When the library's other copies of its album were last looked
    /// through for a file of it and had none (looked through again after
    /// RETRY_MS; 0: not yet).
    looked: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Prints {
    version: u32,
    /// By `key(artist, title)`.
    songs: HashMap<String, Kept>,
}

/// A song as tier 3 heard it.
#[derive(Debug, Clone)]
pub struct Print {
    /// Where it sits among artists: 512 numbers, unit length.
    pub artist: Vec<f32>,
    /// How much it sounds like each of the 400 styles (0 to 1).
    pub styles: Vec<f32>,
    pub from: String,
}

impl Print {
    /// How close two songs sound: 1 alike, around 0.3 nothing alike (the
    /// artist model's cosine).
    pub fn closeness(&self, other: &Print) -> f32 {
        dot(&self.artist, &other.artist)
    }

    /// Its strongest styles by name ("Black Metal"), strongest first.
    pub fn top_styles(&self, names: &[String], n: usize) -> Vec<String> {
        let mut order: Vec<usize> = (0..self.styles.len().min(names.len())).collect();
        order.sort_by(|a, b| self.styles[*b].total_cmp(&self.styles[*a]));
        let mut out: Vec<String> = Vec::new();
        for i in order {
            let name = style_name(&names[i]);
            if !out.contains(&name) {
                out.push(name);
            }
            if out.len() >= n {
                break;
            }
        }
        out
    }
}

/// How close two sounds are: the dot product of two unit-length artist
/// parts (their cosine).
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Several songs' sound together: their artist parts averaged, at unit
/// length (None for none).
pub fn average(prints: &[&Print]) -> Option<Vec<f32>> {
    if prints.is_empty() {
        return None;
    }
    let mut sum = vec![0f32; ARTIST_DIMS];
    for p in prints {
        for (s, v) in sum.iter_mut().zip(&p.artist) {
            *s += v;
        }
    }
    let length = sum.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !(length > 0.0) {
        return None;
    }
    sum.iter_mut().for_each(|v| *v /= length);
    Some(sum)
}

/// "Rock---Black Metal" -> "Black Metal"
fn style_name(full: &str) -> String {
    full.rsplit("---").next().unwrap_or(full).to_string()
}

/// A song to hear: who, what, and its album (to find the right preview).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Song {
    pub artist: String,
    pub title: String,
    pub album: String,
    /// Its Roon handle (this session's), for a song that may be yours: its
    /// file is asked of the Core first (see `Sound::core_files`). None: a
    /// preview only (a song new to you).
    #[serde(skip)]
    pub handle: Option<String>,
}

/// Heard from your own file (not a preview).
pub fn own_file(from: &str) -> bool {
    from.starts_with("core:") || from.starts_with("file:")
}

fn key(artist: &str, title: &str) -> String {
    format!("{}|{}", fold(artist), fold(title))
}

fn album_key(song: &Song) -> String {
    format!("{}|{}", fold(&song.artist), fold(&song.album))
}

fn pack(heard: &Heard, from: String) -> Kept {
    let most = heard.artist.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
    let scale = most / 127.0;
    let artist: Vec<u8> = heard.artist.iter().map(|v| (v / scale).round().clamp(-127.0, 127.0) as i8 as u8).collect();
    let styles: Vec<u8> = heard.styles.iter().map(|v| (v.clamp(0.0, 1.0).sqrt() * 255.0).round() as u8).collect();
    Kept { artist: B64.encode(artist), scale, styles: B64.encode(styles), from, missing: false, at: now_ms(), looked: 0 }
}

fn unpack(kept: &Kept) -> Option<Print> {
    let artist = B64.decode(&kept.artist).ok().filter(|a| a.len() == ARTIST_DIMS)?;
    let mut artist: Vec<f32> = artist.iter().map(|b| *b as i8 as f32 * kept.scale).collect();
    let length = artist.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !(length > 0.0) {
        return None;
    }
    artist.iter_mut().for_each(|v| *v /= length);
    let styles = B64.decode(&kept.styles).ok().filter(|s| s.len() == STYLES)?;
    let styles = styles.iter().map(|b| (*b as f32 / 255.0).powi(2)).collect();
    Some(Print { artist, styles, from: kept.from.clone() })
}

/// Where a song's preview is.
#[derive(Debug, Clone)]
struct Preview {
    url: String,
    /// "deezer:<id>" or "itunes:<id>"
    from: String,
    ext: &'static str,
}

/// A title without its version ("Hero (Live)", "Hero - Remastered 2019":
/// "hero"), folded.
pub(crate) fn base_title(title: &str) -> String {
    fold(short_title(title))
}

/// A title without its version, as written ("Ayahuasca (Original Mix)":
/// "Ayahuasca").
fn short_title(title: &str) -> &str {
    let mut t = title;
    for cut in [" (", " [", " - ", " – "] {
        if let Some(at) = t.find(cut).filter(|at| *at > 0) {
            t = &t[..at];
        }
    }
    t.trim()
}

/// How well a found song's names fit the one wanted (None: not it). Its
/// artist the same, its title the same (more) or the same but for its
/// version (less, and less again for a live take or such that wasn't
/// asked for), its album the same (more).
fn fit(want: &Song, artist: &str, title: &str, short: &str, album: &str) -> Option<i32> {
    let (fa, ft) = (fold(&want.artist), fold(&want.title));
    if fold(artist) != fa || ft.is_empty() {
        return None;
    }
    let full = fold(title);
    let mut score = if full == ft {
        4
    } else if (!short.is_empty() && fold(short) == ft) || base_title(title) == base_title(&want.title) {
        2
    } else {
        return None;
    };
    let lower = title.to_lowercase();
    let asked = want.title.to_lowercase();
    if score < 4 && ["live", "demo", "remix", "acoustic", "instrumental", "rehearsal", "edit"].iter().any(|w| lower.contains(w) && !asked.contains(w)) {
        score -= 1;
    }
    let (fal, a) = (fold(&want.album), fold(album));
    if !fal.is_empty() && !a.is_empty() {
        if a == fal {
            score += 2;
        } else if a.starts_with(&fal) || fal.starts_with(&a) {
            score += 1;
        }
    }
    Some(score)
}

/// The best fit among Deezer's songs (a search's, or an album's list).
fn deezer_pick(tracks: &[Value], want: &Song) -> Option<Preview> {
    let mut best: Option<(i32, Preview)> = None;
    for t in tracks {
        let url = t["preview"].as_str().unwrap_or("");
        if url.is_empty() {
            continue;
        }
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        let Some(score) = fit(want, &s(&t["artist"]["name"]), &s(&t["title"]), &s(&t["title_short"]), &s(&t["album"]["title"])) else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            let id = t["id"].as_u64().map(|id| id.to_string()).unwrap_or_default();
            best = Some((score, Preview { url: url.to_string(), from: format!("deezer:{id}"), ext: "mp3" }));
        }
    }
    best.map(|(_, p)| p)
}

/// The best fit among Apple's songs.
fn itunes_pick(results: &[Value], want: &Song) -> Option<Preview> {
    let mut best: Option<(i32, Preview)> = None;
    for r in results {
        let url = r["previewUrl"].as_str().unwrap_or("");
        if url.is_empty() {
            continue;
        }
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        let Some(score) = fit(want, &s(&r["artistName"]), &s(&r["trackName"]), "", &s(&r["collectionName"])) else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            let id = r["trackId"].as_u64().map(|id| id.to_string()).unwrap_or_default();
            best = Some((score, Preview { url: url.to_string(), from: format!("itunes:{id}"), ext: "m4a" }));
        }
    }
    best.map(|(_, p)| p)
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())
}

fn folder(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("sound"))
}

fn prints_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-sound.json"))
}

/// A model file there, whole (by its size; its SHA-256 was checked when it
/// came).
fn whole(dir: &Path, file: &ModelFile) -> bool {
    std::fs::metadata(dir.join(file.name)).is_ok_and(|m| m.len() == file.bytes)
}

fn have_all(dir: &Path) -> bool {
    FILES.iter().all(|f| whole(dir, f))
}

pub struct Sound {
    /// The models while they're in use, and when they last were.
    models: Mutex<Option<(Arc<Models>, Instant)>>,
    names: Mutex<Option<Arc<Vec<String>>>>,
    loading: tokio::sync::Mutex<()>,
    prints: Mutex<Option<Prints>>,
    /// One gate per service: Deezer, Apple.
    deezer: tokio::sync::Mutex<Option<Instant>>,
    itunes: tokio::sync::Mutex<Option<Instant>>,
    /// Groups of songs heard at once (each takes about 150 MB and a core
    /// while it runs).
    ears: Arc<tokio::sync::Semaphore>,
    workers: usize,
    /// Your files fetched and decoded at once.
    files: Arc<tokio::sync::Semaphore>,
}

impl Default for Sound {
    fn default() -> Self {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let workers = (threads / 3).clamp(1, 5);
        Sound {
            models: Mutex::new(None),
            names: Mutex::new(None),
            loading: tokio::sync::Mutex::new(()),
            prints: Mutex::new(None),
            deezer: tokio::sync::Mutex::new(None),
            itunes: tokio::sync::Mutex::new(None),
            ears: Arc::new(tokio::sync::Semaphore::new(workers)),
            workers,
            files: Arc::new(tokio::sync::Semaphore::new(FILES_AT_ONCE)),
        }
    }
}

impl Sound {
    /// Tier 3 is set up: the models are here.
    pub fn ready(app: &AppHandle) -> bool {
        folder(app).is_some_and(|dir| have_all(&dir))
    }

    /// The models, loaded when they aren't (and let go again once unused
    /// for a while).
    async fn models(&self, app: &AppHandle) -> Result<Arc<Models>, String> {
        let _one = self.loading.lock().await;
        if let Some((models, used)) = self.models.lock().unwrap().as_mut() {
            *used = Instant::now();
            return Ok(models.clone());
        }
        let dir = folder(app).ok_or("No app data folder.")?;
        if !have_all(&dir) {
            return Err(NOT_SET_UP.into());
        }
        let models = tauri::async_runtime::spawn_blocking(move || Models::load(&dir.join(STYLE.name), &dir.join(ARTIST.name)))
            .await
            .map_err(|e| e.to_string())??;
        let models = Arc::new(models);
        *self.models.lock().unwrap() = Some((models.clone(), Instant::now()));
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let gone = {
                    let sound = app.state::<Sound>();
                    let mut held = sound.models.lock().unwrap();
                    let idle = held.as_ref().map(|(m, used)| used.elapsed() >= KEEP_MODELS && Arc::strong_count(m) == 1);
                    if idle == Some(true) {
                        *held = None;
                    }
                    idle != Some(false)
                };
                if gone {
                    break;
                }
            }
        });
        Ok(models)
    }

    /// The 400 style names (read once from the style model's notes).
    pub fn names(&self, app: &AppHandle) -> Result<Arc<Vec<String>>, String> {
        let mut held = self.names.lock().unwrap();
        if let Some(names) = held.as_ref() {
            return Ok(names.clone());
        }
        let dir = folder(app).ok_or("No app data folder.")?;
        let names = Arc::new(crate::audio::style_names(&dir.join(NAMES.name))?);
        *held = Some(names.clone());
        Ok(names)
    }

    fn touch(&self) {
        if let Some((_, used)) = self.models.lock().unwrap().as_mut() {
            *used = Instant::now();
        }
    }

    fn with_prints<R>(&self, app: &AppHandle, f: impl FnOnce(&mut Prints) -> R) -> R {
        let mut guard = self.prints.lock().unwrap();
        let prints = guard.get_or_insert_with(|| {
            let mut p: Prints = prints_path(app).and_then(|p| read_json(&p)).unwrap_or_default();
            if p.version != PRINTS_VERSION {
                p = Prints { version: PRINTS_VERSION, songs: HashMap::new() };
            }
            p
        });
        f(prints)
    }

    fn kept(&self, app: &AppHandle, key: &str) -> Option<Kept> {
        self.with_prints(app, |p| p.songs.get(key).cloned())
    }

    fn save(&self, app: &AppHandle) {
        let Some(path) = prints_path(app) else { return };
        let guard = self.prints.lock().unwrap();
        if let Some(prints) = guard.as_ref() {
            write_json(&path, prints);
        }
    }

    /// A kept print for the song, if it was heard before.
    pub fn print_kept(&self, app: &AppHandle, artist: &str, title: &str) -> Option<Print> {
        self.kept(app, &key(artist, title)).filter(|k| !k.missing).and_then(|k| unpack(&k))
    }

    async fn turn(gate: &tokio::sync::Mutex<Option<Instant>>, every: Duration) {
        let mut last = gate.lock().await;
        if let Some(at) = *last {
            let since = at.elapsed();
            if since < every {
                tokio::time::sleep(every - since).await;
            }
        }
        *last = Some(Instant::now());
    }

    /// One call to Deezer's API (50 calls in 5 s at most: one every 120 ms
    /// here, and a wait when it says that's too many).
    async fn deezer(&self, client: &reqwest::Client, url: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        for attempt in 0..2 {
            Self::turn(&self.deezer, Duration::from_millis(120)).await;
            let v: Value = client
                .get(url)
                .query(query)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            match v["error"]["code"].as_i64() {
                None if v.get("error").is_none() => return Ok(v),
                // (too many calls)
                Some(4) if attempt == 0 => tokio::time::sleep(Duration::from_secs(5)).await,
                _ => return Err(v["error"]["message"].as_str().unwrap_or("Deezer said no").to_string()),
            }
        }
        Err("Deezer is busy".into())
    }

    /// An album's songs on Deezer, with their previews: a song of it found
    /// there without a search of its own.
    async fn deezer_album(&self, client: &reqwest::Client, artist: &str, album: &str) -> Option<Vec<Value>> {
        let q = format!("{artist} {album}");
        let found = self.deezer(client, "https://api.deezer.com/search/album", &[("q", q.as_str()), ("limit", "25")]).await.ok()?;
        let (fa, fal, base) = (fold(artist), fold(album), base_title(album));
        let mut best: Option<(i32, &Value)> = None;
        for a in found["data"].as_array()? {
            if fold(a["artist"]["name"].as_str().unwrap_or("")) != fa {
                continue;
            }
            let title = a["title"].as_str().unwrap_or("");
            let score = if fold(title) == fal {
                2
            } else if base_title(title) == base {
                1
            } else {
                continue;
            };
            if best.is_none_or(|(b, _)| score > b) {
                best = Some((score, a));
            }
        }
        let (_, chosen) = best?;
        let id = chosen["id"].as_u64()?;
        let tracks = self.deezer(client, &format!("https://api.deezer.com/album/{id}/tracks"), &[("limit", "200")]).await.ok()?;
        let mut list = tracks["data"].as_array()?.clone();
        // (the album's title on each, as a search's songs have it)
        for t in &mut list {
            t["album"] = serde_json::json!({ "title": chosen["title"] });
        }
        Some(list)
    }

    /// A song's preview: Deezer's (a plain search, then by artist and
    /// title), else Apple's. Err when a service couldn't be reached (so
    /// it's not marked as having none).
    async fn find(&self, client: &reqwest::Client, song: &Song) -> Result<Option<Preview>, String> {
        let mut unreached = None;
        let plain = format!("{} {}", song.artist, song.title);
        let exact = format!("artist:\"{}\" track:\"{}\"", song.artist, song.title);
        let mut queries = vec![plain, exact];
        // (a version in the title, "(Original Mix)" and such, finds nothing
        // on Deezer: the title without it too; `fit` still prefers the
        // same version when there is one)
        let short = short_title(&song.title);
        if !short.is_empty() && short != song.title.trim() {
            queries.push(format!("{} {short}", song.artist));
        }
        for q in queries {
            match self.deezer(client, "https://api.deezer.com/search", &[("q", q.as_str()), ("limit", "25")]).await {
                Ok(v) => {
                    if let Some(p) = deezer_pick(v["data"].as_array().map(|a| a.as_slice()).unwrap_or(&[]), song) {
                        return Ok(Some(p));
                    }
                }
                Err(e) => unreached = Some(e),
            }
        }
        // Apple's search: about 20 calls a minute, so one every 3 s
        Self::turn(&self.itunes, Duration::from_secs(3)).await;
        let term = format!("{} {}", song.artist, if short.is_empty() { song.title.as_str() } else { short });
        let asked = client
            .get("https://itunes.apple.com/search")
            .query(&[("term", term.as_str()), ("media", "music"), ("entity", "song"), ("limit", "25")])
            .send()
            .await
            .and_then(|r| r.error_for_status());
        match asked {
            Ok(r) => match r.json::<Value>().await {
                Ok(v) => {
                    if let Some(p) = itunes_pick(v["results"].as_array().map(|a| a.as_slice()).unwrap_or(&[]), song) {
                        return Ok(Some(p));
                    }
                }
                Err(e) => unreached = Some(e.to_string()),
            },
            Err(e) => unreached = Some(e.to_string()),
        }
        match unreached {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }

    /// Prints for the songs, in their order (None: not heard, no file of
    /// it nor a preview found): the kept ones, the rest heard now, from
    /// your files where Roon has them (also songs heard from a preview
    /// before), else from previews. An album that several previews come
    /// from is looked up once. They're fetched a chunk at a time while the
    /// chunk before is heard, in groups that fill one of the artist model's
    /// batches (four previews, or two of your files), `workers` groups at
    /// once. `tick(done, total)` as the new ones come.
    pub async fn prints(&self, app: &AppHandle, songs: &[Song], tick: &(dyn Fn(usize, usize) + Send + Sync)) -> Result<Vec<Option<Print>>, String> {
        if !Sound::ready(app) {
            return Err(NOT_SET_UP.into());
        }
        let now = now_ms();
        // Each song once: to hear (not heard yet, or a while since nothing
        // of it was found), or heard from a preview and maybe yours
        let mut wanted: Vec<(String, Song)> = Vec::new();
        let mut maybe: Vec<(String, Song)> = Vec::new();
        // (heard from a preview, not Roon's: maybe in your music folder)
        let mut maybe_folder: Vec<(String, Song)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for s in songs {
            if fold(&s.artist).is_empty() || fold(&s.title).is_empty() {
                continue;
            }
            let k = key(&s.artist, &s.title);
            if !seen.insert(k.clone()) {
                continue;
            }
            match self.kept(app, &k) {
                Some(kept) if !kept.missing && own_file(&kept.from) => {}
                Some(kept) if !kept.missing || now - kept.at < RETRY_MS => {
                    if s.handle.is_some() {
                        maybe.push((k, s.clone()));
                    } else {
                        maybe_folder.push((k, s.clone()));
                    }
                }
                _ => wanted.push((k, s.clone())),
            }
        }
        // Your files, where Roon has them
        let asked: Vec<(String, Song)> = wanted.iter().chain(&maybe).filter(|(_, s)| s.handle.is_some()).cloned().collect();
        let (mut files, looked) = if asked.is_empty() { (HashMap::new(), Vec::new()) } else { self.core_files(app, &asked, now).await };
        if !asked.is_empty() {
            println!("[sound] your files: {} of the {} songs that may be yours, through Roon", files.len(), asked.len());
        }
        // Your music folder (Settings, "Manually choose music folder") for
        // the songs Roon didn't hand over
        let folder = if wanted.is_empty() && maybe.is_empty() && maybe_folder.is_empty() { None } else { crate::musicfolder::ready(app).await };
        let local_of = |s: &Song| folder.as_ref().and_then(|_| crate::musicfolder::find(&s.artist, &s.title, &s.album, None));
        let mut jobs: Vec<Job> = wanted
            .into_iter()
            .map(|(key, song)| {
                let file = files.remove(&key);
                let local = if file.is_none() { local_of(&song) } else { None };
                Job { file, local, key, song }
            })
            .collect();
        jobs.extend(maybe.into_iter().chain(maybe_folder).filter_map(|(key, song)| match files.remove(&key) {
            Some(file) => Some(Job { key, song, file: Some(file), local: None }),
            None => local_of(&song).map(|local| Job { key, song, file: None, local: Some(local) }),
        }));
        if folder.is_some() {
            let n = jobs.iter().filter(|j| j.local.is_some()).count();
            if n > 0 {
                println!("[sound] your music folder: {n} songs found there");
            }
        }
        let total = jobs.len();
        tick(0, total);
        if total == 0 {
            self.mark_looked(app, &looked, now);
            return Ok(songs.iter().map(|s| self.print_kept(app, &s.artist, &s.title)).collect());
        }
        let models = self.models(app).await?;
        let client = http()?;
        let core = core_http()?;
        // The albums two or more previews come from: their lists, once each
        let mut per_album: HashMap<String, usize> = HashMap::new();
        for j in jobs.iter().filter(|j| j.file.is_none()) {
            if !fold(&j.song.album).is_empty() {
                *per_album.entry(album_key(&j.song)).or_default() += 1;
            }
        }
        let mut lists: HashMap<String, Arc<Vec<Value>>> = HashMap::new();
        for j in jobs.iter().filter(|j| j.file.is_none()) {
            let k = album_key(&j.song);
            if per_album.get(&k).copied().unwrap_or(0) >= 2 && !lists.contains_key(&k) {
                let list = self.deezer_album(&client, &j.song.artist, &j.song.album).await.unwrap_or_default();
                lists.insert(k, Arc::new(list));
            }
        }
        let done = std::sync::atomic::AtomicUsize::new(0);
        let count = |n: usize| tick(done.fetch_add(n, Ordering::SeqCst) + n, total);
        let chunks: Vec<Vec<Job>> = jobs.chunks(self.workers * GROUP_SONGS * 2).map(|c| c.to_vec()).collect();
        let mut fetched = fetch_all(app, &client, &core, &chunks[0], &lists).await;
        for at in 0..chunks.len() {
            let now_heard = std::mem::take(&mut fetched);
            let (kept, next) = tokio::join!(self.hear_all(models.clone(), now_heard, &count), async {
                match chunks.get(at + 1) {
                    Some(chunk) => fetch_all(app, &client, &core, chunk, &lists).await,
                    None => Vec::new(),
                }
            });
            // (a song heard from a preview keeps when it was last looked for
            // on disk)
            self.with_prints(app, |p| {
                for (k, mut new) in kept {
                    if let Some(old) = p.songs.get(&k) {
                        new.looked = old.looked;
                    }
                    p.songs.insert(k, new);
                }
            });
            self.save(app);
            fetched = next;
        }
        self.mark_looked(app, &looked, now);
        self.touch();
        Ok(songs.iter().map(|s| self.print_kept(app, &s.artist, &s.title)).collect())
    }

    /// Notes that the songs' albums' other copies were looked through just
    /// now, for nothing (not looked through again for a while).
    fn mark_looked(&self, app: &AppHandle, keys: &[String], now: i64) {
        if keys.is_empty() {
            return;
        }
        self.with_prints(app, |p| {
            for k in keys {
                if let Some(kept) = p.songs.get_mut(k) {
                    kept.looked = now;
                }
            }
        });
        self.save(app);
    }

    /// Your files of the songs, where Roon has them, by song key: the Core
    /// asked by the songs' Roon handles where their tracks' files are (as
    /// for Roon's Export), then, for songs whose track has none (a
    /// streaming service's copy in the library), the same song on another
    /// copy of its album in the library (`twins`; not again for a while
    /// for a song that had none). Also the songs looked for that way for
    /// nothing.
    async fn core_files(&self, app: &AppHandle, songs: &[(String, Song)], now: i64) -> (HashMap<String, CoreFile>, Vec<String>) {
        let mut out = HashMap::new();
        let (Some(web), Some(client)) = (core_web(app), app.state::<Library>().client()) else {
            return (out, Vec::new());
        };
        let handles: Vec<String> = songs.iter().map(|(_, s)| s.handle.clone().unwrap_or_default()).collect();
        let found = match client.track_files(&handles).await {
            Ok(found) => found,
            Err(e) => {
                println!("[sound] Roon didn't say where your files are ({e}), so the songs are heard from previews");
                return (out, Vec::new());
            }
        };
        let mut odd: Vec<String> = Vec::new();
        let mut without: Vec<&(String, Song)> = Vec::new();
        // (a file the Core doesn't give out is as good as none)
        for (song, file) in songs.iter().zip(found) {
            match file.filter(|f| !f.http_path.is_empty()).map(|f| core_file(&web, &f)) {
                Some(Ok(file)) => {
                    out.insert(song.0.clone(), file);
                }
                Some(Err(ext)) => odd.push(ext),
                None => without.push(song),
            }
        }
        // (TWIN_ALBUMS albums at a time; the rest's songs next time)
        let mut albums: Vec<String> = Vec::new();
        let ask: Vec<(String, Song)> = without
            .into_iter()
            .filter(|(k, s)| !fold(&s.album).is_empty() && self.kept(app, k).is_none_or(|kept| now - kept.looked >= RETRY_MS))
            .filter(|(_, s)| {
                let album = album_key(s);
                if albums.contains(&album) {
                    return true;
                }
                if albums.len() >= TWIN_ALBUMS {
                    return false;
                }
                albums.push(album);
                true
            })
            .cloned()
            .collect();
        let mut looked = Vec::new();
        if !ask.is_empty() {
            let (twins, searched) = twins(&client, ask.clone()).await;
            for (k, _) in &ask {
                match twins.get(k).filter(|f| !f.http_path.is_empty()).map(|f| core_file(&web, f)) {
                    Some(Ok(file)) => {
                        out.insert(k.clone(), file);
                    }
                    Some(Err(ext)) => odd.push(ext),
                    None if searched.contains(k) => looked.push(k.clone()),
                    None => {}
                }
            }
        }
        if !odd.is_empty() {
            odd.sort();
            odd.dedup();
            println!("[sound] some of your files are {} files, which can't be heard yet: their previews instead", odd.join(", ").to_uppercase());
        }
        (out, looked)
    }

    /// Hears the fetched songs in groups (up to 64 patches each, so four
    /// previews), `workers` groups at once; what to keep of each, and of
    /// the ones without a preview. `count(n)` as songs are done.
    async fn hear_all(&self, models: Arc<Models>, fetched: Vec<Fetched>, count: &(dyn Fn(usize) + Sync)) -> Vec<(String, Kept)> {
        let mut out: Vec<(String, Kept)> = Vec::new();
        let mut groups: Vec<Vec<Ready>> = Vec::new();
        let mut settled = 0;
        for f in fetched {
            match f {
                Fetched::Ready(ready) => {
                    let fits = groups.last().is_some_and(|g| g.iter().map(|r| r.patches.count()).sum::<usize>() + ready.patches.count() <= crate::audio::ARTIST_BATCH);
                    if fits {
                        groups.last_mut().unwrap().push(ready);
                    } else {
                        groups.push(vec![ready]);
                    }
                }
                Fetched::Missing(k, kept) => {
                    out.push((k, kept));
                    settled += 1;
                }
                Fetched::Unreached => settled += 1,
            }
        }
        if settled > 0 {
            count(settled);
        }
        let mut set = tokio::task::JoinSet::new();
        for group in groups {
            let (ears, models) = (self.ears.clone(), models.clone());
            set.spawn(async move {
                let _ear = ears.acquire_owned().await;
                let mut keys = Vec::new();
                let mut froms = Vec::new();
                let mut patches = Vec::new();
                for r in group {
                    keys.push(r.key);
                    froms.push(r.from);
                    patches.push(r.patches);
                }
                let heard = tauri::async_runtime::spawn_blocking(move || models.hear_many(&patches))
                    .await
                    .unwrap_or_else(|e| keys.iter().map(|_| Err(e.to_string())).collect());
                keys.into_iter()
                    .zip(froms)
                    .zip(heard)
                    .map(|((k, from), heard)| match heard {
                        Ok(heard) => (k, pack(&heard, from)),
                        Err(e) => {
                            println!("[sound] {k}: couldn't hear {from} ({e})");
                            (k, Kept { missing: true, from, at: now_ms(), ..Default::default() })
                        }
                    })
                    .collect::<Vec<_>>()
            });
        }
        while let Some(finished) = set.join_next().await {
            if let Ok(list) = finished {
                count(list.len());
                out.extend(list);
            }
        }
        out
    }
}

/// Songs to a group heard together: four previews (15 patches each) fill
/// one of the artist model's batches of 64 (two of your files, 32 each).
const GROUP_SONGS: usize = 4;

/// A song to hear now, and your file of it when Roon has one.
#[derive(Clone)]
struct Job {
    key: String,
    song: Song,
    file: Option<CoreFile>,
    /// The song's file in your music folder (musicfolder.rs).
    local: Option<PathBuf>,
}

/// Where to fetch one of your files from the Core.
#[derive(Debug, Clone)]
struct CoreFile {
    url: String,
    /// Its type, from its name ("flac").
    ext: String,
    /// "core:" and the end of its path on the Core
    from: String,
}

/// The Core's web address ("http://192.168.0.68:9330"): where its
/// extensions connect, and where it gives out its files.
fn core_web(app: &AppHandle) -> Option<String> {
    let core = app.state::<crate::roon::Roon>().status().connected.filter(|c| !c.host.is_empty())?;
    let port = if core.port != 0 {
        core.port
    } else {
        crate::settings::path(app).map(|p| crate::settings::load(&p).manual_core_port).filter(|p| *p != 0).unwrap_or(crate::settings::DEFAULT_CORE_PORT)
    };
    let host = if core.host.contains(':') { format!("[{}]", core.host) } else { core.host };
    Some(format!("http://{host}:{port}"))
}

/// For your files: the Core is on the local network (no proxy), and a file
/// takes a while longer than a preview.
fn core_http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())
}

/// Where to fetch a track's file from the Core (at its web address), or,
/// for a file of a type that can't be heard yet, its type.
fn core_file(web: &str, file: &TrackFile) -> Result<CoreFile, String> {
    let name = file.file_path.rsplit(['/', '\\']).next().unwrap_or("");
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if !crate::audio::can_decode(&ext) {
        return Err(if ext.is_empty() { "unnamed".into() } else { ext });
    }
    let url = if file.http_path.starts_with("http") {
        file.http_path.clone()
    } else {
        format!("{web}{}{}", if file.http_path.starts_with('/') { "" } else { "/" }, file.http_path)
    };
    // (its artist's folder, album's folder and name, when it has them)
    let parts: Vec<&str> = file.file_path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let from = format!("core:{}", parts[parts.len().saturating_sub(3)..].join("/"));
    Ok(CoreFile { url, ext, from })
}

/// The files of songs whose own track in the library has none, found on
/// another copy of their album there: Roon keeps a streaming service's
/// copy of an album you also have on disk as a hidden duplicate, and a
/// search can land on its songs. One search per album (four at once), its
/// copies in the library looked through for the song by title, and those
/// asked for their files. By song key; also the songs whose album was
/// searched (a search that failed is tried again next time).
async fn twins(client: &LibraryClient, songs: Vec<(String, Song)>) -> (HashMap<String, TrackFile>, HashSet<String>) {
    let mut albums: Vec<(String, Vec<(String, Song)>)> = Vec::new();
    for ks in songs {
        let ak = album_key(&ks.1);
        match albums.iter_mut().find(|(a, _)| *a == ak) {
            Some((_, list)) => list.push(ks),
            None => albums.push((ak, vec![ks])),
        }
    }
    let gate = Arc::new(tokio::sync::Semaphore::new(4));
    let mut set = tokio::task::JoinSet::new();
    for (_, list) in albums {
        let (client, gate) = (client.clone(), gate.clone());
        set.spawn(async move {
            let _turn = gate.acquire_owned().await;
            let first = &list[0].1;
            let hits = client.search(&format!("{} {}", first.artist, first.album), 10).await.ok()?;
            let (artist, album, base) = (fold(&first.artist), fold(&first.album), base_title(&first.album));
            let copies: Vec<String> = hits
                .albums
                .iter()
                .filter(|a| a.in_library && fold(&a.artist).contains(&artist) && (fold(&a.title) == album || base_title(&a.title) == base))
                .take(4)
                .map(|a| a.handle.clone())
                .collect();
            let mut candidates: Vec<(String, String)> = Vec::new();
            for copy in copies {
                let Ok(tracks) = client.album_tracks_for("", &copy).await else {
                    continue;
                };
                for (k, s) in &list {
                    let (title, base) = (fold(&s.title), base_title(&s.title));
                    for t in tracks.iter().filter(|t| t.in_library && (fold(&t.title) == title || base_title(&t.title) == base)) {
                        if s.handle.as_ref() != Some(&t.handle) {
                            candidates.push((k.clone(), t.handle.clone()));
                        }
                    }
                }
            }
            Some((list.into_iter().map(|(k, _)| k).collect::<Vec<String>>(), candidates))
        });
    }
    let mut searched: HashSet<String> = HashSet::new();
    let mut candidates: Vec<(String, String)> = Vec::new();
    while let Some(finished) = set.join_next().await {
        if let Ok(Some((keys, found))) = finished {
            searched.extend(keys);
            candidates.extend(found);
        }
    }
    let mut out = HashMap::new();
    if candidates.is_empty() {
        return (out, searched);
    }
    let handles: Vec<String> = candidates.iter().map(|(_, h)| h.clone()).collect();
    match client.track_files(&handles).await {
        Ok(files) => {
            for ((k, _), file) in candidates.into_iter().zip(files) {
                if let Some(file) = file {
                    out.entry(k).or_insert(file);
                }
            }
        }
        // (asked again next time)
        Err(_) => searched.clear(),
    }
    (out, searched)
}

/// A temporary file to hold one of your songs while it's heard (any left
/// behind by an earlier run of Toasted cleared the first time).
fn temp_path(ext: &str) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    static CLEARED: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join("roon-toasted-sound");
    CLEARED.call_once(|| {
        let _ = std::fs::remove_dir_all(&dir);
    });
    std::fs::create_dir_all(&dir).map_err(|e| format!("no temporary folder for it ({e})"))?;
    Ok(dir.join(format!("{}-{}.{ext}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))))
}

/// One of your files fetched from the Core into a temporary file
/// (FILES_AT_ONCE at a time) and heard whole; the temporary file is gone
/// after.
async fn hear_core_file(app: &AppHandle, core: &reqwest::Client, file: &CoreFile) -> Result<crate::audio::Patches, String> {
    let gate = app.state::<Sound>().files.clone();
    let _turn = gate.acquire_owned().await.map_err(|e| e.to_string())?;
    let path = temp_path(&file.ext)?;
    let result = async {
        let mut response = core.get(&file.url).send().await.and_then(|r| r.error_for_status()).map_err(|e| format!("the Core didn't give it ({e})"))?;
        if response.content_length().is_some_and(|n| n > MOST_FILE_BYTES) {
            return Err("it's too big".to_string());
        }
        let mut out = std::fs::File::create(&path).map_err(|e| format!("couldn't keep it for a moment ({e})"))?;
        let mut got = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|e| format!("it stopped coming ({e})"))? {
            got += chunk.len() as u64;
            if got > MOST_FILE_BYTES {
                return Err("it's too big".to_string());
            }
            out.write_all(&chunk).map_err(|e| format!("couldn't keep it for a moment ({e})"))?;
        }
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        let (at, ext) = (path.clone(), file.ext.clone());
        tauri::async_runtime::spawn_blocking(move || crate::audio::hear_file(&at, &ext)).await.map_err(|e| e.to_string())?
    }
    .await;
    let _ = std::fs::remove_file(&path);
    result
}

/// A file in your music folder heard whole (FILES_AT_ONCE at a time, like
/// the Core's).
async fn hear_local_file(app: &AppHandle, path: &Path) -> Result<crate::audio::Patches, String> {
    let gate = app.state::<Sound>().files.clone();
    let _turn = gate.acquire_owned().await.map_err(|e| e.to_string())?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MOST_FILE_BYTES) {
        return Err("it's too big".into());
    }
    let at = path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || crate::audio::hear_file(&at, &ext)).await.map_err(|e| e.to_string())?
}

/// A song found and fetched, made ready to hear.
struct Ready {
    key: String,
    from: String,
    patches: crate::audio::Patches,
}

/// A song after its file or preview was looked for: ready to hear, without
/// either (kept as such), or a service couldn't be reached (tried again
/// next time).
enum Fetched {
    Ready(Ready),
    Missing(String, Kept),
    Unreached,
}

/// The chunk's songs found, fetched and made ready, FETCHING at a time.
async fn fetch_all(app: &AppHandle, client: &reqwest::Client, core: &reqwest::Client, chunk: &[Job], lists: &HashMap<String, Arc<Vec<Value>>>) -> Vec<Fetched> {
    let gate = Arc::new(tokio::sync::Semaphore::new(FETCHING));
    let mut set = tokio::task::JoinSet::new();
    for job in chunk.iter().cloned() {
        let (app, client, core, gate, list) = (app.clone(), client.clone(), core.clone(), gate.clone(), lists.get(&album_key(&job.song)).cloned());
        set.spawn(async move {
            let _turn = gate.acquire_owned().await;
            fetch_one(&app, &client, &core, job, list).await
        });
    }
    let mut out = Vec::new();
    while let Some(finished) = set.join_next().await {
        if let Ok(f) = finished {
            out.push(f);
        }
    }
    out
}

/// One song made ready to hear: your file of it, fetched from the Core and
/// decoded, when Roon has one; else its preview found (the album's list
/// first, when there is one), fetched and decoded.
async fn fetch_one(app: &AppHandle, client: &reqwest::Client, core: &reqwest::Client, job: Job, list: Option<Arc<Vec<Value>>>) -> Fetched {
    let Job { key, song, file, mut local } = job;
    if let Some(file) = file {
        match hear_core_file(app, core, &file).await {
            Ok(patches) if patches.count() > 0 => return Fetched::Ready(Ready { key, from: file.from, patches }),
            Ok(_) => println!("[sound] {} - {}: your file is too short to hear", song.artist, song.title),
            Err(e) => println!("[sound] {} - {}: your file couldn't be had from Roon ({e})", song.artist, song.title),
        }
        // The same file in your music folder, by its path on the Core
        if crate::musicfolder::ready(app).await.is_some() {
            local = crate::musicfolder::find(&song.artist, &song.title, &song.album, file.from.strip_prefix("core:"));
        }
    }
    if let Some(path) = local {
        match hear_local_file(app, &path).await {
            Ok(patches) if patches.count() > 0 => {
                let parts: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
                return Fetched::Ready(Ready { key, from: format!("file:{}", parts[parts.len().saturating_sub(3)..].join("/")), patches });
            }
            Ok(_) => println!("[sound] {} - {}: your file is too short to hear", song.artist, song.title),
            Err(e) => println!("[sound] {} - {}: your file in the music folder couldn't be heard ({e})", song.artist, song.title),
        }
    }
    let sound = app.state::<Sound>();
    let found = match list.as_deref().and_then(|l| deezer_pick(l, &song)) {
        Some(p) => Ok(Some(p)),
        None => sound.find(client, &song).await,
    };
    let preview = match found {
        Ok(Some(p)) => p,
        Ok(None) => {
            println!("[sound] no preview of {} - {}", song.artist, song.title);
            return Fetched::Missing(key, Kept { missing: true, at: now_ms(), ..Default::default() });
        }
        Err(e) => {
            println!("[sound] {} - {}: {e}", song.artist, song.title);
            return Fetched::Unreached;
        }
    };
    let bytes = match client.get(&preview.url).send().await.and_then(|r| r.error_for_status()) {
        Ok(r) => match r.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                println!("[sound] {} - {}: the preview didn't come ({e})", song.artist, song.title);
                return Fetched::Unreached;
            }
        },
        Err(e) => {
            println!("[sound] {} - {}: the preview didn't come ({e})", song.artist, song.title);
            return Fetched::Unreached;
        }
    };
    let ext = preview.ext;
    let made = tauri::async_runtime::spawn_blocking(move || -> Result<crate::audio::Patches, String> {
        let (mono, rate) = crate::audio::decode(bytes, ext)?;
        Ok(crate::audio::prepare(&crate::audio::resample(&mono, rate), crate::audio::PATCH_EVERY))
    })
    .await;
    match made {
        Ok(Ok(patches)) if patches.count() > 0 => Fetched::Ready(Ready { key, from: preview.from, patches }),
        Ok(Ok(_)) => {
            println!("[sound] {} - {}: too short to hear", song.artist, song.title);
            Fetched::Missing(key, Kept { missing: true, from: preview.from, at: now_ms(), ..Default::default() })
        }
        Ok(Err(e)) => {
            println!("[sound] {} - {}: couldn't decode {} ({e})", song.artist, song.title, preview.from);
            Fetched::Missing(key, Kept { missing: true, from: preview.from, at: now_ms(), ..Default::default() })
        }
        Err(_) => Fetched::Unreached,
    }
}

// ----- The download (Wrapped's Settings) -----

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static PROGRESS: Mutex<Option<SoundProgress>> = Mutex::new(None);

/// The download as the page shows it ("sound-download" events).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundProgress {
    /// "download", "check" (loading them once they're here), "done",
    /// "stopped" or "error"
    pub stage: String,
    pub done: u64,
    pub total: u64,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundStatus {
    /// The models are here.
    pub ready: bool,
    /// Their size, all three files.
    pub bytes: u64,
    /// The download running (or how the last one ended).
    pub job: Option<SoundProgress>,
    /// Songs heard so far (kept prints).
    pub heard: usize,
    /// Of them, heard from your own files.
    pub files: usize,
    /// Songs heard at once on this PC (groups of four, `workers` groups).
    pub workers: usize,
}

fn report(app: &AppHandle, stage: &str, done: u64, total: u64, error: &str) {
    let progress = SoundProgress { stage: stage.into(), done, total, error: error.into() };
    *PROGRESS.lock().unwrap() = Some(progress.clone());
    let _ = app.emit("sound-download", progress);
}

fn status(app: &AppHandle) -> SoundStatus {
    let sound = app.state::<Sound>();
    let (heard, files) = if prints_path(app).is_some_and(|p| p.exists()) {
        sound.with_prints(app, |p| {
            let heard: Vec<&Kept> = p.songs.values().filter(|k| !k.missing).collect();
            (heard.len(), heard.iter().filter(|k| own_file(&k.from)).count())
        })
    } else {
        (0, 0)
    };
    SoundStatus {
        ready: Sound::ready(app),
        bytes: FILES.iter().map(|f| f.bytes).sum(),
        job: PROGRESS.lock().unwrap().clone(),
        heard,
        files,
        workers: sound.workers * GROUP_SONGS,
    }
}

#[tauri::command]
pub fn sound_status(app: AppHandle) -> SoundStatus {
    status(&app)
}

/// Stops the download.
#[tauri::command]
pub fn sound_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Downloads the models (the files not here yet), checks each one's
/// SHA-256, then loads them once to be sure they run here (progress as
/// "sound-download" events).
#[tauri::command]
pub async fn sound_download(app: AppHandle) -> Result<SoundStatus, String> {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err("The models are downloading already.".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    let result = download(&app).await;
    match &result {
        Ok(()) => report(&app, "done", 0, 0, ""),
        Err(e) if e == STOPPED => report(&app, "stopped", 0, 0, ""),
        Err(e) => report(&app, "error", 0, 0, e),
    }
    DOWNLOADING.store(false, Ordering::SeqCst);
    result.map(|_| status(&app))
}

async fn download(app: &AppHandle) -> Result<(), String> {
    let dir = folder(app).ok_or("No app data folder.")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't make the models' folder: {e}"))?;
    let missing: Vec<&ModelFile> = FILES.iter().copied().filter(|f| !whole(&dir, f)).collect();
    let total: u64 = missing.iter().map(|f| f.bytes).sum();
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut done = 0u64;
    report(app, "download", done, total, "");
    for file in missing {
        let part = dir.join(format!("{}.part", file.name));
        let result = fetch_file(app, &client, file, &part, &mut done, total).await;
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        std::fs::rename(&part, dir.join(file.name)).map_err(|e| format!("Couldn't keep {}: {e}", file.name))?;
    }
    report(app, "check", total, total, "");
    let sound = app.state::<Sound>();
    sound.names(app)?;
    sound.models(app).await.map(|_| ())
}

async fn fetch_file(app: &AppHandle, client: &reqwest::Client, file: &ModelFile, to: &Path, done: &mut u64, total: u64) -> Result<(), String> {
    let mut response = client
        .get(format!("{BASE}{}", file.name))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't download the models: {e}"))?;
    let mut out = std::fs::File::create(to).map_err(|e| format!("Couldn't save {}: {e}", file.name))?;
    let mut hash = Sha256::new();
    let mut got = 0u64;
    let mut shown = Instant::now();
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err(STOPPED.into());
        }
        match response.chunk().await {
            Ok(Some(chunk)) => {
                out.write_all(&chunk).map_err(|e| format!("Couldn't save {}: {e}", file.name))?;
                hash.update(&chunk);
                got += chunk.len() as u64;
                *done += chunk.len() as u64;
                if shown.elapsed() >= Duration::from_millis(200) {
                    shown = Instant::now();
                    report(app, "download", *done, total, "");
                }
            }
            Ok(None) => break,
            Err(e) => return Err(format!("The download stopped: {e}")),
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    let sha = format!("{:x}", hash.finalize());
    if got != file.bytes || sha != file.sha256 {
        return Err(format!("{} didn't check out (not the file expected), so it wasn't kept.", file.name));
    }
    Ok(())
}

// ----- Sound check: tier 3's test on Deep Dive's page -----

static CHECKING: AtomicBool = AtomicBool::new(false);

/// A song in the Sound check.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeardSong {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Your file of it or its preview was found and heard.
    pub heard: bool,
    /// How close it sounds to the picked songs (their average; for the
    /// picks themselves, None).
    pub closeness: Option<f32>,
    /// Its three strongest styles ("Black Metal").
    pub styles: Vec<String>,
    /// "Deezer" or "Apple" (a preview), or "your file"
    pub from: String,
    /// Heard from your own file, all of it.
    pub file: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeardAlbum {
    pub title: String,
    pub artist: String,
    /// Closest first; the ones not heard at the end.
    pub songs: Vec<HeardSong>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundCheck {
    pub picks: Vec<HeardSong>,
    pub albums: Vec<HeardAlbum>,
    /// Songs heard just now (the rest were heard before, or have no
    /// preview).
    pub heard_now: usize,
    /// Of the songs here, the ones heard from your own files.
    pub files: usize,
    pub seconds: f32,
    pub workers: usize,
}

/// Tier 3's test: the picked songs heard, then the songs of the picked
/// albums ranked by how close they sound to them (progress as
/// "sound-check" events: `{ done, total }`, the songs heard just now).
#[tauri::command]
pub async fn sound_check(app: AppHandle, picks: Vec<DivePick>) -> Result<SoundCheck, String> {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return Err("A Sound check is running already.".into());
    }
    let result = check(&app, &picks).await;
    CHECKING.store(false, Ordering::SeqCst);
    result
}

async fn check(app: &AppHandle, picks: &[DivePick]) -> Result<SoundCheck, String> {
    let started = Instant::now();
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    let songs: Vec<Song> = picks
        .iter()
        .filter(|p| p.kind == "song")
        .map(|p| Song { artist: p.artist.clone(), title: p.title.clone(), album: p.album.clone(), handle: Some(p.handle.clone()) })
        .collect();
    if songs.is_empty() {
        return Err("Pick a song to compare with (its brain button).".into());
    }
    let mut albums: Vec<(String, String, Vec<Song>)> = Vec::new();
    for p in picks.iter().filter(|p| p.kind == "album") {
        let titles: Vec<(String, String)> = match client.artist_album(&p.handle) {
            Some(album) => client.album_songs(&album).await.into_iter().map(|t| (t.title, t.handle)).collect(),
            None => client.album_tracks_for("", &p.handle).await.map(|ts| ts.into_iter().map(|t| (t.title, t.handle)).collect()).unwrap_or_default(),
        };
        let list = titles.into_iter().map(|(title, handle)| Song { artist: p.artist.clone(), title, album: p.title.clone(), handle: Some(handle) }).collect();
        albums.push((p.title.clone(), p.artist.clone(), list));
    }
    if albums.is_empty() {
        return Err("Pick an album too: its songs are ranked by how close they sound to your songs.".into());
    }
    let sound = app.state::<Sound>();
    let names = sound.names(app)?;
    let mut all = songs.clone();
    for (_, _, list) in &albums {
        all.extend(list.iter().cloned());
    }
    // (what each was heard from before: one heard again now from your file
    // counts as heard just now)
    let before: Vec<Option<String>> = all.iter().map(|s| sound.print_kept(app, &s.artist, &s.title).map(|p| p.from)).collect();
    let tick = {
        let app = app.clone();
        move |done: usize, total: usize| {
            let _ = app.emit("sound-check", serde_json::json!({ "done": done, "total": total }));
        }
    };
    let prints = sound.prints(app, &all, &tick).await?;
    let heard_now = prints.iter().zip(&before).filter(|(p, b)| p.as_ref().is_some_and(|p| b.as_deref() != Some(p.from.as_str()))).count();
    let source = |p: &Print| (if own_file(&p.from) { "your file" } else if p.from.starts_with("itunes") { "Apple" } else { "Deezer" }).to_string();
    let reference: Vec<&Print> = prints[..songs.len()].iter().flatten().collect();
    if reference.is_empty() {
        return Err("Your songs couldn't be heard (no file of them in Roon, no preview found), so there's nothing to compare with.".into());
    }
    let describe = |song: &Song, print: Option<&Print>, close: bool| HeardSong {
        title: song.title.clone(),
        artist: song.artist.clone(),
        album: song.album.clone(),
        heard: print.is_some(),
        closeness: if close { print.map(|p| reference.iter().map(|r| p.closeness(r)).sum::<f32>() / reference.len() as f32) } else { None },
        styles: print.map(|p| p.top_styles(&names, 3)).unwrap_or_default(),
        from: print.map(source).unwrap_or_default(),
        file: print.is_some_and(|p| own_file(&p.from)),
    };
    let picks_heard: Vec<HeardSong> = songs.iter().zip(&prints).map(|(s, p)| describe(s, p.as_ref(), false)).collect();
    let mut at = songs.len();
    let mut out_albums = Vec::new();
    for (title, artist, list) in &albums {
        let mut rows: Vec<HeardSong> = list.iter().zip(&prints[at..at + list.len()]).map(|(s, p)| describe(s, p.as_ref(), true)).collect();
        at += list.len();
        rows.sort_by(|a, b| b.closeness.unwrap_or(-2.0).total_cmp(&a.closeness.unwrap_or(-2.0)));
        out_albums.push(HeardAlbum { title: title.clone(), artist: artist.clone(), songs: rows });
    }
    let files = prints.iter().flatten().filter(|p| own_file(&p.from)).count();
    Ok(SoundCheck {
        picks: picks_heard,
        albums: out_albums,
        heard_now,
        files,
        seconds: started.elapsed().as_secs_f32(),
        workers: sound.workers * GROUP_SONGS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(artist: &str, title: &str, album: &str) -> Song {
        Song { artist: artist.into(), title: title.into(), album: album.into(), handle: None }
    }

    #[test]
    fn short_titles() {
        assert_eq!(short_title("Ayahuasca (Original Mix)"), "Ayahuasca");
        assert_eq!(short_title("Hero - Remastered 2019"), "Hero");
        assert_eq!(short_title("Hero"), "Hero");
        assert_eq!(base_title("Ayahuasca (Original Mix)"), "ayahuasca");
    }

    #[test]
    fn previews_fit_the_song() {
        let want = song("Bölzer", "Hero", "Hero");
        // (the same, folded; the album counts too)
        assert_eq!(fit(&want, "Bolzer", "Hero", "Hero", "Hero"), Some(6));
        assert_eq!(fit(&want, "BÖLZER", "Hero", "", "Soma"), Some(4));
        // (another version, less; a live one less again)
        assert_eq!(fit(&want, "Bölzer", "Hero (Remastered)", "Hero", ""), Some(2));
        assert_eq!(fit(&want, "Bölzer", "Hero (Live in Kyiv)", "Hero", ""), Some(1));
        // (another song or another artist: no)
        assert_eq!(fit(&want, "Bölzer", "Heroes", "Heroes", "Hero"), None);
        assert_eq!(fit(&want, "Bolzer Tribute", "Hero", "Hero", "Hero"), None);
        let tracks: Vec<Value> = serde_json::from_str(
            r#"[{"id": 1, "title": "Hero (Live)", "title_short": "Hero", "preview": "https://a/1.mp3", "artist": {"name": "Bölzer"}, "album": {"title": "Live"}},
                {"id": 2, "title": "Hero", "title_short": "Hero", "preview": "https://a/2.mp3", "artist": {"name": "Bölzer"}, "album": {"title": "Hero"}},
                {"id": 3, "title": "Hero", "title_short": "Hero", "preview": "", "artist": {"name": "Bölzer"}, "album": {"title": "Hero"}}]"#,
        )
        .unwrap();
        let p = deezer_pick(&tracks, &want).unwrap();
        assert_eq!(p.from, "deezer:2");
        assert_eq!(p.ext, "mp3");
    }

    #[test]
    fn prints_pack_small() {
        let mut artist: Vec<f32> = (0..ARTIST_DIMS).map(|i| ((i * 7919) % 101) as f32 - 50.0).collect();
        let length = artist.iter().map(|v| v * v).sum::<f32>().sqrt();
        artist.iter_mut().for_each(|v| *v /= length);
        let styles: Vec<f32> = (0..STYLES).map(|i| (i as f32 / STYLES as f32).powi(3)).collect();
        let heard = Heard { artist: artist.clone(), styles: styles.clone() };
        let kept = pack(&heard, "deezer:1".into());
        assert!(kept.artist.len() + kept.styles.len() < 1300);
        let back = unpack(&kept).unwrap();
        let original = Print { artist, styles: styles.clone(), from: String::new() };
        assert!(back.closeness(&original) > 0.999);
        for (a, b) in back.styles.iter().zip(&styles) {
            assert!((a - b).abs() < 0.01, "{a} {b}");
        }
        let names: Vec<String> = (0..STYLES).map(|i| format!("Rock---Style {i}")).collect();
        assert_eq!(back.top_styles(&names, 2), vec!["Style 399".to_string(), "Style 398".to_string()]);
    }
}
