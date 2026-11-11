// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Media keys: the keyboard's Play/Pause, Stop, Next and Previous keys,
//! and its Volume Up, Volume Down and Mute keys, control the selected Roon
//! zone instead of Windows and other apps.
//!
//! Settings > General > Media Keys has a switch for each set, independent
//! of each other: Playback (`mediaKeysPlayback`, on by default: a music
//! player is what people expect those keys to control) and Volume
//! (`mediaKeysVolume`, off by default: taking the volume keys away from
//! Windows is the bigger step; Vulk 2026-11-10).
//!
//! The keys are global hotkeys like the others (hotkeys.rs registers them;
//! a registered key never reaches Windows, so the Windows volume stays
//! put). They're only taken while there's something for them to do:
//! - the volume keys while the selected zone has a volume Roon can change
//!   (a fixed-volume zone leaves them with Windows);
//! - the playback keys while there's a zone at all (no Roon, no capture).
//!
//! A key another app has registered for itself is refused to us as a
//! hotkey (Roon does that with the playback keys, and then plays and
//! pauses whichever zone is picked in Roon, not the one picked here):
//! those are taken with a keyboard hook instead (keyhook.rs, `take`).
//!
//! `zones_updated` notices when that changes and has hotkeys.rs register
//! or release them. Holding a volume key keeps stepping (Windows' own key
//! repeat is switched off for hotkeys, so the repeat is done here).
//!
//! The steps keep their own time: each one moves a volume this file keeps
//! count of (`TARGETS`) and sends it to Roon as an exact value, without
//! waiting for the answer to the one before. Roon can take longer than a
//! step to answer, and waiting made a held key crawl; exact values also
//! mean nothing is left queued up to play out after the key is let go.
//! The pages show that volume right away too (`shown_volume`, used by
//! zones.rs): Roon reports a volume that's on the move only every few
//! steps, which made the number jump by 4 to 6 at a time.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use roon_api::{ControlAction, MuteAction, VolumeMode};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, Shortcut, ShortcutState};

use crate::settings::AppSettings;
use crate::zones::{ZoneView, Zones, ZonesPayload};

#[derive(Clone, Copy, PartialEq)]
enum Key {
    VolumeUp,
    VolumeDown,
    Mute,
    PlayPause,
    Stop,
    Next,
    Previous,
}

/// Each key, the key code the hotkeys know it by, its name in messages,
/// and Windows' own code for it (VK_VOLUME_UP and so on, for the hook).
const KEYS: [(Key, Code, &str, u32); 7] = [
    (Key::VolumeUp, Code::AudioVolumeUp, "Volume Up", 0xAF),
    (Key::VolumeDown, Code::AudioVolumeDown, "Volume Down", 0xAE),
    (Key::Mute, Code::AudioVolumeMute, "Mute", 0xAD),
    (Key::PlayPause, Code::MediaPlayPause, "Play/Pause", 0xB3),
    (Key::Stop, Code::MediaStop, "Stop", 0xB2),
    (Key::Next, Code::MediaTrackNext, "Next", 0xB0),
    (Key::Previous, Code::MediaTrackPrevious, "Previous", 0xB1),
];

impl Key {
    fn is_volume(self) -> bool {
        matches!(self, Key::VolumeUp | Key::VolumeDown | Key::Mute)
    }
}

/// What the selected zone offers: `HAS_ZONE` when there is one,
/// `HAS_VOLUME` when Roon can change its volume.
static ZONE: AtomicU8 = AtomicU8::new(0);
const HAS_ZONE: u8 = 1;
const HAS_VOLUME: u8 = 2;

/// Counts volume key presses and releases: a held key keeps stepping only
/// while this still has the number its press gave it.
static HOLD: AtomicU64 = AtomicU64::new(0);

/// Before a held volume key starts repeating, and between its steps.
const REPEAT_AFTER: Duration = Duration::from_millis(400);
const REPEAT_EVERY: Duration = Duration::from_millis(100);
/// A held key gives up after this long (a release that never arrived).
const HOLD_LIMIT: Duration = Duration::from_secs(20);

/// Where the keys last sent each output's volume, and when: the next step
/// goes on from there (Roon's own report of the volume lags behind quick
/// presses), unless it's older than `TARGET_KEPT` (the volume may have
/// been changed somewhere else since).
static TARGETS: Mutex<Option<HashMap<String, (f64, Instant)>>> = Mutex::new(None);
const TARGET_KEPT: Duration = Duration::from_secs(2);

