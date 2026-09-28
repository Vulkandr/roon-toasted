// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Now-playing toasts: a small card in a corner of the screen that shows the
//! new track when it changes (whether or not the Toaster is open). Clicking
//! it opens the Toaster on the Playing tab; it goes away on its own after a
//! few seconds (waiting while the mouse is on it). Corner, screen and time are
//! set in Settings.
//!
//! The toast is its own window ("toast" in tauri.conf.json) that never takes
//! focus, so a toast never interrupts typing or a game. How one appears:
//! 1. zones.rs reports every zone update here (`zones_updated`).
//! 2. When the selected zone's track changed, the `toast-show` event sends
//!    the track to the toast page (toast.js).
//! 3. The page fills itself in, waits a moment for the album art, then calls
//!    `toast_present`, which puts the window in its corner and shows it.
//! 4. When its time is up (or it's closed) the page fades it out and calls
//!    `toast_hide`.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::settings::{self, ToastPosition, ToasterColors};
use crate::zones::{ZonesPayload, Zones};

/// The toast window's label, as set in tauri.conf.json.
pub(crate) const TOAST_WINDOW: &str = "toast";

/// The toast's size at 100% zoom, in CSS pixels (toast.css is laid out for
/// this). It grows and shrinks with the Toaster's zoom setting.
const TOAST_WIDTH: f64 = 340.0;
const TOAST_HEIGHT: f64 = 92.0;

/// Gap between the toast and the edges of the screen (or the taskbar).
const MARGIN: f64 = 16.0;

/// The last track seen, to tell a new track from any other zone update.
#[derive(Default)]
pub struct ToastState {
    seen: Mutex<Seen>,
}

#[derive(Default)]
struct Seen {
    /// False until the first update, so starting the app doesn't toast.
    started: bool,
    zone_id: Option<String>,
    /// Title, artist, album of the last track (kept while nothing plays, so
    /// stopping and restarting the same track doesn't toast again).
    track: Option<(String, String, String)>,
}

/// What the toast page shows.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToastPayload {
    zone_name: String,
    title: String,
    artist: String,
    album: String,
    image_key: Option<String>,
    /// How long it stays.
    seconds: u32,
    /// The Toaster's color setting, so the toast matches it.
    colors: ToasterColors,
}

/// Called by zones.rs after every zone update. Shows a toast when the
/// selected zone moves on to a different track, unless toasts are off. It
/// shows even with the Toaster open (Vulk's call: redundant, but fine).
/// Switching zones, or the first update after starting or reconnecting,
/// never toasts.
pub fn zones_updated(app: &AppHandle, payload: &ZonesPayload) {
    let zone = payload
        .selected_zone_id
        .as_ref()
        .and_then(|id| payload.zones.iter().find(|z| &z.zone_id == id));
    let now_playing = zone.and_then(|z| z.now_playing.as_ref());
    let track = now_playing.map(|np| (np.title.clone(), np.artist.clone(), np.album.clone()));

    let new_track = {
        let state = app.state::<ToastState>();
        let mut seen = state.seen.lock().unwrap();
        if !seen.started || seen.zone_id != payload.selected_zone_id {
            seen.started = true;
            seen.zone_id = payload.selected_zone_id.clone();
            seen.track = track;
            false
        } else if track.is_some() && track != seen.track {
            seen.track = track;
            true
        } else {
            false
        }
    };
    if !new_track {
        return;
    }
    let (Some(zone), Some(np)) = (zone, now_playing) else {
        return;
    };
    if np.title.is_empty() {
        return;
    }

    let saved = settings::get_settings(app.clone());
    if !saved.toasts_enabled {
        return;
    }
    // A toast on top of an exclusive full-screen game minimizes the game, so
    // none is shown then (borderless windowed games are fine and still get one)
    if exclusive_full_screen() {
        return;
    }
    send(
        app,
        ToastPayload {
            zone_name: zone.name.clone(),
            title: np.title.clone(),
            artist: np.artist.clone(),
            album: np.album.clone(),
            image_key: np.image_key.clone(),
            seconds: saved.toast_seconds,
            colors: saved.toaster_colors,
        },
    );
}

/// Whether a game is running in exclusive full-screen mode (or Windows is in
/// presentation mode). Windows reports plain full-screen windows (borderless
/// games, videos) as something else, and those are left alone.
#[cfg(windows)]
fn exclusive_full_screen() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    // SAFETY: no arguments; just asks Windows.
    unsafe {
        matches!(
            SHQueryUserNotificationState(),
            Ok(QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE)
        )
    }
}

#[cfg(not(windows))]
fn exclusive_full_screen() -> bool {
    false
}

fn send(app: &AppHandle, payload: ToastPayload) {
    let _ = app.emit_to(TOAST_WINDOW, "toast-show", payload);
}

