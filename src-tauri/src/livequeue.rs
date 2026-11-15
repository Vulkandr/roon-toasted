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
use tauri::{AppHandle, Manager};

use crate::library::Library;
use crate::settings::AppSettings;
use crate::zones::{ZoneView, Zones};

/// How many songs, from the playing one on, a mirrored zone and the
/// playlist are given.
const SONGS: usize = 25;
/// What the making of the silent track and its playlist (silenttrack.rs)
/// is at: not asked for, under way, or done (well or not).
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
    /// Nothing is copied before this (see AFTER_TAKEOVER).
    hold_off: Option<Instant>,
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
        loop {
            tick.tick().await;
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
                continue;
            }
            let Some(client) = app.state::<Library>().client() else {
                // (the link is down, the Core restarting perhaps: everything is looked at afresh after it)
                if run.config.on {
                    println!("[livequeue] the library link is down; waiting for it");
                    run = Run::default();
                }
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
        }
    });
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

    // The leader: kept while its zone is there; without one, the zone that is playing
    if run.leader.as_ref().is_some_and(|id| !members.iter().any(|z| &z.zone_id == id)) {
        println!("[livequeue] the leading zone is gone");
        run.leader = None;
        run.seen = None;
    }
    if run.leader.is_none() {
        if let Some(z) = members.iter().find(|z| z.state == "playing" && !is_silent(&title_of(z))) {
            println!("[livequeue] \"{}\" leads, on \"{}\"", z.name, title_of(z));
            run.leader = Some(z.zone_id.clone());
            run.seen = None;
        }
    }
    let Some(leader_id) = run.leader.clone() else { return };
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
    let seen = (title, leader.queue_items_remaining);
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
}

/// The leader's songs, the playing one first; none when the queue hasn't
/// caught up with what the zone says is playing.
async fn songs_of(client: &LibraryClient, leader: &ZoneView) -> Vec<QueueSong> {
    let songs = match client.queue_songs(&leader.zone_id, None, 0).await {
        Ok(songs) => songs,
        Err(e) => {
            println!("[livequeue] the queue of \"{}\" could not be read: {e:?}", leader.name);
            return Vec::new();
        }
    };
    let ahead: Vec<QueueSong> = songs.into_iter().filter(|s| !s.played && !s.skipped).take(SONGS).collect();
    match ahead.first() {
        Some(first) if first.title == title_of(leader) => ahead,
        Some(first) => {
            println!("[livequeue] the queue of \"{}\" starts at \"{}\", not at \"{}\" yet", leader.name, first.title, title_of(leader));
            Vec::new()
        }
        None => Vec::new(),
    }
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
        "[livequeue] \"{}\" is on \"{first}\", {} songs copied in {} ms; zones: {}; playlist: {playlist}",
        leader.name,
        handles.len(),
        began.elapsed().as_millis(),
        if done.is_empty() { "none to load".to_string() } else { done.join(", ") },
    );
    // (a zone loaded only moments ago gets its turn at the next try)
    !waiting
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
                }
                z => println!("[livequeue] pickup: \"{}\" never got to \"{leading}\" (it is on \"{}\")", starter.name, z.as_ref().map(title_of).unwrap_or_default()),
            }
        }
    } else {
        println!("[livequeue] \"{}\" brought music of its own; it leads now", starter.name);
    }
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
