// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! "Enable Wrapper" in Roon's own title bar: while the Wrapper is switched
//! on but Roon has its own window (after "Return Roon", or Roon started by
//! itself), a small button sits in the middle of Roon's title bar. Clicking
//! it opens the Wrapper around Roon again.
//!
//! The button is our "wrapper-button" window (page wrapper-button.html):
//! no frame, transparent, never takes focus. It isn't attached to Roon's
//! window (a window of another program as owner ties the two programs'
//! input together, so a stuck Roon could freeze Toasted); instead it
//! follows Roon:
//! - Roon's moves, size changes, showing, hiding, minimizing and closing
//!   come in through a WinEvent hook for Roon's process, and the button is
//!   placed again (or hidden) each time.
//! - It's kept directly above Roon in the stacking order, so a window over
//!   Roon covers the button too. When Roon is clicked it comes up over the
//!   button, and the foreground hook in wrapper.rs puts the button back
//!   above it, again a moment later (Windows can finish raising Roon after
//!   telling us it came to the front, which left the button under Roon
//!   until Roon moved).
//!
//! Hidden while Roon is minimized, on another virtual desktop, full screen
//! (no title bar), or too narrow for the button to stay clear of its title.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;

use super::windows_wrapper as wrapper;
use super::BUTTON_WINDOW;

/// The button's size in logical pixels (the page fills it).
const WIDTH: f64 = 140.0;
const HEIGHT: f64 = 22.0;

/// Roon's window while the button sits on it (0 = none).
static ROON: AtomicIsize = AtomicIsize::new(0);
/// The hook watching Roon's process (0 = none).
static HOOK: AtomicIsize = AtomicIsize::new(0);
static APP: OnceLock<AppHandle> = OnceLock::new();

fn hwnd(raw: isize) -> Option<HWND> {
    (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
}

fn button(app: &AppHandle) -> Option<HWND> {
    let window = app.get_webview_window(BUTTON_WINDOW)?;
    window.hwnd().ok().map(|h| HWND(h.0))
}

/// Shows or hides the button for the Wrapper's current state: on Roon's
/// window while the Wrapper is switched on and not holding Roon, hidden
/// otherwise. Safe from any thread (the work happens on the main thread,
/// whose message loop delivers the hook's events).
pub(super) fn refresh(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let wanted = if super::enabled(&handle) && wrapper::is_off() {
            hwnd(ROON.load(Ordering::Relaxed))
                .filter(|roon| wrapper::is_roon_main_window(*roon))
                .or_else(|| wrapper::find_roon(false))
        } else {
            None
        };
        match wanted {
            Some(roon) => attach(&handle, roon),
            None => detach(&handle),
        }
    });
}

/// A window came to the front (from wrapper.rs's foreground hook, on the
/// main thread). Roon: the button goes back above it, or onto it if Roon
/// was just started by itself.
pub(super) fn on_foreground(window: HWND) {
    let Some(app) = APP.get() else { return };
    if hwnd(ROON.load(Ordering::Relaxed)) == Some(window) {
        place(app);
        place_again_soon(app);
    } else if ROON.load(Ordering::Relaxed) == 0
        && super::enabled(app)
        && wrapper::is_off()
        && wrapper::is_roon_main_window(window)
    {
        attach(app, window);
    }
}

/// Places the button again after Roon has surely finished coming up.
fn place_again_soon(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        for wait in [80, 320] {
            std::thread::sleep(std::time::Duration::from_millis(wait));
            let app = handle.clone();
            let _ = handle.run_on_main_thread(move || place(&app));
        }
    });
}

fn attach(app: &AppHandle, roon: HWND) {
    if ROON.swap(roon.0 as isize, Ordering::Relaxed) != roon.0 as isize {
        unhook();
        hook(roon);
    }
    place(app);
}

fn detach(app: &AppHandle) {
    ROON.store(0, Ordering::Relaxed);
    unhook();
    if let Some(button) = button(app) {
        hide(button);
    }
}

/// Watches Roon's process for its window moving, resizing, showing,
/// hiding, closing and switching virtual desktops.
fn hook(roon: HWND) {
    use windows::Win32::UI::Accessibility::SetWinEventHook;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, EVENT_OBJECT_DESTROY, EVENT_OBJECT_UNCLOAKED,
        WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    };
    // SAFETY: a valid window handle; the callback lives for the whole
    // program and the hook is removed in `unhook`.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(roon, Some(&mut pid));
        if pid == 0 {
            return;
        }
        let hook = SetWinEventHook(
            EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_UNCLOAKED,
            None,
            Some(on_roon_event),
            pid,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        HOOK.store(hook.0 as isize, Ordering::Relaxed);
    }
}

