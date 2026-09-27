//! Taskbar widget (experimental, off by default): a small player that sits
//! on the main monitor's taskbar (album art, title and artist, previous /
//! play-pause / next). Clicking the art or text opens the Toaster.
//!
//! Windows 11 has no official way to put something on the taskbar, so this
//! is its own window ("widget" in tauri.conf.json, page widget.html/css/js)
//! that never takes focus, floats on top, and is placed over the taskbar:
//! - Side (left or right end), Position (distance from that end) and Width
//!   come from Settings, since only the user knows where the taskbar has
//!   free space; the height follows the taskbar's.
//! - Clicking the taskbar raises it above the widget, so the widget puts
//!   itself back on top whenever another window comes to the front, and once
//!   a second checks whether the taskbar is above it, or whether Windows
//!   moved it (screen changes, e.g. switching HDR on or off, can do that).
//! - Every 30 seconds it's also put back in place and on top from scratch,
//!   so anything the checks miss sorts itself out.
//! - It hides while the taskbar is hidden (auto-hide), sideways (docked left
//!   or right), or while a full-screen app or game is running.
//! - Once a second it also follows the taskbar if it moves or changes size
//!   (e.g. Explorer restarting).
//!
//! Like the toast, it's shown and hidden straight through Windows (Tauri's
//! own show/hide would take focus, and would disagree with Windows about
//! whether the window is showing).

use tauri::AppHandle;

use crate::settings::AppSettings;

/// The widget window's label, as set in tauri.conf.json.
pub(crate) const WIDGET_WINDOW: &str = "widget";

