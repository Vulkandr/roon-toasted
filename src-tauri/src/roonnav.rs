// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which page Roon's own window is on, for the Wrapper's Wrapped menu
//! ("This album" opens the album Roon is showing).
//!
//! Neither of Roon's protocols reports what its app is showing (the pages
//! are the app's own business), but the Roon app writes every page change
//! to its log, `%LOCALAPPDATA%\Roon\Logs\Roon_log.txt`, as lines like
//!
//!   10/15 03:38:30 Debug: UI-NAV: album details / album id: 825903 / bookmarkdata:
//!   10/15 03:38:25 Debug: UI-NAV: artist details / performer: [object ...] / bookmarkdata:
//!   10/15 03:38:00 Debug: UI-NAV: home / bookmarkdata:
//!
//! so the tail of that file is read twice a second (a few bytes a time;
//! the file is Roon's and is only ever read). Album pages carry the album's
//! library id, which the library link resolves; artist pages don't carry
//! anything, so the last album seen is kept (its artist is the usual
//! answer). The log rotates (Roon_log.01.txt...) and the reader starts over
//! when the file shrinks.
//!
//! Roon's back and forward buttons log only the kind of page they go to
//! (`UI-BACK: mode: home`, `UI-FWD: mode: albumdetails`), so a copy of
//! Roon's history is kept here: every `UI-NAV` is a new page (it clears the
//! forward list), `UI-BACK` steps back in it and `UI-FWD` steps forward.
//! A new page logs `UI-FWD` right before its `UI-NAV`, so a forward step is
//! undone again when a `UI-NAV` follows it. Whenever the kind of page Roon
//! names doesn't match the copy, the copy is dropped and only the kind is
//! known until the next new page.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Where Roon is, as last seen in its log.
#[derive(Serialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoonNav {
    /// "album", "artist", "home", another page's name as Roon logs it, or
    /// "" while nothing has been seen yet.
    pub page: String,
    /// The library album id of the album page Roon shows (page "album").
    pub album_id: Option<i64>,
    /// The last album page seen, kept while Roon is on other pages (an
    /// artist page is usually that album's artist).
    pub last_album_id: Option<i64>,
    /// Whether the log file was found (Roon installed where it usually is).
    pub available: bool,
    /// Counts every navigation Roon logged, also to the page it was already
    /// on (a sidebar click on the current page), so the Wrapper can react
    /// to the click itself.
    pub nav_count: u64,
    /// The Core image keys Roon fetched in the seconds after the last
    /// navigation (an artist page's own pictures among them; the log names
    /// no performer, but a library artist can be told by its picture).
    pub image_keys: Vec<String>,
}

static NAV: Mutex<RoonNav> = Mutex::new(RoonNav {
    page: String::new(),
    album_id: None,
    last_album_id: None,
    available: false,
    nav_count: 0,
    image_keys: Vec::new(),
});

/// How long after a navigation (by the log's own clock) image fetches
/// still count as that page's. The log's times, not ours: the first read
/// after a start catches up on old lines all at once.
const IMAGES_FOR_SECS: u32 = 3;

