//! The weekly playlists, Wrapped's picking mode at its simplest:
//!
//! - 🌟 Discover Weekly: artists you don't have and have never played,
//!   from Last.fm's listener neighbours of the artists you've played most
//!   lately (each neighbour scored by how close it is to how many of
//!   them), one top song each.
//! - 💎 Library Rediscover: artists in your library you've never played
//!   (about three quarters) or not for a year or more (a quarter, more
//!   when the never-played run short), the ones nearest your current
//!   listening first, one library song each.
//!
//! Both are real Roon playlists, rebuilt weekly (Monday, like Spotify's;
//! the schedule comes in a later step). The last two stay, renamed with
//! their date ("🌟 Discover Weekly, Oct 12"); older ones are deleted.
//! Artists from those two aren't picked again. What was built is kept in
//! `wrapped-discover.json`, with the Roon playlist ids (the session's
//! handles change).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use roon_library::{LibraryClient, MixOptions, PlaylistCard};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::library::Library;
use crate::tags::{sound_match, Tag, TagKind, Tags};
use crate::wrapped::{counts, read_json, write_json, LibraryCopy, Wrapped, WrappedSettings};

/// Songs in each playlist (one per artist).
const SONGS: usize = 30;
/// Old copies kept besides the current one.
const KEEP_OLD: usize = 2;
/// Artists the lists start from: the most played lately, this many deep
/// (the top ones' close neighbours are often all known; the lists keep
/// going down the played artists until they fill).
const SEEDS_DEEP: usize = 40;
/// Artists from this many past lists (of each kind) aren't picked again.
const REMEMBER_LISTS: usize = 8;
/// An artist from a list you never played from rests this many lists
/// (you saw it in the queue; it shouldn't bounce straight back).
const REST_UNPLAYED: usize = 2;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Which {
    Weekly,
    Rediscover,
}

impl Which {
    /// The playlist's name in Roon (the dated copies add ", Oct 12").
    fn name(self) -> &'static str {
        match self {
            Which::Weekly => "🌟 Discover Weekly",
            Which::Rediscover => "💎 Library Rediscover",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Which::Weekly => "artists you don't have yet, from the listener neighbours of what you've played most lately",
            Which::Rediscover => "artists in your library you've never played, or not for a year",
        }
    }
}

/// One built playlist, as kept on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Built {
    /// Roon's durable id for the playlist (hex).
    playlist_id: Option<String>,
    name: String,
    built_at: i64,
    /// "Oct 19", in the PC's time zone when it was built.
    date: String,
    songs: usize,
    artists: Vec<String>,
    /// Each song's Roon track id and the artist it was picked for, so the
    /// player can tell a Discover song is playing (and whose it is).
    tracks: Vec<ListSong>,
    /// The first songs' album covers (image keys), for the card when Roon
    /// hasn't made the playlist's own cover yet.
    covers: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListSong {
    pub track_id: String,
    pub artist: String,
    /// (to know the song by when Roon plays it under another track id)
    pub title: String,
    /// "Suggested because you listened to Amon Amarth", for the player.
    pub reason: String,
    /// The artists of yours it came from (named in `reason`), to open.
    pub because: Vec<String>,
    /// Tier 2's one line about the pick ("Gothenburg-style melodic death
    /// with clean choruses"), empty without it.
    pub why: String,
}

/// Your rating of an artist a list brought you: thumbs (1 up, -1 down,
/// 0 none) and whether you hearted its song.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Vote {
    pub name: String,
    pub vote: i8,
    pub heart: bool,
    pub at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Store {
    /// Newest first: the current one, then the dated copies.
    weekly: Vec<Built>,
    rediscover: Vec<Built>,
    /// The PC's offset from UTC in minutes (as JavaScript's
    /// getTimezoneOffset gives it, negated), for dates worked out here.
    utc_offset_minutes: i32,
    /// Ratings by artist (lower-cased name).
    votes: HashMap<String, Vote>,
    /// Songs and artists from lists already deleted (the last REMEMBER_LISTS
    /// lists' artists, and every song ever, stay out).
    past: Vec<Past>,
    /// Songs of the lists that came up on a zone (seen by Toasted), with
    /// how much of each was heard: Roon's history has no trace of a song
    /// skipped in its first seconds.
    shown: Vec<Shown>,
    /// "Start fresh" (Unix ms): the lists from before it don't keep their
    /// songs and artists out any more. Their songs still don't count as
    /// listening (no feeding on itself).
    reset_at: i64,
    /// What you asked the next Discover Weekly for (oldest first): used by
    /// the next one, or by every one when permanent.
    requests: Vec<Request>,
}

/// Something you asked Discover Weekly for: an aspect of a song (from the
/// player's brain button), your own words, or both.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Request {
    /// When it was asked (Unix ms), which also tells it apart.
    pub id: i64,
    pub text: String,
    pub song: Option<RequestSong>,
    /// What about the song: "guitars", "vocals", "rhythm", "mood",
    /// "production"; none (or "everything") for all of it.
    pub aspects: Vec<String>,
    /// Kept for every list, not just the next one.
    pub permanent: bool,
    /// Which list it's for: "rediscover", else Discover Weekly.
    pub list: String,
}

