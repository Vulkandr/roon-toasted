// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Pictures from outside Roon, for the artists Roon has none of: Deezer's
//! (its public API, no account needed). The page asks only once all of
//! Roon's own pictures of an artist have failed to load.
//!
//! An artist is matched by name, and then only when that Deezer artist has
//! an album or a top song by the same title as one of the library's (so a
//! band sharing a name with another doesn't get its picture). A name only
//! Deezer has once and that's long enough to be its own (12 characters or
//! more) goes without that check. Answers, found or not, are kept in
//! wrapped-artist-pictures.json: a picture for good, a miss for 30 days.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::discover::fold;
use crate::wrapped::{now_ms, read_json, write_json, LibraryCopy};

const USER_AGENT: &str = concat!("RoonToasted/", env!("CARGO_PKG_VERSION"), " (https://github.com/Vulkandr/roon-toasted)");
/// How long a miss is kept before Deezer is asked again.
const MISS_DAYS: i64 = 30;
/// A name Deezer has only once is taken without an album check from this
/// long on.
const DISTINCT_NAME: usize = 12;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Found {
    /// The picture's address on Deezer's servers, or none found.
    url: Option<String>,
    deezer_id: Option<u64>,
    checked_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    /// Keyed by the artist's name, folded (see discover::fold).
    artists: HashMap<String, Found>,
}

#[derive(Default)]
pub struct Pictures {
    store: Mutex<Option<Store>>,
    /// One artist looked up at a time.
    looking: tokio::sync::Mutex<()>,
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-artist-pictures.json"))
}

impl Pictures {
    fn with_store<T>(&self, app: &AppHandle, f: impl FnOnce(&mut Store) -> T) -> T {
        let mut guard = self.store.lock().unwrap();
        let store = guard.get_or_insert_with(|| path(app).and_then(|p| read_json(&p)).unwrap_or_default());
        f(store)
    }

    fn known(&self, app: &AppHandle, key: &str) -> Option<Option<String>> {
        let mut found = self.with_store(app, |s| s.artists.get(key).cloned())?;
        // (kept before the grey figure was told apart: a miss)
        found.url = found.url.filter(|url| real_picture(url));
        if found.url.is_some() || now_ms() - found.checked_at < MISS_DAYS * 24 * 3600 * 1000 {
            Some(found.url)
        } else {
            None
        }
    }

    fn keep(&self, app: &AppHandle, key: String, found: Found) {
        self.with_store(app, |s| {
            s.artists.insert(key, found);
        });
        if let Some(path) = path(app) {
            self.with_store(app, |s| write_json(&path, s));
        }
    }
}

/// A title as it's compared: what's in brackets taken off ("(Deluxe
/// Edition)", "[Remastered]"), then folded.
fn title_key(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    fold(&out)
}

/// The picture of an artist Roon has none of, from Deezer: its address, or
/// nothing when Deezer has no match (see the module's notes).
#[tauri::command]
pub async fn wrapped_artist_picture(app: AppHandle, name: String) -> Option<String> {
    let key = fold(&name);
    if key.is_empty() {
        return None;
    }
    let pictures = app.state::<Pictures>();
    if let Some(known) = pictures.known(&app, &key) {
        return known;
    }
    let _turn = pictures.looking.lock().await;
    // (asked meanwhile, by another tile of the same artist)
    if let Some(known) = pictures.known(&app, &key) {
        return known;
    }
    let found = look_up(&app, &name, &key).await;
    match found {
        Ok(found) => {
            let url = found.url.clone();
            println!("[pictures] {name}: {}", if url.is_some() { "found on Deezer" } else { "none on Deezer" });
            pictures.keep(&app, key, found);
            url
        }
        // (Deezer unreachable or busy: not kept, so it's asked again)
        Err(e) => {
            println!("[pictures] {name}: {e}");
            None
        }
    }
}

async fn look_up(app: &AppHandle, name: &str, key: &str) -> Result<Found, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| e.to_string())?;
    let sound = app.state::<crate::sound::Sound>();
    let miss = Found { url: None, deezer_id: None, checked_at: now_ms() };
    let found = sound.deezer(&client, "https://api.deezer.com/search/artist", &[("q", name), ("limit", "10")]).await?;
    let same: Vec<&Value> = found["data"].as_array().map(|a| a.iter().filter(|c| c["name"].as_str().map(fold).as_deref() == Some(key)).collect()).unwrap_or_default();
    if same.is_empty() {
        return Ok(miss);
    }
    // What the library has of the artist: album and song titles
    let copy = app.state::<LibraryCopy>();
    let albums: HashSet<String> = copy.albums(app).iter().filter(|a| fold(&a.artist) == key).map(|a| title_key(&a.title)).collect();
    let songs: HashSet<String> = copy.tracks(app).iter().filter(|t| fold(&t.artist) == key).map(|t| title_key(&t.title)).collect();
    for candidate in same.iter().take(3) {
        let Some(id) = candidate["id"].as_u64() else { continue };
        let theirs = sound.deezer(&client, &format!("https://api.deezer.com/artist/{id}/albums"), &[("limit", "100")]).await?;
        let mut matched = theirs["data"].as_array().is_some_and(|list| list.iter().any(|a| a["title"].as_str().is_some_and(|t| albums.contains(&title_key(t)))));
        if !matched && !songs.is_empty() {
            let top = sound.deezer(&client, &format!("https://api.deezer.com/artist/{id}/top"), &[("limit", "100")]).await?;
            matched = top["data"].as_array().is_some_and(|list| list.iter().any(|t| t["title"].as_str().is_some_and(|t| songs.contains(&title_key(t)))));
        }
        if matched {
            return Ok(Found { url: picture_of(candidate), deezer_id: Some(id), checked_at: now_ms() });
        }
    }
    // (a name of its own, Deezer's only one by it)
    if same.len() == 1 && key.chars().count() >= DISTINCT_NAME {
        return Ok(Found { url: picture_of(same[0]), deezer_id: same[0]["id"].as_u64(), checked_at: now_ms() });
    }
    Ok(miss)
}

/// Deezer's stand-in for an artist without a picture (a grey figure): its
/// picture id is the md5 of nothing.
const DEEZER_BLANK: &str = "d41d8cd98f00b204e9800998ecf8427e";

/// Whether an address is a real Deezer picture: not the blank one (no
/// picture id, or the grey figure's).
fn real_picture(url: &str) -> bool {
    !url.is_empty() && !url.contains("/artist//") && !url.contains(DEEZER_BLANK)
}

/// A Deezer artist's picture, 500 pixels; none when Deezer has only its
/// blank one.
fn picture_of(artist: &Value) -> Option<String> {
    let url = artist["picture_big"].as_str()?;
    real_picture(url).then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_without_editions() {
        assert_eq!(title_key("Everblack (Deluxe Edition)"), title_key("Everblack"));
        assert_eq!(title_key("Mgła [Remastered]"), "mgla");
    }

    #[test]
    fn blank_pictures_left_out() {
        let blank = serde_json::json!({ "picture_big": "https://cdn-images.dzcdn.net/images/artist//500x500-000000-80-0-0.jpg" });
        let figure = serde_json::json!({ "picture_big": "https://cdn-images.dzcdn.net/images/artist/d41d8cd98f00b204e9800998ecf8427e/500x500-000000-80-0-0.jpg" });
        let real = serde_json::json!({ "picture_big": "https://cdn-images.dzcdn.net/images/artist/2c3c/500x500-000000-80-0-0.jpg" });
        assert_eq!(picture_of(&blank), None);
        assert_eq!(picture_of(&figure), None);
        assert!(picture_of(&real).is_some());
    }
}
