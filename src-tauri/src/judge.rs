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
/// Ollama's room for one judgement, in tokens: a brief runs 4,000 to 6,000,
/// plus the answer (and any thinking). Ollama's own default is only 4,096 on
/// smaller graphics cards, and it quietly drops the start of a longer prompt
/// (the instructions). Always the same, since a change makes Ollama load the
/// model again.
const OLLAMA_CONTEXT: u32 = 16_384;
/// How long Ollama keeps the model loaded after the last judgement, so the
/// graphics card's memory comes back soon after a build (games want it).
const OLLAMA_KEEP_ALIVE: &str = "2m";
/// How long one answer may take: a local model may have to load first, and
/// Ollama answers one at a time while the others wait.
const OLLAMA_WAIT_SECS: u64 = 300;
const ONLINE_WAIT_SECS: u64 = 180;

/// What the model said about one candidate for one seed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Verdict {
    /// The two sounds side by side in specific terms, written before the
    /// verdict (a model that doesn't think first decides better after it).
    pub comparison: String,
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
    /// "light" (keep or drop, pick the song) or "heavy" (also adds picks of
    /// its own and puts the list in order, see `suggest` and `order`).
    pub mode: String,
    /// How long one answer may take, in seconds (0: the service's usual,
    /// ONLINE_WAIT_SECS or OLLAMA_WAIT_SECS; Deep Dive allows more).
    pub wait_secs: u64,
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
        let url = if provider == "ollama" { crate::ollama::address(s) } else { url };
        let mode = if s.llm_mode.trim().eq_ignore_ascii_case("heavy") { "heavy" } else { "light" }.to_string();
        Some(Llm { provider, key, model, url, mode, wait_secs: 0 })
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
        "ollama" => "gemma4:12b",
        _ => "claude-haiku-5-5",
    }
}

/// An album as the model should see it.
#[derive(Debug, Clone, Default)]
pub struct AlbumBrief {
    pub title: String,
    pub year: Option<u32>,
    /// Last.fm's tags for the album.
    pub tags: Vec<Tag>,
    /// Roon's review (TiVo).
    pub review: String,
    /// Last.fm's write-up and Wikipedia's summary (when Roon has no review).
    pub lastfm: String,
    pub wikipedia: String,
    /// Discogs' styles for the release (with a token).
    pub styles: Vec<String>,
    /// The candidate's songs to choose from (empty for a seed album).
    pub songs: Vec<String>,
}

/// The seed as the model should see it: the band and the albums you played.
#[derive(Debug, Clone, Default)]
pub struct SeedBrief {
    pub name: String,
    pub tags: Vec<Tag>,
    pub era: Option<(u32, u32)>,
    pub albums: Vec<AlbumBrief>,
    /// Roon's bio; Last.fm's when Roon has none; Wikipedia's summary.
    pub bio: String,
    pub lastfm_bio: String,
    pub wikipedia: String,
    /// A request standing in for a seed: what the listener asked for (an
    /// aspect of a song, then the song's artist and album are the seed's),
    /// or their own words (no name, no albums).
    pub request: String,
    /// The listener's requests for this list, for every pick: one that goes
    /// against them is less likely to be enjoyed.
    pub wishes: Vec<String>,
}

/// A candidate as the model should see it.
#[derive(Debug, Clone, Default)]
pub struct CandidateBrief {
    pub name: String,
    pub tags: Vec<Tag>,
    pub bio: String,
    pub lastfm_bio: String,
    pub wikipedia: String,
    pub albums: Vec<AlbumBrief>,
    /// Tier 3's hearing of the songs to choose from against the seed's own
    /// played songs ("Title: 0.88 (Black Metal, Viking Metal)"), when it
    /// listened.
    pub sound: Vec<String>,
}

/// An album's brief: Roon's review, Last.fm's tags, and the outside texts
/// (sources.rs).
pub async fn album_brief(app: &AppHandle, client: &LibraryClient, key: &str, discogs: &str, artist: &str, title: &str, year: Option<u32>, album_handle: Option<&str>) -> AlbumBrief {
    let review = match album_handle {
        Some(h) => client.album_review(h).await.ok().flatten().unwrap_or_default(),
        None => String::new(),
    };
    let tags = if key.is_empty() { Vec::new() } else { app.state::<crate::tags::Tags>().lookup_album(app, artist, title, key).await.tags };
    let more = app.state::<crate::sources::Sources>().album(app, artist, title, key, discogs, review.is_empty()).await;
    AlbumBrief { title: title.to_string(), year, tags, review, lastfm: more.lastfm, wikipedia: more.wikipedia, styles: more.styles, songs: Vec::new() }
}

