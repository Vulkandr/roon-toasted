// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Global hotkeys: two key combinations that work anywhere in Windows, even
//! while another app (or a game) has focus.
//!
//! - Player (default Ctrl+Alt+Z): opens the Toaster, or hides it when it's
//!   already open and in front.
//! - Search (default Ctrl+Alt+S): opens the Toaster on the Search tab with the
//!   search bar ready to type.
//!
//! Both are set in Settings and saved in settings.json (`hotkeyPlayer`,
//! `hotkeySearch`; null = off). The Hotkeys switch (`hotkeysEnabled`) turns
//! both off at once, releasing the combinations for other apps. Settings changes them with `set_hotkey`, which
//! checks the combination, registers it with Windows and only saves it if that
//! worked (another app may already own it). While Settings is recording a new
//! combination, `pause_hotkeys` switches them all off, so pressing the current
//! combination doesn't open or hide the Toaster.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::settings;

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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    /// The Hotkeys switch in Settings.
    pub enabled: bool,
    /// e.g. "Ctrl+Alt+Z", or null when off.
    pub player: Option<String>,
    pub search: Option<String>,
    /// Why a hotkey isn't working, if it isn't.
    pub player_error: Option<String>,
    pub search_error: Option<String>,
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
    if event.state() != ShortcutState::Pressed {
        return;
    }
    let saved = settings::get_settings(app.clone());
    if parse(saved.hotkey_player.as_deref()).as_ref() == Some(shortcut) {
        crate::toggle_toaster(app);
    } else if parse(saved.hotkey_search.as_deref()).as_ref() == Some(shortcut) {
        crate::open_toaster(app, crate::ToasterMode::Search);
    }
}

/// Registers one saved hotkey; returns why it didn't work, if it didn't.
fn register(app: &AppHandle, text: Option<&str>) -> Option<String> {
    let text = text?; // off: nothing to register, nothing wrong
    let Some(shortcut) = parse(Some(text)) else {
        return Some(format!("{} isn't a key combination the app can use.", pretty(text)));
    };
    match app.global_shortcut().register(shortcut) {
        Ok(()) => None,
        Err(_) => Some(format!("{} is already used by another app.", pretty(text))),
    }
}

/// Registers the saved hotkeys (none when the Hotkeys switch is off),
/// replacing any registered before, and remembers any problems for Settings.
pub fn register_saved(app: &AppHandle) {
    let _ = app.global_shortcut().unregister_all();
    let saved = settings::get_settings(app.clone());
    let errors = if saved.hotkeys_enabled {
        Errors {
            player: register(app, saved.hotkey_player.as_deref()),
            search: register(app, saved.hotkey_search.as_deref()),
        }
    } else {
        Errors::default()
    };
    *app.state::<HotkeyState>().errors.lock().unwrap() = errors;
}

fn status(app: &AppHandle) -> HotkeyStatus {
    let saved = settings::get_settings(app.clone());
    let errors = app.state::<HotkeyState>().errors.lock().unwrap().clone();
    HotkeyStatus {
        enabled: saved.hotkeys_enabled,
        player: saved.hotkey_player,
        search: saved.hotkey_search,
        player_error: errors.player,
        search_error: errors.search,
    }
}

/// The hotkeys and whether they're working.
#[tauri::command]
pub fn hotkey_status(app: AppHandle) -> HotkeyStatus {
    status(&app)
}

/// Sets a hotkey (`which`: "player" or "search"; `shortcut`: e.g. "Ctrl+Alt+Z",
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
    let other = match which.as_str() {
        "player" => {
            updated.hotkey_player = shortcut.clone();
            before.hotkey_search.clone()
        }
        "search" => {
            updated.hotkey_search = shortcut.clone();
            before.hotkey_player.clone()
        }
        _ => return Err(format!("Unknown hotkey: {which}")),
    };

    if let Some(text) = &shortcut {
        let Some(parsed) = parse(Some(text)) else {
            register_saved(&app);
            return Err(format!("{} isn't a key combination the app can use.", pretty(text)));
        };
        if parse(other.as_deref()) == Some(parsed) {
            register_saved(&app);
            return Err(format!("{} is already the other hotkey.", pretty(text)));
        }
    }

    settings::save(&path, &updated)?;
    register_saved(&app);

    // Windows refused it (another app owns it): put the old one back.
    let refused = {
        let state = app.state::<HotkeyState>();
        let errors = state.errors.lock().unwrap();
        match which.as_str() {
            "player" => errors.player.clone(),
            _ => errors.search.clone(),
        }
    };
    if let (Some(reason), Some(_)) = (refused, &shortcut) {
        settings::save(&path, &before)?;
        register_saved(&app);
        return Err(reason);
    }

    let _ = app.emit("settings-changed", &updated);
    Ok(status(&app))
}

/// Switches all hotkeys off while Settings records a new combination
/// (`paused: true`), and back on afterwards.
#[tauri::command]
pub async fn pause_hotkeys(app: AppHandle, paused: bool) {
    if paused {
        let _ = app.global_shortcut().unregister_all();
    } else {
        register_saved(&app);
    }
}
