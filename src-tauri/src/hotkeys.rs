// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Global hotkeys: key combinations that work anywhere in Windows, even
//! while another app (or a game) has focus.
//!
//! - Player (default Ctrl+Alt+Z): opens the Toaster, or hides it when it's
//!   already open and in front.
//! - Search (default Ctrl+Alt+S): opens the Toaster on the Search tab with the
//!   search bar ready to type.
//!   With the Wrapper on, Search can open Roon's own search instead (always,
//!   while the Toaster is off; `settings::search_in_roon`).
//!   (Player only while the Toaster is on, Search while either is.)
//! - Wrapper (default Ctrl+Alt+W): opens the Wrapper, or hides it when it's
//!   open and in front. Only registered while the Wrapper is switched on.
//!
//! The media keys (Volume Up and so on, see mediakeys.rs) are registered
//! here too, with their own switch in Settings; the Hotkeys switch doesn't
//! touch them.
//!
//! Registering goes through `apply`, which compares what should be
//! registered with what is and changes only the difference. It asks the
//! main thread to do each change and waits for it, so it must never run on
//! the main thread itself: `register_saved` (callable from anywhere) runs
//! it on a thread of its own and says `hotkeys-changed` when it's done.
//!
//! They're set in Settings and saved in settings.json (`hotkeyPlayer`,
//! `hotkeySearch`, `hotkeyWrapper`; null = off). The Hotkeys switch
//! (`hotkeysEnabled`) turns them all off at once, releasing the combinations for other apps. Settings changes them with `set_hotkey`, which
//! checks the combination, registers it with Windows and only saves it if that
//! worked (another app may already own it). While Settings is recording a new
//! combination, `pause_hotkeys` switches them all off, so pressing the current
//! combination doesn't open or hide the Toaster.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::settings;

/// What's registered with Windows right now. Only `apply` touches it, and
/// holds it for its whole run, so two runs never overlap.
static REGISTERED: Mutex<Vec<Shortcut>> = Mutex::new(Vec::new());
/// Settings is recording a new combination: nothing is registered.
static PAUSED: AtomicBool = AtomicBool::new(false);

/// The last registration problem for each hotkey, for Settings to show
/// (e.g. another app already owns the combination).
#[derive(Default)]
pub struct HotkeyState {
    errors: Mutex<Errors>,
}

#[derive(Default, Clone)]
struct Errors {
    player: Option<String>,
    search: Option<String>,
    wrapper: Option<String>,
    /// The media keys another app holds on to.
    media: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    /// The Hotkeys switch in Settings.
    pub enabled: bool,
    /// e.g. "Ctrl+Alt+Z", or null when off.
    pub player: Option<String>,
    pub search: Option<String>,
    pub wrapper: Option<String>,
    /// Why a hotkey isn't working, if it isn't.
    pub player_error: Option<String>,
    pub search_error: Option<String>,
    pub wrapper_error: Option<String>,
    /// Media keys that couldn't be taken (another app has them).
    pub media_error: Option<String>,
    /// Why the media keys are with Windows although their switch is on
    /// (no zone, or a fixed-volume one).
    pub media_note: Option<String>,
}

/// The plugin, with one handler for every hotkey.
pub fn plugin() -> tauri::plugin::TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(on_hotkey)
        .build()
}

fn parse(text: Option<&str>) -> Option<Shortcut> {
    text.and_then(|t| t.parse::<Shortcut>().ok())
}

