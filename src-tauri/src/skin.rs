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
//! left, ending in a clean vertical line where the grey stops. When Roon
//! has its sidebar collapsed (the hamburger menu), there's no such line and
//! the filter steps aside.
//!
//! While nothing of Roon shows (the page's Now Playing covers it, or Roon
//! is repainting after a resize) the filter keeps going in its place, but
//! fully see-through, leaving the Wrapper out of its picture. wrapper.rs
//! makes it solid again right before the hole opens (see `wait`), so it's
//! there in the same frame as Roon.
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

/// Whether nothing of Roon shows through the Wrapper: the filter goes
/// see-through (true), or solid again (false). wrapper.rs calls this right
/// before it changes its window's shape, so the filter shows and hides in
/// step with the hole. Safe from any thread.
#[cfg(windows)]
pub fn wait(waiting: bool) {
    imp::wait(waiting);
}

/// Finds the sidebar ahead of time in a picture of Roon's window (wrapper.rs
/// takes one, even while Now Playing covers Roon): `pixels` are BGRA rows,
/// top down, `width` wide, and the hole is its top `height` rows. Used the
/// next time the hole opens (within a couple of seconds), so the filter is
/// there at once instead of after Roon shows on screen and is looked at.
#[cfg(windows)]
pub fn prime(pixels: &[u8], width: i32, height: i32) {
    imp::prime(pixels, width, height);
}

/// Roon's sidebar as last seen: its width (real pixels from the hole's left
/// edge) and Roon's own sidebar grey (RGB), or None while Roon has it
/// collapsed or it hasn't been seen yet. The Wrapper page lays its Wrapped
/// button and page out by the width, and draws the button in Roon's colors
/// (the filter recolors it along with the sidebar).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn sidebar() -> Option<(i32, [u8; 3])> {
    #[cfg(windows)]
    return imp::sidebar();
    #[cfg(not(windows))]
    None
}

