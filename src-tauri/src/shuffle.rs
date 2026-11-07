// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Shuffle library (the Tracks page): a queue of AHEAD songs from the
//! library, kept that long as you listen, instead of Roon's thousands at
//! once. Each top-up picks songs not played lately (Today by default: the
//! local day; or the last N days; or off), never one this shuffle queued
//! already, and no artist of the last ARTIST_GAP songs. Songs you add to the
//! queue yourself count toward the AHEAD, so the shuffle waits until the
//! queue is under it again. It ends when Stop is pressed or the zone plays
//! something with none of the shuffle's songs coming.
//!
//! It's saved as it goes (`wrapped-shuffle.json`: the options, the songs it
//! queued, the last artists), so after Toasted restarts (`resume`) it carries
//! on, if that zone's queue still has its songs coming.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use roon_library::{LibraryClient, LibraryTrack, PlayHow};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::discover::fold;
use crate::library::Library;
use crate::wrapped::now_ms;

/// Songs kept ahead of the playing one.
const AHEAD: usize = 50;
/// No artist again within this many songs.
const ARTIST_GAP: usize = 50;
/// How often the zone is looked at.
const TICK: Duration = Duration::from_secs(3);
/// The queue is looked at on every song change, and at least this often
/// (for songs you add or take out yourself).
const LOOK_EVERY: Duration = Duration::from_secs(20);
/// Roon's history is read again (new plays) at most this often.
const HISTORY_EVERY: Duration = Duration::from_secs(10 * 60);
/// Roon lookups at once (track handles by id).
const AT_ONCE: usize = 8;
/// Reads of the queue in a row with none of the shuffle's songs coming
/// before it counts as the zone having moved on. (A read can come back
/// before Roon has sent the queue's songs, most of all just after the
/// library link comes up; a read with no songs at all isn't counted.)
const MISSES_TO_END: usize = 3;

/// How the Tracks page starts a shuffle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShuffleOptions {
    pub zone_id: String,
    /// Only hearted tracks (the page's hearts switch).
    #[serde(default)]
    pub hearts_only: bool,
    /// Songs played since then are left out: "today", "3", "7", "30",
    /// "90" (days) or "off".
    #[serde(default)]
    pub recent: String,
    /// The page's getTimezoneOffset (for "today").
    #[serde(default)]
    pub tz_offset_min: i32,
}

/// What the page shows ("wrapped-shuffle" events).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShuffleStatus {
    pub running: bool,
    pub zone_id: String,
    pub hearts_only: bool,
    pub recent: String,
    /// Songs the shuffle has queued so far.
    pub added: usize,
    /// Why it ended, if it did on its own.
    pub ended: String,
}

struct Run {
    options: ShuffleOptions,
    /// Track ids this shuffle queued (no song twice).
    used: HashSet<u64>,
    /// Folded artists of the last ARTIST_GAP songs queued.
    artists: VecDeque<String>,
    /// Ids of every song queued (TrackId; the queue may show either id).
    ours: HashSet<u64>,
    added: usize,
    /// The zone's playing song when last looked.
    playing: Option<String>,
    looked: Instant,
    history_read: Instant,
    /// Songs played lately (track ids, and folded "artist|title").
    recent_ids: HashSet<u64>,
    recent_names: HashSet<String>,
    rng: u64,
}

/// What's saved of a run, to carry on after a restart.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    options: ShuffleOptions,
    used: Vec<u64>,
    artists: Vec<String>,
    ours: Vec<u64>,
    added: usize,
}

fn saved_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("wrapped-shuffle.json"))
}

