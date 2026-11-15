// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The living queue's silent track ("Living Queue", by Roon: Toasted): a
//! few seconds of digital silence that stands first in the "Living Queue"
//! playlist, is what a hide zone shows, and is what ARC is started on.
//!
//! Toasted makes the file itself and puts it where Roon keeps music. The
//! Core tells its storage folders as it reaches them
//! ("\\\\192.168.0.18\\roon music"); the first one this computer can write
//! to gets `Roon Toasted\Living Queue\Toasted - Living Queue.flac`. Roon
//! then finds it on its own (it watches its folders), the track is looked
//! up by its title and artist, and the playlist is made with it first.
//!
//! When no folder of Roon's can be written from here (the music is on the
//! Core's own disk, or on a share this computer isn't signed in to), the
//! file is left in Toasted's data folder and the Living Queue page asks for
//! one of two things: the music folder, chosen once, or the file copied
//! there by hand. Until Roon has the track, Hide Current (and ARC, which
//! starts on it) can't be used; Mirrored needs none of it.
//!
//! Silence needs no encoder: every FLAC frame is two "constant" subframes
//! holding the value 0.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use roon_library::LibraryClient;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::library::Library;

pub const TITLE: &str = "Living Queue";
pub const ARTIST: &str = "Roon: Toasted";
pub const PLAYLIST: &str = "Living Queue";
const FOLDERS: [&str; 2] = ["Roon Toasted", "Living Queue"];
const FILE: &str = "Toasted - Living Queue.flac";
const COVER: &[u8] = include_bytes!("../icons/living-queue.jpg");

const RATE: u64 = 44100;
const CHANNELS: u64 = 2;
const BITS: u64 = 16;
const BLOCK: u32 = 4096;
const SECONDS: u64 = 13;
/// The MD5 of the audio (13 s of zeros, 16 bits, stereo), as FLAC keeps it.
const AUDIO_MD5: [u8; 16] = [0x68, 0xd3, 0x9d, 0xd0, 0xbd, 0x5b, 0x4f, 0x86, 0xf8, 0x1d, 0x3b, 0x84, 0x25, 0x44, 0xfa, 0xef];

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

/// A frame number the way FLAC writes it (as UTF-8 writes a character).
fn utf8_number(n: u32) -> Vec<u8> {
    if n < 0x80 {
        vec![n as u8]
    } else if n < 0x800 {
        vec![0xC0 | (n >> 6) as u8, 0x80 | (n & 0x3F) as u8]
    } else {
        vec![0xE0 | (n >> 12) as u8, 0x80 | ((n >> 6) & 0x3F) as u8, 0x80 | (n & 0x3F) as u8]
    }
}

fn frame(number: u32, samples: u32) -> Vec<u8> {
    let mut out = vec![0xFF, 0xF8];
    // (block size: 4096 has a code of its own; any other is written out after the frame number. 0x9: 44.1 kHz)
    out.push((if samples == BLOCK { 0xC0 } else { 0x70 }) | 0x09);
    out.push(0x18); // two independent channels, 16 bits
    out.extend(utf8_number(number));
    if samples != BLOCK {
        out.extend(((samples - 1) as u16).to_be_bytes());
    }
    out.push(crc8(&out));
    // (per channel: a constant subframe, value 0)
    out.extend([0u8; 6]);
    let crc = crc16(&out);
    out.extend(crc.to_be_bytes());
    out
}

fn block(kind: u8, data: &[u8], last: bool) -> Vec<u8> {
    let mut out = vec![(if last { 0x80 } else { 0 }) | kind];
    out.extend(&(data.len() as u32).to_be_bytes()[1..]);
    out.extend(data);
    out
}

/// A JPEG's width and height, off its frame header.
fn jpeg_size(data: &[u8]) -> (u32, u32) {
    let mut i = 2;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return (width, height);
        }
        i += 2 + u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
    }
    (0, 0)
}

