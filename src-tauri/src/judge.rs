// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tier 2 of Wrapped Discovery: a language model reads what's written about
//! a seed (as you play it) and a candidate, and says whether the candidate
//! fits, which of its songs to open with, and why, in one line for the
//! player. Tags can't tell 11 Dreams from Mercenary's power metal side;
//! Roon's own review of the album can.
//!
//! "Light" (the only mode for now): tier 1 ranks the candidates as before;
//! the model can only drop one or choose its song. Verdicts are cached for
//! 90 days per candidate, seed and the seed's albums, so a rebuild costs
//! nothing new.
//!
//! Services: Anthropic, OpenAI, Google Gemini (each with a key), or Ollama
//! on this PC (an address). All take the same system and user text and
//! answer with a small JSON object.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use roon_library::{LibraryClient, TopTrack};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::tags::Tag;
use crate::wrapped::{read_json, write_json, WrappedSettings};

const KEEP_FOR_MS: i64 = 90 * 86_400_000;
/// How many judgements run at once (Gemini's free tier: one at a time).
const AT_ONCE: usize = 4;
/// Longest text handed over per bio or review, in characters.
const BIO_CHARS: usize = 1200;
const REVIEW_CHARS: usize = 700;

/// What the model said about one candidate for one seed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Verdict {
    pub fit: bool,
    pub confidence: f64,
    /// The song to open with, as titled in the candidate list (None: no
    /// preference, tier 1's album choice stands).
    pub song: Option<String>,
    /// One line for the listener.
    pub why: String,
    /// When not a fit: why not, for the log.
    pub note: String,
    pub at: i64,
    /// Which service and model said it.
    pub by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Store {
    verdicts: HashMap<String, Verdict>,
}

#[derive(Default)]
pub struct Judge {
    store: Mutex<Option<Store>>,
}