impl Request {
    fn which(&self) -> Which {
        if self.list == "rediscover" {
            Which::Rediscover
        } else {
            Which::Weekly
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RequestSong {
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// A request as the page shows it, with the words tier 2 reads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCard {
    #[serde(flatten)]
    pub request: Request,
    pub said: String,
}

/// At most this many requests stand at once.
const REQUESTS_MAX: usize = 10;
/// Tier 2's own artists for the requests, about this many in all (3 to 6
/// per request).
const REQUEST_SHARE: usize = 15;

fn and_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// "the guitars and the vocals of "Hero" by Bolzer (from Lese Majesty):
/// rawer", or your own words as they are. `album`: with the song's album.
fn describe(r: &Request, album: bool) -> String {
    let text = r.text.trim();
    let Some(song) = r.song.as_ref().filter(|s| !s.title.trim().is_empty()) else { return text.to_string() };
    let parts: Vec<String> = r
        .aspects
        .iter()
        .filter_map(|a| match a.as_str() {
            "guitars" => Some("the guitars"),
            "vocals" => Some("the vocals"),
            "rhythm" => Some("the drums and bass"),
            "mood" => Some("the mood"),
            "production" => Some("the production"),
            _ => None,
        })
        .map(String::from)
        .collect();
    let mut out = if parts.is_empty() { format!("songs like \"{}\"", song.title.trim()) } else { format!("{} of \"{}\"", and_list(&parts), song.title.trim()) };
    if !song.artist.trim().is_empty() {
        out.push_str(&format!(" by {}", song.artist.trim()));
    }
    if album && !song.album.trim().is_empty() {
        out.push_str(&format!(" (from {})", song.album.trim()));
    }
    if !text.is_empty() {
        out.push_str(&format!(": {text}"));
    }
    out
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Shown {
    artist: String,
    title: String,
    /// When it came up (Unix ms).
    at: i64,
    /// How far into it playback got before the next song, in seconds.
    heard: f64,
}

/// A song that came up and went within this many seconds was skipped.
const SKIPPED_WITHIN: f64 = 30.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Past {
    which: Which,
    artist: String,
    track_id: String,
    title: String,
    built_at: i64,
}

impl Default for Which {
    fn default() -> Self {
        Which::Weekly
    }
}

impl Store {
    fn lists(&self, which: Which) -> &Vec<Built> {
        match which {
            Which::Weekly => &self.weekly,
            Which::Rediscover => &self.rediscover,
        }
    }

    fn list(&mut self, which: Which) -> &mut Vec<Built> {
        match which {
            Which::Weekly => &mut self.weekly,
            Which::Rediscover => &mut self.rediscover,
        }
    }
}

#[derive(Default)]
pub struct Discover {
    store: Mutex<Option<Store>>,
    building: Mutex<Option<Which>>,
    /// Why the last build of each failed, until one works.
    failed: Mutex<HashMap<String, String>>,
}

impl Discover {
    fn path(app: &AppHandle) -> Option<PathBuf> {
        app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-discover.json"))
    }

    fn with_store<T>(&self, app: &AppHandle, f: impl FnOnce(&mut Store) -> T) -> T {
        let mut guard = self.store.lock().unwrap();
        let store = guard.get_or_insert_with(|| Self::path(app).and_then(|p| read_json(&p)).unwrap_or_default());
        f(store)
    }

    fn save(&self, app: &AppHandle) {
        let Some(path) = Self::path(app) else { return };
        let guard = self.store.lock().unwrap();
        if let Some(store) = guard.as_ref() {
            write_json(&path, store);
        }
    }
}

/// What's on each zone, for telling a list's song heard from one skipped.
struct OnZone {
    title: String,
    artist: String,
    album: String,
    /// When it came up (Unix ms).
    at: i64,
    /// The furthest position reached, in seconds.
    heard: f64,
    /// The song before it on the zone (artist, title), if one was seen.
    before: Option<(String, String)>,
}

/// A song that finished on a zone, with its neighbours: (artist, title).
struct Finished {
    artist: String,
    title: String,
    at: i64,
    heard: f64,
    before: Option<(String, String)>,
    after: (String, String),
}

static ON_ZONES: Mutex<Option<HashMap<String, OnZone>>> = Mutex::new(None);

/// zones.rs, after every zone update: a zone that moved on to another song
/// is done with the one before; if that was a list's song, it's noted with
/// how much of it was heard.
pub fn zones_updated(app: &AppHandle, payload: &crate::zones::ZonesPayload) {
    if !enabled(app) {
        return;
    }
    let mut done: Vec<Finished> = Vec::new();
    {
        let mut guard = ON_ZONES.lock().unwrap();
        let zones = guard.get_or_insert_with(HashMap::new);
        for z in &payload.zones {
            let Some(np) = z.now_playing.as_ref() else { continue };
            if np.title.is_empty() {
                continue;
            }
            let artist = crate::zones::main_artist(&np.artist_full).to_string();
            let pos = z.seek_position.unwrap_or(0.0);
            let fresh = |before: Option<(String, String)>| OnZone { title: np.title.clone(), artist: artist.clone(), album: np.album.clone(), at: now_ms(), heard: pos, before };
            match zones.get_mut(&z.zone_id) {
                Some(cur) if cur.title == np.title && cur.artist == artist && cur.album == np.album => cur.heard = cur.heard.max(pos),
                Some(cur) => {
                    let next = fresh(Some((cur.artist.clone(), cur.title.clone())));
                    let old = std::mem::replace(cur, next);
                    done.push(Finished { artist: old.artist, title: old.title, at: old.at, heard: old.heard, before: old.before, after: (artist.clone(), np.title.clone()) });
                }
                None => {
                    zones.insert(z.zone_id.clone(), fresh(None));
                }
            }
        }
    }
    if !done.is_empty() {
        note_shown(app, done);
    }
}

/// Wrapped Discovery on or off (Wrapped's settings), kept here so the zone
/// watcher doesn't read the file on every update: 0 not read yet, 1 on, 2 off.
static ENABLED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn enabled(app: &AppHandle) -> bool {
    use std::sync::atomic::Ordering;
    match ENABLED.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = WrappedSettings::load(app).discovery;
            set_enabled(on);
            on
        }
    }
}

/// wrapped.rs, when the setting changes. Off also forgets what's on the
/// zones, so nothing half-watched is noted when it's turned back on.
pub fn set_enabled(on: bool) {
    ENABLED.store(if on { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
    if !on {
        *ON_ZONES.lock().unwrap() = None;
    }
}

/// zones.rs, on the once-a-second position updates.
pub fn seeked(zone_id: &str, position: Option<f64>) {
    let Some(pos) = position else { return };
    if let Some(cur) = ON_ZONES.lock().unwrap().as_mut().and_then(|z| z.get_mut(zone_id)) {
        cur.heard = cur.heard.max(pos);
    }
}

/// A list's song by title, and the artist one way or the other ("A / B"
/// credits).
fn is_song(song: &ListSong, artist: &str, title: &str) -> bool {
    let (a, sa) = (artist.to_lowercase(), song.artist.to_lowercase());
    song.title.eq_ignore_ascii_case(title) && (sa == a || sa.contains(&a) || a.contains(&sa))
}

/// On one of the lists (each list's songs), with the song before or after
/// it on the same list.
fn played_from_list<'a>(mut lists: impl Iterator<Item = &'a [ListSong]>, f: &Finished) -> bool {
    lists.any(|tracks| {
        let has = |artist: &str, title: &str| tracks.iter().any(|song| is_song(song, artist, title));
        has(&f.artist, &f.title) && (has(&f.after.0, &f.after.1) || f.before.as_ref().is_some_and(|(a, t)| has(a, t)))
    })
}

/// Keeps the finished songs that were played from a list: on a list, with
/// the song before or after it on the same list (a list's song played from
/// anywhere else, a Deep Dive or some other playlist, is ordinary listening,
/// which the history has).
fn note_shown(app: &AppHandle, done: Vec<Finished>) {
    let discover = app.state::<Discover>();
    let noted = discover.with_store(app, |s| {
        let mut noted = Vec::new();
        for f in done {
            if played_from_list(s.weekly.iter().chain(s.rediscover.iter()).map(|b| b.tracks.as_slice()), &f) {
                noted.push(format!("{} - {}: {:.0} s", f.artist, f.title, f.heard));
                s.shown.push(Shown { artist: f.artist, title: f.title, at: f.at, heard: f.heard });
            }
        }
        // (a year's worth is plenty)
        let cutoff = now_ms() - 365 * DAY_MS;
        s.shown.retain(|x| x.at >= cutoff);
        noted
    });
    if !noted.is_empty() {
        discover.save(app);
        note(format!("list songs heard: {}", noted.join("; ")));
    }
}

/// A built playlist as the page shows it: what was built, and Roon's
/// playlist when it's still there.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltCard {
    pub name: String,
    pub built_at: i64,
    pub date: String,
    pub songs: usize,
    pub artists: Vec<String>,
    pub playlist: Option<PlaylistCard>,
    /// The songs in order, for the card's track list.
    pub tracks: Vec<ListSong>,
    /// The first songs' album covers, when the playlist has no cover of its
    /// own (yet).
    pub covers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverStatus {
    /// Newest first: the current one, then the dated copies.
    pub weekly: Vec<BuiltCard>,
    pub rediscover: Vec<BuiltCard>,
    pub building: Option<Which>,
    pub weekly_error: Option<String>,
    pub rediscover_error: Option<String>,
    /// Discover Weekly's Fine Tuning, and Library Rediscover's.
    pub requests: Vec<RequestCard>,
    pub rediscover_requests: Vec<RequestCard>,
    /// Tier 2 is set up (requests need it).
    pub tier2: bool,
    /// Tier 3 is set up (its models downloaded).
    pub tier3: bool,
}

/// The current build's notes: printed, and written to `discover-log.txt`
/// in the app data folder when it ends (what was picked and left out, and
/// why it failed, without the terminal).
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(line: String) {
    println!("[discover] {line}");
    LOG.lock().unwrap().push(line);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The calendar date (year, month 1-12, day) of a Unix day number (days
/// since 1970-01-01), Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// "Oct 19" for a moment, in the PC's time zone (`offset` minutes east of UTC).
fn short_date(ms: i64, offset_minutes: i32) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let local = ms + i64::from(offset_minutes) * 60_000;
    let (_, month, day) = civil_from_days(local.div_euclid(DAY_MS));
    format!("{} {day}", MONTHS[(month - 1) as usize])
}

/// A small random source (xorshift, seeded from the clock): the picks get a
/// little shuffle so a week's list isn't only the same top scores.
struct Jitter(u64);

impl Jitter {
    fn new() -> Self {
        Jitter((now_ms() as u64) ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    /// 0.0 up to 1.0.
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// What the history says about an artist.
#[derive(Debug, Clone, Default)]
pub(crate) struct Heard {
    name: String,
    performer_id: Option<u64>,
    /// Listens (half a track or more) in the last year, ever.
    year: u32,
    listens: u32,
    /// The last 60 days' listens, each counted less the older it is (half
    /// after two weeks): what's being played now, not a month-old binge.
    /// Plays from the Discover lists themselves don't count (see `Context`).
    fresh: f64,
    /// How much of that was on purpose: the artist in Roon's Recent
    /// Activity (you started its album, song or page yourself, not a
    /// library shuffle), 1.0 for today, half after two weeks.
    intent: f64,
    /// Every play, skipped ones too.
    plays: u32,
    last_played: i64,
    /// The albums you played (lower-cased title): the seed as you listen
    /// to it, early Ulver or late Ulver, not the band's whole career.
    albums: HashMap<String, PlayedAlbum>,
}

#[derive(Debug, Clone, Default)]
struct PlayedAlbum {
    name: String,
    album_id: Option<u64>,
    /// Listens, recent ones counting more (like `fresh`, over a year).
    weight: f64,
}

/// Half-life of a listen, and of a Recent Activity entry, in days.
const HALF_LIFE_DAYS: f64 = 14.0;
/// An artist in Recent Activity counts up to this much more as a seed (2.5
/// times, started today; 1.75 times two weeks ago).
const INTENT_BOOST: f64 = 1.5;
/// Two plays this close together (start to start) are one listening
/// session, for telling a Discover list played through from a song of it
/// played on its own.
const SESSION_GAP_MS: i64 = 20 * 60 * 1000;

/// What the build knows beyond the plays themselves: which plays came from
/// the Discover lists (they don't feed the next list: a list you play
/// every week would otherwise keep feeding itself), and which artists you
/// started on purpose (Roon's Recent Activity).
#[derive(Default)]
struct Context {
    /// (played at, lower-cased title) of the plays from a list.
    from_lists: HashSet<(i64, String)>,
    /// Lower-cased artist: 0.0 up to 1.0, by how recently.
    intent: HashMap<String, f64>,
}

/// The plays that came from a Discover list: a song of a list, played
/// after the list was made, next to another song of the same list (the
/// play before or after it, in the same session). The same song played on
/// its own, or from its album, is a play like any other.
fn plays_from_lists(app: &AppHandle, lists: &[(String, String, String, String, i64)]) -> HashSet<(i64, String)> {
    // (track id or "artist|title" -> the lists (key, built at) holding it)
    let mut holding: HashMap<String, Vec<(&str, i64)>> = HashMap::new();
    for (list, track_id, artist, title, at) in lists {
        if !track_id.is_empty() {
            holding.entry(format!("id:{track_id}")).or_default().push((list, *at));
        }
        holding.entry(format!("{}|{}", artist.to_lowercase(), title.to_lowercase())).or_default().push((list, *at));
    }
    let oldest = lists.iter().map(|l| l.4).min().unwrap_or(i64::MAX);
    app.state::<Wrapped>().with_plays(app, |plays| {
        // (newest first: stop at the oldest list)
        let recent: Vec<&roon_library::Play> = plays.iter().take_while(|p| p.played_at >= oldest).collect();
        let lists_of = |p: &roon_library::Play| -> Vec<&str> {
            let mut out: Vec<&str> = Vec::new();
            let by_id = p.track_id.map(|id| format!("id:{id}"));
            let by_name = format!("{}|{}", p.artist.to_lowercase(), p.title.to_lowercase());
            for key in by_id.iter().chain(std::iter::once(&by_name)) {
                for (list, at) in holding.get(key).into_iter().flatten() {
                    if p.played_at >= *at && !out.contains(list) {
                        out.push(list);
                    }
                }
            }
            out
        };
        let mut out = HashSet::new();
        for (i, p) in recent.iter().enumerate() {
            let mine = lists_of(p);
            if mine.is_empty() {
                continue;
            }
            let next_to = |j: Option<usize>| {
                j.and_then(|j| recent.get(j)).is_some_and(|q| (q.played_at - p.played_at).abs() <= SESSION_GAP_MS && lists_of(q).iter().any(|l| mine.contains(l)))
            };
            if next_to(i.checked_sub(1)) || next_to(Some(i + 1)) {
                out.insert((p.played_at, p.title.to_lowercase()));
            }
        }
        out
    })
}

/// Roon's Recent Activity, as how recently each artist was started on
/// purpose. The call's mode strings aren't known, so a few are tried and
/// what each gives is logged; the first with artists in it is used.
async fn intent_from_recents(client: &LibraryClient, now: i64) -> HashMap<String, f64> {
    let mut out: HashMap<String, f64> = HashMap::new();
    for mode in ["", "all", "home", "played", "play", "recent", "activity"] {
        match client.recents(mode).await {
            Ok((entries, shape)) => {
                let sample: Vec<String> = entries.iter().take(6).map(|e| format!("{}/{} {} - {}", e.kind, e.data_type, e.artist, e.title)).collect();
                note(format!("recent activity, mode {mode:?}: {} entries ({shape}) {}", entries.len(), sample.join("; ")));
                if !out.is_empty() {
                    continue;
                }
                for e in entries.iter().filter(|e| !e.artist.is_empty()) {
                    // (no time: counted as two weeks old)
                    let age = if e.at > 0 { (now - e.at).max(0) } else { HALF_LIFE_DAYS as i64 * DAY_MS };
                    if age > 60 * DAY_MS {
                        continue;
                    }
                    let w = 0.5f64.powf(age as f64 / (HALF_LIFE_DAYS * DAY_MS as f64));
                    let row = out.entry(e.artist.to_lowercase()).or_insert(0.0);
                    *row = row.max(w);
                }
            }
            Err(e) => note(format!("recent activity, mode {mode:?}: {e}")),
        }
    }
    if !out.is_empty() {
        let mut names: Vec<(&String, &f64)> = out.iter().collect();
        names.sort_by(|a, b| b.1.total_cmp(a.1));
        note(format!("started on purpose lately: {}", names.iter().map(|(k, w)| format!("{k} {w:.2}")).collect::<Vec<_>>().join(", ")));
    }
    out
}

/// Deep Dive (dive.rs): every artist in the history (name, listens), most
/// played first.
pub(crate) fn played_artists(app: &AppHandle) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = history_by_artist(app, now_ms(), &Context::default()).into_values().filter(|h| !h.name.is_empty()).map(|h| (h.name, h.listens)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// Deep Dive: the artists given a thumbs down (lower-cased).
pub(crate) fn turned_down(app: &AppHandle) -> HashSet<String> {
    app.state::<Discover>().with_store(app, |s| s.votes.iter().filter(|(_, v)| v.vote < 0).map(|(k, _)| k.clone()).collect())
}

fn history_by_artist(app: &AppHandle, now: i64, ctx: &Context) -> HashMap<String, Heard> {
    app.state::<Wrapped>().with_plays(app, |plays| {
        let mut out: HashMap<String, Heard> = HashMap::new();
        for p in plays {
            if p.artist.is_empty() || ctx.from_lists.contains(&(p.played_at, p.title.to_lowercase())) {
                continue;
            }
            let row = out.entry(p.artist.to_lowercase()).or_insert_with(|| Heard { name: p.artist.clone(), ..Default::default() });
            row.plays += 1;
            row.last_played = row.last_played.max(p.played_at);
            if row.performer_id.is_none() {
                row.performer_id = p.performer_id;
            }
            if counts(p) {
                row.listens += 1;
                let age = now - p.played_at;
                if age <= 60 * DAY_MS {
                    row.fresh += 0.5f64.powf(age as f64 / (HALF_LIFE_DAYS * DAY_MS as f64));
                }
                if age <= 365 * DAY_MS {
                    row.year += 1;
                    if !p.album.is_empty() {
                        let album = row.albums.entry(p.album.to_lowercase()).or_insert_with(|| PlayedAlbum { name: p.album.clone(), album_id: p.album_id, weight: 0.0 });
                        // (a listen this week counts 1, a half-year-old one about 0.1)
                        album.weight += 0.1 + 0.9 * 0.5f64.powf(age as f64 / (HALF_LIFE_DAYS * DAY_MS as f64));
                    }
                }
            }
        }
        for (k, row) in out.iter_mut() {
            row.intent = ctx.intent.get(k).copied().unwrap_or(0.0);
        }
        out
    })
}

/// Seeds whose suggestions you liked count for more: a quarter more per
/// liked suggestion, up to three. Heaviest first again after.
fn give_credit(seeds: &mut [(Heard, f64)], credit: &HashMap<String, u32>) {
    let mut noted = Vec::new();
    for (h, w) in seeds.iter_mut() {
        if let Some(n) = credit.get(&h.name.to_lowercase()) {
            *w *= 1.0 + 0.25 * (*n).min(3) as f64;
            noted.push(format!("{} x{}", h.name, n));
        }
    }
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1));
    if !noted.is_empty() {
        note(format!("good suggestions credited to: {}", noted.join(", ")));
    }
}

/// An artist's weight as a seed: its recent listens, more when you started
/// it yourself (Recent Activity) than when it came up on a shuffle.
fn seed_weight(h: &Heard) -> f64 {
    let base = if h.intent > 0.0 { h.fresh.max(1.0) } else { h.fresh };
    base * (1.0 + INTENT_BOOST * h.intent.min(1.0))
}

/// The artists the lists start from: the most played lately, recent plays
/// counting most (the last year when the last 60 days are too few), with
/// their weights. A few extra, for the ones Last.fm knows nothing about.
fn seeds(heard: &HashMap<String, Heard>) -> Vec<(Heard, f64)> {
    let usable = |h: &&Heard| h.name.to_lowercase() != "various artists";
    let mut rows: Vec<(Heard, f64)> =
        heard.values().filter(usable).filter(|h| h.fresh >= 1.0 || (h.intent > 0.0 && h.fresh > 0.0)).map(|h| (h.clone(), seed_weight(h))).collect();
    if rows.len() < 5 {
        rows = heard.values().filter(usable).filter(|h| h.year >= 2).map(|h| (h.clone(), h.year as f64)).collect();
    }
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    rows.truncate(SEEDS_DEEP);
    rows
}

/// The seeds' Last.fm neighbours, each seed's list on its own (the lists
/// are taken from in turns, so one corner of the listening, five death
/// metal bands that all share neighbours, doesn't fill the whole list).
struct Neighbours {
    /// The seeds Last.fm had neighbours for (at most SEEDS_DEEP), heaviest first.
    seeds: Vec<(Heard, f64)>,
    /// Per seed: (lower-cased name, name, match), closest first.
    lists: Vec<Vec<(String, String, f64)>>,
    /// Which seeds list an artist.
    listed_by: HashMap<String, Vec<usize>>,
}

async fn neighbours(app: &AppHandle, seeds: Vec<(Heard, f64)>, cap: usize, key: &str, progress: &impl Fn(String)) -> Neighbours {
    let tags = app.state::<Tags>();
    let mut out = Neighbours { seeds: Vec::new(), lists: Vec::new(), listed_by: HashMap::new() };
    let total = seeds.len().min(cap);
    for (seed, weight) in seeds {
        if out.seeds.len() >= cap {
            break;
        }
        progress(format!("Asking Last.fm about {} ({} of {total})...", seed.name, out.seeds.len() + 1));
        let list: Vec<(String, String, f64)> = tags.lookup_similar(app, &seed.name, key).await.into_iter().map(|(name, m)| (name.to_lowercase(), name, m)).collect();
        if list.is_empty() {
            continue;
        }
        let i = out.seeds.len();
        for (k, _, _) in &list {
            out.listed_by.entry(k.clone()).or_default().push(i);
        }
        out.seeds.push((seed, weight));
        out.lists.push(list);
    }
    out
}

/// Below this tag match a neighbour doesn't sound enough like the seed it
/// came from, however many listeners they share (against Amon Amarth:
/// Heaven Shall Burn's metalcore 0.36, Avatar 0.54, The Halo Effect 0.72).
const MIN_SOUND: f64 = 0.6;
/// Below this Last.fm match a neighbour is noise (Tool's 158th neighbour
/// at 0.02 isn't a Tool-like band); the radio's seeds sit at 0.3 and up.
const MIN_MATCH: f64 = 0.12;
/// Below this score (match x 1.5 + sound x 0.5) a pick is the fringe of a
/// seed whose real neighbours you already have: Rob Zombie's close ones
/// known, what's left is Eisbrecher at 0.61. The radio's picks sit at
/// 1.2 and up; a seed with nothing above it gives nothing this week.
const MIN_SCORE: f64 = 1.2;
/// A seed with nothing above MIN_SCORE may still give its best down to
/// this (Slugdge, Carpenter Brut: real neighbours, just less overlap).
const MIN_SCORE_LOW: f64 = 1.0;
/// Artists one seed may put in a list, so no corner of the listening
/// (Morbid Angel's deep and untouched list) fills it.
const PER_SEED_MAX: usize = 2;
/// A seed is named in "Suggested because you listened to" only when the
/// artist is among this many of its closest neighbours.
const NAME_WITHIN: usize = 25;
/// How many of each seed's closest eligible neighbours are checked.
const PER_SEED: usize = 14;
/// With tier 2 judging, tier 1 can dig deeper and hand over more: this many
/// neighbours checked per seed, this many taken (tier 2 drops a good share,
/// and its later rounds need candidates left to try).
const PER_SEED_DEEP: usize = 24;
const PER_SEED_MAX_DEEP: usize = 3;

/// A seed as you listen to it: the tags and era of the albums you played
/// (the band's own when none of its albums is tagged or dated), and those
/// albums' names, most played first.
#[derive(Debug, Clone, Default)]
struct SeedView {
    tags: Vec<Tag>,
    era: Option<(u32, u32)>,
    albums: Vec<String>,
}

/// How many of a seed's most played albums stand for it.
const SEED_ALBUMS: usize = 3;

async fn seed_view(app: &AppHandle, client: &LibraryClient, key: &str, seed: &Heard) -> SeedView {
    let tags = app.state::<Tags>();
    let mut albums: Vec<&PlayedAlbum> = seed.albums.values().collect();
    albums.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    albums.truncate(SEED_ALBUMS);
    // The albums' tags together, each album's by how much you played it
    let mut sum: HashMap<String, (f64, Tag)> = HashMap::new();
    let mut years: Vec<u32> = Vec::new();
    for album in &albums {
        if !key.is_empty() {
            for t in tags.lookup_album(app, &seed.name, &album.name, key).await.tags {
                let row = sum.entry(t.name.clone()).or_insert_with(|| (0.0, t.clone()));
                row.0 += t.weight as f64 * album.weight;
            }
        }
        if let Some(id) = album.album_id {
            if let Ok(card) = client.album_by_id(id).await {
                years.extend(card.year);
            }
        }
    }
    let top = sum.values().map(|(w, _)| *w).fold(0.0, f64::max);
    let mut merged: Vec<Tag> = sum
        .into_values()
        .map(|(w, mut t)| {
            t.weight = (w / top * 100.0).round() as u32;
            t
        })
        .filter(|t| t.weight > 0)
        .collect();
    merged.sort_by(|a, b| b.weight.cmp(&a.weight));
    // (an album nobody tagged says nothing: the band's tags then)
    let tags = if merged.len() >= 3 { merged } else { tags.lookup_quick(app, &seed.name, key).await.tags };
    years.sort_unstable();
    let era = match (years.first(), years.last()) {
        (Some(a), Some(b)) => Some((*a, *b)),
        _ => None,
    };
    SeedView { tags, era, albums: albums.iter().map(|a| a.name.clone()).collect() }
}

/// A seed for tier 2: its view, the albums you played (Roon's reviews,
/// their tags, Discogs styles, Last.fm and Wikipedia where Roon has no
/// review), and its bios.
async fn seed_brief(app: &AppHandle, client: &LibraryClient, key: &str, discogs: &str, found: &Neighbours, views: &[SeedView], handles: &[Option<String>], i: usize) -> crate::judge::SeedBrief {
    let Some((heard, _)) = found.seeds.get(i) else { return crate::judge::SeedBrief::default() };
    let view = views.get(i).cloned().unwrap_or_default();
    let mut albums = Vec::new();
    let mut played: Vec<&PlayedAlbum> = heard.albums.values().collect();
    played.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    for album in played.iter().take(SEED_ALBUMS) {
        let card = match album.album_id {
            Some(id) => client.album_by_id(id).await.ok(),
            None => None,
        };
        let year = card.as_ref().and_then(|c| c.year);
        let handle = card.as_ref().map(|c| c.handle.clone());
        albums.push(crate::judge::album_brief(app, client, key, discogs, &heard.name, &album.name, year, handle.as_deref()).await);
    }
    let bio = match handles.get(i).cloned().flatten() {
        Some(h) => client.artist_bio(&h).await.ok().flatten().unwrap_or_default(),
        None => String::new(),
    };
    let texts = app.state::<crate::sources::Sources>().artist(app, &heard.name, key, bio.is_empty()).await;
    crate::judge::SeedBrief { name: heard.name.clone(), tags: view.tags, era: view.era, albums, bio, lastfm_bio: texts.lastfm, wikipedia: texts.wikipedia, ..Default::default() }
}

/// Every seed's view, in the seeds' order (one lookup per seed, cached by
/// Last.fm's tag store between builds).
async fn seed_views(app: &AppHandle, client: &LibraryClient, key: &str, seeds: &[(Heard, f64)], progress: &impl Fn(String)) -> Vec<SeedView> {
    let mut out = Vec::new();
    for (i, (seed, _)) in seeds.iter().enumerate() {
        progress(format!("Reading what you played of {} ({} of {})...", seed.name, i + 1, seeds.len()));
        let view = seed_view(app, client, key, seed).await;
        if !view.albums.is_empty() {
            let era = view.era.map(|(a, b)| format!(" {a}-{b}")).unwrap_or_default();
            note(format!("{} as you play it: {}{era}, top tags {}", seed.name, view.albums.join(", "), view.tags.iter().take(4).map(|t| format!("{} {}", t.name, t.weight)).collect::<Vec<_>>().join(", ")));
        }
        out.push(view);
    }
    out
}

/// The neighbours that pass `eligible` and sound like the seed they come
/// from, up to `want`. Like the radio: each seed's closest eligible
/// neighbours (Last.fm's match, at least MIN_MATCH) are checked by their
/// tags against the seed's (`tags::sound_match`; Last.fm's tags fetched
/// when missing, one request a second) and scored match x 1.5 + sound x
/// 0.5, the sound at least MIN_SOUND. Then the seeds take turns, best
/// score first within each (the heaviest third of seeds gets two turns a
/// round); a seed whose eligible neighbours are all far down its list
/// gives nothing. With the seeds each came from, closest first.
async fn spread(
    app: &AppHandle,
    n: &Neighbours,
    views: &[SeedView],
    eligible: impl Fn(&str) -> bool,
    key: &str,
    want: usize,
    deep: bool,
    avoid: &[Avoid],
    jitter: &mut Jitter,
    progress: &impl Fn(String),
) -> Vec<(String, String, Vec<usize>)> {
    let (per_seed, per_seed_max) = if deep { (PER_SEED_DEEP, PER_SEED_MAX_DEEP) } else { (PER_SEED, PER_SEED_MAX) };
    let tags = app.state::<Tags>();
    // (the sound is judged against the albums you play; what counts as the
    // seed's world, for the telling and defining tags, is those albums'
    // tags and the band's own together: album tags are often thin)
    let seed_tags: Vec<Vec<Tag>> = (0..n.seeds.len()).map(|i| views.get(i).map(|v| v.tags.clone()).unwrap_or_default()).collect();
    let mut seed_world: Vec<Vec<Tag>> = Vec::new();
    for (i, (seed, _)) in n.seeds.iter().enumerate() {
        let mut world = seed_tags[i].clone();
        for t in tags.lookup_quick(app, &seed.name, key).await.tags {
            if !world.iter().any(|w| w.name == t.name) {
                world.push(t);
            }
        }
        seed_world.push(world);
    }
    // Each seed's candidates, scored
    let mut queues: Vec<Vec<(f64, String, String)>> = Vec::new();
    let mut checked = 0;
    let mut considered: HashSet<String> = HashSet::new();
    let mut pushed_noted: HashSet<String> = HashSet::new();
    for (i, list) in n.lists.iter().enumerate() {
        let mut scored = Vec::new();
        for (k, name, m) in list.iter().filter(|(k, _, m)| *m >= MIN_MATCH && eligible(k)).take(per_seed) {
            checked += 1;
            progress(format!("Checking how {name} sounds ({checked} checked)..."));
            let theirs = tags.lookup_quick(app, name, key).await.tags;
            // (a skipped artist itself stays out)
            if let Some(it) = avoid.iter().find(|a| a.name.eq_ignore_ascii_case(name)) {
                if considered.insert(k.clone()) {
                    note(format!("{name}: you {} it", if it.why == SKIPPED { "skipped" } else { "turned down" }));
                }
                continue;
            }
            if let Some(like) = avoid.iter().find(|a| a.why != SKIPPED && sound_match(&a.tags, &theirs) >= a.limit) {
                if considered.insert(k.clone()) {
                    note(format!("{name} sounds too much like {}, which you {}", like.name, like.why));
                }
                continue;
            }
            let skipped_like: Vec<&str> = avoid.iter().filter(|a| a.why == SKIPPED && sound_match(&a.tags, &theirs) >= a.limit).map(|a| a.name.as_str()).collect();
            if skipped_like.len() >= 2 {
                if considered.insert(k.clone()) {
                    note(format!("{name} sounds too much like {} and {}, which you skipped", skipped_like[0], skipped_like[1]));
                }
                continue;
            }
            let push = if skipped_like.is_empty() { 1.0 } else { SKIP_PUSH };
            let sound = if seed_tags[i].is_empty() { MIN_SOUND } else { sound_match(&seed_tags[i], &theirs) };
            if !shares_telling_tag(&seed_world[i], &theirs) {
                if considered.insert(k.clone()) {
                    note(format!("{name} shares nothing telling with {} (only umbrella tags)", n.seeds[i].0.name));
                }
                continue;
            }
            if let Some(foreign) = foreign_defining_tag(&seed_world[i], &theirs) {
                if considered.insert(k.clone()) {
                    note(format!("{name} is {foreign}, which {} isn't", n.seeds[i].0.name));
                }
                continue;
            }
            if sound < MIN_SOUND {
                if considered.insert(k.clone()) {
                    note(format!("{name} doesn't sound like {} ({sound:.2}, match {m:.2})", n.seeds[i].0.name));
                }
                continue;
            }
            let shared = n.listed_by.get(k).map_or(1, Vec::len) as f64;
            let score = (m * 1.5 + sound * 0.5) * (1.0 + 0.1 * (shared - 1.0)) * (0.92 + 0.16 * jitter.next()) * push;
            let pushed = if push < 1.0 { format!(", pushed down: sounds like {}, which you skipped", skipped_like[0]) } else { String::new() };
            if score < MIN_SCORE_LOW {
                if considered.insert(k.clone()) {
                    note(format!("{name} is only the fringe of {} (score {score:.2}, match {m:.2}{pushed})", n.seeds[i].0.name));
                }
                continue;
            }
            if push < 1.0 && pushed_noted.insert(k.clone()) {
                note(format!("{name}{pushed}"));
            }
            scored.push((score, k.clone(), name.clone()));
        }
        // (taken from the end: lowest score first in the vector)
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        queues.push(scored);
    }
    // (a seed's picks under MIN_SCORE only when it has none above: its best
    // is still a real neighbour, its tail isn't)
    for q in &mut queues {
        let strong = q.iter().any(|(score, _, _)| *score >= MIN_SCORE);
        if strong {
            q.retain(|(score, _, _)| *score >= MIN_SCORE);
        }
    }
    let turns: Vec<usize> = (0..n.seeds.len()).collect();
    let mut taken: HashSet<String> = HashSet::new();
    let mut given: Vec<usize> = vec![0; n.seeds.len()];
    let mut out = Vec::new();
    loop {
        let mut any = false;
        for &i in &turns {
            if out.len() >= want {
                return out;
            }
            if given[i] >= per_seed_max {
                continue;
            }
            while let Some((score, k, name)) = queues[i].pop() {
                if !taken.insert(k.clone()) {
                    continue;
                }
                given[i] += 1;
                let mut from: Vec<usize> = n
                    .listed_by
                    .get(&k)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| *s == i || n.lists[*s].iter().take(NAME_WITHIN).any(|(kk, _, _)| *kk == k))
                    .collect();
                from.sort_by_key(|s| (*s != i, *s));
                note(format!("{name} from {} (score {score:.2})", n.seeds[i].0.name));
                out.push((k, name, from));
                any = true;
                break;
            }
        }
        if !any {
            break;
        }
    }
    out
}

/// Tags that say nothing about a sound on their own: the umbrella genres
/// and places. A candidate has to share something more specific with its
/// seed (Septic Flesh's "symphonic death metal", not just "death metal").
const GENERIC: &[&str] = &[
    "metal", "heavy metal", "death metal", "black metal", "doom metal", "thrash metal", "progressive metal", "progressive rock", "rock", "hard rock",
    "nu metal", "alternative metal", "metalcore", "hardcore", "punk", "industrial", "electronic", "seen live", "favorites", "favourites",
];

/// Whether `theirs` shares one of the seed's telling tags: any non-generic,
/// non-scene tag the seed has (when it has none, anything goes).
fn shares_telling_tag(seed: &[Tag], theirs: &[Tag]) -> bool {
    let telling: Vec<&str> = seed
        .iter()
        .filter(|t| matches!(t.kind, TagKind::Genre | TagKind::Style) && !GENERIC.contains(&t.name.as_str()) && !JUNK_TAGS.contains(&t.name.as_str()))
        .map(|t| t.name.as_str())
        .collect();
    telling.is_empty() || theirs.iter().any(|t| telling.contains(&t.name.as_str()))
}

/// A tag that defines the candidate (non-umbrella, at least DEFINING of its
/// strongest tag) and has nothing like it among the seed's: Limp Bizkit's
/// "rapcore" against Deftones, whose one big shared tag (nu metal) made the
/// two look alike. A narrower or wider form of a seed's tag is like it
/// ("technical death metal" for a "death metal" seed).
fn foreign_defining_tag(seed: &[Tag], theirs: &[Tag]) -> Option<String> {
    let counts = |t: &&Tag| matches!(t.kind, TagKind::Genre | TagKind::Style) && !JUNK_TAGS.contains(&t.name.as_str());
    let seed_names: Vec<String> = seed.iter().filter(counts).map(|t| format!(" {} ", t.name.to_lowercase().replace(['-', '/'], " "))).collect();
    if seed_names.is_empty() {
        return None;
    }
    let top = theirs.iter().filter(counts).map(|t| t.weight).max()?;
    theirs
        .iter()
        .filter(counts)
        .filter(|t| !GENERIC.contains(&t.name.as_str()) && t.weight as f64 >= top as f64 * DEFINING && names_a_style(&t.name))
        .find(|t| {
            let mine = format!(" {} ", t.name.to_lowercase().replace(['-', '/'], " "));
            // (a one-word seed tag, "metal", is too broad to count as the
            // wider form of anything)
            !seed_names.iter().any(|s| s.contains(&mine) || (s.trim().contains(' ') && mine.contains(s.as_str())))
        })
        .map(|t| t.name.clone())
}

/// How strong (against a candidate's top tag) a tag has to be to define it.
/// (Half: Mercenary's power metal at 43 is a side of the band, not what it
/// is; which album shows that side, no tag or genre tells. Tier 2's job.)
const DEFINING: f64 = 0.5;

/// Whether a tag names a style of music ("power metal", "rapcore",
/// "shoegaze"), not a band, a place or a word ("lacrimosa", "noruega"),
/// which listeners file under genre too.
fn names_a_style(tag: &str) -> bool {
    const WORDS: &[&str] = &[
        "metal", "rock", "core", "punk", "wave", "gaze", "pop", "rap", "hop", "trap", "jazz", "folk", "doom", "sludge", "ambient", "noise", "synth",
        "electro", "industrial", "grind", "goth", "techno", "house", "step", "blues", "country", "soul", "funk", "prog", "power", "thrash", "death",
        "black", "djent", "drone", "stoner", "emo", "dark", "neofolk", "classical", "orchestral", "symphonic", "experimental", "avant", "math", "post",
    ];
    let t = tag.to_lowercase();
    WORDS.iter().any(|w| t.contains(w))
}

const JUNK_TAGS: &[&str] = &["lidarr", "beets", "plex", "jellyfin", "itunes", "musicbee", "foobar2000", "spotify playlist"];

/// One song per artist from the candidates: the one whose album's own
/// Last.fm tags sound most like the reference (the seed's tags), and at
/// least MIN_SOUND like it; none when no album of theirs does.
/// `lenient` (tier 2 on): an artist whose albums' tags don't sound like the
/// seed keeps its best album's song, for tier 2 to judge: Last.fm's album
/// tags are often too thin to tell (Desaster's at 0.27 next to Witchery).
#[allow(clippy::too_many_arguments)]
async fn choose_by_album(
    app: &AppHandle,
    key: &str,
    candidates: &[Vec<roon_library::TopTrack>],
    names: &[String],
    refs: &[Vec<Tag>],
    eras: &[Option<(u32, u32)>],
    lenient: bool,
    progress: &impl Fn(String),
) -> Vec<Vec<String>> {
    let tags = app.state::<Tags>();
    let mut out = Vec::new();
    for (i, all_songs) in candidates.iter().enumerate() {
        let reference = refs.get(i).cloned().unwrap_or_default();
        // (only songs from the seed's era: 80s Exodus isn't for a Witchery
        // fan, whatever the tags say. A song with no year is let through.)
        let era = eras.get(i).copied().flatten();
        let songs: Vec<roon_library::TopTrack> = match era {
            Some((from, to)) => all_songs.iter().filter(|t| t.year.is_none_or(|y| y + ERA_SLACK >= from && y <= to + ERA_SLACK)).cloned().collect(),
            None => all_songs.clone(),
        };
        if songs.is_empty() {
            if let (Some((from, to)), false) = (era, all_songs.is_empty()) {
                let years: Vec<String> = all_songs.iter().filter_map(|t| t.year).map(|y| y.to_string()).collect();
                note(format!("{}: nothing from the seed's era ({from}-{to}), only {}", names[i], years.join(", ")));
            }
            out.push(Vec::new());
            continue;
        }
        let songs = &songs;
        if reference.is_empty() || key.is_empty() {
            out.push(vec![songs[0].handle.clone()]);
            continue;
        }
        progress(format!("Checking {}'s albums...", names[i]));
        let mut best: Option<(f64, &roon_library::TopTrack)> = None;
        let mut seen_albums: HashSet<&str> = HashSet::new();
        for t in songs {
            if !seen_albums.insert(t.album_key.as_str()) {
                continue;
            }
            let album = tags.lookup_album(app, &names[i], &t.album, key).await;
            // (an album nobody tagged: judged by the artist instead)
            let theirs = if album.tags.is_empty() { tags.cached(app, &names[i]).map(|a| a.tags).unwrap_or_default() } else { album.tags };
            let sound = sound_match(&reference, &theirs);
            if best.as_ref().is_none_or(|(b, _)| sound > *b) {
                best = Some((sound, t));
            }
        }
        match best {
            Some((sound, t)) if sound >= MIN_SOUND => out.push(vec![t.handle.clone()]),
            Some((sound, t)) if lenient => {
                note(format!("{}: no album's tags sound like the seed (best {} at {sound:.2}), left to tier 2", names[i], t.album));
                out.push(vec![t.handle.clone()]);
            }
            Some((sound, t)) => {
                note(format!("{}: no album sounds like the seed (best {} at {sound:.2})", names[i], t.album));
                out.push(Vec::new());
            }
            None => out.push(Vec::new()),
        }
    }
    out
}

/// Years either side of a seed's era a suggestion's album may be from.
const ERA_SLACK: u32 = 5;
/// With tier 2, a band's own albums from the seed's years when none of its
/// best-known songs are from then: this many albums, this many songs each.
const ERA_ALBUMS: usize = 3;
const ERA_SONGS: usize = 3;

/// A seed's era: the release years of its best known songs (Roon's top
/// songs, its own studio albums), the odd outlier either end left out.
/// None when Roon gives fewer than two years. Cached per seed handle.
async fn era_of(client: &LibraryClient, cache: &mut HashMap<String, Option<(u32, u32)>>, handle: &str) -> Option<(u32, u32)> {
    if let Some(known) = cache.get(handle) {
        return *known;
    }
    let mut years: Vec<u32> = client
        .top_tracks(handle)
        .await
        .unwrap_or_default()
        .iter()
        .filter(|t| !t.live && !t.compilation)
        .filter_map(|t| t.year)
        .collect();
    years.sort_unstable();
    if years.len() >= 5 {
        years.remove(0);
        years.pop();
    }
    let era = match (years.first(), years.last()) {
        (Some(a), Some(b)) if years.len() >= 2 => Some((*a, *b)),
        _ => None,
    };
    cache.insert(handle.to_string(), era);
    era
}

/// The sound of what's played lately: the seeds' tags together, each
/// seed's counted by its weight (cached tags only).
fn taste(app: &AppHandle, seeds: &[(Heard, f64)]) -> Vec<Tag> {
    let tags = app.state::<Tags>();
    let mut sum: HashMap<String, (f64, Tag)> = HashMap::new();
    for (seed, weight) in seeds {
        for t in tags.cached(app, &seed.name).map(|a| a.tags).unwrap_or_default() {
            let row = sum.entry(t.name.clone()).or_insert_with(|| (0.0, t.clone()));
            row.0 += t.weight as f64 * weight;
        }
    }
    let top = sum.values().map(|(w, _)| *w).fold(0.0, f64::max);
    sum.into_values()
        .map(|(w, mut t)| {
            t.weight = if top > 0.0 { (w / top * 100.0).round() as u32 } else { 0 };
            t
        })
        .collect()
}

/// An artist the lists keep away from the sound of: one you gave a thumbs
/// down (0.8 and up counts as the same sound), or only skipped (0.9).
struct Avoid {
    name: String,
    tags: Vec<Tag>,
    limit: f64,
    why: &'static str,
}

const SKIPPED: &str = "skipped";

/// A sound like one artist you skipped: the score is worth this much (one
/// quick skip isn't a verdict on a whole genre: "death metal, brutal death
/// metal" sounds like most of death metal). Like two or more skipped
/// artists, or a thumbs down: kept out.
const SKIP_PUSH: f64 = 0.85;

/// What your reactions to the lists say, for the next builds.
#[derive(Default)]
struct Feedback {
    /// Thumbs down: never picked again.
    down: HashSet<String>,
    avoid: Vec<Avoid>,
    /// Artists you liked, as seeds: thumbs up (weight 6) or a heart (4).
    /// The newest few. Only the ones you've also played outside the lists:
    /// until then a like says "good suggestion", not "more like this".
    liked: Vec<(Heard, f64)>,
    /// Good suggestions, as credit to the seeds that made them (lower-cased
    /// seed name -> how many liked songs it suggested): those seeds count
    /// for more in the next list.
    credit: HashMap<String, u32>,
    /// Every song (Roon track id) any list ever held: never picked again.
    played_songs: Vec<String>,
}

/// What happened to the artists the lists brought since each list was
/// built: (listens, plays) by lower-cased name.
fn since_brought(app: &AppHandle, brought: &[(String, i64)]) -> HashMap<String, (u32, u32)> {
    let since: HashMap<String, i64> = brought.iter().fold(HashMap::new(), |mut m, (name, at)| {
        let k = name.to_lowercase();
        let at = m.get(&k).map_or(*at, |had: &i64| (*had).min(*at));
        m.insert(k, at);
        m
    });
    let oldest = since.values().copied().min().unwrap_or(i64::MAX);
    app.state::<Wrapped>().with_plays(app, |plays| {
        let mut out: HashMap<String, (u32, u32)> = HashMap::new();
        // (newest first: stop at the oldest list)
        for p in plays.iter().take_while(|p| p.played_at >= oldest) {
            let k = p.artist.to_lowercase();
            if since.get(&k).is_some_and(|at| p.played_at >= *at) {
                let row = out.entry(k).or_default();
                row.1 += 1;
                if counts(p) {
                    row.0 += 1;
                }
            }
        }
        out
    })
}

/// (lower-cased artist, lower-cased title) of every play since the lists
/// began, to know a list's song by title when its id differs.
fn played_titles(app: &AppHandle, brought: &[(String, i64)]) -> HashSet<(String, String)> {
    let oldest = brought.iter().map(|(_, at)| *at).min().unwrap_or(i64::MAX);
    app.state::<Wrapped>().with_plays(app, |plays| {
        plays.iter().take_while(|p| p.played_at >= oldest).map(|p| (p.artist.to_lowercase(), p.title.to_lowercase())).collect()
    })
}

async fn feedback(
    app: &AppHandle,
    key: &str,
    heard: &HashMap<String, Heard>,
    votes: &HashMap<String, Vote>,
    brought: &[(String, i64)],
    skipped: &HashSet<String>,
    suggested_by: &HashMap<String, Vec<String>>,
) -> Feedback {
    let tags = app.state::<Tags>();
    let mut out = Feedback::default();
    let mut liked: Vec<(Heard, f64, i64)> = Vec::new();
    for (k, v) in votes {
        let seed = |weight: f64| (heard.get(k).cloned().unwrap_or(Heard { name: v.name.clone(), ..Default::default() }), weight, v.at);
        if v.vote < 0 {
            out.down.insert(k.clone());
            out.avoid.push(Avoid { name: v.name.clone(), tags: tags.lookup_quick(app, &v.name, key).await.tags, limit: 0.8, why: "gave a thumbs down" });
        } else if v.vote > 0 || v.heart {
            // (played on its own: a seed. Only on a list so far: credit
            // to whatever suggested it)
            if heard.get(k).is_some_and(|h| h.listens > 0) {
                liked.push(seed(if v.vote > 0 { 6.0 } else { 4.0 }));
            } else {
                for seed_name in suggested_by.get(k).into_iter().flatten() {
                    *out.credit.entry(seed_name.to_lowercase()).or_default() += 1;
                }
            }
        }
    }
    // Unrated artists from the lists, by what you did with their songs
    // after the list brought them (not plays from before)
    let after = since_brought(app, brought);
    let mut seen = HashSet::new();
    for (name, _) in brought {
        let k = name.to_lowercase();
        if !seen.insert(k.clone()) || votes.get(&k).is_some_and(|v| v.vote != 0 || v.heart) {
            continue;
        }
        // (listened through is no seed by itself: a list played every week
        // would keep feeding itself. Its artists seed once you play them
        // outside the list, through the history like any other.)
        // (a skip in the first seconds isn't in Roon's history: Toasted saw
        // it come and go, see `zones_updated`)
        let (listens, plays) = after.get(&k).copied().unwrap_or((0, 0));
        let h = heard.get(&k).cloned().unwrap_or(Heard { name: name.clone(), ..Default::default() });
        if listens == 0 && (plays > 0 || skipped.contains(&k)) {
            out.avoid.push(Avoid { name: h.name.clone(), tags: tags.lookup_quick(app, &h.name, key).await.tags, limit: 0.9, why: SKIPPED });
        }
    }
    liked.sort_by(|a, b| b.2.cmp(&a.2));
    out.liked = liked.into_iter().take(6).map(|(h, w, _)| (h, w)).collect();
    // (one with no tags still keeps itself out, by name)
    out
}

/// The artists in a collaboration's name ("neurosis & jarboe" is
/// "neurosis" and "jarboe"); none for a plain name.
fn name_parts(name: &str) -> Vec<String> {
    let mut parts = vec![name.to_string()];
    for sep in [" & ", " and ", " x ", " feat. ", " feat ", " ft. ", " with ", " vs. ", " vs ", ", ", " / "] {
        parts = parts.iter().flat_map(|p| p.split(sep).map(|q| q.trim().to_string())).collect();
    }
    parts.retain(|p| !p.is_empty());
    if parts.len() < 2 {
        return Vec::new();
    }
    parts
}

/// Roon's genres for an artist handle, asked once.
pub(crate) async fn genres_of(client: &LibraryClient, cache: &mut HashMap<String, Vec<String>>, handle: &str) -> Vec<String> {
    if let Some(have) = cache.get(handle) {
        return have.clone();
    }
    let genres: Vec<String> = client.artist_facts(handle).await.map(|f| f.genres).unwrap_or_default().into_iter().map(|g| g.to_lowercase()).collect();
    cache.insert(handle.to_string(), genres.clone());
    genres
}

/// An artist outside the library found in Roon by exact name; with several
/// of that name (Last.fm lumps them together), the one sharing a Roon
/// genre with the seeds it came from, else one Roon has no genres for.
/// A name as it's compared: lower-cased, accents and the like taken off
/// ("Mgła" is "mgla", "Bølzer" and "Bölzer" are "bolzer"), letters and
/// digits only.
pub(crate) fn fold(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        let plain = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ą' | 'ă' => "a",
            'æ' => "ae",
            'ç' | 'ć' | 'č' => "c",
            'ď' | 'ð' | 'đ' => "d",
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => "e",
            'ì' | 'í' | 'î' | 'ï' | 'ī' => "i",
            'ł' | 'ľ' | 'ĺ' => "l",
            'ñ' | 'ń' | 'ň' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
            'œ' => "oe",
            'ŕ' | 'ř' => "r",
            'ś' | 'š' | 'ş' => "s",
            'ß' => "ss",
            'ť' | 'ţ' => "t",
            'þ' => "th",
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
            'ý' | 'ÿ' => "y",
            'ź' | 'ż' | 'ž' => "z",
            c if c.is_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => continue,
        };
        out.push_str(plain);
    }
    out
}

pub(crate) async fn find_by_name(client: &LibraryClient, cache: &mut HashMap<String, Vec<String>>, name: &str, like: &HashSet<String>) -> Option<String> {
    let hits = client.search(name, 10).await.ok()?;
    let wanted = fold(name);
    let mut unknown = None;
    for hit in hits.artists.into_iter().filter(|a| fold(&a.name) == wanted) {
        if like.is_empty() {
            return Some(hit.handle);
        }
        let genres = genres_of(client, cache, &hit.handle).await;
        if genres.iter().any(|g| like.contains(g)) {
            return Some(hit.handle);
        }
        if genres.is_empty() && unknown.is_none() {
            unknown = Some(hit.handle);
        }
    }
    unknown
}

/// An artist suggested for the Home page (home.rs): new to you, from the
/// same tag-matched neighbours as Discover Weekly, without the songs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TasteArtist {
    pub name: String,
    /// "Suggested because you listened to X (album) and Y"
    pub because: String,
    /// The seeds it came from.
    pub seeds: Vec<String>,
}

/// Your favorite artists over all time (name, weight: listens ever, this
/// year's a little more, a hearted artist 20 more), heaviest first.
pub(crate) async fn home_favorites(app: &AppHandle, client: &LibraryClient) -> (Vec<(String, f64)>, HashMap<String, Heard>) {
    let now = now_ms();
    // (plays from the lists don't count, as for the lists themselves)
    let every: Vec<(String, String, String, String, i64)> = app.state::<Discover>().with_store(app, |s| {
        s.weekly
            .iter()
            .map(|b| (Which::Weekly, b))
            .chain(s.rediscover.iter().map(|b| (Which::Rediscover, b)))
            .flat_map(|(w, b)| b.tracks.iter().map(move |t| (format!("{}@{}", w.name(), b.built_at), t.track_id.clone(), t.artist.clone(), t.title.clone(), b.built_at)))
            .collect()
    });
    let ctx = Context { from_lists: plays_from_lists(app, &every), intent: intent_from_recents(client, now).await };
    let heard = history_by_artist(app, now, &ctx);
    // (not the last few weeks, which is what Discover Weekly works from:
    // every listen ever counts, and a hearted artist counts as well played)
    let hearted: HashSet<String> = client.library_artists(&|_, _| {}).await.map(|cards| cards.into_iter().filter(|c| c.favorite).map(|c| c.name.to_lowercase()).collect()).unwrap_or_default();
    // (and the Last.fm scrobbles from before Roon's history starts)
    let oldest = app.state::<Wrapped>().with_plays(app, |plays| plays.last().map(|p| p.played_at).unwrap_or(now));
    let scrobbled = crate::scrobbles::listens_before(app, oldest);
    let mut rows: Vec<(String, f64)> = heard
        .values()
        .filter(|h| h.name.to_lowercase() != "various artists" && h.listens > 0)
        .map(|h| {
            let k = h.name.to_lowercase();
            let old = scrobbled.get(&k).map(|(_, n)| *n).unwrap_or(0);
            (h.name.clone(), (h.listens + old) as f64 + h.year as f64 * 0.5 + if hearted.contains(&k) { 20.0 } else { 0.0 })
        })
        .collect();
    for (k, (name, n)) in &scrobbled {
        if !heard.contains_key(k) && k != "various artists" {
            rows.push((name.clone(), *n as f64 + if hearted.contains(k) { 20.0 } else { 0.0 }));
        }
    }
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    (rows, heard)
}

/// What you listen to, for the Home page: your favorites (`home_favorites`)
/// and up to `want` artists new to you that sound like the top SEEDS_DEEP
/// of them (tier 1's tag gates against the albums you play; no tier 2, no
/// songs). Last.fm's neighbours and tags are cached by tags.rs.
pub(crate) async fn home_taste(app: &AppHandle, client: &LibraryClient, key: &str, want: usize, progress: &impl Fn(String)) -> Result<(Vec<(String, f64)>, Vec<TasteArtist>), String> {
    let (favorites, heard) = home_favorites(app, client).await;
    // (a favorite known only from scrobbles stands by its name: the band's
    // own tags stand in for the albums played)
    let seeds: Vec<(Heard, f64)> = favorites
        .iter()
        .take(SEEDS_DEEP)
        .map(|(name, w)| (heard.get(&name.to_lowercase()).cloned().unwrap_or_else(|| Heard { name: name.clone(), ..Default::default() }), *w))
        .collect();
    if key.is_empty() || seeds.is_empty() {
        return Ok((favorites, Vec::new()));
    }
    let no_progress = |_: usize, _: usize| {};
    let library: HashSet<String> = match client.library_artists(&no_progress).await {
        Ok(cards) => cards.into_iter().map(|c| c.name.to_lowercase()).collect(),
        Err(_) => app.state::<LibraryCopy>().names(app),
    };
    let down = turned_down(app);
    let found = neighbours(app, seeds, SEEDS_DEEP, key, progress).await;
    let views = seed_views(app, client, key, &found.seeds, progress).await;
    let known = |k: &str| heard.contains_key(k) || library.contains(k);
    let open = |k: &str| !known(k) && !down.contains(k) && k != "various artists" && !name_parts(k).iter().any(|part| known(part));
    let mut jitter = Jitter::new();
    let ranked = spread(app, &found, &views, open, key, want, false, &[], &mut jitter, progress).await;
    let out = ranked
        .into_iter()
        .map(|(_, name, from)| {
            let (because, seeds) = because(&found, &views, &from);
            TasteArtist { name, because, seeds }
        })
        .collect();
    Ok((favorites, out))
}

/// The seeds' Roon handles, by performer id (else by exact name).
async fn seed_handles(client: &LibraryClient, seeds: &[(Heard, f64)]) -> Vec<Option<String>> {
    let mut out = Vec::new();
    for (seed, _) in seeds {
        let by_id = match seed.performer_id {
            Some(id) => client.artist_by_id(id).await.ok().map(|c| c.handle),
            None => None,
        };
        let handle = match by_id {
            Some(h) => Some(h),
            None => {
                let wanted = seed.name.to_lowercase();
                client.search(&seed.name, 5).await.ok().and_then(|hits| hits.artists.into_iter().find(|a| a.name.to_lowercase() == wanted).map(|a| a.handle))
            }
        };
        out.push(handle);
    }
    out
}

/// The artists picked and their songs, in play order.
struct Picked {
    artists: Vec<String>,
    songs: Vec<String>,
    /// The artist each song (by handle) was picked for, why, and the
    /// artists of yours behind it.
    song_artist: HashMap<String, (String, String, Vec<String>)>,
    /// Tier 2's line per song (by handle).
    song_why: HashMap<String, String>,
    /// Tier 2 had your requests to work from.
    requests_used: bool,
}

/// The picked artists' songs in play order, with whose each one is and why
/// it's there.
/// Heavy: how many of the strongest seeds tier 2 suggests artists for, and
/// how many each.
const HEAVY_SEEDS: usize = 12;
const HEAVY_PER_SEED: usize = 4;
/// How many times tier 2 is asked for its own picks (the second only when
/// the list is still short), and how many artists you know are named per
/// seed for it to reach past.
const HEAVY_PASSES: usize = 2;
const KNOWN_SHOWN: usize = 25;
/// Heavy: the artists you play most, named as known for all seeds.
const FAVORITES_SHOWN: usize = 200;

/// Heavy: the artists tier 2 brings in itself for the strongest seeds, as
/// tier 1's candidates are (key, name, seeds). Left out: artists you know,
/// listed lately or turned down (`open`), skipped ones, and ones this build
/// tried already.
#[allow(clippy::too_many_arguments)]
async fn heavy_suggestions(
    llm: Option<&crate::judge::Llm>,
    found: &Neighbours,
    views: &[SeedView],
    known_near: &[Vec<String>],
    favorites: &[String],
    tried: &[String],
    asked: &mut Vec<String>,
    open: &impl Fn(&str) -> bool,
    avoid: &[Avoid],
    progress: &impl Fn(String),
) -> Vec<(String, String, Vec<usize>)> {
    let Some(llm) = llm else { return Vec::new() };
    progress("Asking for artists of tier 2's own...".into());
    let seeds: Vec<crate::judge::SeedLine> = found
        .seeds
        .iter()
        .take(HEAVY_SEEDS)
        .enumerate()
        .map(|(i, (h, _))| crate::judge::SeedLine {
            name: h.name.clone(),
            albums: views.get(i).map(|v| v.albums.clone()).unwrap_or_default(),
            era: views.get(i).and_then(|v| v.era),
            tags: views.get(i).map(|v| v.tags.iter().take(4).cloned().collect()).unwrap_or_default(),
            known: known_near.get(i).cloned().unwrap_or_default(),
        })
        .collect();
    let mut skip: Vec<String> = found.seeds.iter().map(|(h, _)| h.name.clone()).collect();
    skip.extend(tried.iter().cloned());
    skip.extend(asked.iter().cloned());
    let started = std::time::Instant::now();
    let list = match crate::judge::suggest(llm, &seeds, favorites, &skip, HEAVY_PER_SEED).await {
        Ok(list) => list,
        Err(e) => {
            note(format!("tier 2 couldn't suggest artists of its own: {e}"));
            return Vec::new();
        }
    };
    note(format!("tier 2 suggested {} artists of its own in {} s", list.len(), started.elapsed().as_secs()));
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for s in list {
        asked.push(s.artist.clone());
        let k = s.artist.to_lowercase();
        let Some(seed_i) = found.seeds.iter().position(|(h, _)| h.name.eq_ignore_ascii_case(&s.seed)) else {
            note(format!("tier 2's own pick {} left out: \"{}\" isn't one of the seeds", s.artist, s.seed));
            continue;
        };
        if !seen.insert(k.clone()) || tried.iter().any(|t| t.eq_ignore_ascii_case(&k)) {
            continue;
        }
        if !open(&k) || avoid.iter().any(|a| a.name.eq_ignore_ascii_case(&k)) {
            note(format!("tier 2's own pick {} left out: known, listed lately, skipped or turned down", s.artist));
            continue;
        }
        note(format!("tier 2 suggests {} for {} ({}): {}", s.artist, found.seeds[seed_i].0.name, s.album, s.why));
        out.push((k, s.artist.clone(), vec![seed_i]));
    }
    out
}

/// Heavy: the kept picks (indices) in tier 2's order, or None when it
/// couldn't say.
#[allow(clippy::too_many_arguments)]
async fn heavy_order(
    llm: &crate::judge::Llm,
    kept: &[usize],
    picks: &[Vec<String>],
    candidates: &[Vec<roon_library::TopTrack>],
    names: &[String],
    froms: &[usize],
    seed_names: &[String],
    liking: &HashMap<usize, f64>,
    song_why: &HashMap<String, String>,
    progress: &impl Fn(String),
) -> Option<Vec<usize>> {
    progress("Putting the list in order...".into());
    let lines: Vec<crate::judge::ListLine> = kept
        .iter()
        .map(|&i| {
            let handle = picks[i].first().cloned().unwrap_or_default();
            let song = candidates.get(i).and_then(|c| c.iter().find(|t| t.handle == handle));
            crate::judge::ListLine {
                artist: names[i].clone(),
                song: song.map(|t| t.title.clone()).unwrap_or_default(),
                album: song.map(|t| t.album.clone()).unwrap_or_default(),
                year: song.and_then(|t| t.year),
                seed: seed_names.get(froms[i]).cloned().unwrap_or_default(),
                liking: liking.get(&i).copied().unwrap_or(0.5),
                why: song_why.get(&handle).cloned().unwrap_or_default(),
            }
        })
        .collect();
    let started = std::time::Instant::now();
    match crate::judge::order(llm, &lines).await {
        Ok(order) => {
            let order: Vec<usize> = order.into_iter().map(|n| kept[n]).collect();
            let shown: Vec<&str> = order.iter().take(SONGS).take(5).map(|i| names[*i].as_str()).collect();
            note(format!("tier 2 put the {} songs in order in {} s, opening with {}{}", order.len(), started.elapsed().as_secs(), shown.join(", "), if order.len() > SONGS { format!("; {} didn't make the cut", order.len() - SONGS) } else { String::new() }));
            Some(order)
        }
        Err(e) => {
            note(format!("tier 2 couldn't put the list in order ({e}); tier 1's order stands"));
            None
        }
    }
}

/// `picked` in the given order (tier 2's, Heavy), as it is.
fn picked_in_order(order: &[usize], picks: &[Vec<String>], names: &[String], reasons: &[(String, Vec<String>)]) -> Picked {
    let mut out = picked(order, picks, names, reasons);
    out.songs = order.iter().flat_map(|i| picks[*i].iter().cloned()).collect();
    out
}

fn picked(chosen: &[usize], picks: &[Vec<String>], names: &[String], reasons: &[(String, Vec<String>)]) -> Picked {
    let artists = chosen.iter().map(|i| names[*i].clone()).collect();
    let song_artist = chosen
        .iter()
        .flat_map(|i| {
            let (why, from) = reasons.get(*i).cloned().unwrap_or_default();
            picks[*i].iter().map(move |h| (h.clone(), (names[*i].clone(), why.clone(), from.clone())))
        })
        .collect();
    // The play order: songs from the same seed spread out, never two in a
    // row where it can be helped (the seed is the first name in `from`)
    let group = |i: &usize| reasons.get(*i).and_then(|(_, from)| from.first().cloned()).unwrap_or_default();
    let mut by_group: Vec<(String, Vec<usize>)> = Vec::new();
    for i in chosen {
        let g = group(i);
        match by_group.iter_mut().find(|(k, _)| *k == g) {
            Some((_, v)) => v.push(*i),
            None => by_group.push((g, vec![*i])),
        }
    }
    // (biggest groups first, so they get spread the widest)
    by_group.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let total = chosen.len();
    let mut slots: Vec<Option<usize>> = vec![None; total];
    for (_, members) in &by_group {
        // each member gets an even share of the free slots
        let free: Vec<usize> = (0..total).filter(|k| slots[*k].is_none()).collect();
        let step = free.len() as f64 / members.len() as f64;
        for (n, i) in members.iter().enumerate() {
            let mut at = (n as f64 * step + step / 2.0) as usize;
            while free.get(at).is_some_and(|k| slots[*k].is_some()) {
                at += 1;
            }
            let k = free.get(at).or_else(|| free.iter().find(|k| slots[**k].is_none())).copied();
            if let Some(k) = k {
                slots[k] = Some(*i);
            }
        }
    }
    let order: Vec<usize> = slots.into_iter().flatten().collect();
    let songs: Vec<String> = order.iter().flat_map(|i| picks[*i].iter().cloned()).collect();
    Picked { artists, songs, song_artist, song_why: HashMap::new(), requests_used: false }
}

/// "Suggested because you listened to Amon Amarth and Iced Earth": the (at
/// most two) seeds an artist came from.
/// "Suggested because you listened to Ulver (Bergtatt) and Amon Amarth": the
/// seeds (names, to open) with the album you played most of each.
fn because(found: &Neighbours, views: &[SeedView], from: &[usize]) -> (String, Vec<String>) {
    let names: Vec<String> = from.iter().take(2).filter_map(|i| found.seeds.get(*i)).map(|(h, _)| h.name.clone()).collect();
    let shown: Vec<String> = from
        .iter()
        .take(2)
        .filter_map(|i| found.seeds.get(*i).map(|(h, _)| (h, views.get(*i))))
        .map(|(h, v)| match v.and_then(|v| v.albums.first()) {
            Some(album) => format!("{} ({album})", h.name),
            None => h.name.clone(),
        })
        .collect();
    let text = match shown.as_slice() {
        [] => String::new(),
        [one] => format!("Suggested because you listened to {one}"),
        [one, two, ..] => format!("Suggested because you listened to {one} and {two}"),
    };
    (text, names)
}

#[allow(clippy::too_many_arguments)]
// ----- Tier 3 in the lists (sound.rs): the seeds heard as you play them,
// each candidate's songs heard and the closest to its seed kept -----

/// A seed's sound: its songs you played most (this year, else ever).
const SEED_SONGS: usize = 3;
/// Under this, none of an artist's songs sound like its seed: the artist
/// goes to the back of the list (used only when the list is short).
const SOUND_FAR: f32 = 0.60;
/// Under this, clearly lowered (after the closer ones).
const SOUND_MID: f32 = 0.75;

#[derive(Default)]
struct Hearing {
    /// Each seed's sound, by its index: its played songs heard together
    /// (None: none of them could be heard).
    seeds: HashMap<usize, Option<Vec<f32>>>,
    /// A song (handle) heard: its closeness to its artist's seed and its
    /// strongest styles.
    songs: HashMap<String, (f32, Vec<String>)>,
    /// Artists (list index) whose songs all sound far from their seed, or
    /// only middling.
    far: HashSet<usize>,
    mid: HashSet<usize>,
    heard: usize,
    files: usize,
    secs: u64,
    /// Tier 3 failed (not set up after all): not tried again this build.
    off: bool,
}

impl Hearing {
    /// The order bucket of an artist: 0 close (or not heard), 1 middling,
    /// 2 far.
    fn bucket(&self, i: usize) -> u8 {
        if self.far.contains(&i) {
            2
        } else if self.mid.contains(&i) {
            1
        } else {
            0
        }
    }

    /// Tier 2's SOUND lines for an artist's songs.
    fn lines(&self, songs: &[roon_library::TopTrack]) -> Vec<String> {
        songs
            .iter()
            .filter_map(|t| {
                let (close, styles) = self.songs.get(&t.handle)?;
                Some(format!("{}: {close:.2}{}", t.title, if styles.is_empty() { String::new() } else { format!(" ({})", styles.join(", ")) }))
            })
            .collect()
    }
}

/// Tier 3 for one batch of the weekly list: the batch's seeds heard (their
/// songs you played most), every candidate song of each artist heard, the
/// closest to the artist's seed becomes its pick, and an artist with
/// nothing close is marked far (or middling). `seed_of[j]`: artist j's seed
/// (index and name); `first`: the batch's place in the whole list.
#[allow(clippy::too_many_arguments)]
async fn hear_batch(
    app: &AppHandle,
    client: &LibraryClient,
    hearing: &mut Hearing,
    seed_year: &HashMap<String, Vec<(String, u64, u32)>>,
    seed_ever: &HashMap<String, Vec<(String, u64, u32)>>,
    seed_of: &[Option<(usize, String)>],
    names: &[String],
    batch: &[Vec<roon_library::TopTrack>],
    first: usize,
    picks: &mut [Vec<String>],
) {
    use crate::sound::{Print, Song, Sound};
    let sound = app.state::<Sound>();
    let mut songs: Vec<Song> = Vec::new();
    // The seeds not heard yet
    let mut seed_ranges: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
    for (b, name) in seed_of.iter().flatten() {
        if hearing.seeds.contains_key(b) || seed_ranges.iter().any(|(x, _)| x == b) {
            continue;
        }
        let k = name.to_lowercase();
        let list = seed_year.get(&k).filter(|l| !l.is_empty()).or_else(|| seed_ever.get(&k)).cloned().unwrap_or_default();
        let start = songs.len();
        for (title, id, _) in list.iter().take(SEED_SONGS) {
            let handle = client.track_by_id(*id).await.ok();
            songs.push(Song { artist: name.clone(), title: title.clone(), album: String::new(), handle });
        }
        seed_ranges.push((*b, start..songs.len()));
    }
    // Every candidate song of the batch's artists
    let cand_start = songs.len();
    let mut cand_of: Vec<(usize, String, String)> = Vec::new();
    for (j, list) in batch.iter().enumerate() {
        if seed_of.get(j).and_then(|s| s.as_ref()).is_none() || picks.get(j).is_none_or(|p| p.is_empty()) {
            continue;
        }
        for t in list {
            songs.push(Song { artist: names[j].clone(), title: t.title.clone(), album: t.album.clone(), handle: Some(t.handle.clone()) });
            cand_of.push((j, t.handle.clone(), t.title.clone()));
        }
    }
    if songs.is_empty() {
        return;
    }
    let started = std::time::Instant::now();
    let prints: Vec<Option<Print>> = match sound.prints(app, &songs, &|_, _| {}).await {
        Ok(p) => p,
        Err(e) => {
            note(format!("tier 3 skipped: {e}"));
            hearing.off = true;
            return;
        }
    };
    hearing.secs += started.elapsed().as_secs();
    hearing.heard += prints.iter().flatten().count();
    hearing.files += prints.iter().flatten().filter(|p| crate::sound::own_file(&p.from)).count();
    let style_names = sound.names(app).unwrap_or_default();
    for (b, range) in seed_ranges {
        let heard: Vec<&Print> = prints[range].iter().flatten().collect();
        let name = seed_of.iter().flatten().find(|(x, _)| *x == b).map(|(_, n)| n.clone()).unwrap_or_default();
        if heard.is_empty() {
            note(format!("tier 3: no songs of the seed {name} could be heard"));
        }
        hearing.seeds.insert(b, crate::sound::average(&heard));
    }
    for j in 0..batch.len() {
        let Some((b, seed_name)) = seed_of.get(j).and_then(|s| s.as_ref()) else { continue };
        let Some(Some(seed)) = hearing.seeds.get(b) else { continue };
        let mut scored: Vec<(f32, String, String)> = Vec::new();
        for (k, (jj, handle, title)) in cand_of.iter().enumerate() {
            if *jj != j {
                continue;
            }
            if let Some(p) = &prints[cand_start + k] {
                let close = crate::sound::dot(&p.artist, seed);
                hearing.songs.insert(handle.clone(), (close, p.top_styles(&style_names, 3)));
                scored.push((close, handle.clone(), title.clone()));
            }
        }
        if scored.is_empty() {
            continue;
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        note(format!("sound of {} for {seed_name}: {}", names[j], scored.iter().map(|(c, _, t)| format!("{t} {c:.2}")).collect::<Vec<_>>().join(", ")));
        let (best, handle, title) = scored[0].clone();
        if picks[j].first() != Some(&handle) {
            note(format!("tier 3 chose {} - {title} ({best:.2}), the closest to {seed_name}", names[j]));
            picks[j] = vec![handle];
        }
        if best < SOUND_FAR {
            hearing.far.insert(first + j);
        } else if best < SOUND_MID {
            hearing.mid.insert(first + j);
        }
    }
}

async fn pick_weekly(app: &AppHandle, client: &LibraryClient, key: &str, recently: &HashSet<String>, said: &Feedback, ctx: &Context, requests: &[Request], progress: &impl Fn(String)) -> Result<Picked, String> {
    if key.is_empty() {
        return Err("Discover Weekly needs a Last.fm key (Wrapped's Settings): it finds new artists through Last.fm's listeners.".into());
    }
    progress("Reading your listening...".into());
    let now = now_ms();
    let heard = history_by_artist(app, now, ctx);
    let mut seeds = seeds(&heard);
    give_credit(&mut seeds, &said.credit);
    if seeds.is_empty() {
        return Err("Not enough listening in the history yet (Wrapped's History page reads it from Roon).".into());
    }
    // (artists you liked from earlier lists join the seeds, on top of the
    // usual number)
    for (h, w) in &said.liked {
        if !seeds.iter().any(|(s, _)| s.name.eq_ignore_ascii_case(&h.name)) {
            seeds.push((h.clone(), *w));
        }
    }
    // (and so do the artists of the songs you asked from, up front: what
    // you asked for comes first)
    let top = seeds.iter().map(|(_, w)| *w).fold(6.0, f64::max);
    let mut from_requests = 0;
    for song in requests.iter().filter_map(|r| r.song.as_ref()).filter(|s| !s.artist.trim().is_empty()) {
        let name = song.artist.trim();
        match seeds.iter_mut().find(|(s, _)| s.name.eq_ignore_ascii_case(name)) {
            Some((_, w)) => *w = top,
            None => {
                seeds.push((heard.get(&name.to_lowercase()).cloned().unwrap_or(Heard { name: name.to_string(), ..Default::default() }), top));
                from_requests += 1;
            }
        }
    }
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1));
    progress("Reading your library...".into());
    let no_progress = |_: usize, _: usize| {};
    let library: HashSet<String> = match client.library_artists(&no_progress).await {
        Ok(cards) => cards.into_iter().map(|c| c.name.to_lowercase()).collect(),
        Err(_) => app.state::<LibraryCopy>().names(app),
    };
    let found = neighbours(app, seeds, SEEDS_DEEP + said.liked.len() + from_requests, key, progress).await;
    note(format!("weekly seeds: {}", found.seeds.iter().map(|(h, w)| format!("{} {w:.1}", h.name)).collect::<Vec<_>>().join(", ")));
    let views = seed_views(app, client, key, &found.seeds, progress).await;
    let mut jitter = Jitter::new();
    // Never played, not in the library, not in the last two lists, and not
    // a collaboration of an artist you know ("Neurosis & Jarboe")
    let known = |k: &str| heard.contains_key(k) || library.contains(k);
    let mut llm = crate::judge::Llm::from_settings(&WrappedSettings::load(app));
    // (Fine Tuning's switches: tier 2 and tier 3 for this list)
    let switches = WrappedSettings::load(app);
    if llm.is_some() && !switches.weekly_tier2 {
        note("tier 2 switched off for Discover Weekly (Fine Tuning)".to_string());
        llm = None;
    }
    let tier3 = crate::sound::Sound::ready(app) && switches.weekly_tier3;
    if !switches.weekly_tier3 && crate::sound::Sound::ready(app) {
        note("tier 3 switched off for Discover Weekly (Fine Tuning)".to_string());
    }
    let mut hearing = Hearing::default();
    let (seed_year, seed_ever) = if tier3 { (songs_by_artist(app, ctx, Some(now - 365 * DAY_MS)), songs_by_artist(app, ctx, None)) } else { (HashMap::new(), HashMap::new()) };
    // (Ollama not answering: tier 1's picks as they are, rather than every
    // judgement failing one by one)
    // (Wrapped runs Ollama: started for the build if it isn't running,
    // closed a minute after it, when Wrapped started it)
    let mut _ollama = None;
    if let Some(l) = &llm {
        if l.provider == "ollama" {
            progress("Starting Ollama...".into());
            match crate::ollama::lease(app).await {
                Ok((held, started)) => {
                    if started {
                        note("tier 2: started Ollama for this build".to_string());
                    }
                    _ollama = Some(held);
                }
                Err(e) => {
                    note(format!("tier 2 skipped: {e}"));
                    llm = None;
                }
            }
        }
    }
    let tier2 = llm.is_some();
    // (spares: some artists aren't in Roon or have no songs of their own it
    // can play; with tier 2, enough for two more rounds as well)
    let open = |k: &str| !known(k) && !recently.contains(k) && !said.down.contains(k) && k != "various artists" && !name_parts(k).iter().any(|part| known(part));
    let mut ranked = spread(
        app,
        &found,
        &views,
        open,
        key,
        if tier2 { SONGS + 24 + (ROUNDS - 1) * (ROUND_MORE + 6) } else { SONGS + 14 },
        tier2,
        &said.avoid,
        &mut jitter,
        progress,
    )
    .await;

    progress("Finding the seeds in Roon...".into());
    let seed_handles = seed_handles(client, &found.seeds).await;
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut eras: HashMap<String, Option<(u32, u32)>> = HashMap::new();
    let mut seed_briefs: HashMap<usize, crate::judge::SeedBrief> = HashMap::new();
    // Everything found so far, in tier 1's order (one entry per artist)
    let mut names: Vec<String> = Vec::new();
    let mut reasons: Vec<(String, Vec<String>)> = Vec::new();
    let mut handles: Vec<String> = Vec::new();
    let mut froms: Vec<usize> = Vec::new();
    let mut seed_tags: Vec<Vec<Tag>> = Vec::new();
    let mut seed_eras: Vec<Option<(u32, u32)>> = Vec::new();
    let mut candidates: Vec<Vec<roon_library::TopTrack>> = Vec::new();
    let mut picks: Vec<Vec<String>> = Vec::new();
    let mut song_why: HashMap<String, String> = HashMap::new();
    let mut cursor = 0;
    // Several songs each (from different albums), the one from the album
    // that sounds most like the seed kept: a band's sound moves over a
    // career (Klone's groove years next to Soen), and the artist's tags
    // cover all of it
    let options = MixOptions { per_artist: 6, include_live: false, library_only: false, own_albums: true, from_top: 10, never: said.played_songs.clone() };
    // Rounds: tier 1 finds a batch, tier 2 judges it; under SONGS kept,
    // tier 1 brings ROUND_MORE more and tier 2 judges those, at most ROUNDS
    // rounds in all (then the list is as long as it got)
    // Heavy: after tier 1's rounds, tier 2 brings artists of its own (judged
    // like the rest; asked again once while the list is short), then puts
    // the list in order
    // (requests: always Heavy, whatever the setting says: the best list
    // there is)
    let heavy = llm.as_ref().is_some_and(|l| l.mode == "heavy" || !requests.is_empty());
    let mut liking: HashMap<usize, f64> = HashMap::new();
    let mut adding = false;
    let mut heavy_passes = 0;
    // (Heavy: what tier 2 suggested so far, and the artists you know among
    // each strong seed's neighbours, so it reaches past the obvious names)
    let mut asked: Vec<String> = Vec::new();
    let favorites: Vec<String> = {
        let mut all: Vec<&Heard> = heard.values().filter(|h| !h.name.is_empty()).collect();
        all.sort_by(|a, b| b.listens.cmp(&a.listens));
        all.into_iter().take(FAVORITES_SHOWN).map(|h| h.name.clone()).collect()
    };
    let known_near: Vec<Vec<String>> = found
        .lists
        .iter()
        .take(HEAVY_SEEDS)
        .map(|list| list.iter().filter(|(k, _, _)| known(k)).take(KNOWN_SHOWN).map(|(_, name, _)| name.clone()).collect())
        .collect();
    // Requests (with tier 2): each one stands in for a seed of its own,
    // numbered after the real ones; tier 2 brings artists for them, and
    // they go first. Every judgement hears what you asked for.
    let n_seeds = found.seeds.len();
    let asks: Vec<&Request> = if llm.is_some() { requests.iter().collect() } else { Vec::new() };
    if !requests.is_empty() {
        note(format!("requests: {}", requests.iter().map(|r| describe(r, true)).collect::<Vec<_>>().join("; ")));
        if asks.is_empty() {
            note("requests wait for tier 2 (Wrapped's Settings); the songs' artists joined the seeds".to_string());
        }
    }
    let wishes: Vec<String> = asks.iter().map(|r| describe(r, true)).collect();
    // (a song request's artist, as a seed: its tags, era and genres)
    let req_base: Vec<Option<usize>> = asks
        .iter()
        .map(|r| r.song.as_ref().and_then(|song| found.seeds.iter().position(|(h, _)| h.name.eq_ignore_ascii_case(song.artist.trim()))))
        .collect();
    let base_of = |i: usize| if i < n_seeds { Some(i) } else { req_base.get(i - n_seeds).copied().flatten() };
    let mut seed_names: Vec<String> = found.seeds.iter().map(|(h, _)| h.name.clone()).collect();
    seed_names.extend(wishes.iter().map(|w| format!("their request: {w}")));
    let mut request_count = 0;
    if let Some(llm) = llm.as_ref().filter(|_| !asks.is_empty()) {
        progress("Reading up on what you asked for...".into());
        let discogs = WrappedSettings::load(app).discogs_token.trim().to_string();
        for (n, r) in asks.iter().enumerate() {
            let mut brief = match req_base[n] {
                Some(b) => seed_brief(app, client, key, &discogs, &found, &views, &seed_handles, b).await,
                None => crate::judge::SeedBrief::default(),
            };
            if let Some(song) = &r.song {
                brief.name = song.artist.trim().to_string();
                let album = song.album.trim();
                if !album.is_empty() && !brief.albums.iter().any(|a| a.title.eq_ignore_ascii_case(album)) {
                    brief.albums.insert(0, crate::judge::album_brief(app, client, key, &discogs, song.artist.trim(), album, None, None).await);
                }
            }
            brief.request = wishes[n].clone();
            brief.wishes = wishes.iter().enumerate().filter(|(m, _)| *m != n).map(|(_, w)| w.clone()).collect();
            seed_briefs.insert(n_seeds + n, brief);
        }
        progress("Asking for artists for your requests...".into());
        let per = (REQUEST_SHARE / asks.len()).clamp(3, 6);
        let skip: Vec<String> = found.seeds.iter().map(|(h, _)| h.name.clone()).collect();
        let started = std::time::Instant::now();
        match crate::judge::suggest_for_requests(llm, &wishes, &favorites, &skip, per).await {
            Ok(list) => {
                note(format!("tier 2 suggested {} artists for your requests in {} s", list.len(), started.elapsed().as_secs()));
                let mut first: Vec<(String, String, Vec<usize>)> = Vec::new();
                for sug in list {
                    let Some(n) = sug.seed.trim().parse::<usize>().ok().filter(|n| *n >= 1 && *n <= asks.len()) else {
                        note(format!("tier 2's pick {} left out: \"{}\" isn't one of the requests", sug.artist, sug.seed));
                        continue;
                    };
                    asked.push(sug.artist.clone());
                    let k = sug.artist.to_lowercase();
                    if let Some((_, _, from)) = first.iter_mut().find(|(f, _, _)| *f == k) {
                        from.push(n_seeds + n - 1);
                        continue;
                    }
                    if !open(&k) || said.avoid.iter().any(|a| a.name.eq_ignore_ascii_case(&k)) {
                        note(format!("tier 2's pick {} for your request left out: known, listed lately, skipped or turned down", sug.artist));
                        continue;
                    }
                    note(format!("tier 2 suggests {} for your request {} ({}): {}", sug.artist, wishes[n - 1], sug.album, sug.why));
                    first.push((k, sug.artist.clone(), vec![n_seeds + n - 1]));
                }
                request_count = first.len();
                ranked.retain(|(k, _, _)| !first.iter().any(|(f, _, _)| f == k));
                ranked.splice(0..0, first);
            }
            Err(e) => note(format!("tier 2 couldn't suggest artists for your requests: {e}")),
        }
    }
    let mut round = 0;
    loop {
        let target = if adding {
            ranked.len() - cursor
        } else if round == 0 {
            if tier2 {
                SONGS + 16 + request_count
            } else {
                SONGS + 8
            }
        } else {
            ROUND_MORE
        };
        let first = handles.len();
        let mut tried = 0;
        while handles.len() - first < target && cursor < ranked.len() && tried < target * 3 {
            let (_, name, from) = &ranked[cursor];
            let from: Vec<usize> = from.clone();
            let name = name.clone();
            cursor += 1;
            tried += 1;
            progress(format!("Finding artists in Roon... {} of {target}", handles.len() - first));
            // The genres of the two seeds it's closest to
            let mut like = HashSet::new();
            for seed in from.iter().take(2) {
                if let Some(Some(h)) = base_of(*seed).and_then(|b| seed_handles.get(b)) {
                    like.extend(genres_of(client, &mut cache, h).await);
                }
            }
            match find_by_name(client, &mut cache, &name, &like).await {
                Some(handle) => {
                    names.push(name.clone());
                    reasons.push(match from.first().filter(|s| **s >= n_seeds).map(|s| asks[s - n_seeds]) {
                        Some(r) => (format!("Suggested because you asked for {}", describe(r, false)), r.song.iter().map(|s| s.artist.trim().to_string()).filter(|a| !a.is_empty()).collect()),
                        None => because(&found, &views, &from),
                    });
                    handles.push(handle);
                    let seed_i = from.first().copied().unwrap_or(0);
                    froms.push(seed_i);
                    let view = base_of(seed_i).and_then(|b| views.get(b));
                    seed_tags.push(view.map(|v| v.tags.clone()).unwrap_or_default());
                    // (the era you played the seed in; the band's own when
                    // the albums you played have no year)
                    let seed_handle = base_of(seed_i).and_then(|b| seed_handles.get(b).cloned().flatten());
                    seed_eras.push(match (view.and_then(|v| v.era), seed_handle) {
                        (Some(era), _) => Some(era),
                        (None, Some(h)) => era_of(client, &mut eras, &h).await,
                        (None, None) => None,
                    });
                }
                None => note(format!("left out {name}: not in Roon, or no genre shared with its seeds")),
            }
        }
        if handles.len() == first {
            if round > 0 && !adding {
                note("no more candidates to try".to_string());
            }
            // (nothing more from this source: tier 2's own picks, if Heavy
            // has any left to ask for)
            let total = picks.iter().filter(|p| !p.is_empty()).count();
            if let Next::Heavy = next_step(total, true, round, false, heavy, heavy_passes) {
                heavy_passes += 1;
                adding = true;
                let more = heavy_suggestions(llm.as_ref(), &found, &views, &known_near, &favorites, &names, &mut asked, &open, &said.avoid, progress).await;
                if !more.is_empty() {
                    cursor = ranked.len();
                    ranked.extend(more);
                    round += 1;
                    continue;
                }
            }
            break;
        }
        progress("Picking songs...".into());
        let (mut batch, _) = client.pick_artist_tracks(&handles[first..], &options).await;
        // With tier 2: a band whose best-known songs all come from other
        // years than the seed's gets a look through its own albums of those
        // years (Sadist's 90s records for a 90s Death listener; Roon's top
        // songs lean to the newest), and tier 2 reads those
        if tier2 {
            for (j, songs) in batch.iter_mut().enumerate() {
                let i = first + j;
                let Some((from, to)) = seed_eras[i] else { continue };
                let (lo, hi) = (from.saturating_sub(ERA_SLACK), to + ERA_SLACK);
                if songs.iter().any(|t| t.year.is_none_or(|y| y >= lo && y <= hi)) {
                    continue;
                }
                progress(format!("Looking through {}'s albums from {lo} to {hi}...", names[i]));
                let never = |t: &roon_library::TopTrack| said.played_songs.iter().any(|p| *p == t.handle || t.track_id.as_deref() == Some(p.as_str()));
                let found: Vec<roon_library::TopTrack> = client.songs_from_years(&handles[i], lo, hi, ERA_ALBUMS, ERA_SONGS).await.into_iter().filter(|t| !never(t)).collect();
                if !found.is_empty() {
                    let albums: HashSet<&str> = found.iter().map(|t| t.album.as_str()).collect();
                    note(format!("{}: its best-known songs are from other years; {} songs from {} of its albums from {lo} to {hi} instead", names[i], found.len(), albums.len()));
                    *songs = found;
                }
            }
        }
        let mut batch_picks = choose_by_album(app, key, &batch, &names[first..], &seed_tags[first..], &seed_eras[first..], tier2, progress).await;
        // Tier 3: every candidate song heard against its seed as you play
        // it; the closest becomes the pick, and an artist with nothing close
        // goes to the back
        if tier3 && !hearing.off {
            let seed_of: Vec<Option<(usize, String)>> = (first..handles.len()).map(|i| base_of(froms[i]).and_then(|b| found.seeds.get(b).map(|(h, _)| (b, h.name.clone())))).collect();
            progress("Listening to the picks and their seeds...".into());
            hear_batch(app, client, &mut hearing, &seed_year, &seed_ever, &seed_of, &names[first..], &batch, first, &mut batch_picks).await;
        }
        candidates.extend(batch);
        picks.extend(batch_picks);
        let with_songs = (first..picks.len()).filter(|i| !picks[*i].is_empty()).count();
        if adding {
            note(format!("tier 2's own picks: {with_songs} of {} found in Roon have a song of their own to pick", handles.len() - first));
        } else {
            note(format!("round {}: {with_songs} of {} artists found in Roon have a song of their own to pick", round + 1, handles.len() - first));
        }
        // Tier 2: the language model reads up on each pick against the
        // seed as you play it, drops the ones that don't fit, and may
        // choose the song (light mode: tier 1's order stands)
        let Some(llm) = &llm else { break };
        progress(if adding {
            format!("Reading up on tier 2's own {with_songs} picks...")
        } else if round == 0 {
            "Reading up on the picks...".to_string()
        } else {
            format!("Reading up on {with_songs} more picks (round {} of {ROUNDS})...", round + 1)
        });
        let judged: Vec<usize> = (first..picks.len()).filter(|i| !picks[*i].is_empty()).collect();
        let discogs = WrappedSettings::load(app).discogs_token.trim().to_string();
        for &i in &judged {
            let seed_i = froms[i];
            if !seed_briefs.contains_key(&seed_i) {
                progress(format!("Reading up on {}...", found.seeds.get(seed_i).map(|(h, _)| h.name.as_str()).unwrap_or("a seed")));
                let mut brief = seed_brief(app, client, key, &discogs, &found, &views, &seed_handles, seed_i).await;
                brief.wishes = wishes.clone();
                seed_briefs.insert(seed_i, brief);
            }
        }
        // (the candidates' briefs four at a time: Roon, Last.fm, Wikipedia
        // and Discogs each keep their own pace)
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let mut jobs = tokio::task::JoinSet::new();
        for (n, &i) in judged.iter().enumerate() {
            let (app, client, key, discogs, gate) = (app.clone(), client.clone(), key.to_string(), discogs.clone(), gate.clone());
            let (name, handle, songs) = (names[i].clone(), handles[i].clone(), candidates[i].clone());
            jobs.spawn(async move {
                let _turn = gate.acquire_owned().await;
                (n, crate::judge::candidate_brief(&app, &client, &key, &discogs, &name, &handle, &songs).await)
            });
        }
        let mut briefs: Vec<Option<crate::judge::CandidateBrief>> = vec![None; judged.len()];
        let mut read = 0;
        while let Some(done) = jobs.join_next().await {
            if let Ok((n, brief)) = done {
                read += 1;
                progress(format!("Reading up on the picks... {read} of {}", judged.len()));
                briefs[n] = Some(brief);
            }
        }
        let pairs: Vec<(crate::judge::SeedBrief, crate::judge::CandidateBrief)> = judged
            .iter()
            .zip(briefs)
            .map(|(&i, b)| {
                let mut brief = b.unwrap_or_else(|| crate::judge::CandidateBrief { name: names[i].clone(), ..Default::default() });
                brief.sound = hearing.lines(&candidates[i]);
                (seed_briefs[&froms[i]].clone(), brief)
            })
            .collect();
        let judging = std::time::Instant::now();
        let verdicts = crate::judge::judge_all(app, llm, pairs, progress).await;
        let took = judging.elapsed().as_secs();
        let (mut kept, mut dropped, mut failed) = (0, 0, 0);
        for (n, &i) in judged.iter().enumerate() {
            match &verdicts[n] {
                Ok(v) if !v.fit || v.confidence < KEEP_AT => {
                    dropped += 1;
                    if v.fit {
                        note(format!("tier 2 dropped {}: too unsure ({:.0}%): {}", names[i], v.confidence * 100.0, v.why));
                    } else {
                        note(format!("tier 2 dropped {}: {} ({:.0}%)", names[i], if v.note.is_empty() { v.why.clone() } else { v.note.clone() }, v.confidence * 100.0));
                    }
                    if !v.comparison.is_empty() {
                        note(format!("    compared: {}", v.comparison));
                    }
                    picks[i].clear();
                }
                Ok(v) => {
                    kept += 1;
                    liking.insert(i, v.confidence);
                    if let Some(song) = &v.song {
                        if let Some(t) = candidates[i].iter().find(|t| t.title.eq_ignore_ascii_case(song)) {
                            if picks[i].first() != Some(&t.handle) {
                                note(format!("tier 2 chose {} - {} ({}) over the album pick", names[i], t.title, t.album));
                            }
                            picks[i] = vec![t.handle.clone()];
                        }
                    }
                    if !v.why.is_empty() {
                        for h in &picks[i] {
                            song_why.insert(h.clone(), v.why.clone());
                        }
                    }
                    note(format!("tier 2 kept {}: {} ({:.0}%)", names[i], v.why, v.confidence * 100.0));
                    if !v.comparison.is_empty() {
                        note(format!("    compared: {}", v.comparison));
                    }
                }
                Err(e) => {
                    failed += 1;
                    note(format!("tier 2 couldn't judge {}: {e}", names[i]));
                }
            }
        }
        let total = picks.iter().filter(|p| !p.is_empty()).count();
        let which = if adding { "its own picks".to_string() } else { format!("round {}", round + 1) };
        note(format!("tier 2 {which} ({} {}): {kept} kept, {dropped} dropped, {failed} not judged in {took} s; {total} songs so far", llm.provider, llm.model));
        round += 1;
        match next_step(total, adding, round, cursor < ranked.len(), heavy, heavy_passes) {
            Next::Tier1 => {
                note(format!("under {SONGS} songs: back to tier 1 for {ROUND_MORE} more"));
                continue;
            }
            Next::Heavy => {
                heavy_passes += 1;
                adding = true;
                let more = heavy_suggestions(Some(llm), &found, &views, &known_near, &favorites, &names, &mut asked, &open, &said.avoid, progress).await;
                if !more.is_empty() {
                    cursor = ranked.len();
                    ranked.extend(more);
                    continue;
                }
                break;
            }
            Next::Done => break,
        }
    }
    let mut kept: Vec<usize> = (0..picks.len()).filter(|i| !picks[*i].is_empty()).collect();
    if tier3 && hearing.heard > 0 {
        note(format!(
            "tier 3 heard {} songs ({} from your files) in {} s; {} artists sound far from their seed, {} middling",
            hearing.heard,
            hearing.files,
            hearing.secs,
            kept.iter().filter(|i| hearing.far.contains(i)).count(),
            kept.iter().filter(|i| hearing.mid.contains(i)).count()
        ));
        // (a hard nudge: the far ones to the back, used only when the list
        // is short; the middling ones after the close ones)
        kept.sort_by_key(|i| hearing.bucket(*i));
    }
    // Heavy: tier 2 puts the list in order (and so decides which make the
    // cut when there are more than a list holds); Light, or when that
    // fails: tier 1's order, spread by seed
    let ordered = match (&llm, heavy && kept.len() > 1) {
        (Some(llm), true) => heavy_order(llm, &kept, &picks, &candidates, &names, &froms, &seed_names, &liking, &song_why, progress).await,
        _ => None,
    };
    let mut out = match ordered {
        Some(mut order) => {
            // (tier 2 saw the sound; the far ones still go last)
            order.sort_by_key(|i| hearing.far.contains(i));
            picked_in_order(&order.into_iter().take(SONGS).collect::<Vec<_>>(), &picks, &names, &reasons)
        }
        None => picked(&kept.into_iter().take(SONGS).collect::<Vec<_>>(), &picks, &names, &reasons),
    };
    out.song_why = song_why;
    out.requests_used = !asks.is_empty();
    Ok(out)
}

/// Tier 2's rounds: at most this many of tier 1's passes in all; another
/// one only while the list has fewer than SONGS songs, each bringing
/// ROUND_MORE more artists from tier 1.
const ROUNDS: usize = 3;
const ROUND_MORE: usize = 15;

/// Tier 2 keeps a pick only when it judges you're at least this likely to
/// enjoy it: below that it's a coin toss (Alter Bridge and Staind for an
/// Audioslave listener came in at 50%).
const KEEP_AT: f64 = 0.6;

/// What a build does after a round: another of tier 1's, tier 2's own
/// picks (Heavy: once in any case, a second time while the list is short),
/// or done.
enum Next {
    Tier1,
    Heavy,
    Done,
}

fn next_step(total: usize, adding: bool, round: usize, tier1_left: bool, heavy: bool, heavy_passes: usize) -> Next {
    if !adding && total < SONGS && round < ROUNDS && tier1_left {
        return Next::Tier1;
    }
    if heavy && heavy_passes < HEAVY_PASSES && (heavy_passes == 0 || total < SONGS) {
        return Next::Heavy;
    }
    Next::Done
}

/// Your songs by artist from the history (lower-cased artist): title, the
/// Core's track id and how often you listened to it, most first. Plays from
/// the lists don't count.
fn songs_by_artist(app: &AppHandle, ctx: &Context, since: Option<i64>) -> HashMap<String, Vec<(String, u64, u32)>> {
    app.state::<Wrapped>().with_plays(app, |plays| {
        let mut by: HashMap<String, HashMap<String, (String, u64, u32)>> = HashMap::new();
        for p in plays {
            let Some(id) = p.track_id else { continue };
            if since.is_some_and(|at| p.played_at < at) {
                break; // newest first: the rest is older
            }
            if p.artist.is_empty() || !counts(p) || ctx.from_lists.contains(&(p.played_at, p.title.to_lowercase())) {
                continue;
            }
            let row = by.entry(p.artist.to_lowercase()).or_default().entry(p.title.to_lowercase()).or_insert_with(|| (p.title.clone(), id, 0));
            row.2 += 1;
        }
        by.into_iter()
            .map(|(k, songs)| {
                let mut v: Vec<(String, u64, u32)> = songs.into_values().collect();
                v.sort_by(|a, b| b.2.cmp(&a.2));
                (k, v)
            })
            .collect()
    })
}

/// Library Rediscover: forgotten gems, not a second Discover. Two thirds
/// old favorites (artists you played a lot and haven't for a year or more,
/// with one of your own most played songs of theirs), a third never played
/// (library artists you've never touched, nudged toward your taste as a
/// whole, shuffled each week). No Last.fm neighbours and no tier 2: it's
/// your own library and your own history.
async fn pick_rediscover(app: &AppHandle, client: &LibraryClient, _key: &str, recently: &HashSet<String>, said: &Feedback, ctx: &Context, requests: &[Request], progress: &impl Fn(String)) -> Result<Picked, String> {
    progress("Reading your listening and library...".into());
    let now = now_ms();
    let heard = history_by_artist(app, now, ctx);
    let library = app.state::<LibraryCopy>().artists(app);
    if library.is_empty() {
        return Err("Wrapped has no copy of your library's artists yet (Wrapped's Settings reads it).".into());
    }
    let mut jitter = Jitter::new();
    let open = |k: &str| !recently.contains(k) && !said.down.contains(k) && k != "various artists";

    // Fine Tuning: tier 2 names the library artists whose music answers
    // the requests; their songs open the list (the whole list when the
    // request asks for all or only of something, else a third), the
    // forgotten ones among them first
    let mut asked: Vec<(String, Vec<String>)> = Vec::new();
    let mut requests_used = false;
    if !requests.is_empty() {
        match crate::dive::llm_or_err(app) {
            Ok(llm) => {
                let said_all: Vec<String> = requests.iter().map(|r| describe(r, true)).collect();
                let takeover = said_all.iter().any(|t| {
                    let t = format!(" {} ", t.to_lowercase());
                    [" all ", " only ", " entire ", " whole ", " every ", " nothing but ", " just "].iter().any(|w| t.contains(w))
                });
                let want = if takeover { SONGS } else { SONGS / 3 };
                let brief = format!(
                    "WHAT THEY ASKED FOR (a playlist from their own library, of music they haven't played in a long time or never):\n{}\n",
                    said_all.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n")
                );
                let names: Vec<String> = library.iter().filter(|a| open(&a.name.to_lowercase())).map(|a| a.name.clone()).collect();
                progress("Reading the Fine Tuning...".into());
                let picked = crate::dive::library_artists_for(&llm, &brief, &names, want + 10, progress).await;
                // (the ones not played for a year, or never, first)
                let forgotten = |name: &str| heard.get(&name.to_lowercase()).is_none_or(|h| now - h.last_played >= 365 * DAY_MS);
                let mut chosen: Vec<String> = picked.iter().map(|i| names[*i].clone()).collect();
                chosen.sort_by_key(|n| !forgotten(n));
                chosen.truncate(want);
                note(format!("fine tuning ({}): {} library artists named: {}", said_all.join("; "), chosen.len(), chosen.join(", ")));
                requests_used = !chosen.is_empty();
                asked = chosen.into_iter().map(|n| (n, said_all.clone())).collect();
            }
            Err(e) => note(format!("fine tuning skipped: {e}")),
        }
    }
    let asked_names: HashSet<String> = asked.iter().map(|(n, _)| n.to_lowercase()).collect();

    // Old favorites: played a lot (and not mostly skipped), not for a year;
    // the most played first, shuffled a little so the list moves each week
    let mut old: Vec<(f64, String, u32, i64)> = Vec::new();
    // Never played: in the library, nothing in the history
    let mut never: Vec<(String, u64)> = Vec::new();
    for artist in &library {
        let k = artist.name.to_lowercase();
        if !open(&k) || asked_names.contains(&k) {
            continue;
        }
        match heard.get(&k) {
            Some(h) if now - h.last_played >= 365 * DAY_MS && h.listens >= OLD_FAVORITE_LISTENS && h.listens * 2 >= h.plays => {
                old.push(((h.listens as f64).ln() * (0.75 + 0.5 * jitter.next()), artist.name.clone(), h.listens, h.last_played));
            }
            None => {
                if let Some(id) = artist.performer_id {
                    never.push((artist.name.clone(), id));
                }
            }
            Some(_) => {}
        }
    }
    old.sort_by(|a, b| b.0.total_cmp(&a.0));
    // (your taste as a whole: the most played artists ever, by their tags)
    let mut all_time: Vec<(Heard, f64)> = heard.values().filter(|h| h.listens > 0).map(|h| (h.clone(), h.listens as f64)).collect();
    all_time.sort_by(|a, b| b.1.total_cmp(&a.1));
    all_time.truncate(40);
    let profile = taste(app, &all_time);
    let tags = app.state::<Tags>();
    let mut never: Vec<(f64, String, u64)> = never
        .into_iter()
        .map(|(name, id)| {
            let near = tags.cached(app, &name).map(|t| sound_match(&profile, &t.tags)).unwrap_or(0.0);
            (near + 0.35 * jitter.next(), name, id)
        })
        .collect();
    never.sort_by(|a, b| b.0.total_cmp(&a.0));
    note(format!("rediscover pool: {} old favorites not played for a year, {} never played", old.len(), never.len()));

    let mut names: Vec<String> = Vec::new();
    let mut reasons: Vec<(String, Vec<String>)> = Vec::new();
    let mut picks: Vec<Vec<String>> = Vec::new();
    let songs = songs_by_artist(app, ctx, None);
    let played: HashSet<&str> = said.played_songs.iter().map(String::as_str).collect();

    // The Fine Tuning's artists first: one of your most played songs of
    // theirs when you've played them, else one of their best known in
    // your library
    if !asked.is_empty() {
        progress("Finding the Fine Tuning's songs...".into());
        let mut by_library: Vec<(String, String)> = Vec::new();
        for (name, said_all) in &asked {
            let mine: Vec<&(String, u64, u32)> = songs.get(&name.to_lowercase()).map(|v| v.iter().filter(|s| !played.contains(s.1.to_string().as_str())).take(3).collect()).unwrap_or_default();
            let mut handle = None;
            for (_, id, _) in &mine {
                if let Ok(h) = client.track_by_id(*id).await {
                    handle = Some(h);
                    break;
                }
            }
            match handle {
                Some(h) => {
                    names.push(name.clone());
                    reasons.push((format!("From your Fine Tuning: {}", and_list(said_all)), Vec::new()));
                    picks.push(vec![h]);
                }
                None => {
                    if let Some(card) = library.iter().find(|a| a.name == *name) {
                        if let Some(id) = card.performer_id {
                            if let Ok(c) = client.artist_by_id(id).await {
                                by_library.push((name.clone(), c.handle));
                            }
                        }
                    }
                }
            }
        }
        if !by_library.is_empty() {
            let handles: Vec<String> = by_library.iter().map(|(_, h)| h.clone()).collect();
            let options = MixOptions { per_artist: 1, include_live: false, library_only: true, own_albums: true, from_top: 5, never: said.played_songs.clone() };
            let (found, _) = client.pick_artist_tracks(&handles, &options).await;
            for (i, list) in found.iter().enumerate() {
                if let Some(song) = list.first() {
                    names.push(by_library[i].0.clone());
                    reasons.push((format!("From your Fine Tuning: {}", and_list(&asked[0].1)), Vec::new()));
                    picks.push(vec![song.handle.clone()]);
                }
            }
        }
        note(format!("fine tuning: {} songs", picks.len()));
    }
    let from_requests = picks.len();
    let room = SONGS.saturating_sub(from_requests);
    let want_old = (room * 2 / 3).min(old.len());
    let want_never = room - want_old;

    // Old favorites' songs: one of your three most played of theirs (not one
    // a list already brought), straight from the history
    for (_, name, listens, last) in &old {
        if picks.len() >= from_requests + want_old {
            break;
        }
        progress(format!("Finding your old favorites... {} of {want_old}", picks.len() - from_requests));
        let mut mine: Vec<&(String, u64, u32)> = songs.get(&name.to_lowercase()).map(|v| v.iter().filter(|s| !played.contains(s.1.to_string().as_str())).take(3).collect()).unwrap_or_default();
        // (a different one of the three each week)
        let start = (jitter.next() * mine.len() as f64) as usize;
        let turn = start.min(mine.len().saturating_sub(1));
        mine.rotate_left(turn);
        let mut handle = None;
        for (_, id, _) in &mine {
            if let Ok(h) = client.track_by_id(*id).await {
                handle = Some(h);
                break;
            }
        }
        let Some(handle) = handle else {
            note(format!("left out {name}: none of your songs of theirs found in Roon"));
            continue;
        };
        let year = civil_from_days(last / DAY_MS).0;
        names.push(name.clone());
        reasons.push((format!("An old favorite: {listens} listens, last played in {year}"), Vec::new()));
        picks.push(vec![handle]);
    }
    let old_found = picks.len() - from_requests;

    // Never played: one of their best known songs in your library
    let want_never = want_never + want_old.saturating_sub(old_found);
    let mut handles = Vec::new();
    let mut never_names = Vec::new();
    for (_, name, id) in never.iter().take(want_never + 10) {
        progress(format!("Finding artists in Roon... {} of {want_never}", handles.len()));
        if let Ok(card) = client.artist_by_id(*id).await {
            handles.push(card.handle);
            never_names.push(name.clone());
        }
    }
    progress("Picking songs...".into());
    let options = MixOptions { per_artist: 1, include_live: false, library_only: true, own_albums: true, from_top: 5, never: said.played_songs.clone() };
    let (found_songs, _) = client.pick_artist_tracks(&handles, &options).await;
    let order_never: Vec<(usize, String)> = found_songs.iter().enumerate().filter_map(|(i, s)| s.first().map(|t| (i, t.handle.clone()))).collect();
    let mut never_found = 0;
    for (i, handle) in order_never {
        if never_found >= want_never {
            break;
        }
        names.push(never_names[i].clone());
        reasons.push(("In your library, never played".to_string(), Vec::new()));
        picks.push(vec![handle]);
        never_found += 1;
    }
    note(format!("rediscover: {from_requests} from the fine tuning, {old_found} old favorites and {never_found} never played"));
    // (the fine tuning's first, then old favorites and never-played mixed
    // through the list)
    let mut order: Vec<usize> = (0..from_requests).collect();
    let (mut a, mut b) = (from_requests..from_requests + old_found, from_requests + old_found..picks.len());
    loop {
        let (x, y, z) = (a.next(), a.next(), b.next());
        if x.is_none() && z.is_none() {
            break;
        }
        order.extend(x);
        order.extend(y);
        order.extend(z);
    }
    let names_o: Vec<String> = order.iter().map(|i| names[*i].clone()).collect();
    let reasons_o: Vec<(String, Vec<String>)> = order.iter().map(|i| reasons[*i].clone()).collect();
    let picks_o: Vec<Vec<String>> = order.iter().map(|i| picks[*i].clone()).collect();
    // (no seeds named, so `picked` keeps this order)
    let chosen: Vec<usize> = (0..picks_o.len()).collect();
    let mut out = picked(&chosen, &picks_o, &names_o, &reasons_o);
    out.requests_used = requests_used;
    Ok(out)
}

/// An old favorite needs at least this many listens (half a song or more).
const OLD_FAVORITE_LISTENS: u32 = 8;

/// Saves the new playlist and moves the older ones along: the previous
/// current one gets its date, anything beyond two dated copies goes.
/// Album covers kept per list, for its card.
const COVERS: usize = 4;

async fn publish(app: &AppHandle, client: &LibraryClient, which: Which, picked: Picked, offset: i32) -> Result<(), String> {
    let now = now_ms();
    let date = short_date(now, offset);
    let description = format!("Built by Roon: Wrapped on {date}: {}.", which.blurb());
    let discover = app.state::<Discover>();
    let mut list = discover.with_store(app, |s| s.list(which).clone());
    let playlists = client.playlists().unwrap_or_default();
    let find = |id: &Option<String>| playlists.iter().find(|p| id.is_some() && p.playlist_id == *id).cloned();
    let mut taken: HashSet<String> = playlists.iter().map(|p| p.name.clone()).collect();
    // A name not in use yet: "🌟 Discover Weekly, Oct 17", then "(2)"...
    let mut free_name = |base: String| -> String {
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base} ({n})");
            n += 1;
        }
        taken.insert(name.clone());
        name
    };
    // Roon won't have two playlists of one name: the current one gets its
    // date before the new one is made (and back if making it fails), and
    // any other playlist by that name is moved aside the same way
    let mut moved: Vec<(String, String)> = Vec::new();
    let current_id = list.first().and_then(|b| b.playlist_id.clone());
    if let Some(previous) = list.first_mut() {
        if let Some(found) = find(&previous.playlist_id) {
            let dated = free_name(format!("{}, {}", which.name(), previous.date));
            match client.rename_playlist(&found.handle, &dated).await {
                Ok(()) => {
                    moved.push((found.handle.clone(), found.name.clone()));
                    previous.name = dated;
                }
                Err(e) => note(format!("couldn't rename {}: {e}", found.name)),
            }
        }
    }
    for other in playlists.iter().filter(|p| p.name == which.name() && (current_id.is_none() || p.playlist_id != current_id)) {
        let aside = free_name(format!("{}, older", which.name()));
        match client.rename_playlist(&other.handle, &aside).await {
            Ok(()) => moved.push((other.handle.clone(), other.name.clone())),
            Err(e) => note(format!("couldn't move {} aside: {e}", other.name)),
        }
    }
    let card = match client.create_playlist_described(which.name(), &description, &picked.songs).await {
        Ok(card) => card,
        Err(e) => {
            for (handle, name) in moved {
                let _ = client.rename_playlist(&handle, &name).await;
            }
            return Err(e.to_string());
        }
    };
    let tracks = picked
        .songs
        .iter()
        .filter_map(|h| {
            Some(ListSong {
                track_id: client.track_id_of(h)?,
                artist: picked.song_artist.get(h).map(|(a, _, _)| a.clone()).unwrap_or_default(),
                title: client.track_title_of(h).unwrap_or_default(),
                reason: picked.song_artist.get(h).map(|(_, r, _)| r.clone()).unwrap_or_default(),
                because: picked.song_artist.get(h).map(|(_, _, b)| b.clone()).unwrap_or_default(),
                why: picked.song_why.get(h).cloned().unwrap_or_default(),
            })
        })
        .collect();
    let mut covers: Vec<String> = Vec::new();
    for h in &picked.songs {
        if covers.len() >= COVERS {
            break;
        }
        if let Some(key) = client.track_image_of(h) {
            if !covers.contains(&key) {
                covers.push(key);
            }
        }
    }
    list.insert(
        0,
        Built {
            playlist_id: card.playlist_id.clone(),
            name: card.name.clone(),
            built_at: now,
            date,
            songs: picked.songs.len(),
            artists: picked.artists,
            tracks,
            covers,
        },
    );
    let mut gone_lists = Vec::new();
    while list.len() > 1 + KEEP_OLD {
        let gone = list.pop().unwrap();
        if let Some(found) = find(&gone.playlist_id) {
            if let Err(e) = client.delete_playlist(&found.handle).await {
                note(format!("couldn't delete {}: {e}", found.name));
            }
        }
        gone_lists.push(gone);
    }
    discover.with_store(app, |s| {
        *s.list(which) = list;
        for gone in gone_lists {
            for t in &gone.tracks {
                s.past.push(Past { which, artist: t.artist.clone(), track_id: t.track_id.clone(), title: t.title.clone(), built_at: gone.built_at });
            }
        }
        // (artists older than REMEMBER_LISTS lists may come back; their songs never)
        let cutoff = {
            let mut dates: Vec<i64> = s.lists(which).iter().map(|b| b.built_at).chain(s.past.iter().filter(|p| p.which == which).map(|p| p.built_at)).collect();
            dates.sort_unstable_by(|a, b| b.cmp(a));
            dates.dedup();
            dates.get(REMEMBER_LISTS - 1).copied().unwrap_or(0)
        };
        for p in s.past.iter_mut().filter(|p| p.which == which && p.built_at < cutoff) {
            p.artist.clear();
        }
    });
    discover.save(app);
    Ok(())
}

