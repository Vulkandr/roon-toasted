// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The living queue: one queue that every zone taking part can pick up,
//! at the song and the second the listener was at.
//!
//! Zones take part one by one (Settings > Living Queue), each as
//! "mirrored" or "hide". One of them leads: the one that is playing, or
//! was last. What the leader plays, and what comes after it, is kept in
//! three places:
//!
//! - every mirrored zone's queue, replaced whenever the leader's song or
//!   queue changes (songs added to an idle zone's queue don't start it);
//! - the Roon playlist named "Living Queue", after its first song, the
//!   silent "Living Queue" track (what ARC will be handed from);
//! - nowhere for a hide zone: it holds only the silent track, so its
//!   device is left to sleep. (One just left keeps the song it was paused
//!   in for as long as that is the leader's song, and is sent the silent
//!   track once the leader is on another.)
//!
//! The pickup: a zone taking part starts playing while another leads. If it
//! is on the leader's song (a mirrored zone), it is moved to the leader's
//! second. If it is on the silent track (a hide zone), or on a song loaded
//! before the leader moved on, it is given the leader's songs to play now
//! and then moved to the second. Anything else is someone starting new
//! music there, which just makes that zone the leader. Each way the zone
//! left is paused.
//!
//! It runs on the Hearts & Library link (reading a queue and loading one
//! both go through it). Everything is told to the terminal as
//! `[livequeue]`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use roon_api::{ControlAction, SeekMode, Transport};
use roon_library::{LibraryClient, PlayHow, QueueSong};
use tauri::{AppHandle, Emitter, Manager};

use crate::library::Library;
use crate::settings::AppSettings;
use crate::zones::{ZoneView, Zones};

/// How many songs, from the playing one on, a mirrored zone and the
/// playlist are given.
const SONGS: usize = 25;
/// What the making of the silent track and its playlist (silenttrack.rs)
/// is at: not asked for, under way, or done (well or not).
/// The zone here that leads, as the phones are told it (companion.rs): what
/// a handoff to ARC carries on from.
#[derive(Clone, serde::Serialize)]
pub struct LeaderNow {
    pub zone: String,
    pub title: String,
    pub artist: String,
    pub seconds: f64,
    pub state: String,
}

static LEADER: Mutex<Option<LeaderNow>> = Mutex::new(None);

pub fn leader_now() -> Option<LeaderNow> {
    LEADER.lock().unwrap().clone()
}

/// What the living queue is doing, for the line under the switch on both Living Queue pages (event
/// `living-queue-status`, command `living_queue_status`).
#[derive(Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// "off", "waiting", "following" or "paused".
    pub state: &'static str,
    pub leader: String,
    pub title: String,
    /// Songs in the leader's queue after the playing one, when known.
    pub after: Option<usize>,
    /// The second line: the last thing it did, or what it keeps in step.
    pub note: String,
    /// The note is a problem (shown in amber).
    pub warn: bool,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

#[tauri::command]
pub fn living_queue_status() -> Status {
    STATUS.lock().unwrap().clone().unwrap_or(Status { state: "off", ..Default::default() })
}

/// How long the line tells of a pickup or a takeover before it goes back to what it keeps in step.
const NOTE_FOR: Duration = Duration::from_secs(10);

/// The leader's queue: how many songs come after the playing one, by zone and song (songs_of).
static AFTER: Mutex<Option<(String, String, usize)>> = Mutex::new(None);
/// How many of the songs before the playing one a mirrored zone is given too, for Previous.
const BEFORE: usize = 5;
/// The leader's songs before the playing one (up to BEFORE, oldest first), by zone and song (songs_of).
static PLAYED: Mutex<Option<(String, String, Vec<QueueSong>)>> = Mutex::new(None);

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

static SETUP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
const SETUP_DONE: u8 = 2;
/// How long the leader's song and queue must have stood still before
/// they are copied (so skipping through songs copies once).
const SETTLE: Duration = Duration::from_millis(1200);
/// After a copy that failed, when to try again.
const RETRY: Duration = Duration::from_secs(5);
/// After one zone took over from another, how long before anything is
/// copied (switching back and forth in quick succession once took the
/// Core down: a zone's queue was replaced, and play pressed on it within
/// a second).
const AFTER_TAKEOVER: Duration = Duration::from_secs(4);
/// The least time between two loads of the same zone's queue.
const BETWEEN_LOADS: Duration = Duration::from_secs(6);
/// ARC still showing the old song this long after the phone was asked to hide it: asked again.
const HIDE_AGAIN: Duration = Duration::from_secs(20);
/// How many times the phone is asked to hide ARC for one song here.
const HIDE_TRIES: u32 = 3;

