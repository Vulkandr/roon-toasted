// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tier 3's own music folder ("Manually choose music folder" in Wrapped's
//! Settings): for songs Roon can't hand over (its web server out of reach,
//! a Core that doesn't export, music Roon doesn't have), tier 3 looks for
//! the song's file in a folder the user picks, before falling back to a
//! 30-second preview. The folder is read into an index (the audio files
//! tier 3 can decode, by their names) when it's picked and again when it's
//! an hour old; a song is found by the end of the Core's path for it when
//! there is one ("Artist/Album/03 - Song.flac"), else by its title in the
//! file names with its artist (and album) in the path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::discover::fold;
use crate::sound::base_title;
use crate::wrapped::WrappedSettings;

/// The index is read again when it's this old.
const FRESH_FOR: Duration = Duration::from_secs(60 * 60);
/// At most this many files are indexed (a whole disk picked by mistake).
const MOST_FILES: usize = 400_000;

struct Entry {
    path: PathBuf,
    /// Its folders below the picked one, folded, joined by "/".
    dirs: String,
    /// The file name without its type, folded (track number and all).
    stem: String,
}

struct Index {
    root: String,
    at: Instant,
    files: Vec<Entry>,
    /// Song titles read off the file names (folded, also without versions)
    /// to the files.
    by_title: HashMap<String, Vec<usize>>,
    /// The last two parts of each path ("album/file"), folded.
    by_tail: HashMap<String, usize>,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);
static SCANNING: AtomicBool = AtomicBool::new(false);

/// What Wrapped's Settings shows.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderStatus {
    pub folder: String,
    /// Audio files found in it (at the last read).
    pub files: usize,
    pub scanning: bool,
    pub error: String,
}

fn status(app: &AppHandle) -> FolderStatus {
    let folder = WrappedSettings::load(app).music_folder;
    let files = INDEX.lock().unwrap().as_ref().filter(|i| i.root == folder).map(|i| i.files.len()).unwrap_or(0);
    FolderStatus { folder, files, scanning: SCANNING.load(Ordering::SeqCst), error: String::new() }
}

/// A song title as file names carry it: the track number and the artist in
/// front taken off ("03 - Hero", "1-03 Hero", "Bölzer - Hero" all "hero").
fn titles_in(stem: &str) -> Vec<String> {
    let mut out = Vec::new();
    let s = stem.trim();
    // (a leading track number, with a disc number and separators)
    let without_number = if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '-' | '.' | '_' | ' ' | ')' | '('))
    } else {
        s
    };
    for t in [s, without_number] {
        out.push(fold(t));
        out.push(fold(&base_title(t)));
        if let Some((_, after)) = t.rsplit_once(" - ") {
            out.push(fold(after));
            out.push(fold(&base_title(after)));
        }
    }
    out.retain(|t| !t.is_empty());
    out.sort();
    out.dedup();
    out
}

fn tail_key(parts: &[&str]) -> String {
    parts.iter().map(|p| fold(p)).collect::<Vec<_>>().join("/")
}

/// Reads the folder (every audio file tier 3 can hear, in every folder
/// below it).
fn scan(root: &str) -> Index {
    let mut files = Vec::new();
    let mut stack = vec![PathBuf::from(root)];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for item in read.flatten() {
            let path = item.path();
            let name = item.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = item.file_type() else { continue };
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            let Some((stem, ext)) = name.rsplit_once('.') else { continue };
            if !crate::audio::can_decode(ext) {
                continue;
            }
            let dirs = path
                .parent()
                .and_then(|p| p.strip_prefix(root).ok())
                .map(|p| p.components().map(|c| fold(&c.as_os_str().to_string_lossy())).collect::<Vec<_>>().join("/"))
                .unwrap_or_default();
            files.push(Entry { path, dirs, stem: stem.to_string() });
            if files.len() >= MOST_FILES {
                break;
            }
        }
        if files.len() >= MOST_FILES {
            break;
        }
    }
    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_tail: HashMap<String, usize> = HashMap::new();
    for (i, e) in files.iter().enumerate() {
        for t in titles_in(&e.stem) {
            by_title.entry(t).or_default().push(i);
        }
        let album = e.dirs.rsplit('/').next().unwrap_or("");
        let file = e.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        by_tail.entry(format!("{album}/{}", fold(&file))).or_insert(i);
    }
    let files = files.into_iter().map(|mut e| {
        e.stem = fold(&e.stem);
        e
    });
    Index { root: root.to_string(), at: Instant::now(), files: files.collect(), by_title, by_tail }
}

