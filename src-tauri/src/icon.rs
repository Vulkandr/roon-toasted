//! The icon in the tray and on the Toaster's and Settings' taskbar buttons:
//! Vulk's simplified toaster as a flat shape, white on a dark taskbar and
//! black on a light one, like Windows' own tray icons. It switches when the
//! taskbar's light/dark mode changes.
//!
//! icons/tray.ico holds the white shape at 16 to 64 px; the size that fits the
//! screen's scaling is picked, so it stays sharp. (The full-color app icon,
//! icons/icon.ico, is still what the installer, Start menu and Explorer show.)

use std::io::Cursor;
use std::time::Duration;

use tauri::image::Image;
use tauri::{AppHandle, Manager};

const TRAY_FILE: &[u8] = include_bytes!("../icons/tray.ico");

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

/// The tray icon for the current taskbar mode: 16 px at 100% scaling.
pub fn tray_icon(app: &AppHandle) -> Option<Image<'static>> {
    let size = (16.0 * screen_scale(app)).ceil() as u32;
    taskbar_icon(size, taskbar_is_light())
}

/// Sets the tray icon and the windows' taskbar-button icons (24 px at 100%
/// scaling) for the current taskbar mode.
fn apply(app: &AppHandle, light: bool) {
    let scale = screen_scale(app);
    if let (Some(tray), Some(icon)) = (
        app.tray_by_id("tray"),
        taskbar_icon((16.0 * scale).ceil() as u32, light),
    ) {
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
    use super::taskbar_icon;

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
}