/// The living queue's part of the settings.
#[derive(Clone, Default, PartialEq)]
struct Config {
    on: bool,
    /// Zone id to "mirrored" or "hide".
    zones: BTreeMap<String, String>,
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

/// Takes the settings as they are now (at start and after every change).
pub fn set_config(settings: &AppSettings) {
    *CONFIG.lock().unwrap() = Some(Config {
        on: settings.living_queue && settings.library_controls,
        zones: settings.living_queue_zones.clone(),
    });
}

fn config() -> Config {
    CONFIG.lock().unwrap().clone().unwrap_or_default()
}

/// What a zone that follows holds.
#[derive(Clone, PartialEq)]
enum Holds {
    /// The leader's songs, by title, the playing one first.
    Songs(Vec<String>),
    Silent,
}

#[derive(Default)]
struct Run {
    config: Config,
    leader: Option<String>,
    /// The leader's song and how many come after it, as last seen.
    seen: Option<(String, Option<u32>)>,
    /// Each zone's state as last seen.
    states: HashMap<String, &'static str>,
    holds: HashMap<String, Holds>,
    /// The leader's songs as last copied (the playing one first).
    songs: Vec<QueueSong>,
    /// Something changed that the followers and the playlist don't have yet.
    dirty: Option<Instant>,
    retry: Option<Instant>,
    /// The silent track, once the playlist was read.
    silent: Option<String>,
    /// The silent track and its playlist were asked to be made (once a run).
    setup_asked: bool,
    /// When each zone's queue was last loaded.
    loaded: HashMap<String, Instant>,
    /// Each phone's ARC state as last seen (companion.rs).
    arc_states: HashMap<String, String>,
    /// The song here each ARC was last put on the silent track for, when it was last asked, and how many
    /// times (asked again after a while when ARC still shows the old song; forgotten once ARC plays or
    /// shows the song here, so the same song coming round again counts afresh).
    arc_hidden: HashMap<String, (String, Instant, u32)>,
    /// What each zone here was on when ARC took over: pressing play on one
    /// still there resumes that song, which is carrying on from ARC, not
    /// music of its own.
    left_on: HashMap<String, String>,
    /// Nothing is copied before this (see AFTER_TAKEOVER).
    hold_off: Option<Instant>,
    /// The last thing it did, for the status line, until when it is told.
    note: Option<(String, Instant)>,
    /// A zone the last copy couldn't load (told until a copy loads everything).
    trouble: Option<String>,
}

fn title_of(zone: &ZoneView) -> String {
    zone.now_playing.as_ref().map(|n| n.title.clone()).unwrap_or_default()
}

fn is_silent(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case(crate::silenttrack::TITLE)
}

/// Has the silent track looked for, and set up where it is missing, in the
/// background: once for a run.
fn ask_setup(app: &AppHandle, run: &mut Run) {
    if run.setup_asked {
        return;
    }
    run.setup_asked = true;
    crate::silenttrack::start(app);
}

/// The setup is over: everything is copied again, with what it made.
pub fn setup_finished() {
    SETUP.store(SETUP_DONE, std::sync::atomic::Ordering::SeqCst);
}

fn going(state: &str) -> bool {
    state == "playing" || state == "loading"
}

fn zone(app: &AppHandle, id: &str) -> Option<ZoneView> {
    app.state::<Zones>().payload_now().zones.into_iter().find(|z| z.zone_id == id)
}

/// Starts the watcher (once, at start).
pub fn start(app: &AppHandle) {
    if let Some(path) = crate::settings::path(app) {
        set_config(&crate::settings::load(&path));
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut run = Run::default();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // (dev build, for now: what the Core tells about where its music is kept, a moment after the link is up)
        let mut storage_told: Option<Instant> = None;
        let mut storage_done = !cfg!(debug_assertions);
        // (dev build, for now: every zone the library link knows of, the private ones too, told when one
        // comes, goes or changes state; for finding ARC)
        let mut zones_seen: HashMap<String, String> = HashMap::new();
        let mut zones_look = Instant::now();
        loop {
            tick.tick().await;
            if cfg!(debug_assertions) && zones_look.elapsed() >= Duration::from_secs(2) {
                zones_look = Instant::now();
                if let Some(client) = app.state::<Library>().client() {
                    let now: HashMap<String, String> = client.zone_report().into_iter().collect();
                    let short = |m: &str| -> String {
                        m.split("; ").filter(|f| ["Name:", "State:", "PrivateToMachineId:", "NowPlaying:", "ZoneId:"].iter().any(|k| f.starts_with(k))).collect::<Vec<_>>().join("; ")
                    };
                    for (oid, members) in &now {
                        match zones_seen.get(oid) {
                            None => println!("[livequeue] zones: new {oid}: {members}"),
                            Some(before) if short(before) != short(members) => println!("[livequeue] zones: {oid} now: {}", short(members)),
                            _ => {}
                        }
                    }
                    for oid in zones_seen.keys().filter(|o| !now.contains_key(*o)) {
                        println!("[livequeue] zones: gone {oid}: {}", short(&zones_seen[oid]));
                    }
                    zones_seen = now;
                } else {
                    zones_seen.clear();
                }
            }
            if !storage_done {
                if let Some(client) = app.state::<Library>().client() {
                    let since = *storage_told.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_secs(8) {
                        storage_done = true;
                        let lines = client.storage_report();
                        println!("[livequeue] storage: {} object(s) about where the Core keeps music", lines.len());
                        for line in lines {
                            println!("[livequeue] storage: {line}");
                        }
                    }
                }
            }
            let config = config();
            if !config.on || config.zones.is_empty() {
                if run.leader.is_some() || run.config.on {
                    println!("[livequeue] off");
                }
                run = Run::default();
                tell(&app, Status { state: "off", ..Default::default() });
                continue;
            }
            let Some(client) = app.state::<Library>().client() else {
                // (the link is down, the Core restarting perhaps: everything is looked at afresh after it)
                if run.config.on {
                    println!("[livequeue] the library link is down; waiting for it");
                    run = Run::default();
                }
                tell(&app, Status { state: "off", ..Default::default() });
                continue;
            };
            if config != run.config {
                println!("[livequeue] on, {} zone(s) taking part", config.zones.len());
                run.config = config;
                run.dirty = Some(Instant::now());
            }
            // (the silent track: looked for as soon as the living queue is on and the link is up)
            ask_setup(&app, &mut run);
            step(&app, &client, &mut run).await;
            let now = status_of(&app, &client, &run);
            tell(&app, now);
        }
    });
}

/// Sends the status to the pages when it changed.
fn tell(app: &AppHandle, status: Status) {
    let mut last = STATUS.lock().unwrap();
    if last.as_ref() != Some(&status) {
        *last = Some(status.clone());
        drop(last);
        let _ = app.emit("living-queue-status", status);
    }
}

/// What the line under the switch says, from how things stand.
fn status_of(app: &AppHandle, client: &LibraryClient, run: &Run) -> Status {
    let all = app.state::<Zones>().payload_now().zones;
    let members: Vec<&ZoneView> = all.iter().filter(|z| run.config.zones.contains_key(&z.zone_id) && client.zone_ids().contains(&z.zone_id)).collect();
    let Some(leader_id) = run.leader.as_ref() else {
        return Status { state: "waiting", note: keeps(run, &members, None), ..Default::default() };
    };
    let (leader, title, playing) = if leader_id.starts_with("arc:") {
        match crate::companion::arcs().into_iter().find(|a| &a.zone_id == leader_id) {
            Some(a) => (format!("ARC on {}", a.name), a.title.clone(), a.playing()),
            None => return Status { state: "waiting", note: keeps(run, &members, None), ..Default::default() },
        }
    } else {
        match members.iter().find(|z| &z.zone_id == leader_id) {
            Some(z) => (z.name.clone(), title_of(z), going(z.state)),
            None => return Status { state: "waiting", note: keeps(run, &members, None), ..Default::default() },
        }
    };
    let after = AFTER.lock().unwrap().as_ref().filter(|(zone, song, _)| zone == leader_id && same_text(song, &title)).map(|(_, _, n)| *n);
    let (note, warn) = match (&run.note, &run.trouble) {
        (Some((text, until)), _) if Instant::now() < *until => (text.clone(), false),
        (_, Some(trouble)) => (trouble.clone(), true),
        _ => (keeps(run, &members, Some(leader_id)), false),
    };
    Status { state: if playing { "following" } else { "paused" }, leader, title, after, note, warn }
}