fn selected(payload: &ZonesPayload) -> Option<&ZoneView> {
    let id = payload.selected_zone_id.as_ref()?;
    payload.zones.iter().find(|z| &z.zone_id == id)
}

/// The zones changed (zones.rs): if the selected zone gained or lost its
/// volume control, or came or went, the keys are registered again to match.
pub fn zones_updated(app: &AppHandle, payload: &ZonesPayload) {
    let now = match selected(payload) {
        Some(zone) if zone.outputs.iter().any(|o| o.volume.is_some()) => HAS_ZONE | HAS_VOLUME,
        Some(_) => HAS_ZONE,
        None => 0,
    };
    if ZONE.swap(now, Ordering::Relaxed) != now {
        crate::hotkeys::register_saved(app);
    }
}

/// The keys to take from Windows right now, with their names.
pub fn wanted(saved: &AppSettings) -> Vec<(Shortcut, &'static str)> {
    let zone = ZONE.load(Ordering::Relaxed);
    KEYS.iter()
        .filter(|(key, _, _, _)| {
            if key.is_volume() {
                saved.media_keys_volume && zone & HAS_VOLUME != 0
            } else {
                saved.media_keys_playback && zone & HAS_ZONE != 0
            }
        })
        .map(|(_, code, name, _)| (Shortcut::new(None, *code), *name))
        .collect()
}

/// Takes the media keys Windows refused as hotkeys (by name; another app
/// has them) with the keyboard hook instead, and lets go of any taken that
/// way before that aren't in the list. False if the hook couldn't be set.
pub fn take(app: &AppHandle, refused: &[&str]) -> bool {
    let codes: Vec<u32> = KEYS.iter().filter(|(_, _, name, _)| refused.contains(name)).map(|k| k.3).collect();
    crate::keyhook::set(app, &codes)
}

/// A key the hook took was pressed or released.
pub fn on_hooked(app: &AppHandle, vk: u32, pressed: bool) {
    if let Some((key, _, _, _)) = KEYS.iter().find(|k| k.3 == vk) {
        handle(app, *key, pressed);
    }
}

/// The volume the keys last sent an output to, while that's fresh: what
/// the pages show for it (zones.rs), ahead of Roon's own report.
pub fn shown_volume(output_id: &str) -> Option<f64> {
    let targets = TARGETS.lock().unwrap_or_else(|e| e.into_inner());
    match targets.as_ref()?.get(output_id) {
        Some((value, at)) if at.elapsed() < TARGET_KEPT => Some(*value),
        _ => None,
    }
}

/// Why keys aren't taken although their switch is on, for Settings.
pub fn note(saved: &AppSettings) -> Option<String> {
    let zone = ZONE.load(Ordering::Relaxed);
    if zone & HAS_ZONE == 0 {
        (saved.media_keys_volume || saved.media_keys_playback)
            .then(|| "No Roon zone to control right now, so the keys work as usual.".into())
    } else if !saved.media_keys_volume {
        None
    } else if zone & HAS_VOLUME == 0 {
        Some("The selected zone has a fixed volume, so the volume keys stay with Windows.".into())
    } else {
        None
    }
}

/// A hotkey was pressed or released: true if it was a media key (handled
/// here), false for any other hotkey.
pub fn on_key(app: &AppHandle, shortcut: &Shortcut, state: ShortcutState) -> bool {
    let Some((key, _, _, _)) = KEYS.iter().find(|(_, code, _, _)| *shortcut == Shortcut::new(None, *code)) else {
        return false;
    };
    handle(app, *key, state == ShortcutState::Pressed);
    true
}

/// A media key went down or came up, as a hotkey or through the hook.
/// (Called on the main thread or the hook's: it only starts tasks.)
fn handle(app: &AppHandle, key: Key, pressed: bool) {
    match key {
        Key::VolumeUp | Key::VolumeDown => {
            // (a press or a release of either ends a hold before it)
            let mine = HOLD.fetch_add(1, Ordering::Relaxed) + 1;
            if pressed {
                let app = app.clone();
                let up = key == Key::VolumeUp;
                tauri::async_runtime::spawn(hold_volume(app, up, mine));
            }
        }
        Key::Mute if pressed => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { toggle_mute(&app).await });
        }
        Key::PlayPause | Key::Stop | Key::Next | Key::Previous if pressed => {
            let action = match key {
                Key::PlayPause => ControlAction::PlayPause,
                Key::Stop => ControlAction::Stop,
                Key::Next => ControlAction::Next,
                _ => ControlAction::Previous,
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move { control(&app, action).await });
        }
        _ => {}
    }
}

