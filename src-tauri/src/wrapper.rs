// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Wrapper ("Roon: Wrapped", experimental, off by default): the real
//! Roon desktop app shown inside a Toasted window, with Toasted's player
//! along the bottom.
//!
//! The "wrapper" window (page wrapper.html) has a hole in it: the page
//! reports where its empty middle is, and this module cuts that rectangle
//! out of the window with `SetWindowRgn`. Windows then neither draws the
//! window there nor sends it clicks there, so whatever sits behind the hole
//! is what you see and what you click. This module keeps Roon's own window
//! parked right behind the hole, lined up so Roon's title bar hides above
//! our top bar and Roon's transport bar hides under our player bar. Nothing
//! is forwarded or captured: it's Roon drawing and taking input as usual,
//! just framed by us.
//!
//! Three states:
//! - Off: Roon is left alone (a normal window, or not running).
//! - Open: the Wrapper is showing, with Roon in it. Every 150 ms (and right
//!   away when the Wrapper moves or resizes) Roon is lined up with the hole,
//!   hidden while the Wrapper is minimized, un-minimized if Windows minimizes
//!   it (Win+D), and kept directly behind the Wrapper: clicking in the hole
//!   makes Roon the active window, which would normally raise it over us, so
//!   we go back on top without taking focus (Roon keeps the keyboard).
//! - Parked: the Wrapper was closed. Roon keeps running, hidden, so opening
//!   the Wrapper again is instant. If Roon shows itself (e.g. started again
//!   from the Start menu), the Wrapper opens around it.
//!
//! While Open or Parked, Roon's window is a tool window: no taskbar button
//! or Alt-Tab entry of its own (the Wrapper is the one you switch to).
//! "Put Roon back" (and quitting Toasted) returns to Off: Roon gets its own
//! window, taskbar button and old position back.
//!
//! If Toasted stops without putting Roon back (a crash, a dev rebuild, an
//! update), Roon is left hidden; the next start finds it (it still has the
//! tool-window style we gave it) and parks it again, or gives it back if
//! the Wrapper has since been switched off.

use serde::Serialize;
use tauri::AppHandle;

use crate::settings;

/// The Wrapper window's label, as set in tauri.conf.json.
pub(crate) const WRAPPER_WINDOW: &str = "wrapper";

/// What the page shows in its title bar when something isn't right.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct WrapperStatus {
    pub open: bool,
    pub roon_found: bool,
    pub note: String,
}

/// Whether the Wrapper is switched on in Settings.
pub fn enabled(app: &AppHandle) -> bool {
    settings::get_settings(app.clone()).wrapper_enabled
}

/// Called by the page with the hole's rectangle in CSS pixels, relative to
/// the page (which fills the window's client area), whenever it changes.
#[tauri::command]
pub fn wrapper_hole(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    #[cfg(windows)]
    windows_wrapper::set_hole(&app, (x, y, width, height));
    #[cfg(not(windows))]
    let _ = (app, x, y, width, height);
}

#[tauri::command]
pub fn wrapper_status(app: AppHandle) -> WrapperStatus {
    #[cfg(windows)]
    return windows_wrapper::status(&app);
    #[cfg(not(windows))]
    {
        let _ = app;
        WrapperStatus { note: "Windows only".into(), ..Default::default() }
    }
}

/// The page's "Put Roon back" button.
#[tauri::command]
pub fn wrapper_put_back(app: AppHandle) {
    put_back(&app);
}