/// The whole file: the silence, its tags and the cover.
pub fn flac(cover: &[u8]) -> Vec<u8> {
    let total = SECONDS * RATE;
    let mut frames = Vec::new();
    let (mut number, mut left) = (0u32, total);
    while left > 0 {
        let n = left.min(BLOCK as u64) as u32;
        frames.push(frame(number, n));
        number += 1;
        left -= n as u64;
    }
    let smallest = frames.iter().map(Vec::len).min().unwrap_or(0) as u32;
    let largest = frames.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let mut info = Vec::new();
    info.extend((BLOCK as u16).to_be_bytes());
    info.extend((BLOCK as u16).to_be_bytes());
    info.extend(&smallest.to_be_bytes()[1..]);
    info.extend(&largest.to_be_bytes()[1..]);
    info.extend(((RATE << 44) | ((CHANNELS - 1) << 41) | ((BITS - 1) << 36) | total).to_be_bytes());
    info.extend(AUDIO_MD5);

    let tags = [
        ("TITLE", TITLE),
        ("ARTIST", ARTIST),
        ("ALBUM", TITLE),
        ("ALBUMARTIST", ARTIST),
        ("TRACKNUMBER", "1"),
        ("DATE", "2026"),
        ("GENRE", "Utility"),
    ];
    let vendor = b"Roon: Toasted";
    let mut comments = Vec::new();
    comments.extend((vendor.len() as u32).to_le_bytes());
    comments.extend(vendor);
    comments.extend((tags.len() as u32).to_le_bytes());
    for (key, value) in tags {
        let entry = format!("{key}={value}");
        comments.extend((entry.len() as u32).to_le_bytes());
        comments.extend(entry.as_bytes());
    }

    let (width, height) = jpeg_size(cover);
    let mime = b"image/jpeg";
    let mut picture = Vec::new();
    picture.extend(3u32.to_be_bytes()); // the front cover
    picture.extend((mime.len() as u32).to_be_bytes());
    picture.extend(mime);
    picture.extend(0u32.to_be_bytes());
    for n in [width, height, 24, 0] {
        picture.extend(n.to_be_bytes());
    }
    picture.extend((cover.len() as u32).to_be_bytes());
    picture.extend(cover);

    let mut out = b"fLaC".to_vec();
    out.extend(block(0, &info, false));
    out.extend(block(4, &comments, false));
    out.extend(block(6, &picture, true));
    for f in frames {
        out.extend(f);
    }
    out
}

fn is_ours(title: &str, artist: &str) -> bool {
    title.trim().eq_ignore_ascii_case(TITLE) && artist.to_lowercase().contains("toasted")
}

/// Where the track stands, for the Living Queue pages.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackStatus {
    /// "unknown" (not looked at yet), "working", "ready" (in the library
    /// and first in the playlist) or "needed" (the user has to help).
    pub state: String,
    /// What is going on, or what to do, in plain words.
    pub message: String,
    /// "needed": the file, in Toasted's data folder, for copying by hand.
    pub file: String,
}

static STATUS: Mutex<Option<TrackStatus>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn status() -> TrackStatus {
    STATUS.lock().unwrap().clone().unwrap_or_else(|| TrackStatus { state: "unknown".into(), ..Default::default() })
}

fn tell(app: &AppHandle, state: &str, message: impl Into<String>, file: &str) {
    let now = TrackStatus { state: state.into(), message: message.into(), file: file.into() };
    *STATUS.lock().unwrap() = Some(now.clone());
    let _ = app.emit("living-queue-track", now);
}

/// The track in the library, when Roon has it.
async fn in_library(client: &LibraryClient) -> Option<String> {
    let hits = client.search(TITLE, 50).await.ok()?;
    hits.tracks.into_iter().find(|t| t.in_library && is_ours(&t.title, &t.artist)).map(|t| t.handle)
}

/// Writes the file under `root` (unless it is there already); where it is.
fn place(root: &Path) -> std::io::Result<PathBuf> {
    let mut dir = root.to_path_buf();
    for part in FOLDERS {
        dir.push(part);
    }
    let path = dir.join(FILE);
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, flac(COVER))?;
    Ok(path)
}

/// The file in Toasted's own data folder, for the user to copy.
fn spare(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?.join("Living Queue");
    let path = dir.join(FILE);
    if !path.is_file() {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, flac(COVER)).ok()?;
    }
    Some(path)
}

