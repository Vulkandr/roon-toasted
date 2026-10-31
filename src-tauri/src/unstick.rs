// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The stuck-at-end watcher. Roon sometimes stops at the end of a song and
//! never starts the next one in the queue: the zone still says it's
//! playing, but the position sits on the song's last second. Every second
//! this looks at each zone, and when one has sat there, playing, for
//! `STUCK_SECS`, it sends Next (once for that song). Not when there's
//! nothing to go on to, and not in repeat-one, where Roon starts the same
//! song over itself. Settings' General > Skip Stuck Songs turns it off.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use roon_api::ControlAction;
use tauri::{AppHandle, Manager};

use crate::zones::{ZoneView, Zones};

/// How long a playing zone may sit on its song's last second.
const STUCK_SECS: u64 = 8;

/// "The last second": this close to the song's length (Roon's positions are
/// whole seconds, and a song can stop a second short of its length).
const END_SLACK: f64 = 1.5;

/// Skip Stuck Songs (set at start and on every settings change).
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// What the watcher remembers of a zone: the song, where it was, since when.
struct Seen {
    song: String,
    position: f64,
    since: Instant,
    /// Next already sent for this song (it's not sent twice).
    nudged: bool,
}

/// A song, told apart from the one before it.
fn song_of(zone: &ZoneView) -> Option<String> {
    let np = zone.now_playing.as_ref()?;
    Some(format!("{}|{}|{}|{:?}", np.title, np.artist_full, np.album, np.length))
}

/// Whether a zone is stuck at its song's end: playing, on the last second,
/// still there after `STUCK_SECS`, with something to go on to, and not in
/// repeat-one.
fn stuck(state: &str, position: f64, length: Option<f64>, still_for: Duration, can_next: bool, loop_mode: &str) -> bool {
    let Some(length) = length.filter(|l| *l > 0.0) else {
        return false; // (a live stream has no end)
    };
    state == "playing"
        && can_next
        && loop_mode != "loop_one"
        && position > 0.0
        && length - position <= END_SLACK
        && still_for >= Duration::from_secs(STUCK_SECS)
}

/// Starts the watcher (from lib.rs setup).
pub fn schedule(app: &AppHandle) {
    if let Some(path) = crate::settings::path(app) {
        set_enabled(crate::settings::load(&path).skip_stuck);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut seen: HashMap<String, Seen> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if !ENABLED.load(Ordering::Relaxed) {
                seen.clear();
                continue;
            }
            let zones = app.state::<Zones>();
            let now = Instant::now();
            let list = zones.payload_now().zones;
            seen.retain(|id, _| list.iter().any(|z| &z.zone_id == id));
            for zone in &list {
                let (Some(song), Some(position)) = (song_of(zone), zone.seek_position) else {
                    seen.remove(&zone.zone_id);
                    continue;
                };
                let entry = seen.entry(zone.zone_id.clone()).or_insert_with(|| Seen {
                    song: song.clone(),
                    position,
                    since: now,
                    nudged: false,
                });
                // A new song, a move, or not playing: watch from here (a song
                // already nudged stays nudged)
                if entry.song != song || (entry.position - position).abs() >= 0.5 || zone.state != "playing" {
                    let nudged = entry.nudged && entry.song == song;
                    *entry = Seen { song, position, since: now, nudged };
                    continue;
                }
                let length = zone.now_playing.as_ref().and_then(|np| np.length);
                if entry.nudged || !stuck(zone.state, position, length, now - entry.since, zone.can_next, zone.loop_mode) {
                    continue;
                }
                entry.nudged = true;
                let name = zone.now_playing.as_ref().map(|np| np.title.clone()).unwrap_or_default();
                println!("[unstick] {} sat at the end of \"{name}\" for {STUCK_SECS} s: next", zone.name);
                match zones.transport() {
                    Ok(transport) => {
                        if let Err(e) = transport.control(&zone.zone_id, ControlAction::Next).await {
                            eprintln!("[unstick] couldn't send next: {e}");
                        }
                    }
                    Err(e) => eprintln!("[unstick] {e}"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(STUCK_SECS);
    const SHORT: Duration = Duration::from_secs(STUCK_SECS - 1);

    #[test]
    fn stuck_at_the_end() {
        assert!(stuck("playing", 244.0, Some(245.0), LONG, true, "disabled"));
        assert!(stuck("playing", 245.0, Some(245.0), LONG, true, "loop"));
    }

    #[test]
    fn not_stuck() {
        // not long enough yet, not at the end, paused
        assert!(!stuck("playing", 244.0, Some(245.0), SHORT, true, "disabled"));
        assert!(!stuck("playing", 200.0, Some(245.0), LONG, true, "disabled"));
        assert!(!stuck("paused", 244.0, Some(245.0), LONG, true, "disabled"));
        // nothing next, repeat one, no length (a stream)
        assert!(!stuck("playing", 244.0, Some(245.0), LONG, false, "disabled"));
        assert!(!stuck("playing", 244.0, Some(245.0), LONG, true, "loop_one"));
        assert!(!stuck("playing", 244.0, None, LONG, true, "disabled"));
    }
}