fn status(app: &AppHandle) -> DiscoverStatus {
    let discover = app.state::<Discover>();
    let playlists = app.state::<Library>().client().and_then(|c| c.playlists().ok()).unwrap_or_default();
    let card = |b: &Built| BuiltCard {
        name: b.name.clone(),
        built_at: b.built_at,
        date: b.date.clone(),
        songs: b.songs,
        artists: b.artists.clone(),
        playlist: playlists.iter().find(|p| b.playlist_id.is_some() && p.playlist_id == b.playlist_id).cloned(),
        tracks: b.tracks.clone(),
        covers: b.covers.clone(),
    };
    let (weekly, rediscover, requests, rediscover_requests) = discover.with_store(app, |s| {
        let requests = s.requests.iter().filter(|r| r.which() == Which::Weekly).map(|r| RequestCard { request: r.clone(), said: describe(r, true) }).collect();
        let rediscover_requests = s.requests.iter().filter(|r| r.which() == Which::Rediscover).map(|r| RequestCard { request: r.clone(), said: describe(r, true) }).collect();
        (s.weekly.iter().map(card).collect(), s.rediscover.iter().map(card).collect(), requests, rediscover_requests)
    });
    let building = *discover.building.lock().unwrap();
    let failed = discover.failed.lock().unwrap().clone();
    let tier2 = crate::judge::Llm::from_settings(&WrappedSettings::load(app)).is_some();
    let tier3 = crate::sound::Sound::ready(app);
    DiscoverStatus { weekly, rediscover, building, weekly_error: failed.get("weekly").cloned(), rediscover_error: failed.get("rediscover").cloned(), requests, rediscover_requests, tier2, tier3 }
}

