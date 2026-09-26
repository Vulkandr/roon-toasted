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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// The zone the Toaster controls, when the user has picked one.
    /// Changed through `roon_select_zone`, not `update_settings`.
    pub selected_zone_id: Option<String>,
    /// How the Toaster window opens.
    pub toaster_window: ToasterWindow,
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

    let mut current = serde_json::to_value(load(&path)).map_err(|e| e.to_string())?;
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
    save(&path, &updated)?;
    let _ = app.emit("settings-changed", &updated);
    Ok(updated)
}