/// What it keeps in step: "2 zones mirrored, 1 hidden, playlist up to date".
fn keeps(run: &Run, members: &[&ZoneView], leader: Option<&String>) -> String {
    let others = |mode: &str| run.config.zones.iter().filter(|(id, m)| m.as_str() == mode && Some(*id) != leader && (id.starts_with("arc:") || members.iter().any(|z| &z.zone_id == *id))).count();
    let (mirrored, hidden) = (others("mirrored"), others("hide"));
    let mut parts = Vec::new();
    if mirrored > 0 {
        parts.push(format!("{mirrored} zone{} mirrored", if mirrored == 1 { "" } else { "s" }));
    }
    if hidden > 0 {
        parts.push(format!("{hidden}{} hidden", if mirrored > 0 { String::new() } else if hidden == 1 { " zone".into() } else { " zones".into() }));
    }
    if leader.is_some() && run.silent.is_some() {
        parts.push(if run.dirty.is_none() { "playlist up to date".into() } else { "updating the playlist".into() });
    }
    if parts.is_empty() {
        "No other zone taking part is here right now".into()
    } else {
        let text = parts.join(", ");
        let mut c = text.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }
}

/// One look at the zones, and whatever follows from it.
async fn step(app: &AppHandle, client: &LibraryClient, run: &mut Run) {
    let all = app.state::<Zones>().payload_now().zones;
    let members: Vec<&ZoneView> = all.iter().filter(|z| run.config.zones.contains_key(&z.zone_id) && client.zone_ids().contains(&z.zone_id)).collect();

    // Zones that started playing since the last look (the first look at a zone is not a start)
    let starters: Vec<&ZoneView> = members
        .iter()
        .filter(|z| going(z.state) && run.states.get(&z.zone_id).is_some_and(|before| !going(before)))
        .copied()
        .collect();
    // (a zone that came back, a phone's Roon app reopened, has nothing of ours for sure)
    for z in &members {
        if !run.states.contains_key(&z.zone_id) && run.holds.remove(&z.zone_id).is_some() {
            run.dirty.get_or_insert_with(Instant::now);
        }
    }
    run.states = members.iter().map(|z| (z.zone_id.clone(), z.state)).collect();

    // ARC on the phones taking part (through the companion app), and the one that just started playing
    let arcs: Vec<crate::companion::ArcNow> = crate::companion::arcs().into_iter().filter(|a| run.config.zones.get(&a.zone_id).is_some_and(|m| m == "hide")).collect();
    // (started: it plays now and didn't before; on the silent track, the companion's handoff, counts as not
    // playing, its state kept as "silent:<state>")
    let arc_started = arcs
        .iter()
        .find(|a| a.playing() && !is_silent(&a.title) && run.arc_states.get(&a.zone_id).is_some_and(|before| before != "playing"))
        .cloned();
    run.arc_states = arcs
        .iter()
        .map(|a| (a.zone_id.clone(), if !a.online { "away".into() } else if is_silent(&a.title) { format!("silent:{}", a.state) } else { a.state.clone() }))
        .collect();

    // The leader: kept while its zone is there; without one, the zone that is playing
    let here = |id: &String| members.iter().any(|z| &z.zone_id == id) || arcs.iter().any(|a| &a.zone_id == id);
    if run.leader.as_ref().is_some_and(|id| !here(id)) {
        println!("[livequeue] the leading zone is gone");
        run.leader = None;
        run.seen = None;
    }
    if run.leader.is_none() {
        if let Some(z) = members.iter().find(|z| z.state == "playing" && !is_silent(&title_of(z))) {
            println!("[livequeue] \"{}\" leads, on \"{}\"", z.name, title_of(z));
            run.leader = Some(z.zone_id.clone());
            run.seen = None;
        } else if let Some(a) = arcs.iter().find(|a| a.playing() && !is_silent(&a.title)) {
            println!("[livequeue] ARC on \"{}\" leads, on \"{}\"", a.name, a.title);
            run.left_on = members.iter().map(|z| (z.zone_id.clone(), title_of(z))).collect();
            run.leader = Some(a.zone_id.clone());
            run.seen = None;
        }
    }
    let Some(leader_id) = run.leader.clone() else {
        *LEADER.lock().unwrap() = None;
        return;
    };
    *LEADER.lock().unwrap() = members.iter().find(|z| z.zone_id == leader_id).and_then(|z| {
        let n = z.now_playing.as_ref()?;
        Some(LeaderNow { zone: z.name.clone(), title: n.title.clone(), artist: n.artist.clone(), seconds: z.seek_position.unwrap_or(0.0), state: z.state.to_string() })
    });

    // ARC started playing while another zone led: it leads now, and the zone left is paused
    if let Some(arc) = arc_started.filter(|a| a.zone_id != leader_id) {
        println!("[livequeue] ARC on \"{}\" started (on \"{}\") while another zone led; it leads now", arc.name, arc.title);
        if let Some(left) = members.iter().find(|z| z.zone_id == leader_id) {
            if going(left.state) {
                pause_zone(app, left);
            }
            // (left in ARC's song, it holds the songs after it already: not loaded again for ARC)
            if same_text(&title_of(left), &arc.title) && run.songs.first().is_some_and(|s| same_text(&s.title, &arc.title)) {
                run.holds.insert(left.zone_id.clone(), Holds::Songs(run.songs.iter().map(|s| s.title.clone()).collect()));
            }
        } else if leader_id.starts_with("arc:") {
            crate::companion::command(&leader_id, "pause");
        }
        let from = members.iter().find(|z| z.zone_id == leader_id).map(|z| z.name.clone()).unwrap_or_else(|| "another zone".into());
        run.note = Some((format!("ARC on {} took over from {from}", arc.name), Instant::now() + NOTE_FOR));
        run.left_on = members.iter().map(|z| (z.zone_id.clone(), title_of(z))).collect();
        run.leader = Some(arc.zone_id.clone());
        run.seen = None;
        run.hold_off = Some(Instant::now() + AFTER_TAKEOVER);
        return;
    }
    // ARC leads: a zone here that starts carries on from it; otherwise the zones here follow ARC's song
    if leader_id.starts_with("arc:") {
        let Some(arc) = arcs.iter().find(|a| a.zone_id == leader_id).cloned() else { return };
        if let Some(starter) = starters.first() {
            take_over_from_arc(app, client, run, &arc, starter).await;
            let now = app.state::<Zones>().payload_now().zones;
            run.states = now.iter().filter(|z| run.config.zones.contains_key(&z.zone_id)).map(|z| (z.zone_id.clone(), z.state)).collect();
            return;
        }
        if arc.title.is_empty() || is_silent(&arc.title) {
            return;
        }
        let seen = (arc.title.clone(), None);
        if run.seen.as_ref() != Some(&seen) {
            run.seen = Some(seen);
            run.dirty = Some(Instant::now());
            run.retry = None;
        }
        let due = run.dirty.is_some_and(|since| since.elapsed() >= SETTLE)
            && run.retry.is_none_or(|at| Instant::now() >= at)
            && run.hold_off.is_none_or(|at| Instant::now() >= at);
        if due {
            if copy_from_arc(app, client, run, &arc, &members).await {
                run.dirty = None;
                run.retry = None;
            } else {
                run.retry = Some(Instant::now() + RETRY);
            }
        }
        return;
    }
    // (the silent track and its playlist were just made: everything is copied again, with them)
    if SETUP.compare_exchange(SETUP_DONE, 0, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst).is_ok() {
        run.dirty = Some(Instant::now());
        run.retry = None;
    }

    // A pickup, or new music started on another zone
    if let Some(starter) = starters.iter().find(|z| z.zone_id != leader_id) {
        let Some(leader) = members.iter().find(|z| z.zone_id == leader_id) else { return };
        take_over(app, client, run, leader, starter).await;
        // (what the zones did meanwhile was ours)
        let now = app.state::<Zones>().payload_now().zones;
        run.states = now.iter().filter(|z| run.config.zones.contains_key(&z.zone_id)).map(|z| (z.zone_id.clone(), z.state)).collect();
        return;
    }

    // The leader's song or queue changed: copy it once it stands still
    let Some(leader) = members.iter().find(|z| z.zone_id == leader_id) else { return };
    let title = title_of(leader);
    if title.is_empty() || is_silent(&title) {
        return;
    }
    let seen = (title.clone(), leader.queue_items_remaining);
    if run.seen.as_ref() != Some(&seen) {
        run.seen = Some(seen);
        run.dirty = Some(Instant::now());
        run.retry = None;
    }
    let due = run.dirty.is_some_and(|since| since.elapsed() >= SETTLE)
        && run.retry.is_none_or(|at| Instant::now() >= at)
        && run.hold_off.is_none_or(|at| Instant::now() >= at);
    if due {
        if copy(app, client, run, leader, &members).await {
            run.dirty = None;
            run.retry = None;
        } else {
            run.retry = Some(Instant::now() + RETRY);
        }
    }
    // ARC on Hide Current (it can't be mirrored): once what it shows is no longer the song here, the
    // phone puts it on the silent track, paused. Pressing play on that in ARC is the handoff. (Once this
    // song is copied, the playlist included: putting ARC on the silent track makes it fetch the playlist
    // again, so its copy holds this song by the time play is pressed.)
    if run.dirty.is_none() && run.hold_off.is_none_or(|at| Instant::now() >= at) {
        for arc in &arcs {
            let busy = arc.online && (arc.state == "playing" || arc.state == "buffering");
            if busy || is_silent(&arc.title) || same_text(&arc.title, &title) {
                run.arc_hidden.remove(&arc.zone_id);
                continue;
            }
            if !arc.online {
                continue;
            }
            let tries = match run.arc_hidden.get(&arc.zone_id) {
                Some((song, _, _)) if !same_text(song, &title) => 0,
                Some((_, at, tries)) if *tries >= HIDE_TRIES || at.elapsed() < HIDE_AGAIN => continue,
                Some((_, _, tries)) => *tries,
                None => 0,
            };
            println!(
                "[livequeue] ARC on \"{}\" shows \"{}\" while \"{}\" is on \"{title}\": the phone puts it on the silent track{}",
                arc.name,
                arc.title,
                leader.name,
                if tries > 0 { format!(" (asked again, {} of {HIDE_TRIES})", tries + 1) } else { String::new() }
            );
            crate::companion::command(&arc.zone_id, "hide");
            run.arc_hidden.insert(arc.zone_id.clone(), (title.clone(), Instant::now(), tries + 1));
        }
    }
}

