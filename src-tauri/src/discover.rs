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

/// What's on each zone, for telling a list's song heard from one skipped:
/// zone id -> (title, artist, album, when it came up, furthest position).
static ON_ZONES: Mutex<Option<HashMap<String, (String, String, String, i64, f64)>>> = Mutex::new(None);

/// zones.rs, after every zone update: a zone that moved on to another song
/// is done with the one before; if that was a list's song, it's noted with
/// how much of it was heard.
pub fn zones_updated(app: &AppHandle, payload: &crate::zones::ZonesPayload) {
    if !enabled(app) {
        return;
    }
    let mut done: Vec<(String, String, i64, f64)> = Vec::new();
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
            match zones.get_mut(&z.zone_id) {
                Some(cur) if cur.0 == np.title && cur.1 == artist && cur.2 == np.album => cur.4 = cur.4.max(pos),
                Some(cur) => {
                    done.push((cur.1.clone(), cur.0.clone(), cur.3, cur.4));
                    *cur = (np.title.clone(), artist, np.album.clone(), now_ms(), pos);
                }
                None => {
                    zones.insert(z.zone_id.clone(), (np.title.clone(), artist, np.album.clone(), now_ms(), pos));
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
        cur.4 = cur.4.max(pos);
    }
}

/// Keeps the finished songs that are on a list (by title, and the artist
/// one way or the other: "A / B" credits).
fn note_shown(app: &AppHandle, done: Vec<(String, String, i64, f64)>) {
    let discover = app.state::<Discover>();
    let noted = discover.with_store(app, |s| {
        let mut noted = Vec::new();
        for (artist, title, at, heard) in done {
            let (a, t) = (artist.to_lowercase(), title.to_lowercase());
            let on_list = s.weekly.iter().chain(s.rediscover.iter()).flat_map(|b| b.tracks.iter()).any(|song| {
                let sa = song.artist.to_lowercase();
                song.title.to_lowercase() == t && (sa == a || sa.contains(&a) || a.contains(&sa))
            });
            if on_list {
                noted.push(format!("{artist} - {title}: {heard:.0} s"));
                s.shown.push(Shown { artist, title, at, heard });
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
struct Heard {
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

/// A seed for tier 2: its view, Roon's reviews of the albums you played,
/// and its bio.
async fn seed_brief(client: &LibraryClient, found: &Neighbours, views: &[SeedView], handles: &[Option<String>], i: usize) -> crate::judge::SeedBrief {
    let Some((heard, _)) = found.seeds.get(i) else { return crate::judge::SeedBrief::default() };
    let view = views.get(i).cloned().unwrap_or_default();
    let mut albums = Vec::new();
    let mut played: Vec<&PlayedAlbum> = heard.albums.values().collect();
    played.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    for album in played.iter().take(SEED_ALBUMS) {
        let (year, review) = match album.album_id {
            Some(id) => match client.album_by_id(id).await {
                Ok(card) => (card.year, client.album_review(&card.handle).await.ok().flatten().unwrap_or_default()),
                Err(_) => (None, String::new()),
            },
            None => (None, String::new()),
        };
        albums.push((album.name.clone(), year, review));
    }
    let bio = match handles.get(i).cloned().flatten() {
        Some(h) => client.artist_bio(&h).await.ok().flatten().unwrap_or_default(),
        None => String::new(),
    };
    crate::judge::SeedBrief { name: heard.name.clone(), tags: view.tags, era: view.era, albums, bio }
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
    avoid: &[Avoid],
    jitter: &mut Jitter,
    progress: &impl Fn(String),
) -> Vec<(String, String, Vec<usize>)> {
    let tags = app.state::<Tags>();
    let seed_tags: Vec<Vec<Tag>> = (0..n.seeds.len()).map(|i| views.get(i).map(|v| v.tags.clone()).unwrap_or_default()).collect();
    // Each seed's candidates, scored
    let mut queues: Vec<Vec<(f64, String, String)>> = Vec::new();
    let mut checked = 0;
    let mut considered: HashSet<String> = HashSet::new();
    for (i, list) in n.lists.iter().enumerate() {
        let mut scored = Vec::new();
        for (k, name, m) in list.iter().filter(|(k, _, m)| *m >= MIN_MATCH && eligible(k)).take(PER_SEED) {
            checked += 1;
            progress(format!("Checking how {name} sounds ({checked} checked)..."));
            let theirs = tags.lookup_quick(app, name, key).await.tags;
            if let Some(like) = avoid.iter().find(|a| sound_match(&a.tags, &theirs) >= a.limit) {
                if considered.insert(k.clone()) {
                    note(format!("{name} sounds too much like {}, which you {}", like.name, like.why));
                }
                continue;
            }
            let sound = if seed_tags[i].is_empty() { MIN_SOUND } else { sound_match(&seed_tags[i], &theirs) };
            if !shares_telling_tag(&seed_tags[i], &theirs) {
                if considered.insert(k.clone()) {
                    note(format!("{name} shares nothing telling with {} (only umbrella tags)", n.seeds[i].0.name));
                }
                continue;
            }
            if let Some(foreign) = foreign_defining_tag(&seed_tags[i], &theirs) {
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
            let score = (m * 1.5 + sound * 0.5) * (1.0 + 0.1 * (shared - 1.0)) * (0.92 + 0.16 * jitter.next());
            if score < MIN_SCORE_LOW {
                if considered.insert(k.clone()) {
                    note(format!("{name} is only the fringe of {} (score {score:.2}, match {m:.2})", n.seeds[i].0.name));
                }
                continue;
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
            if given[i] >= PER_SEED_MAX {
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
    let seed_names: Vec<String> = seed.iter().filter(counts).map(|t| format!(" {} ", t.name.to_lowercase().replace('-', " "))).collect();
    if seed_names.is_empty() {
        return None;
    }
    let top = theirs.iter().filter(counts).map(|t| t.weight).max()?;
    theirs
        .iter()
        .filter(counts)
        .filter(|t| !GENERIC.contains(&t.name.as_str()) && t.weight as f64 >= top as f64 * DEFINING && names_a_style(&t.name))
        .find(|t| {
            let mine = format!(" {} ", t.name.to_lowercase().replace('-', " "));
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
async fn choose_by_album(
    app: &AppHandle,
    key: &str,
    candidates: &[Vec<roon_library::TopTrack>],
    names: &[String],
    refs: &[Vec<Tag>],
    eras: &[Option<(u32, u32)>],
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
            out.avoid.push(Avoid { name: h.name.clone(), tags: tags.lookup_quick(app, &h.name, key).await.tags, limit: 0.9, why: "skipped" });
        }
    }
    liked.sort_by(|a, b| b.2.cmp(&a.2));
    out.liked = liked.into_iter().take(6).map(|(h, w, _)| (h, w)).collect();
    out.avoid.retain(|a| !a.tags.is_empty());
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
async fn genres_of(client: &LibraryClient, cache: &mut HashMap<String, Vec<String>>, handle: &str) -> Vec<String> {
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
async fn find_by_name(client: &LibraryClient, cache: &mut HashMap<String, Vec<String>>, name: &str, like: &HashSet<String>) -> Option<String> {
    let hits = client.search(name, 10).await.ok()?;
    let wanted = name.to_lowercase();
    let mut unknown = None;
    for hit in hits.artists.into_iter().filter(|a| a.name.to_lowercase() == wanted) {
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
}

/// The picked artists' songs in play order, with whose each one is and why
/// it's there.
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
    Picked { artists, songs, song_artist, song_why: HashMap::new() }
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

async fn pick_weekly(app: &AppHandle, client: &LibraryClient, key: &str, recently: &HashSet<String>, said: &Feedback, ctx: &Context, progress: &impl Fn(String)) -> Result<Picked, String> {
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
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1));
    progress("Reading your library...".into());
    let no_progress = |_: usize, _: usize| {};
    let library: HashSet<String> = match client.library_artists(&no_progress).await {
        Ok(cards) => cards.into_iter().map(|c| c.name.to_lowercase()).collect(),
        Err(_) => app.state::<LibraryCopy>().names(app),
    };
    let found = neighbours(app, seeds, SEEDS_DEEP + said.liked.len(), key, progress).await;
    note(format!("weekly seeds: {}", found.seeds.iter().map(|(h, w)| format!("{} {w:.1}", h.name)).collect::<Vec<_>>().join(", ")));
    let views = seed_views(app, client, key, &found.seeds, progress).await;
    let mut jitter = Jitter::new();
    // Never played, not in the library, not in the last two lists, and not
    // a collaboration of an artist you know ("Neurosis & Jarboe")
    let known = |k: &str| heard.contains_key(k) || library.contains(k);
    // (a few spares: some artists aren't in Roon or have no songs of their
    // own it can play)
    let ranked = spread(
        app,
        &found,
        &views,
        |k| !known(k) && !recently.contains(k) && !said.down.contains(k) && k != "various artists" && !name_parts(k).iter().any(|part| known(part)),
        key,
        SONGS + 14,
        &said.avoid,
        &mut jitter,
        progress,
    )
    .await;

    progress("Finding the seeds in Roon...".into());
    let seed_handles = seed_handles(client, &found.seeds).await;
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    // (a few spares: some artists have no songs of their own Roon can play)
    let want = SONGS + 8;
    let mut names = Vec::new();
    let mut reasons = Vec::new();
    let mut handles = Vec::new();
    let mut ranked_seed_tags: Vec<Vec<Tag>> = Vec::new();
    let mut ranked_seed_eras: Vec<Option<(u32, u32)>> = Vec::new();
    let mut eras: HashMap<String, Option<(u32, u32)>> = HashMap::new();
    for (n, (_, name, from)) in ranked.iter().enumerate() {
        if handles.len() >= want || n >= want * 3 {
            break;
        }
        progress(format!("Finding artists in Roon... {} of {want}", handles.len()));
        // The genres of the two seeds it's closest to
        let mut like = HashSet::new();
        for seed in from.iter().take(2) {
            if let Some(Some(h)) = seed_handles.get(*seed) {
                like.extend(genres_of(client, &mut cache, h).await);
            }
        }
        match find_by_name(client, &mut cache, name, &like).await {
            Some(handle) => {
                names.push(name.clone());
                reasons.push(because(&found, &views, from));
                handles.push(handle);
                let view = from.first().and_then(|i| views.get(*i));
                ranked_seed_tags.push(view.map(|v| v.tags.clone()).unwrap_or_default());
                // (the era you played the seed in; the band's own when the
                // albums you played have no year)
                let seed_handle = from.first().and_then(|i| seed_handles.get(*i).cloned().flatten());
                ranked_seed_eras.push(match (view.and_then(|v| v.era), seed_handle) {
                    (Some(era), _) => Some(era),
                    (None, Some(h)) => era_of(client, &mut eras, &h).await,
                    (None, None) => None,
                });
            }
            None => note(format!("left out {name}: not in Roon, or no genre shared with its seeds")),
        }
    }
    progress("Picking songs...".into());
    // Several songs each (from different albums), the one from the album
    // that sounds most like the seed kept: a band's sound moves over a
    // career (Klone's groove years next to Soen), and the artist's tags
    // cover all of it
    let options = MixOptions { per_artist: 6, include_live: false, library_only: false, own_albums: true, from_top: 10, never: said.played_songs.clone() };
    let (candidates, _) = client.pick_artist_tracks(&handles, &options).await;
    let mut picks = choose_by_album(app, key, &candidates, &names, &ranked_seed_tags, &ranked_seed_eras, progress).await;
    let with_songs = picks.iter().filter(|p| !p.is_empty()).count();
    note(format!("{with_songs} of {} artists found in Roon have a song of their own to pick", handles.len()));
    // Tier 2: the language model reads up on each pick against the seed
    // as you play it, drops the ones that don't fit, and may choose the
    // song (light mode: tier 1's order stands)
    let mut song_why: HashMap<String, String> = HashMap::new();
    if let Some(llm) = crate::judge::Llm::from_settings(&WrappedSettings::load(app)) {
        progress("Reading up on the picks...".into());
        let judged: Vec<usize> = (0..picks.len()).filter(|i| !picks[*i].is_empty()).take(SONGS + 6).collect();
        let mut pairs = Vec::new();
        let mut seed_briefs: HashMap<usize, crate::judge::SeedBrief> = HashMap::new();
        for &i in &judged {
            let seed_i = ranked.get(i).and_then(|(_, _, from)| from.first().copied()).unwrap_or(0);
            if !seed_briefs.contains_key(&seed_i) {
                let brief = seed_brief(client, &found, &views, &seed_handles, seed_i).await;
                seed_briefs.insert(seed_i, brief);
            }
            let cand = crate::judge::candidate_brief(app, client, key, &names[i], &handles[i], &candidates[i]).await;
            pairs.push((seed_briefs[&seed_i].clone(), cand));
        }
        let verdicts = crate::judge::judge_all(app, &llm, pairs, progress).await;
        let (mut kept, mut dropped, mut failed) = (0, 0, 0);
        for (n, &i) in judged.iter().enumerate() {
            match &verdicts[n] {
                Ok(v) if !v.fit => {
                    dropped += 1;
                    note(format!("tier 2 dropped {}: {} ({:.0}%)", names[i], if v.note.is_empty() { v.why.clone() } else { v.note.clone() }, v.confidence * 100.0));
                    picks[i].clear();
                }
                Ok(v) => {
                    kept += 1;
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
                }
                Err(e) => {
                    failed += 1;
                    note(format!("tier 2 couldn't judge {}: {e}", names[i]));
                }
            }
        }
        note(format!("tier 2 ({} {}): {kept} kept, {dropped} dropped, {failed} not judged", llm.provider, llm.model));
    }
    let chosen: Vec<usize> = (0..picks.len()).filter(|i| !picks[*i].is_empty()).take(SONGS).collect();
    let mut out = picked(&chosen, &picks, &names, &reasons);
    out.song_why = song_why;
    Ok(out)
}

async fn pick_rediscover(app: &AppHandle, client: &LibraryClient, key: &str, recently: &HashSet<String>, said: &Feedback, ctx: &Context, progress: &impl Fn(String)) -> Result<Picked, String> {
    progress("Reading your listening and library...".into());
    let now = now_ms();
    let heard = history_by_artist(app, now, ctx);
    let library = app.state::<LibraryCopy>().artists(app);
    if library.is_empty() {
        return Err("Wrapped has no copy of your library's artists yet (Wrapped's Settings reads it).".into());
    }
    // The candidates: never played, or not for a year (and not mostly
    // skipped when they were)
    let mut never: HashMap<String, (String, u64)> = HashMap::new();
    let mut old: HashMap<String, (String, u64)> = HashMap::new();
    for artist in &library {
        let k = artist.name.to_lowercase();
        let Some(id) = artist.performer_id else { continue };
        if recently.contains(&k) || said.down.contains(&k) || k == "various artists" {
            continue;
        }
        match heard.get(&k) {
            None => {
                never.insert(k, (artist.name.clone(), id));
            }
            Some(h) if now - h.last_played >= 365 * DAY_MS && h.listens * 2 >= h.plays => {
                old.insert(k, (artist.name.clone(), id));
            }
            Some(_) => {}
        }
    }
    // Their order: the ones that sound like what's played lately first,
    // taken from each seed's neighbours in turns (with a Last.fm key), then
    // the rest by how close their tags are to the sound of the seeds
    // together, with a little shuffle so the order changes each week
    let mut jitter = Jitter::new();
    let mut seeds = seeds(&heard);
    give_credit(&mut seeds, &said.credit);
    let found = if key.is_empty() || seeds.is_empty() { None } else { Some(neighbours(app, seeds.clone(), SEEDS_DEEP, key, progress).await) };
    let views = match &found {
        Some(found) => seed_views(app, client, key, &found.seeds, progress).await,
        None => Vec::new(),
    };
    let profile = taste(app, &seeds);
    let tags = app.state::<Tags>();
    let mut ordered: Vec<Vec<(String, u64, (String, Vec<String>))>> = Vec::new();
    for (pool, want, where_from) in [(&never, SONGS + 10, "In your library, never played"), (&old, SONGS / 4 + 8, "In your library, last played over a year ago")] {
        let mut out: Vec<(String, u64, (String, Vec<String>))> = Vec::new();
        let mut placed: HashSet<String> = HashSet::new();
        if let Some(found) = &found {
            for (k, _, from) in spread(app, found, &views, |k| pool.contains_key(k), key, want, &said.avoid, &mut jitter, progress).await {
                placed.insert(k.clone());
                let (name, id) = pool[&k].clone();
                let (why, of) = because(found, &views, &from);
                out.push((name, id, (format!("{where_from}. {why}"), of)));
            }
        }
        let mut rest: Vec<(f64, (String, u64))> = pool
            .iter()
            .filter(|(k, _)| !placed.contains(*k))
            .map(|(k, v)| {
                let near = tags.cached(app, k).map(|t| sound_match(&profile, &t.tags)).unwrap_or(0.0);
                (near + 0.15 * jitter.next(), v.clone())
            })
            .collect();
        rest.sort_by(|a, b| b.0.total_cmp(&a.0));
        out.extend(rest.into_iter().map(|(_, (name, id))| (name, id, (format!("{where_from}. Close to the sound of what you've been playing"), Vec::new()))));
        ordered.push(out);
    }
    let old = ordered.pop().unwrap_or_default();
    let never = ordered.pop().unwrap_or_default();
    note(format!("rediscover pool: {} never played, {} not for a year", never.len(), old.len()));

    // A quarter from the year-old ones, more when the never-played run short
    let want_old = (SONGS / 4).max(SONGS.saturating_sub(never.len())).min(old.len());
    let want_never = SONGS.saturating_sub(want_old).min(never.len());
    // (spares: an artist's top songs may not be in the library)
    let pool: Vec<(String, u64, bool, (String, Vec<String>))> = never
        .iter()
        .take(want_never + 10)
        .map(|(n, id, why)| (n.clone(), *id, false, why.clone()))
        .chain(old.iter().take(want_old + 6).map(|(n, id, why)| (n.clone(), *id, true, why.clone())))
        .collect();
    let mut names = Vec::new();
    let mut reasons = Vec::new();
    let mut handles = Vec::new();
    let mut is_old = Vec::new();
    for (n, (name, id, was_old, why)) in pool.iter().enumerate() {
        progress(format!("Finding artists in Roon... {} of {}", n + 1, pool.len()));
        if let Ok(card) = client.artist_by_id(*id).await {
            names.push(name.clone());
            reasons.push(why.clone());
            handles.push(card.handle);
            is_old.push(*was_old);
        }
    }
    progress("Picking songs...".into());
    let options = MixOptions { per_artist: 6, include_live: false, library_only: true, own_albums: true, from_top: 10, never: said.played_songs.clone() };
    let (candidates, _) = client.pick_artist_tracks(&handles, &options).await;
    // (the seed as you play it, when a seed is named; the whole taste else)
    let view_of = |name: &str| -> Option<Vec<Tag>> {
        let found = found.as_ref()?;
        let i = found.seeds.iter().position(|(h, _)| h.name == name)?;
        views.get(i).map(|v| v.tags.clone())
    };
    let refs: Vec<Vec<Tag>> = reasons
        .iter()
        .map(|(_, from)| from.first().and_then(|n| view_of(n)).unwrap_or_else(|| profile.clone()))
        .collect();
    let picks = choose_by_album(app, key, &candidates, &names, &refs, &[], progress).await;
    let usable = |old: bool| -> Vec<usize> { (0..picks.len()).filter(|i| is_old[*i] == old && !picks[*i].is_empty()).collect() };
    let (never_ok, old_ok) = (usable(false), usable(true));
    // The quarter from the old ones; whichever side runs short, the other fills
    let take_old = want_old.min(old_ok.len()).max(SONGS.saturating_sub(never_ok.len())).min(old_ok.len());
    let take_never = SONGS.saturating_sub(take_old).min(never_ok.len());
    let mut chosen: Vec<usize> = never_ok.iter().take(take_never).chain(old_ok.iter().take(take_old)).copied().collect();
    // (never-played first in the priority, but mixed in the play order)
    chosen.sort_by_key(|i| *i);
    note(format!("rediscover: {take_never} never played and {take_old} not for a year"));
    Ok(picked(&chosen, &picks, &names, &reasons))
}

/// Saves the new playlist and moves the older ones along: the previous
/// current one gets its date, anything beyond two dated copies goes.
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
    };
    let (weekly, rediscover) = discover.with_store(app, |s| (s.weekly.iter().map(card).collect(), s.rediscover.iter().map(card).collect()));
    let building = *discover.building.lock().unwrap();
    let failed = discover.failed.lock().unwrap().clone();
    DiscoverStatus { weekly, rediscover, building, weekly_error: failed.get("weekly").cloned(), rediscover_error: failed.get("rediscover").cloned() }
}

/// A build number for the title bar, bumped by hand with each push: it
/// comes from Rust so it only changes when the app was really rebuilt
/// (page files reload on their own in a dev build).
pub const BUILD: u32 = 1018;

#[tauri::command]
pub fn wrapper_build() -> u32 {
    BUILD
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
    let picked = match which {
        Which::Weekly => pick_weekly(app, &client, &key, &recently, &said, &ctx, &progress).await?,
        Which::Rediscover => pick_rediscover(app, &client, &key, &recently, &said, &ctx, &progress).await?,
    };
    if picked.songs.is_empty() {
        return Err("Roon found no songs for the artists picked.".into());
    }
    progress("Saving the playlist...".into());
    let (songs, artists) = (picked.songs.len(), picked.artists.len());
    publish(app, &client, which, picked, offset).await?;
    note(format!("{}: {songs} songs from {artists} artists in {} s", which.name(), started.elapsed().as_secs()));
    Ok(())
}

/// A song from one of the lists, for the player's thumbs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverSong {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[(&str, u32)]) -> Vec<Tag> {
        list.iter().map(|(n, w)| Tag { name: n.to_string(), weight: *w, kind: TagKind::Genre }).collect()
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
        let paradise = tags(&[("gothic metal", 100), ("doom metal", 80)]);
        let theatre = tags(&[("gothic metal", 100), ("lacrimosa", 50), ("noruega", 45)]);
        assert_eq!(foreign_defining_tag(&paradise, &theatre), None);
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