impl Judge {
    fn path(app: &AppHandle) -> Option<PathBuf> {
        app.path().app_data_dir().ok().map(|dir| dir.join("wrapped-judge.json"))
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

/// The language model's settings, as the build uses them.
#[derive(Debug, Clone)]
pub struct Llm {
    pub provider: String,
    pub key: String,
    pub model: String,
    pub url: String,
    /// "light" or "heavy" (heavy isn't built yet: judged the same for now).
    #[allow(dead_code)]
    pub mode: String,
}

impl Llm {
    /// None when tier 2 isn't set up (no key, or no Ollama address).
    pub fn from_settings(s: &WrappedSettings) -> Option<Llm> {
        let provider = s.llm_provider.trim().to_lowercase();
        let provider = if provider.is_empty() { "anthropic".to_string() } else { provider };
        let key = s.llm_key.trim().to_string();
        let url = s.llm_url.trim().trim_end_matches('/').to_string();
        if provider != "ollama" && key.is_empty() {
            return None;
        }
        let model = if s.llm_model.trim().is_empty() { default_model(&provider).to_string() } else { s.llm_model.trim().to_string() };
        let url = if provider == "ollama" && url.is_empty() { "http://localhost:11434".to_string() } else { url };
        let mode = if s.llm_mode.trim().eq_ignore_ascii_case("heavy") { "heavy" } else { "light" }.to_string();
        Some(Llm { provider, key, model, url, mode })
    }

    fn at_once(&self) -> usize {
        if self.provider == "gemini" {
            1
        } else {
            AT_ONCE
        }
    }
}

pub fn default_model(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-4o-mini",
        "gemini" => "gemini-2.5-flash",
        "ollama" => "llama3.1",
        _ => "claude-haiku-5-5",
    }
}

/// The seed as the model should see it: the band and the albums you played.
#[derive(Debug, Clone, Default)]
pub struct SeedBrief {
    pub name: String,
    pub tags: Vec<Tag>,
    pub era: Option<(u32, u32)>,
    /// (title, year, review excerpt)
    pub albums: Vec<(String, Option<u32>, String)>,
    pub bio: String,
}

/// A candidate as the model should see it.
#[derive(Debug, Clone, Default)]
pub struct CandidateBrief {
    pub name: String,
    pub tags: Vec<Tag>,
    pub bio: String,
    /// (title, year, tags, review excerpt, song titles)
    pub albums: Vec<(String, Option<u32>, Vec<Tag>, String, Vec<String>)>,
}

/// A candidate's brief from its songs (several albums) and what Roon and
/// Last.fm say; `artist_handle` for the bio.
pub async fn candidate_brief(app: &AppHandle, client: &LibraryClient, key: &str, name: &str, artist_handle: &str, songs: &[TopTrack]) -> CandidateBrief {
    let tags = app.state::<crate::tags::Tags>();
    let bio = client.artist_bio(artist_handle).await.ok().flatten().unwrap_or_default();
    let mut albums: Vec<(String, Option<u32>, Vec<Tag>, String, Vec<String>)> = Vec::new();
    for t in songs {
        match albums.iter_mut().find(|a| a.0 == t.album) {
            Some(a) => a.4.push(t.title.clone()),
            None => {
                let review = match &t.album_handle {
                    Some(h) => client.album_review(h).await.ok().flatten().unwrap_or_default(),
                    None => String::new(),
                };
                let album_tags = if key.is_empty() { Vec::new() } else { tags.lookup_album(app, name, &t.album, key).await.tags };
                albums.push((t.album.clone(), t.year, album_tags, review, vec![t.title.clone()]));
            }
        }
    }
    CandidateBrief { name: name.to_string(), tags: tags.cached(app, name).map(|t| t.tags).unwrap_or_default(), bio, albums }
}

fn clip(text: &str, chars: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= chars {
        return text;
    }
    let cut: String = text.chars().take(chars).collect();
    match cut.rfind(". ") {
        Some(i) if i > chars / 2 => cut[..=i].to_string(),
        _ => format!("{cut}..."),
    }
}

fn tag_line(tags: &[Tag], n: usize) -> String {
    tags.iter().take(n).map(|t| format!("{} {}", t.name, t.weight)).collect::<Vec<_>>().join(", ")
}

const SYSTEM: &str = "You help build one listener's weekly discovery playlist. You get one artist they played (the seed), described by the albums they actually played, and one candidate artist found through listeners the two share. Decide whether the candidate, introduced by one of the listed songs, fits someone who played those seed albums. Judge the sound and era of what they played, not the band's name or fame: 1980s thrash is not a fit for a listener of modern blackened thrash, a band's power metal albums are not a fit for a melodic death metal listener, a band's later polished records are not a fit for someone who plays its raw early ones, and the reverse. Use the texts and tags given, and what you know of the bands. Pick the song whose album is closest to the seed albums' era and sound; avoid intros, interludes, ballads, covers and joke songs unless the seed albums are like that. Answer with JSON only, no prose around it: {\"fit\": true or false, \"confidence\": 0 to 1, \"song\": \"a title exactly as listed, or null\", \"why\": \"one line to the listener, at most 140 characters, no greeting, do not repeat the band names\", \"note\": \"when not a fit, why not, at most 100 characters\"}";

fn user_text(seed: &SeedBrief, cand: &CandidateBrief) -> String {
    let mut out = String::new();
    out.push_str(&format!("SEED: {}\n", seed.name));
    if let Some((a, b)) = seed.era {
        out.push_str(&format!("Era of the albums played: {a}-{b}\n"));
    }
    if !seed.tags.is_empty() {
        out.push_str(&format!("Tags of the albums played: {}\n", tag_line(&seed.tags, 8)));
    }
    for (title, year, review) in &seed.albums {
        let y = year.map(|y| format!(" ({y})")).unwrap_or_default();
        out.push_str(&format!("Album played: {title}{y}\n"));
        if !review.is_empty() {
            out.push_str(&format!("  Review: {}\n", clip(review, REVIEW_CHARS)));
        }
    }
    if !seed.bio.is_empty() {
        out.push_str(&format!("Bio: {}\n", clip(&seed.bio, BIO_CHARS / 2)));
    }
    out.push_str(&format!("\nCANDIDATE: {}\n", cand.name));
    if !cand.tags.is_empty() {
        out.push_str(&format!("Tags: {}\n", tag_line(&cand.tags, 8)));
    }
    if !cand.bio.is_empty() {
        out.push_str(&format!("Bio: {}\n", clip(&cand.bio, BIO_CHARS)));
    }
    for (title, year, tags, review, songs) in &cand.albums {
        let y = year.map(|y| format!(" ({y})")).unwrap_or_default();
        out.push_str(&format!("Album: {title}{y}\n"));
        if !tags.is_empty() {
            out.push_str(&format!("  Tags: {}\n", tag_line(tags, 5)));
        }
        if !review.is_empty() {
            out.push_str(&format!("  Review: {}\n", clip(review, REVIEW_CHARS)));
        }
        out.push_str(&format!("  Songs to choose from: {}\n", songs.join(" | ")));
    }
    out
}

fn cache_key(llm: &Llm, seed: &SeedBrief, cand: &CandidateBrief) -> String {
    let albums: Vec<String> = seed.albums.iter().map(|(t, _, _)| t.to_lowercase()).collect();
    format!("{}|{}|{}|{}|{}", llm.provider, llm.model, cand.name.to_lowercase(), seed.name.to_lowercase(), albums.join(","))
}

/// One candidate judged against its seed (cached).
pub async fn judge_one(app: &AppHandle, llm: &Llm, seed: &SeedBrief, cand: &CandidateBrief) -> Result<Verdict, String> {
    let judge = app.state::<Judge>();
    let key = cache_key(llm, seed, cand);
    let now = crate::wrapped::now_ms();
    if let Some(have) = judge.with_store(app, |s| s.verdicts.get(&key).cloned()) {
        if now - have.at < KEEP_FOR_MS {
            return Ok(have);
        }
    }
    let text = complete(llm, SYSTEM, &user_text(seed, cand)).await?;
    let mut verdict = parse_verdict(&text)?;
    verdict.at = now;
    verdict.by = format!("{} {}", llm.provider, llm.model);
    // (a song the list doesn't have is no choice)
    if let Some(song) = &verdict.song {
        let listed = cand.albums.iter().flat_map(|a| a.4.iter()).any(|t| t.eq_ignore_ascii_case(song));
        if !listed {
            verdict.song = None;
        }
    }
    judge.with_store(app, |s| {
        s.verdicts.retain(|_, v| now - v.at < KEEP_FOR_MS);
        s.verdicts.insert(key, verdict.clone());
    });
    judge.save(app);
    Ok(verdict)
}

/// Every candidate judged, `at_once` at a time (one at a time, 13 s apart,
/// on Gemini); `progress` gets a line per finished judgement. Errors come
/// back as Err per candidate, so one failure doesn't stop the build.
pub async fn judge_all(app: &AppHandle, llm: &Llm, pairs: Vec<(SeedBrief, CandidateBrief)>, progress: &impl Fn(String)) -> Vec<Result<Verdict, String>> {
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(llm.at_once()));
    let mut jobs = tokio::task::JoinSet::new();
    for (i, (seed, cand)) in pairs.into_iter().enumerate() {
        let (app, llm, gate) = (app.clone(), llm.clone(), gate.clone());
        jobs.spawn(async move {
            let _turn = gate.acquire_owned().await;
            let out = judge_one(&app, &llm, &seed, &cand).await;
            if llm.provider == "gemini" {
                tokio::time::sleep(std::time::Duration::from_secs(13)).await;
            }
            (i, out)
        });
    }
    let total = jobs.len();
    let mut out: Vec<Result<Verdict, String>> = (0..total).map(|_| Err("not judged".to_string())).collect();
    let mut done = 0;
    while let Some(next) = jobs.join_next().await {
        if let Ok((i, verdict)) = next {
            done += 1;
            progress(format!("Reading up on the picks... {done} of {total}"));
            out[i] = verdict;
        }
    }
    out
}