/// A candidate's brief from its songs (several albums) and what Roon,
/// Last.fm, Wikipedia and Discogs say; `artist_handle` for Roon's bio.
pub async fn candidate_brief(app: &AppHandle, client: &LibraryClient, key: &str, discogs: &str, name: &str, artist_handle: &str, songs: &[TopTrack]) -> CandidateBrief {
    let tags = app.state::<crate::tags::Tags>();
    let bio = client.artist_bio(artist_handle).await.ok().flatten().unwrap_or_default();
    let texts = app.state::<crate::sources::Sources>().artist(app, name, key, bio.is_empty()).await;
    let mut albums: Vec<AlbumBrief> = Vec::new();
    for t in songs {
        match albums.iter_mut().find(|a| a.title == t.album) {
            Some(a) => a.songs.push(t.title.clone()),
            None => {
                let mut a = album_brief(app, client, key, discogs, name, &t.album, t.year, t.album_handle.as_deref()).await;
                a.songs.push(t.title.clone());
                albums.push(a);
            }
        }
    }
    CandidateBrief { name: name.to_string(), tags: tags.cached(app, name).map(|t| t.tags).unwrap_or_default(), bio, lastfm_bio: texts.lastfm, wikipedia: texts.wikipedia, albums, sound: Vec::new() }
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

const SYSTEM: &str = "You help build one listener's weekly discovery playlist. You get one artist they played (the seed), described by the albums they actually played, and one candidate artist found through listeners the two share. Decide whether the candidate, introduced by one of the listed songs, fits someone who played those seed albums. Sometimes the listener asked for something instead (WHAT THEY ASKED FOR): an aspect of a song they name (its guitars, vocals, rhythm, mood, production, or all of it), or a sound in their own words; then judge the candidate against that request, and the seed albums, when given, are that song's album. When other requests of theirs are listed, a pick that goes against one is less likely to be enjoyed. Judge the sound and era of what they played, not the band's name or fame. Shared listeners are only how the candidate was found, not evidence: bands that shared radio playlists, tours, labels or an era often share fans without sounding alike (two bands played side by side on the same radio stations can be nothing alike). Broad labels such as rock, hard rock, alternative, metal or extreme metal say nothing on their own: decide by the specifics of the music, its vocals, guitar sound and riffing, rhythm, heaviness, production and mood. 1980s thrash is not a fit for a listener of modern blackened thrash, a band's power metal albums are not a fit for a melodic death metal listener, a band's later polished records are not a fit for someone who plays its raw early ones, and the reverse. Judge the candidate by its listed albums, not its whole career: when any one of them fits the seed albums, the candidate is a fit and the song comes from that album (a band whose early records are power metal still fits a melodic death metal listener through a later album that is melodic death metal); drop it only when none of its listed albums fits. Use the texts and tags given (Roon's reviews, Discogs' per-album styles, Last.fm, Wikipedia), and what you know of the bands. When SOUND is given, it is a model that listened to the actual audio of the candidate's songs and of the seed songs the listener played: weigh it heavily, prefer the closest-sounding song unless it is an intro, interlude or such, and a candidate whose songs all sound far (under 0.6) rarely fits. Pick the song whose album is closest to the seed albums' era and sound; avoid intros, interludes, ballads, covers and joke songs unless the seed albums are like that. The why line describes the candidate's sound and what ties it to the seed albums, in at most 140 characters: name only albums listed above (the seed's played albums or the candidate's), never another album; no filler such as \"a good next step\", \"for your playlist\" or \"your current rotation\"; no greeting; do not repeat the band names. First compare the two sounds, then decide. Answer with JSON only, no prose around it: {\"comparison\": \"the best-fitting listed album's sound next to the seed albums' sound, in those specific terms, at most 220 characters\", \"fit\": true or false, \"confidence\": a number from 0 to 1, how likely this listener enjoys the song (low when it isn't a fit), \"song\": \"a title exactly as listed, or null\", \"why\": \"the line\", \"note\": \"when not a fit, why not, at most 100 characters\"}";

/// Bumped when the instructions change, so cached verdicts from older ones
/// are judged again.
const PROMPT_VERSION: u32 = 9;

/// Longest Last.fm or Wikipedia text handed over, in characters.
const OTHER_CHARS: usize = 500;

fn album_lines(out: &mut String, a: &AlbumBrief, label: &str) {
    let y = a.year.map(|y| format!(" ({y})")).unwrap_or_default();
    out.push_str(&format!("{label}: {}{y}\n", a.title));
    if !a.tags.is_empty() {
        out.push_str(&format!("  Tags (Last.fm): {}\n", tag_line(&a.tags, 5)));
    }
    if !a.styles.is_empty() {
        out.push_str(&format!("  Styles (Discogs): {}\n", a.styles.join(", ")));
    }
    if !a.review.is_empty() {
        out.push_str(&format!("  Review (Roon): {}\n", clip(&a.review, REVIEW_CHARS)));
    }
    if !a.lastfm.is_empty() {
        out.push_str(&format!("  Last.fm: {}\n", clip(&a.lastfm, OTHER_CHARS)));
    }
    if !a.wikipedia.is_empty() {
        out.push_str(&format!("  Wikipedia: {}\n", clip(&a.wikipedia, OTHER_CHARS)));
    }
    if !a.songs.is_empty() {
        out.push_str(&format!("  Songs to choose from: {}\n", a.songs.join(" | ")));
    }
}

fn bio_lines(out: &mut String, bio: &str, lastfm: &str, wikipedia: &str, chars: usize) {
    if !bio.is_empty() {
        out.push_str(&format!("Bio (Roon): {}\n", clip(bio, chars)));
    } else if !lastfm.is_empty() {
        out.push_str(&format!("Bio (Last.fm): {}\n", clip(lastfm, chars)));
    }
    if !wikipedia.is_empty() {
        out.push_str(&format!("Wikipedia: {}\n", clip(wikipedia, OTHER_CHARS)));
    }
}

fn user_text(seed: &SeedBrief, cand: &CandidateBrief) -> String {
    let mut out = String::new();
    if !seed.request.is_empty() {
        out.push_str(&format!("WHAT THEY ASKED FOR: {}\n", seed.request));
    }
    if !seed.name.is_empty() {
        out.push_str(&format!("SEED: {}\n", seed.name));
    }
    if let Some((a, b)) = seed.era {
        out.push_str(&format!("Era of the albums played: {a}-{b}\n"));
    }
    if !seed.tags.is_empty() {
        out.push_str(&format!("Tags of the albums played: {}\n", tag_line(&seed.tags, 8)));
    }
    for a in &seed.albums {
        album_lines(&mut out, a, "Album played");
    }
    bio_lines(&mut out, &seed.bio, &seed.lastfm_bio, &seed.wikipedia, BIO_CHARS / 2);
    out.push_str(&format!("\nCANDIDATE: {}\n", cand.name));
    if !cand.tags.is_empty() {
        out.push_str(&format!("Tags: {}\n", tag_line(&cand.tags, 8)));
    }
    bio_lines(&mut out, &cand.bio, &cand.lastfm_bio, &cand.wikipedia, BIO_CHARS);
    for a in &cand.albums {
        album_lines(&mut out, a, "Album");
    }
    if !cand.sound.is_empty() {
        out.push_str(&format!(
            "SOUND (a model listened to these songs and to the seed songs they played; closeness 1 = alike, about 0.3 = nothing alike; then its strongest styles): {}\n",
            cand.sound.join(" | ")
        ));
    }
    if !seed.wishes.is_empty() {
        out.push_str(&format!("\nTHIS WEEK THEY ALSO ASKED FOR: {}\n", seed.wishes.join("; ")));
    }
    out
}

fn cache_key(llm: &Llm, seed: &SeedBrief, cand: &CandidateBrief) -> String {
    let albums: Vec<String> = seed.albums.iter().map(|a| a.title.to_lowercase()).collect();
    format!(
        "v{PROMPT_VERSION}|{}|{}|{}|{}|{}|{}|{}",
        llm.provider,
        llm.model,
        cand.name.to_lowercase(),
        seed.name.to_lowercase(),
        albums.join(","),
        seed.request,
        seed.wishes.join(";")
    )
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
    let text = complete(llm, SYSTEM, &user_text(seed, cand), Some(&verdict_shape())).await?;
    let mut verdict = parse_verdict(&text)?;
    verdict.at = now;
    verdict.by = format!("{} {}", llm.provider, llm.model);
    // (a song the list doesn't have is no choice)
    if let Some(song) = &verdict.song {
        let listed = cand.albums.iter().flat_map(|a| a.songs.iter()).any(|t| t.eq_ignore_ascii_case(song));
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

/// The answer's shape, for services that can hold a model to it (Ollama:
/// small local models otherwise skip fields or add prose). The song is a
/// string, empty or "null" when there's no choice.
///
/// (the properties go out in alphabetical order, and Ollama has the model
/// write them in that order: "comparison" comes first, before the verdict)
fn verdict_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "comparison": { "type": "string" },
            "fit": { "type": "boolean" },
            "confidence": { "type": "number" },
            "song": { "type": "string" },
            "why": { "type": "string" },
            "note": { "type": "string" },
        },
        "required": ["comparison", "confidence", "fit", "note", "song", "why"],
    })
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
        comparison: v.get("comparison").and_then(|w| w.as_str()).unwrap_or("").trim().to_string(),
        fit: v.get("fit").and_then(|f| f.as_bool()).unwrap_or(false),
        confidence: v.get("confidence").and_then(|c| c.as_f64().or_else(|| c.as_str()?.trim().parse().ok())).unwrap_or(0.5).clamp(0.0, 1.0),
        song,
        why: v.get("why").and_then(|w| w.as_str()).unwrap_or("").trim().trim_end_matches('.').to_string(),
        note: v.get("note").and_then(|w| w.as_str()).unwrap_or("").trim().to_string(),
        at: 0,
        by: String::new(),
    })
}

