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
}

static NAV: Mutex<RoonNav> = Mutex::new(RoonNav {
    page: String::new(),
    album_id: None,
    last_album_id: None,
    available: false,
});

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
        let mut tick = tokio::time::interval(POLL);
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
                    if let Some((page, album_id)) = parse(line) {
                        if album_id.is_some() {
                            nav.last_album_id = album_id;
                        }
                        if nav.page != page || nav.album_id != album_id {
                            nav.page = page;
                            nav.album_id = album_id;
                            changed = true;
                        }
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

/// A `UI-NAV:` line: the page's name ("album details" -> "album", "artist
/// details" -> "artist", "home", or whatever Roon calls another page) and,
/// for an album page, the album id.
fn parse(line: &str) -> Option<(String, Option<i64>)> {
    let rest = line.split("UI-NAV: ").nth(1)?.trim();
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
    Some((page, album_id))
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
        (true, lines.into_iter().filter(|l| l.contains("UI-NAV: ")).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_pages() {
        assert_eq!(
            parse("10/15 03:38:30 Debug: UI-NAV: album details / album id: 825903 / bookmarkdata:"),
            Some(("album".into(), Some(825903)))
        );
        assert_eq!(
            parse("10/15 03:38:25 Debug: UI-NAV: artist details / performer: [object X] / bookmarkdata:"),
            Some(("artist".into(), None))
        );
        assert_eq!(parse("10/15 03:38:00 Debug: UI-NAV: home / bookmarkdata:"), Some(("home".into(), None)));
        assert_eq!(parse("10/15 03:38:00 Debug: UI-FWD: mode: home"), None);
    }
}