/// The leader's songs, the playing one first; none when the queue hasn't
/// caught up with what the zone says is playing.
async fn songs_of(client: &LibraryClient, leader: &ZoneView) -> Vec<QueueSong> {
    let (songs, report) = match client.queue_songs_report(&leader.zone_id, None, BEFORE, false).await {
        Ok(got) => got,
        Err(e) => {
            println!("[livequeue] the queue of \"{}\" could not be read: {e:?}", leader.name);
            return Vec::new();
        }
    };
    // From the song the Core marks as playing on (a radio song can come marked played as well); without
    // one, the songs not played yet
    let ahead: Vec<QueueSong> = match songs.iter().position(|s| s.current) {
        Some(at) => songs[at..].iter().take(SONGS).cloned().collect(),
        None => songs.iter().filter(|s| !s.played && !s.skipped).take(SONGS).cloned().collect(),
    };
    // (for the status line: all the songs after the playing one, not only the ones copied)
    if let Some(first) = ahead.first() {
        let total = match songs.iter().position(|s| s.current) {
            Some(at) => songs.len() - at - 1,
            None => songs.iter().filter(|s| !s.played && !s.skipped).count().saturating_sub(1),
        };
        *AFTER.lock().unwrap() = Some((leader.zone_id.clone(), first.title.clone(), total));
        // (skipped ones too: Next marks the song left as skipped, and Previous goes back to it all the same)
        let before: Vec<QueueSong> = match songs.iter().position(|s| s.current) {
            Some(at) => songs[at.saturating_sub(BEFORE)..at].to_vec(),
            None => Vec::new(),
        };
        // (dev, while Previous on a mirrored zone is being worked out: what the leader's queue holds before the
        // playing song, once per song)
        static TOLD_BEFORE: Mutex<String> = Mutex::new(String::new());
        let key = format!("{}|{}", leader.zone_id, first.title);
        let mut told = TOLD_BEFORE.lock().unwrap();
        if *told != key {
            *told = key;
            println!(
                "[livequeue] the queue of \"{}\": {} song(s) before \"{}\" ({} played, {} skipped), {} given for Previous; {report}",
                leader.name,
                songs.iter().position(|s| s.current).unwrap_or(0),
                first.title,
                songs.iter().filter(|s| s.played).count(),
                songs.iter().filter(|s| s.skipped).count(),
                before.len()
            );
        }
        drop(told);
        *PLAYED.lock().unwrap() = Some((leader.zone_id.clone(), first.title.clone(), before));
    }
    let why = match ahead.first() {
        Some(first) if first.title == title_of(leader) => return ahead,
        Some(first) => format!("starts at \"{}\", not at \"{}\" yet", first.title, title_of(leader)),
        None => format!("has nothing from \"{}\" on ({} songs in it; {report})", title_of(leader), songs.len()),
    };
    // (said once per song and zone, not at every retry)
    static TOLD: Mutex<String> = Mutex::new(String::new());
    let key = format!("{}|{}", leader.zone_id, title_of(leader));
    let mut told = TOLD.lock().unwrap();
    if *told != key {
        *told = key;
        println!("[livequeue] the queue of \"{}\" {why}; trying again", leader.name);
    }
    Vec::new()
}