/// The index of the folder, read now when it's missing, of another folder
/// or old. Blocking (a big folder on a network share takes a while).
fn ensure(root: &str) {
    let fresh = INDEX.lock().unwrap().as_ref().is_some_and(|i| i.root == root && i.at.elapsed() < FRESH_FOR);
    if fresh || root.is_empty() {
        return;
    }
    SCANNING.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let index = scan(root);
    println!("[sound] music folder {root}: {} audio files in {} s", index.files.len(), started.elapsed().as_secs());
    *INDEX.lock().unwrap() = Some(index);
    SCANNING.store(false, Ordering::SeqCst);
}

/// The folder, read (in the background when needed) so `find` can look in
/// it; None when there's no folder.
pub async fn ready(app: &AppHandle) -> Option<String> {
    let root = WrappedSettings::load(app).music_folder;
    if root.is_empty() {
        return None;
    }
    let r = root.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || ensure(&r)).await;
    Some(root)
}

/// A song's file in the folder: by the end of the Core's path for it
/// (`core_tail`, "Artist/Album/03 - Song.flac") when given, else by its
/// title in a file name with its artist in the path or the name (its album
/// counting too).
pub fn find(artist: &str, title: &str, album: &str, core_tail: Option<&str>) -> Option<PathBuf> {
    let guard = INDEX.lock().unwrap();
    let index = guard.as_ref()?;
    if let Some(tail) = core_tail {
        let parts: Vec<&str> = tail.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 2 {
            if let Some(i) = index.by_tail.get(&tail_key(&parts[parts.len() - 2..])) {
                return Some(index.files[*i].path.clone());
            }
        }
    }
    let first = artist.split(" / ").next().unwrap_or(artist).split(", ").next().unwrap_or(artist);
    let (artist, album, base_album) = (fold(first), fold(album), fold(&base_title(album)));
    if artist.is_empty() {
        return None;
    }
    let mut best: Option<(i32, usize)> = None;
    let mut seen = Vec::new();
    for key in [fold(title), fold(&base_title(title))] {
        for &i in index.by_title.get(&key).into_iter().flatten() {
            if seen.contains(&i) {
                continue;
            }
            seen.push(i);
            let e = &index.files[i];
            if !(e.dirs.contains(&artist) || e.stem.contains(&artist)) {
                continue;
            }
            let mut score = if key == fold(title) { 2 } else { 1 };
            if !album.is_empty() && (e.dirs.contains(&album) || (!base_album.is_empty() && e.dirs.contains(&base_album))) {
                score += 3;
            }
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, i));
            }
        }
    }
    best.map(|(_, i)| index.files[i].path.clone())
}

/// "Manually choose music folder": Windows' folder picker; the folder
/// chosen is kept in Wrapped's settings and read in the background.
#[tauri::command]
pub async fn wrapped_music_folder_pick(app: AppHandle) -> Result<FolderStatus, String> {
    let chosen = tauri::async_runtime::spawn_blocking(pick_folder).await.map_err(|e| e.to_string())??;
    let Some(folder) = chosen else {
        return Ok(status(&app));
    };
    set(&app, folder)
}

/// Forgets the folder.
#[tauri::command]
pub fn wrapped_music_folder_clear(app: AppHandle) -> Result<FolderStatus, String> {
    set(&app, String::new())
}

/// Reads the folder again now.
#[tauri::command]
pub fn wrapped_music_folder_rescan(app: AppHandle) -> FolderStatus {
    *INDEX.lock().unwrap() = None;
    read_in_background(&app);
    status(&app)
}

