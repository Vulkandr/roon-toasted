// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What's written about artists and albums outside Roon, for tier 2
//! (judge.rs): Last.fm's bios and album write-ups (with the Last.fm key
//! Wrapped already has), Wikipedia's summaries (no key), and Discogs' styles
//! per release (with the user's own Discogs token, optional). Cached 90 days
//! in `wrapped-texts.json`; each service asked at its own pace.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::wrapped::{now_ms, read_json, write_json};

const KEEP_FOR_MS: i64 = 90 * 86_400_000;
const USER_AGENT: &str = concat!("RoonToasted/", env!("CARGO_PKG_VERSION"), " (https://github.com/Vulkandr/roon-toasted)");

/// One looked-up text (or list), empty when the service had nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Cached {
    text: String,
    list: Vec<String>,
    at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Store {
    entries: HashMap<String, Cached>,
}

#[derive(Default)]
pub struct Sources {
    store: Mutex<Option<Store>>,
    /// One gate per service: Last.fm, Wikipedia, Discogs.
    lastfm: tokio::sync::Mutex<Option<Instant>>,
    wikipedia: tokio::sync::Mutex<Option<Instant>>,
    discogs: tokio::sync::Mutex<Option<Instant>>,
}

/// What's written about an artist.
#[derive(Debug, Clone, Default)]
pub struct ArtistText {
    pub lastfm: String,
    pub wikipedia: String,
}

/// What's written about an album.
#[derive(Debug, Clone, Default)]
pub struct AlbumText {
    pub lastfm: String,
    pub wikipedia: String,
    /// Discogs' styles for the release ("Melodic Death Metal").
    pub styles: Vec<String>,
}

impl Sources {
    fn path(app: &AppHandle) -> Option<PathBuf> {
        app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-texts.json"))
    }

    fn get(&self, app: &AppHandle, key: &str) -> Option<Cached> {
        let mut guard = self.store.lock().unwrap();
        let store = guard.get_or_insert_with(|| Self::path(app).and_then(|p| read_json(&p)).unwrap_or_default());
        store.entries.get(key).filter(|c| now_ms() - c.at < KEEP_FOR_MS).cloned()
    }

