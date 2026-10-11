// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The icon in the tray and on the Toaster's and Settings' taskbar buttons:
//! Vulk's simplified toaster as a flat shape, white on a dark taskbar and
//! black on a light one, like Windows' own tray icons. It switches when the
//! taskbar's light/dark mode changes. While the Wrapper is switched on, the
//! tray shows the Roon: Wrapped mark instead (in color, so it suits either
//! taskbar).
//!
//! icons/tray.ico holds the white shape at 16 to 64 px; the size that fits the
//! screen's scaling is picked, so it stays sharp. (The full-color app icon,
//! icons/icon.ico, is still what the installer, Start menu and Explorer show.)

use std::io::Cursor;
use std::time::Duration;

use tauri::image::Image;
use tauri::{AppHandle, Manager};

const TRAY_FILE: &[u8] = include_bytes!("../icons/tray.ico");

/// The Roon: Wrapped mark, in the sizes Windows uses at each display
/// scaling: the tray icon while the Wrapper is on, and the Wrapper window's
/// own icons (wrapper.rs). 16 and 24 are drawn on the pixel grid; the rest
/// are scaled from the full-size artwork.
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

/// The shape at about `size` pixels (the closest size in the file, bigger
/// rather than smaller), white, or black when `light` is true.
pub fn taskbar_icon(size: u32, light: bool) -> Option<Image<'static>> {
    let dir = ico::IconDir::read(Cursor::new(TRAY_FILE)).ok()?;
    let mut entries: Vec<&ico::IconDirEntry> = dir.entries().iter().collect();
    entries.sort_by_key(|entry| entry.width());
    let entry = entries
        .iter()
        .find(|entry| entry.width() >= size)
        .or(entries.last())?;
    let image = entry.decode().ok()?;
    let mut rgba = image.rgba_data().to_vec();
    if light {
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[..3].fill(0);
        }
    }
    Some(Image::new_owned(rgba, image.width(), image.height()))
}

/// The main screen's scaling (1.0 = 100%).
fn screen_scale(app: &AppHandle) -> f64 {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor.scale_factor())
        .unwrap_or(1.0)
}

/// The Wrapped mark at about `size` pixels (the closest size, bigger rather
/// than smaller).
pub fn wrapped_icon(size: u32) -> Option<Image<'static>> {
    let size = size as i32;
    let &(_, png) = WRAPPED_ICONS
        .iter()
        .find(|(s, _)| *s >= size)
        .or(WRAPPED_ICONS.last())?;
    let image = ico::IconImage::read_png(Cursor::new(png)).ok()?;
    Some(Image::new_owned(image.rgba_data().to_vec(), image.width(), image.height()))
}

/// The tray icon (16 px at 100% scaling): the Wrapped mark while the
/// Wrapper is on, otherwise the toaster for the taskbar's mode (`light`).
fn tray_image(app: &AppHandle, light: bool) -> Option<Image<'static>> {
    let size = (16.0 * screen_scale(app)).ceil() as u32;
    if crate::wrapper::enabled(app) {
        wrapped_icon(size)
    } else {
        taskbar_icon(size, light)
    }
}

/// The tray icon for now (see `tray_image`).
pub fn tray_icon(app: &AppHandle) -> Option<Image<'static>> {
    tray_image(app, taskbar_is_light())
}

/// Puts the right icon in the tray, after the Wrapper is switched on or off.
pub fn refresh_tray(app: &AppHandle) {
    if let (Some(tray), Some(icon)) = (app.tray_by_id("tray"), tray_icon(app)) {
        let _ = tray.set_icon(Some(icon));
    }
}

/// Sets the tray icon and the windows' taskbar-button icons (24 px at 100%
/// scaling) for the current taskbar mode.
fn apply(app: &AppHandle, light: bool) {
    let scale = screen_scale(app);
    if let (Some(tray), Some(icon)) = (app.tray_by_id("tray"), tray_image(app, light)) {
        let _ = tray.set_icon(Some(icon));
    }
    if let Some(icon) = taskbar_icon((24.0 * scale).ceil() as u32, light) {
        for label in [crate::TOASTER_WINDOW, crate::SETTINGS_WINDOW] {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.set_icon(icon.clone());
            }
        }
    }
}

/// Sets the icons now (in setup, after the tray exists), then checks the
/// taskbar's light/dark mode every couple of seconds and switches them.
pub fn start(app: &AppHandle) {
    let mut light = taskbar_is_light();
    apply(app, light);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let now = taskbar_is_light();
            if now != light {
                light = now;
                apply(&app, light);
            }
        }
    });
}

/// Whether the taskbar is in light mode (Settings > Personalization > Colors:
/// "Choose your default Windows mode" = Light). Dark if it can't be read.
#[cfg(windows)]
fn taskbar_is_light() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: a pointer to a u32 and its size.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    result == ERROR_SUCCESS && value == 1
}

#[cfg(not(windows))]
fn taskbar_is_light() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{taskbar_icon, wrapped_icon, WRAPPED_ICONS};

    #[test]
    fn icon_sizes_and_colors() {
        // Exact sizes come back as they are; in-between sizes round up
        for (asked, got) in [(16, 16), (20, 20), (24, 24), (30, 32), (36, 40), (48, 48), (300, 64)] {
            let icon = taskbar_icon(asked, false).expect("icon decodes");
            assert_eq!((icon.width(), icon.height()), (got, got));
        }
        // Dark taskbar: white; light taskbar: black (same shape)
        let white = taskbar_icon(32, false).unwrap();
        let black = taskbar_icon(32, true).unwrap();
        let opaque = |rgba: &[u8]| rgba.chunks(4).filter(|p| p[3] > 128).count();
        assert_eq!(opaque(white.rgba()), opaque(black.rgba()));
        assert!(white.rgba().chunks(4).filter(|p| p[3] > 128).all(|p| p[0] > 200));
        assert!(black.rgba().chunks(4).filter(|p| p[3] > 128).all(|p| p[0] == 0));
    }

    #[test]
    fn wrapped_sizes() {
        // Every embedded size decodes at its own size
        for &(size, _) in WRAPPED_ICONS {
            let icon = wrapped_icon(size as u32).expect("icon decodes");
            assert_eq!((icon.width(), icon.height()), (size as u32, size as u32));
        }
        // In-between sizes round up; past the biggest, the biggest
        assert_eq!(wrapped_icon(18).unwrap().width(), 20);
        assert_eq!(wrapped_icon(1000).unwrap().width(), 256);
    }
}