// ----- Heavy: artists of the model's own, and the list's order -----

/// A played artist as the Heavy prompts show it: the albums you play,
/// their years and their tags.
pub struct SeedLine {
    pub name: String,
    pub albums: Vec<String>,
    pub era: Option<(u32, u32)>,
    pub tags: Vec<Tag>,
    /// Similar artists the listener knows already (the obvious names).
    pub known: Vec<String>,
}

/// An artist the model brings in itself, for one of the played artists.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub seed: String,
    pub artist: String,
    pub album: String,
    pub why: String,
}

const SUGGEST: &str = "You help build one listener's weekly discovery playlist. Below are artists they play, each with the albums they actually play. For each one, suggest artists the listener has probably never heard whose specific albums fit those played albums in sound and era: real artists with released albums that streaming services carry. Decide by the specifics of the music (vocals, guitar sound and riffing, rhythm, heaviness, production, mood), not by shared fans, radio play, tours or broad labels, and reach past the well-known names of each style: the listener knows those already (some are named for each artist), so favor lesser-known artists. Never suggest a listed artist, one they know already, or one from the list to leave out. Answer with JSON only, no prose around it: {\"picks\": [{\"seed\": \"the played artist's name exactly as listed\", \"artist\": \"the suggested artist\", \"album\": \"its album that fits best\", \"why\": \"what ties that album to the played albums, at most 80 characters\"}]}";

fn suggest_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "picks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "album": { "type": "string" },
                        "artist": { "type": "string" },
                        "seed": { "type": "string" },
                        "why": { "type": "string" },
                    },
                    "required": ["album", "artist", "seed", "why"],
                },
            },
        },
        "required": ["picks"],
    })
}

/// Up to `per_seed` artists of the model's own for each played artist,
/// none of `skip` (the played artists, and the ones tried already).
pub async fn suggest(llm: &Llm, seeds: &[SeedLine], favorites: &[String], skip: &[String], per_seed: usize) -> Result<Vec<Suggestion>, String> {
    let mut user = String::from("Artists this listener plays, as they play them:\n");
    for seed in seeds {
        let era = match seed.era {
            Some((a, b)) if a == b => format!(" ({a})"),
            Some((a, b)) => format!(" ({a}-{b})"),
            None => String::new(),
        };
        user.push_str(&format!("- {}{era}: {}", seed.name, seed.albums.join(", ")));
        if !seed.tags.is_empty() {
            user.push_str(&format!("; tags: {}", tag_line(&seed.tags, 4)));
        }
        if !seed.known.is_empty() {
            user.push_str(&format!("; they know already: {}", seed.known.join(", ")));
        }
        user.push('\n');
    }
    if !favorites.is_empty() {
        user.push_str(&format!("\nThe artists they play most (they know these, and know their styles' well-known names): {}\n", favorites.join(", ")));
    }
    user.push_str(&format!("\n{per_seed} suggestions for each artist above. Leave out these, known already or tried: {}\n", skip.join(", ")));
    let text = complete(llm, SUGGEST, &user, Some(&suggest_shape())).await?;
    parse_suggestions(&text)
}

fn json_in(text: &str) -> Result<serde_json::Value, String> {
    let start = text.find('{').ok_or_else(|| format!("no JSON in the answer: {}", clip(text, 200)))?;
    let end = text.rfind('}').ok_or_else(|| format!("no JSON in the answer: {}", clip(text, 200)))?;
    serde_json::from_str(&text[start..=end]).map_err(|e| format!("bad JSON in the answer ({e}): {}", clip(&text[start..=end], 200)))
}