/// In setup: watches for Roon coming to the front, and picks up a Roon
/// that an earlier run of Toasted left hidden.
pub fn start(app: &AppHandle) {
    #[cfg(windows)]
    windows_wrapper::start(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Shows the Wrapper with Roon in it (starting Roon if it isn't running).
/// Does nothing while the Wrapper is switched off.
pub fn open(app: &AppHandle) {
    #[cfg(windows)]
    if enabled(app) {
        windows_wrapper::open(app);
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// The Wrapper hotkey: hides the Wrapper (Roon stays running, hidden) when
/// it's open and in front, otherwise opens it.
pub fn toggle(app: &AppHandle) {
    #[cfg(windows)]
    if enabled(app) {
        windows_wrapper::toggle(app);
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Closing the Wrapper: hides it and Roon; Roon keeps running.
pub fn park(app: &AppHandle) {
    #[cfg(windows)]
    windows_wrapper::park(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Gives Roon its own window back and hides the Wrapper ("Put Roon back",
/// switching the Wrapper off, quitting Toasted).
pub fn put_back(app: &AppHandle) {
    #[cfg(windows)]
    windows_wrapper::put_back(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Lines Roon up with the Wrapper right now (it moved or resized).
pub fn sync(app: &AppHandle) {
    #[cfg(windows)]
    windows_wrapper::sync(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Started with Windows: opens the Wrapper if it's on and set to open at
/// sign-in.
pub fn at_sign_in(app: &AppHandle) {
    let saved = settings::get_settings(app.clone());
    if saved.wrapper_enabled && saved.wrapper_at_sign_in {
        open(app);
    }
}

/// After a settings change: switching the Wrapper off puts Roon back.
pub fn settings_changed(app: &AppHandle, before: &settings::AppSettings, after: &settings::AppSettings) {
    if before.wrapper_enabled && !after.wrapper_enabled {
        put_back(app);
    }
}

#[cfg(windows)]
mod windows_wrapper {
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use tauri::{AppHandle, Manager};
    use windows::Win32::Foundation::{HWND, POINT, RECT};

    use super::{WrapperStatus, WRAPPER_WINDOW};

    /// Height of Roon's transport bar (play controls at the bottom of its
    /// window), in logical pixels: Roon's window is made that much taller
    /// than the hole, so the bar sits hidden under our player bar.
    const ROON_BOTTOM_BAR: f64 = 100.0;

    /// Roon's window is never made shorter than this (logical pixels). Its
    /// width is always exactly the hole's: Roon collapses its sidebar below
    /// about 1400 logical px, so the Wrapper's minimum width in
    /// tauri.conf.json (1440) keeps the hole wide enough. Making Roon wider
    /// than the hole instead would let it stick out past the Wrapper.
    const ROON_MIN_HEIGHT: f64 = 500.0;

    const OFF: u8 = 0;
    const OPEN: u8 = 1;
    const PARKED: u8 = 2;
    static STATE: AtomicU8 = AtomicU8::new(OFF);

    /// Our window and Roon's window (0 = none).
    static HOST_HWND: AtomicIsize = AtomicIsize::new(0);
    static ROON_HWND: AtomicIsize = AtomicIsize::new(0);
    /// The hole, in CSS pixels relative to the page (x, y, width, height).
    static HOLE: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
    /// A hole cut out of a window: x, y, width, height in window
    /// coordinates, then the window's width and height.
    type Region = (i32, i32, i32, i32, i32, i32);

    /// The hole last cut out of the window, so the region is only replaced
    /// when it changes.
    static LAST_REGION: Mutex<Option<Region>> = Mutex::new(None);
    /// Where Roon's window was before we took it, to put it back.
    static ROON_RESTORE: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
    /// Roon's extended window style before we made it a tool window
    /// (0 = not changed).
    static ROON_EXSTYLE: AtomicIsize = AtomicIsize::new(0);
    /// One line for the page's title bar.
    static NOTE: Mutex<String> = Mutex::new(String::new());
    /// Whether the keep-in-sync loop is running (it stops itself when Off).
    static LOOP_RUNNING: AtomicBool = AtomicBool::new(false);
    /// Whether the saved maximized state has been applied this run (only
    /// possible once the window is showing).
    static MAXIMIZE_RESTORED: AtomicBool = AtomicBool::new(false);

    fn hwnd_of(cell: &AtomicIsize) -> Option<HWND> {
        let raw = cell.load(Ordering::Relaxed);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    /// Roon's window, if we're holding one and it still exists.
    fn roon_window() -> Option<HWND> {
        hwnd_of(&ROON_HWND).filter(|w| is_window(*w))
    }

    fn set_note(text: impl Into<String>) {
        *NOTE.lock().unwrap() = text.into();
    }

    pub fn status(_app: &AppHandle) -> WrapperStatus {
        WrapperStatus {
            open: STATE.load(Ordering::Relaxed) == OPEN,
            roon_found: roon_window().is_some(),
            note: NOTE.lock().unwrap().clone(),
        }
    }

    pub fn start(app: &AppHandle) {
        remember_host(app);
        watch_foreground();

        // A Roon still marked as ours: an earlier run stopped without
        // putting it back
        let Some(roon) = find_roon(true).filter(|w| is_tool_window(*w)) else {
            return;
        };
        if super::enabled(app) {
            ROON_HWND.store(roon.0 as isize, Ordering::Relaxed);
            *ROON_RESTORE.lock().unwrap() = None;
            hide(roon);
            STATE.store(PARKED, Ordering::Relaxed);
            ensure_loop(app);
        } else {
            ROON_HWND.store(roon.0 as isize, Ordering::Relaxed);
            release(roon);
            show_where_it_is(roon);
            ROON_HWND.store(0, Ordering::Relaxed);
        }
    }

    fn remember_host(app: &AppHandle) {
        if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
            if let Ok(hwnd) = window.hwnd() {
                HOST_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            }
        }
    }

    pub fn set_hole(app: &AppHandle, hole: (f64, f64, f64, f64)) {
        *HOLE.lock().unwrap() = Some(hole);
        sync(app);
    }

    pub fn open(app: &AppHandle) {
        let Some(window) = app.get_webview_window(WRAPPER_WINDOW) else {
            return;
        };
        remember_host(app);

        if roon_window().is_none() {
            match find_roon(true) {
                Some(roon) => {
                    adopt(roon);
                    set_note("");
                }
                None => {
                    ROON_HWND.store(0, Ordering::Relaxed);
                    *ROON_RESTORE.lock().unwrap() = None;
                    if launch_roon() {
                        set_note("Starting Roon...");
                    } else {
                        set_note("Roon isn't running and couldn't be started. Start Roon and it will appear here.");
                    }
                }
            }
        }

        STATE.store(OPEN, Ordering::Relaxed);
        *LAST_REGION.lock().unwrap() = None;
        // A remembered size still has to fit the screen it opens on
        crate::fit_to_screen(app, WRAPPER_WINDOW, false);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        // Maximized last time: only possible now that it's showing
        if !MAXIMIZE_RESTORED.swap(true, Ordering::Relaxed) {
            use tauri_plugin_window_state::{StateFlags, WindowExt};
            let _ = window.restore_state(StateFlags::MAXIMIZED);
        }
        sync(app);
        ensure_loop(app);
    }

    pub fn toggle(app: &AppHandle) {
        if STATE.load(Ordering::Relaxed) == OPEN {
            if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
                let showing = window.is_visible().unwrap_or(false)
                    && !window.is_minimized().unwrap_or(false);
                let roon_in_front = roon_window().is_some_and(|roon| foreground() == Some(roon));
                if showing && (window.is_focused().unwrap_or(false) || roon_in_front) {
                    park(app);
                    return;
                }
            }
        }
        open(app);
    }

    pub fn park(app: &AppHandle) {
        if STATE.load(Ordering::Relaxed) == OFF {
            if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
                let _ = window.hide();
            }
            return;
        }
        STATE.store(PARKED, Ordering::Relaxed);
        if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
            let _ = window.hide();
        }
        if let Some(roon) = roon_window() {
            hide(roon);
        }
        crate::save_window_state(app);
    }

    pub fn put_back(app: &AppHandle) {
        let before = STATE.swap(OFF, Ordering::Relaxed);
        if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
            if before == OPEN {
                crate::save_window_state(app);
            }
            let _ = window.hide();
        }
        if let Some(host) = hwnd_of(&HOST_HWND) {
            clear_region(host);
        }
        *LAST_REGION.lock().unwrap() = None;
        if let Some(roon) = roon_window() {
            release(roon);
            match ROON_RESTORE.lock().unwrap().take() {
                Some(rect) => place_window(roon, rect),
                None => show_where_it_is(roon),
            }
            bring_to_front(roon);
        }
        ROON_HWND.store(0, Ordering::Relaxed);
        set_note("");
    }

    /// Runs `sync` every 150 ms until the Wrapper is switched Off.
    fn ensure_loop(app: &AppHandle) {
        if LOOP_RUNNING.swap(true, Ordering::Relaxed) {
            return;
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(150));
            loop {
                tick.tick().await;
                if STATE.load(Ordering::Relaxed) == OFF {
                    break;
                }
                sync(&app);
            }
            LOOP_RUNNING.store(false, Ordering::Relaxed);
        });
    }

    /// One pass of keeping Roon where it belongs. Cheap enough to run every
    /// 150 ms: a few window queries, and a move only when something changed.
    pub fn sync(app: &AppHandle) {
        match STATE.load(Ordering::Relaxed) {
            PARKED => sync_parked(app),
            OPEN => sync_open(app),
            _ => {}
        }
    }

    /// Parked: Roon stays hidden. If it quit, forget it; if it showed itself
    /// (started again from the Start menu, say), open the Wrapper around it.
    fn sync_parked(app: &AppHandle) {
        let Some(roon) = hwnd_of(&ROON_HWND) else {
            return;
        };
        if !is_window(roon) {
            ROON_HWND.store(0, Ordering::Relaxed);
            return;
        }
        if is_visible(roon) {
            open(app);
        }
    }

    fn sync_open(app: &AppHandle) {
        let Some(host) = hwnd_of(&HOST_HWND) else {
            return;
        };
        let Some(window) = app.get_webview_window(WRAPPER_WINDOW) else {
            return;
        };

        // Roon may have started (or quit) since the last pass
        let roon = match roon_window() {
            Some(roon) => Some(roon),
            None => {
                let found = find_roon(true);
                match found {
                    Some(roon) => {
                        adopt(roon);
                        set_note("");
                    }
                    None => ROON_HWND.store(0, Ordering::Relaxed),
                }
                found
            }
        };

        // The hole, in real pixels on the screen
        let scale = window.scale_factor().unwrap_or(1.0);
        let Some((hx, hy, hw, hh)) = *HOLE.lock().unwrap() else {
            return;
        };
        let Some(client_origin) = client_origin(host) else {
            return;
        };
        let Some(host_rect) = window_rect(host) else {
            return;
        };
        let hole = (
            client_origin.x + (hx * scale).round() as i32,
            client_origin.y + (hy * scale).round() as i32,
            (hw * scale).round() as i32,
            (hh * scale).round() as i32,
        );

        // Cut the hole out of our window (only when it changed); no Roon yet
        // means no hole, so the page's "Starting Roon" background shows
        let region_key = (
            hole.0 - host_rect.0,
            hole.1 - host_rect.1,
            hole.2,
            hole.3,
            host_rect.2,
            host_rect.3,
        );
        {
            let mut last = LAST_REGION.lock().unwrap();
            if roon.is_some() && hole.2 > 0 && hole.3 > 0 {
                if *last != Some(region_key) {
                    cut_hole(host, region_key);
                    *last = Some(region_key);
                }
            } else if last.is_some() {
                clear_region(host);
                *last = None;
            }
        }

        let Some(roon) = roon else {
            return;
        };

        // We're minimized or hidden: Roon waits out of sight
        if is_minimized(host) || !is_visible(host) {
            hide(roon);
            return;
        }
        // Roon got minimized on its own (Win+D, "minimize all"): bring it
        // back without making it active
        if is_minimized(roon) {
            show_no_activate(roon);
        }

        // Roon's client area = the hole plus its transport bar below it
        let bar = (ROON_BOTTOM_BAR * scale).round() as i32;
        let min_h = (ROON_MIN_HEIGHT * scale).round() as i32;
        let client = (hole.0, hole.1, hole.2, (hole.3 + bar).max(min_h));
        place_roon(roon, client, host);
        keep_order(host, roon);
    }

    /// Moves Roon so its client area is `client` (x, y, width, height in
    /// real pixels), accounting for Roon's own title bar and borders, shows
    /// it, and puts it directly behind `behind`.
    fn place_roon(roon: HWND, client: (i32, i32, i32, i32), behind: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_SHOWWINDOW};
        let (Some(rect), Some(origin), Some(size)) =
            (window_rect(roon), client_origin(roon), client_size(roon))
        else {
            return;
        };
        // Roon's frame: how far its client area sits inside its window
        let left = origin.x - rect.0;
        let top = origin.y - rect.1;
        let right = (rect.0 + rect.2) - (origin.x + size.0);
        let bottom = (rect.1 + rect.3) - (origin.y + size.1);
        let want = (
            client.0 - left,
            client.1 - top,
            client.2 + left + right,
            client.3 + top + bottom,
        );
        if want == rect && is_visible(roon) {
            return;
        }
        // SAFETY: valid window handles.
        unsafe {
            let _ = SetWindowPos(
                roon,
                Some(behind),
                want.0,
                want.1,
                want.2,
                want.3,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// Puts a window at `rect` (its whole window: x, y, width, height) and
    /// shows it.
    fn place_window(hwnd: HWND, (x, y, w, h): (i32, i32, i32, i32)) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOZORDER, SWP_SHOWWINDOW};
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_SHOWWINDOW);
        }
    }

    fn show_where_it_is(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
        };
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW,
            );
        }
    }

    fn bring_to_front(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
        // SAFETY: a valid window handle. We were just clicked, so Windows
        // lets us hand the foreground on.
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
    }

    /// Keeps Roon directly behind the Wrapper. If Roon has come out on top
    /// (it was clicked, which activates it), the Wrapper goes back above it
    /// without taking focus, so typing still goes to Roon.
    fn keep_order(host: HWND, roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindow, SetWindowPos, GW_HWNDNEXT, GW_HWNDPREV, HWND_TOP, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE,
        };
        // SAFETY: plain window queries and moves on valid handles.
        unsafe {
            // Is Roon somewhere above us?
            let mut window = host;
            for _ in 0..200 {
                match GetWindow(window, GW_HWNDPREV) {
                    Ok(above) if above == roon => {
                        let _ = SetWindowPos(
                            host,
                            Some(HWND_TOP),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                        break;
                    }
                    Ok(above) if !above.is_invalid() => window = above,
                    _ => break,
                }
            }
            // Directly below us?
            if GetWindow(host, GW_HWNDNEXT).ok() != Some(roon) {
                let _ = SetWindowPos(
                    roon,
                    Some(host),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Cuts a hole out of the window.
    fn cut_hole(hwnd: HWND, (x, y, w, h, width, height): Region) {
        use windows::Win32::Graphics::Gdi::{
            CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, RGN_DIFF,
        };
        // SAFETY: GDI calls on regions we just made; the window takes
        // ownership of the final region in SetWindowRgn.
        unsafe {
            let whole = CreateRectRgn(0, 0, width, height);
            let hole = CreateRectRgn(x, y, x + w, y + h);
            let _ = CombineRgn(Some(whole), Some(whole), Some(hole), RGN_DIFF);
            let _ = DeleteObject(hole.into());
            let _ = SetWindowRgn(hwnd, Some(whole), true);
        }
    }

    fn clear_region(hwnd: HWND) {
        use windows::Win32::Graphics::Gdi::SetWindowRgn;
        // SAFETY: a valid window handle; None removes the region.
        unsafe {
            let _ = SetWindowRgn(hwnd, None, true);
        }
    }

    /// Roon's main window: the biggest titled, unowned window with a title
    /// bar that belongs to a process whose program is Roon.exe. Visible
    /// windows win; with `include_ours`, a hidden one counts too if it has
    /// the tool-window style we give Roon (one we parked earlier).
    fn find_roon(include_ours: bool) -> Option<HWND> {
        use windows::core::BOOL;
        use windows::Win32::Foundation::LPARAM;
        use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

        unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the Vec we passed below, alive for the call.
            let found = unsafe { &mut *(lparam.0 as *mut Vec<(HWND, bool, i64)>) };
            if is_roon_main_window(hwnd) {
                if let Some((_, _, w, h)) = window_rect(hwnd) {
                    found.push((hwnd, is_visible(hwnd), w as i64 * h as i64));
                }
            }
            BOOL(1)
        }

        let mut found: Vec<(HWND, bool, i64)> = Vec::new();
        // SAFETY: the callback only reads the Vec through the pointer while
        // EnumWindows runs.
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
        }
        found
            .into_iter()
            .filter(|(hwnd, visible, _)| *visible || (include_ours && is_tool_window(*hwnd)))
            .max_by_key(|(_, visible, area)| (*visible, *area))
            .map(|(hwnd, _, _)| hwnd)
    }

    fn is_roon_main_window(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowThreadProcessId,
            GWL_STYLE, GW_OWNER, WS_CAPTION,
        };
        // SAFETY: plain window queries.
        unsafe {
            if GetWindowTextLengthW(hwnd) == 0 {
                return false;
            }
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            if style & WS_CAPTION.0 != WS_CAPTION.0 {
                return false;
            }
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
                return false;
            }
            match window_rect(hwnd) {
                Some((_, _, w, h)) if w >= 300 && h >= 200 => {}
                _ => return false,
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == std::process::id() {
                return false;
            }
            process_exe_name(pid).is_some_and(|name| name.eq_ignore_ascii_case("roon.exe"))
        }
    }

    /// The file name of a process's program (e.g. "Roon.exe").
    fn process_exe_name(pid: u32) -> Option<String> {
        use windows::core::PWSTR;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: the handle is closed before returning; the buffer outlives
        // the query.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buffer = [0u16; 1024];
            let mut len = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(process);
            result.ok()?;
            let path = String::from_utf16_lossy(&buffer[..len as usize]);
            path.rsplit(['\\', '/']).next().map(str::to_string)
        }
    }

    /// Starts Roon from its usual per-user install folder.
    fn launch_roon() -> bool {
        let Some(local) = std::env::var_os("LOCALAPPDATA") else {
            return false;
        };
        let exe = std::path::PathBuf::from(local).join("Roon").join("Application").join("Roon.exe");
        if !exe.is_file() {
            return false;
        }
        std::process::Command::new(exe).spawn().is_ok()
    }

    /// Takes Roon's window into the Wrapper: remembers where it was and its
    /// style, un-maximizes it, and makes it a tool window (no taskbar
    /// button or Alt-Tab entry). The style only takes effect on the taskbar
    /// once the window has been hidden and shown again; `sync` shows it.
    fn adopt(roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        };
        ROON_HWND.store(roon.0 as isize, Ordering::Relaxed);
        // A Roon we parked in an earlier run has no old position to go back to
        let ours = is_tool_window(roon);
        *ROON_RESTORE.lock().unwrap() = if ours { None } else { window_rect(roon) };
        restore_if_maximized(roon);
        // SAFETY: style queries and changes on a valid window handle.
        unsafe {
            let exstyle = GetWindowLongPtrW(roon, GWL_EXSTYLE);
            if !ours && exstyle != 0 && ROON_EXSTYLE.load(Ordering::Relaxed) == 0 {
                ROON_EXSTYLE.store(exstyle, Ordering::Relaxed);
            }
            let tool = (exstyle | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            if tool != exstyle {
                hide(roon);
                SetWindowLongPtrW(roon, GWL_EXSTYLE, tool);
            }
        }
    }

    /// Gives Roon its own taskbar button back (the style it had before we
    /// took it; for one parked by an earlier run, just without the
    /// tool-window style). Leaves it hidden; the caller shows it.
    fn release(roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
        };
        let saved = ROON_EXSTYLE.swap(0, Ordering::Relaxed);
        // SAFETY: style queries and changes on a valid window handle.
        unsafe {
            let current = GetWindowLongPtrW(roon, GWL_EXSTYLE);
            let original = if saved != 0 {
                saved
            } else {
                current & !(WS_EX_TOOLWINDOW.0 as isize)
            };
            if original != current {
                hide(roon);
                SetWindowLongPtrW(roon, GWL_EXSTYLE, original);
            }
        }
    }

    fn is_tool_window(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW};
        // SAFETY: a plain style query.
        let exstyle = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        exstyle & WS_EX_TOOLWINDOW.0 as isize != 0
    }

    fn foreground() -> Option<HWND> {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        // SAFETY: no arguments.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_invalid()).then_some(hwnd)
    }

    fn show_no_activate(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
        // SAFETY: a valid window handle.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }

    fn is_window(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IsWindow;
        // SAFETY: any handle value is fine to ask about.
        unsafe { IsWindow(Some(hwnd)).as_bool() }
    }

    fn is_visible(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
        // SAFETY: any handle value is fine to ask about.
        unsafe { IsWindowVisible(hwnd).as_bool() }
    }

    fn is_minimized(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IsIconic;
        // SAFETY: a valid window handle.
        unsafe { IsIconic(hwnd).as_bool() }
    }

    fn restore_if_maximized(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{IsZoomed, ShowWindow, SW_RESTORE};
        // SAFETY: a valid window handle.
        unsafe {
            if IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
        }
    }

    fn hide(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
        if is_visible(hwnd) {
            // SAFETY: a valid window handle.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }

    /// (x, y, width, height) of a window in real pixels on the screen.
    fn window_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
        let mut rect = RECT::default();
        // SAFETY: a valid window handle and out-pointer.
        unsafe {
            GetWindowRect(hwnd, &mut rect).ok()?;
        }
        Some((rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top))
    }

    /// Where a window's client area starts on the screen.
    fn client_origin(hwnd: HWND) -> Option<POINT> {
        use windows::Win32::Graphics::Gdi::ClientToScreen;
        let mut point = POINT::default();
        // SAFETY: a valid window handle and out-pointer.
        unsafe {
            if !ClientToScreen(hwnd, &mut point).as_bool() {
                return None;
            }
        }
        Some(point)
    }

    /// A window's client area size in real pixels.
    fn client_size(hwnd: HWND) -> Option<(i32, i32)> {
        use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
        let mut rect = RECT::default();
        // SAFETY: a valid window handle and out-pointer.
        unsafe {
            GetClientRect(hwnd, &mut rect).ok()?;
        }
        Some((rect.right - rect.left, rect.bottom - rect.top))
    }

    /// Puts the Wrapper back above Roon the moment Roon becomes the active
    /// window (clicking in the hole). Set up on the main thread, whose
    /// message loop delivers the events.
    fn watch_foreground() {
        use windows::Win32::UI::Accessibility::SetWinEventHook;
        use windows::Win32::UI::WindowsAndMessaging::{EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT};
        // SAFETY: a valid callback that lives for the whole program.
        unsafe {
            let _ = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_foreground),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
        }
    }

    unsafe extern "system" fn on_foreground(
        _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
        _event: u32,
        window: HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if STATE.load(Ordering::Relaxed) != OPEN {
            return;
        }
        if let (Some(host), Some(roon)) = (hwnd_of(&HOST_HWND), hwnd_of(&ROON_HWND)) {
            if window == roon && is_visible(host) {
                keep_order(host, roon);
            }
        }
    }
}