/// A build number for the title bar, bumped by hand with each push: it
/// comes from Rust so it only changes when the app was really rebuilt
/// (page files reload on their own in a dev build).
pub const BUILD: u32 = 1092;

/// The build number, for a dev build's title bar only (a release shows
/// none).
#[tauri::command]
pub fn wrapper_build() -> Option<u32> {
    cfg!(debug_assertions).then_some(BUILD)
}

#[tauri::command]
pub fn wrapped_discover_status(app: AppHandle) -> DiscoverStatus {
    status(&app)
}

/// Builds one of the weekly playlists now (progress as "wrapped-discover"
/// events: `{ which, text }`). `utcOffsetMinutes`: the PC's offset east of
/// UTC, for the dates in the names.
#[tauri::command]
pub async fn wrapped_discover_build(app: AppHandle, which: Which, utc_offset_minutes: Option<i32>) -> Result<DiscoverStatus, String> {
    if !enabled(&app) {
        return Err("Wrapped Discovery is off (Wrapped's Settings).".into());
    }
    let discover = app.state::<Discover>();
    {
        let mut building = discover.building.lock().unwrap();
        if building.is_some() {
            return Err("A list is being built already.".into());
        }
        *building = Some(which);
    }
    if let Some(offset) = utc_offset_minutes {
        discover.with_store(&app, |s| s.utc_offset_minutes = offset);
    }
    LOG.lock().unwrap().clear();
    note(format!("building {} ({})", which.name(), short_date(now_ms(), discover.with_store(&app, |s| s.utc_offset_minutes))));
    let result = build(&app, which).await;
    let slot = if which == Which::Weekly { "weekly" } else { "rediscover" };
    match &result {
        Ok(()) => {
            discover.failed.lock().unwrap().remove(slot);
        }
        Err(e) => {
            note(format!("failed: {e}"));
            discover.failed.lock().unwrap().insert(slot.to_string(), e.clone());
        }
    }
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::write(dir.join("discover-log.txt"), LOG.lock().unwrap().join("\n"));
    }
    *discover.building.lock().unwrap() = None;
    let _ = app.emit("wrapped-discover", serde_json::json!({ "which": which, "done": true }));
    result.map(|_| status(&app))
}