/// Starts the widget (in setup): shows it now if it's switched on, and
/// keeps it in place from then on.
pub fn start(app: &AppHandle) {
    #[cfg(windows)]
    windows_widget::start(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Puts the widget where the settings say (or hides it), right away; called
/// when a widget setting changes, so the sliders move it live.
pub fn refresh(app: &AppHandle) {
    #[cfg(windows)]
    windows_widget::update(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Whether any widget setting differs between two versions of the settings.
pub fn settings_changed(before: &AppSettings, after: &AppSettings) -> bool {
    before.widget_enabled != after.widget_enabled
        || before.widget_side != after.widget_side
        || before.widget_position != after.widget_position
        || before.widget_width != after.widget_width
}

#[cfg(windows)]
mod windows_widget {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use tauri::{AppHandle, Manager};
    use windows::Win32::Foundation::{HWND, RECT};

    use super::WIDGET_WINDOW;
    use crate::settings::{self, WidgetSide};

    /// Gap between the widget and the taskbar's top and bottom edges, in
    /// logical pixels.
    const MARGIN: f64 = 5.0;

    /// The widget's window handle, for the foreground hook (0 = none yet).
    static WIDGET_HWND: AtomicIsize = AtomicIsize::new(0);

    /// Where the widget was last put (x, y, width, height in real pixels), or
    /// None while it's hidden. Never held while calling Windows (see `update`).
    static PLACED: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

    /// Whether the widget is showing, for the foreground hook.
    static SHOWING: AtomicBool = AtomicBool::new(false);

    pub fn start(app: &AppHandle) {
        let Some(window) = app.get_webview_window(WIDGET_WINDOW) else {
            return;
        };
        let Ok(hwnd) = window.hwnd() else {
            return;
        };
        WIDGET_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        watch_foreground();
        update(app);

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut seconds: u32 = 0;
            loop {
                tick.tick().await;
                seconds = seconds.wrapping_add(1);
                if seconds % 30 == 0 {
                    reset(&app);
                } else {
                    update(&app);
                }
            }
        });
    }

    fn widget_hwnd() -> Option<HWND> {
        let raw = WIDGET_HWND.load(Ordering::Relaxed);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    /// Shows the widget where it belongs, or hides it when it's off or
    /// shouldn't be showing right now.
    pub fn update(app: &AppHandle) {
        place_or_hide(app, false);
    }

    /// Like `update`, but puts the widget back in place and on top from
    /// scratch even if it looks fine (every 30 seconds, as a safety net).
    fn reset(app: &AppHandle) {
        place_or_hide(app, true);
    }

    fn place_or_hide(app: &AppHandle, from_scratch: bool) {
        let Some(hwnd) = widget_hwnd() else {
            return;
        };
        let saved = settings::get_settings(app.clone());
        let target = if saved.widget_enabled {
            target_rect(app, &saved)
        } else {
            None
        };

        // Decide under the lock, then let go of it before calling Windows:
        // moving a window waits on the main thread, which may be waiting here
        let before = std::mem::replace(&mut *PLACED.lock().unwrap(), target);
        SHOWING.store(target.is_some(), Ordering::Relaxed);
        match target {
            Some(rect) if from_scratch => {
                drop_from_top(hwnd);
                place(hwnd, rect);
            }
            // Not where it should be (or not showing): Windows may have moved
            // it, e.g. when the screen was reset for HDR
            Some(rect) if before != Some(rect) || current_rect(hwnd) != Some(rect) => {
                place(hwnd, rect)
            }
            // In place: just make sure it's still above the taskbar
            Some(_) => bring_to_top(hwnd),
            None if before.is_some() => hide(hwnd),
            None => {}
        }
    }

    /// Where the widget actually is (x, y, width, height), if it's showing.
    fn current_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindowVisible};
        let mut rect = RECT::default();
        // SAFETY: a valid window handle and out-pointer.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return None;
            }
            GetWindowRect(hwnd, &mut rect).ok()?;
        }
        Some((rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top))
    }

    /// Whether the taskbar is above the widget (it can raise itself over
    /// other always-on-top windows when clicked, or after a screen change).
    fn taskbar_above(hwnd: HWND) -> bool {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindow, GW_HWNDPREV};
        // SAFETY: plain window queries.
        unsafe {
            let Ok(taskbar) = FindWindowW(w!("Shell_TrayWnd"), None) else {
                return false;
            };
            // Walk up from the widget through the windows above it
            let mut window = hwnd;
            for _ in 0..1000 {
                match GetWindow(window, GW_HWNDPREV) {
                    Ok(above) if above == taskbar => return true,
                    Ok(above) if !above.is_invalid() => window = above,
                    _ => return false,
                }
            }
            false
        }
    }

    /// Takes the widget out of the always-on-top group for a moment, so
    /// putting it back makes Windows sort it to the very top again.
    fn drop_from_top(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_NOTOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        };
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// Where the widget goes (x, y, width, height in real pixels), or None
    /// when it shouldn't show right now.
    fn target_rect(app: &AppHandle, saved: &settings::AppSettings) -> Option<(i32, i32, i32, i32)> {
        if full_screen_app() {
            return None;
        }
        let taskbar = taskbar_rect()?;
        let monitor = app.primary_monitor().ok().flatten()?;
        let scale = monitor.scale_factor();

        let taskbar_width = taskbar.right - taskbar.left;
        let taskbar_height = taskbar.bottom - taskbar.top;
        // Docked on the left or right side of the screen: not supported
        if taskbar_width <= taskbar_height {
            return None;
        }
        // Auto-hide: mostly slid off the screen
        let screen_top = monitor.position().y;
        let screen_bottom = screen_top + monitor.size().height as i32;
        let on_screen = taskbar.bottom.min(screen_bottom) - taskbar.top.max(screen_top);
        if on_screen < taskbar_height * 3 / 4 {
            return None;
        }

        let margin = (MARGIN * scale).round() as i32;
        let height = taskbar_height - 2 * margin;
        if height < 16 {
            return None;
        }
        let width = ((saved.widget_width as f64 * scale).round() as i32).min(taskbar_width);
        let position = (saved.widget_position as f64 * scale).round() as i32;
        let x = match saved.widget_side {
            WidgetSide::Left => taskbar.left + position,
            WidgetSide::Right => taskbar.right - position - width,
        };
        // Never past either end of the taskbar
        let x = x.clamp(taskbar.left, taskbar.right - width);
        let y = taskbar.top + margin;
        Some((x, y, width, height))
    }

    /// The main taskbar's position and size, in real pixels.
    fn taskbar_rect() -> Option<RECT> {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect, IsWindowVisible};
        // SAFETY: plain window queries with a valid out-pointer.
        unsafe {
            let taskbar = FindWindowW(w!("Shell_TrayWnd"), None).ok()?;
            if !IsWindowVisible(taskbar).as_bool() {
                return None;
            }
            let mut rect = RECT::default();
            GetWindowRect(taskbar, &mut rect).ok()?;
            Some(rect)
        }
    }

    /// Whether a full-screen app, game or presentation is running (Windows
    /// holds back notifications then too).
    fn full_screen_app() -> bool {
        use windows::Win32::UI::Shell::{
            SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
            QUNS_RUNNING_D3D_FULL_SCREEN,
        };
        // SAFETY: no arguments; just asks Windows.
        unsafe {
            matches!(
                SHQueryUserNotificationState(),
                Ok(QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE)
            )
        }
    }

    fn place(hwnd: HWND, (x, y, width, height): (i32, i32, i32, i32)) {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW,
        };
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// Puts the widget above other windows again; if the taskbar is still
    /// above it after that, takes it out of the always-on-top group and back
    /// in, which Windows can't ignore.
    fn bring_to_top(hwnd: HWND) {
        raise(hwnd);
        if taskbar_above(hwnd) {
            drop_from_top(hwnd);
            raise(hwnd);
        }
    }

    fn raise(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        };
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn hide(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
        // SAFETY: a valid window handle.
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }

    /// Puts the widget back on top whenever a window comes to the front
    /// (clicking the taskbar raises it above the widget). Set up on the main
    /// thread, whose message loop delivers the events.
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
        _window: HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if SHOWING.load(Ordering::Relaxed) {
            if let Some(hwnd) = widget_hwnd() {
                bring_to_top(hwnd);
            }
        }
    }
}