/// Copies the leader's songs to the mirrored zones and the playlist, and
/// gives the hide zones the silent track. False when it should be tried
/// again.
async fn copy(app: &AppHandle, client: &LibraryClient, run: &mut Run, leader: &ZoneView, members: &[&ZoneView]) -> bool {
    let songs = songs_of(client, leader).await;
    let Some(first) = songs.first().map(|s| s.title.clone()) else { return false };
    let handles: Vec<String> = songs.iter().map(|s| s.handle.clone()).collect();
    let titles: Vec<String> = songs.iter().map(|s| s.title.clone()).collect();
    run.songs = songs;
    let began = Instant::now();
    let mut waiting = false;

    // The playlist first: it is also where the silent track is found
    let playlist = sync_playlist(client, run).await;

    let mut done = Vec::new();
    let mut trouble = None;
    // (the songs before the playing one, for Previous on a mirrored zone)
    let before: Vec<String> = PLAYED
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(zone, song, _)| zone == &leader.zone_id && song == &first)
        .map(|(_, _, list)| list.iter().map(|s| s.handle.clone()).collect())
        .unwrap_or_default();
    for z in members.iter().filter(|z| z.zone_id != leader.zone_id) {
        // (as it is right now: a zone that is playing is never touched)
        let Some(now) = zone(app, &z.zone_id) else { continue };
        if going(now.state) {
            continue;
        }
        let hide = run.config.zones.get(&z.zone_id).is_some_and(|m| m == "hide");
        let (give, holds) = if hide {
            let Some(silent) = run.silent.clone() else { continue };
            // (not while what it shows is still right: a hide zone just left is paused in the leader's song, and
            // keeps it until the leader is on another; only then is it sent the silent track, once)
            match run.holds.get(&z.zone_id) {
                Some(Holds::Silent) => continue,
                Some(Holds::Songs(held)) if held.first() == Some(&first) => continue,
                _ => {}
            }
            (vec![silent], Holds::Silent)
        } else {
            // (a zone that already holds these songs, the one just left for one, is not loaded again)
            if run.holds.get(&z.zone_id) == Some(&Holds::Songs(titles.clone())) {
                continue;
            }
            (handles.clone(), Holds::Songs(titles.clone()))
        };
        if run.loaded.get(&z.zone_id).is_some_and(|at| at.elapsed() < BETWEEN_LOADS) {
            waiting = true;
            continue;
        }
        run.loaded.insert(z.zone_id.clone(), Instant::now());
        if let Err(e) = client.queue_clear(&z.zone_id).await {
            println!("[livequeue] the queue of \"{}\" could not be cleared: {e:?}", z.name);
            trouble = Some(format!("Couldn't load {} last time; it gets the next song", z.name));
            continue;
        }
        match client.play_tracks_how(&z.zone_id, &give, PlayHow::Queue).await {
            Ok(()) => {
                let mut extra = String::new();
                if !hide && !before.is_empty() {
                    extra = format!(", {}", with_played(app, client, z, &before, &first).await);
                }
                done.push(format!("\"{}\" ({}{extra})", z.name, if hide { "the silent track".to_string() } else { format!("{} songs", give.len()) }));
                run.holds.insert(z.zone_id.clone(), holds);
            }
            Err(e) => {
                println!("[livequeue] the songs could not be added to the queue of \"{}\": {e:?}", z.name);
                trouble = Some(format!("Couldn't load {} last time; it gets the next song", z.name));
            }
        }
    }
    run.trouble = trouble.clone();
    println!(
        "[livequeue] \"{}\" is on \"{first}\", {} songs copied in {} ms; zones: {}; playlist: {playlist}",
        leader.name,
        handles.len(),
        began.elapsed().as_millis(),
        if done.is_empty() { "none to load".to_string() } else { done.join(", ") },
    );
    // (a zone loaded only moments ago gets its turn at the next try; one that failed, with the next song)
    !waiting
}

