// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Roon's sidebar in the Wrapper's colors (experimental, part of the
//! Wrapper).
//!
//! Roon draws its own sidebar and nothing outside Roon can restyle it, so a
//! color filter is laid over it: a window of ours, exactly over Roon's
//! sidebar, holding a Windows magnifier control (the Magnification API, as
//! used by Windows' own Magnifier and its color filters) at 1x zoom. It
//! shows what's on screen beneath it, live, through a color matrix, so
//! Roon's sidebar grey comes out as the Wrapper's background and Roon's
//! purple as the Wrapper's accent, with text and icons keeping their
//! contrast. Roon is never asked to do anything (no copying of its window).
//!
//! The window ignores the mouse completely (WS_EX_TRANSPARENT), so every
//! click, scroll and hover goes straight through to Roon's real sidebar.
//! It's left out of its own picture (the magnifier's filter list) and out
//! of screen captures (display affinity), so it never sees itself. It sits
//! directly above the Wrapper's window, over the sidebar part of the hole,
//! so it recolors Roon through the hole, and Roon's live picture while the
//! Wrapper is dragged (wrapper.rs keeps that order).
//!
//! Threads: the filter's window lives on its own thread, so the main
//! thread must never wait on it (that froze the app while dragging):
//! the main thread only ever moves it with SWP_ASYNCWINDOWPOS, the window
//! isn't owned by the Wrapper (an owner on another thread ties the two
//! threads' input together), and no lock is held during a Windows call.
//!
//! The sidebar's edge and Roon's sidebar grey are found once a second from
//! a capture of that part of the screen (which, thanks to the display
//! affinity, shows Roon underneath, not us): a column of the grey from the
//! left, ending in a clean vertical line into the content's plain
//! background. When Roon has its sidebar collapsed (the hamburger menu),
//! there's no such line and the filter steps aside.
//!
//! It runs on its own thread (so it never slows the Wrapper's own window,
//! e.g. while it's dragged), which owns our window and the magnifier and
//! refreshes the picture about every 8 ms, waking right away when its
//! window is moved (during a drag), so it keeps up with Roon's scrolling
//! and with the Wrapper as closely as possible.

use tauri::AppHandle;

/// The Wrapper page tells us its colors (background and accent, as RGB)
/// whenever they change, e.g. with album colors.
#[tauri::command]
pub fn wrapper_colors(bg: [u8; 3], accent: [u8; 3]) {
    #[cfg(windows)]
    imp::set_colors(bg, accent);
    #[cfg(not(windows))]
    let _ = (bg, accent);
}