fn parse_verdict(text: &str) -> Result<Verdict, String> {
    // (some models wrap the JSON in a code fence or a sentence)
    let start = text.find('{').ok_or_else(|| format!("no JSON in the answer: {}", clip(text, 200)))?;
    let end = text.rfind('}').ok_or_else(|| format!("no JSON in the answer: {}", clip(text, 200)))?;
    let body = &text[start..=end];
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("bad JSON in the answer ({e}): {}", clip(body, 200)))?;
    let song = match v.get("song") {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() && s.trim().to_lowercase() != "null" => Some(s.trim().to_string()),
        _ => None,
    };
    Ok(Verdict {
        fit: v.get("fit").and_then(|f| f.as_bool()).unwrap_or(false),
        confidence: v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.5),
        song,
        why: v.get("why").and_then(|w| w.as_str()).unwrap_or("").trim().trim_end_matches('.').to_string(),
        note: v.get("note").and_then(|w| w.as_str()).unwrap_or("").trim().to_string(),
        at: 0,
        by: String::new(),
    })
}

/// One completion from the chosen service: the answer's text.
pub async fn complete(llm: &Llm, system: &str, user: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .user_agent("roon-toasted-wrapped")
        .build()
        .map_err(|e| e.to_string())?;
    let (request, parse): (reqwest::RequestBuilder, fn(&serde_json::Value) -> Option<String>) = match llm.provider.as_str() {
        "openai" => (
            client
                .post("https://api.openai.com/v1/chat/completions")
                .bearer_auth(&llm.key)
                .json(&serde_json::json!({
                    "model": llm.model,
                    "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
                    "response_format": { "type": "json_object" },
                    "temperature": 0.2,
                })),
            |v| v["choices"][0]["message"]["content"].as_str().map(str::to_string),
        ),
        "gemini" => (
            client
                .post(format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", llm.model))
                .header("x-goog-api-key", &llm.key)
                .json(&serde_json::json!({
                    "systemInstruction": { "parts": [{ "text": system }] },
                    "contents": [{ "role": "user", "parts": [{ "text": user }] }],
                    "generationConfig": { "responseMimeType": "application/json", "temperature": 0.2 },
                })),
            |v| v["candidates"][0]["content"]["parts"][0]["text"].as_str().map(str::to_string),
        ),
        "ollama" => (
            client.post(format!("{}/api/chat", llm.url)).json(&serde_json::json!({
                "model": llm.model,
                "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
                "stream": false,
                "format": "json",
                "options": { "temperature": 0.2 },
            })),
            |v| v["message"]["content"].as_str().map(str::to_string),
        ),
        _ => (
            client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &llm.key)
                .header("anthropic-version", "2023-06-01")
                // (no temperature: newer Claude models refuse it)
                .json(&serde_json::json!({
                    "model": llm.model,
                    "max_tokens": 400,
                    "system": system,
                    "messages": [{ "role": "user", "content": user }],
                })),
            |v| {
                v["content"]
                    .as_array()?
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
                    .into()
            },
        ),
    };
    let response = request.send().await.map_err(|e| format!("{}: {e}", service_name(&llm.provider)))?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let said = body["error"]["message"].as_str().or_else(|| body["error"].as_str()).or_else(|| body["message"].as_str()).unwrap_or("");
        return Err(format!("{} answered {}{}", service_name(&llm.provider), status.as_u16(), if said.is_empty() { String::new() } else { format!(": {said}") }));
    }
    parse(&body).ok_or_else(|| format!("{} gave no text back", service_name(&llm.provider)))
}