    fn put(&self, app: &AppHandle, key: String, text: String, list: Vec<String>) {
        {
            let mut guard = self.store.lock().unwrap();
            let store = guard.get_or_insert_with(|| Self::path(app).and_then(|p| read_json(&p)).unwrap_or_default());
            store.entries.insert(key, Cached { text, list, at: now_ms() });
        }
        let Some(path) = Self::path(app) else { return };
        let guard = self.store.lock().unwrap();
        if let Some(store) = guard.as_ref() {
            write_json(&path, store);
        }
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

    /// An artist's Last.fm bio (only when Roon has none: `want_lastfm`) and
    /// Wikipedia summary.
    pub async fn artist(&self, app: &AppHandle, name: &str, lastfm_key: &str, want_lastfm: bool) -> ArtistText {
        let mut out = ArtistText::default();
        let Ok(client) = client() else { return out };
        if want_lastfm && !lastfm_key.is_empty() {
            let key = format!("lfm-artist|{}", name.to_lowercase());
            out.lastfm = match self.get(app, &key) {
                Some(c) => c.text,
                None => {
                    Self::turn(&self.lastfm, Duration::from_millis(250)).await;
                    match lastfm_info(&client, &[("method", "artist.getinfo"), ("artist", name)], lastfm_key, &["artist", "bio", "summary"]).await {
                        Ok(text) => {
                            self.put(app, key, text.clone(), Vec::new());
                            text
                        }
                        Err(_) => String::new(),
                    }
                }
            };
        }
        let key = format!("wiki-artist|{}", name.to_lowercase());
        out.wikipedia = match self.get(app, &key) {
            Some(c) => c.text,
            None => match self.wikipedia_artist(&client, name).await {
                Ok(text) => {
                    self.put(app, key, text.clone(), Vec::new());
                    text
                }
                Err(_) => String::new(),
            },
        };
        out
    }

    /// An album's Last.fm write-up and Wikipedia summary (only when Roon has
    /// no review: `want_text`), and its Discogs styles (with a token).
    pub async fn album(&self, app: &AppHandle, artist: &str, album: &str, lastfm_key: &str, discogs_token: &str, want_text: bool) -> AlbumText {
        let mut out = AlbumText::default();
        let Ok(client) = client() else { return out };
        let id = format!("{}|{}", artist.to_lowercase(), album.to_lowercase());
        if want_text {
            if !lastfm_key.is_empty() {
                let key = format!("lfm-album|{id}");
                out.lastfm = match self.get(app, &key) {
                    Some(c) => c.text,
                    None => {
                        Self::turn(&self.lastfm, Duration::from_millis(250)).await;
                        match lastfm_info(&client, &[("method", "album.getinfo"), ("artist", artist), ("album", album)], lastfm_key, &["album", "wiki", "summary"]).await {
                            Ok(text) => {
                                self.put(app, key, text.clone(), Vec::new());
                                text
                            }
                            Err(_) => String::new(),
                        }
                    }
                };
            }
            let key = format!("wiki-album|{id}");
            out.wikipedia = match self.get(app, &key) {
                Some(c) => c.text,
                None => match self.wikipedia_album(&client, artist, album).await {
                    Ok(text) => {
                        self.put(app, key, text.clone(), Vec::new());
                        text
                    }
                    Err(_) => String::new(),
                },
            };
        }
        if !discogs_token.is_empty() {
            let key = format!("discogs|{id}");
            out.styles = match self.get(app, &key) {
                Some(c) => c.list,
                None => match self.discogs_styles(&client, artist, album, discogs_token).await {
                    Ok(list) => {
                        self.put(app, key, String::new(), list.clone());
                        list
                    }
                    Err(_) => Vec::new(),
                },
            };
        }
        out
    }

    async fn wikipedia_artist(&self, client: &reqwest::Client, name: &str) -> Result<String, String> {
        let want = plain(name);
        for title in self.wikipedia_search(client, &format!("{name} band")).await? {
            let t = plain(&title);
            // ("Mercenary (band)", "Ulver")
            if t != want && !t.starts_with(&format!("{want} ")) {
                continue;
            }
            let (description, extract) = self.wikipedia_summary(client, &title).await?;
            let d = description.to_lowercase();
            let musical = ["band", "musician", "singer", "group", "project", "rapper", "dj", "composer", "duo", "producer"].iter().any(|w| d.contains(w));
            if musical {
                return Ok(extract);
            }
        }
        Ok(String::new())
    }

    async fn wikipedia_album(&self, client: &reqwest::Client, artist: &str, album: &str) -> Result<String, String> {
        let want = plain(album);
        let by = artist.to_lowercase();
        for title in self.wikipedia_search(client, &format!("\"{album}\" {artist} album")).await? {
            let t = plain(&title);
            // ("Everblack (album)", "11 Dreams")
            if t != want && !t.starts_with(&format!("{want} ")) {
                continue;
            }
            let (description, extract) = self.wikipedia_summary(client, &title).await?;
            let d = description.to_lowercase();
            let is_album = ["album", "ep", "record"].iter().any(|w| d.contains(w));
            if is_album && (d.contains(&by) || extract.to_lowercase().contains(&by)) {
                return Ok(extract);
            }
        }
        Ok(String::new())
    }

    async fn wikipedia_search(&self, client: &reqwest::Client, query: &str) -> Result<Vec<String>, String> {
        Self::turn(&self.wikipedia, Duration::from_millis(200)).await;
        let res = client
            .get("https://en.wikipedia.org/w/api.php")
            .query(&[("action", "query"), ("list", "search"), ("srsearch", query), ("srlimit", "4"), ("srnamespace", "0"), ("format", "json")])
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        Ok(body["query"]["search"].as_array().map(|a| a.iter().filter_map(|r| r["title"].as_str().map(str::to_string)).collect()).unwrap_or_default())
    }

    /// A page's (description, lead summary).
    async fn wikipedia_summary(&self, client: &reqwest::Client, title: &str) -> Result<(String, String), String> {
        Self::turn(&self.wikipedia, Duration::from_millis(200)).await;
        let mut url = reqwest::Url::parse("https://en.wikipedia.org/api/rest_v1/page/summary/").map_err(|e| e.to_string())?;
        url.path_segments_mut().map_err(|_| "bad url".to_string())?.pop_if_empty().push(&title.replace(' ', "_"));
        let res = client.get(url).send().await.map_err(|e| e.to_string())?;
        let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        if body["type"].as_str() == Some("disambiguation") {
            return Ok((String::new(), String::new()));
        }
        Ok((body["description"].as_str().unwrap_or("").to_string(), body["extract"].as_str().unwrap_or("").to_string()))
    }

    async fn discogs_styles(&self, client: &reqwest::Client, artist: &str, album: &str, token: &str) -> Result<Vec<String>, String> {
        let want = plain(album);
        for kind in ["master", "release"] {
            // (60 requests a minute with a token)
            Self::turn(&self.discogs, Duration::from_millis(1100)).await;
            let res = client
                .get("https://api.discogs.com/database/search")
                .header("Authorization", format!("Discogs token={token}"))
                .query(&[("type", kind), ("artist", artist), ("release_title", album), ("per_page", "5")])
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !res.status().is_success() {
                return Err(format!("Discogs answered {}", res.status().as_u16()));
            }
            let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
            for r in body["results"].as_array().into_iter().flatten() {
                // ("Mercenary - Everblack")
                let title = r["title"].as_str().unwrap_or("");
                let name = title.split_once(" - ").map(|(_, t)| t).unwrap_or(title);
                if plain(name) != want && !plain(name).starts_with(&want) {
                    continue;
                }
                let styles: Vec<String> = r["style"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect();
                if !styles.is_empty() {
                    return Ok(styles);
                }
            }
        }
        Ok(Vec::new())
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(12)).build().map_err(|e| e.to_string())
}

/// A Last.fm getInfo call's text at `path` (bio or wiki summary), without its
/// markup and the "Read more on Last.fm" link.
async fn lastfm_info(client: &reqwest::Client, params: &[(&str, &str)], key: &str, path: &[&str]) -> Result<String, String> {
    let mut query: Vec<(&str, &str)> = params.to_vec();
    query.extend([("api_key", key), ("format", "json"), ("autocorrect", "1")]);
    let res = client.get("https://ws.audioscrobbler.com/2.0/").query(&query).send().await.map_err(|e| e.to_string())?;
    let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    if body.get("error").is_some() {
        return Err(body["message"].as_str().unwrap_or("Last.fm error").to_string());
    }
    let mut v = &body;
    for step in path {
        v = &v[*step];
    }
    Ok(strip_markup(v.as_str().unwrap_or("")))
}

/// Text without HTML tags, links' words kept, Last.fm's "Read more" dropped.
pub fn strip_markup(html: &str) -> String {
    let cut = html.find("<a href=\"https://www.last.fm").map(|i| &html[..i]).unwrap_or(html);
    let mut out = String::new();
    let mut in_tag = false;
    for c in cut.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Letters and digits only, lower-cased, words kept apart by single spaces
/// ("Everblack (album)" -> "everblack album").
fn plain(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_goes() {
        assert_eq!(
            strip_markup("Mercenary is a <b>Danish</b> band. <a href=\"https://www.last.fm/music/Mercenary\">Read more on Last.fm</a>"),
            "Mercenary is a Danish band."
        );
        assert_eq!(strip_markup("A &amp; B"), "A & B");
    }

    #[test]
    fn plain_titles() {
        assert_eq!(plain("Everblack (album)"), "everblack album");
        assert_eq!(plain("11 Dreams"), "11 dreams");
        assert!(plain("Mercenary (band)").starts_with(&format!("{} ", plain("Mercenary"))));
    }
}