async fn build(app: &AppHandle, which: Which) -> Result<(), String> {
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    let key = WrappedSettings::load(app).lastfm_key;
    // The latest plays first: what you did with the last lists is in them
    let app2 = app.clone();
    let _ = app2.emit("wrapped-discover", serde_json::json!({ "which": which, "text": "Reading your latest plays from Roon..." }));
    match crate::wrapped::history_catch_up(app).await {
        Ok(n) => note(format!("history: {n} new plays read")),
        Err(e) => note(format!("history not refreshed: {e}")),
    }
    let discover = app.state::<Discover>();
    let (mine, offset, votes, brought, others, every, shown, suggested_by) = discover.with_store(app, |s| {
        let offset = s.utc_offset_minutes;
        // (lists from before "Start fresh" count only for telling list
        // plays apart, see `every`)
        let fresh = s.reset_at;
        // Every artist and song the lists brought, with when
        // (the deleted lists' too: a skip on an old list still counts)
        let brought: Vec<(String, i64)> = s
            .weekly
            .iter()
            .chain(s.rediscover.iter())
            .flat_map(|b| b.artists.iter().map(|a| (a.clone(), b.built_at)))
            .chain(s.past.iter().filter(|p| !p.artist.is_empty()).map(|p| (p.artist.clone(), p.built_at)))
            .filter(|(_, at)| *at >= fresh)
            .collect();
        let mine: Vec<(String, String, String, i64)> = s
            .lists(which)
            .iter()
            .take(REMEMBER_LISTS)
            .flat_map(|b| b.tracks.iter().map(move |t| (t.artist.clone(), t.track_id.clone(), t.title.clone(), b.built_at)))
            .chain(s.past.iter().filter(|p| p.which == which).map(|p| (p.artist.clone(), p.track_id.clone(), p.title.clone(), p.built_at)))
            .filter(|(_, _, _, at)| *at >= fresh)
            .collect();
        let others: Vec<String> = s
            .lists(match which {
                Which::Weekly => Which::Rediscover,
                Which::Rediscover => Which::Weekly,
            })
            .iter()
            .filter(|b| b.built_at >= fresh)
            .flat_map(|b| b.tracks.iter().map(|t| t.track_id.clone()))
            .chain(s.past.iter().filter(|p| p.which != which && p.built_at >= fresh).map(|p| p.track_id.clone()))
            .collect();
        // Every song of every list, by list (which and when built)
        let every: Vec<(String, String, String, String, i64)> = s
            .weekly
            .iter()
            .map(|b| (Which::Weekly, b))
            .chain(s.rediscover.iter().map(|b| (Which::Rediscover, b)))
            .flat_map(|(w, b)| b.tracks.iter().map(move |t| (format!("{}@{}", w.name(), b.built_at), t.track_id.clone(), t.artist.clone(), t.title.clone(), b.built_at)))
            .chain(s.past.iter().map(|p| (format!("{}@{}", p.which.name(), p.built_at), p.track_id.clone(), p.artist.clone(), p.title.clone(), p.built_at)))
            .collect();
        // Which seeds suggested each artist on the lists still kept
        let mut suggested_by: HashMap<String, Vec<String>> = HashMap::new();
        for song in s.weekly.iter().chain(s.rediscover.iter()).filter(|b| b.built_at >= fresh).flat_map(|b| b.tracks.iter()) {
            let seeds = suggested_by.entry(song.artist.to_lowercase()).or_default();
            for name in &song.because {
                if !seeds.contains(name) {
                    seeds.push(name.clone());
                }
            }
        }
        (mine, offset, s.votes.clone(), brought, others, every, s.shown.clone(), suggested_by)
    });
    // The lists' songs that came up on a zone: (artist, title) heard at all,
    // and the artists whose songs were only ever skipped
    let shown_songs: HashSet<(String, String)> = shown.iter().map(|x| (x.artist.to_lowercase(), x.title.to_lowercase())).collect();
    let mut skipped_artists: HashSet<String> = shown.iter().filter(|x| x.heard < SKIPPED_WITHIN).map(|x| x.artist.to_lowercase()).collect();
    for x in shown.iter().filter(|x| x.heard >= SKIPPED_WITHIN) {
        skipped_artists.remove(&x.artist.to_lowercase());
    }
    // Plays from the lists don't feed the next one; Recent Activity says
    // what you started on purpose
    let _ = app.emit("wrapped-discover", serde_json::json!({ "which": which, "text": "Reading your Recent Activity..." }));
    let ctx = Context { from_lists: plays_from_lists(app, &every), intent: intent_from_recents(&client, now_ms()).await };
    note(format!("{} plays came from the Discover lists and don't count as listening", ctx.from_lists.len()));
    // A song you never played carries no judgment: only the ones you did
    // play (even skipped) stay out, and only their artists rest for a
    // while. The rest of a list you didn't get to may come around again.
    let heard_since = since_brought(app, &brought);
    let played_titles = played_titles(app, &brought);
    let mut recently: HashSet<String> = HashSet::new();
    let mut past_songs: Vec<String> = others;
    // (the newest REST_UNPLAYED lists' build times: their unplayed artists
    // rest too)
    let mut dates: Vec<i64> = mine.iter().map(|(_, _, _, at)| *at).collect();
    dates.sort_unstable_by(|a, b| b.cmp(a));
    dates.dedup();
    let rest_after = dates.get(REST_UNPLAYED - 1).copied().unwrap_or(i64::MIN);
    let (mut kept, mut resting, mut freed) = (0, 0, 0);
    for (artist, track_id, title, at) in &mine {
        let k = artist.to_lowercase();
        let played = heard_since.get(&k).is_some_and(|(_, plays)| *plays > 0)
            || played_titles.contains(&(k.clone(), title.to_lowercase()))
            || shown_songs.contains(&(k.clone(), title.to_lowercase()))
            || skipped_artists.contains(&k);
        if played {
            kept += 1;
            if !artist.is_empty() {
                recently.insert(k);
            }
            past_songs.push(track_id.clone());
        } else if *at >= rest_after {
            resting += 1;
            if !artist.is_empty() {
                recently.insert(k);
            }
        } else {
            freed += 1;
        }
    }
    note(format!("past lists: {kept} songs played stay out, {resting} unplayed rest for now, {freed} older unplayed may come back"));
    let app2 = app.clone();
    let progress = move |text: String| {
        let _ = app2.emit("wrapped-discover", serde_json::json!({ "which": which, "text": text }));
    };
    let started = std::time::Instant::now();
    progress("Reading your ratings...".into());
    let mut said = feedback(app, &key, &history_by_artist(app, now_ms(), &ctx), &votes, &brought, &skipped_artists, &suggested_by).await;
    said.played_songs = past_songs;
    note(format!(
        "ratings: {} turned down, {} liked as seeds ({}), {} sounds to keep away from",
        said.down.len(),
        said.liked.len(),
        said.liked.iter().map(|(h, w)| format!("{} {w}", h.name)).collect::<Vec<_>>().join(", "),
        said.avoid.len()
    ));
    let requests: Vec<Request> = discover.with_store(app, |s| s.requests.iter().filter(|r| r.which() == which).cloned().collect());
    let picked = match which {
        Which::Weekly => pick_weekly(app, &client, &key, &recently, &said, &ctx, &requests, &progress).await?,
        Which::Rediscover => pick_rediscover(app, &client, &key, &recently, &said, &ctx, &requests, &progress).await?,
    };
    if picked.songs.is_empty() {
        return Err("Roon found no songs for the artists picked.".into());
    }
    progress("Saving the playlist...".into());
    let (songs, artists) = (picked.songs.len(), picked.artists.len());
    let answered = picked.requests_used;
    publish(app, &client, which, picked, offset).await?;
    // (the requests this list answered are done with, unless kept for
    // every list; ones asked during the build wait for the next)
    if answered {
        let done: HashSet<i64> = requests.iter().filter(|r| !r.permanent).map(|r| r.id).collect();
        discover.with_store(app, |s| s.requests.retain(|r| r.permanent || !done.contains(&r.id)));
        discover.save(app);
    }
    note(format!("{}: {songs} songs from {artists} artists in {} s", which.name(), started.elapsed().as_secs()));
    Ok(())
}

