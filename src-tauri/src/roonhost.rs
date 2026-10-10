// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Roon panel (prototype): shows the real Roon desktop app inside a Toasted
//! window.
//!
//! The "roonhost" window (page roonhost.html) has a hole in it: the page
//! reports where its empty middle is, and this module cuts that rectangle
//! out of the window with `SetWindowRgn`. Windows then neither draws the
//! window there nor sends it clicks there, so whatever sits behind the hole
//! is what you see and what you click. This module keeps Roon's own window
//! parked right behind the hole, lined up so Roon's title bar hides above
//! our top bar and Roon's transport bar hides under our player bar. Nothing
//! is forwarded or captured: it's Roon drawing and taking input as usual,
//! just framed by us.
//!
//! What's kept in sync (every 150 ms while attached, and right away when
//! our window moves or resizes):
//! - Roon's window follows ours (position, size, hidden while we're
//!   minimized).
//! - Z-order: Roon stays directly behind us. Clicking in the hole makes Roon
//!   the active window, which would normally raise it over us, so we put
//!   ourselves back on top without taking focus (Roon keeps the keyboard).
//!
//! While it's in the panel, Roon's window is marked as a tool window, so it
//! has no taskbar button and no Alt-Tab entry of its own (the panel is the
//! one you switch to); if Roon still gets minimized (Win+D, say), the next
//! pass un-minimizes it.
//!
//! Detaching puts Roon back where it was, with its own taskbar button, and
//! removes the hole.

use serde::Serialize;
use tauri::AppHandle;

/// The panel window's label, as set in tauri.conf.json.
pub(crate) const ROONHOST_WINDOW: &str = "roonhost";

/// What the page shows in its status line.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoonHostStatus {
    pub attached: bool,
    pub roon_found: bool,
    pub note: String,
}

/// Called by the page with the hole's rectangle in CSS pixels, relative to
/// the page (which fills the window's client area), whenever it changes.
#[tauri::command]
pub fn roonhost_hole(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    #[cfg(windows)]
    windows_host::set_hole(&app, (x, y, width, height));
    #[cfg(not(windows))]
    let _ = (app, x, y, width, height);
}

#[tauri::command]
pub fn roonhost_status(app: AppHandle) -> RoonHostStatus {
    #[cfg(windows)]
    return windows_host::status(&app);
    #[cfg(not(windows))]
    {
        let _ = app;
        RoonHostStatus { note: "Windows only".into(), ..Default::default() }
    }
}