/// Creates the filter window (in setup, after the Wrapper's own start).
pub fn start(app: &AppHandle) {
    #[cfg(windows)]
    imp::start(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// The Wrapper is being dragged: moves the filter along right away, to
/// the hole's new top-left corner (screen pixels).
#[cfg(windows)]
pub fn follow(x: i32, y: i32) {
    imp::follow(x, y);
}

/// The filter's window while it's showing (wrapper.rs keeps it directly
/// above the Wrapper).
#[cfg(windows)]
pub fn filter_window() -> Option<windows::Win32::Foundation::HWND> {
    imp::filter_window()
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
    use std::sync::Mutex;

    use tauri::{AppHandle, Manager};
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
    };
    use windows::Win32::UI::Magnification::{
        MagInitialize, MagSetColorEffect, MagSetWindowFilterList, MagSetWindowSource,
        MagSetWindowTransform, MAGCOLOREFFECT, MAGTRANSFORM, MW_FILTERMODE_EXCLUDE, WC_MAGNIFIER,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
    };

    /// Roon's colors in its sidebar (dark theme), as RGB: its text grey and
    /// accent purple. The background grey is measured.
    const ROON_TEXT: [f32; 3] = [168.0, 168.0, 168.0];
    const ROON_ACCENT: [f32; 3] = [106.0, 110.0, 217.0];

    /// Refreshes between looking for the sidebar's edge again (about a
    /// second).
    const DETECT_EVERY: u32 = 120;

    /// Our window and the magnifier inside it (0 = none).
    static HOST: AtomicIsize = AtomicIsize::new(0);
    static MAGNIFIER: AtomicIsize = AtomicIsize::new(0);
    static SHOWN: AtomicBool = AtomicBool::new(false);
    static TICKS: AtomicU32 = AtomicU32::new(0);
    /// Refreshes since the Wrapper started moving, and how many are still
    /// allowed then (see `tick`).
    static MOVING_TICKS: AtomicU32 = AtomicU32::new(0);
    const FRESH_AT_DRAG: u32 = 3;

    /// The Wrapper's background and accent; the Toaster's defaults until
    /// the page says otherwise.
    static COLORS: Mutex<([u8; 3], [u8; 3])> = Mutex::new(([27, 27, 31], [123, 95, 199]));

    /// What's currently set up: where the filter sits (screen pixels),
    /// Roon's sidebar grey, and the colors the matrix was made for.
    struct Placed {
        rect: [i32; 4],
        roon_bg: [u8; 3],
        colors: ([u8; 3], [u8; 3]),
    }
    static PLACED: Mutex<Option<Placed>> = Mutex::new(None);
    /// The last sidebar found: its width and grey (None = not found).
    static SIDEBAR: Mutex<Option<(i32, [u8; 3])>> = Mutex::new(None);

    fn hwnd(cell: &AtomicIsize) -> Option<HWND> {
        let raw = cell.load(Ordering::Relaxed);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    pub fn set_colors(bg: [u8; 3], accent: [u8; 3]) {
        *COLORS.lock().unwrap() = (bg, accent);
    }

    pub fn start(app: &AppHandle) {
        let Some(owner) = app
            .get_webview_window(crate::wrapper::WRAPPER_WINDOW)
            .and_then(|w| w.hwnd().ok())
            .map(|h| HWND(h.0))
        else {
            return;
        };
        let owner = owner.0 as isize;
        let _ = std::thread::Builder::new()
            .name("roon-skin".into())
            .spawn(move || run(HWND(owner as *mut core::ffi::c_void)));
    }

    /// The filter's thread: makes the windows, then refreshes about every
    /// 8 ms, handling its windows' messages in between.
    fn run(owner: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage, MSG,
            PM_REMOVE, QS_ALLINPUT,
        };
        // SAFETY: the magnifier needs initializing on the thread that uses it.
        unsafe {
            if !MagInitialize().as_bool() {
                eprintln!("[skin] the Magnification API isn't available");
                return;
            }
        }
        let Some((host, magnifier)) = create_windows() else {
            eprintln!("[skin] couldn't make the filter window");
            return;
        };
        HOST.store(host.0 as isize, Ordering::Relaxed);
        MAGNIFIER.store(magnifier.0 as isize, Ordering::Relaxed);

        loop {
            let mut msg = MSG::default();
            // SAFETY: a plain message pump for this thread's windows.
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if tick(owner) {
                // Showing: refresh again soon, or right away when there's a
                // message (the Wrapper moving our window during a drag, which
                // used to wait up to a whole screen refresh)
                // SAFETY: waits on this thread's message queue only.
                unsafe {
                    let _ = MsgWaitForMultipleObjects(None, false, 8, QS_ALLINPUT);
                }
            } else {
                // Nothing to show: check back now and then
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;
        // SAFETY: default handling; the window does nothing itself.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// Our window (layered, ignores the mouse, left out of captures) with
    /// the magnifier filling it.
    fn create_windows() -> Option<(HWND, HWND)> {
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, RegisterClassW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, LWA_ALPHA, WDA_EXCLUDEFROMCAPTURE, WNDCLASSW, WS_CHILD,
            WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
            WS_VISIBLE,
        };
        // SAFETY: plain window creation; the class name is static.
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(host_proc),
                hInstance: instance.into(),
                lpszClassName: w!("RoonToastedSkin"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let host = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("RoonToastedSkin"),
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .ok()?;
            // Fully opaque (a layered window needs its alpha set to show)
            let _ = SetLayeredWindowAttributes(host, COLORREF(0), 255, LWA_ALPHA);
            // Not in screen captures, so measuring the sidebar sees Roon
            let _ = SetWindowDisplayAffinity(host, WDA_EXCLUDEFROMCAPTURE);

            let magnifier = CreateWindowExW(
                Default::default(),
                WC_MAGNIFIER,
                w!(""),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                1,
                1,
                Some(host),
                None,
                Some(instance.into()),
                None,
            )
            .ok()?;
            // 1x: no zoom
            let mut transform = MAGTRANSFORM { v: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] };
            let _ = MagSetWindowTransform(magnifier, &mut transform);
            // Never show ourselves
            let mut excluded = [host];
            let _ = MagSetWindowFilterList(magnifier, MW_FILTERMODE_EXCLUDE, 1, excluded.as_mut_ptr());
            Some((host, magnifier))
        }
    }

    /// Our window while it's showing, for wrapper.rs to keep it directly
    /// above the Wrapper.
    pub fn filter_window() -> Option<HWND> {
        if SHOWN.load(Ordering::Relaxed) {
            hwnd(&HOST)
        } else {
            None
        }
    }

    /// Called on the main thread while the Wrapper is dragged (it has
    /// already moved its record of the hole): moves our window without
    /// waiting for our thread, which points the picture at the new spot on
    /// its next frame.
    pub fn follow(x: i32, y: i32) {
        use windows::Win32::UI::WindowsAndMessaging::{SWP_ASYNCWINDOWPOS, SWP_NOSIZE};
        let Some(host) = hwnd(&HOST) else { return };
        if !SHOWN.load(Ordering::Relaxed) {
            return;
        }
        // The exact spot, not a step from where we think we are: our thread
        // may be rewriting that record at the same moment
        if let Some(p) = PLACED.lock().unwrap().as_mut() {
            p.rect[0] = x;
            p.rect[1] = y;
        }
        // SAFETY: our own window; asynchronous, so no waiting on its thread.
        unsafe {
            let _ = SetWindowPos(
                host,
                None,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }

    fn hide() {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
        if SHOWN.swap(false, Ordering::Relaxed) {
            if let Some(host) = hwnd(&HOST) {
                // SAFETY: our own window.
                unsafe {
                    let _ = ShowWindow(host, SW_HIDE);
                }
            }
        }
        *PLACED.lock().unwrap() = None;
    }

    /// Every frame: keep the filter over Roon's sidebar (or hidden), with
    /// the current colors, and refresh its picture. False when hidden.
    fn tick(wrapper: HWND) -> bool {
        let (Some(host), Some(magnifier)) = (hwnd(&HOST), hwnd(&MAGNIFIER)) else {
            return false;
        };
        let Some((roon, hole)) = crate::wrapper::visible_hole() else {
            *SIDEBAR.lock().unwrap() = None;
            hide();
            return false;
        };

        // The Wrapper is being dragged: no refreshes (each one makes Windows
        // copy part of the screen, which held the drag to about 60 fps). Our
        // window still moves along (see `follow`), carrying its last picture.
        // The first few still refresh, so that picture is from after the
        // Wrapper came to the front (not of a window that was over it).
        if crate::wrapper::moving() {
            if MOVING_TICKS.fetch_add(1, Ordering::Relaxed) >= FRESH_AT_DRAG && SHOWN.load(Ordering::Relaxed) {
                return true;
            }
        } else {
            MOVING_TICKS.store(0, Ordering::Relaxed);
        }

        // Where the sidebar is (now and then, and whenever it isn't known)
        let ticks = TICKS.fetch_add(1, Ordering::Relaxed);
        let known = *SIDEBAR.lock().unwrap();
        let sidebar = if known.is_none() || ticks.is_multiple_of(DETECT_EVERY) {
            match find_sidebar(hole, roon) {
                Detect::Found(found) => {
                    *SIDEBAR.lock().unwrap() = found;
                    found
                }
                // Too much of it covered by other windows to tell: keep
                // what we knew
                Detect::Covered => known,
            }
        } else {
            known
        };
        let Some((width, roon_bg)) = sidebar else {
            hide();
            return false;
        };

        let rect = [hole[0], hole[1], width, hole[3]];
        let colors = *COLORS.lock().unwrap();
        let (moved, recolor) = {
            let placed = PLACED.lock().unwrap();
            (
                placed.as_ref().is_none_or(|p| p.rect != rect),
                placed.as_ref().is_none_or(|p| p.colors != colors || p.roon_bg != roon_bg),
            )
        };
        // SAFETY: our own windows, on their own thread.
        unsafe {
            if recolor {
                let mut effect = color_effect(roon_bg, colors.0, colors.1);
                let _ = MagSetColorEffect(magnifier, &mut effect);
            }
            if moved {
                // Directly above the Wrapper (or at the top if nothing is
                // above it)
                use windows::Win32::UI::WindowsAndMessaging::{GetWindow, GW_HWNDPREV, HWND_TOP};
                let above = GetWindow(wrapper, GW_HWNDPREV).ok().filter(|w| !w.is_invalid());
                let after = match above {
                    Some(w) if w == host => GetWindow(host, GW_HWNDPREV).ok().filter(|w| !w.is_invalid()),
                    other => other,
                };
                let _ = SetWindowPos(
                    host,
                    Some(after.unwrap_or(HWND_TOP)),
                    rect[0],
                    rect[1],
                    rect[2],
                    rect[3],
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                let _ = SetWindowPos(magnifier, None, 0, 0, rect[2], rect[3], SWP_NOZORDER | SWP_NOACTIVATE);
                SHOWN.store(true, Ordering::Relaxed);
            }
            // Setting the source again is what makes the magnifier redraw
            let _ = MagSetWindowSource(
                magnifier,
                RECT { left: rect[0], top: rect[1], right: rect[0] + rect[2], bottom: rect[1] + rect[3] },
            );
        }
        *PLACED.lock().unwrap() = Some(Placed { rect, roon_bg, colors });
        true
    }

    enum Detect {
        /// The sidebar's width and grey, or None when Roon has it collapsed
        Found(Option<(i32, [u8; 3])>),
        /// Other windows cover too much of it to tell
        Covered,
    }

    /// Captures the hole's area of the screen (Roon, since our window is
    /// left out of captures) and finds the sidebar: the color at the far
    /// left, running right until a clean vertical edge into the content's
    /// plain background. Only rows where Roon is the window on top count
    /// (another window may be over part of the Wrapper).
    fn find_sidebar(hole: [i32; 4], roon: HWND) -> Detect {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};
        let width = (hole[2] * 6 / 10).max(200).min(hole[2]);
        let height = hole[3];
        if width < 150 || height < 100 {
            return Detect::Found(None);
        }
        // Rows to look at: the lower part, below Roon's logo and top bar,
        // where Roon is on top at both ends of the row, and also a little
        // above and below it: another window's shadow (much bigger while
        // it's the active window) darkens Roon next to it, which breaks
        // the plain colors this looks for
        let roon_at = |x: i32, y: i32| {
            // SAFETY: plain hit tests.
            unsafe {
                let w = WindowFromPoint(POINT { x, y });
                !w.is_invalid() && GetAncestor(w, GA_ROOT) == roon
            }
        };
        const SHADOW: i32 = 48;
        let clear = |y: i32| {
            // (kept inside the hole, so the last rows still count)
            [(y - SHADOW).max(0), y, (y + SHADOW).min(height - 1)].iter().all(|&sy| {
                roon_at(hole[0] + 4, hole[1] + sy) && roon_at(hole[0] + width - 1, hole[1] + sy)
            })
        };
        let rows: Vec<i32> = (0..24)
            .map(|i| height * 30 / 100 + i * (height * 65 / 100) / 24)
            .filter(|&y| clear(y))
            .collect();
        if rows.len() < 6 {
            return Detect::Covered;
        }
        let all_clear = rows.len() >= 20;
        // SAFETY: GDI objects made and freed here; the DIB holds width *
        // height 4-byte pixels while it's read.
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let found = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) => {
                    let old = SelectObject(dc, bitmap.into());
                    let copied =
                        BitBlt(dc, 0, 0, width, height, Some(screen), hole[0], hole[1], SRCCOPY).is_ok();
                    let pixels = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
                    let found = if copied { edge(pixels, width, &rows) } else { None };
                    SelectObject(dc, old);
                    let _ = DeleteObject(bitmap.into());
                    found
                }
                Err(_) => None,
            };
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
            // "No sidebar" (collapsed) only counts with Roon in clear view:
            // with part of it covered, a miss is more likely the covering
            // window's doing
            if found.is_none() && !all_clear {
                return Detect::Covered;
            }
            Detect::Found(found)
        }
    }

    fn edge(pixels: &[u8], width: i32, rows: &[i32]) -> Option<(i32, [u8; 3])> {
        let stride = width as usize * 4;
        let at = |x: i32, y: i32| -> [u8; 3] {
            let i = y as usize * stride + x as usize * 4;
            [pixels[i + 2], pixels[i + 1], pixels[i]]
        };
        let bg = mode(rows.iter().map(|&y| at(4, y)))?;
        for x in 120..width - 16 {
            let inside = rows.iter().filter(|&&y| at(x - 1, y) == bg).count();
            if inside * 10 < rows.len() * 8 {
                continue;
            }
            // Next column different from the sidebar on (nearly) every row,
            // and the same plain color for a while after it
            let Some(content) = mode(rows.iter().map(|&y| at(x, y))) else {
                continue;
            };
            if content == bg {
                continue;
            }
            let clean = rows.iter().filter(|&&y| (x..x + 16).all(|cx| at(cx, y) == content)).count();
            if clean * 10 >= rows.len() * 8 {
                return Some((x, bg));
            }
        }
        None
    }

    /// The most common color, if it covers most of the samples.
    fn mode(samples: impl Iterator<Item = [u8; 3]>) -> Option<[u8; 3]> {
        let samples: Vec<[u8; 3]> = samples.collect();
        let mut best = None;
        let mut best_count = 0;
        for s in &samples {
            let count = samples.iter().filter(|o| *o == s).count();
            if count > best_count {
                best_count = count;
                best = Some(*s);
            }
        }
        (best_count * 10 >= samples.len() * 6).then_some(best?)
    }

    /// The color matrix that turns Roon's sidebar colors into the
    /// Wrapper's: an affine map (the only kind a color matrix can do) fixed
    /// by three colors. Roon's grey becomes the Wrapper's background, Roon's
    /// text grey keeps its difference from it (flipped if Roon and the
    /// Wrapper differ in light/dark), Roon's purple becomes the accent, and
    /// everything in between follows (anti-aliased edges, hover rows,
    /// dimmer icons).
    fn color_effect(roon_bg: [u8; 3], bg: [u8; 3], accent: [u8; 3]) -> MAGCOLOREFFECT {
        MAGCOLOREFFECT { transform: color_matrix(roon_bg, bg, accent) }
    }

    pub(super) fn color_matrix(roon_bg: [u8; 3], bg: [u8; 3], accent: [u8; 3]) -> [f32; 25] {
        let f = |c: [u8; 3]| c.map(|v| v as f32);
        let (p0, t0, a) = (f(roon_bg), f(bg), f(accent));
        let dark = |c: [f32; 3]| c[0] * 0.3 + c[1] * 0.6 + c[2] * 0.1 < 128.0;
        let sign = if dark(p0) == dark(t0) { 1.0 } else { -1.0 };

        let sub = |x: [f32; 3], y: [f32; 3]| [x[0] - y[0], x[1] - y[1], x[2] - y[2]];
        let u1 = sub(ROON_TEXT, p0);
        let u2 = sub(ROON_ACCENT, p0);
        let n = cross(u1, u2);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-3);
        let u3 = n.map(|v| v / len * 100.0);
        // Where those three directions should go
        let v1 = u1.map(|v| v * sign);
        let v2 = sub(a, t0);
        let v3 = u3.map(|v| v * sign);

        // M = [v1 v2 v3] * [u1 u2 u3]^-1, then offset = t0 - M p0
        let from = [[u1[0], u2[0], u3[0]], [u1[1], u2[1], u3[1]], [u1[2], u2[2], u3[2]]];
        let to = [[v1[0], v2[0], v3[0]], [v1[1], v2[1], v3[1]], [v1[2], v2[2], v3[2]]];
        let m = match invert(from) {
            Some(inv) => multiply(to, inv),
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        let offset = [0, 1, 2].map(|r| t0[r] - (m[r][0] * p0[0] + m[r][1] * p0[1] + m[r][2] * p0[2]));

        // The magnifier's 5x5 matrix: row = input channel, column = output
        // channel, colors from 0 to 1, offsets in the fifth row
        let mut t = [0.0f32; 25];
        for out in 0..3 {
            for input in 0..3 {
                t[input * 5 + out] = m[out][input];
            }
            t[20 + out] = offset[out] / 255.0;
        }
        t[3 * 5 + 3] = 1.0; // alpha unchanged
        t[24] = 1.0;
        t
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }

    fn multiply(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
        let mut out = [[0.0; 3]; 3];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell = (0..3).map(|k| a[r][k] * b[k][c]).sum();
            }
        }
        out
    }

    fn invert(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if det.abs() < 1e-6 {
            return None;
        }
        let inv = 1.0 / det;
        Some([
            [
                (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv,
                (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv,
                (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv,
            ],
            [
                (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv,
                (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv,
                (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv,
            ],
            [
                (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv,
                (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv,
                (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv,
            ],
        ])
    }
}
