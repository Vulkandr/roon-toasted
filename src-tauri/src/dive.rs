//! Deep Dive: a playlist made to order, with tier 2. You pick up to five
//! songs, albums or artists (the brain buttons where the radio buttons
//! were) and say what you want; tier 2 talks it over with you (a few
//! questions, yours answered), finds artists in your library or new to you
//! (or both), and chooses and orders the songs from what Roon has of theirs. The result is a Roon playlist,
//! "🧠 <title>", and the last ones are kept in `wrapped-dive.json`.
//! Each build's notes go to `dive-log.txt` in the app data folder.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use roon_library::{LibraryClient, PlaylistCard, TopTrack};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::judge::{DiveReply, Llm};
use crate::library::Library;
use crate::wrapped::{read_json, write_json, LibraryCopy, WrappedSettings};

/// Something picked with a brain button.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DivePick {
    /// "song", "album" or "artist".
    pub kind: String,
    /// The song's, album's or artist's Roon handle (this session's).
    pub handle: String,
    /// The song's or album's title, or the artist's name.
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_handle: Option<String>,
    /// The artist's handle (an artist pick's own, an album's performer).
    pub artist_handle: Option<String>,
    /// For the page's pick cards, when the talk is taken up again.
    pub image_key: Option<String>,
}

/// One turn of the talk before building: `from` "listener" or "dive".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiveTurn {
    pub from: String,
    pub text: String,
}

/// What the talk settled (tier 2's last reply).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DivePlan {
    pub summary: String,
    /// Smart Radio: songs as close to the picks as can be, nothing asked.
    pub smart: bool,
    pub source: String,
    pub count: usize,
    pub per_artist: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiveSong {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<u32>,
    pub why: String,
    /// In your library already (else new to you).
    pub in_library: bool,
    /// How close it sounds to the picks (tier 3), when it was heard.
    pub sound: Option<f32>,
}

/// A Deep Dive made: its playlist's name, what was asked, its songs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiveList {
    pub name: String,
    pub request: String,
    pub picks: Vec<String>,
    pub built_at: i64,
    pub playlist_id: Option<String>,
    pub songs: Vec<DiveSong>,
    /// What it was made from, for taking the talk up again ("Talk it over"):
    /// the picks, the talk and what it settled, and the Deep Dive it follows
    /// (its build time), if any.
    pub pick_list: Vec<DivePick>,
    pub talk: Vec<DiveTurn>,
    pub plan: DivePlan,
    pub follows: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Store {
    /// Newest first.
    dives: Vec<DiveList>,
}

/// A kept Deep Dive as the page shows it, with Roon's playlist while it's
/// still there.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiveCard {
    #[serde(flatten)]
    pub list: DiveList,
    pub playlist: Option<PlaylistCard>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiveStatus {
    pub dives: Vec<DiveCard>,
    pub building: bool,
}

/// How many Deep Dives the page keeps.
const KEPT: usize = 12;
/// At most this many picks.
const PICKS_MAX: usize = 5;
/// The library's artists go to tier 2 this many at a time.
const LIBRARY_CHUNK: usize = 250;
/// Albums tier 2 may choose per artist, and artists per album-choosing call
/// (at most ALBUMS_LISTED of an artist's albums listed).
const ALBUMS_EACH: usize = 2;
const ARTISTS_PER_CALL: usize = 10;
const ALBUMS_LISTED: usize = 25;
/// Albums per song-choosing call, at most SONGS_LISTED songs of each.
const ALBUMS_PER_CALL: usize = 4;
const SONGS_LISTED: usize = 24;
/// Candidate songs chosen from in one go; more are narrowed down first.
const CHOOSE_AT_ONCE: usize = 100;
/// Last.fm's similar artists offered as hints for new artists: per picked
/// artist, and in all.
const HINTS_EACH: usize = 20;
const HINTS_MAX: usize = 50;
/// New artists are asked for in parts that run side by side, each its own
/// stretch of the alphabet, about this many artists a part, at most this
/// many parts: one long answer (50 artists from Opus) took more than the
/// 3 minutes allowed, and the build went on without them.
const NEW_PER_PART: usize = 12;
const NEW_PARTS_MAX: usize = 5;
/// How long one of Deep Dive's answers may take: its model may be a large
/// one that thinks at length.
const DIVE_WAIT_SECS: u64 = 420;

static BUILDING: AtomicBool = AtomicBool::new(false);
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(line: String) {
    println!("[dive] {line}");
    LOG.lock().unwrap().push(line);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn store_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-dive.json"))
}

fn load(app: &AppHandle) -> Store {
    store_path(app).and_then(|p| read_json(&p)).unwrap_or_default()
}

fn save(app: &AppHandle, store: &Store) {
    if let Some(p) = store_path(app) {
        write_json(&p, store);
    }
}

fn clip(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}...", cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut))
}

/// The last brief (by picks and words): the talk asks for it every turn.
static BRIEF: Mutex<Option<(String, String)>> = Mutex::new(None);

/// The picks and the request as tier 2 reads them: each pick with what's
/// written about it (the album's tags, styles and review; the artist's tags
/// and bio), then their words.
async fn brief(app: &AppHandle, client: &LibraryClient, picks: &[DivePick], text: &str) -> String {
    let key = format!("{}|{}", serde_json::to_string(picks).unwrap_or_default(), text.trim());
    if let Some((k, b)) = BRIEF.lock().unwrap().as_ref() {
        if *k == key {
            return b.clone();
        }
    }
    let out = brief_fresh(app, client, picks, text).await;
    *BRIEF.lock().unwrap() = Some((key, out.clone()));
    out
}