/// Installs the foreground watcher (in setup, on the main thread).
pub fn start(app: &AppHandle) {
    #[cfg(windows)]
    windows_host::start(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Tray: shows the panel with Roon inside, or puts Roon back and hides it.
pub fn toggle(app: &AppHandle) {
    #[cfg(windows)]
    windows_host::toggle(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Puts Roon back where it was and hides the panel (closing the panel, Quit).
pub fn detach(app: &AppHandle) {
    #[cfg(windows)]
    windows_host::detach(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Lines Roon up with the panel right now (the panel moved or resized).
pub fn sync(app: &AppHandle) {
    #[cfg(windows)]
    windows_host::sync(app);
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(windows)]
mod windows_host {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use tauri::{AppHandle, Manager};
    use windows::Win32::Foundation::{HWND, POINT, RECT};

    use super::{RoonHostStatus, ROONHOST_WINDOW};

    /// Height of Roon's transport bar (play controls at the bottom of its
    /// window), in logical pixels: Roon's window is made that much taller
    /// than the hole, so the bar sits hidden under our player bar.
    const ROON_BOTTOM_BAR: f64 = 100.0;

    /// Roon's window must be at least this big (logical pixels) or Roon
    /// starts hiding its own sidebar and controls.
    const ROON_MIN_WIDTH: f64 = 900.0;
    const ROON_MIN_HEIGHT: f64 = 500.0;

    /// Our panel window and Roon's window (0 = none).
    static HOST_HWND: AtomicIsize = AtomicIsize::new(0);
    static ROON_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Whether Roon is currently framed by the panel.
    static ATTACHED: AtomicBool = AtomicBool::new(false);
    /// The hole, in CSS pixels relative to the page (x, y, width, height).
    static HOLE: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
    /// The hole last cut out of the window, in window coordinates, so the
    /// region is only replaced when it changes.
    static LAST_REGION: Mutex<Option<(i32, i32, i32, i32, i32, i32)>> = Mutex::new(None);
    /// Where Roon's window was before attaching, to put it back.
    static ROON_RESTORE: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
    /// Roon's extended window style before we made it a tool window
    /// (restored on detach; 0 = not changed).
    static ROON_EXSTYLE: AtomicIsize = AtomicIsize::new(0);
    /// One line for the page's status line.
    static NOTE: Mutex<String> = Mutex::new(String::new());
    /// Whether the sync loop is running (it stops itself when detached).
    static LOOP_RUNNING: AtomicBool = AtomicBool::new(false);

    fn hwnd_of(cell: &AtomicIsize) -> Option<HWND> {
        let raw = cell.load(Ordering::Relaxed);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    fn set_note(text: impl Into<String>) {
        *NOTE.lock().unwrap() = text.into();
    }

    pub fn status(_app: &AppHandle) -> RoonHostStatus {
        RoonHostStatus {
            attached: ATTACHED.load(Ordering::Relaxed),
            roon_found: hwnd_of(&ROON_HWND).is_some_and(is_window),
            note: NOTE.lock().unwrap().clone(),
        }
    }

    pub fn start(app: &AppHandle) {
        if let Some(window) = app.get_webview_window(ROONHOST_WINDOW) {
            if let Ok(hwnd) = window.hwnd() {
                HOST_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            }
        }
        watch_foreground();
    }

    pub fn set_hole(app: &AppHandle, hole: (f64, f64, f64, f64)) {
        *HOLE.lock().unwrap() = Some(hole);
        sync(app);
    }

    pub fn toggle(app: &AppHandle) {
        if ATTACHED.load(Ordering::Relaxed) {
            detach(app);
        } else {
            attach(app);
        }
    }

    fn attach(app: &AppHandle) {
        let Some(host) = app.get_webview_window(ROONHOST_WINDOW) else {
            return;
        };
        if let Ok(hwnd) = host.hwnd() {
            HOST_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        }

        match find_roon() {
            Some(roon) => {
                adopt(roon);
                set_note("Roon is inside the panel.");
            }
            None => {
                ROON_HWND.store(0, Ordering::Relaxed);
                *ROON_RESTORE.lock().unwrap() = None;
                if launch_roon() {
                    set_note("Starting Roon...");
                } else {
                    set_note("Roon isn't running and couldn't be started. Start Roon, then try again.");
                }
            }
        }

        ATTACHED.store(true, Ordering::Relaxed);
        *LAST_REGION.lock().unwrap() = None;
        let _ = host.unminimize();
        let _ = host.show();
        let _ = host.set_focus();
        sync(app);

        if !LOOP_RUNNING.swap(true, Ordering::Relaxed) {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_millis(150));
                loop {
                    tick.tick().await;
                    if !ATTACHED.load(Ordering::Relaxed) {
                        break;
                    }
                    sync(&app);
                }
                LOOP_RUNNING.store(false, Ordering::Relaxed);
            });
        }
    }

    pub fn detach(app: &AppHandle) {
        if !ATTACHED.swap(false, Ordering::Relaxed) {
            return;
        }
        if let Some(host) = hwnd_of(&HOST_HWND) {
            clear_region(host);
        }
        *LAST_REGION.lock().unwrap() = None;
        if let Some(window) = app.get_webview_window(ROONHOST_WINDOW) {
            let _ = window.hide();
        }
        if let Some(roon) = hwnd_of(&ROON_HWND).filter(|w| is_window(*w)) {
            release(roon);
            let restore = ROON_RESTORE.lock().unwrap().take();
            place_roon(roon, restore, None);
        }
        set_note("Detached.");
    }

    /// One pass of keeping Roon behind the hole. Cheap enough to run every
    /// 150 ms: a few window queries, and a move only when something changed.
    pub fn sync(app: &AppHandle) {
        if !ATTACHED.load(Ordering::Relaxed) {
            return;
        }
        let Some(host) = hwnd_of(&HOST_HWND) else {
            return;
        };
        let Some(window) = app.get_webview_window(ROONHOST_WINDOW) else {
            return;
        };

        // Roon may have started (or quit) since the last pass
        let roon = match hwnd_of(&ROON_HWND).filter(|w| is_window(*w)) {
            Some(roon) => Some(roon),
            None => {
                let found = find_roon();
                if let Some(roon) = found {
                    adopt(roon);
                    set_note("Roon is inside the panel.");
                } else {
                    ROON_HWND.store(0, Ordering::Relaxed);
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

        // Cut the hole out of our window (only when it changed)
        let region_key = (
            hole.0 - host_rect.0,
            hole.1 - host_rect.1,
            hole.2,
            hole.3,
            host_rect.2,
            host_rect.3,
        );
        if roon.is_some() && hole.2 > 0 && hole.3 > 0 {
            let mut last = LAST_REGION.lock().unwrap();
            if *last != Some(region_key) {
                cut_hole(host, region_key);
                *last = Some(region_key);
            }
        } else {
            let mut last = LAST_REGION.lock().unwrap();
            if last.is_some() {
                clear_region(host);
                *last = None;
            }
        }

        let Some(roon) = roon else {
            return;
        };

        // We're minimized: Roon waits out of sight
        if is_minimized(host) {
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
        let min_w = (ROON_MIN_WIDTH * scale).round() as i32;
        let min_h = (ROON_MIN_HEIGHT * scale).round() as i32;
        let client = (hole.0, hole.1, hole.2.max(min_w), (hole.3 + bar).max(min_h));
        place_roon(roon, Some(client), Some(host));
        keep_order(host, roon);
    }

    /// Moves Roon so its client area is `client` (x, y, width, height in
    /// real pixels), accounting for Roon's own title bar and borders. With
    /// `behind`, Roon is also put directly behind that window. Without a
    /// rectangle, Roon is just shown where it is.
    fn place_roon(roon: HWND, client: Option<(i32, i32, i32, i32)>, behind: Option<HWND>) {
        use windows::Win32::UI::WindowsAndMessaging::{
            IsWindowVisible, SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
            SWP_SHOWWINDOW,
        };
        let Some(target) = client else {
            // SAFETY: a valid window handle.
            unsafe {
                let _ = SetWindowPos(
                    roon,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            return;
        };
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
            target.0 - left,
            target.1 - top,
            target.2 + left + right,
            target.3 + top + bottom,
        );
        // SAFETY: valid window handles.
        let visible = unsafe { IsWindowVisible(roon).as_bool() };
        if want == rect && visible {
            return;
        }
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
        if behind.is_none() {
            flags |= SWP_NOZORDER;
        }
        // SAFETY: valid window handles.
        unsafe {
            let _ = SetWindowPos(roon, behind, want.0, want.1, want.2, want.3, flags);
        }
    }

    /// Keeps Roon directly behind the panel. If Roon has come out on top
    /// (it was clicked, which activates it), the panel goes back above it
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

    /// Cuts `hole` (x, y, width, height in window coordinates, then the
    /// window's width and height) out of the window.
    fn cut_hole(hwnd: HWND, (x, y, w, h, width, height): (i32, i32, i32, i32, i32, i32)) {
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

    /// Roon's main window: the biggest visible, titled, unowned window that
    /// belongs to a process whose program is Roon.exe.
    fn find_roon() -> Option<HWND> {
        use windows::core::BOOL;
        use windows::Win32::Foundation::LPARAM;
        use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

        unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the Vec we passed below, alive for the call.
            let found = unsafe { &mut *(lparam.0 as *mut Vec<(HWND, i64)>) };
            if is_roon_main_window(hwnd) {
                if let Some((_, _, w, h)) = window_rect(hwnd) {
                    found.push((hwnd, w as i64 * h as i64));
                }
            }
            BOOL(1)
        }

        let mut found: Vec<(HWND, i64)> = Vec::new();
        // SAFETY: the callback only reads the Vec through the pointer while
        // EnumWindows runs.
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
        }
        found.into_iter().max_by_key(|(_, area)| *area).map(|(hwnd, _)| hwnd)
    }

    fn is_roon_main_window(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
        };
        // SAFETY: plain window queries.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
                return false;
            }
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
                return false;
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

    /// Takes Roon's window into the panel: remembers where it was and its
    /// style, un-maximizes it, and makes it a tool window (no taskbar
    /// button or Alt-Tab entry). The style only takes effect on the taskbar
    /// once the window has been hidden and shown again; `sync` shows it.
    fn adopt(roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        };
        ROON_HWND.store(roon.0 as isize, Ordering::Relaxed);
        *ROON_RESTORE.lock().unwrap() = window_rect(roon);
        restore_if_maximized(roon);
        // SAFETY: style queries and changes on a valid window handle.
        unsafe {
            let exstyle = GetWindowLongPtrW(roon, GWL_EXSTYLE);
            if exstyle != 0 && ROON_EXSTYLE.load(Ordering::Relaxed) == 0 {
                ROON_EXSTYLE.store(exstyle, Ordering::Relaxed);
            }
            let tool = (exstyle | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            if tool != exstyle {
                hide(roon);
                SetWindowLongPtrW(roon, GWL_EXSTYLE, tool);
            }
        }
    }

    /// Gives Roon its own taskbar button back.
    fn release(roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowLongPtrW, GWL_EXSTYLE};
        let exstyle = ROON_EXSTYLE.swap(0, Ordering::Relaxed);
        if exstyle == 0 {
            return;
        }
        // SAFETY: a valid window handle; the style is the one we read earlier.
        unsafe {
            hide(roon);
            SetWindowLongPtrW(roon, GWL_EXSTYLE, exstyle);
        }
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
        use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, ShowWindow, SW_HIDE};
        // SAFETY: a valid window handle.
        unsafe {
            if IsWindowVisible(hwnd).as_bool() {
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

    /// Puts the panel back above Roon the moment Roon becomes the active
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
        if !ATTACHED.load(Ordering::Relaxed) {
            return;
        }
        if let (Some(host), Some(roon)) = (hwnd_of(&HOST_HWND), hwnd_of(&ROON_HWND)) {
            if window == roon {
                keep_order(host, roon);
            }
        }
    }
}
