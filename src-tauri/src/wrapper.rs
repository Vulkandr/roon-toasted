// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Wrapper ("Roon: Wrapped", experimental, off by default): the real
//! Roon desktop app shown inside a Toasted window, with Toasted's player
//! along the bottom.
//!
//! The "wrapper" window (page wrapper.html) has a hole in it: the page
//! reports where its empty middle is, and this module cuts that rectangle
//! out of the window with `SetWindowRgn`. Anything the page draws over the
//! hole (the zone menu, the queue panel) is reported too and left in, so it
//! shows on top of Roon. Windows then neither draws the
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
//! The window has no Windows frame or shadow (a window with a region loses
//! both, and the invisible frame would show as a white edge). Its rounded
//! corners are drawn by the page instead (the window is transparent and
//! the page's rounded panel fills it), which keeps them smooth; a rounded
//! region would be cut pixel by pixel and look stepped.
//! The page leaves a few pixels of its own along the sides of the hole,
//! where Tauri's resize borders are, so the Wrapper can still be resized
//! from every edge.
//!
//! While the Wrapper is being resized, Roon is hidden and the hole closed
//! (our background shows), so Roon neither repaints at every step nor
//! shows outside the shrinking Wrapper; once the resizing stops, Roon is
//! fitted to the new size, given a moment to repaint, and the hole opens
//! again.
//!
//! Roon has a minimum window size of its own. When it's bigger than the
//! hole, the Wrapper's minimum size grows by the difference (and the
//! Wrapper grows if it's smaller), so Roon never hangs out below or beside
//! it.
//!
//! Roon's window loses its resize borders (WS_THICKFRAME) while it's in
//! the Wrapper. Otherwise its invisible borders, a few pixels outside what
//! it draws, poke out past the Wrapper's edges, and grabbing there would
//! resize Roon instead of the Wrapper. They come back with "Return Roon".
//!
//! Roon's minimum size is measured when it's taken in (asked to be 1 x 1
//! while hidden, it stops at its minimum), so the Wrapper's own minimum is
//! big enough from the start, also in the middle of a resize.
//!
//! While Open or Parked, Roon's window is a tool window: no taskbar button
//! or Alt-Tab entry of its own (the Wrapper is the one you switch to).
//! "Return Roon" (and quitting Toasted) returns to Off: Roon gets its own
//! window, taskbar button and old position back. While the Wrapper is still
//! switched on, an "Enable Wrapper" button then sits in Roon's title bar to
//! wrap it again (wrapper_button.rs).
//!
//! If Toasted stops without putting Roon back (a crash, a dev rebuild, an
//! update), Roon is left hidden; the next start finds it (it still has the
//! tool-window style we gave it) and parks it again, or gives it back if
//! the Wrapper has since been switched off.

use serde::Serialize;
use tauri::AppHandle;

use crate::settings;

/// Where Roon shows through the Wrapper right now (its window, and the
/// hole as x, y, width, height in real screen pixels), or None when it
/// doesn't (closed, parked, minimized, resizing). For skin.rs.
#[cfg(windows)]
pub(crate) fn visible_hole() -> Option<(windows::Win32::Foundation::HWND, [i32; 4])> {
    windows_wrapper::visible_hole()
}

/// The Wrapper window's label, as set in tauri.conf.json.
pub(crate) const WRAPPER_WINDOW: &str = "wrapper";

/// The "Enable Wrapper" button's window (in Roon's title bar, see
/// wrapper_button.rs), as set in tauri.conf.json.
pub(crate) const BUTTON_WINDOW: &str = "wrapper-button";