/// A song from one of the lists, for the player's thumbs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverSong {
    /// Which list it's on (with `which`): the list's build time.
    pub built_at: i64,
    pub track_id: String,
    pub artist: String,
    pub title: String,
    pub reason: String,
    pub because: Vec<String>,
    pub why: String,
    pub which: Which,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ratings {
    /// Every song in the current lists and their dated copies.
    pub songs: Vec<DiscoverSong>,
    /// By lower-cased artist name.
    pub votes: HashMap<String, Vote>,
    /// When each current list was built (for the "new list" dot).
    pub newest: Vec<i64>,
}

fn ratings(app: &AppHandle) -> Ratings {
    app.state::<Discover>().with_store(app, |s| {
        let mut songs = Vec::new();
        for (which, list) in [(Which::Weekly, &s.weekly), (Which::Rediscover, &s.rediscover)] {
            for b in list {
                songs.extend(b.tracks.iter().map(|t| DiscoverSong {
                    built_at: b.built_at,
                    track_id: t.track_id.clone(),
                    artist: t.artist.clone(),
                    title: t.title.clone(),
                    reason: t.reason.clone(),
                    because: t.because.clone(),
                    why: t.why.clone(),
                    which,
                }));
            }
        }
        let newest = [s.weekly.first(), s.rediscover.first()].into_iter().flatten().map(|b| b.built_at).collect();
        Ratings { songs, votes: s.votes.clone(), newest }
    })
}