/// "Ctrl+Alt+R" -> "Ctrl + Alt + R", for messages.
fn pretty(text: &str) -> String {
    text.split('+')
        .map(|part| match part {
            "Super" => "Win",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn on_hotkey(app: &AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
    if crate::mediakeys::on_key(app, shortcut, event.state()) {
        return;
    }
    if event.state() != ShortcutState::Pressed {
        return;
    }
    let saved = settings::get_settings(app.clone());
    if parse(saved.hotkey_player.as_deref()).as_ref() == Some(shortcut) {
        crate::toggle_toaster(app);
    } else if parse(saved.hotkey_search.as_deref()).as_ref() == Some(shortcut) {
        crate::open_search(app);
    } else if parse(saved.hotkey_wrapper.as_deref()).as_ref() == Some(shortcut) {
        crate::wrapper::toggle(app);
    }
}

/// "A", "A and B", "A, B and C".
fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [most @ .., last] => format!("{} and {last}", most.join(", ")),
    }
}

/// Registers the saved hotkeys and the media keys, on a thread of its own
/// (see the top of this file), and tells the pages when it's done.
/// Callable from anywhere, the main thread included.
pub fn register_saved(app: &AppHandle) {
    let app = app.clone();
    let _ = std::thread::Builder::new().name("hotkeys".into()).spawn(move || {
        apply(&app);
        let _ = app.emit("hotkeys-changed", ());
    });
}

/// Makes what's registered with Windows match the settings: the saved
/// hotkeys (none when the Hotkeys switch is off; Open Toaster only while
/// the Toaster is on, Search while the Toaster or the Wrapper is, Open
/// Wrapper only while the Wrapper is) and the media keys (mediakeys.rs),
/// or nothing at all while Settings records a combination. Remembers any
/// problems for Settings. Never on the main thread: it waits for it.
fn apply(app: &AppHandle) {
    let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    let saved = settings::get_settings(app.clone());
    let paused = PAUSED.load(Ordering::Relaxed);
    let mut errors = Errors::default();

    // What should be registered: each with where its error goes (0 to 2,
    // the hotkeys; 3, a media key) and its name
    let mut wanted: Vec<(Shortcut, usize, String)> = Vec::new();
    if saved.hotkeys_enabled && !paused {
        let combos = [
            (saved.toaster_enabled, saved.hotkey_player.as_deref()),
            (saved.toaster_enabled || saved.wrapper_enabled, saved.hotkey_search.as_deref()),
            (saved.wrapper_enabled, saved.hotkey_wrapper.as_deref()),
        ];
        for (slot, (on, text)) in combos.into_iter().enumerate() {
            let (true, Some(text)) = (on, text) else {
                continue; // off: nothing to register, nothing wrong
            };
            match parse(Some(text)) {
                Some(shortcut) => wanted.push((shortcut, slot, pretty(text))),
                None => {
                    let problem = Some(format!("{} isn't a key combination the app can use.", pretty(text)));
                    match slot {
                        0 => errors.player = problem,
                        1 => errors.search = problem,
                        _ => errors.wrapper = problem,
                    }
                }
            }
        }
    }
    if !paused {
        for (shortcut, name) in crate::mediakeys::wanted(&saved) {
            wanted.push((shortcut, 3, name.to_string()));
        }
    }

    // Let go of the ones no longer wanted
    let gone: Vec<Shortcut> = registered.iter().filter(|r| !wanted.iter().any(|(w, _, _)| w == *r)).copied().collect();
    if !gone.is_empty() {
        let _ = app.global_shortcut().unregister_multiple(gone.clone());
        registered.retain(|r| !gone.contains(r));
    }

    // Take the new ones
    let mut media_refused: Vec<&str> = Vec::new();
    for (shortcut, slot, name) in &wanted {
        if registered.contains(shortcut) {
            continue;
        }
        match app.global_shortcut().register(*shortcut) {
            Ok(()) => registered.push(*shortcut),
            Err(e) => {
                println!("[hotkeys] {name} wasn't registered: {e}");
                let problem = Some(format!("{name} is already used by another app."));
                match slot {
                    0 => errors.player = problem,
                    1 => errors.search = problem,
                    2 => errors.wrapper = problem,
                    _ => media_refused.push(name),
                }
            }
        }
    }
    // Media keys another app registered (Roon does) are taken with the
    // keyboard hook instead; only if that fails do they stay refused
    if !media_refused.is_empty() {
        println!("[hotkeys] taking {} with the keyboard hook instead", listed(&media_refused));
    }
    if !crate::mediakeys::take(app, &media_refused) && !media_refused.is_empty() {
        let (verb, they) = if media_refused.len() == 1 { ("is", "it stays") } else { ("are", "they stay") };
        errors.media = Some(format!(
            "{} {verb} already used by another app, so {they} with Windows.",
            listed(&media_refused)
        ));
    }
    *app.state::<HotkeyState>().errors.lock().unwrap() = errors;
}

fn status(app: &AppHandle) -> HotkeyStatus {
    let saved = settings::get_settings(app.clone());
    let errors = app.state::<HotkeyState>().errors.lock().unwrap().clone();
    let media_note = crate::mediakeys::note(&saved);
    HotkeyStatus {
        enabled: saved.hotkeys_enabled,
        player: saved.hotkey_player,
        search: saved.hotkey_search,
        wrapper: saved.hotkey_wrapper,
        player_error: errors.player,
        search_error: errors.search,
        wrapper_error: errors.wrapper,
        media_error: errors.media,
        media_note,
    }
}

/// The hotkeys and whether they're working.
#[tauri::command]
pub fn hotkey_status(app: AppHandle) -> HotkeyStatus {
    status(&app)
}

/// Sets a hotkey (`which`: "player", "search" or "wrapper"; `shortcut`: e.g. "Ctrl+Alt+Z",
/// or null to turn it off). Only saved if Windows accepts it; otherwise the
/// old one stays and the reason comes back as the error.
#[tauri::command]
pub async fn set_hotkey(
    app: AppHandle,
    which: String,
    shortcut: Option<String>,
) -> Result<HotkeyStatus, String> {
    let shortcut = shortcut.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let path = settings::path(&app).ok_or("Couldn't find the app data folder.")?;
    let before = settings::load(&path);

    let mut updated = before.clone();
    // The other two, which a new combination mustn't repeat
    let others = match which.as_str() {
        "player" => {
            updated.hotkey_player = shortcut.clone();
            [before.hotkey_search.clone(), before.hotkey_wrapper.clone()]
        }
        "search" => {
            updated.hotkey_search = shortcut.clone();
            [before.hotkey_player.clone(), before.hotkey_wrapper.clone()]
        }
        "wrapper" => {
            updated.hotkey_wrapper = shortcut.clone();
            [before.hotkey_player.clone(), before.hotkey_search.clone()]
        }
        _ => return Err(format!("Unknown hotkey: {which}")),
    };

    if let Some(text) = &shortcut {
        let Some(parsed) = parse(Some(text)) else {
            apply(&app);
            return Err(format!("{} isn't a key combination the app can use.", pretty(text)));
        };
        if others.iter().any(|other| parse(other.as_deref()).as_ref() == Some(&parsed)) {
            apply(&app);
            return Err(format!("{} is already another hotkey.", pretty(text)));
        }
    }

    settings::save(&path, &updated)?;
    apply(&app);

    // Windows refused it (another app owns it): put the old one back.
    let refused = {
        let state = app.state::<HotkeyState>();
        let errors = state.errors.lock().unwrap();
        match which.as_str() {
            "player" => errors.player.clone(),
            "search" => errors.search.clone(),
            _ => errors.wrapper.clone(),
        }
    };
    if let (Some(reason), Some(_)) = (refused, &shortcut) {
        settings::save(&path, &before)?;
        apply(&app);
        return Err(reason);
    }

    let _ = app.emit("settings-changed", &updated);
    Ok(status(&app))
}

/// Switches all hotkeys off while Settings records a new combination
/// (`paused: true`), and back on afterwards.
#[tauri::command]
pub async fn pause_hotkeys(app: AppHandle, paused: bool) {
    PAUSED.store(paused, Ordering::Relaxed);
    apply(&app);
}