/// The shares Windows is connected to right now ("\\\\SERVER\\Share"), as
/// `net use` lists them. A share the Core reaches by its address is often
/// one this computer is signed in to under the server's name.
#[cfg(windows)]
fn connected_shares() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new("net").arg("use").creation_flags(CREATE_NO_WINDOW).output() else { return Vec::new() };
    shares_in(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
fn connected_shares() -> Vec<String> {
    Vec::new()
}

/// The remote names in `net use`'s list: from the "\\\\" on each line to
/// its end, without the network's name that follows a short one.
fn shares_in(listing: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in listing.lines() {
        let Some(at) = line.find("\\\\") else { continue };
        let mut remote = line[at..].trim_end();
        for network in ["Microsoft Windows Network", "Web Client Network"] {
            remote = remote.strip_suffix(network).unwrap_or(remote).trim_end();
        }
        if remote.len() > 2 && !out.iter().any(|r| r.eq_ignore_ascii_case(remote)) {
            out.push(remote.to_string());
        }
    }
    out
}

/// "\\\\server\\share\\and\\more" as its share's name and what follows it.
fn share_of(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix("\\\\")?;
    let mut parts = rest.splitn(3, '\\');
    let _server = parts.next()?;
    let share = parts.next().filter(|s| !s.is_empty())?;
    Some((share.to_lowercase(), parts.next().unwrap_or("").to_string()))
}

/// Makes sure the silent track is in the library and first in the
/// "Living Queue" playlist: the file written into one of the Core's music
/// folders when Roon doesn't have the track, Roon waited for (and asked to
/// rescan), the playlist made (or one that is there set right). Says what
/// it did; an error says, in the user's words, what is in the way.
async fn ensure(app: &AppHandle, client: &LibraryClient) -> Result<String, String> {
    let mut did = Vec::new();
    let mut handle = in_library(client).await;
    if handle.is_none() {
        let folders: Vec<_> = client.storage_folders().into_iter().filter(|f| f.online).collect();
        // Where to try, in order: the Core's music folders as it reaches them; the same shares under the
        // names Windows is connected to them by; the folder chosen on the Living Queue page; the one
        // picked in Wrapped's Settings
        let mut roots: Vec<(String, PathBuf)> = folders.iter().map(|f| (f.name.clone(), PathBuf::from(&f.path))).collect();
        let shares = tokio::task::spawn_blocking(connected_shares).await.unwrap_or_default();
        for f in &folders {
            let Some((share, rest)) = share_of(&f.path) else { continue };
            for known in &shares {
                if share_of(known).is_some_and(|(s, _)| s == share) && !known.eq_ignore_ascii_case(&f.path) {
                    let path = if rest.is_empty() { PathBuf::from(known) } else { Path::new(known).join(&rest) };
                    roots.push((format!("{} as Windows knows it", f.name), path));
                }
            }
        }
        let settings = crate::settings::path(app).map(|p| crate::settings::load(&p)).unwrap_or_default();
        if !settings.living_queue_folder.trim().is_empty() {
            roots.push(("the folder chosen on the Living Queue page".into(), PathBuf::from(settings.living_queue_folder.trim())));
        }
        let picked = crate::wrapped::WrappedSettings::load(app).music_folder;
        if !picked.trim().is_empty() {
            roots.push(("the music folder picked in Wrapped's Settings".into(), PathBuf::from(picked)));
        }
        let mut placed = None;
        let mut failures = Vec::new();
        for (name, root) in roots {
            let tried = root.clone();
            match tokio::task::spawn_blocking(move || place(&tried)).await {
                Ok(Ok(path)) => {
                    placed = Some(path);
                    break;
                }
                Ok(Err(e)) => failures.push(format!("\"{name}\" ({}): {e}", root.display())),
                Err(e) => failures.push(format!("\"{name}\": {e}")),
            }
        }
        if !failures.is_empty() {
            did.push(format!("not writable from here: {}", failures.join("; ")));
        }
        match &placed {
            Some(path) => did.push(format!("the file is at {}", path.display())),
            None => did.push("the file could not be put in any music folder".into()),
        }
        println!("[livequeue] set up: {}; waiting for Roon to find it", did.join("; "));
        // Roon finds a new file on its own (it watches its folders), or after being told to scan them, as
        // some network shares need. With nothing placed, the user may have copied the file in by hand:
        // the scan is asked for at once, and waited for a shorter while.
        let began = std::time::Instant::now();
        let (scan_after, give_up) = if placed.is_some() { (Duration::from_secs(20), Duration::from_secs(150)) } else { (Duration::ZERO, Duration::from_secs(45)) };
        let mut scanned = false;
        while handle.is_none() && began.elapsed() < give_up {
            if !scanned && began.elapsed() >= scan_after {
                scanned = true;
                for f in &folders {
                    let scan = client.force_scan(&f.handle).await;
                    println!("[livequeue] set up: rescan of \"{}\" asked: {scan:?}", f.name);
                }
                did.push("a rescan was asked".into());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
            handle = in_library(client).await;
        }
        match (&handle, &placed) {
            (Some(_), _) => did.push(format!("Roon had it after {} s", began.elapsed().as_secs())),
            (None, Some(path)) => {
                return Err(format!(
                    "Toasted put the Living Queue track at {}, but Roon hasn't picked it up. Check that this folder is one of Roon's music folders (Roon > Settings > Storage), then press Check again.",
                    path.display()
                ))
            }
            (None, None) => {
                return Err(if folders.is_empty() {
                    "Roon has no music folder set up (Roon > Settings > Storage). Hide Current needs a small silent track in your Roon library: add a music folder in Roon, then press Check again.".into()
                } else {
                    "Hide Current needs a small silent track in your Roon library, and Toasted can't reach Roon's music folder from this computer to put it there. Choose your Roon music folder, or copy the file below into it yourself, then press Check again.".into()
                })
            }
        }
    } else {
        did.push("the track was in the library already".into());
    }
    let Some(handle) = handle else { return Err("The Living Queue track was not found.".into()) };

    // The playlist: the track first
    let lists = client.playlists().map_err(|e| format!("Roon's playlists could not be listed ({e:?}). Press Check again."))?;
    match lists.into_iter().find(|p| p.name.trim().eq_ignore_ascii_case(PLAYLIST) && !p.read_only) {
        None => {
            client.create_playlist(PLAYLIST, &[handle]).await.map_err(|e| format!("The Living Queue playlist could not be made ({e:?}). Press Check again."))?;
            did.push("the playlist was made".into());
        }
        Some(list) => {
            let entries = client.playlist_tracks(&list.handle).await.map_err(|e| format!("The Living Queue playlist could not be read ({e:?}). Press Check again."))?;
            if entries.first().is_some_and(|e| is_ours(&e.entry.track.title, &e.entry.track.artist)) {
                did.push("the playlist had it first already".into());
            } else {
                // (songs can only be added at the end: everything out, the track in; the songs come back at the next copy)
                let items: Vec<String> = entries.iter().map(|e| e.item_handle.clone()).collect();
                if !items.is_empty() {
                    client.remove_playlist_items(&list.handle, &items).await.map_err(|e| format!("The Living Queue playlist could not be emptied ({e:?}). Press Check again."))?;
                    for _ in 0..15 {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        if client.playlist_tracks(&list.handle).await.is_ok_and(|left| left.is_empty()) {
                            break;
                        }
                    }
                }
                client.add_to_playlist(&list.handle, &[handle]).await.map_err(|e| format!("The track could not be added to the Living Queue playlist ({e:?}). Press Check again."))?;
                did.push(format!("the playlist was emptied of {} song(s) and the track put first", items.len()));
            }
        }
    }
    Ok(did.join("; "))
}

/// Looks for the track and sets it up where it is missing, in the
/// background (Roon can take a minute over a new file). False when that
/// is under way already, or the library link is down.
pub fn start(app: &AppHandle) -> bool {
    let Some(client) = app.state::<Library>().client() else { return false };
    if RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    tell(app, "working", "Setting up the Living Queue track. This can take a minute.", "");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // (dev build: TOASTED_LQ_NOTRACK=1 acts out a computer that can't reach Roon's music folder, to see the page)
        let outcome = if cfg!(debug_assertions) && std::env::var("TOASTED_LQ_NOTRACK").is_ok() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Err("Hide Current needs a small silent track in your Roon library, and Toasted can't reach Roon's music folder from this computer to put it there. Choose your Roon music folder, or copy the file below into it yourself, then press Check again.".to_string())
        } else {
            ensure(&app, &client).await
        };
        match outcome {
            Ok(did) => {
                println!("[livequeue] set up: {did}");
                tell(&app, "ready", "", "");
            }
            Err(e) => {
                println!("[livequeue] set up failed: {e}");
                let file = spare(&app).map(|p| p.display().to_string()).unwrap_or_default();
                tell(&app, "needed", e, &file);
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
        crate::livequeue::setup_finished();
    });
    true
}

/// Where the track stands (the Living Queue pages).
#[tauri::command]
pub fn living_queue_track_status() -> TrackStatus {
    status()
}

/// "Check again": looks for the track once more, with a rescan.
#[tauri::command]
pub fn living_queue_track_check(app: AppHandle) -> TrackStatus {
    start(&app);
    status()
}

/// "Choose music folder": Windows' folder picker; the folder is kept and
/// the track put in it.
#[tauri::command]
pub async fn living_queue_track_pick(app: AppHandle) -> Result<TrackStatus, String> {
    let chosen = tauri::async_runtime::spawn_blocking(crate::musicfolder::pick_folder).await.map_err(|e| e.to_string())??;
    if let Some(folder) = chosen {
        crate::settings::set_living_queue_folder(&app, folder)?;
        start(&app);
    }
    Ok(status())
}

/// "Show the file": Explorer, on the copy in Toasted's data folder.
#[tauri::command]
pub fn living_queue_track_reveal(app: AppHandle) -> Result<(), String> {
    let path = spare(&app).ok_or("The file could not be written to Toasted's data folder.")?;
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())
}