fn parse_suggestions(text: &str) -> Result<Vec<Suggestion>, String> {
    let v = json_in(text)?;
    let field = |p: &serde_json::Value, k: &str| p[k].as_str().unwrap_or("").trim().to_string();
    Ok(v["picks"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| Suggestion { seed: field(p, "seed"), artist: field(p, "artist"), album: field(p, "album"), why: field(p, "why") })
                .filter(|s| !s.artist.is_empty() && !s.seed.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

const ASKED: &str = "You help build one listener's weekly discovery playlist. They asked for the numbered requests below: some name a song and what they like about it (its guitars, vocals, rhythm, mood, production, or all of it), some are in their own words. For each request, suggest artists the listener has probably never heard whose specific albums answer it in sound: real artists with released albums that streaming services carry. Decide by the specifics of the music, not by shared fans, radio play or broad labels, and favor lesser-known artists over the well-known names they know already (the artists they play most are listed). Never suggest the artist of a named song, one they know, or one from the list to leave out. Answer with JSON only, no prose around it: {\"picks\": [{\"request\": the request's number, \"artist\": \"the suggested artist\", \"album\": \"its album that answers the request\", \"why\": \"how it answers the request, at most 80 characters\"}]}";

fn asked_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "picks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "album": { "type": "string" },
                        "artist": { "type": "string" },
                        "request": { "type": "integer" },
                        "why": { "type": "string" },
                    },
                    "required": ["album", "artist", "request", "why"],
                },
            },
        },
        "required": ["picks"],
    })
}

/// Up to `per_request` artists for each of the listener's requests (their
/// numbers, from 1, in `Suggestion::seed`).
pub async fn suggest_for_requests(llm: &Llm, requests: &[String], favorites: &[String], skip: &[String], per_request: usize) -> Result<Vec<Suggestion>, String> {
    let mut user = String::from("What they asked for:\n");
    for (n, r) in requests.iter().enumerate() {
        user.push_str(&format!("{}. {r}\n", n + 1));
    }
    if !favorites.is_empty() {
        user.push_str(&format!("\nThe artists they play most (they know these, and know their styles' well-known names): {}\n", favorites.join(", ")));
    }
    user.push_str(&format!("\n{per_request} suggestions for each request. Leave out these, known already or tried: {}\n", skip.join(", ")));
    let text = complete(llm, ASKED, &user, Some(&asked_shape())).await?;
    let v = json_in(&text)?;
    let field = |p: &serde_json::Value, k: &str| p[k].as_str().unwrap_or("").trim().to_string();
    Ok(v["picks"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| Suggestion {
                    seed: p["request"].as_u64().map(|n| n.to_string()).unwrap_or_else(|| field(p, "request")),
                    artist: field(p, "artist"),
                    album: field(p, "album"),
                    why: field(p, "why"),
                })
                .filter(|s| !s.artist.is_empty() && !s.seed.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

// ----- Deep Dive: a playlist made to order -----

/// Tier 2's side of the talk before a Deep Dive: what it says, whether it
/// knows enough to start, and what's settled so far.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiveReply {
    pub say: String,
    pub done: bool,
    /// Everything the listener wants, in plain words, for choosing songs.
    pub summary: String,
    pub count: usize,
    pub per_artist: usize,
}

const DIVE_CHAT: &str = "You help one listener make a playlist from what they picked (songs, albums or artists, described below) and what they asked for. Before it's made, you talk it over with them, the way a friend behind a record store counter would: ask one short question at a time about what would most change which songs belong (which part of the sound matters most, how far from the picks it may stray, an era, the mood, how strict to be). Where the songs come from (their own library, music new to them, or both) is already set on the page (WHERE FROM below): never ask about it, and don't offer to change it. If they ask you something (what you meant, a word, a band), answer briefly and plainly, then carry on with a question if one is still needed. Never ask what they've already told you, and ask only what you really need (HOW MANY QUESTIONS below says how many). Talk plainly, for a listener who may not know genre names: describe the music in everyday words (fast or slow, heavy or gentle, screamed or sung, raw or polished, dark or upbeat), and when a genre name helps, say in a few words what it sounds like. If they use genre names themselves, you can too. Keep each message short. When there's a last try (THE LAST TRY below), they've heard it and will tell you which songs hit the mark and which missed: work out what the hits share (the sound, the era, the pace) and what the misses got wrong, ask only if that's unclear, and put it in the summary, naming the hits and the misses. When you know enough, or they say to go ahead, set done to true and say in a sentence or two what you'll make. With every answer, fill in what's settled so far: summary (everything they want, for whoever picks the songs: what to match, what to avoid, how strict; here exact genre and style names help), count (how many songs: 30 until they say, 10 to 60), per_artist (songs by any one artist: 1 until they say, at most 3). Answer with JSON only, no prose around it: {\"count\": number, \"done\": true or false, \"per_artist\": number, \"say\": \"what you say to them\", \"summary\": \"...\"}";

fn dive_chat_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "count": { "type": "integer" },
            "done": { "type": "boolean" },
            "per_artist": { "type": "integer" },
            "say": { "type": "string" },
            "summary": { "type": "string" },
        },
        "required": ["count", "done", "per_artist", "say", "summary"],
    })
}

/// Tier 2's next turn in the talk before a Deep Dive (`brief`: the picks
/// and the request; `source` where the songs come from, "library", "new"
/// or "both", chosen on the page; `talk`: the turns so far, the listener's
/// marked true; `mode`: "make" (no questions: settled from their words and
/// made straight away) or "deeper" (at least one question, at most
/// DIVE_QUESTIONS)).
pub async fn dive_chat(llm: &Llm, brief: &str, source: &str, talk: &[(bool, String)], mode: &str) -> Result<DiveReply, String> {
    let mut user = brief.to_string();
    user.push_str(match source {
        "library" => "\nWHERE FROM: their own library only.\n",
        "new" => "\nWHERE FROM: music new to them only (artists they don't know).\n",
        _ => "\nWHERE FROM: their own library and music new to them, both.\n",
    });
    let asked = talk.iter().filter(|(listener, _)| !listener).count();
    let make = mode == "make";
    if make {
        user.push_str("\nHOW MANY QUESTIONS: none. They chose to have it made straight from their words: don't ask anything, set done to true, fill in summary, count and per_artist from what they asked for, and say in a sentence what you'll make.\n");
    } else if asked >= DIVE_QUESTIONS {
        user.push_str(&format!("\nHOW MANY QUESTIONS: you've asked {asked}, all there are: set done to true now and say in a sentence or two what you'll make.\n"));
    } else {
        user.push_str(&format!(
            "\nHOW MANY QUESTIONS: they chose a deeper dive, so ask at least one, then only as many as you need to make what they asked for, {DIVE_QUESTIONS} at most in all (asked so far: {asked}). A request that's already clear needs only one.\n"
        ));
    }
    if talk.is_empty() {
        user.push_str(if make { "\nYou haven't said anything yet.\n" } else { "\nYou haven't said anything yet: open the talk.\n" });
    } else {
        user.push_str("\nTHE TALK SO FAR:\n");
        for (listener, text) in talk {
            user.push_str(&format!("{}: {}\n", if *listener { "Listener" } else { "You" }, text.trim()));
        }
        user.push_str("\nYour turn.\n");
    }
    let text = complete(llm, DIVE_CHAT, &user, Some(&dive_chat_shape())).await?;
    let mut reply = parse_dive_reply(&text)?;
    // (the limits hold whatever it answered: made straight away, or made
    // once the last question is answered)
    if !reply.done && (make || asked >= DIVE_QUESTIONS) {
        reply.done = true;
        reply.say.clear();
    }
    Ok(reply)
}