/// Settings' "Test" button: shows a toast for what's playing right now (or a
/// sample when nothing is), even with toasts off.
#[tauri::command]
pub fn toast_test(app: AppHandle) {
    let saved = settings::get_settings(app.clone());
    let zones = app.state::<Zones>().payload_now();
    let zone = zones
        .selected_zone_id
        .as_ref()
        .and_then(|id| zones.zones.iter().find(|z| &z.zone_id == id));
    let payload = match zone.and_then(|z| z.now_playing.as_ref().map(|np| (z, np))) {
        Some((zone, np)) => ToastPayload {
            zone_name: zone.name.clone(),
            title: np.title.clone(),
            artist: np.artist.clone(),
            album: np.album.clone(),
            image_key: np.image_key.clone(),
            seconds: saved.toast_seconds,
            colors: saved.toaster_colors,
        },
        None => ToastPayload {
            zone_name: zone.map(|z| z.name.clone()).unwrap_or_default(),
            title: "Nothing playing".into(),
            artist: "Play something in Roon".into(),
            album: "and the toast shows it here".into(),
            image_key: None,
            seconds: saved.toast_seconds,
            colors: saved.toaster_colors,
        },
    };
    send(&app, payload);
}

/// Called by the toast page once it's filled in: sizes the toast for the
/// zoom setting, puts it in the chosen corner of the chosen screen (above the
/// taskbar) and shows it without taking focus.
#[tauri::command]
pub fn toast_present(app: AppHandle) -> Result<(), String> {
    let window = app.get_webview_window(TOAST_WINDOW).ok_or("The toast window is missing.")?;
    let saved = settings::get_settings(app.clone());
    let monitor = crate::monitors::find(&app, saved.toast_monitor.as_deref())
        .ok_or("No screen found.")?;

    // Everything in real screen pixels
    let scale = monitor.scale_factor();
    let zoom = saved.zoom * settings::BASE_ZOOM;
    let width = (TOAST_WIDTH * zoom * scale).round() as i32;
    let height = (TOAST_HEIGHT * zoom * scale).round() as i32;
    let margin = (MARGIN * scale).round() as i32;

    // The screen minus the taskbar
    let area = monitor.work_area();

    // Moving onto a screen with a different scale makes Windows resize the
    // window to suit, so move it onto the screen first, then set its size
    let _ = window.set_position(PhysicalPosition::new(area.position.x, area.position.y));
    let _ = window.set_size(PhysicalSize::new(width as u32, height as u32));
    let left = area.position.x + margin;
    let right = area.position.x + area.size.width as i32 - margin - width;
    let top = area.position.y + margin;
    let bottom = area.position.y + area.size.height as i32 - margin - height;
    let (x, y) = match saved.toast_position {
        ToastPosition::TopLeft => (left, top),
        ToastPosition::TopRight => (right, top),
        ToastPosition::BottomLeft => (left, bottom),
        ToastPosition::BottomRight => (right, bottom),
    };

    // Windows may put invisible borders around the visible window; place the
    // visible part, not the borders, in the corner
    let (dx, dy) = match (window.outer_position(), window.inner_position()) {
        (Ok(outer), Ok(inner)) => (inner.x - outer.x, inner.y - outer.y),
        _ => (0, 0),
    };
    let _ = window.set_position(PhysicalPosition::new(x - dx, y - dy));

    wake_webview(&window);
    show_without_focus(&window);
    Ok(())
}

/// Hides the toast; the page calls this once it has faded out.
#[tauri::command]
pub fn toast_hide(app: AppHandle) {
    if let Some(window) = app.get_webview_window(TOAST_WINDOW) {
        hide_window(&window);
    }
}

// Showing and hiding go straight to Windows, both of them: Tauri's own show()
// would take focus from whatever the user is doing, and it keeps its own note
// of whether the window is showing, which would then be wrong, so its hide()
// would do nothing.

/// Tells the toast's page (WebView2) that it is about to be seen, by switching
/// its visibility off and on again. The window is shown and hidden behind
/// WebView2's back (see above), and after a screen change (resolution, scaling,
/// HDR) it could come back as a blank white box until the app was restarted;
/// this makes WebView2 start drawing again on every toast.
#[cfg(windows)]
fn wake_webview(window: &WebviewWindow) {
    let _ = window.with_webview(|webview| {
        // SAFETY: a valid WebView2 controller; with_webview runs this on the
        // thread that owns the window.
        unsafe {
            let controller = webview.controller();
            let _ = controller.SetIsVisible(false);
            let _ = controller.SetIsVisible(true);
        }
    });
}

#[cfg(not(windows))]
fn wake_webview(_window: &WebviewWindow) {}

/// Shows the window on top of other windows without taking focus.
#[cfg(windows)]
fn show_without_focus(window: &WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    if let Ok(hwnd) = window.hwnd() {
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
}

#[cfg(windows)]
fn hide_window(window: &WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    if let Ok(hwnd) = window.hwnd() {
        // SAFETY: a valid window handle.
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn show_without_focus(window: &WebviewWindow) {
    let _ = window.show();
}

#[cfg(not(windows))]
fn hide_window(window: &WebviewWindow) {
    let _ = window.hide();
}