/// A log line's time of day in seconds ("10/16 03:46:59 Debug: ...").
fn log_secs(line: &str) -> Option<u32> {
    let time = line.get(6..14)?;
    let mut parts = time.split(':').map(|p| p.parse::<u32>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    (h < 24 && m < 60 && s < 60).then_some(h * 3600 + m * 60 + s)
}
const IMAGE_KEYS_MAX: usize = 16;

/// The key of a Core image Roon fetched ("GET to http://core:9330/image/<key>.<size>.jpg").
fn image_key_of(line: &str) -> Option<String> {
    let rest = line.split("/image/").nth(1)?;
    let key: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (!key.is_empty() && line.contains("[easyhttp]")).then_some(key)
}

/// One page in the copy of Roon's history.
#[derive(Clone, PartialEq, Debug)]
struct Entry {
    page: String,
    album_id: Option<i64>,
}

/// The copy of Roon's history: the pages behind (the last is the current
/// one) and the pages ahead (for its forward button).
#[derive(Default)]
struct History {
    back: Vec<Entry>,
    forward: Vec<Entry>,
    /// The last line stepped forward in the copy; a `UI-NAV` right after
    /// means that was a new page, not the forward button, so it's undone.
    stepped_forward: bool,
}

/// What a log line says about navigation.
#[derive(Debug, PartialEq)]
enum Step {
    /// A new page (the kind, and an album page's id).
    New(String, Option<i64>),
    Back(String),
    Forward(String),
}

impl History {
    /// Applies a step; the page Roon is on afterwards, when it's known.
    fn apply(&mut self, step: Step) -> Option<Entry> {
        let stepped = std::mem::replace(&mut self.stepped_forward, false);
        match step {
            Step::New(page, album_id) => {
                if stepped {
                    // That `UI-FWD` was this new page, not the forward button
                    if let Some(undone) = self.back.pop() {
                        self.forward.push(undone);
                    }
                }
                self.forward.clear();
                // An artist page reached from an album is that album's
                // artist (the log names no performer); from anywhere else
                // the artist stays unknown
                let album_id = match (page.as_str(), album_id) {
                    ("artist", None) => self.back.last().filter(|e| e.page == "album").and_then(|e| e.album_id),
                    (_, id) => id,
                };
                let entry = Entry { page, album_id };
                self.back.push(entry.clone());
                Some(entry)
            }
            Step::Back(page) => {
                if self.back.len() >= 2 {
                    let leaving = self.back.pop().unwrap();
                    if self.back.last().map(|e| e.page == page) == Some(true) {
                        self.forward.push(leaving);
                        return self.back.last().cloned();
                    }
                }
                self.reset(page)
            }
            Step::Forward(page) => {
                // The forward button, when the page ahead is of that kind;
                // otherwise most likely the `UI-FWD` of a new page (its
                // `UI-NAV` follows), so the copy is left alone and only the
                // kind is known for now
                if self.forward.last().is_some_and(|next| next.page == page) {
                    let next = self.forward.pop().unwrap();
                    self.back.push(next.clone());
                    self.stepped_forward = true;
                    return Some(next);
                }
                Some(Entry { page, album_id: None })
            }
        }
    }

    /// The copy doesn't match what Roon says: only the kind of page is known.
    fn reset(&mut self, page: String) -> Option<Entry> {
        self.back.clear();
        self.forward.clear();
        let entry = Entry { page, album_id: None };
        self.back.push(entry.clone());
        Some(entry)
    }
}

/// How often the log is looked at.
const POLL: Duration = Duration::from_millis(500);
/// On a first look (and after a rotation that left a big file), only this
/// much of the end is read: enough for the page Roon is on.
const TAIL: u64 = 64 * 1024;
/// More than this behind (the Wrapper was asleep, Roon logged a lot): skip
/// ahead rather than read it all.
const CATCH_UP: u64 = 1024 * 1024;

/// The page Roon is on, as last seen.
#[tauri::command]
pub fn wrapper_nav() -> RoonNav {
    NAV.lock().unwrap().clone()
}

/// Starts following the log (in setup). Changes go to the Wrapper's page as
/// "roon-nav" events with a `RoonNav`.
pub fn start(app: &AppHandle) {
    let Some(path) = log_path() else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut reader = LogReader { path, offset: None, partial: String::new() };
        let mut history = History::default();
        let mut tick = tokio::time::interval(POLL);
        let mut last_step: Option<u32> = None;
        loop {
            tick.tick().await;
            let (available, lines) = reader.poll();
            let mut changed = false;
            {
                let mut nav = NAV.lock().unwrap();
                if nav.available != available {
                    nav.available = available;
                    changed = true;
                }
                for line in &lines {
                    if let Some(key) = image_key_of(line) {
                        let recent = match (log_secs(line), last_step) {
                            (Some(at), Some(step)) => (at + 86_400 - step) % 86_400 <= IMAGES_FOR_SECS,
                            _ => false,
                        };
                        if recent && nav.image_keys.len() < IMAGE_KEYS_MAX && !nav.image_keys.contains(&key) {
                            nav.image_keys.push(key);
                            changed = true;
                        }
                        continue;
                    }
                    let Some(step) = parse(line) else { continue };
                    let Some(entry) = history.apply(step) else { continue };
                    nav.nav_count += 1;
                    nav.image_keys.clear();
                    last_step = log_secs(line);
                    changed = true;
                    if entry.album_id.is_some() {
                        nav.last_album_id = entry.album_id;
                    }
                    if nav.page != entry.page || nav.album_id != entry.album_id {
                        nav.page = entry.page;
                        nav.album_id = entry.album_id;
                        changed = true;
                    }
                }
            }
            if changed {
                let nav = NAV.lock().unwrap().clone();
                let _ = app.emit_to(crate::wrapper::WRAPPER_WINDOW, "roon-nav", &nav);
            }
        }
    });
}