/// The most questions a Deeper Dive asks.
pub const DIVE_QUESTIONS: usize = 4;

fn parse_dive_reply(text: &str) -> Result<DiveReply, String> {
    let v = json_in(text)?;
    let number = |k: &str| v[k].as_u64().or_else(|| v[k].as_str()?.trim().parse().ok()).map(|n| n as usize);
    let done = v["done"].as_bool().unwrap_or_else(|| v["done"].as_str().is_some_and(|s| s.trim().eq_ignore_ascii_case("true")));
    let say = v["say"].as_str().unwrap_or("").trim().to_string();
    if say.is_empty() && !done {
        return Err(format!("nothing said in the answer: {}", clip(text, 200)));
    }
    Ok(DiveReply {
        say,
        done,
        summary: v["summary"].as_str().unwrap_or("").trim().to_string(),
        count: number("count").unwrap_or(30).clamp(10, 60),
        per_artist: number("per_artist").unwrap_or(1).clamp(1, 3),
    })
}

const DIVE_LIBRARY: &str = "You build one listener's playlist from their own music library. From the numbered artists below (all in their library), choose the ones whose music can answer what they asked for, best first: judge by what you know of each artist's albums and sound (style, era, heaviness, vocals, production), not by shared fans or broad labels, and leave out artists you don't know. Answer with JSON only, no prose around it: {\"artists\": [the chosen artists' numbers]}";

fn numbers_shape(key: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { key: { "type": "array", "items": { "type": "integer" } } },
        "required": [key],
    })
}

/// The library artists (indices into `names`) that can answer the brief,
/// best first, at most `want`.
pub async fn dive_library_artists(llm: &Llm, brief: &str, names: &[String], want: usize) -> Result<Vec<usize>, String> {
    let mut user = format!("{brief}\nTHEIR LIBRARY'S ARTISTS:\n");
    for (n, name) in names.iter().enumerate() {
        user.push_str(&format!("{}. {name}\n", n + 1));
    }
    user.push_str(&format!("\nAt most {want} artists.\n"));
    let text = complete(llm, DIVE_LIBRARY, &user, Some(&numbers_shape("artists"))).await?;
    let mut out = parse_numbers(&text, "artists", names.len())?;
    out.truncate(want);
    Ok(out)
}

/// 1-based numbers under `key`, as indices (in range, once each).
fn parse_numbers(text: &str, key: &str, count: usize) -> Result<Vec<usize>, String> {
    let v = json_in(text)?;
    let mut out: Vec<usize> = Vec::new();
    for n in v[key].as_array().into_iter().flatten().filter_map(|n| n.as_u64().or_else(|| n.as_str()?.trim().parse().ok())) {
        let at = n as usize;
        if at >= 1 && at <= count && !out.contains(&(at - 1)) {
            out.push(at - 1);
        }
    }
    Ok(out)
}

const DIVE_NEW: &str = "You build one listener's playlist from music new to them. Suggest artists they have probably never heard whose specific albums answer what they asked for: real artists with released albums that streaming services carry, judged by the specifics of the music (style, era, heaviness, vocals, guitar sound, production, mood), not by shared fans or broad labels. For each, name the album that answers it best and, when you know them, the songs on that album that do (only songs you're sure are on it). Favor lesser-known artists over the well-known names they know already (the artists they play most are listed). Artists that share listeners with their picks may be listed too: some may fit, many won't (shared fans are not the same sound), so suggest only the ones whose music answers the request, alongside your own. Never suggest an artist of their picks, one they know, or one from the list to leave out. Answer with JSON only, no prose around it: {\"picks\": [{\"album\": \"its album that answers the request\", \"artist\": \"the suggested artist\", \"songs\": [\"songs on that album that do\"], \"why\": \"how it answers the request, at most 80 characters\"}]}";

fn dive_new_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "picks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "album": { "type": "string" },
                        "artist": { "type": "string" },
                        "songs": { "type": "array", "items": { "type": "string" } },
                        "why": { "type": "string" },
                    },
                    "required": ["album", "artist", "songs", "why"],
                },
            },
        },
        "required": ["picks"],
    })
}

/// An artist new to the listener, with the album (and songs) tier 2 means.
#[derive(Debug, Clone, Default)]
pub struct DiveNew {
    pub artist: String,
    pub album: String,
    pub songs: Vec<String>,
    pub why: String,
}

/// Artists new to the listener for the brief, about `want` of them
/// (`hints`: artists sharing listeners with the picks, to consider;
/// `part`: which part of the ask this is, when it's asked in parts).
pub async fn dive_new_artists(llm: &Llm, brief: &str, favorites: &[String], skip: &[String], hints: &[String], want: usize, part: &str) -> Result<Vec<DiveNew>, String> {
    let mut user = brief.to_string();
    if !favorites.is_empty() {
        user.push_str(&format!("\nThe artists they play most (they know these, and their styles' well-known names): {}\n", favorites.join(", ")));
    }
    if !hints.is_empty() {
        user.push_str(&format!("\nArtists that share listeners with their picks (they don't know these; consider them, suggest only the ones that fit): {}\n", hints.join(", ")));
    }
    if !skip.is_empty() {
        user.push_str(&format!("\nLeave out: {}\n", skip.join(", ")));
    }
    if !part.is_empty() {
        user.push_str(&format!("\n{part}\n"));
    }
    user.push_str(&format!("\n{want} artists, fewer only if you truly can't name more that fit.\n"));
    let text = complete(llm, DIVE_NEW, &user, Some(&dive_new_shape())).await?;
    parse_dive_new(&text)
}