#[tauri::command]
pub fn wrapped_discover_ratings(app: AppHandle) -> Ratings {
    ratings(&app)
}

/// Rates an artist a list brought: `vote` 1 (thumbs up), -1 (down) or 0
/// (taken back); `heart` when its song was hearted.
#[tauri::command]
pub fn wrapped_discover_vote(app: AppHandle, artist: String, vote: Option<i8>, heart: Option<bool>) -> Ratings {
    let discover = app.state::<Discover>();
    discover.with_store(&app, |s| {
        let row = s.votes.entry(artist.to_lowercase()).or_insert_with(|| Vote { name: artist.clone(), ..Default::default() });
        if let Some(vote) = vote {
            row.vote = vote.clamp(-1, 1);
        }
        if let Some(heart) = heart {
            row.heart = heart;
        }
        row.at = now_ms();
        if row.vote == 0 && !row.heart {
            s.votes.remove(&artist.to_lowercase());
        }
    });
    discover.save(&app);
    ratings(&app)
}

/// The ban list: artists never suggested again (Discover, Deep Dive, the
/// radio, Home); a thumbs down on a Discover song is the same thing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Banned {
    pub name: String,
    pub at: i64,
}

fn ban_list(app: &AppHandle) -> Vec<Banned> {
    let mut out: Vec<Banned> = app.state::<Discover>().with_store(app, |s| s.votes.values().filter(|v| v.vote < 0).map(|v| Banned { name: v.name.clone(), at: v.at }).collect());
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out
}