/// Saves the running shuffle (or, with none running, removes the file).
fn save(app: &AppHandle) {
    let Some(path) = saved_path(app) else { return };
    let saved = RUN.lock().unwrap().as_ref().map(|r| Saved {
        options: r.options.clone(),
        used: r.used.iter().copied().collect(),
        artists: r.artists.iter().cloned().collect(),
        ours: r.ours.iter().copied().collect(),
        added: r.added,
    });
    match saved {
        Some(saved) => {
            if let Ok(json) = serde_json::to_string(&saved) {
                let _ = std::fs::write(&path, json);
            }
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// At start (lib.rs setup): a shuffle that was running when Toasted closed
/// carries on, once the library link is up, if its zone's queue still has
/// its songs coming; otherwise it's dropped.
pub fn resume(app: &AppHandle) {
    let Some(path) = saved_path(app) else { return };
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(saved) = serde_json::from_str::<Saved>(&text) else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // The library link and the zone (up to about 3 minutes)
        let mut client = None;
        for _ in 0..90 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if RUN.lock().unwrap().is_some() {
                return; // a new shuffle was started meanwhile
            }
            if let Some(c) = app.state::<Library>().client().filter(|c| c.zone_ids().contains(&saved.options.zone_id)) {
                client = Some(c);
                break;
            }
        }
        let Some(client) = client else {
            println!("[shuffle] the saved shuffle's zone isn't there yet; left for next time");
            return;
        };
        // Its songs still coming? Read the queue until it's clear (a read
        // with no songs means Roon hasn't sent them yet: asked again)
        let ours: HashSet<u64> = saved.ours.iter().copied().collect();
        let (mut found, mut misses) = (false, 0);
        for _ in 0..12 {
            if RUN.lock().unwrap().is_some() {
                return;
            }
            if let Ok(queue) = client.queue_tracks(&saved.options.zone_id).await {
                if queue.ahead_ids.iter().any(|id| ours.contains(id)) {
                    found = true;
                    break;
                }
                if !queue.ahead_ids.is_empty() {
                    misses += 1;
                    if misses >= MISSES_TO_END {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        if !found {
            if misses >= MISSES_TO_END {
                println!("[shuffle] the saved shuffle's zone moved on; dropped");
                let _ = std::fs::remove_file(&path);
            } else {
                println!("[shuffle] couldn't tell whether the saved shuffle's songs are still coming; left for next time");
            }
            return;
        }
        let mut run = Run {
            options: saved.options,
            used: saved.used.into_iter().collect(),
            artists: saved.artists.into_iter().collect(),
            ours,
            added: saved.added,
            playing: None,
            looked: Instant::now(),
            history_read: Instant::now(),
            recent_ids: HashSet::new(),
            recent_names: HashSet::new(),
            rng: (now_ms() as u64) ^ 0x9E37_79B9_7F4A_7C15 | 1,
        };
        let _ = crate::wrapped::history_catch_up(&app).await;
        read_recent(&app, &mut run);
        let generation = {
            let mut guard = RUN.lock().unwrap();
            if guard.is_some() {
                return;
            }
            println!("[shuffle] carried on after a restart on {}: {} songs so far", run.options.zone_id, run.added);
            *guard = Some(run);
            GENERATION.fetch_add(1, Ordering::SeqCst) + 1
        };
        ENDED.lock().unwrap().clear();
        emit(&app);
        watch(app, generation).await;
    });
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);
static ENDED: Mutex<String> = Mutex::new(String::new());
/// Bumped on every start and stop; a watcher whose number is old ends.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn status() -> ShuffleStatus {
    let run = RUN.lock().unwrap();
    match run.as_ref() {
        Some(r) => ShuffleStatus {
            running: true,
            zone_id: r.options.zone_id.clone(),
            hearts_only: r.options.hearts_only,
            recent: r.options.recent.clone(),
            added: r.added,
            ended: String::new(),
        },
        None => ShuffleStatus { ended: ENDED.lock().unwrap().clone(), ..Default::default() },
    }
}

fn emit(app: &AppHandle) {
    let _ = app.emit("wrapped-shuffle", status());
}

fn end(app: &AppHandle, why: &str) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    let was = RUN.lock().unwrap().take();
    if let Some(r) = was {
        println!("[shuffle] ended ({why}) after {} songs", r.added);
    }
    save(app); // (none running: the saved one goes)
    *ENDED.lock().unwrap() = why.to_string();
    emit(app);
}

#[tauri::command]
pub fn wrapped_shuffle_status() -> ShuffleStatus {
    status()
}

#[tauri::command]
pub fn wrapped_shuffle_stop(app: AppHandle) -> ShuffleStatus {
    end(&app, "");
    status()
}

/// Starts a shuffle on the zone: AHEAD songs as a fresh queue, then kept
/// topped up (see the module's notes).
#[tauri::command]
pub async fn wrapped_shuffle_start(app: AppHandle, options: ShuffleOptions) -> Result<ShuffleStatus, String> {
    let client = app.state::<Library>().client().ok_or("Not connected to the Roon Core's library yet.")?;
    end(&app, "");
    ENDED.lock().unwrap().clear();
    let generation = GENERATION.load(Ordering::SeqCst);
    // The latest plays first, so today's are known
    let _ = crate::wrapped::history_catch_up(&app).await;
    let mut run = Run {
        options: options.clone(),
        used: HashSet::new(),
        artists: VecDeque::new(),
        ours: HashSet::new(),
        added: 0,
        playing: None,
        looked: Instant::now(),
        history_read: Instant::now(),
        recent_ids: HashSet::new(),
        recent_names: HashSet::new(),
        rng: (now_ms() as u64) ^ 0x9E37_79B9_7F4A_7C15 | 1,
    };
    read_recent(&app, &mut run);
    let picks = pick(&app, &mut run, AHEAD);
    if picks.is_empty() {
        return Err(if options.hearts_only { "No hearted tracks to shuffle (or all played lately)." } else { "No tracks to shuffle: read the library first (Settings)." }.into());
    }
    let (handles, ids) = handles_of(&client, &picks).await;
    if handles.is_empty() {
        return Err("Roon didn't find the picked songs.".into());
    }
    run.ours.extend(ids);
    // A fresh queue of its own, like the radio
    let _ = client.queue_clear(&options.zone_id).await;
    client.play_tracks_how(&options.zone_id, &handles, PlayHow::Now).await.map_err(|e| e.to_string())?;
    run.added = handles.len();
    run.looked = Instant::now();
    run.playing = client.track_for_zone(&options.zone_id).and_then(|t| t.track_id);
    println!("[shuffle] started on {}: {} songs{}, recent {}", options.zone_id, handles.len(), if options.hearts_only { " (hearted)" } else { "" }, options.recent);
    if GENERATION.load(Ordering::SeqCst) != generation {
        return Ok(status());
    }
    *RUN.lock().unwrap() = Some(run);
    save(&app);
    emit(&app);
    // The queue's start, for the queue panel's played part
    {
        let (app, client, zone) = (app.clone(), client.clone(), options.zone_id.clone());
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            if let Ok(songs) = client.queue_songs(&zone, None, 1).await {
                if let Some(first) = songs.iter().find(|s| s.current).and_then(|s| s.item_id) {
                    crate::queue::set_start(&app, &zone, first);
                }
            }
        });
    }
    let watcher = app.clone();
    tauri::async_runtime::spawn(async move { watch(watcher, generation).await });
    Ok(status())
}