/// Gives a mirrored zone, just loaded with the leader's songs, the songs before the playing one too: added
/// at the end, then moved ahead of the song it starts on (Queue::MoveItems), where Previous goes back to
/// them. Says how it went, for the log.
async fn with_played(app: &AppHandle, client: &LibraryClient, z: &ZoneView, before: &[String], first: &str) -> String {
    if let Err(e) = client.play_tracks_how(&z.zone_id, before, PlayHow::Queue).await {
        return format!("the {} before it not added: {e:?}", before.len());
    }
    // (the Core lists the added items a moment later)
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Err(e) = client.queue_move_last_to_front(&z.zone_id, before.len()).await {
        return format!("the {} before it added at the end, not moved: {e:?}", before.len());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let on = zone(app, &z.zone_id).map(|now| title_of(&now)).unwrap_or_default();
    let queue = match client.queue_songs_report(&z.zone_id, None, BEFORE + 2, false).await {
        Ok((songs, _)) => {
            let at = songs.iter().position(|s| s.current);
            format!(
                "queue now {} songs, the current one {} (\"{}\"), {} played before it",
                songs.len(),
                at.map(|i| format!("at {}", i + 1)).unwrap_or_else(|| "not marked".into()),
                at.map(|i| songs[i].title.clone()).unwrap_or_default(),
                songs.iter().filter(|s| s.played).count()
            )
        }
        Err(e) => format!("queue not read: {e:?}"),
    };
    let can = zone(app, &z.zone_id).map(|now| now.can_previous).unwrap_or(false);
    format!("{} before it moved ahead; the zone shows \"{on}\"{}; previous allowed: {can}; {queue}", before.len(), if same_text(&on, first) { "" } else { " (not the song!)" })
}

/// The same song, as far as a queue's song and a playlist's can be told to be.
fn same(song: &QueueSong, title: &str, length: Option<i32>) -> bool {
    song.title.to_lowercase() == title.to_lowercase() && (song.length_seconds.is_none() || length.is_none() || song.length_seconds == length)
}

/// Brings the "Living Queue" playlist to the silent track followed by the
/// leader's songs, changing as little as it can: songs the leader is past
/// are taken off the front, new ones added at the end. Says what it did.
async fn sync_playlist(client: &LibraryClient, run: &mut Run) -> String {
    let lists = match client.playlists() {
        Ok(lists) => lists,
        Err(e) => return format!("could not be listed: {e:?}"),
    };
    run.silent = None;
    let Some(list) = lists.into_iter().find(|p| p.name.trim().eq_ignore_ascii_case(crate::silenttrack::PLAYLIST) && !p.read_only) else {
        return "none named Living Queue yet".into();
    };
    let entries = match client.playlist_tracks(&list.handle).await {
        Ok(entries) => entries,
        Err(e) => return format!("could not be read: {e:?}"),
    };
    run.silent = entries.iter().find(|e| is_silent(&e.entry.track.title)).map(|e| e.entry.handle.clone());
    if run.silent.is_none() {
        return "has no Living Queue track in it yet, left alone".into();
    }
    let tail: Vec<_> = entries.iter().filter(|e| !is_silent(&e.entry.track.title)).collect();
    // How many of its songs go off the front for the rest to be how the leader's songs begin
    let fits = |from: usize| tail[from..].len() <= run.songs.len() && tail[from..].iter().zip(&run.songs).all(|(e, s)| same(s, &e.entry.track.title, e.entry.track.length_seconds));
    let off = (0..=tail.len()).find(|k| fits(*k)).unwrap_or(tail.len());
    let kept = tail.len() - off;
    let add: Vec<String> = run.songs[kept..].iter().map(|s| s.handle.clone()).collect();
    if off == 0 && add.is_empty() {
        return "already right".into();
    }
    if off > 0 {
        let items: Vec<String> = tail[..off].iter().map(|e| e.item_handle.clone()).collect();
        if let Err(e) = client.remove_playlist_items(&list.handle, &items).await {
            return format!("{off} songs could not be taken off: {e:?}");
        }
        // (no answer comes: read it again until they're gone, so the new ones land after what's kept)
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            match client.playlist_tracks(&list.handle).await {
                Ok(left) if !left.iter().any(|e| items.contains(&e.item_handle)) => break,
                _ => {}
            }
        }
    }
    if !add.is_empty() {
        if let Err(e) = client.add_to_playlist(&list.handle, &add).await {
            return format!("{off} off, but {} could not be added: {e:?}", add.len());
        }
    }
    format!("{off} off the front, {kept} kept, {} added", add.len())
}