#[tauri::command]
pub fn wrapped_ban_list(app: AppHandle) -> Vec<Banned> {
    ban_list(&app)
}

#[tauri::command]
pub fn wrapped_ban_add(app: AppHandle, artist: String) -> Vec<Banned> {
    if !artist.trim().is_empty() {
        wrapped_discover_vote(app.clone(), artist.trim().to_string(), Some(-1), Some(false));
        crate::home::banned(&app, artist.trim());
    }
    ban_list(&app)
}

#[tauri::command]
pub fn wrapped_ban_remove(app: AppHandle, artist: String) -> Vec<Banned> {
    wrapped_discover_vote(app.clone(), artist, Some(0), None);
    ban_list(&app)
}

/// "Start fresh": ratings and skips forgotten, and the songs and artists of
/// the lists so far free to come back. The lists themselves stay (and their
/// songs still don't count as your listening).
#[tauri::command]
pub fn wrapped_discover_reset(app: AppHandle) -> DiscoverStatus {
    let discover = app.state::<Discover>();
    let (votes, shown) = discover.with_store(&app, |s| {
        let counts = (s.votes.len(), s.shown.len());
        s.votes.clear();
        s.shown.clear();
        s.reset_at = now_ms();
        counts
    });
    discover.save(&app);
    note(format!("started fresh: {votes} ratings and {shown} heard songs forgotten, earlier lists' songs free again"));
    status(&app)
}

/// Asks the next Discover Weekly for something: `song` with `aspects` of
/// it (the player's brain button), `text` in your own words, or both.
#[tauri::command]
pub fn wrapped_discover_request_add(app: AppHandle, text: String, song: Option<RequestSong>, aspects: Option<Vec<String>>, which: Option<Which>) -> Result<DiscoverStatus, String> {
    let text: String = text.trim().chars().take(300).collect();
    let song = song.filter(|s| !s.title.trim().is_empty());
    if text.is_empty() && song.is_none() {
        return Err("Say what you'd like, or ask from a song.".into());
    }
    let aspects: Vec<String> = aspects.unwrap_or_default().into_iter().filter(|a| a != "everything").collect();
    let discover = app.state::<Discover>();
    let added = discover.with_store(&app, |s| {
        let which = which.unwrap_or(Which::Weekly);
        if s.requests.iter().filter(|r| r.which() == which).count() >= REQUESTS_MAX {
            return Err(format!("{REQUESTS_MAX} requests at most: remove one first."));
        }
        let id = now_ms().max(s.requests.iter().map(|r| r.id + 1).max().unwrap_or(0));
        s.requests.push(Request { id, text, song, aspects, permanent: false, list: if which == Which::Rediscover { "rediscover".into() } else { String::new() } });
        Ok(())
    });
    added?;
    discover.save(&app);
    Ok(status(&app))
}

#[tauri::command]
pub fn wrapped_discover_request_remove(app: AppHandle, id: i64) -> DiscoverStatus {
    let discover = app.state::<Discover>();
    discover.with_store(&app, |s| s.requests.retain(|r| r.id != id));
    discover.save(&app);
    status(&app)
}

/// Keeps a request for every list (`permanent`), or just the next one.
#[tauri::command]
pub fn wrapped_discover_request_keep(app: AppHandle, id: i64, permanent: bool) -> DiscoverStatus {
    let discover = app.state::<Discover>();
    discover.with_store(&app, |s| {
        if let Some(r) = s.requests.iter_mut().find(|r| r.id == id) {
            r.permanent = permanent;
        }
    });
    discover.save(&app);
    status(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[(&str, u32)]) -> Vec<Tag> {
        list.iter().map(|(n, w)| Tag { name: n.to_string(), weight: *w, kind: TagKind::Genre }).collect()
    }

    #[test]
    fn names_fold() {
        assert_eq!(fold("Mgła"), "mgla");
        assert_eq!(fold("Bølzer"), fold("Bölzer"));
        assert_eq!(fold("Blut Aus Nord"), fold("Blut aus Nord"));
        assert_eq!(fold("Urðr"), "urdr");
        assert_eq!(fold("Dødheimsgard"), "dodheimsgard");
    }

    #[test]
    fn list_songs_count_only_from_the_list() {
        let song = |artist: &str, title: &str| ListSong { artist: artist.into(), title: title.into(), ..Default::default() };
        let rediscover = vec![song("Abscession", "Rat King Crawl"), song("Grave", "Into the Grave")];
        let weekly = vec![song("Portal", "Curtain"), song("Krallice", "Inhume")];
        let lists = || [rediscover.as_slice(), weekly.as_slice()].into_iter();
        let finished = |before: Option<(&str, &str)>, after: (&str, &str)| Finished {
            artist: "Abscession".into(),
            title: "Rat King Crawl".into(),
            at: 0,
            heard: 10.0,
            before: before.map(|(a, t)| (a.into(), t.into())),
            after: (after.0.into(), after.1.into()),
        };
        // (a Deep Dive: Portal before it, Ulcerate after; Portal is on another list)
        assert!(!played_from_list(lists(), &finished(Some(("Portal", "Curtain")), ("Ulcerate", "Abrogation"))));
        // (the list itself, either neighbour)
        assert!(played_from_list(lists(), &finished(None, ("Grave", "Into the Grave"))));
        assert!(played_from_list(lists(), &finished(Some(("Grave / Someone", "Into the Grave")), ("Ulcerate", "Abrogation"))));
    }

    #[test]
    fn requests_in_words() {
        let song = Some(RequestSong { title: "Hero".into(), artist: "Bolzer".into(), album: "Hero".into() });
        let r = Request { song: song.clone(), aspects: vec!["guitars".into(), "vocals".into()], ..Default::default() };
        assert_eq!(describe(&r, true), "the guitars and the vocals of \"Hero\" by Bolzer (from Hero)");
        assert_eq!(describe(&r, false), "the guitars and the vocals of \"Hero\" by Bolzer");
        let r = Request { song, text: "but rawer".into(), ..Default::default() };
        assert_eq!(describe(&r, false), "songs like \"Hero\" by Bolzer: but rawer");
        let r = Request { text: "  90s melodic death with female vocals ".into(), ..Default::default() };
        assert_eq!(describe(&r, true), "90s melodic death with female vocals");
        assert_eq!(and_list(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }

    #[test]
    fn defining_tags() {
        let amon = tags(&[("melodic death metal", 100), ("viking metal", 70), ("viking", 50), ("death metal", 37), ("metal", 8)]);
        let mercenary = tags(&[("melodic death metal", 100), ("power metal", 43), ("progressive metal", 24), ("death metal", 11)]);
        // (power metal at 43 is a side of Mercenary, not what defines it)
        assert_eq!(foreign_defining_tag(&amon, &mercenary), None);
        let power = tags(&[("melodic death metal", 100), ("power metal", 70)]);
        assert_eq!(foreign_defining_tag(&amon, &power).as_deref(), Some("power metal"));
        let deftones = tags(&[("nu metal", 100), ("alternative", 73), ("shoegaze", 50), ("alternative metal", 17)]);
        let limp = tags(&[("nu metal", 100), ("rapcore", 80), ("alternative", 33)]);
        assert_eq!(foreign_defining_tag(&deftones, &limp).as_deref(), Some("rapcore"));
        // (a narrower form of the seed's tag is fine; a band name isn't a style)
        let morbid = tags(&[("death metal", 100), ("brutal death metal", 30)]);
        let cryptopsy = tags(&[("technical death metal", 100), ("brutal death metal", 80)]);
        assert_eq!(foreign_defining_tag(&morbid, &cryptopsy), None);
        // ("speed/power-metal" is power metal)
        let iced = tags(&[("power metal", 100), ("thrash metal", 33), ("heavy metal", 21)]);
        let primal = tags(&[("power metal", 100), ("speed/power-metal", 60), ("heavy metal", 50)]);
        assert_eq!(foreign_defining_tag(&iced, &primal), None);
        let paradise = tags(&[("gothic metal", 100), ("doom metal", 80)]);
        let theatre = tags(&[("gothic metal", 100), ("lacrimosa", 50), ("noruega", 45)]);
        assert_eq!(foreign_defining_tag(&paradise, &theatre), None);
    }

    #[test]
    fn rounds_fill_the_list() {
        let is = |n: Next, want: &str| matches!((n, want), (Next::Tier1, "tier1") | (Next::Heavy, "heavy") | (Next::Done, "done"));
        // short after round 1: tier 1 again, Light or Heavy
        assert!(is(next_step(23, false, 1, true, false, 0), "tier1"));
        assert!(is(next_step(23, false, 1, true, true, 0), "tier1"));
        // full, or out of rounds: Light is done, Heavy asks tier 2 once
        assert!(is(next_step(31, false, 1, true, false, 0), "done"));
        assert!(is(next_step(31, false, 1, true, true, 0), "heavy"));
        assert!(is(next_step(25, false, 3, true, true, 0), "heavy"));
        // after tier 2's own picks: again only while short, at most twice
        assert!(is(next_step(25, true, 4, false, true, 1), "heavy"));
        assert!(is(next_step(31, true, 4, false, true, 1), "done"));
        assert!(is(next_step(25, true, 5, false, true, 2), "done"));
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_745), (2026, 10, 19));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2026-10-19 05:00 UTC is still Oct 19 in Chicago (UTC-5) at midnight
        let ms = 20_745 * DAY_MS + 5 * 3_600_000;
        assert_eq!(short_date(ms, -300), "Oct 19");
        assert_eq!(short_date(ms - 3_600_000, -300), "Oct 18");
    }

    #[test]
    fn sound() {
        use crate::tags::TagKind;
        let tags = |list: &[(&str, u32)]| -> Vec<Tag> { list.iter().map(|(n, w)| Tag { name: n.to_string(), weight: *w, kind: TagKind::Genre }).collect() };
        let amon = tags(&[("melodic death metal", 100), ("viking metal", 70), ("death metal", 37), ("metal", 8)]);
        let halo = tags(&[("melodic death metal", 100), ("death metal", 16), ("metal", 13)]);
        let hsb = tags(&[("metalcore", 100), ("melodic death metal", 61), ("deathcore", 22), ("death metal", 19), ("hardcore", 8)]);
        assert!(sound_match(&amon, &halo) > MIN_SOUND);
        assert!(sound_match(&amon, &hsb) < MIN_SOUND);
        assert!((sound_match(&amon, &amon) - 1.0).abs() < 1e-9);
        assert_eq!(sound_match(&amon, &[]), 0.0);
    }

    #[test]
    fn collaborations() {
        assert_eq!(name_parts("neurosis & jarboe"), vec!["neurosis", "jarboe"]);
        assert_eq!(name_parts("perturbator / health"), vec!["perturbator", "health"]);
        assert!(name_parts("amon amarth").is_empty());
    }

    #[test]
    fn jitter_in_range() {
        let mut j = Jitter::new();
        for _ in 0..1000 {
            let x = j.next();
            assert!((0.0..1.0).contains(&x));
        }
    }
}
