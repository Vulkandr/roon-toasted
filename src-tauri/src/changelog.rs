// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Changelog window (page changelog.html): what's new in each version,
//! from src/changelog.json, which ships with the app (newest first; written
//! for each release from its notes).
//!
//! It opens by itself on the first start after an update: the version that
//! ran last is kept in the settings (`lastVersion`), and a different one
//! means the app was updated. A fresh install (no settings file yet) just
//! notes its version. Settings' Updates card has a Changelog button to open
//! it again any time.

use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::settings;

/// The Changelog window's label, as set in tauri.conf.json.
pub(crate) const CHANGELOG_WINDOW: &str = "changelog";

/// In setup: notes this version, and opens the Changelog a moment later if
/// the app was updated since it last ran.
pub fn at_start(app: &AppHandle) {
    let Some(path) = settings::path(app) else {
        return;
    };
    let version = app.package_info().version.to_string();
    // No settings file yet: a fresh install, nothing to announce
    let installed_before = path.exists();
    let mut saved = settings::load(&path);
    if saved.last_version.as_deref() == Some(version.as_str()) {
        return;
    }
    // (An install from before this existed has no last version: that's an
    // update too, as long as it has settings)
    saved.last_version = Some(version);
    if settings::save(&path, &saved).is_err() || !installed_before {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Let the app settle first (tray, Roon connection)
        tokio::time::sleep(Duration::from_secs(2)).await;
        open(&app);
    });
}

/// Shows the Changelog, centered on the main screen.
pub fn open(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(CHANGELOG_WINDOW) {
        if !window.is_visible().unwrap_or(false) {
            let _ = window.center();
        }
    }
    crate::show(app, CHANGELOG_WINDOW);
}