/// Another zone started while `leader` led: it carries on from the leader
/// (a pickup) or it brought music of its own. Either way it leads now and
/// the zone left is paused.
async fn take_over(app: &AppHandle, client: &LibraryClient, run: &mut Run, leader: &ZoneView, starter: &ZoneView) {
    let seen = Instant::now();
    let on = title_of(starter);
    let leading = title_of(leader);
    let second = leader.seek_position.unwrap_or(0.0);
    println!(
        "[livequeue] \"{}\" started ({}, on \"{on}\") while \"{}\" leads ({}, on \"{leading}\" at {second:.0} s)",
        starter.name, starter.state, leader.name, leader.state
    );
    let transport = match app.state::<Zones>().transport() {
        Ok(t) => t,
        Err(e) => {
            println!("[livequeue] {e}");
            return;
        }
    };
    // The zone left: it should not play on (the pause goes out by itself, its answer takes about half a second)
    if going(leader.state) {
        let transport = transport.clone();
        let (id, name) = (leader.zone_id.clone(), leader.name.clone());
        tauri::async_runtime::spawn(async move {
            let pause = transport.control(&id, ControlAction::Pause).await;
            println!("[livequeue] pause of \"{name}\" answered {} ms after the start was seen: {pause:?}", seen.elapsed().as_millis());
        });
    }
    let held = run.holds.get(&starter.zone_id).cloned();
    let stale = matches!(&held, Some(Holds::Songs(t)) if t.first() == Some(&on)) && on != leading;
    // Whether the starter ends up with the leader's songs (then the zone left holds the same ones already)
    let mut carried = false;
    // (a hide zone on the leader's song is one that was left in it: the songs after it are the leader's only
    // while the leader's queue is as it was then)
    let hide = run.config.zones.get(&starter.zone_id).is_some_and(|m| m == "hide");
    let now: Vec<String> = run.songs.iter().map(|s| s.title.clone()).collect();
    let behind = hide && on == leading && held != Some(Holds::Songs(now));
    if !leading.is_empty() && on == leading && !behind {
        // On the leader's song already: only the second is missing
        println!("[livequeue] pickup: \"{}\" is on the song, moving it to {second:.0} s", starter.name);
        land(app, &transport, starter, second, seen).await;
        carried = true;
    } else if !leading.is_empty() && (is_silent(&on) || stale || behind) {
        // On the silent track, or on a song from before the leader moved on: the leader's songs, played now
        let mut songs = run.songs.clone();
        if songs.first().map(|s| s.title.as_str()) != Some(leading.as_str()) {
            songs = songs_of(client, leader).await;
        }
        if songs.is_empty() {
            println!("[livequeue] pickup: the songs of \"{}\" are not known; \"{}\" is left as it is", leader.name, starter.name);
        } else {
            let handles: Vec<String> = songs.iter().map(|s| s.handle.clone()).collect();
            let cleared = client.queue_clear(&starter.zone_id).await;
            let played = client.play_tracks_how(&starter.zone_id, &handles, PlayHow::Now).await;
            println!(
                "[livequeue] pickup: \"{}\" was on {}; queue cleared: {cleared:?}, {} songs played now: {played:?}, {} ms after the start was seen",
                starter.name,
                if stale { "an older song" } else if behind { "the song, with an older queue after it" } else { "the silent track" },
                handles.len(),
                seen.elapsed().as_millis()
            );
            // (the song has to be the one playing before a seek means anything)
            let waiting = Instant::now();
            while waiting.elapsed() < Duration::from_secs(6) {
                if zone(app, &starter.zone_id).is_some_and(|z| title_of(&z) == leading && going(z.state)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            match zone(app, &starter.zone_id) {
                Some(z) if title_of(&z) == leading => {
                    land(app, &transport, starter, second, seen).await;
                    carried = true;
                    // (and the songs before it, for Previous, once it plays at the right second)
                    let before: Vec<String> = PLAYED
                        .lock()
                        .unwrap()
                        .as_ref()
                        .filter(|(zone, song, _)| zone == &leader.zone_id && song == &leading)
                        .map(|(_, _, list)| list.iter().map(|s| s.handle.clone()).collect())
                        .unwrap_or_default();
                    if !before.is_empty() {
                        println!("[livequeue] pickup: \"{}\": {}", starter.name, with_played(app, client, starter, &before, &leading).await);
                    }
                }
                z => println!("[livequeue] pickup: \"{}\" never got to \"{leading}\" (it is on \"{}\")", starter.name, z.as_ref().map(title_of).unwrap_or_default()),
            }
        }
    } else {
        println!("[livequeue] \"{}\" brought music of its own; it leads now", starter.name);
    }
    run.note = Some((
        if carried { format!("{} picked up from {} at {}", starter.name, leader.name, clock(second)) } else { format!("{} started music of its own and leads now", starter.name) },
        Instant::now() + NOTE_FOR,
    ));
    run.holds.remove(&starter.zone_id);
    // The zone left is paused in the song the new leader carries on, with the same songs after it. A mirrored
    // one is not loaded again for as long as those stay the songs; a hide one keeps showing the song until
    // the leader is on another, and only then gets the silent track (after new music, the zone left is
    // loaded like any other)
    if carried && run.songs.first().is_some_and(|s| s.title == leading) {
        run.holds.insert(leader.zone_id.clone(), Holds::Songs(run.songs.iter().map(|s| s.title.clone()).collect()));
    } else {
        run.holds.remove(&leader.zone_id);
    }
    run.hold_off = Some(Instant::now() + AFTER_TAKEOVER);
    run.leader = Some(starter.zone_id.clone());
    run.seen = None;
    run.dirty = Some(Instant::now());
    run.retry = None;
}

/// Moves a zone that just started to `second`. A seek sent while the zone
/// is still loading the song is dropped, so it goes out once the zone says
/// "playing", and again until the zone is seen there.
async fn land(app: &AppHandle, transport: &Transport, target: &ZoneView, second: f64, seen: Instant) {
    let wanted = second.round() as i64;
    if wanted < 2 {
        println!("[livequeue] pickup: the song had only just begun, nothing to move");
        return;
    }
    for attempt in 1..=8 {
        let waiting = Instant::now();
        while zone(app, &target.zone_id).map(|z| z.state) != Some("playing") && waiting.elapsed() < Duration::from_secs(3) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let before = zone(app, &target.zone_id);
        if before.is_none() {
            println!("[livequeue] pickup: \"{}\" is gone, nothing moved", target.name);
            return;
        }
        let seek = transport.seek(&target.zone_id, SeekMode::Absolute, wanted).await;
        println!(
            "[livequeue] pickup: seek {attempt} to {wanted} s sent {} ms after the start was seen (the zone was {} at {:.0} s): {seek:?}",
            seen.elapsed().as_millis(),
            before.as_ref().map(|z| z.state).unwrap_or("gone"),
            before.as_ref().and_then(|z| z.seek_position).unwrap_or(-1.0),
        );
        if seek.is_err() {
            // (the Core is not answering: asking again only adds to it)
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let at = zone(app, &target.zone_id).and_then(|z| z.seek_position).unwrap_or(-1.0);
        if (at - wanted as f64).abs() <= 4.0 {
            println!("[livequeue] pickup: \"{}\" is at {at:.0} s, {} ms after the start was seen: landed", target.name, seen.elapsed().as_millis());
            return;
        }
        println!("[livequeue] pickup: \"{}\" is at {at:.0} s, not there yet", target.name);
    }
    println!("[livequeue] pickup: \"{}\" never took the seek", target.name);
}

/// Pauses a zone, without waiting for the answer (it takes about half a second).
fn pause_zone(app: &AppHandle, zone: &ZoneView) {
    let Ok(transport) = app.state::<Zones>().transport() else { return };
    let (id, name) = (zone.zone_id.clone(), zone.name.clone());
    tauri::async_runtime::spawn(async move {
        let pause = transport.control(&id, ControlAction::Pause).await;
        println!("[livequeue] pause of \"{name}\": {pause:?}");
    });
}

fn same_text(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// One artist line names the other (ARC and a search can join several artists differently).
fn same_artist(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().to_lowercase(), b.trim().to_lowercase());
    !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a) || a.split([',', '/', '&']).next().map(str::trim) == b.split([',', '/', '&']).next().map(str::trim))
}

/// ARC's song as Roon songs to play, with what comes after it when that is
/// known: from the Living Queue playlist when ARC is playing from it (that
/// song and the ones under it), else the song alone, found by a search.
async fn songs_for_arc(client: &LibraryClient, arc: &crate::companion::ArcNow) -> (Vec<String>, Vec<String>, &'static str) {
    if let Ok(lists) = client.playlists() {
        if let Some(list) = lists.into_iter().find(|p| p.name.trim().eq_ignore_ascii_case(crate::silenttrack::PLAYLIST)) {
            if let Ok(entries) = client.playlist_tracks(&list.handle).await {
                let rows: Vec<_> = entries.iter().filter(|e| !is_silent(&e.entry.track.title)).collect();
                if let Some(at) = rows.iter().position(|e| same_text(&e.entry.track.title, &arc.title) && same_artist(&e.entry.track.artist, &arc.artist)) {
                    let rows = &rows[at..rows.len().min(at + SONGS)];
                    return (
                        rows.iter().map(|e| e.entry.handle.clone()).collect(),
                        rows.iter().map(|e| e.entry.track.title.clone()).collect(),
                        "from the Living Queue playlist",
                    );
                }
            }
        }
    }
    let query = format!("{} {}", arc.title, arc.artist.split([',', '/', '&']).next().unwrap_or("").trim());
    match client.search(query.trim(), 25).await {
        Ok(hits) => {
            let mut fits: Vec<_> = hits.tracks.into_iter().filter(|t| same_text(&t.title, &arc.title) && same_artist(&t.artist, &arc.artist)).collect();
            // (the copy in the library first)
            fits.sort_by_key(|t| !t.in_library);
            match fits.first() {
                Some(t) => (vec![t.handle.clone()], vec![t.title.clone()], "found by a search, the song alone"),
                None => (Vec::new(), Vec::new(), "not found by a search"),
            }
        }
        Err(e) => {
            println!("[livequeue] the search for \"{query}\" failed: {e:?}");
            (Vec::new(), Vec::new(), "the search failed")
        }
    }
}

/// A zone here started while ARC led: it carries on from ARC's song and
/// second (or it brought music of its own), and ARC is paused.
async fn take_over_from_arc(app: &AppHandle, client: &LibraryClient, run: &mut Run, arc: &crate::companion::ArcNow, starter: &ZoneView) {
    let seen = Instant::now();
    let on = title_of(starter);
    let second = arc.second_at(seen);
    println!(
        "[livequeue] \"{}\" started ({}, on \"{on}\") while ARC on \"{}\" leads ({}, on \"{}\" at {second:.0} s)",
        starter.name, starter.state, arc.name, arc.state, arc.title
    );
    crate::companion::command(&arc.zone_id, "pause");
    let held = run.holds.get(&starter.zone_id).cloned();
    let resumed = run.left_on.get(&starter.zone_id).is_some_and(|t| !t.is_empty() && same_text(t, &on));
    let ours = resumed || is_silent(&on) || same_text(&on, &arc.title) || matches!(&held, Some(Holds::Songs(t)) if t.first() == Some(&on));
    run.left_on.clear();
    run.leader = Some(starter.zone_id.clone());
    run.seen = None;
    run.holds.remove(&starter.zone_id);
    run.dirty = Some(Instant::now());
    run.retry = None;
    run.hold_off = Some(Instant::now() + AFTER_TAKEOVER);
    run.note = Some((
        if ours { format!("{} picked up from ARC on {} at {}", starter.name, arc.name, clock(second)) } else { format!("{} started music of its own and leads now", starter.name) },
        Instant::now() + NOTE_FOR,
    ));
    if !ours {
        println!("[livequeue] \"{}\" brought music of its own; it leads now", starter.name);
        return;
    }
    if arc.title.is_empty() {
        return;
    }
    let transport = match app.state::<Zones>().transport() {
        Ok(t) => t,
        Err(e) => {
            println!("[livequeue] {e}");
            return;
        }
    };
    if same_text(&on, &arc.title) {
        println!("[livequeue] pickup from ARC: \"{}\" is on the song, moving it to {second:.0} s", starter.name);
        land(app, &transport, starter, second, seen).await;
        return;
    }
    let (handles, _, how) = songs_for_arc(client, arc).await;
    if handles.is_empty() {
        println!("[livequeue] pickup from ARC: \"{}\" {how}; \"{}\" is left as it is", arc.title, starter.name);
        return;
    }
    let cleared = client.queue_clear(&starter.zone_id).await;
    let played = client.play_tracks_how(&starter.zone_id, &handles, PlayHow::Now).await;
    println!(
        "[livequeue] pickup from ARC: \"{}\" ({how}), {} song(s); queue cleared: {cleared:?}, played now: {played:?}, {} ms after the start was seen",
        arc.title,
        handles.len(),
        seen.elapsed().as_millis()
    );
    let waiting = Instant::now();
    while waiting.elapsed() < Duration::from_secs(6) {
        if zone(app, &starter.zone_id).is_some_and(|z| same_text(&title_of(&z), &arc.title) && going(z.state)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    match zone(app, &starter.zone_id) {
        Some(z) if same_text(&title_of(&z), &arc.title) => land(app, &transport, starter, second, seen).await,
        z => println!("[livequeue] pickup from ARC: \"{}\" never got to \"{}\" (it is on \"{}\")", starter.name, arc.title, z.as_ref().map(title_of).unwrap_or_default()),
    }
}

/// ARC leads: its song (and the ones after it, when it plays from the
/// Living Queue playlist) loaded into the mirrored zones here, and the hide
/// zones given the silent track once what they show isn't ARC's song. The
/// playlist is left as it is. False when it should be tried again.
async fn copy_from_arc(app: &AppHandle, client: &LibraryClient, run: &mut Run, arc: &crate::companion::ArcNow, members: &[&ZoneView]) -> bool {
    let began = Instant::now();
    let (handles, titles, how) = songs_for_arc(client, arc).await;
    if handles.is_empty() {
        println!("[livequeue] ARC's \"{}\" {how}; the zones here are left as they are", arc.title);
        return true;
    }
    let mut done = Vec::new();
    let mut waiting = false;
    for z in members {
        let Some(now) = zone(app, &z.zone_id) else { continue };
        if going(now.state) {
            continue;
        }
        let hide = run.config.zones.get(&z.zone_id).is_some_and(|m| m == "hide");
        let (give, holds) = if hide {
            let Some(silent) = run.silent.clone() else { continue };
            match run.holds.get(&z.zone_id) {
                Some(Holds::Silent) => continue,
                Some(Holds::Songs(held)) if held.first() == titles.first() => continue,
                _ => {}
            }
            // (one that still shows the song it was left in, ARC's song before this one, keeps it until now)
            if run.left_on.get(&z.zone_id).is_some_and(|t| same_text(t, &title_of(&now))) && same_text(&title_of(&now), &arc.title) {
                continue;
            }
            (vec![silent], Holds::Silent)
        } else {
            // (already holding these songs, as far as both lists go: the zone ARC took over from, say)
            let holding = matches!(run.holds.get(&z.zone_id), Some(Holds::Songs(held)) if !held.is_empty() && held.iter().zip(&titles).all(|(a, b)| same_text(a, b)));
            if holding || same_text(&title_of(&now), &arc.title) && titles.len() == 1 {
                continue;
            }
            (handles.clone(), Holds::Songs(titles.clone()))
        };
        if run.loaded.get(&z.zone_id).is_some_and(|at| at.elapsed() < BETWEEN_LOADS) {
            waiting = true;
            continue;
        }
        run.loaded.insert(z.zone_id.clone(), Instant::now());
        if let Err(e) = client.queue_clear(&z.zone_id).await {
            println!("[livequeue] the queue of \"{}\" could not be cleared: {e:?}", z.name);
            continue;
        }
        match client.play_tracks_how(&z.zone_id, &give, PlayHow::Queue).await {
            Ok(()) => {
                done.push(format!("\"{}\" ({})", z.name, if hide { "the silent track".to_string() } else { format!("{} songs", give.len()) }));
                run.holds.insert(z.zone_id.clone(), holds);
            }
            Err(e) => println!("[livequeue] the songs could not be added to the queue of \"{}\": {e:?}", z.name),
        }
    }
    println!(
        "[livequeue] ARC on \"{}\" is on \"{}\" ({how}), {} song(s) in {} ms; zones: {}",
        arc.name,
        arc.title,
        handles.len(),
        began.elapsed().as_millis(),
        if done.is_empty() { "none to load".to_string() } else { done.join(", ") },
    );
    !waiting
}