pub fn service_name(provider: &str) -> &'static str {
    match provider {
        "openai" => "OpenAI",
        "gemini" => "Gemini",
        "ollama" => "Ollama",
        _ => "Anthropic",
    }
}

/// Settings' Test button: a tiny call, and what came back (or the error).
#[tauri::command]
pub async fn wrapped_llm_test(app: AppHandle) -> Result<String, String> {
    let settings = WrappedSettings::load(&app);
    let llm = Llm::from_settings(&settings).ok_or("No key set (or no Ollama address).")?;
    let text = complete(&llm, "Answer with JSON only.", "Say hello to a metal listener in at most ten words, as {\"hello\": \"...\"}.").await?;
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(text.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim());
    let hello = parsed.ok().and_then(|v| v["hello"].as_str().map(str::to_string)).unwrap_or(text);
    Ok(format!("{} ({}) says: {}", service_name(&llm.provider), llm.model, hello.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_parse_with_wrapping() {
        let v = parse_verdict("Sure:\n```json\n{\"fit\": true, \"confidence\": 0.8, \"song\": \"Falling\", \"why\": \"Melodic death with clean hooks.\", \"note\": \"\"}\n```").unwrap();
        assert!(v.fit);
        assert_eq!(v.song.as_deref(), Some("Falling"));
        assert_eq!(v.why, "Melodic death with clean hooks");
        let v = parse_verdict("{\"fit\": false, \"song\": null, \"note\": \"power metal\"}").unwrap();
        assert!(!v.fit && v.song.is_none());
        assert!(parse_verdict("no idea").is_err());
    }

    #[test]
    fn clips_at_a_sentence() {
        let long = "One sentence here. Another one follows it. And a third that runs on and on.";
        assert_eq!(clip(long, 45), "One sentence here. Another one follows it.");
        assert_eq!(clip("short", 40), "short");
    }
}