fn log_path() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let mut path = PathBuf::from(local);
    path.push("Roon");
    path.push("Logs");
    path.push("Roon_log.txt");
    Some(path)
}

/// A navigation line: `UI-NAV:` (a new page: its kind, "album details" ->
/// "album", "artist details" -> "artist", "home", or whatever Roon calls
/// it, and an album page's id), `UI-BACK: mode:` or `UI-FWD: mode:` (the
/// kind only: "albumdetails" -> "album", "artistdetails" -> "artist").
fn parse(line: &str) -> Option<Step> {
    if let Some(rest) = line.split("UI-NAV: ").nth(1) {
        let rest = rest.trim();
        let kind = rest.split(" / ").next().unwrap_or("").trim();
        let page = match kind {
            "album details" => "album".to_string(),
            "artist details" => "artist".to_string(),
            other => other.to_string(),
        };
        let album_id = rest
            .split("album id:")
            .nth(1)
            .map(|s| s.trim())
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|digits| digits.parse().ok());
        let album_id = if page == "album" { album_id } else { None };
        return Some(Step::New(page, album_id));
    }
    let mode_page = |mode: &str| match mode.trim() {
        "albumdetails" => "album".to_string(),
        "artistdetails" => "artist".to_string(),
        other => other.to_string(),
    };
    if let Some(mode) = line.split("UI-BACK: mode: ").nth(1) {
        return Some(Step::Back(mode_page(mode)));
    }
    if let Some(mode) = line.split("UI-FWD: mode: ").nth(1) {
        return Some(Step::Forward(mode_page(mode)));
    }
    None
}

/// Reads whatever was added to the log since the last look.
struct LogReader {
    path: PathBuf,
    /// Where the next read starts; None before the first look.
    offset: Option<u64>,
    /// The end of the last read, if it stopped mid-line.
    partial: String,
}