/// Keeps the queue topped up while this shuffle runs.
async fn watch(app: AppHandle, generation: u64) {
    // Reads in a row with none of the shuffle's songs coming
    let mut misses = 0;
    loop {
        tokio::time::sleep(TICK).await;
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let Some(client) = app.state::<Library>().client() else { continue };
        let (zone, due) = {
            let mut guard = RUN.lock().unwrap();
            let Some(run) = guard.as_mut() else { return };
            let now_playing = client.track_for_zone(&run.options.zone_id).and_then(|t| t.track_id);
            let changed = now_playing != run.playing;
            run.playing = now_playing;
            (run.options.zone_id.clone(), changed || run.looked.elapsed() >= LOOK_EVERY)
        };
        if !due {
            continue;
        }
        let Ok(queue) = client.queue_tracks(&zone).await else { continue };
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let refresh_history = RUN.lock().unwrap().as_ref().is_some_and(|r| r.history_read.elapsed() >= HISTORY_EVERY);
        if refresh_history {
            let _ = crate::wrapped::history_catch_up(&app).await;
        }
        let want = {
            let mut guard = RUN.lock().unwrap();
            let Some(run) = guard.as_mut() else { return };
            run.looked = Instant::now();
            if refresh_history {
                read_recent(&app, run);
            }
            // No songs in the read: Roon hasn't sent them yet (not counted)
            if queue.ahead_ids.is_empty() {
                continue;
            }
            // Something else took over the zone: none of ours playing or to
            // come, MISSES_TO_END reads in a row
            if !queue.ahead_ids.iter().any(|id| run.ours.contains(id)) {
                misses += 1;
                println!("[shuffle] none of its songs coming ({misses} of {MISSES_TO_END})");
                if misses >= MISSES_TO_END {
                    drop(guard);
                    end(&app, "The zone moved on to something else.");
                    return;
                }
                continue;
            }
            misses = 0;
            AHEAD.saturating_sub(queue.upcoming)
        };
        if want == 0 {
            continue;
        }
        let picks = {
            let mut guard = RUN.lock().unwrap();
            let Some(run) = guard.as_mut() else { return };
            pick(&app, run, want)
        };
        if picks.is_empty() {
            end(&app, "No songs left to shuffle.");
            return;
        }
        let (handles, ids) = handles_of(&client, &picks).await;
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        if handles.is_empty() || client.play_tracks_how(&zone, &handles, PlayHow::Queue).await.is_err() {
            continue;
        }
        if let Some(run) = RUN.lock().unwrap().as_mut() {
            run.added += handles.len();
            run.ours.extend(ids);
        }
        save(&app);
        emit(&app);
    }
}

