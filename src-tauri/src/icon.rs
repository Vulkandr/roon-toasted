// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The icon in the tray and on the Toaster's and Settings' taskbar buttons:
//! the Roon: Toasted logo, in color (so it suits a light or dark taskbar
//! alike).
//!
//! icons/tray.ico holds it at 16 to 64 px (16, 20, 24 and 32 drawn on the
//! pixel grid); the size that fits the screen's scaling is picked, so it
//! stays sharp. The installer, Start menu and Explorer use icons/icon.ico,
//! the same logo.
//!
//! The Wrapper keeps its own mark (Roon: Wrapped) for its window only
//! (`WRAPPED_ICONS`, used by wrapper.rs).

use std::io::Cursor;

use tauri::image::Image;
use tauri::{AppHandle, Manager};

const TRAY_FILE: &[u8] = include_bytes!("../icons/tray.ico");

/// The Roon: Wrapped mark, in the sizes Windows uses at each display
/// scaling: the Wrapper window's own icons (wrapper.rs). 16 and 24 are
/// drawn on the pixel grid; the rest are scaled from the full-size artwork.
#[cfg(windows)]
pub const WRAPPED_ICONS: &[(i32, &[u8])] = &[
    (16, include_bytes!("../icons/wrapped/16.png")),
    (20, include_bytes!("../icons/wrapped/20.png")),
    (24, include_bytes!("../icons/wrapped/24.png")),
    (28, include_bytes!("../icons/wrapped/28.png")),
    (32, include_bytes!("../icons/wrapped/32.png")),
    (40, include_bytes!("../icons/wrapped/40.png")),
    (48, include_bytes!("../icons/wrapped/48.png")),
    (56, include_bytes!("../icons/wrapped/56.png")),
    (64, include_bytes!("../icons/wrapped/64.png")),
    (96, include_bytes!("../icons/wrapped/96.png")),
    (128, include_bytes!("../icons/wrapped/128.png")),
    (256, include_bytes!("../icons/wrapped/256.png")),
];

/// The logo at about `size` pixels (the closest size in the file, bigger
/// rather than smaller).
pub fn taskbar_icon(size: u32) -> Option<Image<'static>> {
    let dir = ico::IconDir::read(Cursor::new(TRAY_FILE)).ok()?;
    let mut entries: Vec<&ico::IconDirEntry> = dir.entries().iter().collect();
    entries.sort_by_key(|entry| entry.width());
    let entry = entries
        .iter()
        .find(|entry| entry.width() >= size)
        .or(entries.last())?;
    let image = entry.decode().ok()?;
    Some(Image::new_owned(image.rgba_data().to_vec(), image.width(), image.height()))
}

/// The main screen's scaling (1.0 = 100%).
fn screen_scale(app: &AppHandle) -> f64 {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor.scale_factor())
        .unwrap_or(1.0)
}

/// The tray icon: 16 px at 100% scaling.
pub fn tray_icon(app: &AppHandle) -> Option<Image<'static>> {
    taskbar_icon((16.0 * screen_scale(app)).ceil() as u32)
}

/// Sets the tray icon and the windows' taskbar-button icons (24 px at 100%
/// scaling), in setup once the tray exists.
pub fn start(app: &AppHandle) {
    if let (Some(tray), Some(icon)) = (app.tray_by_id("tray"), tray_icon(app)) {
        let _ = tray.set_icon(Some(icon));
    }
    if let Some(icon) = taskbar_icon((24.0 * screen_scale(app)).ceil() as u32) {
        for label in [crate::TOASTER_WINDOW, crate::SETTINGS_WINDOW] {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.set_icon(icon.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::taskbar_icon;

    #[test]
    fn icon_sizes() {
        // Exact sizes come back as they are; in-between sizes round up
        for (asked, got) in [(16, 16), (20, 20), (24, 24), (30, 32), (36, 40), (48, 48), (300, 64)] {
            let icon = taskbar_icon(asked).expect("icon decodes");
            assert_eq!((icon.width(), icon.height()), (got, got));
        }
    }
}