fn unhook() {
    use windows::Win32::UI::Accessibility::UnhookWinEvent;
    let raw = HOOK.swap(0, Ordering::Relaxed);
    if raw != 0 {
        // SAFETY: a hook we set and haven't removed yet.
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(raw as *mut core::ffi::c_void));
        }
    }
}

unsafe extern "system" fn on_roon_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    use windows::Win32::UI::WindowsAndMessaging::{EVENT_OBJECT_DESTROY, OBJID_WINDOW};
    // Roon's window itself, not something inside it
    if object != OBJID_WINDOW.0 || child != 0 || hwnd(ROON.load(Ordering::Relaxed)) != Some(window) {
        return;
    }
    let Some(app) = APP.get() else { return };
    if event == EVENT_OBJECT_DESTROY {
        // Roon closed. Not unhooking from inside the hook's own callback.
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || detach(&handle));
    } else {
        place(app);
    }
}

/// Puts the button in the middle of Roon's title bar, right above Roon in
/// the stacking order, or hides it when Roon's title bar isn't showing.
fn place(app: &AppHandle) {
    let Some(button) = button(app) else { return };
    let Some(roon) = hwnd(ROON.load(Ordering::Relaxed)) else {
        hide(button);
        return;
    };
    match spot(app, roon) {
        Some(rect) => show_above(button, roon, rect),
        None => hide(button),
    }
}

/// Where the button goes (x, y, width, height in real pixels), or None
/// when Roon's title bar isn't showing.
fn spot(app: &AppHandle, roon: HWND) -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_STYLE, WS_CAPTION};
    if !wrapper::is_window(roon) || !wrapper::is_visible(roon) || wrapper::is_minimized(roon) || is_cloaked(roon) {
        return None;
    }
    // SAFETY: a plain style query.
    let style = unsafe { GetWindowLongPtrW(roon, GWL_STYLE) } as u32;
    if style & WS_CAPTION.0 != WS_CAPTION.0 {
        return None;
    }
    let (_, wy, _, _) = wrapper::window_rect(roon)?;
    // What's drawn of the window (without the invisible resize borders)
    let frame = dwm_rect(roon, windows::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS)?;
    // The title bar's height, from the minimize/maximize/close buttons
    // (relative to the window's corner)
    let (top, height) = match dwm_rect(roon, windows::Win32::Graphics::Dwm::DWMWA_CAPTION_BUTTON_BOUNDS) {
        Some(caption) if caption.bottom > caption.top => (wy + caption.top, caption.bottom - caption.top),
        _ => (frame.top, 0),
    };
    let center_x = (frame.left + frame.right) / 2;
    let scale = app
        .monitor_from_point(center_x as f64, (top + height / 2) as f64)
        .ok()
        .flatten()
        .map(|monitor| monitor.scale_factor())
        .unwrap_or(1.0);
    let width = (WIDTH * scale).round() as i32;
    let button_height = (HEIGHT * scale).round() as i32;
    let height = if height > 0 { height } else { (31.0 * scale).round() as i32 };
    // Too narrow: it would sit on Roon's title text
    if frame.right - frame.left < width * 4 {
        return None;
    }
    Some((
        center_x - width / 2,
        top + (height - button_height) / 2,
        width,
        button_height,
    ))
}

fn dwm_rect(hwnd: HWND, attribute: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE) -> Option<RECT> {
    use windows::Win32::Graphics::Dwm::DwmGetWindowAttribute;
    let mut rect = RECT::default();
    // SAFETY: a RECT-sized out-buffer for a RECT attribute.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            attribute,
            &mut rect as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    Some(rect)
}

/// On another virtual desktop (or otherwise kept off screen by Windows).
fn is_cloaked(hwnd: HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    let mut cloaked = 0u32;
    // SAFETY: a u32 out-buffer for the cloaked attribute.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

/// Shows the button at `rect`, directly above Roon, without taking focus.
fn show_above(button: HWND, roon: HWND, (x, y, w, h): (i32, i32, i32, i32)) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindow, GetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, GW_HWNDPREV, HWND_TOP,
        SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, WS_EX_TOPMOST,
    };
    // SAFETY: plain window queries and a move on valid handles.
    unsafe {
        let above = GetWindow(roon, GW_HWNDPREV).ok().filter(|w| !w.is_invalid());
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
        // Under whatever is right above Roon, which puts it right above
        // Roon; at the top when nothing is (or only always-on-top windows)
        let after = match above {
            Some(window) if window == button => {
                flags |= SWP_NOZORDER;
                None
            }
            Some(window) if GetWindowLongPtrW(window, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize == 0 => Some(window),
            _ => Some(HWND_TOP),
        };
        let _ = SetWindowPos(button, after, x, y, w, h, flags);
    }
}

fn hide(button: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    if wrapper::is_visible(button) {
        // SAFETY: a valid window handle.
        unsafe {
            let _ = ShowWindow(button, SW_HIDE);
        }
    }
}