/// The plays since the shuffle's window began (Roon's history copy).
fn read_recent(app: &AppHandle, run: &mut Run) {
    run.history_read = Instant::now();
    run.recent_ids.clear();
    run.recent_names.clear();
    let now = now_ms();
    let since = match run.options.recent.as_str() {
        "off" => return,
        "" | "today" => {
            // The local day's start
            let offset = run.options.tz_offset_min as i64 * 60_000;
            let local = now - offset;
            local - local.rem_euclid(86_400_000) + offset
        }
        days => now - days.parse::<i64>().unwrap_or(1) * 86_400_000,
    };
    app.state::<crate::wrapped::Wrapped>().with_plays(app, |plays| {
        for p in plays.iter().take_while(|p| p.played_at >= since) {
            run.recent_ids.extend(p.track_id);
            run.recent_names.insert(format!("{}|{}", fold(&p.artist), fold(&p.title)));
        }
    });
}

impl Run {
    fn next(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

/// Up to `n` songs from the library for the shuffle, each one marked used.
fn pick(app: &AppHandle, run: &mut Run, n: usize) -> Vec<LibraryTrack> {
    let tracks = app.state::<crate::wrapped::LibraryCopy>().tracks(app);
    let artist_of = |t: &LibraryTrack| fold(t.artist.split(" / ").next().unwrap_or(&t.artist));
    let mut pool: Vec<LibraryTrack> = tracks
        .into_iter()
        .filter(|t| !run.options.hearts_only || t.favorite)
        .filter(|t| t.track_id.is_some_and(|id| !run.used.contains(&id) && !run.recent_ids.contains(&id)))
        .filter(|t| !run.recent_names.contains(&format!("{}|{}", fold(&t.artist), fold(&t.title))))
        .collect();
    let mut out = Vec::new();
    while out.len() < n && !pool.is_empty() {
        // The artist gap, shortened only when nothing else is left
        let mut gap = ARTIST_GAP;
        let candidates: Vec<usize> = loop {
            let recent: HashSet<&String> = run.artists.iter().rev().take(gap).collect();
            let ok: Vec<usize> = (0..pool.len()).filter(|i| !recent.contains(&artist_of(&pool[*i]))).collect();
            if !ok.is_empty() || gap == 0 {
                break ok;
            }
            gap /= 2;
        };
        if candidates.is_empty() {
            break;
        }
        let at = candidates[(run.next() % candidates.len() as u64) as usize];
        let t = pool.swap_remove(at);
        run.used.insert(t.track_id.unwrap_or_default());
        run.artists.push_back(artist_of(&t));
        while run.artists.len() > ARTIST_GAP {
            run.artists.pop_front();
        }
        out.push(t);
    }
    out
}

/// The songs' handles for this session (by track id), in the songs' order
/// (the ones Roon can't find left out), and every id the queue may show
/// them by.
async fn handles_of(client: &LibraryClient, picks: &[LibraryTrack]) -> (Vec<String>, Vec<u64>) {
    let mut found: Vec<Option<String>> = vec![None; picks.len()];
    for (chunk_at, chunk) in picks.chunks(AT_ONCE).enumerate() {
        let mut set = tokio::task::JoinSet::new();
        for (i, t) in chunk.iter().enumerate() {
            let (client, id) = (client.clone(), t.track_id);
            set.spawn(async move { (i, match id { Some(id) => client.track_by_id(id).await.ok(), None => None }) });
        }
        while let Some(Ok((i, handle))) = set.join_next().await {
            found[chunk_at * AT_ONCE + i] = handle;
        }
    }
    let mut ours = Vec::new();
    let handles: Vec<String> = picks
        .iter()
        .zip(found)
        .filter_map(|(t, h)| {
            let h = h?;
            ours.extend(t.track_id);
            Some(h)
        })
        .collect();
    // The queue may show a song by either of its ids
    for h in &handles {
        if let Some(id) = client.track_id_of(h).and_then(|id| id.parse::<u64>().ok()) {
            ours.push(id);
        }
    }
    (handles, ours)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_round_trip() {
        let saved = Saved {
            options: ShuffleOptions { zone_id: "z1".into(), hearts_only: true, recent: "today".into(), tz_offset_min: 300 },
            used: vec![1, 2, 3],
            artists: vec!["bolzer".into()],
            ours: vec![1, 2, 3, 99],
            added: 3,
        };
        let json = serde_json::to_string(&saved).unwrap();
        assert!(json.contains("\"zoneId\":\"z1\"") && json.contains("\"heartsOnly\":true"));
        let back: Saved = serde_json::from_str(&json).unwrap();
        assert_eq!(back.options.zone_id, "z1");
        assert_eq!(back.ours, vec![1, 2, 3, 99]);
        assert_eq!(back.added, 3);
    }
}
