//! The screens connected to the computer, with the names Windows shows for
//! them in Settings > Display (e.g. "DELL U2720Q"), so Settings can offer a
//! choice of screen for the toast.
//!
//! Tauri knows each screen only by its technical name ("\\.\DISPLAY2"),
//! which can change when screens are plugged in again, so on Windows each
//! screen is saved by its device path instead (stays the same for the same
//! screen on the same port) and matched up again when a toast is shown.

use serde::Serialize;
use tauri::{AppHandle, Monitor};

/// One screen, for the list in Settings.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    /// What's saved as `toastMonitor`.
    pub id: String,
    /// e.g. "DELL U2720Q"
    pub name: String,
    /// Whether Windows has it as the main display.
    pub primary: bool,
    /// Resolution in pixels.
    pub width: u32,
    pub height: u32,
}

/// A screen as Windows describes it.
struct Display {
    /// "\\.\DISPLAY2", the name Tauri uses too.
    gdi_name: String,
    /// "DELL U2720Q" (can be empty, e.g. some laptop screens).
    friendly_name: String,
    /// The saved id: the device path, or the technical name if there's none.
    id: String,
}

#[cfg(windows)]
fn wide(text: &[u16]) -> String {
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

#[cfg(windows)]
fn displays() -> Vec<Display> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    let mut path_count = 0u32;
    let mut mode_count = 0u32;
    // SAFETY: pointers to valid counters and buffers of the sizes given.
    unsafe {
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            != ERROR_SUCCESS
        {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return Vec::new();
        }
        paths.truncate(path_count as usize);

        let mut found: Vec<Display> = Vec::new();
        for path in &paths {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }
            let gdi_name = wide(&source.viewGdiDeviceName);
            if found.iter().any(|d| d.gdi_name == gdi_name) {
                continue;
            }

            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;
            let (friendly_name, id) = if DisplayConfigGetDeviceInfo(&mut target.header) == 0 {
                (wide(&target.monitorFriendlyDeviceName), wide(&target.monitorDevicePath))
            } else {
                (String::new(), String::new())
            };
            found.push(Display {
                id: if id.is_empty() { gdi_name.clone() } else { id },
                gdi_name,
                friendly_name,
            });
        }
        found
    }
}

/// Outside Windows, Tauri's names are all there is.
#[cfg(not(windows))]
fn displays() -> Vec<Display> {
    Vec::new()
}

/// "\\.\DISPLAY2" -> "Display 2", for screens without a proper name.
fn fallback_name(gdi_name: &str) -> String {
    match gdi_name.rsplit("DISPLAY").next() {
        Some(number) if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) => {
            format!("Display {number}")
        }
        _ => gdi_name.to_string(),
    }
}

/// The connected screens, left to right, for Settings' Monitor list.
#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Vec<MonitorInfo> {
    let displays = displays();
    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .and_then(|m| m.name().cloned());
    let mut monitors = app.available_monitors().unwrap_or_default();
    monitors.sort_by_key(|m| (m.position().x, m.position().y));

    let mut list: Vec<MonitorInfo> = monitors
        .iter()
        .map(|monitor| {
            let gdi_name = monitor.name().cloned().unwrap_or_default();
            let display = displays.iter().find(|d| d.gdi_name == gdi_name);
            let name = display
                .map(|d| d.friendly_name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| fallback_name(&gdi_name));
            MonitorInfo {
                id: display.map(|d| d.id.clone()).unwrap_or_else(|| gdi_name.clone()),
                name,
                primary: primary.as_ref() == Some(&gdi_name),
                width: monitor.size().width,
                height: monitor.size().height,
            }
        })
        .collect();

    // Two identical screens: number them, left to right ("LG 27GL850 2")
    for i in 0..list.len() {
        let same: Vec<usize> = (0..list.len()).filter(|&j| list[j].name == list[i].name).collect();
        if same.len() > 1 && same[0] == i {
            for (n, &j) in same.iter().enumerate() {
                list[j].name = format!("{} {}", list[j].name, n + 1);
            }
        }
    }
    list
}

/// The screen saved in Settings (by id), or the main display when none is
/// saved or the saved one isn't connected right now.
pub fn find(app: &AppHandle, id: Option<&str>) -> Option<Monitor> {
    if let Some(id) = id {
        let gdi_name = displays()
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| d.gdi_name)
            .unwrap_or_else(|| id.to_string());
        let saved = app
            .available_monitors()
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.name() == Some(&gdi_name));
        if saved.is_some() {
            return saved;
        }
    }
    app.primary_monitor().ok().flatten()
}