fn parse_dive_new(text: &str) -> Result<Vec<DiveNew>, String> {
    let v = json_in(text)?;
    let field = |p: &serde_json::Value, k: &str| p[k].as_str().unwrap_or("").trim().to_string();
    Ok(v["picks"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| DiveNew {
                    artist: field(p, "artist"),
                    album: field(p, "album"),
                    songs: p["songs"].as_array().into_iter().flatten().filter_map(|s| s.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    why: field(p, "why"),
                })
                .filter(|s| !s.artist.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

const DIVE_ALBUMS: &str = "You build one listener's playlist. For each numbered artist below, choose which of their listed albums can answer what they asked for, best first, judged by what you know of each album's sound (style, era, heaviness, vocals, guitar sound, production, mood); a band's sound moves over a career, so pick the albums, not the band. Leave an artist out when none of its albums fits, and when its listed albums aren't the ones you know under that name (another act with the same name). A note says why an artist was suggested, when it was. Answer with JSON only, no prose around it: {\"artists\": [{\"albums\": [album numbers, best first], \"n\": the artist's number}]}";

fn dive_albums_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "artists": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "albums": { "type": "array", "items": { "type": "integer" } },
                        "n": { "type": "integer" },
                    },
                    "required": ["albums", "n"],
                },
            },
        },
        "required": ["artists"],
    })
}

/// One artist for `dive_albums`: its name, why it was suggested (or
/// nothing), and its albums ("Title (year)").
#[derive(Debug, Clone, Default)]
pub struct DiveArtistAlbums {
    pub artist: String,
    pub note: String,
    pub albums: Vec<String>,
}

/// For each artist (index into `artists`), the albums chosen (indices into
/// its list), best first, at most `each`.
pub async fn dive_albums(llm: &Llm, brief: &str, artists: &[DiveArtistAlbums], each: usize) -> Result<Vec<(usize, Vec<usize>)>, String> {
    let mut user = format!("{brief}\nTHE ARTISTS AND THEIR ALBUMS:\n");
    for (n, a) in artists.iter().enumerate() {
        user.push_str(&format!("{}. {}{}\n", n + 1, a.artist, if a.note.is_empty() { String::new() } else { format!(" (note: {})", a.note) }));
        for (m, album) in a.albums.iter().enumerate() {
            user.push_str(&format!("   {}. {album}\n", m + 1));
        }
    }
    user.push_str(&format!("\nAt most {each} albums per artist.\n"));
    let text = complete(llm, DIVE_ALBUMS, &user, Some(&dive_albums_shape())).await?;
    parse_dive_albums(&text, artists, each)
}

fn parse_dive_albums(text: &str, artists: &[DiveArtistAlbums], each: usize) -> Result<Vec<(usize, Vec<usize>)>, String> {
    let v = json_in(text)?;
    let num = |x: &serde_json::Value| x.as_u64().or_else(|| x.as_str()?.trim().parse().ok()).map(|n| n as usize);
    let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
    for a in v["artists"].as_array().into_iter().flatten() {
        let Some(n) = num(&a["n"]).filter(|n| *n >= 1 && *n <= artists.len()) else { continue };
        if out.iter().any(|(i, _)| *i == n - 1) {
            continue;
        }
        let count = artists[n - 1].albums.len();
        let mut albums: Vec<usize> = Vec::new();
        for m in a["albums"].as_array().into_iter().flatten().filter_map(num) {
            if m >= 1 && m <= count && !albums.contains(&(m - 1)) && albums.len() < each {
                albums.push(m - 1);
            }
        }
        if !albums.is_empty() {
            out.push((n - 1, albums));
        }
    }
    Ok(out)
}

const DIVE_SONGS: &str = "You build one listener's playlist. For each numbered album below, choose the songs on it that best answer what they asked for, best first, or none when the album doesn't fit after all. Go by what's written about the album (a review that singles out songs counts for a lot), each song's title, length and place on the album, and what you know of the songs themselves. Songs marked popular are the band's best known on the streaming service: that alone is no reason to choose them. A song may carry what listening to it found: sound, how close it sounds to their picks (from a model trained to tell artists apart by their sound: 0.90 and up very close, around 0.80 related, under 0.70 far), and styles, what it sounds like among Discogs' 400 styles. When they ask for the sound of their picks (guitars, vocals, pace, production, feel), let sound weigh heavily, more than an album's reputation or a song's popularity; when they ask for something their picks don't have (another genre, a mood, an era), go by the request first. Styles help with words like heavier, rawer, cleaner or not atmospheric. A song without them wasn't heard. Answer with JSON only, no prose around it: {\"albums\": [{\"n\": the album's number, \"songs\": [{\"n\": the song's number, \"why\": \"why it answers the request, at most 80 characters\"}]}]}";

fn dive_songs_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "albums": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "n": { "type": "integer" },
                        "songs": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "n": { "type": "integer" },
                                    "why": { "type": "string" },
                                },
                                "required": ["n", "why"],
                            },
                        },
                    },
                    "required": ["n", "songs"],
                },
            },
        },
        "required": ["albums"],
    })
}

/// For each album (`blocks`: what's written about it and its numbered
/// songs; `counts`: how many songs each has), the songs chosen (album
/// index, song index, why), best first, at most `each` per album.
pub async fn dive_album_songs(llm: &Llm, brief: &str, blocks: &[String], counts: &[usize], each: usize) -> Result<Vec<(usize, usize, String)>, String> {
    let mut user = format!("{brief}\nTHE ALBUMS:\n");
    for (n, block) in blocks.iter().enumerate() {
        user.push_str(&format!("\n{}. {block}\n", n + 1));
    }
    user.push_str(&format!("\nAt most {each} songs per album.\n"));
    let text = complete(llm, DIVE_SONGS, &user, Some(&dive_songs_shape())).await?;
    parse_dive_album_songs(&text, counts, each)
}