impl LogReader {
    /// Whether the file is there, and the complete new lines.
    fn poll(&mut self) -> (bool, Vec<String>) {
        let Ok(len) = std::fs::metadata(&self.path).map(|m| m.len()) else {
            self.offset = None;
            self.partial.clear();
            return (false, Vec::new());
        };
        let mut start = match self.offset {
            // First look: just the end
            None => len.saturating_sub(TAIL),
            // Shrunk: rotated or truncated, start over
            Some(offset) if len < offset => 0,
            Some(offset) => offset,
        };
        if len - start > CATCH_UP {
            start = len - TAIL;
            self.partial.clear();
        }
        if start >= len {
            self.offset = Some(len);
            return (true, Vec::new());
        }
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return (false, Vec::new());
        };
        let mut bytes = Vec::with_capacity((len - start) as usize);
        if file.seek(SeekFrom::Start(start)).is_err()
            || file.take(len - start).read_to_end(&mut bytes).is_err()
        {
            return (true, Vec::new());
        }
        self.offset = Some(start + bytes.len() as u64);
        let text = String::from_utf8_lossy(&bytes);
        let mut buffer = std::mem::take(&mut self.partial);
        buffer.push_str(&text);
        let mut lines: Vec<String> = buffer.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
        // The last piece is only a whole line if the text ended with a newline
        if !buffer.ends_with('\n') {
            self.partial = lines.pop().unwrap_or_default();
        } else {
            lines.pop(); // the empty piece after the final newline
        }
        (
            true,
            lines
                .into_iter()
                .filter(|l| l.contains("UI-NAV: ") || l.contains("UI-BACK: ") || l.contains("UI-FWD: ") || (l.contains("[easyhttp]") && l.contains("/image/")))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{log_secs, parse, Entry, History, Step};

    #[test]
    fn parses_pages() {
        assert_eq!(
            parse("10/15 03:38:30 Debug: UI-NAV: album details / album id: 825903 / bookmarkdata:"),
            Some(Step::New("album".into(), Some(825903)))
        );
        assert_eq!(
            parse("10/15 03:38:25 Debug: UI-NAV: artist details / performer: [object X] / bookmarkdata:"),
            Some(Step::New("artist".into(), None))
        );
        assert_eq!(parse("10/15 03:38:00 Debug: UI-NAV: home / bookmarkdata:"), Some(Step::New("home".into(), None)));
        assert_eq!(log_secs("10/16 03:46:59 Debug: UI-NAV: home"), Some(3 * 3600 + 46 * 60 + 59));
        assert_eq!(log_secs("garbage"), None);
        assert_eq!(parse("10/15 16:56:16 Debug: UI-BACK: mode: home"), Some(Step::Back("home".into())));
        assert_eq!(parse("10/15 16:56:17 Debug: UI-FWD: mode: albumdetails"), Some(Step::Forward("album".into())));
        assert_eq!(parse("10/15 03:38:00 Debug: GMS: saving nav stack"), None);
    }

    fn page(p: &str, id: Option<i64>) -> Option<Entry> {
        Some(Entry { page: p.into(), album_id: id })
    }

    #[test]
    fn follows_back_and_forward() {
        let mut h = History::default();
        // Roon logs UI-FWD then UI-NAV for every new page
        assert_eq!(h.apply(Step::Forward("home".into())), page("home", None));
        assert_eq!(h.apply(Step::New("home".into(), None)), page("home", None));
        h.apply(Step::Forward("artist".into()));
        assert_eq!(h.apply(Step::New("artist".into(), None)), page("artist", None));
        h.apply(Step::Forward("album".into()));
        assert_eq!(h.apply(Step::New("album".into(), Some(7))), page("album", Some(7)));
        // Back twice, forward twice: the album id comes back with it
        assert_eq!(h.apply(Step::Back("artist".into())), page("artist", None));
        assert_eq!(h.apply(Step::Back("home".into())), page("home", None));
        assert_eq!(h.apply(Step::Forward("artist".into())), page("artist", None));
        assert_eq!(h.apply(Step::Forward("album".into())), page("album", Some(7)));
        // A new page from the middle of the history drops what was ahead
        h.apply(Step::Back("artist".into()));
        h.apply(Step::Forward("album".into()));
        assert_eq!(h.apply(Step::New("album".into(), Some(9))), page("album", Some(9)));
        assert_eq!(h.apply(Step::Back("artist".into())), page("artist", None));
        assert_eq!(h.apply(Step::Forward("album".into())), page("album", Some(9)));
        // A kind that doesn't match the copy: only the kind is known
        assert_eq!(h.apply(Step::Back("home".into())), page("home", None));
    }
}
