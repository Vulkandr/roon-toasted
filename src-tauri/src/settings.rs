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
    /// Whether Ctrl + / Ctrl - / Ctrl 0 / Ctrl + mouse wheel change the zoom.
    pub zoom_hotkeys: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            selected_zone_id: None,
            toaster_window: ToasterWindow::default(),
            zoom: 1.0,
            zoom_hotkeys: true,
        }
    }
}

/// What the app calls 100%: the Toaster is designed at this real zoom, so the
/// user's 100% is 110% underneath (picked by Vulk as the ideal size).
pub const BASE_ZOOM: f64 = 1.1;

/// Smallest and largest zoom, as the user sees it.
pub const ZOOM_RANGE: std::ops::RangeInclusive<f64> = 0.5..=1.8;

/// Applies the saved zoom to the Toaster (at page load and when it changes).
pub fn apply_zoom(app: &AppHandle) {
    let zoom = get_settings(app.clone()).zoom * BASE_ZOOM;
    if let Some(toaster) = app.get_webview_window(crate::TOASTER_WINDOW) {
        let _ = toaster.set_zoom(zoom);
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
    save(&path, &updated)?;
    if updated.zoom != before.zoom {
        apply_zoom(&app);
    }
    let _ = app.emit("settings-changed", &updated);
    Ok(updated)
}