fn parse_dive_album_songs(text: &str, counts: &[usize], each: usize) -> Result<Vec<(usize, usize, String)>, String> {
    let v = json_in(text)?;
    let num = |x: &serde_json::Value| x.as_u64().or_else(|| x.as_str()?.trim().parse().ok()).map(|n| n as usize);
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    for a in v["albums"].as_array().into_iter().flatten() {
        let Some(n) = num(&a["n"]).filter(|n| *n >= 1 && *n <= counts.len()) else { continue };
        let mut taken = 0;
        for song in a["songs"].as_array().into_iter().flatten() {
            let Some(m) = num(&song["n"]).filter(|m| *m >= 1 && *m <= counts[n - 1]) else { continue };
            if taken >= each || out.iter().any(|(x, y, _)| *x == n - 1 && *y == m - 1) {
                continue;
            }
            taken += 1;
            out.push((n - 1, m - 1, song["why"].as_str().unwrap_or("").trim().to_string()));
        }
    }
    Ok(out)
}

const DIVE_CHOOSE: &str = "You make one listener's playlist from the numbered candidate songs below: choose the ones that best answer what they asked for, judged by the specifics of each song and its album (style, era, heaviness, vocals, guitar sound, production, mood), and leave out the ones that don't, even if that makes the list shorter. Some candidates carry what listening to them found: sound, how close the song sounds to their picks (0.90 and up very close, under 0.70 far), and the styles heard in it; weigh them as the request asks, heavily when it's about the sound of their picks. Keep to the per-artist limit you're given. Order the chosen songs so the playlist opens strong and flows, pace and mood varied, the same artist kept apart. Give the playlist a short title of a few words that says what it is (no quotes, no emoji). Answer with JSON only, no prose around it: {\"songs\": [{\"n\": the song's number, \"why\": \"why it belongs, at most 80 characters\"}], \"title\": \"...\"}";

fn dive_choose_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "songs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "n": { "type": "integer" },
                        "why": { "type": "string" },
                    },
                    "required": ["n", "why"],
                },
            },
            "title": { "type": "string" },
        },
        "required": ["songs", "title"],
    })
}

/// The chosen songs (indices into `lines`, in play order, with why) and a
/// title, at most `want` songs and `per_artist` per artist.
pub async fn dive_choose(llm: &Llm, brief: &str, lines: &[String], want: usize, per_artist: usize) -> Result<(String, Vec<(usize, String)>), String> {
    let mut user = format!("{brief}\nCANDIDATE SONGS (artist - song (album, year)):\n");
    for (n, line) in lines.iter().enumerate() {
        user.push_str(&format!("{}. {line}\n", n + 1));
    }
    user.push_str(&format!("\nAt most {want} songs, at most {per_artist} by any one artist.\n"));
    let text = complete(llm, DIVE_CHOOSE, &user, Some(&dive_choose_shape())).await?;
    parse_dive_choice(&text, lines.len())
}

fn parse_dive_choice(text: &str, count: usize) -> Result<(String, Vec<(usize, String)>), String> {
    let v = json_in(text)?;
    let title: String = v["title"].as_str().unwrap_or("").trim().trim_matches('"').chars().take(60).collect();
    let mut out: Vec<(usize, String)> = Vec::new();
    for song in v["songs"].as_array().into_iter().flatten() {
        let n = song["n"].as_u64().or_else(|| song["n"].as_str()?.trim().parse().ok());
        let Some(n) = n.map(|n| n as usize).filter(|n| *n >= 1 && *n <= count) else { continue };
        if out.iter().any(|(i, _)| *i == n - 1) {
            continue;
        }
        out.push((n - 1, song["why"].as_str().unwrap_or("").trim().to_string()));
    }
    Ok((title, out))
}

/// One song of the list as the order prompt shows it.
pub struct ListLine {
    pub artist: String,
    pub song: String,
    pub album: String,
    pub year: Option<u32>,
    /// The played artist it was picked for.
    pub seed: String,
    /// How likely the listener enjoys it, as tier 2 judged it.
    pub liking: f64,
    pub why: String,
}

const ORDER: &str = "You put one listener's weekly discovery playlist in order. Each numbered line is a song, the artist they play that it was picked for, how likely they are to enjoy it (0 to 1), and why it fits. Order the whole list: the surest and strongest songs first so the list opens well, songs picked for one of their requests in the first half, songs picked for the same played artist kept apart, and the pace and mood varied from one song to the next, with no long run of one style. Answer with JSON only, no prose around it: {\"order\": [every line number, in the new order]}";

fn order_shape() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "order": { "type": "array", "items": { "type": "integer" } } },
        "required": ["order"],
    })
}

/// The list in the model's order: positions in `lines`, every one once (any
/// it left out follow in their old order).
pub async fn order(llm: &Llm, lines: &[ListLine]) -> Result<Vec<usize>, String> {
    let mut user = String::new();
    for (n, l) in lines.iter().enumerate() {
        let year = l.year.map(|y| format!(", {y}")).unwrap_or_default();
        user.push_str(&format!("{}. {} - {} ({}{year}) | for {} | {:.2} | {}\n", n + 1, l.artist, l.song, l.album, l.seed, l.liking, l.why));
    }
    let text = complete(llm, ORDER, &user, Some(&order_shape())).await?;
    parse_order(&text, lines.len())
}

fn parse_order(text: &str, count: usize) -> Result<Vec<usize>, String> {
    let v = json_in(text)?;
    let given = v["order"].as_array().ok_or("no order in the answer")?;
    let mut out: Vec<usize> = Vec::new();
    for n in given.iter().filter_map(|n| n.as_u64().or_else(|| n.as_str()?.trim().parse().ok())) {
        let at = n as usize;
        if at >= 1 && at <= count && !out.contains(&(at - 1)) {
            out.push(at - 1);
        }
    }
    if out.len() < count / 2 {
        return Err(format!("the order named only {} of {count} songs", out.len()));
    }
    for at in 0..count {
        if !out.contains(&at) {
            out.push(at);
        }
    }
    Ok(out)
}

