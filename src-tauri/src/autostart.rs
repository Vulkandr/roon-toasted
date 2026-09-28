// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Start with Windows (setting `startWithWindows`, on by default): the app
//! adds itself to Windows' startup list for the current user, so it's in the
//! tray (with toasts, hotkeys and the widget ready) after signing in. It
//! starts plainly, so it stays in the tray until opened.
//!
//! The entry lives under HKEY_CURRENT_USER\Software\Microsoft\Windows\
//! CurrentVersion\Run, named after the app ("Roon Toasted", the product name),
//! which is also the name the uninstaller removes. It's rewritten at every
//! start, so it always points at wherever the app is.
//!
//! Dev builds never touch it (the entry would point at the dev build, which
//! doesn't work on its own); they only print what they would have done.

use tauri::AppHandle;

use crate::settings;

/// Adds the app to Windows' startup list or takes it off, to match the
/// setting (at startup and when the setting changes).
pub fn apply(app: &AppHandle) {
    let on = settings::get_settings(app.clone()).start_with_windows;
    if cfg!(debug_assertions) {
        println!(
            "[autostart] dev build: not changing Windows' startup list (Start with Windows is {})",
            if on { "on" } else { "off" }
        );
    } else if !write(app, on) {
        eprintln!("[autostart] couldn't update Windows' startup list");
    }
}

/// Adds or removes the startup entry; false if Windows refused.
#[cfg(windows)]
fn write(app: &AppHandle, on: bool) -> bool {
    let name = app.package_info().name.clone();
    if !on {
        return remove_value(RUN_KEY, &name);
    }
    match std::env::current_exe() {
        Ok(exe) => crate::launch::set_value(RUN_KEY, Some(&name), &format!("\"{}\"", exe.display())),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn write(_app: &AppHandle, _on: bool) -> bool {
    true
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Removes one value under HKEY_CURRENT_USER (fine if it isn't there).
#[cfg(windows)]
fn remove_value(path: &str, name: &str) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
    // SAFETY: valid strings.
    let result = unsafe {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(path), &HSTRING::from(name))
    };
    result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND
}