#[tauri::command]
pub fn wrapped_music_folder_status(app: AppHandle) -> FolderStatus {
    status(&app)
}

fn set(app: &AppHandle, folder: String) -> Result<FolderStatus, String> {
    // (through the settings' own update, so the pages hear of it)
    crate::wrapped::wrapped_settings_update(app.clone(), serde_json::json!({ "musicFolder": folder }))?;
    *INDEX.lock().unwrap() = None;
    read_in_background(app);
    Ok(status(app))
}

fn read_in_background(app: &AppHandle) {
    let app = app.clone();
    let root = WrappedSettings::load(&app).music_folder;
    if root.is_empty() {
        let _ = app.emit("music-folder", status(&app));
        return;
    }
    SCANNING.store(true, Ordering::SeqCst);
    let _ = app.emit("music-folder", status(&app));
    tauri::async_runtime::spawn_blocking(move || {
        ensure(&root);
        let _ = app.emit("music-folder", status(&app));
    });
}

/// Windows' folder picker (IFileOpenDialog with FOS_PICKFOLDERS); None
/// when cancelled.
#[cfg(windows)]
fn pick_folder() -> Result<Option<String>, String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};
    // SAFETY: COM on this (blocking pool) thread for the dialog's lifetime;
    // the path string is freed with CoTaskMemFree as the API asks.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> Result<Option<String>, String> {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
            let options = dialog.GetOptions().map_err(|e| e.to_string())?;
            dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).map_err(|e| e.to_string())?;
            let title: Vec<u16> = "Choose your music folder".encode_utf16().chain(std::iter::once(0)).collect();
            let _ = dialog.SetTitle(windows::core::PCWSTR(title.as_ptr()));
            if dialog.Show(None).is_err() {
                return Ok(None); // cancelled
            }
            let item = dialog.GetResult().map_err(|e| e.to_string())?;
            let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| e.to_string())?;
            let path = name.to_string().map_err(|e| e.to_string());
            CoTaskMemFree(Some(name.0 as *const _));
            path.map(Some)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
fn pick_folder() -> Result<Option<String>, String> {
    Err("Choosing a folder works on Windows only.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_from_file_names() {
        assert!(titles_in("03 - Hero").contains(&"hero".to_string()));
        assert!(titles_in("1-03 Hero").contains(&"hero".to_string()));
        assert!(titles_in("Bölzer - Hero").contains(&"hero".to_string()));
        assert!(titles_in("07. The Archer (Remastered)").contains(&"thearcher".to_string()));
        // (a title that starts with a number keeps it too)
        assert!(titles_in("1979").contains(&"1979".to_string()));
    }

    #[test]
    fn finds_in_a_folder() {
        let root = std::env::temp_dir().join(format!("rt-music-{}", std::process::id()));
        let album = root.join("Bölzer").join("2016 - Hero");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("03 - Hero.flac"), b"x").unwrap();
        std::fs::write(album.join("04 - The Archer.flac"), b"x").unwrap();
        std::fs::write(album.join("cover.jpg"), b"x").unwrap();
        let other = root.join("Someone Else").join("Hero");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("01 Hero.mp3"), b"x").unwrap();
        let r = root.to_string_lossy().to_string();
        *INDEX.lock().unwrap() = None;
        ensure(&r);
        assert_eq!(INDEX.lock().unwrap().as_ref().unwrap().files.len(), 3);
        let hero = find("Bölzer", "Hero", "Hero", None).unwrap();
        assert!(hero.ends_with("03 - Hero.flac"));
        assert!(find("Bolzer", "The Archer", "", None).unwrap().ends_with("04 - The Archer.flac"));
        assert!(find("Nobody", "Hero", "", None).is_none());
        // By the Core's path, whatever the names say
        assert!(find("x", "y", "", Some("Bölzer/2016 - Hero/04 - The Archer.flac")).unwrap().ends_with("04 - The Archer.flac"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