/// Whether the Wrapper is being moved right now (dragged, or within a
/// moment of its last move step). The sidebar filter pauses its refreshes
/// meanwhile: each one makes Windows copy part of the screen, which held
/// the drag to about 60 fps.
#[cfg(windows)]
pub(crate) fn moving() -> bool {
    windows_wrapper::moving()
}

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
/// the page (which fills the window's client area), whenever it changes;
/// `covers` are the page's own panels and menus over the hole ([x, y,
/// width, height] each), which stay part of the window.
#[tauri::command]
pub fn wrapper_hole(
    app: AppHandle,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    covers: Option<Vec<[f64; 4]>>,
) {
    #[cfg(windows)]
    windows_wrapper::set_hole(&app, [x, y, width, height], covers.unwrap_or_default());
    #[cfg(not(windows))]
    let _ = (app, x, y, width, height, covers);
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

/// The page's "Return Roon" button.
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

/// Gives Roon its own window back and hides the Wrapper ("Return Roon",
/// switching the Wrapper off, quitting Toasted).
pub fn put_back(app: &AppHandle) {
    #[cfg(windows)]
    windows_wrapper::put_back(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// The Wrapper is being resized: closes the hole until it stops (see the
/// module notes).
pub fn resized(app: &AppHandle) {
    #[cfg(windows)]
    windows_wrapper::resized(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Lines Roon up with the Wrapper right now (it moved).
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

/// The font families installed on this PC, sorted, for Now Playing's
/// font choices (the page lists its bundled fonts first).
#[tauri::command]
pub fn list_fonts() -> Vec<String> {
    #[cfg(windows)]
    {
        windows_wrapper::installed_fonts()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// After a settings change: switching the Wrapper off puts Roon back.
/// Either way, the "Enable Wrapper" button in Roon's title bar comes or
/// goes with the switch.
pub fn settings_changed(app: &AppHandle, before: &settings::AppSettings, after: &settings::AppSettings) {
    if before.wrapper_enabled && !after.wrapper_enabled {
        put_back(app);
    }
    #[cfg(windows)]
    if before.wrapper_enabled != after.wrapper_enabled {
        button::refresh(app);
    }
}

#[cfg(windows)]
#[path = "wrapper_button.rs"]
mod button;

#[cfg(windows)]
mod windows_wrapper {
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use tauri::{AppHandle, Manager};
    use windows::Win32::Foundation::{HWND, POINT, RECT};

    use super::{WrapperStatus, WRAPPER_WINDOW};

    /// Height of Roon's transport bar (play controls at the bottom of its
    /// window, with the line above it), in logical pixels: Roon's window is
    /// made that much taller than the hole, so the bar sits hidden under our
    /// player bar and Roon's content ends right where ours starts. Measured
    /// on Roon 2.x.
    const ROON_BOTTOM_BAR: f64 = 104.0;

    // Roon's width is always exactly the hole's: Roon collapses its sidebar
    // below about 1400 logical px, so the Wrapper's minimum width in
    // tauri.conf.json (1440) keeps the hole wide enough.


    /// After the last resize step: how long the hole stays closed, then how
    /// long Roon gets to repaint at its new size before the hole reopens.
    const RESIZE_QUIET: Duration = Duration::from_millis(250);
    const RESIZE_REPAINT: Duration = Duration::from_millis(200);

    const OFF: u8 = 0;
    const OPEN: u8 = 1;
    const PARKED: u8 = 2;
    static STATE: AtomicU8 = AtomicU8::new(OFF);

    /// Our window and Roon's window (0 = none).
    static HOST_HWND: AtomicIsize = AtomicIsize::new(0);
    static ROON_HWND: AtomicIsize = AtomicIsize::new(0);
    /// The hole and what the page draws over it, in CSS pixels relative to
    /// the page ([x, y, width, height]).
    struct Hole {
        rect: [f64; 4],
        covers: Vec<[f64; 4]>,
    }
    static HOLE: Mutex<Option<Hole>> = Mutex::new(None);

    /// The region last given to the window (window size, then the hole and
    /// covers in window coordinates), so it's only replaced when it changes.
    static LAST_REGION: Mutex<Option<Vec<i32>>> = Mutex::new(None);
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
    /// When the Wrapper last changed size (None = not since it opened).
    static LAST_RESIZE: Mutex<Option<Instant>> = Mutex::new(None);
    /// When the Wrapper was last dragged by its title bar. While that's
    /// going on, Roon and the sidebar filter are moved along in the
    /// Wrapper's own move step (see `on_host_message`), and the regular
    /// sync passes stay out of the way: each one asks Roon about its window
    /// and may wait on it, which made dragging feel choppy.
    static LAST_MOVE: Mutex<Option<Instant>> = Mutex::new(None);

    /// Where Roon's window sits relative to ours (top-left to top-left,
    /// real pixels), as of the last full sync. A drag puts Roon at the
    /// Wrapper's new spot plus this, rather than nudging Roon from wherever
    /// it seems to be: with moves that don't wait, Roon may not have caught
    /// up with the last step yet, and nudging lost those steps.
    static ROON_OFFSET: Mutex<Option<(i32, i32)>> = Mutex::new(None);

    /// How long after the last move step a drag counts as over.
    const MOVE_QUIET: Duration = Duration::from_millis(200);
    /// Where Roon currently shows through (screen pixels), see
    /// `visible_hole`.
    static VISIBLE_HOLE: Mutex<Option<[i32; 4]>> = Mutex::new(None);

    /// What the page draws over the hole (screen pixels), as of the last
    /// sync pass that showed Roon.
    static VISIBLE_COVERS: Mutex<Vec<[i32; 4]>> = Mutex::new(Vec::new());

    /// While the Wrapper is dragged by its title bar, Roon isn't moved
    /// along at all: the hole is closed, Roon waits just off the screen,
    /// and a live picture of it (a DWM thumbnail, which the
    /// screen composer draws as part of our window) shows in its place. So
    /// the whole Wrapper moves as one window, at the speed of any other
    /// window. When the drag ends Roon is put at the new spot and the hole
    /// opens again. See `start_drag` / `end_drag`.
    static DRAGGING: AtomicBool = AtomicBool::new(false);
    /// The live pictures of Roon (one per piece of the hole not under a
    /// cover), to remove once the hole is open again.
    static THUMBNAILS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
    static APP: OnceLock<AppHandle> = OnceLock::new();

    /// Only once the Wrapper has actually moved: pressing the title bar
    /// starts Windows' move before the mouse moves, and the Wrapper comes
    /// to the front right then, so the filter still has to refresh.
    pub fn moving() -> bool {
        DRAGGING.load(Ordering::Relaxed)
            || LAST_MOVE.lock().unwrap().is_some_and(|at| at.elapsed() < MOVE_QUIET)
    }

    pub fn visible_hole() -> Option<(HWND, [i32; 4])> {
        let hole = (*VISIBLE_HOLE.lock().unwrap())?;
        Some((roon_window()?, hole))
    }

    /// Records where Roon shows through when a sync pass ends, whichever
    /// way it ends (None unless the pass got all the way to showing Roon).
    struct HoleReport(Option<[i32; 4]>);

    impl Drop for HoleReport {
        fn drop(&mut self) {
            *VISIBLE_HOLE.lock().unwrap() = self.0;
        }
    }

    /// The Wrapper's minimum size (logical pixels) once Roon has turned out
    /// to need more than tauri.conf.json's.
    static LEARNED_MIN: Mutex<Option<(f64, f64)>> = Mutex::new(None);
    /// How much bigger than asked Roon was on the last pass; only acted on
    /// when two passes in a row agree, so a Roon still busy resizing
    /// doesn't count.
    static ROON_EXTRA: Mutex<Option<(i32, i32)>> = Mutex::new(None);
    /// Roon's window style before we took its resize borders (0 = not
    /// changed).
    static ROON_STYLE: AtomicIsize = AtomicIsize::new(0);
    /// The smallest client area Roon allows, measured when it's taken in,
    /// in logical pixels: in real pixels it depends on the screen's scaling,
    /// so it's worked out again for whichever screen the Wrapper is on.
    static ROON_MIN_CLIENT: Mutex<Option<(f64, f64)>> = Mutex::new(None);

    /// Every font family Windows knows, once each, sorted; the "@" ones
    /// (vertical versions of East Asian fonts) left out.
    pub fn installed_fonts() -> Vec<String> {
        use std::collections::BTreeSet;
        use windows::Win32::Foundation::LPARAM;
        use windows::Win32::Graphics::Gdi::{
            EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW, TEXTMETRICW,
        };

        unsafe extern "system" fn each(font: *const LOGFONTW, _: *const TEXTMETRICW, _: u32, lparam: LPARAM) -> i32 {
            // SAFETY: GDI passes a valid LOGFONTW; lparam is our set.
            unsafe {
                let name = &(*font).lfFaceName;
                let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                let name = String::from_utf16_lossy(&name[..len]);
                if !name.starts_with('@') && !name.is_empty() {
                    (*(lparam.0 as *mut BTreeSet<String>)).insert(name);
                }
            }
            1
        }

        let mut names = BTreeSet::new();
        let wanted = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
        // SAFETY: the screen DC, released after; the set outlives the call.
        unsafe {
            let dc = GetDC(None);
            EnumFontFamiliesExW(dc, &wanted, Some(each), LPARAM(&mut names as *mut _ as isize), 0);
            ReleaseDC(None, dc);
        }
        names.into_iter().collect()
    }

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

    /// The Wrapper isn't holding Roon (Roon has its own window, or isn't
    /// running).
    pub(super) fn is_off() -> bool {
        STATE.load(Ordering::Relaxed) == OFF
    }

    pub fn status(_app: &AppHandle) -> WrapperStatus {
        WrapperStatus {
            open: STATE.load(Ordering::Relaxed) == OPEN,
            roon_found: roon_window().is_some(),
            note: NOTE.lock().unwrap().clone(),
        }
    }

    pub fn start(app: &AppHandle) {
        let _ = APP.set(app.clone());
        remember_host(app);
        watch_foreground();
        follow_moves();
        // A Roon of its own while the Wrapper is on gets the title-bar button
        super::button::refresh(app);

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
                let hwnd = HWND(hwnd.0);
                let first = HOST_HWND.swap(hwnd.0 as isize, Ordering::Relaxed) != hwnd.0 as isize;
                if first {
                    let dpi = (window.scale_factor().unwrap_or(1.0) * 96.0).round() as u32;
                    set_icons(hwnd, dpi);
                }
            }
        }
    }

    /// The Wrapper's own icon, in the sizes Windows uses (see icon.rs)
    use crate::icon::WRAPPED_ICONS as ICONS;
    /// The icons we gave the window (0 = none yet), to free when replaced.
    static SMALL_ICON: AtomicIsize = AtomicIsize::new(0);
    static BIG_ICON: AtomicIsize = AtomicIsize::new(0);

    /// Gives the Wrapper's window its icons for a display scaling (`dpi`,
    /// 96 = 100%): small (16 px at 100%) and big (32 px at 100%; the
    /// taskbar and Alt+Tab), each made from the closest size that's at
    /// least as big. Tauri would give one picture for every size, which
    /// Windows shrinks and blurs.
    fn set_icons(hwnd: HWND, dpi: u32) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateIconFromResourceEx, DestroyIcon, SendMessageW, HICON, ICON_BIG, ICON_SMALL,
            LR_DEFAULTCOLOR, WM_SETICON,
        };
        let dpi = dpi.clamp(96, 96 * 4) as i32;
        for (kind, base, cell) in [(ICON_SMALL, 16, &SMALL_ICON), (ICON_BIG, 32, &BIG_ICON)] {
            let size = (base * dpi + 48) / 96;
            let Some(&(_, png)) = ICONS.iter().find(|(s, _)| *s >= size).or(ICONS.last()) else {
                continue;
            };
            // SAFETY: a PNG icon image (supported since Vista) at the
            // size wanted; the window keeps using it until it's replaced.
            unsafe {
                let Ok(icon) = CreateIconFromResourceEx(png, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR) else {
                    continue;
                };
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
                let old = cell.swap(icon.0 as isize, Ordering::Relaxed);
                if old != 0 {
                    let _ = DestroyIcon(HICON(old as *mut core::ffi::c_void));
                }
            }
        }
    }

    pub fn resized(app: &AppHandle) {
        *LAST_RESIZE.lock().unwrap() = Some(Instant::now());
        sync(app);
    }

    pub fn set_hole(app: &AppHandle, rect: [f64; 4], covers: Vec<[f64; 4]>) {
        *HOLE.lock().unwrap() = Some(Hole { rect, covers });
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
        super::button::refresh(app);
        *LAST_REGION.lock().unwrap() = None;
        // A remembered size still has to fit the screen it opens on
        crate::fit_to_screen(app, WRAPPER_WINDOW, false);
        // fit_to_screen sets the configured minimum; Roon may need more
        // (but never more than this screen has room for)
        let learned = *LEARNED_MIN.lock().unwrap();
        if let Some(min) = learned {
            let (w, h) = within_screen(&window, min);
            let _ = window.set_min_size(Some(tauri::Size::Logical(tauri::LogicalSize::new(w, h))));
        }
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
        super::button::refresh(app);
        *VISIBLE_HOLE.lock().unwrap() = None;
        DRAGGING.store(false, Ordering::Relaxed);
        drop_thumbnails();
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
        *VISIBLE_HOLE.lock().unwrap() = None;
        DRAGGING.store(false, Ordering::Relaxed);
        drop_thumbnails();
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
        // Roon's title bar gets "Enable Wrapper" (if the Wrapper is still on)
        super::button::refresh(app);
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
        *VISIBLE_HOLE.lock().unwrap() = None;
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
        let mut report = HoleReport(None);
        // Being dragged: everything is already moving along (see LAST_MOVE
        // and DRAGGING)
        if DRAGGING.load(Ordering::Relaxed)
            || LAST_MOVE.lock().unwrap().is_some_and(|at| at.elapsed() < MOVE_QUIET)
        {
            // Leave VISIBLE_HOLE as the move steps have it (writing back what
            // was read could undo a step made in between)
            std::mem::forget(report);
            return;
        }
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

        // The hole and covers, in real pixels: on the screen, and relative
        // to our window (for the region)
        let scale = window.scale_factor().unwrap_or(1.0);
        let (Some(client_origin), Some(host_rect)) = (client_origin(host), window_rect(host)) else {
            return;
        };
        let to_window = |r: &[f64; 4]| {
            [
                client_origin.x - host_rect.0 + (r[0] * scale).round() as i32,
                client_origin.y - host_rect.1 + (r[1] * scale).round() as i32,
                (r[2] * scale).round() as i32,
                (r[3] * scale).round() as i32,
            ]
        };
        let (hole, covers) = {
            let guard = HOLE.lock().unwrap();
            let Some(saved) = guard.as_ref() else {
                return;
            };
            (to_window(&saved.rect), saved.covers.iter().map(to_window).collect::<Vec<_>>())
        };
        let hole_on_screen = (host_rect.0 + hole[0], host_rect.1 + hole[1], hole[2], hole[3]);

        // Being resized: keep the hole closed until it stops, then fit Roon
        // and give it a moment to repaint before opening the hole again
        let since_resize = LAST_RESIZE.lock().unwrap().map(|at| at.elapsed());
        let resizing = since_resize.is_some_and(|t| t < RESIZE_QUIET);
        let repainting = since_resize.is_some_and(|t| t < RESIZE_QUIET + RESIZE_REPAINT);

        // The window's shape (only replaced when something changed): rounded
        // corners, minus the hole when Roon is there and not mid-resize. No
        // Roon yet means no hole, so the page's "Starting Roon" note shows.
        let maximized = is_maximized(host);
        let open_hole = roon.is_some() && !repainting && hole[2] > 0 && hole[3] > 0;
        // Only the client area: a maximized window reaches past the screen's
        // edges, and that strip would show as a white line
        let Some((client_w, client_h)) = client_size(host) else {
            return;
        };
        let bounds = [
            client_origin.x - host_rect.0,
            client_origin.y - host_rect.1,
            client_w,
            client_h,
        ];
        let mut region_key = bounds.to_vec();
        region_key.push(open_hole as i32);
        if open_hole {
            region_key.extend_from_slice(&hole);
            for cover in &covers {
                region_key.extend_from_slice(cover);
            }
        }
        {
            let mut last = LAST_REGION.lock().unwrap();
            if last.as_ref() != Some(&region_key) {
                let hole = open_hole.then_some(hole);
                set_region(host, bounds, hole, &covers);
                *last = Some(region_key);
            }
        }
        // A drag just ended: the hole is back, so the live pictures that
        // stood in for Roon can go (after the next screen refresh, so the
        // hole is showing Roon by then)
        if !THUMBNAILS.lock().unwrap().is_empty() {
            use windows::Win32::Graphics::Dwm::DwmFlush;
            // SAFETY: no arguments.
            let _ = unsafe { DwmFlush() };
            drop_thumbnails();
        }
        let hole = hole_on_screen;

        let Some(roon) = roon else {
            return;
        };
        let bar = (ROON_BOTTOM_BAR * scale).round() as i32;

        // The hole must fit Roon's minimum: checked on every pass, so even
        // in the middle of a resize the Wrapper can't get smaller than that
        if !maximized {
            if let Some((min_w, min_h)) = *ROON_MIN_CLIENT.lock().unwrap() {
                // In this screen's real pixels
                let (min_w, min_h) = ((min_w * scale).ceil() as i32, (min_h * scale).ceil() as i32);
                let extra = ((min_w - hole.2).max(0), (min_h - (hole.3 + bar)).max(0));
                if extra.0 > 0 || extra.1 > 0 {
                    grow_minimum(&window, scale, extra.0, extra.1);
                }
            }
        }

        // Being resized: Roon waits hidden until it stops
        if resizing {
            hide(roon);
            return;
        }

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
        let client = (hole.0, hole.1, hole.2, hole.3 + bar);
        // Roon goes right below us (the sidebar filter sits right above
        // us, see skin.rs)
        place_roon(roon, client, host);
        keep_order(host, roon);
        if let (Some(r), Some(h)) = (window_rect(roon), window_rect(host)) {
            *ROON_OFFSET.lock().unwrap() = Some((r.0 - h.0, r.1 - h.1));
        }
        if open_hole {
            let hole = [hole.0, hole.1, hole.2, hole.3];
            let covers: Vec<[i32; 4]> = covers
                .iter()
                .map(|c| [host_rect.0 + c[0], host_rect.1 + c[1], c[2], c[3]])
                .collect();
            // Nothing of Roon shows when the covers take the whole hole
            // (the page's Now Playing view), and the sidebar filter should
            // know that
            if !subtract(hole, &covers).is_empty() {
                report.0 = Some(hole);
            }
            *VISIBLE_COVERS.lock().unwrap() = covers;
        }

        // Roon still wouldn't get that small (its minimum wasn't measured,
        // or changed): make the Wrapper's minimum bigger by the difference
        if !maximized {
            if let Some((roon_w, roon_h)) = client_size(roon) {
                let extra = ((roon_w - client.2).max(0), (roon_h - client.3).max(0));
                let too_big = extra.0 > 1 || extra.1 > 1;
                let before = std::mem::replace(&mut *ROON_EXTRA.lock().unwrap(), too_big.then_some(extra));
                if too_big && before == Some(extra) {
                    grow_minimum(&window, scale, extra.0, extra.1);
                }
            }
        }
    }

    /// A minimum size (logical pixels) cut down to what the Wrapper's
    /// screen has room for. A minimum bigger than the screen made Windows
    /// push the Wrapper partly onto the next screen, where the scaling (and
    /// so the minimum) changed, back and forth until the app locked up.
    fn within_screen(window: &tauri::WebviewWindow, (w, h): (f64, f64)) -> (f64, f64) {
        let room = window.current_monitor().ok().flatten().map(|monitor| {
            let area = monitor.work_area();
            let scale = monitor.scale_factor();
            (
                (area.size.width as f64 / scale).floor(),
                (area.size.height as f64 / scale).floor(),
            )
        });
        match room {
            Some((room_w, room_h)) => (w.min(room_w), h.min(room_h)),
            None => (w, h),
        }
    }

    /// Raises the Wrapper's minimum size so its hole fits Roon's minimum
    /// (`extra_w` / `extra_h` real pixels more than now), growing the
    /// Wrapper if it's smaller than that. Never past what the screen has
    /// room for (`within_screen`): then Roon is a little bigger than the
    /// hole instead.
    fn grow_minimum(window: &tauri::WebviewWindow, scale: f64, extra_w: i32, extra_h: i32) {
        use tauri::{LogicalSize, Size};
        let Ok(inner) = window.inner_size() else {
            return;
        };
        let need = (
            (inner.width as f64 + extra_w as f64) / scale,
            (inner.height as f64 + extra_h as f64) / scale,
        );
        let configured = window
            .app_handle()
            .config()
            .app
            .windows
            .iter()
            .find(|w| w.label == WRAPPER_WINDOW)
            .map(|w| (w.min_width.unwrap_or(0.0), w.min_height.unwrap_or(0.0)))
            .unwrap_or_default();
        let min = {
            let mut learned = LEARNED_MIN.lock().unwrap();
            let current = learned.unwrap_or(configured);
            let min = (current.0.max(need.0).ceil(), current.1.max(need.1).ceil());
            let min = within_screen(window, min);
            if Some(min) == *learned {
                return;
            }
            *learned = Some(min);
            min
        };
        let _ = window.set_min_size(Some(Size::Logical(LogicalSize::new(min.0, min.1))));
        let now = (inner.width as f64 / scale, inner.height as f64 / scale);
        if now.0 < min.0 || now.1 < min.1 {
            let _ = window.set_size(LogicalSize::new(now.0.max(min.0), now.1.max(min.1)));
        }
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
        if want != rect || !is_visible(roon) {
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

    /// Keeps Roon directly behind the Wrapper (and the sidebar filter
    /// directly in front of it when it's showing, see skin.rs). If Roon has come out on top
    /// (it was clicked, which activates it), the Wrapper goes back above it
    /// without taking focus, so typing still goes to Roon.
    fn keep_order(host: HWND, roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindow, SetWindowPos, GW_HWNDNEXT, GW_HWNDPREV, HWND_TOP, SWP_ASYNCWINDOWPOS,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
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
            // The filter right above us (it's on another thread: never wait
            // on it). It recolors whatever is under it: Roon through the
            // hole, or Roon's live picture while we're dragged.
            if let Some(filter) = crate::skin::filter_window() {
                let above = GetWindow(host, GW_HWNDPREV).ok().filter(|w| !w.is_invalid());
                if above != Some(filter) {
                    let _ = SetWindowPos(
                        filter,
                        Some(above.unwrap_or(HWND_TOP)),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                    );
                }
            }
            // Roon right below us
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

    /// Gives the window its shape: `bounds` (its client area) minus the hole
    /// if there is one, except where the covers are (all in window
    /// coordinates: [x, y, width, height]).
    fn set_region(
        hwnd: HWND,
        bounds: [i32; 4],
        hole: Option<[i32; 4]>,
        covers: &[[i32; 4]],
    ) {
        use windows::Win32::Graphics::Gdi::{
            CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, RGN_DIFF,
        };
        // SAFETY: GDI calls on regions we just made; the window takes
        // ownership of the final region in SetWindowRgn.
        unsafe {
            let [x, y, w, h] = bounds;
            let whole = CreateRectRgn(x, y, x + w, y + h);
            if let Some(hole) = hole {
                let cut = CreateRectRgn(hole[0], hole[1], hole[0] + hole[2], hole[1] + hole[3]);
                for c in covers {
                    let cover = CreateRectRgn(c[0], c[1], c[0] + c[2], c[1] + c[3]);
                    let _ = CombineRgn(Some(cut), Some(cut), Some(cover), RGN_DIFF);
                    let _ = DeleteObject(cover.into());
                }
                let _ = CombineRgn(Some(whole), Some(whole), Some(cut), RGN_DIFF);
                let _ = DeleteObject(cut.into());
            }
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
    pub(super) fn find_roon(include_ours: bool) -> Option<HWND> {
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

    pub(super) fn is_roon_main_window(hwnd: HWND) -> bool {
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
        hide(roon);
        measure_minimum(roon);
        remove_resize_borders(roon);
        // SAFETY: style queries and changes on a valid window handle.
        unsafe {
            let exstyle = GetWindowLongPtrW(roon, GWL_EXSTYLE);
            if !ours && exstyle != 0 && ROON_EXSTYLE.load(Ordering::Relaxed) == 0 {
                ROON_EXSTYLE.store(exstyle, Ordering::Relaxed);
            }
            let tool = (exstyle | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            if tool != exstyle {
                SetWindowLongPtrW(roon, GWL_EXSTYLE, tool);
            }
        }
    }

    /// Finds the smallest client area Roon allows: asked to be 1 x 1 (it's
    /// hidden), it stops at its minimum. Only while it still has its resize
    /// borders, which is when Windows applies its minimum.
    fn measure_minimum(roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowPos, GWL_STYLE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
            WS_THICKFRAME,
        };
        // SAFETY: a style query and a resize of a valid, hidden window.
        unsafe {
            if GetWindowLongPtrW(roon, GWL_STYLE) as u32 & WS_THICKFRAME.0 == 0 {
                return;
            }
            let _ = SetWindowPos(roon, None, 0, 0, 1, 1, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        if let Some((w, h)) = client_size(roon) {
            if w > 50 && h > 50 {
                // The scaling of the screen Roon is on (the Wrapper's, if
                // Roon is somewhere off screen)
                let app = APP.get();
                let scale = app
                    .zip(window_rect(roon))
                    .and_then(|(app, (x, y, _, _))| app.monitor_from_point(x as f64 + 1.0, y as f64 + 1.0).ok().flatten())
                    .map(|monitor| monitor.scale_factor())
                    .or_else(|| app?.get_webview_window(WRAPPER_WINDOW)?.scale_factor().ok())
                    .unwrap_or(1.0);
                *ROON_MIN_CLIENT.lock().unwrap() = Some((w as f64 / scale, h as f64 / scale));
            }
        }
    }

    /// Takes away Roon's resize borders (remembering its style for later).
    fn remove_resize_borders(roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_STYLE, WS_THICKFRAME};
        // SAFETY: style queries and changes on a valid window handle.
        unsafe {
            let style = GetWindowLongPtrW(roon, GWL_STYLE);
            let without = style & !(WS_THICKFRAME.0 as isize);
            if without != style {
                if ROON_STYLE.load(Ordering::Relaxed) == 0 {
                    ROON_STYLE.store(style, Ordering::Relaxed);
                }
                SetWindowLongPtrW(roon, GWL_STYLE, without);
                frame_changed(roon);
            }
        }
    }

    /// Tells Windows a window's frame style changed, so it's redrawn and
    /// its borders recalculated.
    fn frame_changed(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
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
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Gives Roon its own taskbar button back (the style it had before we
    /// took it; for one parked by an earlier run, just without the
    /// tool-window style). Leaves it hidden; the caller shows it.
    fn release(roon: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, WS_THICKFRAME};
        let saved = ROON_EXSTYLE.swap(0, Ordering::Relaxed);
        let saved_style = ROON_STYLE.swap(0, Ordering::Relaxed);
        // SAFETY: style queries and changes on a valid window handle.
        unsafe {
            // Resize borders back (for one parked by an earlier run, just
            // the borders on top of what it has now)
            let style = GetWindowLongPtrW(roon, GWL_STYLE);
            let original_style = if saved_style != 0 {
                saved_style
            } else {
                style | WS_THICKFRAME.0 as isize
            };
            if original_style != style {
                hide(roon);
                SetWindowLongPtrW(roon, GWL_STYLE, original_style);
                frame_changed(roon);
            }
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

    pub(super) fn is_window(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IsWindow;
        // SAFETY: any handle value is fine to ask about.
        unsafe { IsWindow(Some(hwnd)).as_bool() }
    }

    pub(super) fn is_visible(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
        // SAFETY: any handle value is fine to ask about.
        unsafe { IsWindowVisible(hwnd).as_bool() }
    }

    fn is_maximized(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IsZoomed;
        // SAFETY: a valid window handle.
        unsafe { IsZoomed(hwnd).as_bool() }
    }

    pub(super) fn is_minimized(hwnd: HWND) -> bool {
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
    pub(super) fn window_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
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

    /// Moves Roon along with the Wrapper while it's dragged by its title
    /// bar: Windows asks the Wrapper's window before each step of a move
    /// (WM_WINDOWPOSCHANGING), and Roon is sent to the same spot right then,
    /// without waiting for Roon to finish (SWP_ASYNCWINDOWPOS), instead of
    /// after the Wrapper has already moved. Resizes are left to `resized`.
    /// Set up on the main thread, which owns the window.
    fn follow_moves() {
        use windows::Win32::UI::Shell::SetWindowSubclass;
        let Some(host) = hwnd_of(&HOST_HWND) else {
            return;
        };
        // SAFETY: our own window, on its own thread; the callback lives for
        // the whole program.
        unsafe {
            let _ = SetWindowSubclass(host, Some(on_host_message), 0x5772_6170, 0);
        }
    }

    unsafe extern "system" fn on_host_message(
        hwnd: HWND,
        msg: u32,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: windows::Win32::Foundation::LPARAM,
        _id: usize,
        _data: usize,
    ) -> windows::Win32::Foundation::LRESULT {
        use windows::Win32::UI::Shell::DefSubclassProc;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, WINDOWPOS, WM_EXITSIZEMOVE, WM_MOVING, WM_WINDOWPOSCHANGING,
        };
        // Moved to a screen with different scaling: icons for that size
        if msg == windows::Win32::UI::WindowsAndMessaging::WM_DPICHANGED {
            set_icons(hwnd, (wparam.0 & 0xFFFF) as u32);
        }
        if STATE.load(Ordering::Relaxed) == OPEN {
            // Dragged by the title bar: the first step of the drag swaps
            // Roon for its live picture, letting go swaps it back
            if msg == WM_MOVING && !DRAGGING.load(Ordering::Relaxed) {
                start_drag(hwnd);
            } else if msg == WM_EXITSIZEMOVE && DRAGGING.load(Ordering::Relaxed) {
                end_drag(hwnd);
            }
        }
        if msg == WM_WINDOWPOSCHANGING && STATE.load(Ordering::Relaxed) == OPEN {
            // SAFETY: for this message lparam points at a WINDOWPOS.
            let pos = unsafe { &*(lparam.0 as *const WINDOWPOS) };
            let moving = !pos.flags.contains(SWP_NOMOVE);
            if moving {
                if let (Some(now), Some(roon)) = (window_rect(hwnd), roon_window()) {
                    let same_size = pos.flags.contains(SWP_NOSIZE) || (pos.cx == now.2 && pos.cy == now.3);
                    let (dx, dy) = (pos.x - now.0, pos.y - now.1);
                    if same_size && (dx != 0 || dy != 0) {
                        *LAST_MOVE.lock().unwrap() = Some(Instant::now());
                        // Where Roon shows through moves too (the next sync
                        // pass will agree); the sidebar filter follows it
                        let hole = VISIBLE_HOLE.lock().unwrap().as_mut().map(|hole| {
                            hole[0] += dx;
                            hole[1] += dy;
                            *hole
                        });
                        if let Some(hole) = hole {
                            crate::skin::follow(hole[0], hole[1]);
                        }
                    }
                    let dragging = DRAGGING.load(Ordering::Relaxed);
                    if same_size && (dx != 0 || dy != 0) && is_visible(roon) && !dragging {
                        // Where Roon belongs next to the Wrapper's new spot
                        let offset = *ROON_OFFSET.lock().unwrap();
                        let target = match offset {
                            Some((ox, oy)) => Some((pos.x + ox, pos.y + oy)),
                            None => window_rect(roon).map(|r| (r.0 + dx, r.1 + dy)),
                        };
                        if let Some((x, y)) = target {
                            // SAFETY: a valid window handle.
                            unsafe {
                                let _ = SetWindowPos(
                                    roon,
                                    None,
                                    x,
                                    y,
                                    0,
                                    0,
                                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                                );
                            }
                        }
                    }
                }
            }
        }
        // The smallest size Windows lets the Wrapper be: whatever Tauri
        // says, but never bigger than the screen it's on. A minimum wider
        // than the screen (dragged from a big screen onto a small one, a
        // portrait one, say) left it hanging onto the next screen; with
        // different scaling there, each screen's resize pushed it back to
        // the other, endlessly, and the app locked up.
        if msg == windows::Win32::UI::WindowsAndMessaging::WM_GETMINMAXINFO {
            // SAFETY: Tauri fills in the sizes first, as every subclass must
            // pass messages on.
            let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            if let Some((room_w, room_h)) = screen_room(hwnd) {
                // SAFETY: for this message lparam points at a MINMAXINFO.
                let info = unsafe { &mut *(lparam.0 as *mut windows::Win32::UI::WindowsAndMessaging::MINMAXINFO) };
                info.ptMinTrackSize.x = info.ptMinTrackSize.x.min(room_w);
                info.ptMinTrackSize.y = info.ptMinTrackSize.y.min(room_h);
            }
            return result;
        }
        // SAFETY: passing the message on, as every subclass must.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// The size of the work area (the screen minus the taskbar, real
    /// pixels) of the screen a window is mostly on.
    fn screen_room(hwnd: HWND) -> Option<(i32, i32)> {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
        // SAFETY: a valid window handle, and an out-struct with its size set.
        unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            let work = info.rcWork;
            Some((work.right - work.left, work.bottom - work.top))
        }
    }

    /// The first step of a drag by the title bar: shows a live picture of
    /// Roon where the hole is (one per piece of it not under a cover, so the
    /// page's panels stay on top), closes the hole, and moves Roon just off
    /// the screen, where it waits out the drag. Only when Roon is showing
    /// through the hole right now.
    fn start_drag(host: HWND) {
        use windows::Win32::Foundation::TRUE;
        use windows::Win32::Graphics::Dwm::{
            DwmRegisterThumbnail, DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES,
            DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE,
            DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        };
        let Some(hole) = *VISIBLE_HOLE.lock().unwrap() else {
            return;
        };
        let Some(roon) = roon_window().filter(|r| is_visible(*r)) else {
            return;
        };
        let (Some(origin), Some(host_rect), Some((client_w, client_h))) =
            (client_origin(host), window_rect(host), client_size(host))
        else {
            return;
        };
        // Somewhere off the screen for Roon to wait (none: no live picture,
        // Roon just follows the drag as before)
        let Some(away) = off_screen_spot(roon) else {
            return;
        };
        let covers = VISIBLE_COVERS.lock().unwrap().clone();
        let pieces = subtract(hole, &covers);

        let mut thumbnails = Vec::new();
        for piece in pieces.iter().take(16) {
            // SAFETY: two valid top-level windows; the properties live for
            // the call.
            unsafe {
                let Ok(thumbnail) = DwmRegisterThumbnail(host, roon) else {
                    continue;
                };
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION
                        | DWM_TNP_RECTSOURCE
                        | DWM_TNP_OPACITY
                        | DWM_TNP_VISIBLE
                        | DWM_TNP_SOURCECLIENTAREAONLY,
                    // Where on our window (from its client area's corner)
                    rcDestination: RECT {
                        left: piece[0] - origin.x,
                        top: piece[1] - origin.y,
                        right: piece[0] + piece[2] - origin.x,
                        bottom: piece[1] + piece[3] - origin.y,
                    },
                    // The same piece of Roon (its client area starts at the
                    // hole's corner)
                    rcSource: RECT {
                        left: piece[0] - hole[0],
                        top: piece[1] - hole[1],
                        right: piece[0] + piece[2] - hole[0],
                        bottom: piece[1] + piece[3] - hole[1],
                    },
                    opacity: 255,
                    fVisible: TRUE,
                    fSourceClientAreaOnly: TRUE,
                };
                let _ = DwmUpdateThumbnailProperties(thumbnail, &props);
                thumbnails.push(thumbnail);
            }
        }
        if thumbnails.is_empty() {
            return;
        }
        THUMBNAILS.lock().unwrap().extend(thumbnails);
        DRAGGING.store(true, Ordering::Relaxed);

        // Close the hole (the next sync pass after the drag opens it again)
        let bounds = [origin.x - host_rect.0, origin.y - host_rect.1, client_w, client_h];
        set_region(host, bounds, None, &[]);
        *LAST_REGION.lock().unwrap() = None;
        // Roon waits off the screen, still showing (so its live picture
        // keeps working), without waiting on it
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(
                roon,
                None,
                away.0,
                away.1,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }

    /// A spot just past an edge of the monitor Roon is on, touching no
    /// monitor, where that monitor is still the nearest one (so Windows
    /// keeps treating Roon as on it and doesn't rescale it for another
    /// monitor's scaling). Returns the window's new top-left corner.
    fn off_screen_spot(roon: HWND) -> Option<(i32, i32)> {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, MONITORINFO,
            MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
        };
        let (x, y, w, h) = window_rect(roon)?;
        // SAFETY: plain monitor queries with valid pointers.
        unsafe {
            let monitor = MonitorFromWindow(roon, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            let m = info.rcMonitor;
            const GAP: i32 = 8;
            let candidates = [
                (x, m.bottom + GAP),
                (x, m.top - h - GAP),
                (m.right + GAP, y),
                (m.left - w - GAP, y),
            ];
            candidates.into_iter().find(|&(cx, cy)| {
                let rect = RECT { left: cx, top: cy, right: cx + w, bottom: cy + h };
                MonitorFromRect(&rect, MONITOR_DEFAULTTONULL).is_invalid()
                    && MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) == monitor
            })
        }
    }

    /// The drag is over: Roon goes to its spot behind the hole, then a sync
    /// pass opens the hole and removes the live pictures.
    fn end_drag(host: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE};
        if let (Some(roon), Some(now), Some((ox, oy))) =
            (roon_window(), window_rect(host), *ROON_OFFSET.lock().unwrap())
        {
            // SAFETY: valid window handles.
            unsafe {
                let _ = SetWindowPos(roon, Some(host), now.0 + ox, now.1 + oy, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        DRAGGING.store(false, Ordering::Relaxed);
        *LAST_MOVE.lock().unwrap() = None;
        if let Some(app) = APP.get() {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { sync(&app) });
        }
    }

    fn drop_thumbnails() {
        use windows::Win32::Graphics::Dwm::DwmUnregisterThumbnail;
        let thumbnails = std::mem::take(&mut *THUMBNAILS.lock().unwrap());
        for thumbnail in thumbnails {
            // SAFETY: a thumbnail we registered.
            unsafe {
                let _ = DwmUnregisterThumbnail(thumbnail);
            }
        }
    }

    /// `rect` minus `cuts`, as rectangles ([x, y, width, height]).
    fn subtract(rect: [i32; 4], cuts: &[[i32; 4]]) -> Vec<[i32; 4]> {
        let mut pieces = vec![rect];
        for c in cuts {
            let mut next = Vec::new();
            for p in pieces {
                let (l, t, r, b) = (p[0], p[1], p[0] + p[2], p[1] + p[3]);
                let (cl, ct, cr, cb) = (c[0].max(l), c[1].max(t), (c[0] + c[2]).min(r), (c[1] + c[3]).min(b));
                if cl >= cr || ct >= cb {
                    next.push(p);
                    continue;
                }
                if ct > t {
                    next.push([l, t, p[2], ct - t]);
                }
                if cb < b {
                    next.push([l, cb, p[2], b - cb]);
                }
                if cl > l {
                    next.push([l, ct, cl - l, cb - ct]);
                }
                if cr < r {
                    next.push([cr, ct, r - cr, cb - ct]);
                }
            }
            pieces = next;
        }
        pieces
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
        super::button::on_foreground(window);
        if STATE.load(Ordering::Relaxed) != OPEN {
            return;
        }
        if let (Some(host), Some(roon)) = (hwnd_of(&HOST_HWND), hwnd_of(&ROON_HWND)) {
            // Roon was clicked, or we were (by the title bar, say, while
            // another window covered part of us: Roon has to come up too)
            if (window == roon || window == host) && is_visible(host) && is_visible(roon) {
                keep_order(host, roon);
            }
        }
    }
}