/// One completion from the chosen service: the answer's text. `shape` is a
/// JSON schema the answer must follow, where the service can enforce one
/// (Ollama; the others are asked for JSON in general).
pub async fn complete(llm: &Llm, system: &str, user: &str, shape: Option<&serde_json::Value>) -> Result<String, String> {
    let local = llm.provider == "ollama";
    // (a local model that thinks first takes many times as long per pick,
    // and can think its way out of room: off when the model allows)
    let think = if local { crate::ollama::least_thinking(&llm.url, &llm.model).await } else { None };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(if llm.wait_secs > 0 {
            llm.wait_secs
        } else if local {
            OLLAMA_WAIT_SECS
        } else {
            ONLINE_WAIT_SECS
        }))
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
            client.post(format!("{}/api/chat", llm.url)).json(&{
                let mut body = serde_json::json!({
                    "model": llm.model,
                    "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
                    "stream": false,
                    "format": shape.cloned().unwrap_or_else(|| serde_json::json!("json")),
                    "keep_alive": OLLAMA_KEEP_ALIVE,
                    "options": { "temperature": 0.2, "num_ctx": OLLAMA_CONTEXT },
                });
                if let Some(think) = think {
                    body["think"] = think;
                }
                body
            }),
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
                    // (room for the model's thinking, which counts here too:
                    // 400 cut answers off mid-JSON, 8192 a long Heavy list;
                    // only what's used is billed)
                    "max_tokens": 16000,
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
    let response = request.send().await.map_err(|e| {
        if local && e.is_connect() {
            format!("Ollama isn't answering at {}. Is it running?", llm.url)
        } else if e.is_timeout() {
            format!("{} took too long to answer", service_name(&llm.provider))
        } else {
            format!("{}: {e}", service_name(&llm.provider))
        }
    })?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    // (an answer cut short, or declined, says so rather than "no JSON")
    match body["stop_reason"].as_str().or_else(|| body["choices"][0]["finish_reason"].as_str()).or_else(|| body["done_reason"].as_str()) {
        Some("max_tokens") | Some("length") => return Err(format!("{} ran out of room before finishing its answer", service_name(&llm.provider))),
        Some("refusal") => return Err(format!("{} declined to answer", service_name(&llm.provider))),
        _ => {}
    }
    if local && status.as_u16() == 404 {
        return Err(format!("Ollama doesn't have {} yet. Get it with: ollama pull {}", llm.model, llm.model));
    }
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
    // (Wrapped's Ollama started for the test when it isn't running)
    let _ollama = if llm.provider == "ollama" { Some(crate::ollama::lease(&app).await?.0) } else { None };
    // (the same for everyone: no genre assumed)
    let text = complete(&llm, "Answer with JSON only.", "Greet someone who just connected you to their music player, in at most ten words, as {\"hello\": \"...\"}.", None).await?;
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(text.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim());
    let hello = parsed.ok().and_then(|v| v["hello"].as_str().map(str::to_string)).unwrap_or(text);
    Ok(format!("Connected. {} ({}) says: {}", service_name(&llm.provider), llm.model, hello.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dive_answers() {
        let r = parse_dive_reply(r#"{"count":"80","done":"true","per_artist":0,"say":"Got it.","summary":"guitars like the picks"}"#).unwrap();
        assert!(r.done);
        assert_eq!((r.count, r.per_artist), (60, 1));
        let r = parse_dive_reply(r#"{"done":false,"say":"How strict should it be?"}"#).unwrap();
        assert_eq!((r.count, r.per_artist, r.done), (30, 1, false));
        assert!(parse_dive_reply(r#"{"done":false,"say":""}"#).is_err());
        let n = parse_numbers(r#"{"artists":[3,"1",3,9,0]}"#, "artists", 5).unwrap();
        assert_eq!(n, vec![2, 0]);
        let new = parse_dive_new(r#"{"picks":[{"album":"Hero","artist":"Bölzer","songs":["Hero"," ",""],"why":"x"},{"artist":""}]}"#).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].songs, vec!["Hero"]);
        let artists = vec![
            DiveArtistAlbums { artist: "A".into(), albums: vec!["One".into(), "Two".into(), "Three".into()], ..Default::default() },
            DiveArtistAlbums { artist: "B".into(), albums: vec!["Only".into()], ..Default::default() },
        ];
        let chosen = parse_dive_albums(r#"{"artists":[{"n":1,"albums":[3,3,1,2]},{"n":"2","albums":[4]},{"n":1,"albums":[2]}]}"#, &artists, 2).unwrap();
        assert_eq!(chosen, vec![(0, vec![2, 0])]);
        let songs = parse_dive_album_songs(r#"{"albums":[{"n":2,"songs":[{"n":5,"why":"long and epic"},{"n":5,"why":"again"},{"n":9},{"n":1,"why":"b"},{"n":2,"why":"c"}]}]}"#, &[3, 6], 2).unwrap();
        assert_eq!(songs, vec![(1, 4, "long and epic".to_string()), (1, 0, "b".to_string())]);
        let (title, songs) = parse_dive_choice(r#"Sure: {"songs":[{"n":2,"why":"a"},{"n":"5","why":"b"},{"n":2,"why":"c"},{"n":7}],"title":"\"Bolzer Guitars\""}"#, 5).unwrap();
        assert_eq!(title, "Bolzer Guitars");
        assert_eq!(songs, vec![(1, "a".to_string()), (4, "b".to_string())]);
    }

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
    fn reads_heavy_answers() {
        let picks = parse_suggestions(r#"{"picks": [{"seed": "Witchery", "artist": "Nekromantheon", "album": "Rise, Vulcan Spectre", "why": "raw blackened thrash"}, {"seed": "", "artist": "x"}]}"#).unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].artist, "Nekromantheon");
        assert_eq!(parse_order(r#"{"order": [3, 1, "2", 3, 9]}"#, 4).unwrap(), vec![2, 0, 1, 3]);
        assert!(parse_order(r#"{"order": [1]}"#, 6).is_err());
    }

    #[test]
    fn clips_at_a_sentence() {
        let long = "One sentence here. Another one follows it. And a third that runs on and on.";
        assert_eq!(clip(long, 45), "One sentence here. Another one follows it.");
        assert_eq!(clip("short", 40), "short");
    }
}