/// Roon's own background color in its content header (RGB), as last seen
/// on screen, or None until Roon has been seen there. The Wrapper page
/// paints it around its content-header button's rounded corners, so they
/// look rounded although the window's cut-out for the button is square.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn header_color() -> Option<[u8; 3]> {
    #[cfg(windows)]
    return imp::header_color();
    #[cfg(not(windows))]
    None
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};
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
    /// The screen our window was on at the last refresh during a drag (0 =
    /// not dragging, -1 = across two screens).
    static DRAG_SCREEN: AtomicIsize = AtomicIsize::new(0);

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
    /// Whether the magnifier leaves the Wrapper's window out of its picture
    /// (while waiting).
    static WITHOUT_WRAPPER: AtomicBool = AtomicBool::new(false);
    /// Waiting (see `wait`), and the see-through-ness last given to our
    /// window (-1 = not yet).
    static WAITING: AtomicBool = AtomicBool::new(false);
    static ALPHA: AtomicI32 = AtomicI32::new(-1);
    /// Refreshes while waiting (see `wait`): only every this many.
    const STANDBY_EVERY: u32 = 6;
    /// Except for this many refreshes after it starts waiting (about half
    /// a second): Roon may be repainting under us then (after a resize),
    /// and the hole may open any moment, so every refresh counts.
    const STANDBY_FRESH: u32 = 60;
    /// Refreshes since it started waiting (see `wait`).
    static STANDBY_TICKS: AtomicU32 = AtomicU32::new(0);
    /// Looking for the sidebar while waiting (in a picture of Roon's
    /// window): every this many refreshes (about a quarter second).
    const STANDBY_DETECT_EVERY: u32 = 30;
    /// Whether this wait has looked yet.
    static STANDBY_LOOKED: AtomicBool = AtomicBool::new(false);
    /// Whether the last refresh had Roon showing through the hole. Coming
    /// to it (from waiting, or from nothing) looks at the screen
    /// right away, to correct whatever was kept in the meantime.
    static OVER_ROON: AtomicBool = AtomicBool::new(false);
    /// When a look first found no sidebar where one was known. "No
    /// sidebar" only counts once it still holds a moment later: a Roon
    /// still repainting (after a resize, or going to a page) shows none
    /// either, and believing it hid the filter until the next look, which
    /// was the flash.
    static MISSED_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    const MISS_HOLDS: std::time::Duration = std::time::Duration::from_millis(150);
    /// While a miss waits to be confirmed: looking again every this many
    /// refreshes.
    const MISS_RECHECK: u32 = 6;
    static PLACED: Mutex<Option<Placed>> = Mutex::new(None);
    /// The last sidebar found: its width and grey (None = not found). Kept
    /// while there's no hole (a resize, the Wrapper hidden), so the filter
    /// is ready again the moment there is.
    static SIDEBAR: Mutex<Option<(i32, [u8; 3])>> = Mutex::new(None);
    /// A sidebar: its width and grey, or None when Roon has it collapsed.
    type Sidebar = Option<(i32, [u8; 3])>;
    /// The sidebar found ahead of time (`prime`), and when.
    static PRIMED: Mutex<Option<(Sidebar, std::time::Instant)>> = Mutex::new(None);
    /// How long a primed sidebar stays good.
    const PRIMED_FOR: std::time::Duration = std::time::Duration::from_secs(2);

    pub fn sidebar() -> Option<(i32, [u8; 3])> {
        *SIDEBAR.lock().unwrap()
    }

    /// Roon's content header background, as last sampled (see `sample_header`).
    static HEADER: Mutex<Option<[u8; 3]>> = Mutex::new(None);

    pub fn header_color() -> Option<[u8; 3]> {
        *HEADER.lock().unwrap()
    }

    /// One pixel of Roon's content header on screen (a few pixels below the
    /// hole's top, in the middle of the content area, where the header is
    /// empty), when Roon is what's on screen there.
    fn sample_header(hole: [i32; 4], roon: HWND, sidebar_width: i32) -> Option<[u8; 3]> {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::GetPixel;
        use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};
        let content = hole[2] - sidebar_width;
        if content < 200 || hole[3] < 60 {
            return None;
        }
        let x = hole[0] + sidebar_width + content / 2;
        let y = hole[1] + 6;
        // SAFETY: a hit test and one pixel read off the screen.
        unsafe {
            let w = WindowFromPoint(POINT { x, y });
            if w.is_invalid() || GetAncestor(w, GA_ROOT) != roon {
                return None;
            }
            let screen = GetDC(None);
            let color = GetPixel(screen, x, y).0;
            ReleaseDC(None, screen);
            if color == 0xFFFF_FFFF {
                return None; // CLR_INVALID
            }
            Some([(color & 0xff) as u8, ((color >> 8) & 0xff) as u8, ((color >> 16) & 0xff) as u8])
        }
    }

    pub fn prime(pixels: &[u8], width: i32, height: i32) {
        if width < 200 || height < 100 || pixels.len() < (width * height * 4) as usize {
            return;
        }
        // The same rows `find_sidebar` reads (nothing covers this picture)
        let rows: Vec<i32> = (0..24).map(|i| height * 30 / 100 + i * (height * 65 / 100) / 24).collect();
        let found = edge(pixels, width, &rows);
        *PRIMED.lock().unwrap() = Some((found, std::time::Instant::now()));
    }

    fn hwnd(cell: &AtomicIsize) -> Option<HWND> {
        let raw = cell.load(Ordering::Relaxed);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    pub fn wait(waiting: bool) {
        WAITING.store(waiting, Ordering::Relaxed);
        apply_alpha();
    }

    /// Our window solid, or see-through while waiting.
    fn apply_alpha() {
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::UI::WindowsAndMessaging::{SetLayeredWindowAttributes, LWA_ALPHA};
        let Some(host) = hwnd(&HOST) else { return };
        let alpha = if WAITING.load(Ordering::Relaxed) { 0 } else { 255 };
        if ALPHA.swap(alpha, Ordering::Relaxed) != alpha {
            // SAFETY: our own layered window; doesn't wait on its thread.
            unsafe {
                let _ = SetLayeredWindowAttributes(host, COLORREF(0), alpha as u8, LWA_ALPHA);
            }
        }
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
            // Fully opaque (a layered window needs its alpha set to show);
            // `apply_alpha` keeps it in line with WAITING from here on
            let _ = SetLayeredWindowAttributes(host, COLORREF(0), 255, LWA_ALPHA);
            ALPHA.store(255, Ordering::Relaxed);
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
        // Roon showing through the hole, or covered by the page's Now
        // Playing (or repainting after a resize): then the filter waits
        // see-through, still recoloring Roon (the Wrapper left out of its
        // picture), so it's already there when the hole opens
        let (roon, hole, standby) = match crate::wrapper::visible_hole() {
            Some((roon, hole)) => (roon, hole, false),
            None => match crate::wrapper::covered_hole() {
                Some((roon, hole)) => (roon, hole, true),
                None => {
                    // (The sidebar stays known: see SIDEBAR)
                    STANDBY_LOOKED.store(false, Ordering::Relaxed);
                    STANDBY_TICKS.store(0, Ordering::Relaxed);
                    OVER_ROON.store(false, Ordering::Relaxed);
                    hide();
                    return false;
                }
            },
        };

        // The Wrapper is being dragged: no refreshes (each one makes Windows
        // copy part of the screen, which held the drag to about 60 fps). Our
        // window still moves along (see `follow`), carrying its last picture.
        // The first few still refresh, so that picture is from after the
        // Wrapper came to the front (not of a window that was over it).
        if crate::wrapper::moving() {
            // Except on its way onto another screen: the magnifier draws
            // nothing there (the sidebar went black until the drag ended)
            // until it's refreshed. So every frame while it's across two
            // screens, and a few fresh ones once it's on the new one.
            let screen = drag_screen(host);
            if DRAG_SCREEN.swap(screen, Ordering::Relaxed) != screen {
                MOVING_TICKS.store(0, Ordering::Relaxed);
            }
            if screen != -1
                && MOVING_TICKS.fetch_add(1, Ordering::Relaxed) >= FRESH_AT_DRAG
                && SHOWN.load(Ordering::Relaxed)
            {
                return true;
            }
        } else {
            DRAG_SCREEN.store(0, Ordering::Relaxed);
            MOVING_TICKS.store(0, Ordering::Relaxed);
        }

        // Where the sidebar is (now and then, and whenever it isn't known)
        let ticks = TICKS.fetch_add(1, Ordering::Relaxed);
        let mut known = *SIDEBAR.lock().unwrap();
        // A sidebar found ahead of time (Roon's page right after its player
        // closed): use it right away, over anything older (and look again
        // on the usual schedule)
        let primed = PRIMED
            .lock()
            .unwrap()
            .take()
            .filter(|(found, at)| at.elapsed() < PRIMED_FOR && (found.is_some() || known.is_none()))
            .is_some_and(|(found, _)| {
                *SIDEBAR.lock().unwrap() = found;
                known = found;
                TICKS.store(1, Ordering::Relaxed);
                true
            });
        let entering = !standby && !OVER_ROON.swap(!standby, Ordering::Relaxed);
        let missed = *MISSED_AT.lock().unwrap();
        // A look found this: a sidebar replaces what was known; "none"
        // where one was known only once it holds a moment
        let take = |found: Sidebar| -> Sidebar {
            if found.is_some() || known.is_none() {
                *MISSED_AT.lock().unwrap() = None;
                *SIDEBAR.lock().unwrap() = found;
                return found;
            }
            match missed {
                Some(at) if at.elapsed() >= MISS_HOLDS => {
                    *MISSED_AT.lock().unwrap() = None;
                    *SIDEBAR.lock().unwrap() = None;
                    None
                }
                Some(_) => known,
                None => {
                    *MISSED_AT.lock().unwrap() = Some(std::time::Instant::now());
                    known
                }
            }
        };
        if !standby {
            STANDBY_LOOKED.store(false, Ordering::Relaxed);
        }
        let sidebar = if standby {
            // Not from the screen while covered, which shows us there: from
            // a picture of Roon's window, on coming here and now and then
            // (more often while nothing is known: hidden, this runs about
            // every 50 ms, so every 4th is about 5 times a second). Never
            // while Roon's player is up for a link: that picture has no
            // sidebar, and what was known before is still right for when
            // Roon closes it.
            let first = !STANDBY_LOOKED.swap(true, Ordering::Relaxed);
            let due = first
                || (known.is_none() && ticks.is_multiple_of(4))
                || ticks.is_multiple_of(STANDBY_DETECT_EVERY);
            if due && !primed && !crate::wrapper::player_busy() {
                let found = crate::wrapper::roon_picture(roon).and_then(|(pixels, width, height)| {
                    let hole_height = hole[3].min(height);
                    (width == hole[2] && hole_height >= 100).then(|| {
                        let rows: Vec<i32> =
                            (0..24).map(|i| hole_height * 30 / 100 + i * (hole_height * 65 / 100) / 24).collect();
                        edge(&pixels, width, &rows)
                    })
                });
                // (A Roon still repainting can show anything: only a
                // sidebar counts here; the screen settles "none" once
                // the hole opens)
                match found {
                    Some(Some(found)) => take(Some(found)),
                    Some(None) | None => known,
                }
            } else {
                known
            }
        } else if known.is_none()
            || entering
            || (missed.is_some() && ticks.is_multiple_of(MISS_RECHECK))
            || (ticks.is_multiple_of(DETECT_EVERY) && !primed)
        {
            match find_sidebar(hole, roon) {
                Detect::Found(found) => take(found),
                // Too much of it covered by other windows to tell: keep
                // what we knew
                Detect::Covered => known,
            }
        } else {
            known
        };
        // Roon's own header background (behind the page's content-header
        // button), now and then, while Roon shows there
        if !standby && (entering || ticks.is_multiple_of(DETECT_EVERY)) {
            if let Some(color) = sample_header(hole, roon, sidebar.map(|(w, _)| w).unwrap_or(0)) {
                *HEADER.lock().unwrap() = Some(color);
            }
        }
        let Some((width, roon_bg)) = sidebar else {
            hide();
            return false;
        };

        let rect = [hole[0], hole[1], width, hole[3]];
        let colors = *COLORS.lock().unwrap();
        // Something came between us and the Wrapper (it was clicked and
        // came to the front, say): back on top of it
        let out_of_order = {
            use windows::Win32::UI::WindowsAndMessaging::{GetWindow, GW_HWNDPREV};
            // SAFETY: a plain window query.
            SHOWN.load(Ordering::Relaxed) && unsafe { GetWindow(wrapper, GW_HWNDPREV) }.ok() != Some(host)
        };
        let (moved, recolor) = {
            let placed = PLACED.lock().unwrap();
            (
                out_of_order || placed.as_ref().is_none_or(|p| p.rect != rect),
                placed.as_ref().is_none_or(|p| p.colors != colors || p.roon_bg != roon_bg),
            )
        };
        // Waiting: see-through, with the Wrapper (Now Playing) left out of
        // the picture, so it shows Roon the moment it's made solid
        apply_alpha();
        let waiting = WAITING.load(Ordering::Relaxed);
        if WITHOUT_WRAPPER.swap(waiting, Ordering::Relaxed) != waiting {
            // SAFETY: our own magnifier; the list holds valid windows.
            unsafe {
                let mut excluded = [host, wrapper];
                let count = if waiting { 2 } else { 1 };
                let _ = MagSetWindowFilterList(magnifier, MW_FILTERMODE_EXCLUDE, count, excluded.as_mut_ptr());
            }
        }
        // SAFETY: our own windows, on their own thread.
        unsafe {
            if recolor {
                let mut effect = color_effect(roon_bg, colors.0, colors.1);
                let _ = MagSetColorEffect(magnifier, &mut effect);
            }
            if moved {
                // Directly above the Wrapper (or at the top if nothing is
                // above it), waiting or not
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
            // Setting the source again is what makes the magnifier redraw.
            // Waiting, a little less often: not early on (see
            // STANDBY_FRESH), and not right after a sidebar was found ahead
            // of time (the picture was of Roon's player until then, and Now
            // Playing is about to drop).
            let waited = if standby { STANDBY_TICKS.fetch_add(1, Ordering::Relaxed) } else { 0 };
            if !standby {
                STANDBY_TICKS.store(0, Ordering::Relaxed);
            }
            if standby
                && waited >= STANDBY_FRESH
                && !moved
                && !recolor
                && !primed
                && !ticks.is_multiple_of(STANDBY_EVERY)
            {
                return true;
            }
            let _ = MagSetWindowSource(
                magnifier,
                RECT { left: rect[0], top: rect[1], right: rect[0] + rect[2], bottom: rect[1] + rect[3] },
            );
        }
        *PLACED.lock().unwrap() = Some(Placed { rect, roon_bg, colors });
        true
    }

    /// The screen our window is on, or -1 while it's across two.
    fn drag_screen(host: HWND) -> isize {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
        let mut rect = RECT::default();
        // SAFETY: our own window, and plain screen queries.
        unsafe {
            if GetWindowRect(host, &mut rect).is_err() {
                return 0;
            }
            let a = MonitorFromPoint(POINT { x: rect.left, y: rect.top }, MONITOR_DEFAULTTONEAREST);
            let b = MonitorFromPoint(POINT { x: rect.right - 1, y: rect.bottom - 1 }, MONITOR_DEFAULTTONEAREST);
            if a == b {
                a.0 as isize
            } else {
                -1
            }
        }
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
        // The sidebar's grey, from several columns: a window next to the
        // Wrapper on the left casts its shadow over Roon's first few dozen
        // pixels (the shadow isn't a window, so the hit tests above don't
        // see it), and the far left alone then reads as "no sidebar". The
        // shadow's shades and the sidebar's lettering each vary; the plain
        // grey is what most samples share.
        let columns = [4, 56, 72, 88, 104, 116].map(|x: i32| x.min(width - 1));
        let bg = mode(rows.iter().flat_map(|&y| columns.iter().map(move |&x| at(x, y))))?;
        // (Never more than half the width: Roon's player screen is black
        // most of the way across, which isn't a sidebar)
        for x in 120..(width / 2).min(width - 16) {
            let inside = rows.iter().filter(|&&y| at(x - 1, y) == bg).count();
            if inside * 10 < rows.len() * 8 {
                continue;
            }
            // Then the sidebar's grey stops on (nearly) every row, for a
            // while (not just a thin line inside it, like its scrollbar).
            // Whatever comes after doesn't matter: a plain background on
            // most pages, but an artist's photo or a blurred picture on
            // others, which used to count as "no sidebar".
            let out = rows.iter().filter(|&&y| (x..x + 16).all(|cx| at(cx, y) != bg)).count();
            if out * 10 >= rows.len() * 8 {
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
