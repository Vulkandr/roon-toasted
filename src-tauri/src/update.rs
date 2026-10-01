// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Updates: checks GitHub for a newer release and installs it on request.
//!
//! How it works: the release on GitHub carries a small `latest.json` (version,
//! notes, and the installer's URL plus its signature). Tauri's updater plugin
//! fetches it, compares versions, and on Install downloads the installer,
//! verifies the signature against the public key in tauri.conf.json, runs it
//! (per-user, passive: a progress bar, no questions) and relaunches the app.
//! Nothing downloads without the person pressing Install.
//!
//! Timing: the first check runs a minute after launch, then every 6 hours,
//! only while the "Check for updates" setting is on. Settings has the card
//! (status, notes, Install / Check now); the tray menu gets an "Update to x"
//! entry when one is waiting. Pages listen to `update-status`.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::settings;

/// Wait after launch before the first check (never slows the start).
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum UpdateState {
    /// Checks are switched off in Settings.
    Off,
    /// Nothing known yet (before the first check).
    Idle,
    Checking,
    UpToDate,
    Available { version: String, notes: Option<String>, date: Option<String> },
    Downloading { version: String, percent: Option<u8> },
    Installing { version: String },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    #[serde(flatten)]
    pub state: UpdateState,
    /// When the last check finished (seconds since the Unix epoch), if any.
    pub checked_at: Option<u64>,
}

#[derive(Default)]
pub struct Updates {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    state: Option<UpdateState>,
    checked_at: Option<u64>,
    /// The update found by the last check, kept for Install.
    pending: Option<Update>,
    busy: bool,
}

impl Updates {
    fn status(&self, app: &AppHandle) -> UpdateStatus {
        let inner = self.inner.lock().unwrap();
        let state = inner.state.clone().unwrap_or_else(|| {
            if settings::get_settings(app.clone()).check_for_updates {
                UpdateState::Idle
            } else {
                UpdateState::Off
            }
        });
        UpdateStatus {
            current_version: app.package_info().version.to_string(),
            state,
            checked_at: inner.checked_at,
        }
    }

    fn set(&self, app: &AppHandle, state: UpdateState) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.state.as_ref() == Some(&state) {
                return;
            }
            inner.state = Some(state);
        }
        let status = self.status(app);
        match &status.state {
            UpdateState::Available { version, .. } => println!("[update] {version} is available"),
            UpdateState::Failed { message } => println!("[update] {message}"),
            UpdateState::Downloading { .. } => {}
            other => println!("[update] {other:?}"),
        }
        let _ = app.emit("update-status", &status);
        crate::tray::sync_update(app, &status);
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Starts the periodic checks (in setup).
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_AFTER).await;
        loop {
            if settings::get_settings(app.clone()).check_for_updates {
                check(&app).await;
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// Called from settings.rs when "Check for updates" is toggled.
pub fn settings_changed(app: &AppHandle, before: &settings::AppSettings, after: &settings::AppSettings) {
    if before.check_for_updates == after.check_for_updates {
        return;
    }
    let updates = app.state::<Updates>();
    if after.check_for_updates {
        updates.set(app, UpdateState::Idle);
        let app = app.clone();
        tauri::async_runtime::spawn(async move { check(&app).await });
    } else {
        updates.inner.lock().unwrap().pending = None;
        updates.set(app, UpdateState::Off);
    }
}

/// One check against the release feed; records the outcome.
async fn check(app: &AppHandle) {
    let updates = app.state::<Updates>();
    {
        let mut inner = updates.inner.lock().unwrap();
        if inner.busy {
            return;
        }
        inner.busy = true;
    }
    updates.set(app, UpdateState::Checking);
    let result = async {
        let updater = app.updater().map_err(|e| e.to_string())?;
        updater.check().await.map_err(|e| e.to_string())
    }
    .await;
    let mut inner = updates.inner.lock().unwrap();
    inner.busy = false;
    inner.checked_at = Some(now_epoch());
    let state = match result {
        Ok(Some(update)) => {
            let state = UpdateState::Available {
                version: update.version.clone(),
                notes: update.body.clone().filter(|b| !b.trim().is_empty()),
                date: update.date.map(|d| d.to_string()),
            };
            inner.pending = Some(update);
            state
        }
        Ok(None) => {
            inner.pending = None;
            UpdateState::UpToDate
        }
        Err(message) => {
            inner.pending = None;
            UpdateState::Failed { message: friendly(&message) }
        }
    };
    drop(inner);
    updates.set(app, state);
}

/// Error text for the Settings card.
fn friendly(message: &str) -> String {
    let m = message.to_ascii_lowercase();
    if m.contains("dns") || m.contains("connect") || m.contains("timed out") || m.contains("network") {
        "Couldn't reach GitHub to check for updates.".into()
    } else if m.contains("release json") || m.contains("status code") {
        "Couldn't get the release list from GitHub.".into()
    } else if m.contains("signature") {
        "The update's signature didn't check out, so it wasn't installed.".into()
    } else {
        format!("Update check failed: {message}")
    }
}

/// The tray entry: installs the waiting update, or checks now and opens Settings.
pub fn from_tray(app: &AppHandle) {
    let waiting = matches!(app.state::<Updates>().status(app).state, UpdateState::Available { .. });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if waiting {
            if let Err(e) = update_install(app.clone()).await {
                println!("[update] {e}");
                crate::show(&app, crate::SETTINGS_WINDOW);
            }
        } else {
            crate::show(&app, crate::SETTINGS_WINDOW);
            check(&app).await;
        }
    });
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn update_status(app: AppHandle, updates: State<'_, Updates>) -> UpdateStatus {
    updates.status(&app)
}

/// Checks now (the button in Settings), whatever the setting says.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> UpdateStatus {
    check(&app).await;
    app.state::<Updates>().status(&app)
}

/// Downloads and installs the update the last check found. On success the
/// installer takes over and this process exits, so the command only returns
/// on failure (or when there is nothing to install).
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let updates = app.state::<Updates>();
    let update = {
        let mut inner = updates.inner.lock().unwrap();
        if inner.busy {
            return Err("An update is already in progress.".into());
        }
        let Some(update) = inner.pending.clone() else {
            return Err("No update to install. Check for updates first.".into());
        };
        inner.busy = true;
        update
    };
    let version = update.version.clone();
    updates.set(&app, UpdateState::Downloading { version: version.clone(), percent: None });

    let progress_app = app.clone();
    let progress_version = version.clone();
    let mut received: u64 = 0;
    let mut last_percent: Option<u8> = None;
    let finished_app = app.clone();
    let finished_version = version.clone();
    let result = update
        .download_and_install(
            move |chunk, total| {
                received += chunk as u64;
                let percent = total.map(|t| ((received.min(t) * 100) / t.max(1)) as u8);
                if percent != last_percent {
                    last_percent = percent;
                    progress_app.state::<Updates>().set(
                        &progress_app,
                        UpdateState::Downloading { version: progress_version.clone(), percent },
                    );
                }
            },
            move || {
                finished_app
                    .state::<Updates>()
                    .set(&finished_app, UpdateState::Installing { version: finished_version.clone() });
            },
        )
        .await;

    let mut inner = updates.inner.lock().unwrap();
    inner.busy = false;
    drop(inner);
    match result {
        Ok(()) => Ok(()), // not reached on Windows: the installer runs and the app exits
        Err(e) => {
            let message = friendly(&e.to_string());
            updates.set(&app, UpdateState::Failed { message: message.clone() });
            Err(message)
        }
    }
}