async fn brief_fresh(app: &AppHandle, client: &LibraryClient, picks: &[DivePick], text: &str) -> String {
    let settings = WrappedSettings::load(app);
    let key = settings.lastfm_key.trim().to_string();
    let discogs = settings.discogs_token.trim().to_string();
    let mut out = String::new();
    if !picks.is_empty() {
        out.push_str("THEIR PICKS:\n");
    }
    for p in picks {
        match p.kind.as_str() {
            "artist" => {
                out.push_str(&format!("- The artist {}\n", p.title));
                let tags = app.state::<crate::tags::Tags>().lookup_quick(app, &p.title, &key).await.tags;
                if !tags.is_empty() {
                    out.push_str(&format!("  Tags: {}\n", tags.iter().take(8).map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")));
                }
                let bio = client.artist_bio(&p.handle).await.ok().flatten().unwrap_or_default();
                let bio = if bio.is_empty() {
                    let texts = app.state::<crate::sources::Sources>().artist(app, &p.title, &key, true).await;
                    if texts.lastfm.is_empty() {
                        texts.wikipedia
                    } else {
                        texts.lastfm
                    }
                } else {
                    bio
                };
                if !bio.is_empty() {
                    out.push_str(&format!("  About them: {}\n", clip(&bio, 500)));
                }
            }
            kind => {
                let (title, album) = if kind == "album" { (String::new(), p.title.clone()) } else { (p.title.clone(), p.album.clone()) };
                let album_handle = if kind == "album" { Some(p.handle.clone()) } else { p.album_handle.clone() };
                let a = crate::judge::album_brief(app, client, &key, &discogs, &p.artist, &album, None, album_handle.as_deref()).await;
                let year = client.album_card(album_handle.as_deref().unwrap_or("")).and_then(|c| c.year).map(|y| format!(", {y}")).unwrap_or_default();
                if kind == "album" {
                    out.push_str(&format!("- The album \"{album}\" by {}{year}\n", p.artist));
                } else {
                    out.push_str(&format!("- The song \"{title}\" by {}, from the album \"{album}\"{year}\n", p.artist));
                }
                if !a.tags.is_empty() {
                    out.push_str(&format!("  Album tags: {}\n", a.tags.iter().take(8).map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")));
                }
                if !a.styles.is_empty() {
                    out.push_str(&format!("  Styles: {}\n", a.styles.join(", ")));
                }
                let review = [&a.review, &a.lastfm, &a.wikipedia].into_iter().find(|t| !t.is_empty()).cloned().unwrap_or_default();
                if !review.is_empty() {
                    out.push_str(&format!("  About the album: {}\n", clip(&review, 600)));
                }
            }
        }
    }
    let text = text.trim();
    out.push_str(&format!(
        "\nWHAT THEY ASKED FOR: {}\n",
        if text.is_empty() { "nothing in words: music like their picks" } else { text }
    ));
    out
}

/// A Deep Dive made before (by its build time), when the talk is taken up
/// again about it.
fn made(app: &AppHandle, built_at: Option<i64>) -> Option<DiveList> {
    let at = built_at?;
    load(app).dives.into_iter().find(|d| d.built_at == at)
}

/// The Deep Dive the talk is about, as tier 2 reads it.
fn last_try(list: &DiveList) -> String {
    let mut out = format!("\nTHE LAST TRY, made for this request (\"{}\"); they've listened to it:\n", list.name);
    for (n, s) in list.songs.iter().enumerate() {
        let year = s.year.map(|y| format!(", {y}")).unwrap_or_default();
        let whence = if s.in_library { "in their library" } else { "new to them" };
        let why = if s.why.is_empty() { String::new() } else { format!(": {}", s.why) };
        out.push_str(&format!("{}. {} - {} ({}{year}) [{whence}]{why}\n", n + 1, s.artist, s.title, s.album));
    }
    out
}

/// Tier 2 for Deep Dive: its service, with Deep Dive's own model when one
/// is set (Wrapped's Settings).
pub(crate) fn llm_or_err(app: &AppHandle) -> Result<Llm, String> {
    let settings = WrappedSettings::load(app);
    let mut llm = Llm::from_settings(&settings).ok_or_else(|| "Deep Dive needs tier 2, a language model (Wrapped's Settings).".to_string())?;
    if !settings.dive_model.trim().is_empty() {
        llm.model = settings.dive_model.trim().to_string();
    }
    llm.wait_secs = DIVE_WAIT_SECS;
    Ok(llm)
}

/// Library artists whose music can answer a request, named by tier 2
/// (`names`, in parts of LIBRARY_CHUNK, all at once online): their
/// indexes, best first, the parts taken in turns. For Library
/// Rediscover's Fine Tuning as well as Deep Dive.
pub(crate) async fn library_artists_for(llm: &Llm, brief: &str, names: &[String], want: usize, progress: &impl Fn(String)) -> Vec<usize> {
    let at_once = if llm.provider == "ollama" { 1 } else { 3 };
    let chunks: Vec<Vec<String>> = names.chunks(LIBRARY_CHUNK).map(|c| c.to_vec()).collect();
    let jobs: Vec<Job<Result<Vec<usize>, String>>> = chunks
        .iter()
        .map(|part| {
            let (llm, brief, part) = (llm.clone(), brief.to_string(), part.clone());
            Box::pin(async move { crate::judge::dive_library_artists(&llm, &brief, &part, want).await }) as Job<_>
        })
        .collect();
    let answers = run_all(at_once, jobs, &|done, total| progress(format!("Going through your library's artists... {done} of {total}"))).await;
    let mut chosen: Vec<Vec<usize>> = Vec::new();
    for (n, answer) in answers.into_iter().enumerate() {
        match answer {
            Some(Ok(list)) => chosen.push(list.into_iter().map(|i| n * LIBRARY_CHUNK + i).collect()),
            Some(Err(e)) => note(format!("library part {} not read: {e}", n + 1)),
            None => note(format!("library part {} not read", n + 1)),
        }
    }
    // (the best of each part in turns, so no part of the alphabet fills it)
    let mut picked: Vec<usize> = Vec::new();
    let mut round = 0;
    while picked.len() < want && chosen.iter().any(|c| c.len() > round) {
        for c in &chosen {
            if let Some(i) = c.get(round) {
                if picked.len() < want && !picked.contains(i) {
                    picked.push(*i);
                }
            }
        }
        round += 1;
    }
    picked
}

/// The alphabet in `n` stretches holding about as many band names each
/// (by first letter), for asking for new artists in parts.
fn alphabet_parts(n: usize) -> Vec<(char, char)> {
    match n {
        0 | 1 => vec![('A', 'Z')],
        2 => vec![('A', 'K'), ('L', 'Z')],
        3 => vec![('A', 'F'), ('G', 'N'), ('O', 'Z')],
        4 => vec![('A', 'D'), ('E', 'K'), ('L', 'P'), ('Q', 'Z')],
        _ => vec![('A', 'C'), ('D', 'H'), ('I', 'M'), ('N', 'R'), ('S', 'Z')],
    }
}

/// One part of the ask, for tier 2: its stretch of the alphabet (the last
/// takes names starting with a digit or in another script too).
fn part_words(i: usize, parts: usize, from: char, to: char) -> String {
    if parts < 2 {
        return String::new();
    }
    let rest = if i + 1 == parts { ", or with a digit, or in a script other than the Latin alphabet" } else { "" };
    format!(
        "This is one of {parts} asks running side by side, each for its own stretch of the alphabet; the others cover the rest. Here, suggest only artists whose names start with {from} to {to}{rest} (a leading \"The\" doesn't count)."
    )
}

type Job<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// Runs the jobs `limit` at a time; the results in the jobs' order (None
/// for one that panicked), `tick(done, total)` after each.
async fn run_all<T: Send + 'static>(limit: usize, jobs: Vec<Job<T>>, tick: &impl Fn(usize, usize)) -> Vec<Option<T>> {
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    let total = jobs.len();
    let mut set = tokio::task::JoinSet::new();
    for (i, job) in jobs.into_iter().enumerate() {
        let gate = gate.clone();
        set.spawn(async move {
            let _turn = gate.acquire_owned().await;
            (i, job.await)
        });
    }
    let mut out: Vec<Option<T>> = (0..total).map(|_| None).collect();
    let mut done = 0;
    while let Some(finished) = set.join_next().await {
        if let Ok((i, value)) = finished {
            out[i] = Some(value);
        }
        done += 1;
        tick(done, total);
    }
    out
}

/// "7:12"
fn length(seconds: Option<i32>) -> String {
    seconds.map(|s| format!("{}:{:02}", s / 60, s % 60)).unwrap_or_default()
}

/// Titles compared loosely (case, accents left as they are, spacing).
fn same_title(a: &str, b: &str) -> bool {
    let plain = |t: &str| t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    plain(a) == plain(b)
}

/// Ollama started for the job when Wrapped runs it.
async fn ollama_for(app: &AppHandle, llm: &Llm) -> Result<Option<crate::ollama::Lease>, String> {
    if llm.provider != "ollama" {
        return Ok(None);
    }
    crate::ollama::lease(app).await.map(|(held, _)| Some(held))
}

/// Tier 2's next turn in the talk before building (`source`: where the
/// songs come from, "library", "new" or "both", chosen on the page;
/// `history`: the turns so far, oldest first; empty for its opening
/// question).
#[tauri::command]
pub async fn wrapped_dive_chat(app: AppHandle, picks: Vec<DivePick>, text: String, source: String, history: Vec<DiveTurn>, follows: Option<i64>, mode: Option<String>) -> Result<DiveReply, String> {
    let llm = llm_or_err(&app)?;
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    let _ollama = ollama_for(&app, &llm).await?;
    let picks: Vec<DivePick> = picks.into_iter().take(PICKS_MAX).collect();
    let mut brief = brief(&app, &client, &picks, &text).await;
    if let Some(list) = made(&app, follows) {
        brief.push_str(&last_try(&list));
    }
    let talk: Vec<(bool, String)> = history.into_iter().filter(|t| !t.text.trim().is_empty()).map(|t| (t.from == "listener", t.text)).collect();
    crate::judge::dive_chat(&llm, &brief, &source, &talk, mode.as_deref().unwrap_or("deeper")).await
}

/// What Smart Radio asks for, in place of a talk.
const SMART_RADIO: &str = "Smart Radio: nothing asked beyond their picks. Find the songs that sound as close as possible to the picks themselves: the same kind of sound (guitars, vocals, pace, production, mood), feel and era, the way a good radio station follows what's playing. Be strict: leave out songs that only share fans or a broad genre with the picks.";

#[tauri::command]
pub fn wrapped_dive_status(app: AppHandle) -> DiveStatus {
    status(&app)
}

fn status(app: &AppHandle) -> DiveStatus {
    let playlists = app.state::<Library>().client().and_then(|c| c.playlists().ok()).unwrap_or_default();
    let dives = load(app)
        .dives
        .into_iter()
        .map(|list| {
            let playlist = playlists.iter().find(|p| list.playlist_id.is_some() && p.playlist_id == list.playlist_id).cloned();
            DiveCard { list, playlist }
        })
        .collect();
    DiveStatus { dives, building: BUILDING.load(Ordering::SeqCst) }
}

/// Forgets a Deep Dive (by when it was built); Roon's playlist stays.
#[tauri::command]
pub fn wrapped_dive_forget(app: AppHandle, built_at: i64) -> DiveStatus {
    let mut store = load(&app);
    store.dives.retain(|d| d.built_at != built_at);
    save(&app, &store);
    status(&app)
}

/// Builds a Deep Dive (progress as "wrapped-dive" events: `{ text }`)
/// from the picks, the words and what the talk settled (`plan`: the
/// summary, `source` "library", "new" or "both", `count` songs, `perArtist`
/// songs by any one artist at most; `history`: the talk itself; `follows`:
/// the Deep Dive the talk was about, when it was taken up again).
#[tauri::command]
pub async fn wrapped_dive_build(app: AppHandle, picks: Vec<DivePick>, text: String, plan: DivePlan, history: Vec<DiveTurn>, follows: Option<i64>) -> Result<DiveStatus, String> {
    if BUILDING.swap(true, Ordering::SeqCst) {
        return Err("A Deep Dive is being made already.".into());
    }
    LOG.lock().unwrap().clear();
    let progress = {
        let app = app.clone();
        move |text: String| {
            let _ = app.emit("wrapped-dive", serde_json::json!({ "text": text }));
        }
    };
    let started = std::time::Instant::now();
    let before = made(&app, follows);
    let result = build(&app, picks, &text, &plan, &history, before.as_ref(), &progress).await;
    match &result {
        Ok(list) => note(format!("made \"{}\": {} songs in {} s", list.name, list.songs.len(), started.elapsed().as_secs())),
        Err(e) => note(format!("failed: {e}")),
    }
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::write(dir.join("dive-log.txt"), LOG.lock().unwrap().join("\n"));
    }
    BUILDING.store(false, Ordering::SeqCst);
    let list = result?;
    let mut store = load(&app);
    store.dives.insert(0, list);
    store.dives.truncate(KEPT);
    save(&app, &store);
    Ok(status(&app))
}

/// An artist in the running, from the library or new to you.
struct Artist {
    name: String,
    handle: String,
    in_library: bool,
    /// New ones: why tier 2 suggested it, and the album and songs it named.
    note: String,
    album_hint: String,
    song_hints: Vec<String>,
    /// Roon's best-known songs of the artist (lower-cased titles).
    popular: HashSet<String>,
    /// Its albums to choose from (`albums_to_choose`).
    albums: Vec<roon_library::ArtistAlbum>,
}

/// An album chosen for an artist, with its songs, what's written about it
/// and, with tier 3, what listening to each song found.
struct Album {
    artist: usize,
    album: roon_library::ArtistAlbum,
    songs: Vec<TopTrack>,
    about: crate::judge::AlbumBrief,
    /// By song (empty without tier 3; None for a song not heard).
    sound: Vec<Option<SongSound>>,
}

/// What tier 3 heard in one of the chosen albums' songs.
#[derive(Debug, Clone, Default)]
struct SongSound {
    /// How close it sounds to the picks (the average over the picks
    /// heard), when any pick was heard.
    close: Option<f32>,
    /// Its three strongest styles ("Black Metal").
    styles: Vec<String>,
}

impl Album {
    /// The album as tier 2 reads it when choosing its songs.
    fn block(&self, artist: &Artist) -> String {
        let a = &self.album;
        let year = a.year.map(|y| format!(" ({y})")).unwrap_or_default();
        let whence = if artist.in_library { "in their library" } else { "new to them" };
        let mut out = format!("{} - \"{}\"{year} [{whence}]\n", artist.name, a.title);
        if !artist.note.is_empty() {
            out.push_str(&format!("Why the artist was suggested: {}\n", artist.note));
        }
        let hinted = !artist.album_hint.is_empty() && (same_title(&artist.album_hint, &a.title) || a.title.to_lowercase().contains(&artist.album_hint.to_lowercase()));
        if hinted && !artist.song_hints.is_empty() {
            out.push_str(&format!("Songs named for it before: {}\n", artist.song_hints.join(", ")));
        }
        if !self.about.tags.is_empty() {
            out.push_str(&format!("Tags: {}\n", self.about.tags.iter().take(8).map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if !self.about.styles.is_empty() {
            out.push_str(&format!("Styles: {}\n", self.about.styles.join(", ")));
        }
        let about = [&self.about.review, &self.about.lastfm, &self.about.wikipedia].into_iter().find(|t| !t.is_empty()).cloned().unwrap_or_default();
        if !about.is_empty() {
            out.push_str(&format!("About it: {}\n", clip(&about, 1200)));
        }
        out.push_str("Songs:\n");
        for (n, t) in self.songs.iter().enumerate() {
            let popular = if artist.popular.contains(&t.title.to_lowercase()) { " popular" } else { "" };
            out.push_str(&format!("{}. {} ({}){popular}{}\n", n + 1, t.title, length(t.length_seconds), self.heard(n)));
        }
        out
    }

    /// What listening found in song `n`, for tier 2 ("; sound 0.90;
    /// styles Black Metal, Viking Metal"), or nothing.
    fn heard(&self, n: usize) -> String {
        let Some(Some(s)) = self.sound.get(n) else { return String::new() };
        let mut out = String::new();
        if let Some(close) = s.close {
            out.push_str(&format!("; sound {close:.2}"));
        }
        if !s.styles.is_empty() {
            out.push_str(&format!("; styles {}", s.styles.join(", ")));
        }
        out
    }
}

/// Tier 3 listens: the picks first (a song itself, an album's proper songs,
/// an artist's five best-known), then every song on the chosen albums. Each
/// song's closeness to the picks (to each pick's songs together, averaged
/// over the picks) and its strongest styles, album by album, song by song.
/// None when it couldn't listen.
async fn listen(app: &AppHandle, client: &LibraryClient, picks: &[DivePick], albums: &[Album], artists: &[Artist], progress: &(impl Fn(String) + Sync)) -> Option<Vec<Vec<Option<SongSound>>>> {
    use crate::sound::{Print, Song, Sound};
    let started = std::time::Instant::now();
    let sound = app.state::<Sound>();
    progress("Listening to your picks...".into());
    let mut pick_songs: Vec<Vec<Song>> = Vec::new();
    for p in picks {
        let list: Vec<Song> = match p.kind.as_str() {
            "song" => vec![Song { artist: p.artist.clone(), title: p.title.clone(), album: p.album.clone(), handle: Some(p.handle.clone()) }],
            "album" => match client.artist_album(&p.handle) {
                Some(album) => client
                    .album_songs(&album)
                    .await
                    .into_iter()
                    .take(12)
                    .map(|t| Song { artist: p.artist.clone(), title: t.title, album: p.title.clone(), handle: Some(t.handle) })
                    .collect(),
                None => Vec::new(),
            },
            _ => client
                .top_tracks(&p.handle)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|t| !t.live)
                .take(5)
                .map(|t| Song { artist: p.title.clone(), title: t.title, album: t.album, handle: Some(t.handle) })
                .collect(),
        };
        pick_songs.push(list);
    }
    let mut all: Vec<Song> = pick_songs.iter().flatten().cloned().collect();
    let first = all.len();
    for album in albums {
        let artist = &artists[album.artist].name;
        // (an album in your library may be on disk: its files are heard)
        let yours = album.album.in_library;
        all.extend(album.songs.iter().map(|t| Song { artist: artist.clone(), title: t.title.clone(), album: album.album.title.clone(), handle: yours.then(|| t.handle.clone()) }));
    }
    let tick = |done: usize, total: usize| {
        if total > 0 {
            progress(format!("Listening to the songs... {done} of {total}"));
        }
    };
    let prints = match sound.prints(app, &all, &tick).await {
        Ok(prints) => prints,
        Err(e) => {
            note(format!("tier 3 didn't listen: {e}"));
            return None;
        }
    };
    let names = sound.names(app).unwrap_or_default();
    // Each pick's sound: its songs' together
    let mut refs: Vec<Vec<f32>> = Vec::new();
    let mut at = 0;
    for (p, list) in picks.iter().zip(&pick_songs) {
        let heard: Vec<&Print> = prints[at..at + list.len()].iter().flatten().collect();
        at += list.len();
        match crate::sound::average(&heard) {
            Some(v) => refs.push(v),
            None => note(format!("tier 3 couldn't hear the pick {} ({})", p.title, p.kind)),
        }
    }
    let mut out = Vec::new();
    let mut at = first;
    let mut heard = 0;
    for album in albums {
        let mut row: Vec<Option<SongSound>> = Vec::new();
        for _ in &album.songs {
            row.push(prints[at].as_ref().map(|p| SongSound {
                close: (!refs.is_empty()).then(|| refs.iter().map(|r| crate::sound::dot(&p.artist, r)).sum::<f32>() / refs.len() as f32),
                styles: p.top_styles(&names, 3),
            }));
            at += 1;
        }
        heard += row.iter().flatten().count();
        // (the log: the album's songs, closest first)
        let mut order: Vec<(f32, &str)> = album.songs.iter().zip(&row).filter_map(|(t, s)| Some((s.as_ref()?.close?, t.title.as_str()))).collect();
        order.sort_by(|a, b| b.0.total_cmp(&a.0));
        if !order.is_empty() {
            note(format!("sound of {} - {}: {}", artists[album.artist].name, album.album.title, order.iter().map(|(c, t)| format!("{t} {c:.2}")).collect::<Vec<_>>().join(", ")));
        }
        out.push(row);
    }
    let (files, all_heard) = (prints.iter().flatten().filter(|p| crate::sound::own_file(&p.from)).count(), prints.iter().flatten().count());
    note(format!(
        "tier 3 heard {heard} of {} songs ({} of {} picks) in {} s; {files} of the {all_heard} songs heard (picks too) from your files, the rest from previews",
        all.len() - first,
        refs.len(),
        picks.len(),
        started.elapsed().as_secs()
    ));
    Some(out)
}

async fn build(app: &AppHandle, picks: Vec<DivePick>, text: &str, plan: &DivePlan, history: &[DiveTurn], before: Option<&DiveList>, progress: &(impl Fn(String) + Sync)) -> Result<DiveList, String> {
    let llm = llm_or_err(app)?;
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    let picks: Vec<DivePick> = picks.into_iter().take(PICKS_MAX).collect();
    if picks.is_empty() && (plan.smart || text.trim().is_empty()) {
        return Err(if plan.smart { "Pick a song, album or artist for Smart Radio first." } else { "Pick something, or say what the playlist should be." }.into());
    }
    let count = if plan.count == 0 { 30 } else { plan.count.clamp(10, 60) };
    let per_artist = plan.per_artist.clamp(1, 3);
    let source = plan.source.as_str();
    let (from_library, from_new) = match source {
        "library" => (true, false),
        "new" => (false, true),
        _ => (true, true),
    };
    progress("Starting...".into());
    let _ollama = ollama_for(app, &llm).await?;
    // (Ollama answers one at a time; the online services several)
    let at_once = if llm.provider == "ollama" { 1 } else { 3 };
    note(format!("Deep Dive ({} {}): {} ({}), picks: {}", llm.provider, llm.model, text.trim(), source, picks.iter().map(|p| format!("{} {} by {}", p.kind, p.title, p.artist)).collect::<Vec<_>>().join("; ")));
    progress("Reading up on your picks...".into());
    let mut brief = brief(app, &client, &picks, text).await;
    // (what the talk settled, and the talk itself in their words; Smart
    // Radio's own words instead)
    if plan.smart {
        brief.push_str(&format!("WHAT THEY WANT: {SMART_RADIO}\n"));
        note("Smart Radio".to_string());
    } else if !plan.summary.trim().is_empty() {
        brief.push_str(&format!("WHAT YOU SETTLED WITH THEM: {}\n", plan.summary.trim()));
        note(format!("settled: {}", plan.summary.trim()));
    }
    let said: Vec<String> = history
        .iter()
        .filter(|t| !t.text.trim().is_empty())
        .map(|t| format!("{}: {}", if t.from == "listener" { "Listener" } else { "You" }, clip(&t.text, 400)))
        .collect();
    if !said.is_empty() {
        brief.push_str(&format!("THE TALK:\n{}\n", said.join("\n")));
        for line in &said {
            note(format!("    {line}"));
        }
    }
    brief.push_str(&format!(
        "WHERE FROM: {}\n",
        match (from_library, from_new) {
            (true, false) => "their own library",
            (false, true) => "music new to them",
            _ => "a mix of their own library and music new to them",
        }
    ));
    // (a Deep Dive taken up again: the last one, with what they said of it
    // in the talk; its songs don't come back)
    if let Some(list) = before {
        brief.push_str(&last_try(list));
        brief.push_str("Their verdict on the last try is in the talk: the songs that hit the mark are the surest guide to what they want, the misses show what to stay away from.\n");
        note(format!("follows \"{}\" ({} songs)", list.name, list.songs.len()));
    }
    let settings = WrappedSettings::load(app);
    let key = settings.lastfm_key.trim().to_string();
    let discogs = settings.discogs_token.trim().to_string();
    let fold = crate::discover::fold;
    // The picks' artists (as the new artists' "not these", and their Roon
    // genres for telling a name's right artist in Roon); the picked songs
    // themselves stay out, and so do the last try's
    let pick_artists: Vec<String> = picks.iter().map(|p| if p.kind == "artist" { p.title.clone() } else { p.artist.clone() }).filter(|a| !a.is_empty()).collect();
    let mut picked_songs: Vec<(String, String, String)> = picks.iter().filter(|p| p.kind == "song").map(|p| (fold(&p.artist), p.album.to_lowercase(), p.title.clone())).collect();
    if let Some(list) = before {
        picked_songs.extend(list.songs.iter().map(|s| (fold(&s.artist), s.album.to_lowercase(), s.title.clone())));
    }
    let mut genre_cache = std::collections::HashMap::new();
    let mut like: HashSet<String> = HashSet::new();
    let mut looked: HashSet<String> = HashSet::new();
    for p in &picks {
        let handle = match (&p.artist_handle, p.kind.as_str()) {
            (Some(h), _) => Some(h.clone()),
            // (a song's artist: found by name)
            (None, "song") if looked.insert(fold(&p.artist)) => crate::discover::find_by_name(&client, &mut genre_cache, &p.artist, &HashSet::new()).await,
            _ => None,
        };
        if let Some(h) = handle {
            like.extend(crate::discover::genres_of(&client, &mut genre_cache, &h).await);
        }
    }
    if !like.is_empty() {
        note(format!("the picks' genres in Roon: {}", like.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    // (artists wanted: enough for tier 2 to choose from, about one and a
    // half per song; half each from the library and new when it's both)
    let mut artists_wanted = (count * 3 / 2 / per_artist).clamp(10, 64);
    if from_library && from_new {
        artists_wanted = artists_wanted.div_ceil(2);
    }
    let songs_each = (per_artist + 1).min(3);
    let no_progress = |_: usize, _: usize| {};
    progress("Reading your library...".into());
    let library_cards = client.library_artists(&no_progress).await.ok();
    let mut artists: Vec<Artist> = Vec::new();
    let artist = |name: String, handle: String, in_library: bool| Artist { name, handle, in_library, note: String::new(), album_hint: String::new(), song_hints: Vec::new(), popular: HashSet::new(), albums: Vec::new() };

    // 1. The artists: tier 2 picks your library's and suggests new ones
    if from_library {
        let cards: Vec<roon_library::ArtistCard> = library_cards.clone().unwrap_or_default().into_iter().filter(|c| !c.name.is_empty() && !c.name.eq_ignore_ascii_case("various artists")).collect();
        if cards.is_empty() {
            note("the library's artists couldn't be read".into());
        }
        let names: Vec<String> = cards.iter().map(|a| a.name.clone()).collect();
        let picked = library_artists_for(&llm, &brief, &names, artists_wanted, progress).await;
        note(format!("library: {} of {} artists chosen: {}", picked.len(), cards.len(), picked.iter().map(|i| cards[*i].name.as_str()).collect::<Vec<_>>().join(", ")));
        for i in picked {
            artists.push(artist(cards[i].name.clone(), cards[i].handle.clone(), true));
        }
    }

    if from_new {
        let played = crate::discover::played_artists(app);
        let favorites: Vec<String> = played.iter().take(150).map(|(n, _)| n.clone()).collect();
        // (names compared folded: "Mgla" is the Mgła you know)
        let mut known: HashSet<String> = played.iter().map(|(n, _)| fold(n)).collect();
        match &library_cards {
            Some(cards) => known.extend(cards.iter().map(|c| fold(&c.name))),
            None => known.extend(app.state::<LibraryCopy>().names(app).iter().map(|n| fold(n))),
        }
        known.extend(crate::discover::turned_down(app).iter().map(|n| fold(n)));
        known.extend(pick_artists.iter().map(|a| fold(a)));
        // (Last.fm's artists sharing listeners with the picked ones, as
        // hints: tier 2 decides which, if any, fit)
        let mut hints: Vec<String> = Vec::new();
        let mut asked_about: HashSet<String> = HashSet::new();
        for a in &pick_artists {
            if !asked_about.insert(a.to_lowercase()) {
                continue;
            }
            progress(format!("Asking Last.fm about {a}..."));
            for (name, _) in app.state::<crate::tags::Tags>().lookup_similar(app, a, &key).await.into_iter().take(HINTS_EACH) {
                if hints.len() < HINTS_MAX && !known.contains(&fold(&name)) && !hints.iter().any(|h| fold(h) == fold(&name)) {
                    hints.push(name);
                }
            }
        }
        if !hints.is_empty() {
            note(format!("Last.fm hints: {}", hints.join(", ")));
        }
        // (asked again, twice at most, while fewer than half come back,
        // leaving out everything suggested so far; each time in parts that
        // run side by side, each its own stretch of the alphabet, so no
        // answer gets long enough to run out of time, and a part that
        // fails loses only its own stretch)
        let mut leave_out: Vec<String> = pick_artists.clone();
        let mut suggested: HashSet<String> = HashSet::new();
        let mut found_new = 0;
        for pass in 0..3 {
            if pass > 0 && found_new * 2 >= artists_wanted {
                break;
            }
            let want = artists_wanted.saturating_sub(found_new) + 8;
            let parts = want.div_ceil(NEW_PER_PART).clamp(1, NEW_PARTS_MAX);
            let each = want.div_ceil(parts);
            let stretches = alphabet_parts(parts);
            let label = if pass == 0 { "Asking for artists new to you".to_string() } else { format!("Only {found_new} new artists so far: asking for more") };
            progress(format!("{label}..."));
            let jobs: Vec<Job<Result<Vec<crate::judge::DiveNew>, String>>> = stretches
                .iter()
                .enumerate()
                .map(|(i, (from, to))| {
                    let part = part_words(i, stretches.len(), *from, *to);
                    let (llm, brief, favorites, leave_out, hints) = (llm.clone(), brief.clone(), favorites.clone(), leave_out.clone(), hints.clone());
                    Box::pin(async move { crate::judge::dive_new_artists(&llm, &brief, &favorites, &leave_out, &hints, each, &part).await }) as Job<_>
                })
                .collect();
            // (all parts at once online; one at a time on Ollama, and on
            // Gemini's few calls a minute)
            let side_by_side = if matches!(llm.provider.as_str(), "ollama" | "gemini") { 1 } else { stretches.len() };
            let answers = run_all(side_by_side, jobs, &|done, total| {
                if total > 1 {
                    progress(format!("{label}... {done} of {total} parts"));
                }
            })
            .await;
            let which = ["", ", second ask", ", third ask"][pass];
            let mut lists: Vec<std::vec::IntoIter<crate::judge::DiveNew>> = Vec::new();
            for (answer, (from, to)) in answers.into_iter().zip(&stretches) {
                let stretch = if stretches.len() > 1 { format!(" ({from} to {to}{which})") } else if pass > 0 { format!(" ({})", &which[2..]) } else { String::new() };
                match answer {
                    Some(Ok(list)) => {
                        note(format!("tier 2 suggested {} new artists{stretch}", list.len()));
                        lists.push(list.into_iter());
                    }
                    Some(Err(e)) => note(format!("tier 2 couldn't suggest new artists{stretch}: {e}")),
                    None => note(format!("tier 2 couldn't suggest new artists{stretch}")),
                }
            }
            // (the best of each part in turns, as each lists its best first)
            let mut merged: Vec<crate::judge::DiveNew> = Vec::new();
            loop {
                let before = merged.len();
                for list in lists.iter_mut() {
                    merged.extend(list.next());
                }
                if merged.len() == before {
                    break;
                }
            }
            for (n, s) in merged.into_iter().enumerate() {
                let k = fold(&s.artist);
                // (named by two parts: once is enough)
                if !suggested.insert(k.clone()) {
                    continue;
                }
                leave_out.push(s.artist.clone());
                if known.contains(&k) || artists.iter().any(|a| fold(&a.name) == k) {
                    note(format!("left out {}: known already", s.artist));
                    continue;
                }
                if found_new >= artists_wanted {
                    continue;
                }
                progress(format!("Finding new artists in Roon... {}", n + 1));
                match crate::discover::find_by_name(&client, &mut genre_cache, &s.artist, &like).await {
                    Some(h) => {
                        note(format!("new: {} ({}{}): {}", s.artist, s.album, if s.songs.is_empty() { String::new() } else { format!(": {}", s.songs.join(", ")) }, s.why));
                        let mut a = artist(s.artist.clone(), h, false);
                        a.note = s.why;
                        a.album_hint = s.album;
                        a.song_hints = s.songs;
                        artists.push(a);
                        found_new += 1;
                    }
                    None => note(format!("left out {}: not in Roon", s.artist)),
                }
            }
        }
    }
    if artists.is_empty() {
        return Err("No artists turned up for this. Try other words, or another Where from.".into());
    }

    // 2. Their albums: each artist's albums (the library's for a library
    // artist) and best-known songs, then tier 2 chooses the albums
    let jobs: Vec<Job<(Vec<roon_library::ArtistAlbum>, HashSet<String>)>> = artists
        .iter()
        .map(|a| {
            let (client, handle, in_library) = (client.clone(), a.handle.clone(), a.in_library);
            Box::pin(async move {
                let albums = client.albums_to_choose(&handle, in_library).await;
                let popular = client.top_tracks(&handle).await.unwrap_or_default().into_iter().take(10).map(|t| t.title.to_lowercase()).collect();
                (albums, popular)
            }) as Job<_>
        })
        .collect();
    let looked = run_all(6, jobs, &|done, total| progress(format!("Looking through the artists' albums... {done} of {total}"))).await;
    for (a, found) in artists.iter_mut().zip(looked) {
        if let Some((albums, popular)) = found {
            a.albums = albums.into_iter().take(ALBUMS_LISTED).collect();
            a.popular = popular;
        }
    }
    for a in artists.iter().filter(|a| a.albums.is_empty()) {
        note(format!("left out {}: no albums of theirs in Roon{}", a.name, if a.in_library { " in your library" } else { "" }));
    }
    // (a new artist whose albums don't include the one tier 2 named, and
    // whose Roon genres share nothing with the picks', is someone else by
    // that name: a hip hop "Mgla" for the black metal Mgła)
    for a in artists.iter_mut().filter(|a| !a.in_library && !a.album_hint.is_empty() && !a.albums.is_empty()) {
        let hint = fold(&a.album_hint);
        let named = a.albums.iter().any(|al| {
            let t = fold(&al.title);
            !t.is_empty() && (t == hint || t.contains(&hint) || hint.contains(&t))
        });
        if named || like.is_empty() {
            continue;
        }
        // (no genres in Roon at all: no telling, so it stays; Cult of Fire's
        // Indian-script album title isn't Roon's English one)
        let genres = crate::discover::genres_of(&client, &mut genre_cache, &a.handle).await;
        if genres.is_empty() || genres.iter().any(|g| like.contains(g)) {
            continue;
        }
        note(format!(
            "left out {}: not the one tier 2 meant (no {} among its albums: {}; its genres: {})",
            a.name,
            a.album_hint,
            a.albums.iter().take(4).map(|al| al.title.as_str()).collect::<Vec<_>>().join(", "),
            genres.join(", ")
        ));
        a.albums.clear();
    }
    let mut picked_albums: Vec<(usize, usize)> = Vec::new();
    let to_choose: Vec<usize> = (0..artists.len()).filter(|i| artists[*i].albums.len() > 1).collect();
    for (i, a) in artists.iter().enumerate() {
        if a.albums.len() == 1 {
            picked_albums.push((i, 0));
        }
    }
    let batches: Vec<Vec<usize>> = to_choose.chunks(ARTISTS_PER_CALL).map(|c| c.to_vec()).collect();
    let jobs: Vec<Job<Result<Vec<(usize, Vec<usize>)>, String>>> = batches
        .iter()
        .map(|batch| {
            let entries: Vec<crate::judge::DiveArtistAlbums> = batch
                .iter()
                .map(|i| {
                    let a = &artists[*i];
                    let note = if a.in_library {
                        String::new()
                    } else if a.album_hint.is_empty() {
                        a.note.clone()
                    } else {
                        format!("suggested for {}: {}", a.album_hint, a.note)
                    };
                    crate::judge::DiveArtistAlbums {
                        artist: a.name.clone(),
                        note,
                        albums: a.albums.iter().map(|al| format!("{}{}", al.title, al.year.map(|y| format!(" ({y})")).unwrap_or_default())).collect(),
                    }
                })
                .collect();
            let (llm, brief) = (llm.clone(), brief.clone());
            Box::pin(async move { crate::judge::dive_albums(&llm, &brief, &entries, ALBUMS_EACH).await }) as Job<_>
        })
        .collect();
    let answers = run_all(at_once, jobs, &|done, total| progress(format!("Choosing their albums... {done} of {total}"))).await;
    for (batch, answer) in batches.iter().zip(answers) {
        match answer {
            Some(Ok(list)) => {
                for (n, albums) in list {
                    picked_albums.extend(albums.into_iter().map(|m| (batch[n], m)));
                }
                // (an artist left out: none of its albums fits)
            }
            other => {
                note(format!("albums not chosen for {} artists ({}); their likeliest album goes on", batch.len(), match other {
                    Some(Err(e)) => e,
                    _ => "no answer".into(),
                }));
                for &i in batch {
                    let a = &artists[i];
                    let hinted = a.albums.iter().position(|al| !a.album_hint.is_empty() && same_title(&al.title, &a.album_hint));
                    let best = hinted.unwrap_or_else(|| {
                        let mut order: Vec<usize> = (0..a.albums.len()).collect();
                        order.sort_by(|x, y| a.albums[*y].popularity.unwrap_or(0.0).total_cmp(&a.albums[*x].popularity.unwrap_or(0.0)));
                        order[0]
                    });
                    picked_albums.push((i, best));
                }
            }
        }
    }
    for (i, a) in artists.iter().enumerate() {
        let chosen: Vec<String> = picked_albums.iter().filter(|(x, _)| *x == i).map(|(_, m)| a.albums[*m].title.clone()).collect();
        if chosen.is_empty() && !a.albums.is_empty() {
            note(format!("left out {}: none of its albums fits", a.name));
        } else if !chosen.is_empty() {
            note(format!("albums of {}: {}", a.name, chosen.join("; ")));
        }
    }
    if picked_albums.is_empty() {
        return Err("None of the artists' albums fit. Try other words, or another Where from.".into());
    }

    // 3. The songs: each chosen album's songs, then what's written about
    // each album (Roon's review, Last.fm, Wikipedia, Discogs) and, with
    // tier 3, how each song sounds, side by side; then tier 2 chooses the
    // songs on each album
    let jobs: Vec<Job<Vec<TopTrack>>> = picked_albums
        .iter()
        .map(|&(i, m)| {
            let client = client.clone();
            let (album, in_library) = (artists[i].albums[m].clone(), artists[i].in_library);
            Box::pin(async move {
                let mut songs = client.album_songs(&album).await;
                if in_library && songs.iter().any(|t| t.in_library) {
                    songs.retain(|t| t.in_library);
                }
                songs
            }) as Job<_>
        })
        .collect();
    let listed = run_all(6, jobs, &|done, total| progress(format!("Listing the albums' songs... {done} of {total}"))).await;
    let mut albums: Vec<Album> = Vec::new();
    for (&(i, m), found) in picked_albums.iter().zip(listed) {
        let Some(mut songs) = found else { continue };
        let album = artists[i].albums[m].clone();
        songs.retain(|t| {
            !picked_songs.iter().any(|(artist, album_title, title)| same_title(title, &t.title) && (fold(&artists[i].name) == *artist || t.album.to_lowercase() == *album_title))
        });
        songs.truncate(SONGS_LISTED);
        if songs.is_empty() {
            note(format!("left out {} - {}: Roon has no songs of it", artists[i].name, album.title));
            continue;
        }
        albums.push(Album { artist: i, album, songs, about: Default::default(), sound: Vec::new() });
    }
    let hearing = crate::sound::Sound::ready(app);
    let jobs: Vec<Job<crate::judge::AlbumBrief>> = albums
        .iter()
        .map(|al| {
            let (app, client, key, discogs) = (app.clone(), client.clone(), key.clone(), discogs.clone());
            let (name, album) = (artists[al.artist].name.clone(), al.album.clone());
            Box::pin(async move { crate::judge::album_brief(&app, &client, &key, &discogs, &name, &album.title, album.year, Some(&album.handle)).await }) as Job<_>
        })
        .collect();
    // (while tier 3 listens, its progress is the one shown)
    let reading_tick = |done: usize, total: usize| {
        if !hearing {
            progress(format!("Reading the albums... {done} of {total}"));
        }
    };
    let (abouts, sounds) = tokio::join!(run_all(4, jobs, &reading_tick), async {
        if hearing {
            listen(app, &client, &picks, &albums, &artists, progress).await
        } else {
            None
        }
    });
    for (album, about) in albums.iter_mut().zip(abouts) {
        album.about = about.unwrap_or_default();
    }
    if let Some(sounds) = sounds {
        for (album, sound) in albums.iter_mut().zip(sounds) {
            album.sound = sound;
        }
    }
    let batches: Vec<Vec<usize>> = (0..albums.len()).collect::<Vec<_>>().chunks(ALBUMS_PER_CALL).map(|c| c.to_vec()).collect();
    let jobs: Vec<Job<Result<Vec<(usize, usize, String)>, String>>> = batches
        .iter()
        .map(|batch| {
            let blocks: Vec<String> = batch.iter().map(|b| albums[*b].block(&artists[albums[*b].artist])).collect();
            let counts: Vec<usize> = batch.iter().map(|b| albums[*b].songs.len()).collect();
            let (llm, brief) = (llm.clone(), brief.clone());
            Box::pin(async move { crate::judge::dive_album_songs(&llm, &brief, &blocks, &counts, songs_each).await }) as Job<_>
        })
        .collect();
    let answers = run_all(at_once, jobs, &|done, total| progress(format!("Choosing songs on the albums... {done} of {total}"))).await;
    // (album, song, why)
    let mut cands: Vec<(usize, usize, String)> = Vec::new();
    for (batch, answer) in batches.iter().zip(answers) {
        match answer {
            Some(Ok(list)) => cands.extend(list.into_iter().map(|(n, m, why)| (batch[n], m, why))),
            other => {
                note(format!("songs not chosen on {} albums ({}); their best-known songs go on", batch.len(), match other {
                    Some(Err(e)) => e,
                    _ => "no answer".into(),
                }));
                for &b in batch {
                    let album = &albums[b];
                    let artist = &artists[album.artist];
                    let mut order: Vec<usize> = (0..album.songs.len()).collect();
                    order.sort_by_key(|n| !artist.popular.contains(&album.songs[*n].title.to_lowercase()));
                    cands.extend(order.into_iter().take(songs_each).map(|n| (b, n, String::new())));
                }
            }
        }
    }
    for (b, album) in albums.iter().enumerate() {
        let chosen: Vec<String> = cands.iter().filter(|(x, _, _)| *x == b).map(|(_, m, why)| format!("{} ({why})", album.songs[*m].title)).collect();
        note(format!("songs on {} - {}: {}", artists[album.artist].name, album.album.title, if chosen.is_empty() { "none fit".to_string() } else { chosen.join("; ") }));
    }

    // 4. The playlist: tier 2 chooses among all of them and puts them in order
    let lines: Vec<String> = cands
        .iter()
        .map(|(b, m, why)| {
            let album = &albums[*b];
            let artist = &artists[album.artist];
            let t = &album.songs[*m];
            let year = t.year.map(|y| format!(", {y}")).unwrap_or_default();
            let whence = if artist.in_library { "in their library" } else { "new to them" };
            let why = if why.is_empty() { String::new() } else { format!(": {why}") };
            format!("{} - {} ({}{year}, {}) [{whence}]{why}{}", artist.name, t.title, t.album, length(t.length_seconds), album.heard(*m))
        })
        .collect();
    note(format!("{} candidate songs from {} albums of {} artists", lines.len(), albums.len(), artists.len()));
    if lines.is_empty() {
        return Err("Tier 2 found no songs on those albums that fit. Try other words, or another Where from.".into());
    }
    // (many candidates: each part narrowed down first, then the final pick)
    let mut pool: Vec<usize> = (0..lines.len()).collect();
    if pool.len() > CHOOSE_AT_ONCE {
        let parts: Vec<Vec<usize>> = pool.chunks(CHOOSE_AT_ONCE).map(|c| c.to_vec()).collect();
        let jobs: Vec<Job<Result<(String, Vec<(usize, String)>), String>>> = parts
            .iter()
            .map(|part| {
                let part_lines: Vec<String> = part.iter().map(|i| lines[*i].clone()).collect();
                let (llm, brief, want) = (llm.clone(), brief.clone(), count.min(part.len()));
                Box::pin(async move { crate::judge::dive_choose(&llm, &brief, &part_lines, want, per_artist).await }) as Job<_>
            })
            .collect();
        let answers = run_all(at_once, jobs, &|done, total| progress(format!("Choosing the songs... part {done} of {total}"))).await;
        let mut kept: Vec<usize> = Vec::new();
        for (n, (part, answer)) in parts.iter().zip(answers).enumerate() {
            match answer {
                Some(Ok((_, chosen))) => kept.extend(chosen.into_iter().map(|(i, _)| part[i])),
                _ => {
                    note(format!("part {} not narrowed down; all of it goes on", n + 1));
                    kept.extend(part.iter().copied());
                }
            }
        }
        pool = kept;
    }
    progress("Choosing the songs and their order...".into());
    let pool_lines: Vec<String> = pool.iter().map(|i| lines[*i].clone()).collect();
    let (title, chosen) = crate::judge::dive_choose(&llm, &brief, &pool_lines, count, per_artist).await?;
    // (the limits kept, whatever the answer)
    let mut per: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut songs: Vec<(usize, usize, String)> = Vec::new();
    for (i, why) in chosen {
        let (b, m, _) = &cands[pool[i]];
        let n = per.entry(albums[*b].artist).or_default();
        if *n >= per_artist || songs.len() >= count {
            continue;
        }
        *n += 1;
        songs.push((*b, *m, why));
    }
    if songs.is_empty() {
        return Err("Tier 2 found nothing among the candidates that fits. Try other words, or another Where from.".into());
    }
    note("the playlist:".into());
    for (b, m, why) in &songs {
        let t = &albums[*b].songs[*m];
        note(format!("{} - {} ({}): {why}{}", artists[albums[*b].artist].name, t.title, t.album, albums[*b].heard(*m)));
    }
    let title = if title.trim().is_empty() { clip(text, 40) } else { title };
    let title = if title.trim().is_empty() { "Deep Dive".to_string() } else { title };
    let name = format!("🧠 {}", title.trim());
    progress("Saving the playlist...".into());
    let handles: Vec<String> = songs.iter().map(|(b, m, _)| albums[*b].songs[*m].handle.clone()).collect();
    let card = client.create_playlist_described(&name, &clip(text, 200), &handles).await.map_err(|e| format!("Roon didn't save the playlist: {e}"))?;
    Ok(DiveList {
        name,
        request: text.trim().to_string(),
        picks: picks.iter().map(|p| if p.kind == "artist" { p.title.clone() } else { format!("{} by {}", p.title, p.artist) }).collect(),
        built_at: now_ms(),
        playlist_id: card.playlist_id,
        songs: songs
            .iter()
            .map(|(b, m, why)| {
                let t = &albums[*b].songs[*m];
                let artist = &artists[albums[*b].artist];
                let sound = albums[*b].sound.get(*m).and_then(|s| s.as_ref()).and_then(|s| s.close);
                DiveSong { title: t.title.clone(), artist: artist.name.clone(), album: t.album.clone(), year: t.year, why: why.clone(), in_library: artist.in_library, sound }
            })
            .collect(),
        pick_list: picks.clone(),
        talk: history.to_vec(),
        plan: plan.clone(),
        follows: before.map(|b| b.built_at),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_artists_asked_in_parts() {
        // (each split covers A to Z, in order, with no gaps or overlaps)
        for n in 1..=NEW_PARTS_MAX {
            let parts = alphabet_parts(n);
            assert_eq!(parts.len(), n);
            assert_eq!(parts[0].0, 'A');
            assert_eq!(parts[n - 1].1, 'Z');
            for pair in parts.windows(2) {
                assert_eq!(pair[0].1 as u32 + 1, pair[1].0 as u32, "{parts:?}");
            }
        }
        // (30 songs one per artist: about 53 wanted, 5 parts of 11)
        let want: usize = 45 + 8;
        let parts = want.div_ceil(NEW_PER_PART).clamp(1, NEW_PARTS_MAX);
        assert_eq!((parts, want.div_ceil(parts)), (5, 11));
        // (the words: none for a single ask; the last part takes the rest)
        assert!(part_words(0, 1, 'A', 'Z').is_empty());
        assert!(part_words(0, 5, 'A', 'C').contains("start with A to C ("));
        assert!(part_words(4, 5, 'S', 'Z').contains("S to Z, or with a digit"));
    }
}