/// One step right away, then, while the key stays down, more of them, on
/// the clock (a slow answer from Roon doesn't hold the next step up).
async fn hold_volume(app: AppHandle, up: bool, mine: u64) {
    let started = Instant::now();
    let mut wait = REPEAT_AFTER;
    loop {
        let step_started = Instant::now();
        let Some(sending) = step_volume(&app, up) else {
            break;
        };
        // The answer is waited for only until the next step is due
        let mut sending = std::pin::pin!(sending);
        let answered = tokio::time::timeout(wait, &mut sending).await.is_ok();
        tokio::time::sleep(wait.saturating_sub(step_started.elapsed())).await;
        wait = REPEAT_EVERY;
        if HOLD.load(Ordering::Relaxed) != mine || started.elapsed() > HOLD_LIMIT {
            // (the last one is seen through, so the volume ends where the
            // key was let go)
            if !answered {
                sending.await;
            }
            break;
        }
    }
    // Once the volume shown is Roon's own again, show it (it may have
    // settled somewhere else, at a volume limit, say)
    tokio::time::sleep(TARGET_KEPT + Duration::from_millis(100)).await;
    if HOLD.load(Ordering::Relaxed) == mine + 1 || HOLD.load(Ordering::Relaxed) == mine {
        app.state::<Zones>().refresh();
    }
}

/// The selected zone as it is now.
fn zone_now(app: &AppHandle) -> Option<ZoneView> {
    let payload = app.state::<Zones>().payload_now();
    selected(&payload).cloned()
}

/// One volume step, up or down, on every output of the selected zone that
/// has a volume (a grouped zone moves together): works out each output's
/// new volume and returns the sending of it, or None with nothing to move.
fn step_volume(app: &AppHandle, up: bool) -> Option<impl std::future::Future<Output = ()>> {
    let zone = zone_now(app)?;
    let transport = app.state::<Zones>().transport().ok()?;
    let mut moves: Vec<(String, String, f64)> = Vec::new();
    {
        let mut targets = TARGETS.lock().unwrap_or_else(|e| e.into_inner());
        let targets = targets.get_or_insert_with(HashMap::new);
        for output in &zone.outputs {
            let Some(volume) = &output.volume else {
                continue;
            };
            let from = match targets.get(&output.output_id) {
                Some((value, at)) if at.elapsed() < TARGET_KEPT => *value,
                _ => volume.value,
            };
            let step = if volume.step > 0.0 { volume.step } else { 1.0 };
            let to = (from + if up { step } else { -step }).clamp(volume.min, volume.max);
            targets.insert(output.output_id.clone(), (to, Instant::now()));
            if to != from {
                moves.push((output.output_id.clone(), output.name.clone(), to));
            }
        }
    }
    if moves.is_empty() {
        return None; // already at the top or the bottom
    }
    // The pages show the new volume now (see `shown_volume`)
    app.state::<Zones>().refresh();
    Some(async move {
        for (output_id, name, to) in moves {
            if let Err(e) = transport.change_volume(&output_id, VolumeMode::Absolute, to).await {
                println!("[media keys] volume on {name}: {e}");
            }
        }
    })
}

/// Mutes the selected zone's outputs, or unmutes them if they all are.
async fn toggle_mute(app: &AppHandle) {
    let Some(zone) = zone_now(app) else {
        return;
    };
    let Ok(transport) = app.state::<Zones>().transport() else {
        return;
    };
    let all_muted = zone.outputs.iter().filter_map(|o| o.volume.as_ref()).all(|v| v.is_muted);
    let action = if all_muted { MuteAction::Unmute } else { MuteAction::Mute };
    for output in zone.outputs.iter().filter(|o| o.volume.is_some()) {
        if let Err(e) = transport.mute(&output.output_id, action).await {
            println!("[media keys] mute on {}: {e}", output.name);
        }
    }
}

/// Play/Pause, Stop, Next or Previous on the selected zone.
async fn control(app: &AppHandle, action: ControlAction) {
    let Some(zone) = zone_now(app) else {
        return;
    };
    let Ok(transport) = app.state::<Zones>().transport() else {
        return;
    };
    match transport.control(&zone.zone_id, action).await {
        Ok(()) => println!("[media keys] {action:?} sent to {}", zone.name),
        Err(e) => println!("[media keys] {action:?} on {}: {e}", zone.name),
    }
}
