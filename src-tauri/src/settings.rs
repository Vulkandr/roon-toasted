//! App preferences, saved as JSON in the app data folder
//! (%APPDATA%\com.vulkan.roon-toasted\settings.json on Windows).
//!
//! Every field has a default, so a missing or older file still loads, and
//! new fields can be added later without breaking existing installs.
//!
//! Pages read them with `get_settings` and change them with
//! `update_settings({ changes: { someSetting: value } })`, which saves and then
//! fires a `settings-changed` event with the full, updated settings.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// The zone the Toaster controls, when the user has picked one.
    /// Changed through `roon_select_zone`, not `update_settings`.
    pub selected_zone_id: Option<String>,
    /// How the Toaster window opens.
    pub toaster_window: ToasterWindow,
    /// The Toaster's size as the user sees it: 1.0 = 100% (the normal size,
    /// which is really `BASE_ZOOM`). Everything (text, spacing, art) scales
    /// together. Allowed range: `ZOOM_RANGE`.
    pub zoom: f64,
    /// Whether Ctrl + / Ctrl - / Ctrl + Enter / Ctrl + mouse wheel change the zoom.
    pub zoom_hotkeys: bool,
    /// Auto-Hide: whether the Toaster (and Settings) go back to the tray when
    /// another window is clicked.
    /// When off, it gets a taskbar button instead (see `apply_taskbar`).
    pub hide_on_blur: bool,
    /// The Toaster's colors: the default purple, or taken from the album art.
    pub toaster_colors: ToasterColors,
    /// Whether the global hotkeys are on at all (off: both released, e.g. for
    /// people who only open the Toaster from the Stream Deck).
    pub hotkeys_enabled: bool,
    /// Global hotkey that opens (or hides) the Toaster, e.g. "Ctrl+Alt+Z";
    /// None = off. Changed through `set_hotkey` (hotkeys.rs), not `update_settings`.
    pub hotkey_player: Option<String>,
    /// Global hotkey that opens the Toaster in search mode; None = off.
    pub hotkey_search: Option<String>,
    /// Whether a now-playing toast pops up when the track changes (see toast.rs).
    pub toasts_enabled: bool,
    /// Which corner of the screen toasts appear in.
    pub toast_position: ToastPosition,
    /// Which screen toasts appear on (an id from `list_monitors`, see
    /// monitors.rs); None = whichever is the main display.
    pub toast_monitor: Option<String>,
    /// How long a toast stays, in seconds (it waits while the mouse is on it).
    /// Allowed range: `TOAST_SECONDS`.
    pub toast_seconds: u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            selected_zone_id: None,
            toaster_window: ToasterWindow::default(),
            zoom: 1.0,
            zoom_hotkeys: true,
            hide_on_blur: true,
            toaster_colors: ToasterColors::default(),
            hotkeys_enabled: true,
            hotkey_player: Some("Ctrl+Alt+Z".into()),
            hotkey_search: Some("Ctrl+Alt+S".into()),
            toasts_enabled: true,
            toast_position: ToastPosition::default(),
            toast_monitor: None,
            toast_seconds: 8,
        }
    }
}

/// What the app calls 100%: the Toaster is designed at this real zoom, so the
/// user's 100% is 110% underneath (picked by Vulk as the ideal size).
pub const BASE_ZOOM: f64 = 1.1;

/// Smallest and largest zoom, as the user sees it.
pub const ZOOM_RANGE: std::ops::RangeInclusive<f64> = 0.5..=1.8;

/// Shortest and longest a toast can stay, in seconds.
pub const TOAST_SECONDS: std::ops::RangeInclusive<u32> = 2..=30;

/// Applies the saved zoom to the Toaster and the toast, so the toast's text
/// is the same size as the Toaster's (at page load and when it changes).
pub fn apply_zoom(app: &AppHandle) {
    let zoom = get_settings(app.clone()).zoom * BASE_ZOOM;
    for label in [crate::TOASTER_WINDOW, crate::toast::TOAST_WINDOW] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.set_zoom(zoom);
        }
    }
}

/// Gives the Toaster and Settings taskbar buttons (and places in Alt+Tab)
/// when "Auto-Hide" is off: they then stay open behind other windows,
/// and without a button there'd be no way back except the tray or a hotkey.
/// With the setting on they live in the tray only (applied at startup and
/// when it changes).
pub fn apply_taskbar(app: &AppHandle) {
    let in_tray_only = get_settings(app.clone()).hide_on_blur;
    for label in [crate::TOASTER_WINDOW, crate::SETTINGS_WINDOW] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.set_skip_taskbar(in_tray_only);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToasterWindow {
    /// Always open at the default size, centered.
    #[default]
    Default,
    /// Reopen at the size, position and maximized state it was last left in.
    Remember,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToasterColors {
    /// The purple theme.
    #[default]
    Default,
    /// Accent and background tint from the album that's playing.
    Album,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToastPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    /// Where Windows shows its own notifications.
    #[default]
    BottomRight,
}

pub fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("settings.json"))
}

pub fn load(path: &Path) -> AppSettings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, settings: &AppSettings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// Current settings (defaults for anything never changed).
#[tauri::command]
pub fn get_settings(app: AppHandle) -> AppSettings {
    path(&app).map(|p| load(&p)).unwrap_or_default()
}

/// Changes one or more settings, e.g. `{ toasterWindow: "remember" }`.
/// Unknown names or wrong value types are refused, so a typo can't silently
/// do nothing. Returns the full updated settings.
#[tauri::command]
pub fn update_settings(app: AppHandle, changes: serde_json::Value) -> Result<AppSettings, String> {
    let path = path(&app).ok_or("Couldn't find the app data folder.")?;
    let Some(changes) = changes.as_object() else {
        return Err("Settings changes must be an object.".into());
    };

    let before = load(&path);
    let mut current = serde_json::to_value(&before).map_err(|e| e.to_string())?;
    let fields = current.as_object_mut().expect("settings serialize to an object");
    for (name, value) in changes {
        if name == "selectedZoneId" {
            return Err("Use roon_select_zone to change the zone.".into());
        }
        if name == "hotkeyPlayer" || name == "hotkeySearch" {
            return Err("Use set_hotkey to change a hotkey.".into());
        }
        if !fields.contains_key(name) {
            return Err(format!("Unknown setting: {name}"));
        }
        fields.insert(name.clone(), value.clone());
    }

    let updated: AppSettings =
        serde_json::from_value(current).map_err(|e| format!("Invalid setting value: {e}"))?;
    if !ZOOM_RANGE.contains(&updated.zoom) {
        return Err(format!(
            "Zoom must be between {} and {}.",
            ZOOM_RANGE.start(),
            ZOOM_RANGE.end()
        ));
    }
    if !TOAST_SECONDS.contains(&updated.toast_seconds) {
        return Err(format!(
            "Toasts must stay between {} and {} seconds.",
            TOAST_SECONDS.start(),
            TOAST_SECONDS.end()
        ));
    }
    save(&path, &updated)?;
    if updated.zoom != before.zoom {
        apply_zoom(&app);
    }
    if updated.hide_on_blur != before.hide_on_blur {
        apply_taskbar(&app);
    }
    if updated.hotkeys_enabled != before.hotkeys_enabled {
        crate::hotkeys::register_saved(&app);
    }
    crate::tray::sync(&app, &updated);
    let _ = app.emit("settings-changed", &updated);
    Ok(updated)
}
