// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Square corners while a window sits in a Zonekeeper zone.
//!
//! Zonekeeper (Vulk's window-snapping Stream Deck plugin) squares the
//! corners of the windows it places in a zone through Windows itself (the
//! DWM corner setting), and lists them in
//! %LOCALAPPDATA%\Zonekeeper\corners.json (window handle and where it put
//! it) until they leave their zone, when it rounds them again. Two kinds of
//! our windows need help with that:
//! - The Wrapper draws its rounded corners itself (the page's panel; the
//!   window is transparent), which Windows' setting doesn't reach. While
//!   it's listed and still where Zonekeeper put it, its page is told to
//!   square them ("zone-squared" event, `zone_squared` command).
//! - The Toaster, Settings and the Changelog are rounded by us
//!   (`round_corners` in lib.rs). Zonekeeper's "back to normal" is Windows'
//!   default, which for a frameless window is square, so they're rounded
//!   again when they leave their zone.
//!
//! Checked every second (the file is tiny); nothing happens without
//! Zonekeeper. Windows only.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, WebviewWindow};

#[cfg(windows)]
use windows_zones::start_checking;

/// Our windows that are squared right now, by label.
static STATE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// Starts the once-a-second check (in setup).
pub fn start(app: &AppHandle) {
    #[cfg(windows)]
    start_checking(app);
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(windows)]
mod windows_zones {
    use std::collections::HashMap;
    use std::time::Duration;

    use serde::Deserialize;
    use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

    use super::STATE;

    /// How far a window may be from where Zonekeeper put it and still count as
    /// in its zone (real pixels; Zonekeeper's own tolerance).
    const TOLERANCE: i32 = 4;

    #[derive(Deserialize, Default)]
    struct CornerFile {
        #[serde(default)]
        squared: Vec<Squared>,
    }

    #[derive(Deserialize)]
    struct Squared {
        hwnd: usize,
        frame: Frame,
        #[serde(default = "yes")]
        squared: bool,
    }

    #[derive(Deserialize, Clone, Copy)]
    struct Frame {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    }

    fn yes() -> bool {
        true
    }

    /// The windows this watches.
    fn labels() -> [&'static str; 4] {
        [
            crate::wrapper::WRAPPER_WINDOW,
            crate::TOASTER_WINDOW,
            crate::SETTINGS_WINDOW,
            crate::changelog::CHANGELOG_WINDOW,
        ]
    }

    fn read_file() -> CornerFile {
        let Some(base) = std::env::var_os("LOCALAPPDATA") else {
            return CornerFile::default();
        };
        let path = std::path::PathBuf::from(base).join("Zonekeeper").join("corners.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Where a window is drawn (its visible frame, as Zonekeeper measures it).
    fn frame_of(window: &WebviewWindow) -> Option<Frame> {
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
        // (Tauri's own copy of the windows crate has its own HWND type)
        let hwnd = HWND(window.hwnd().ok()?.0);
        let mut rect = RECT::default();
        // SAFETY: a valid window handle and a RECT-sized out-buffer.
        unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut rect as *mut RECT as *mut core::ffi::c_void,
                std::mem::size_of::<RECT>() as u32,
            )
            .ok()?;
        }
        Some(Frame { x: rect.left, y: rect.top, w: rect.right - rect.left, h: rect.bottom - rect.top })
    }

    fn close(a: Frame, b: Frame) -> bool {
        (a.x - b.x).abs() <= TOLERANCE
            && (a.y - b.y).abs() <= TOLERANCE
            && (a.w - b.w).abs() <= TOLERANCE
            && (a.h - b.h).abs() <= TOLERANCE
    }

    /// Whether Zonekeeper has this window squared in a zone right now.
    fn squared_in(file: &CornerFile, window: &WebviewWindow) -> bool {
        let Ok(hwnd) = window.hwnd() else {
            return false;
        };
        if !window.is_visible().unwrap_or(false) || window.is_maximized().unwrap_or(false) {
            return false;
        }
        let Some(now) = frame_of(window) else {
            return false;
        };
        file.squared
            .iter()
            .any(|s| s.hwnd == hwnd.0 as usize && s.squared && close(s.frame, now))
    }

    pub fn start_checking(app: &AppHandle) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let handle = app.clone();
                // Window queries belong on the main thread
                let _ = app.run_on_main_thread(move || check(&handle));
            }
        });
    }

    pub(super) fn check(app: &AppHandle) {
        let file = read_file();
        let mut guard = STATE.lock().unwrap();
        let state = guard.get_or_insert_with(HashMap::new);
        for label in labels() {
            let Some(window) = app.get_webview_window(label) else {
                continue;
            };
            let now = squared_in(&file, &window);
            let before = state.insert(label.to_string(), now).unwrap_or(false);
            if now == before {
                continue;
            }
            if label == crate::wrapper::WRAPPER_WINDOW {
                let _ = app.emit_to(crate::wrapper::WRAPPER_WINDOW, "zone-squared", now);
            } else if !now {
                // Out of its zone: our rounding back
                crate::round_corners(&window);
            }
        }
    }
}

/// For a page: whether its window is squared in a Zonekeeper zone right now
/// (the Wrapper asks when it loads; changes come as "zone-squared").
#[tauri::command]
pub fn zone_squared(window: WebviewWindow) -> bool {
    STATE
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|state| state.get(window.label()).copied())
        .unwrap_or(false)
}
