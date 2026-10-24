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
//! Roon has a minimum window size of its own. The Wrapper's minimum is
//! tauri.conf.json's, raised once to what Roon's minimum needs (plus our
//! bars around the hole), so Roon never hangs out below or beside it. It's
//! worked out from Roon's known minimum (ROON_MIN_CLIENT, a fixed fact
//! about Roon), not learned along the way: a minimum that kept growing
//! during resizes limited the Wrapper too early, and changing it mid-resize
//! could lock the app up.
//!
//! Roon's window loses its resize borders (WS_THICKFRAME) while it's in
//! the Wrapper. Otherwise its invisible borders, a few pixels outside what
//! it draws, poke out past the Wrapper's edges, and grabbing there would
//! resize Roon instead of the Wrapper. They come back with "Return Roon".
//!
//! The size the Wrapper is left at is remembered in logical pixels
//! (settings, `wrapperSize`) and put back once it's on screen: the saved
//! real-pixel size came out wrong on a screen with other scaling.
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

/// A picture of Roon's client area (BGRA rows, top down; width, height),
/// taken even while it's covered. For skin.rs, to find the sidebar while
/// the hole is closed.
#[cfg(windows)]
pub(crate) fn roon_picture(roon: windows::Win32::Foundation::HWND) -> Option<(Vec<u8>, i32, i32)> {
    windows_wrapper::roon_picture(roon)
}

/// Where Roon would show through if the page's Now Playing weren't covering
/// it all (Roon's window, and the hole in screen pixels), or None. For
/// skin.rs: its filter keeps going underneath us meanwhile, so it's ready
/// the moment Now Playing closes.
#[cfg(windows)]
pub(crate) fn covered_hole() -> Option<(windows::Win32::Foundation::HWND, [i32; 4])> {
    windows_wrapper::covered_hole()
}

/// Whether Roon's own player is being opened or closed for a link click
/// (or closed for ours closing) right now. For skin.rs: Roon's window shows
/// its player meanwhile, with no sidebar to find.
#[cfg(windows)]
pub(crate) fn player_busy() -> bool {
    windows_wrapper::player_busy()
}

/// Whether the Wrapper was resized (maximized, restored) in the last few
/// seconds. For skin.rs: that's when Roon folds its sidebar away or brings
/// it back, so it only looks at pictures of Roon's window then (taking one
/// makes Roon flicker where it shows).
#[cfg(windows)]
pub(crate) fn resized_lately() -> bool {
    windows_wrapper::resized_lately()
}

/// Roon's search run for `text` (blocking). For ocr.rs.
#[cfg(windows)]
pub(crate) fn roon_search_for(text: &str) -> bool {
    windows_wrapper::roon_search_for(text)
}

/// Scrolls Roon's page to its top (wheel-up messages). For ocr.rs.
#[cfg(windows)]
pub(crate) fn scroll_roon_to_top(sidebar: i32) -> bool {
    windows_wrapper::scroll_roon_to_top(sidebar)
}

/// A click on Roon's client area at (x, y), real pixels. For ocr.rs.
#[cfg(windows)]
pub(crate) fn click_roon_at(x: i32, y: i32) -> bool {
    windows_wrapper::click_roon_at(x, y)
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

/// While the Wrapper is dragged by its title bar and shows a live picture
/// of Roon in place of the hole (until the hole is open again after it):
/// the page's parts over the hole, relative to the hole's top-left corner
/// (real pixels). For skin.rs: its filter goes above the Wrapper meanwhile,
/// recoloring that picture, with these cut out so they keep their own
/// colors.
#[cfg(windows)]
pub(crate) fn drag_cuts() -> Option<Vec<[i32; 4]>> {
    windows_wrapper::drag_cuts()
}

/// The sidebar filter is set up for a drag (its cut-out made, the Wrapper
/// in its picture): the Wrapper can swap Roon for its live picture and go
/// under the filter. For skin.rs.
#[cfg(windows)]
pub(crate) fn drag_ready() {
    windows_wrapper::drag_ready()
}

/// Whether the Wrapper is showing its live picture of Roon for a drag (the
/// hole closed, Roon away). For skin.rs.
#[cfg(windows)]
pub(crate) fn live_picture() -> bool {
    windows_wrapper::live_picture()
}

/// One line in `drag-log.txt` (app data): what a drag of the Wrapper did,
/// for when the sidebar filter doesn't keep up with one. Dev builds only.
#[cfg(windows)]
pub(crate) fn drag_log(line: &str) {
    windows_wrapper::drag_log(line)
}

/// What the page shows in its title bar when something isn't right.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct WrapperStatus {
    pub open: bool,
    pub roon_found: bool,
    pub note: String,
    /// Roon couldn't be started after a few tries: the page says so,
    /// large, or opens Now Playing if there's a zone to show.
    pub roon_lost: bool,
    /// How wide Roon's sidebar is right now (real pixels from the hole's
    /// left edge, as skin.rs measures it), or null while it's collapsed or
    /// not known yet. The page puts its Wrapped button and page by it.
    pub sidebar: Option<i32>,
    /// Roon's own sidebar grey (RGB), with `sidebar`. The page draws its
    /// Wrapped button in Roon's colors: the sidebar filter (skin.rs) lies
    /// over that part of the hole and recolors everything under it, the
    /// button included, so drawn like Roon's sidebar it comes out in ours.
    pub sidebar_color: Option<[u8; 3]>,
    /// Roon's own background in its content header (RGB), as last seen; the
    /// page paints it around its content-header button's rounded corners.
    pub header_color: Option<[u8; 3]>,
}

/// Whether the Wrapper is switched on in Settings.
pub fn enabled(app: &AppHandle) -> bool {
    settings::get_settings(app.clone()).wrapper_enabled
}

/// Called by the page with the hole's rectangle in CSS pixels, relative to
/// the page (which fills the window's client area), whenever it changes;
/// `covers` are the page's own panels and menus over the hole ([x, y,
/// width, height] each), which stay part of the window; `radii`, one per
/// cover (0 or missing = square), round a cover's corners, so what's
/// outside them is Roon, not the page's background. `dim`: the side menu's
/// walkthrough is showing, so the page draws over all of Roon (its dimming
/// reaches Roon's corner by the Wrapped button too) while the sidebar
/// filter stays as it was, for the side menu drawn under it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn wrapper_hole(
    app: AppHandle,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    covers: Option<Vec<[f64; 4]>>,
    radii: Option<Vec<f64>>,
    dim: Option<bool>,
) {
    #[cfg(windows)]
    windows_wrapper::set_hole(
        &app,
        [x, y, width, height],
        covers.unwrap_or_default(),
        radii.unwrap_or_default(),
        dim.unwrap_or(false),
    );
    #[cfg(not(windows))]
    let _ = (app, x, y, width, height, covers, radii, dim);
}

/// Flashes the Wrapper's taskbar button: Deep Dive asked something or is
/// done while the window is minimized or behind another.
#[tauri::command]
pub fn wrapper_attention(app: AppHandle) {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
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

/// The page: its Now Playing opened or closed; Roon's own player screen
/// follows (see the notes on it in windows_wrapper).
#[tauri::command]
pub fn wrapper_roon_player(open: bool) {
    #[cfg(windows)]
    windows_wrapper::roon_player(open);
    #[cfg(not(windows))]
    let _ = open;
}

/// The page is about to close its Now Playing: Roon's own player screen
/// is closed first, and this returns once it's out of the way (at most
/// about a second), so Roon's page and sidebar are back when ours drops.
#[tauri::command]
pub async fn wrapper_roon_close() -> Result<(), String> {
    #[cfg(windows)]
    tauri::async_runtime::spawn_blocking(windows_wrapper::roon_close)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The page: a link in its Now Playing ("title", "album" or "artist") was
/// clicked; Roon goes to that page. Returns once the click is made: true,
/// or false if the link wasn't found on Roon's player screen.
#[tauri::command]
pub async fn wrapper_roon_link(which: String) -> Result<bool, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(move || windows_wrapper::roon_link(&which))
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = which;
        Ok(false)
    }
}

/// Search, set to Roon (see `settings::search_in_roon`): opens the
/// Wrapper; its page closes Now Playing if it's up, then asks for Roon's
/// search (`wrapper_roon_search`).
pub fn search(app: &AppHandle) {
    #[cfg(windows)]
    if enabled(app) {
        use tauri::Emitter;
        windows_wrapper::open(app);
        let _ = app.emit_to(WRAPPER_WINDOW, "wrapper-search", ());
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// The page: Roon's own search box, ready to type (Ctrl+F in Roon). False
/// if Roon couldn't be put in front for it.
#[tauri::command]
pub async fn wrapper_roon_search() -> Result<bool, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(windows_wrapper::roon_search)
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    Ok(false)
}

/// Roon's own queue shown (Ctrl+E in Roon), for the queue panel's button.
/// False if Roon couldn't be put in front for it.
#[tauri::command]
pub async fn wrapper_roon_queue() -> Result<bool, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(windows_wrapper::roon_queue).await.map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    Ok(false)
}

/// Roon's own search run for `text` (its results page comes up). False if
/// Roon couldn't be put in front for it.
#[tauri::command]
pub async fn wrapper_roon_search_for(text: String) -> Result<bool, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(move || windows_wrapper::roon_search_for(&text))
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Ok(false)
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

/// Remembers the Wrapper's size in logical pixels (lib.rs, once it has
/// settled after a move or resize).
#[cfg(windows)]
pub fn remember_size(app: &AppHandle) {
    windows_wrapper::remember_size(app);
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

    use tauri::{AppHandle, Emitter, Manager};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};

    use super::{WrapperStatus, WRAPPER_WINDOW};

    /// Height of Roon's transport bar (play controls at the bottom of its
    /// window, with the line above it), in logical pixels: Roon's window is
    /// made that much taller than the hole, so the bar sits hidden under our
    /// player bar and Roon's content ends right where ours starts. Measured
    /// on Roon 2.x.
    const ROON_BOTTOM_BAR: f64 = 104.0;

    // Roon's width is always exactly the hole's. Roon collapses its sidebar
    // below about 1400 logical px; that's allowed (the hamburger button
    // brings it back), so the Wrapper's minimum width is only what Roon's
    // own minimum needs (ROON_MIN_CLIENT plus our edges, see fit_minimum).


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
        /// Corner radius per cover (CSS px; 0 = square)
        radii: Vec<f64>,
        /// The side menu's walkthrough: the page over all of Roon, the
        /// filter as the covers alone would have it (see `wrapper_hole`)
        dim: bool,
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
    /// Where Roon is, behind the page's Now Playing covering all of the
    /// hole (screen pixels), see `covered_hole`.
    static COVERED_HOLE: Mutex<Option<[i32; 4]>> = Mutex::new(None);

    /// What the page draws over the hole (screen pixels), as of the last
    /// sync pass that showed Roon.
    static VISIBLE_COVERS: Mutex<Vec<[i32; 4]>> = Mutex::new(Vec::new());
    /// Whether the sidebar filter is showing (not see-through), as last
    /// told to the page ("wrapper-filter").
    static FILTER_SHOWN: AtomicBool = AtomicBool::new(true);

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
    /// The page's parts over the hole at the drag's start, relative to the
    /// hole's corner (see `drag_cuts`).
    static DRAG_CUTS: Mutex<Vec<[i32; 4]>> = Mutex::new(Vec::new());
    /// A drag's start waiting for the sidebar filter to be ready (since
    /// when), and whether it is (see `start_drag`).
    static DRAG_PENDING: Mutex<Option<Instant>> = Mutex::new(None);
    static DRAG_READY: AtomicBool = AtomicBool::new(false);
    /// How long a drag's start waits for the filter at most.
    const DRAG_READY_WAIT: Duration = Duration::from_millis(100);
    static APP: OnceLock<AppHandle> = OnceLock::new();

    /// Only once the Wrapper has actually moved: pressing the title bar
    /// starts Windows' move before the mouse moves, and the Wrapper comes
    /// to the front right then, so the filter still has to refresh.
    pub fn moving() -> bool {
        DRAGGING.load(Ordering::Relaxed)
            || LAST_MOVE.lock().unwrap().is_some_and(|at| at.elapsed() < MOVE_QUIET)
    }

    pub fn drag_log(line: &str) {
        use std::io::Write;
        // (a dev build's only)
        if !cfg!(debug_assertions) {
            return;
        }
        let Some(dir) = APP.get().and_then(|app| app.path().app_data_dir().ok()) else {
            return;
        };
        let path = dir.join("drag-log.txt");
        // (kept short: started over past 64 KB)
        let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024);
        let file = std::fs::OpenOptions::new().create(true).append(!too_big).write(true).truncate(too_big).open(&path);
        if let Ok(mut file) = file {
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() % 100_000_000).unwrap_or(0);
            let _ = writeln!(file, "{ms} {line}");
        }
    }

    pub fn drag_cuts() -> Option<Vec<[i32; 4]>> {
        let pending = DRAG_PENDING.lock().unwrap().is_some();
        let picture = pending || DRAGGING.load(Ordering::Relaxed) || !THUMBNAILS.lock().unwrap().is_empty();
        picture.then(|| DRAG_CUTS.lock().unwrap().clone())
    }

    pub fn drag_ready() {
        DRAG_READY.store(true, Ordering::Relaxed);
    }

    pub fn live_picture() -> bool {
        DRAGGING.load(Ordering::Relaxed)
    }

    /// A drag's start no longer waits (it ended first, or the Wrapper
    /// closed).
    fn cancel_pending() -> bool {
        DRAG_PENDING.lock().unwrap().take().is_some()
    }

    pub fn visible_hole() -> Option<(HWND, [i32; 4])> {
        let hole = (*VISIBLE_HOLE.lock().unwrap())?;
        Some((roon_window()?, hole))
    }

    pub fn roon_picture(roon: HWND) -> Option<(Vec<u8>, i32, i32)> {
        let picture = capture_roon(roon)?;
        Some((picture.pixels, picture.width, picture.height))
    }

    pub fn covered_hole() -> Option<(HWND, [i32; 4])> {
        let hole = (*COVERED_HOLE.lock().unwrap())?;
        Some((roon_window()?, hole))
    }

    /// Records where Roon shows through when a sync pass ends, whichever
    /// way it ends (None unless the pass got all the way to showing Roon),
    /// and where it is when Now Playing covers it all.
    struct HoleReport(Option<[i32; 4]>, Option<[i32; 4]>);

    impl Drop for HoleReport {
        fn drop(&mut self) {
            *VISIBLE_HOLE.lock().unwrap() = self.0;
            *COVERED_HOLE.lock().unwrap() = self.1;
        }
    }

    /// Whether the Wrapper is being moved or resized by the mouse right now
    /// (between WM_ENTERSIZEMOVE and WM_EXITSIZEMOVE).
    static IN_SIZE_MOVE: AtomicBool = AtomicBool::new(false);

    /// The Wrapper's minimum size (logical pixels): tauri.conf.json's, or
    /// more where Roon's own minimum needs it (see `fit_minimum`).
    static WRAPPER_MIN: Mutex<Option<(f64, f64)>> = Mutex::new(None);
    /// Roon's window style before we took its resize borders (0 = not
    /// changed).
    static ROON_STYLE: AtomicIsize = AtomicIsize::new(0);
    /// The smallest client area Roon allows, in logical pixels (measured
    /// on Roon 2.x at 100% scaling: a 760 x 783 window, 744 x 744 inside).
    /// A fixed fact about Roon, like ROON_BOTTOM_BAR: it used to be measured
    /// by shrinking Roon's window when it was taken in, but that came back
    /// wrong or not at all (Roon sets its minimum a moment after its window
    /// exists; a Roon parked by an earlier run wasn't re-measured), and the
    /// Wrapper then let itself be made smaller than Roon, with Roon hanging
    /// out below. In real pixels it depends on the screen's scaling, so
    /// it's worked out again for whichever screen the Wrapper is on.
    const ROON_MIN_CLIENT: (f64, f64) = (744.0, 744.0);

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
            roon_lost: ROON_LOST.load(Ordering::Relaxed),
            sidebar: crate::skin::sidebar().map(|(width, _)| width),
            sidebar_color: crate::skin::sidebar().map(|(_, color)| color),
            header_color: crate::skin::header_color(),
        }
    }


    pub fn start(app: &AppHandle) {
        let _ = APP.set(app.clone());
        remember_host(app);
        watch_foreground();
        watch_clicks();
        follow_moves();
        watch_player();
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
        check_soon();
        sync(app);
    }

    pub fn resized_lately() -> bool {
        LAST_RESIZE.lock().unwrap().is_some_and(|at| at.elapsed() < Duration::from_secs(6))
    }

    pub fn set_hole(app: &AppHandle, rect: [f64; 4], covers: Vec<[f64; 4]>, radii: Vec<f64>, dim: bool) {
        *HOLE.lock().unwrap() = Some(Hole { rect, covers, radii, dim });
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
                    // A Roon window out on the desktop: the Wrapper takes its
                    // place and size (Roon inside ends up a little smaller,
                    // by our bars). One we parked earlier has no place.
                    if !is_tool_window(roon) && is_visible(roon) && !is_minimized(roon) {
                        let spot = if is_maximized(roon) { RoonSpot::Maximized } else { visible_bounds(roon).map_or(RoonSpot::None, RoonSpot::At) };
                        *TAKE_ROON_SPOT.lock().unwrap() = spot;
                    }
                    adopt(roon);
                    set_note("");
                }
                None => {
                    ROON_HWND.store(0, Ordering::Relaxed);
                    *ROON_RESTORE.lock().unwrap() = None;
                    // A fresh round of tries
                    LAUNCH_TRIES.store(0, Ordering::Relaxed);
                    ROON_LOST.store(false, Ordering::Relaxed);
                    *MISSING_SINCE.lock().unwrap() = None;
                    if launch_roon() {
                        set_note("Starting Roon...");
                    } else {
                        set_note("Roon isn't running and couldn't be started. Start Roon and it will appear here.");
                    }
                }
            }
        }

        let was = STATE.swap(OPEN, Ordering::Relaxed);
        check_soon();
        super::button::refresh(app);
        // Opened (not already open): the page shows what Settings' Default
        // Menu says
        if was != OPEN {
            let _ = app.emit_to(WRAPPER_WINDOW, "wrapper-opened", ());
        }
        *LAST_REGION.lock().unwrap() = None;
        // A remembered size still has to fit the screen it opens on
        crate::fit_to_screen(app, WRAPPER_WINDOW, false);
        // fit_to_screen sets the configured minimum; Roon may need more
        // (but never more than this screen has room for)
        let min = *WRAPPER_MIN.lock().unwrap();
        if let Some(min) = min {
            let (w, h) = within_screen(&window, min);
            let _ = window.set_min_size(Some(tauri::Size::Logical(tauri::LogicalSize::new(w, h))));
        }
        apply_size(app);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        // Again once it's on its screen: Windows rescales a window that
        // comes up on a screen with other scaling
        restore_size_soon(app);
        // Maximized isn't remembered (Vulk's call): it opens at the last
        // size it had un-maximized (restored at startup, lib.rs)
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
        *COVERED_HOLE.lock().unwrap() = None;
        DRAGGING.store(false, Ordering::Relaxed);
        cancel_pending();
        drop_thumbnails();
        if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
            let _ = window.hide();
        }
        if let Some(roon) = roon_window() {
            hide(roon);
        }
        crate::save_window_state(app);
        // The page closes Now Playing, unless it's set to remember it
        {
            use tauri::Emitter;
            let _ = app.emit_to(WRAPPER_WINDOW, "wrapper-parked", ());
        }
    }

    pub fn put_back(app: &AppHandle) {
        let before = STATE.swap(OFF, Ordering::Relaxed);
        *VISIBLE_HOLE.lock().unwrap() = None;
        *COVERED_HOLE.lock().unwrap() = None;
        DRAGGING.store(false, Ordering::Relaxed);
        cancel_pending();
        drop_thumbnails();
        // Roon goes back where the Wrapper is, at its size, so putting it
        // back looks like the Wrapper's bars just going away
        let mut drop_at = RoonSpot::None;
        if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
            if before == OPEN {
                crate::save_window_state(app);
            }
            if before != OFF && !window.is_minimized().unwrap_or(false) {
                if window.is_maximized().unwrap_or(false) {
                    drop_at = RoonSpot::Maximized;
                } else if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
                    drop_at = RoonSpot::At((pos.x, pos.y, size.width as i32, size.height as i32));
                }
            }
            let _ = window.hide();
        }
        if let Some(host) = hwnd_of(&HOST_HWND) {
            clear_region(host);
        }
        *LAST_REGION.lock().unwrap() = None;
        if let Some(roon) = roon_window() {
            release(roon);
            let restore = ROON_RESTORE.lock().unwrap().take();
            match (drop_at, restore) {
                (RoonSpot::At(rect), _) => place_visible(roon, rect),
                (RoonSpot::Maximized, _) => {
                    // (on the Wrapper's screen, then maximized there)
                    if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
                        if let Ok(pos) = window.outer_position() {
                            let (w, h) = restore.map_or((1200, 800), |r| (r.2, r.3));
                            place_visible(roon, (pos.x + 40, pos.y + 40, w, h));
                        }
                    }
                    maximize(roon);
                }
                (RoonSpot::None, Some(rect)) => place_window(roon, rect),
                (RoonSpot::None, None) => show_where_it_is(roon),
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
        *COVERED_HOLE.lock().unwrap() = None;
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
        let mut report = HoleReport(None, None);
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
        // Roon gone while the Wrapper is showing (it quit, or it was still
        // closing when the Wrapper opened and took its last window): start
        // it again, a moment later, then 5 seconds after that; 10 seconds
        // after the second try, give up and say so (see ROON_LOST)
        if roon.is_some() {
            *MISSING_SINCE.lock().unwrap() = None;
            LAUNCH_TRIES.store(0, Ordering::Relaxed);
            ROON_LOST.store(false, Ordering::Relaxed);
        } else if is_visible(host) && !is_minimized(host) && !ROON_LOST.load(Ordering::Relaxed) {
            let since = *MISSING_SINCE.lock().unwrap().get_or_insert_with(Instant::now);
            let since_launch = LAUNCHED_AT.lock().unwrap().map(|at| at.elapsed());
            match LAUNCH_TRIES.load(Ordering::Relaxed) {
                0 if since.elapsed() >= MISSING_LAUNCH => {
                    if launch_roon() {
                        set_note("Starting Roon...");
                    } else {
                        set_note("Roon isn't running and couldn't be started. Start Roon and it will appear here.");
                    }
                }
                1 if since_launch.is_none_or(|t| t >= SECOND_TRY) && launch_roon() => {
                    set_note("Starting Roon...");
                }
                tries if tries >= 2 && since_launch.is_none_or(|t| t >= GIVE_UP) => {
                    ROON_LOST.store(true, Ordering::Relaxed);
                    set_note("Couldn't open Roon. Restart the Roon desktop app, and it will appear here.");
                }
                _ => {}
            }
        }

        // Roon's own "Enter full screen" (F11, and the item in its sidebar
        // menu) takes its window out of our hands: it drops the title bar
        // and spreads over the whole screen, and the page's buttons lose
        // their place. Undone at once with another F11 (see undo_fullscreen)
        if let Some(roon) = roon {
            if is_fullscreen(roon) {
                undo_fullscreen(app, roon);
                std::mem::forget(report);
                return;
            }
        }

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
        let (hole, covers, radii, dim) = {
            let guard = HOLE.lock().unwrap();
            let Some(saved) = guard.as_ref() else {
                return;
            };
            let radii: Vec<i32> = (0..saved.covers.len())
                .map(|i| (saved.radii.get(i).copied().unwrap_or(0.0) * scale).round() as i32)
                .collect();
            (to_window(&saved.rect), saved.covers.iter().map(to_window).collect::<Vec<_>>(), radii, saved.dim)
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
        // The sidebar filter goes see-through while nothing of Roon shows,
        // and solid again just before the hole opens, so it shows (and
        // hides) in the same frame as Roon. (The side menu's walkthrough
        // closes the hole but not the filter: the side menu under it is
        // drawn for it.)
        let exposed = open_hole && !subtract(hole, &covers).is_empty();
        crate::skin::wait(!exposed);
        // (the page draws the Wrapped button and side menu in Roon's colors
        // for the filter to recolor, and in its own while the filter is
        // see-through, during a resize, say: it's told when that changes)
        if FILTER_SHOWN.swap(exposed, Ordering::Relaxed) != exposed {
            let _ = app.emit_to(WRAPPER_WINDOW, "wrapper-filter", exposed);
        }
        let mut region_key = bounds.to_vec();
        region_key.push(open_hole as i32);
        region_key.push(dim as i32);
        if open_hole {
            region_key.extend_from_slice(&hole);
            for cover in &covers {
                region_key.extend_from_slice(cover);
            }
            region_key.extend_from_slice(&radii);
        }
        // The lock is let go before the region is set: from the sync loop,
        // SetWindowRgn waits for the main thread, which may be in here too
        // (a resize) waiting for this lock, and that locked the app up.
        let region_changed = {
            let mut last = LAST_REGION.lock().unwrap();
            let changed = last.as_ref() != Some(&region_key);
            if changed {
                *last = Some(region_key);
            }
            changed
        };
        if region_changed {
            set_region(host, bounds, (open_hole && !dim).then_some(hole), &covers, &radii);
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

        // The minimum that fits Roon's (only changes when that or the room
        // around the hole does; never touched while being resized)
        if !maximized && !resizing && !IN_SIZE_MOVE.load(Ordering::Relaxed) {
            fit_minimum(&window, scale, (client_w, client_h), (hole.2, hole.3));
        }

        // Being resized: Roon waits hidden until it stops. Without waiting
        // on Roon: this runs on the main thread during a resize, and Roon
        // may be waiting on us just then
        if resizing {
            hide_async(roon);
            return;
        }

        // We're minimized or hidden: Roon waits out of sight
        if is_minimized(host) || !is_visible(host) {
            hide_async(roon);
            return;
        }
        // Roon got minimized on its own (Win+D, "minimize all"): bring it
        // back without making it active
        if is_minimized(roon) {
            show_no_activate(roon);
        }

        // Roon's client area = the hole plus its transport bar below it
        let client = (hole.0, hole.1, hole.2, hole.3 + bar);
        // Roon goes right below us (below the sidebar filter, which sits
        // between us, see skin.rs)
        place_roon(roon, client, roon_anchor(host));
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
            } else {
                report.1 = Some(hole);
            }
            // (the walkthrough's page over all of Roon: a click anywhere in
            // the hole is on the page, not Roon)
            *VISIBLE_COVERS.lock().unwrap() = if dim { vec![hole] } else { covers };
        } else {
            // Roon is in place behind the closed hole, repainting after a
            // resize: the sidebar filter gets ready below us meanwhile, so
            // it's there when the hole opens
            report.1 = Some([hole.0, hole.1, hole.2, hole.3]);
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

    /// tauri.conf.json's minimum size for the Wrapper (logical pixels).
    fn configured_min(window: &tauri::WebviewWindow) -> (f64, f64) {
        window
            .app_handle()
            .config()
            .app
            .windows
            .iter()
            .find(|w| w.label == WRAPPER_WINDOW)
            .map(|w| (w.min_width.unwrap_or(0.0), w.min_height.unwrap_or(0.0)))
            .unwrap_or_default()
    }

    /// Sets the Wrapper's minimum size: tauri.conf.json's, or more where
    /// Roon's measured minimum needs a bigger hole (our bars above and below
    /// it added). Only when that comes out different from before, and never
    /// bigger than the screen; grows the Wrapper if it's smaller. `client`
    /// and `hole` are real pixels. No lock is held while asking the window
    /// anything (those calls wait for the main thread, which may be waiting
    /// for the same lock: that locked the app up).
    fn fit_minimum(window: &tauri::WebviewWindow, scale: f64, client: (i32, i32), hole: (i32, i32)) {
        use tauri::{LogicalSize, Size};
        let (roon_w, roon_h) = ROON_MIN_CLIENT;
        if hole.0 <= 0 || hole.1 <= 0 {
            return;
        }
        // Roon's window is the hole plus its transport bar (hidden under
        // our player bar); the rest of our window is around the hole
        let around = ((client.0 - hole.0) as f64 / scale, (client.1 - hole.1) as f64 / scale);
        let need = (roon_w + around.0, roon_h - ROON_BOTTOM_BAR + around.1);
        let configured = configured_min(window);
        let min = (configured.0.max(need.0).ceil(), configured.1.max(need.1).ceil());
        let changed = {
            let mut current = WRAPPER_MIN.lock().unwrap();
            let changed = current.is_none_or(|c| (c.0 - min.0).abs() > 2.0 || (c.1 - min.1).abs() > 2.0);
            if changed {
                *current = Some(min);
            }
            changed
        };
        if !changed {
            return;
        }
        let (w, h) = within_screen(window, min);
        let _ = window.set_min_size(Some(Size::Logical(LogicalSize::new(w, h))));
        let now = (client.0 as f64 / scale, client.1 as f64 / scale);
        if now.0 < w - 0.5 || now.1 < h - 0.5 {
            let _ = window.set_size(LogicalSize::new(now.0.max(w), now.1.max(h)));
        }
    }

    /// Where a Roon window was when the Wrapper took it in, for the Wrapper
    /// to open there instead of at its remembered size.
    #[derive(Clone, Copy, PartialEq)]
    enum RoonSpot {
        None,
        Maximized,
        /// Its visible bounds: x, y, width, height in real pixels.
        At((i32, i32, i32, i32)),
    }
    static TAKE_ROON_SPOT: Mutex<RoonSpot> = Mutex::new(RoonSpot::None);

    /// The Wrapper's size as it opens: where Roon was, if it was just taken
    /// in from the desktop, else the remembered size.
    fn apply_size(app: &AppHandle) {
        let spot = *TAKE_ROON_SPOT.lock().unwrap();
        match spot {
            RoonSpot::None => restore_size(app),
            RoonSpot::Maximized => {
                if let Some(window) = app.get_webview_window(WRAPPER_WINDOW) {
                    let _ = window.maximize();
                }
            }
            RoonSpot::At(rect) => take_roon_spot(app, rect),
        }
    }

    /// Puts the Wrapper where Roon's window was, at its size (no smaller
    /// than the Wrapper's minimum, and all on that screen).
    fn take_roon_spot(app: &AppHandle, (x, y, w, h): (i32, i32, i32, i32)) {
        use tauri::{PhysicalPosition, PhysicalSize};
        let Some(window) = app.get_webview_window(WRAPPER_WINDOW) else {
            return;
        };
        if window.is_maximized().unwrap_or(false) {
            let _ = window.unmaximize();
        }
        // (on Roon's screen first, so the scale and room are that screen's)
        let _ = window.set_position(PhysicalPosition::new(x, y));
        let Some(monitor) = window
            .available_monitors()
            .ok()
            .and_then(|all| {
                all.into_iter().find(|m| {
                    let (p, s) = (m.position(), m.size());
                    let (cx, cy) = (x + w / 2, y + h / 2);
                    cx >= p.x && cx < p.x + s.width as i32 && cy >= p.y && cy < p.y + s.height as i32
                })
            })
            .or_else(|| window.current_monitor().ok().flatten())
        else {
            return;
        };
        let scale = monitor.scale_factor();
        let work = monitor.work_area();
        let min = (*WRAPPER_MIN.lock().unwrap()).unwrap_or_else(|| configured_min(&window));
        let room = (work.size.width as i32, work.size.height as i32);
        let w = w.max((min.0 * scale).ceil() as i32).min(room.0);
        let h = h.max((min.1 * scale).ceil() as i32).min(room.1);
        let x = x.clamp(work.position.x, (work.position.x + room.0 - w).max(work.position.x));
        let y = y.clamp(work.position.y, (work.position.y + room.1 - h).max(work.position.y));
        let _ = window.set_size(PhysicalSize::new(w as u32, h as u32));
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }

    /// A window's bounds as you see them (without Windows' invisible resize
    /// borders, which GetWindowRect counts), in real pixels.
    fn visible_bounds(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
        use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
        let mut rect = RECT::default();
        // SAFETY: a valid window handle, and an out-pointer of the size given.
        let ok = unsafe {
            DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rect as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_ok()
        };
        if ok && rect.right > rect.left && rect.bottom > rect.top {
            Some((rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top))
        } else {
            window_rect(hwnd)
        }
    }

    /// Puts the Wrapper back at the size it was left at (logical pixels,
    /// saved by lib.rs as `wrapperSize`), fitted to the screen it's on and
    /// to its minimum, keeping it on that screen. Not while maximized.
    fn restore_size(app: &AppHandle) {
        use tauri::{PhysicalPosition, PhysicalSize};
        let Some(window) = app.get_webview_window(WRAPPER_WINDOW) else {
            return;
        };
        let Some([w, h]) = crate::settings::get_settings(app.clone()).wrapper_size else {
            return;
        };
        if window.is_maximized().unwrap_or(false) {
            return;
        }
        let Some(monitor) = window.current_monitor().ok().flatten() else {
            return;
        };
        let scale = monitor.scale_factor();
        let work = monitor.work_area();
        let room = (work.size.width as f64 / scale, work.size.height as f64 / scale);
        let min = (*WRAPPER_MIN.lock().unwrap()).unwrap_or_else(|| configured_min(&window));
        let target = (
            w.max(min.0).min(room.0),
            h.max(min.1).min(room.1),
        );
        let Ok(inner) = window.inner_size() else {
            return;
        };
        let now = (inner.width as f64 / scale, inner.height as f64 / scale);
        if (now.0 - target.0).abs() <= 1.5 && (now.1 - target.1).abs() <= 1.5 {
            return;
        }
        let size = PhysicalSize::new((target.0 * scale).round() as u32, (target.1 * scale).round() as u32);
        let _ = window.set_size(size);
        // Still all on its screen
        if let Ok(pos) = window.outer_position() {
            let x = pos.x.clamp(work.position.x, (work.position.x + work.size.width as i32 - size.width as i32).max(work.position.x));
            let y = pos.y.clamp(work.position.y, (work.position.y + work.size.height as i32 - size.height as i32).max(work.position.y));
            if (x, y) != (pos.x, pos.y) {
                let _ = window.set_position(PhysicalPosition::new(x, y));
            }
        }
    }

    /// `restore_size` again shortly after the Wrapper shows, once Windows
    /// has finished putting it on its screen.
    fn restore_size_soon(app: &AppHandle) {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                if STATE.load(Ordering::Relaxed) == OPEN && !IN_SIZE_MOVE.load(Ordering::Relaxed) {
                    apply_size(&handle);
                }
                // (Roon's spot is taken once: next time, the remembered size)
                *TAKE_ROON_SPOT.lock().unwrap() = RoonSpot::None;
            });
        });
    }

    /// Remembers the Wrapper's size in logical pixels (lib.rs, a second
    /// after it's moved or resized): only a normal, visible window.
    pub fn remember_size(app: &AppHandle) {
        let Some(window) = app.get_webview_window(WRAPPER_WINDOW) else {
            return;
        };
        if !window.is_visible().unwrap_or(false)
            || window.is_maximized().unwrap_or(false)
            || window.is_minimized().unwrap_or(false)
        {
            return;
        }
        let (Ok(inner), Ok(scale)) = (window.inner_size(), window.scale_factor()) else {
            return;
        };
        let size = [(inner.width as f64 / scale).round(), (inner.height as f64 / scale).round()];
        let Some(path) = crate::settings::path(app) else {
            return;
        };
        let mut current = crate::settings::load(&path);
        if current.wrapper_size != Some(size) {
            current.wrapper_size = Some(size);
            let _ = crate::settings::save(&path, &current);
        }
    }

    /// Moves Roon so its client area is `client` (x, y, width, height in
    /// real pixels), accounting for Roon's own title bar and borders, shows
    /// it, and puts it directly behind `behind`.
    /// What Roon goes right behind: the sidebar filter while it's showing
    /// (it sits between us and Roon, see skin.rs), else us.
    fn roon_anchor(host: HWND) -> HWND {
        // (Only while it's right below us: during a drag it's above us, see
        // `drag_cuts`, and Roon below it would be over us)
        crate::skin::filter_window().filter(|f| is_visible(*f) && crate::skin::right_below(host, *f)).unwrap_or(host)
    }

    fn place_roon(roon: HWND, client: (i32, i32, i32, i32), behind: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE};
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
                let _ = SetWindowPos(roon, Some(behind), want.0, want.1, want.2, want.3, SWP_NOACTIVATE);
            }
            show_na(roon);
        }
    }

    /// Shows a hidden window without activating it, with ShowWindow. Never
    /// SetWindowPos's "show" flag for Roon: that shows the window without
    /// telling it (no WM_SHOWWINDOW), and Roon (a Windows Forms app) went on
    /// believing it was hidden, which stalled its animations: its search box
    /// never folded away again, until Roon was restarted, even after the
    /// Wrapper let it go. Showing it this way also puts right a Roon left
    /// like that by an earlier version (the Wrapper hides it first).
    fn show_na(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNA};
        if !is_visible(hwnd) {
            // SAFETY: a valid window handle.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNA);
            }
        }
    }

    /// Puts a window at `rect` (its whole window: x, y, width, height) and
    /// shows it.
    fn place_window(hwnd: HWND, (x, y, w, h): (i32, i32, i32, i32)) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};
        // SAFETY: a valid window handle.
        unsafe {
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        show_na(hwnd);
    }

    /// Places a window so the part you see (without Windows' invisible
    /// resize borders) is `rect` (real pixels), and shows it. The borders
    /// are measured on the window itself, once it's shown.
    fn place_visible(hwnd: HWND, rect: (i32, i32, i32, i32)) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};
        let set = |(x, y, w, h): (i32, i32, i32, i32)| {
            // SAFETY: a valid window handle.
            unsafe {
                let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        };
        set(rect);
        show_na(hwnd);
        // (how far the window's rect reaches past what's drawn, per side)
        if let (Some(outer), Some(seen)) = (window_rect(hwnd), visible_bounds(hwnd)) {
            let left = seen.0 - outer.0;
            let top = seen.1 - outer.1;
            let right = (outer.0 + outer.2) - (seen.0 + seen.2);
            let bottom = (outer.1 + outer.3) - (seen.1 + seen.3);
            if (left, top, right, bottom) != (0, 0, 0, 0) {
                set((rect.0 - left, rect.1 - top, rect.2 + left + right, rect.3 + top + bottom));
            }
        }
    }

    fn maximize(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWMAXIMIZED};
        // SAFETY: a valid window handle.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);
        }
    }

    fn show_where_it_is(hwnd: HWND) {
        show_na(hwnd);
    }

    fn bring_to_front(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
        // SAFETY: a valid window handle. We were just clicked, so Windows
        // lets us hand the foreground on.
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
    }

    /// Keeps Roon directly behind the Wrapper (with the sidebar filter in
    /// between when it's showing, see skin.rs). If Roon has come out on top
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
            // The filter right below us, over Roon (it's on another thread:
            // never wait on it). It recolors Roon through the hole, and the
            // page's parts over the sidebar stay on top of it in their own
            // colors. (While Now Playing covers Roon it waits there
            // see-through, see skin.rs.)
            let hole_there = VISIBLE_HOLE.lock().unwrap().is_some() || COVERED_HOLE.lock().unwrap().is_some();
            let filter = crate::skin::filter_window().filter(|_| hole_there);
            if let Some(filter) = filter {
                if !crate::skin::right_below(host, filter) {
                    let _ = SetWindowPos(
                        filter,
                        Some(host),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                    );
                }
            }
            // Roon right below that (below us when there's no filter)
            let over_roon = filter.unwrap_or(host);
            if GetWindow(over_roon, GW_HWNDNEXT).ok() != Some(roon) {
                let _ = SetWindowPos(
                    roon,
                    Some(over_roon),
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
        radii: &[i32],
    ) {
        use windows::Win32::Graphics::Gdi::{
            CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, SetWindowRgn, RGN_DIFF,
        };
        // SAFETY: GDI calls on regions we just made; the window takes
        // ownership of the final region in SetWindowRgn.
        unsafe {
            let [x, y, w, h] = bounds;
            let whole = CreateRectRgn(x, y, x + w, y + h);
            if let Some(hole) = hole {
                let cut = CreateRectRgn(hole[0], hole[1], hole[0] + hole[2], hole[1] + hole[3]);
                for (i, c) in covers.iter().enumerate() {
                    let radius = radii.get(i).copied().unwrap_or(0);
                    let cover = if radius > 0 {
                        // The round-rect region's right/bottom edges are exclusive too
                        CreateRoundRectRgn(c[0], c[1], c[0] + c[2], c[1] + c[3], radius * 2, radius * 2)
                    } else {
                        CreateRectRgn(c[0], c[1], c[0] + c[2], c[1] + c[3])
                    };
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

    /// When Roon was last started by us, and since when it's been missing
    /// while the Wrapper is open (see `sync_open`).
    static LAUNCHED_AT: Mutex<Option<Instant>> = Mutex::new(None);
    static MISSING_SINCE: Mutex<Option<Instant>> = Mutex::new(None);
    /// Roon missing this long with the Wrapper open: started (again).
    const MISSING_LAUNCH: Duration = Duration::from_millis(1500);
    /// Still not there this long after: started once more (a Roon that was
    /// still closing can swallow a start).
    const SECOND_TRY: Duration = Duration::from_secs(5);
    /// And this long after that: given up on, until the Wrapper is opened
    /// again or Roon shows up by itself.
    const GIVE_UP: Duration = Duration::from_secs(10);
    /// Starts of Roon in this stretch of it missing, and whether we've given
    /// up on it.
    static LAUNCH_TRIES: AtomicU8 = AtomicU8::new(0);
    static ROON_LOST: AtomicBool = AtomicBool::new(false);

    /// Starts Roon from its usual per-user install folder.
    fn launch_roon() -> bool {
        *LAUNCHED_AT.lock().unwrap() = Some(Instant::now());
        LAUNCH_TRIES.fetch_add(1, Ordering::Relaxed);
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

    /// Like `hide`, but without waiting for the window's thread to do it
    /// (ShowWindowAsync): safe from the main thread, which must never wait
    /// on Roon.
    fn hide_async(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindowAsync, SW_HIDE};
        if is_visible(hwnd) {
            // SAFETY: a valid window handle.
            unsafe {
                let _ = ShowWindowAsync(hwnd, SW_HIDE);
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
            SWP_NOZORDER, WINDOWPOS, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_MOVING,
            WM_WINDOWPOSCHANGING,
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
            } else if msg == WM_EXITSIZEMOVE && cancel_pending() {
                // (let go before the live picture came: Roon followed along)
                drag_log("end (before the live picture)");
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
        if msg == WM_ENTERSIZEMOVE {
            IN_SIZE_MOVE.store(true, Ordering::Relaxed);
        } else if msg == WM_EXITSIZEMOVE {
            IN_SIZE_MOVE.store(false, Ordering::Relaxed);
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
        // Losing (or getting) focus: Windows repaints the frame it thinks
        // the window has, and with no frame of ours that came out as white
        // in the transparent corners while the Wrapper was in the
        // background. An lparam of -1 keeps the activation change but skips
        // that repaint.
        if msg == windows::Win32::UI::WindowsAndMessaging::WM_NCACTIVATE {
            // SAFETY: passing the message on, as every subclass must.
            return unsafe { DefSubclassProc(hwnd, msg, wparam, windows::Win32::Foundation::LPARAM(-1)) };
        }
        // SAFETY: passing the message on, as every subclass must.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// Whether Roon is in its full screen mode: no title bar any more (we
    /// only ever take its resize borders, see adopt), or its window spread
    /// over the whole screen, taskbar included.
    fn is_fullscreen(roon: HWND) -> bool {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_STYLE, WS_CAPTION};
        if !is_visible(roon) || is_minimized(roon) {
            return false;
        }
        // SAFETY: plain window queries on a valid handle.
        unsafe {
            let style = GetWindowLongPtrW(roon, GWL_STYLE) as u32;
            if style & WS_CAPTION.0 != WS_CAPTION.0 {
                return true;
            }
            let Some(rect) = window_rect(roon) else {
                return false;
            };
            let monitor = MonitorFromWindow(roon, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return false;
            }
            let m = info.rcMonitor;
            rect.0 <= m.left && rect.1 <= m.top && rect.0 + rect.2 >= m.right && rect.1 + rect.3 >= m.bottom
        }
    }

    /// When Roon last got an F11 from us, and how many in this stretch of
    /// full screen (so a wrong guess can't keep toggling it; see
    /// undo_fullscreen).
    static FULLSCREEN_UNDO: Mutex<Option<(Instant, u32)>> = Mutex::new(None);
    const FULLSCREEN_RETRY: Duration = Duration::from_millis(700);
    const FULLSCREEN_TRIES: u32 = 3;
    const FULLSCREEN_FORGET: Duration = Duration::from_secs(5);

    /// Presses F11 for Roon, which is how its full screen mode is left:
    /// typed when Roon is in front (it just took a click on its menu), posted
    /// to its window otherwise. At most once every 0.7 s and three times per
    /// stretch, and the page is told once ("roon-fullscreen").
    fn undo_fullscreen(app: &AppHandle, roon: HWND) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_F11,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, PostMessageW, WM_KEYDOWN, WM_KEYUP};
        let mut guard = FULLSCREEN_UNDO.lock().unwrap();
        let tries = match *guard {
            Some((at, _)) if at.elapsed() >= FULLSCREEN_FORGET => 0,
            Some((at, _)) if at.elapsed() < FULLSCREEN_RETRY => return,
            Some((_, tries)) if tries >= FULLSCREEN_TRIES => return,
            Some((_, tries)) => tries,
            None => 0,
        };
        *guard = Some((Instant::now(), tries + 1));
        drop(guard);
        if tries == 0 {
            let _ = app.emit_to(WRAPPER_WINDOW, "roon-fullscreen", ());
        }
        // SAFETY: a plain query, then either keyboard input with the size of
        // one INPUT, or messages posted to a valid window.
        unsafe {
            if GetForegroundWindow() == roon {
                let key = |up: bool| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_F11,
                            dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                            ..Default::default()
                        },
                    },
                };
                let inputs = [key(false), key(true)];
                SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            } else {
                // Scan code 0x57, repeat count 1; the key-up has the
                // previous-state and transition bits set
                let _ = PostMessageW(Some(roon), WM_KEYDOWN, WPARAM(VK_F11.0 as usize), LPARAM(0x0057_0001));
                let _ = PostMessageW(Some(roon), WM_KEYUP, WPARAM(VK_F11.0 as usize), LPARAM(0xC057_0001));
            }
        }
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
    ///
    /// With the sidebar filter showing, the first step only asks the filter
    /// to get ready for it (skin.rs, on its own thread: its cut-out for the
    /// page's parts, the Wrapper in its picture), and Roon follows along as
    /// usual meanwhile; a later step, once it's ready, does the rest, going
    /// under the filter in the same go. Doing it all at once showed Roon's
    /// own grey sidebar for a moment, until the filter caught up.
    fn start_drag(host: HWND) {
        use windows::Win32::Foundation::TRUE;
        use windows::Win32::Graphics::Dwm::{
            DwmRegisterThumbnail, DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES,
            DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE,
            DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        };
        let give_up = |why: &str| {
            cancel_pending();
            drag_log(&format!("start: no live picture ({why})"));
        };
        let Some(hole) = *VISIBLE_HOLE.lock().unwrap() else {
            return give_up("Roon isn't showing through the hole");
        };
        let Some(roon) = roon_window().filter(|r| is_visible(*r)) else {
            return give_up("Roon's window isn't showing");
        };
        let (Some(origin), Some(host_rect), Some((client_w, client_h))) =
            (client_origin(host), window_rect(host), client_size(host))
        else {
            return give_up("no size for our window");
        };
        // Somewhere off the screen for Roon to wait (none: no live picture,
        // Roon just follows the drag as before)
        let Some(away) = off_screen_spot(roon) else {
            return give_up("no spot off the screen for Roon");
        };
        let filter = crate::skin::filter_window();
        let pending = *DRAG_PENDING.lock().unwrap();
        match pending {
            None => {
                // The page's parts over the hole, from its corner (the hole
                // moves with the drag from here on, these don't)
                let covers = VISIBLE_COVERS.lock().unwrap().clone();
                *DRAG_CUTS.lock().unwrap() = covers.iter().map(|c| [c[0] - hole[0], c[1] - hole[1], c[2], c[3]]).collect();
                if filter.is_some() {
                    DRAG_READY.store(false, Ordering::Relaxed);
                    *DRAG_PENDING.lock().unwrap() = Some(Instant::now());
                    drag_log(&format!("start: hole {hole:?}, covers {covers:?}, waiting for the filter"));
                    return;
                }
            }
            Some(at) => {
                let ready = DRAG_READY.load(Ordering::Relaxed);
                if !ready && at.elapsed() < DRAG_READY_WAIT {
                    return;
                }
                drag_log(&format!(
                    "start: the filter {} after {} ms",
                    if ready { "ready" } else { "not ready, going ahead" },
                    at.elapsed().as_millis()
                ));
            }
        }
        cancel_pending();
        let covers: Vec<[i32; 4]> = DRAG_CUTS.lock().unwrap().iter().map(|c| [hole[0] + c[0], hole[1] + c[1], c[2], c[3]]).collect();
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
            drag_log("start: no live picture (Windows made none)");
            return;
        }
        // Under the filter (ready over Roon's sidebar), so it recolors the
        // live picture from the first frame. Only while it's right below
        // us, so we don't go under anything else; otherwise it comes up on
        // its own (skin.rs).
        // SAFETY: plain window queries, and our own window moved.
        let under = unsafe {
            filter.filter(|f| crate::skin::right_below(host, *f)).is_some_and(|f| {
                SetWindowPos(host, Some(f), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE).is_ok()
            })
        };
        drag_log(&format!(
            "start: {} pieces, {} live pictures, Roon waits at {away:?}, under the filter {under}",
            pieces.len(),
            thumbnails.len(),
        ));
        THUMBNAILS.lock().unwrap().extend(thumbnails);
        DRAGGING.store(true, Ordering::Relaxed);

        // Close the hole (the next sync pass after the drag opens it again)
        let bounds = [origin.x - host_rect.0, origin.y - host_rect.1, client_w, client_h];
        set_region(host, bounds, None, &[], &[]);
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
                let _ = SetWindowPos(roon, Some(roon_anchor(host)), now.0 + ox, now.1 + oy, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        DRAGGING.store(false, Ordering::Relaxed);
        *LAST_MOVE.lock().unwrap() = None;
        drag_log("end");
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
    /// A low-level mouse hook, for clicks that land on Roon through the
    /// hole (its sidebar, say) while the page has something over the rest of
    /// it: the page gets a "roon-click" event with the screen point and
    /// closes its Wrapped page, like any Roon page would get out of the way.
    /// The hook only looks; every click goes on to Roon. It runs on a
    /// thread of its own with its own message loop (it was on the main
    /// thread: whenever that was held up, Windows waited on the hook for
    /// every mouse move in every app, so the whole PC crawled with almost no
    /// CPU used, Vulk 2026-10-23, and a dev build stuck writing to a stalled
    /// terminal kept it up until the terminal closed).
    fn watch_clicks() {
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, SetWindowsHookExW, MSG, WH_MOUSE_LL};
        let _ = std::thread::Builder::new().name("mouse-hook".into()).spawn(|| {
            // SAFETY: a valid callback that lives for the whole program; the
            // loop only delivers the hook's calls on this thread.
            unsafe {
                if SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), None, 0).is_err() {
                    return;
                }
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
            }
        });
    }

    unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, MSLLHOOKSTRUCT, WM_LBUTTONDOWN};
        // (only while one of our windows, or Roon through the hole, is in
        // front and the Wrapper isn't minimized: a click in another app,
        // over where the hole was, isn't one on Roon)
        let ours = || {
            use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
            if !hwnd_of(&HOST_HWND).is_some_and(|h| !is_minimized(h)) {
                return false;
            }
            let Some(front) = foreground() else { return false };
            if Some(front) == roon_window() {
                return true;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(front, Some(&mut pid));
            pid == std::process::id()
        };
        if code >= 0 && wparam.0 as u32 == WM_LBUTTONDOWN && STATE.load(Ordering::Relaxed) == OPEN && ours() {
            let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            let (x, y) = (info.pt.x, info.pt.y);
            let on_roon = {
                let hole = *VISIBLE_HOLE.lock().unwrap();
                let covers = VISIBLE_COVERS.lock().unwrap().clone();
                hole.is_some_and(|h| {
                    subtract(h, &covers)
                        .iter()
                        .any(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3])
                })
            };
            if on_roon {
                if let Some(app) = APP.get() {
                    let _ = app.emit_to(WRAPPER_WINDOW, "roon-click", [x, y]);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

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

    // ----- Roon's own player screen ---------------------------------------
    //
    // Roon's artist, album and track links only exist on its own player
    // screen (Now Playing). So our links open Roon's player behind ours,
    // click the same link there, and Roon goes to that page. Otherwise
    // Roon's player is kept closed (so Roon's own page, and the sidebar
    // filter over it, are always ready behind ours). The clicks are sent straight to Roon's window
    // (no mouse movement): to its player bar (hidden under ours) to open
    // it, to its down arrow to close it, and to the links. Whether it's open
    // is read off a picture of Roon's window (taken even while it's
    // covered): open, the bar's album art is gone and the down arrow shows.
    // Checked when ours opens or closes and every second, and put right
    // when it doesn't match. The bar's album art stays put (Roon's 100%
    // scaling, measured from the bottom-left of its window); the down arrow
    // and the links move with the window's size, so they're found in the
    // picture (`player_screen`, `find_link`).

    /// Where to click to open Roon's player: the bar's album art (from the
    /// left, from the bottom; logical pixels).
    const SPOT_OPEN: (f64, f64) = (53.0, 52.0);

    /// Whether Roon's player should be open: never, outside a link click.
    static PLAYER_WANTED: AtomicBool = AtomicBool::new(false);
    /// Nothing is checked or clicked until then: Roon's player is still
    /// opening or closing, or Roon is going to a page.
    static PLAYER_QUIET: Mutex<Option<Instant>> = Mutex::new(None);
    /// Clicks made in a row without it matching; stops at 3, so a Roon that
    /// looks different (another language, a zoom) isn't clicked forever.
    static PLAYER_TRIES: AtomicU8 = AtomicU8::new(0);
    /// One check or click sequence at a time.
    static PLAYER_BUSY: Mutex<()> = Mutex::new(());
    /// One link click at a time.
    static LINK_BUSY: Mutex<()> = Mutex::new(());
    /// A link click or a close is using Roon's player (see `player_busy`).
    static PLAYER_IN_USE: AtomicBool = AtomicBool::new(false);

    /// Sets PLAYER_IN_USE for as long as it's held.
    struct InUse;
    impl InUse {
        fn new() -> Self {
            PLAYER_IN_USE.store(true, Ordering::Relaxed);
            InUse
        }
    }
    impl Drop for InUse {
        fn drop(&mut self) {
            PLAYER_IN_USE.store(false, Ordering::Relaxed);
        }
    }

    pub fn player_busy() -> bool {
        PLAYER_IN_USE.load(Ordering::Relaxed)
    }

    fn quiet_for(time: Duration) {
        *PLAYER_QUIET.lock().unwrap() = Some(Instant::now() + time);
    }

    fn wait_out_quiet() {
        let until = *PLAYER_QUIET.lock().unwrap();
        if let Some(until) = until {
            let now = Instant::now();
            if until > now {
                std::thread::sleep(until - now);
            }
        }
    }

    /// The page: our Now Playing opened or closed. Either way Roon's own
    /// player stays (or gets) closed.
    pub fn roon_player(_open: bool) {
        check_soon();
        PLAYER_WANTED.store(false, Ordering::Relaxed);
        PLAYER_TRIES.store(0, Ordering::Relaxed);
        std::thread::spawn(|| {
            wait_out_quiet();
            check_player();
        });
    }

    /// How long after Roon's player looks closed before the page drops
    /// ours: the rest of Roon's closing animation, so its sidebar (and our
    /// filter over it) is back in place when ours is gone.
    const SETTLE: Duration = Duration::from_millis(200);

    /// The page: one of our Now Playing's links was clicked ("title",
    /// "album" or "artist"). Opens Roon's player, clicks the link there as
    /// soon as it can be seen, then waits for Roon to leave its player for
    /// the page, so the page can drop ours onto it. Whether the link was
    /// found and clicked (if not, Roon's player is closed again).
    /// Blocking; run off the main thread.
    pub fn roon_link(which: &str) -> bool {
        // One at a time: clicks on our links while one is under way are
        // dropped, not queued (queued ones reopened Roon's player again and
        // again)
        let Ok(_busy) = LINK_BUSY.try_lock() else {
            return false;
        };
        let _player = PLAYER_BUSY.lock().unwrap();
        if STATE.load(Ordering::Relaxed) != OPEN {
            return false;
        }
        let _in_use = InUse::new();
        let Some(roon) = roon_window().filter(|r| is_visible(*r)) else {
            return false;
        };
        let scale = roon_scale();
        let mut opened_at: Option<Instant> = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut clicked = false;
        while Instant::now() < deadline {
            if let Some(picture) = capture_roon(roon) {
                if screen_state(&picture, scale).is_some() {
                    if let Some(spot) = find_link(&picture, scale, which) {
                        click_at(roon, spot);
                        clicked = true;
                        break;
                    }
                } else if opened_at.is_none_or(|at| at.elapsed() > Duration::from_millis(800)) {
                    roon_click(roon, SPOT_OPEN);
                    opened_at = Some(Instant::now());
                }
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        PLAYER_WANTED.store(false, Ordering::Relaxed);
        PLAYER_TRIES.store(0, Ordering::Relaxed);
        if !clicked {
            // Not found: Roon's player goes back down
            wait_closed(roon, true);
            return false;
        }
        // Roon goes to the page, its player closing by itself
        wait_closed(roon, false);
        true
    }

    /// Roon put in front for a shortcut: once the keys still held from our
    /// own hotkey are let go (Ctrl+F with Alt still down is a different
    /// shortcut), and only while the Wrapper holds Roon. The Wrapper comes
    /// back above Roon without taking the focus, as when Roon is clicked
    /// (see keep_order).
    fn roon_to_front() -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
        if STATE.load(Ordering::Relaxed) != OPEN {
            return false;
        }
        let Some(roon) = roon_window().filter(|r| is_visible(*r)) else {
            return false;
        };
        // The hotkey's own keys may still be down (Ctrl+F with Alt still
        // held is a different shortcut): wait for them to be let go
        let held = || {
            [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN].iter().any(|key| {
                // SAFETY: a plain key state query.
                (unsafe { GetAsyncKeyState(key.0 as i32) } as u16) & 0x8000 != 0
            })
        };
        let deadline = Instant::now() + Duration::from_millis(1500);
        while held() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if held() {
            return false;
        }
        // Roon in front, so the shortcut (and any typing after it) goes to
        // Roon
        // SAFETY: plain window calls on a valid handle.
        unsafe {
            let _ = SetForegroundWindow(roon);
        }
        let deadline = Instant::now() + Duration::from_millis(400);
        // SAFETY: a plain query.
        while unsafe { GetForegroundWindow() } != roon && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: a plain query.
        unsafe { GetForegroundWindow() == roon }
    }

    /// Ctrl plus `vk` pressed (into Roon, once it's in front).
    fn press_ctrl(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL};
        let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                    ..Default::default()
                },
            },
        };
        let inputs = [key(VK_CONTROL, false), key(vk, false), key(vk, true), key(VK_CONTROL, true)];
        // SAFETY: an array of keyboard inputs, with the size of one.
        unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) == inputs.len() as u32 }
    }

    /// Roon's own queue (Ctrl+E in Roon: "Show Now Playing/Queue"), for
    /// the queue panel's "Open Roon's queue", once our Now Playing's close
    /// has taken Roon's player along. Blocking; run off the main thread.
    pub fn roon_queue() -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while player_busy() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        roon_to_front() && press_ctrl(windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0x45))
    }

    /// Roon's own search box (Ctrl+F in Roon), for Search set to Roon.
    /// Blocking; run off the main thread.
    pub fn roon_search() -> bool {
        if !roon_to_front() {
            return false;
        }
        let press = || press_ctrl(windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0x46));
        // The first search after Roon starts takes two presses (Roon's own
        // search button does too), so it's pressed again if Roon's search
        // field doesn't show
        let shows = |wait: Duration| {
            let deadline = Instant::now() + wait;
            loop {
                if search_field_shown() {
                    return true;
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        };
        if !press() {
            return false;
        }
        if !shows(Duration::from_millis(700)) {
            let _ = press();
            return shows(Duration::from_millis(700));
        }
        true
    }

    /// Roon's search box with `text` typed in and entered: Roon goes to its
    /// results page for it. Blocking; run off the main thread.
    pub fn roon_search_for(text: &str) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VK_RETURN,
        };
        if !roon_search() {
            return false;
        }
        // The box takes a moment to take the focus
        std::thread::sleep(Duration::from_millis(250));
        let mut inputs: Vec<INPUT> = Vec::new();
        for unit in text.encode_utf16() {
            for up in [false, true] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wScan: unit,
                            dwFlags: if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE },
                            ..Default::default()
                        },
                    },
                });
            }
        }
        // SAFETY: an array of keyboard inputs, with the size of one.
        let typed = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) == inputs.len() as u32 };
        if !typed {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
        let enter = [false, true].map(|up| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VK_RETURN, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, ..Default::default() },
            },
        });
        // SAFETY: as above.
        unsafe { SendInput(&enter, std::mem::size_of::<INPUT>() as i32) == enter.len() as u32 }
    }

    /// Whether Roon's search field is up: a long white box along the top
    /// of Roon's page, right of the middle (where its search, bookmark and
    /// bell buttons sit otherwise). Read off the screen through the hole.
    fn search_field_shown() -> bool {
        // Roon's own picture first (the Wrapped page or Now Playing may be
        // over it on the screen); the screen when no picture can be taken
        if let Some(found) = search_field_in_picture() {
            return found;
        }
        search_field_on_screen()
    }

    /// The search box in a picture of Roon's window (see `capture_roon`).
    fn search_field_in_picture() -> Option<bool> {
        let roon = roon_window()?;
        let picture = capture_roon(roon)?;
        let scale = roon_scale();
        let x0 = picture.width * 2 / 5;
        let height = ((72.0 * scale).round() as i32).min(picture.height);
        let run_needed = (120.0 * scale).round() as i32;
        Some((0..height).any(|y| {
            let mut run = 0;
            (x0..picture.width).any(|x| {
                let (r, g, b) = picture.rgb(x, y);
                run = if r >= 225 && g >= 225 && b >= 225 { run + 1 } else { 0 };
                run >= run_needed
            })
        }))
    }

    fn search_field_on_screen() -> bool {
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
            SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
        };
        let Some(hole) = *VISIBLE_HOLE.lock().unwrap() else {
            return false;
        };
        let scale = roon_scale();
        let x = hole[0] + hole[2] * 2 / 5;
        let width = hole[2] - hole[2] * 2 / 5;
        let height = ((72.0 * scale).round() as i32).min(hole[3]);
        let run_needed = (120.0 * scale).round() as usize;
        if width <= 0 || height <= 0 {
            return false;
        }
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
            let mut found = false;
            if let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                let old = SelectObject(dc, bitmap.into());
                if BitBlt(dc, 0, 0, width, height, Some(screen), x, hole[1], SRCCOPY).is_ok() {
                    let pixels = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
                    found = (0..height as usize).any(|row| {
                        let line = &pixels[row * width as usize * 4..(row + 1) * width as usize * 4];
                        let mut run = 0usize;
                        line.chunks_exact(4).any(|p| {
                            let white = p[0] >= 225 && p[1] >= 225 && p[2] >= 225;
                            run = if white { run + 1 } else { 0 };
                            run >= run_needed
                        })
                    });
                }
                SelectObject(dc, old);
                let _ = DeleteObject(bitmap.into());
            }
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
            found
        }
    }

    /// The page is closing its Now Playing: closes Roon's player first and
    /// returns once it's out of the way (at most about a second), so ours
    /// drops onto Roon's page with its sidebar already back. Blocking; run
    /// off the main thread.
    pub fn roon_close() {
        PLAYER_WANTED.store(false, Ordering::Relaxed);
        PLAYER_TRIES.store(0, Ordering::Relaxed);
        let _player = PLAYER_BUSY.lock().unwrap();
        if STATE.load(Ordering::Relaxed) != OPEN {
            return;
        }
        if let Some(roon) = roon_window().filter(|r| is_visible(*r)) {
            let _in_use = InUse::new();
            wait_closed(roon, true);
        }
    }

    /// Waits (at most about a second) for Roon's player to be closed, then
    /// for its animation to finish; with `close`, clicks its down arrow
    /// (again only if a click hasn't taken in a while).
    fn wait_closed(roon: HWND, close: bool) {
        let scale = roon_scale();
        let deadline = Instant::now() + Duration::from_millis(1200);
        let mut clicked_at: Option<Instant> = None;
        let mut was_open = false;
        while Instant::now() < deadline {
            let Some(picture) = capture_roon(roon) else { break };
            match screen_state(&picture, scale) {
                None if !was_open => break,
                None => {
                    std::thread::sleep(SETTLE);
                    // Roon's page as it'll show: the sidebar filter finds
                    // the sidebar in it now, so it's there the moment ours
                    // drops (the hole is Roon's window minus its bar)
                    if let Some(picture) = capture_roon(roon) {
                        let bar = (ROON_BOTTOM_BAR * scale).round() as i32;
                        crate::skin::prime(&picture.pixels, picture.width, picture.height - bar);
                    }
                    break;
                }
                Some(arrow) => {
                    was_open = true;
                    if close && clicked_at.is_none_or(|at| at.elapsed() > Duration::from_millis(600)) {
                        click_at(roon, arrow);
                        clicked_at = Some(Instant::now());
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        quiet_for(Duration::from_millis(300));
    }

    /// Whether Roon's player should be looked at: after ours opens or
    /// closes, the Wrapper opens or is resized, or a click to fix it. Not
    /// all the time: each look is a picture of Roon's window (PrintWindow),
    /// which every now and then made all of Roon flash.
    static CHECK_DUE: AtomicBool = AtomicBool::new(true);

    fn check_soon() {
        CHECK_DUE.store(true, Ordering::Relaxed);
    }

    /// Every second while a look is due: Roon's player put in line with
    /// ours.
    fn watch_player() {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(1));
            if CHECK_DUE.load(Ordering::Relaxed) {
                check_player();
            }
        });
    }

    fn check_player() {
        let Ok(_busy) = PLAYER_BUSY.try_lock() else {
            return;
        };
        if STATE.load(Ordering::Relaxed) != OPEN || moving() {
            return;
        }
        if PLAYER_QUIET.lock().unwrap().is_some_and(|until| Instant::now() < until) {
            return;
        }
        if LAST_RESIZE.lock().unwrap().is_some_and(|at| at.elapsed() < RESIZE_QUIET + RESIZE_REPAINT + Duration::from_secs(1)) {
            return;
        }
        let Some(host) = hwnd_of(&HOST_HWND) else { return };
        let Some(roon) = roon_window() else { return };
        if !is_visible(host) || is_minimized(host) || !is_visible(roon) || is_minimized(roon) {
            return;
        }
        let wanted = PLAYER_WANTED.load(Ordering::Relaxed);
        let Some(arrow) = player_screen(roon) else { return };
        if arrow.is_some() == wanted {
            PLAYER_TRIES.store(0, Ordering::Relaxed);
            CHECK_DUE.store(false, Ordering::Relaxed);
            return;
        }
        // Only on a second look a moment later that agrees (a Roon still
        // repainting, after a resize say, can look like anything)
        std::thread::sleep(Duration::from_millis(150));
        let Some(again) = player_screen(roon) else { return };
        let agrees = match (arrow, again) {
            (Some(a), Some(b)) => (a.0 - b.0).abs() <= 4 && (a.1 - b.1).abs() <= 4,
            (None, None) => true,
            _ => false,
        };
        if !agrees {
            return;
        }
        if PLAYER_TRIES.fetch_add(1, Ordering::Relaxed) >= 3 {
            CHECK_DUE.store(false, Ordering::Relaxed);
            return;
        }
        match arrow {
            Some(arrow) => click_at(roon, arrow),
            None => roon_click(roon, SPOT_OPEN),
        }
        quiet_for(Duration::from_millis(700));
        // (Look again to see that it took)
        check_soon();
    }

    /// The screen's scaling where Roon is (the Wrapper's; Roon sits behind it).
    fn roon_scale() -> f64 {
        APP.get()
            .and_then(|app| app.get_webview_window(WRAPPER_WINDOW))
            .and_then(|w| w.scale_factor().ok())
            .unwrap_or(1.0)
    }

    /// A click sent straight to Roon's window at `spot` (from the left, from
    /// the bottom of its client area, logical pixels): to whichever of its
    /// windows is under that point. The mouse doesn't move.
    fn roon_click(roon: HWND, (from_left, from_bottom): (f64, f64)) {
        let Some((_, height)) = client_size(roon) else { return };
        let scale = roon_scale();
        click_at(
            roon,
            ((from_left * scale).round() as i32, height - (from_bottom * scale).round() as i32),
        );
    }

    /// A click on Roon at (x, y) of its client area (real pixels), for
    /// ocr.rs (a search result read off the screen).
    pub fn click_roon_at(x: i32, y: i32) -> bool {
        let Some(roon) = roon_window().filter(|r| is_visible(*r)) else {
            return false;
        };
        click_at(roon, (x, y));
        true
    }

    /// Scrolls Roon's page back to its top: mouse-wheel-up messages sent to
    /// the window under a point in the middle of the page (right of the
    /// sidebar), as if the wheel were turned there. For ocr.rs, which reads
    /// a page's title where it sits at the top.
    pub fn scroll_roon_to_top(sidebar: i32) -> bool {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::Graphics::Gdi::MapWindowPoints;
        use windows::Win32::UI::WindowsAndMessaging::{
            ChildWindowFromPointEx, PostMessageW, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT, WM_MOUSEWHEEL,
        };
        let Some(roon) = roon_window().filter(|r| is_visible(*r)) else {
            return false;
        };
        let (Some((width, height)), Some(origin)) = (client_size(roon), client_origin(roon)) else {
            return false;
        };
        let (x, y) = (sidebar + (width - sidebar) / 2, height / 3);
        // The wheel message carries the point in screen pixels; the window
        // it goes to is the one under that point, as for a click
        let screen = LPARAM((((origin.y + y) as u16 as isize) << 16) | ((origin.x + x) as u16 as isize));
        let mut point = POINT { x, y };
        // SAFETY: window queries and posted messages on valid handles.
        unsafe {
            let mut target = roon;
            for _ in 0..10 {
                let child = ChildWindowFromPointEx(target, point, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT);
                if child.is_invalid() || child == target {
                    break;
                }
                let mut points = [point];
                MapWindowPoints(Some(target), Some(child), &mut points);
                point = points[0];
                target = child;
            }
            // Up, a notch at a time (a page is rarely more than a few dozen
            // notches long; Roon smooths each one)
            // (the high word is the turn: +120, one notch away from the user)
            let up = WPARAM(120usize << 16);
            for _ in 0..40 {
                let _ = PostMessageW(Some(target), WM_MOUSEWHEEL, up, screen);
                std::thread::sleep(Duration::from_millis(8));
            }
        }
        true
    }

    /// A click sent straight to Roon's window at (x, y) of its client area
    /// (real pixels).
    fn click_at(roon: HWND, (x, y): (i32, i32)) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::Graphics::Gdi::MapWindowPoints;
        use windows::Win32::UI::WindowsAndMessaging::{
            ChildWindowFromPointEx, PostMessageW, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT,
            WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
        };
        let mut point = POINT { x, y };
        // SAFETY: window queries and posted messages on valid handles.
        unsafe {
            let mut target = roon;
            for _ in 0..10 {
                let child = ChildWindowFromPointEx(target, point, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT);
                if child.is_invalid() || child == target {
                    break;
                }
                let mut points = [point];
                MapWindowPoints(Some(target), Some(child), &mut points);
                point = points[0];
                target = child;
            }
            let at = LPARAM(((point.y as u32 as isize) << 16) | (point.x as u16 as isize));
            let _ = PostMessageW(Some(target), WM_MOUSEMOVE, WPARAM(0), at);
            std::thread::sleep(Duration::from_millis(60));
            // (wParam 1 = MK_LBUTTON: the left button is down)
            let _ = PostMessageW(Some(target), WM_LBUTTONDOWN, WPARAM(1), at);
            std::thread::sleep(Duration::from_millis(30));
            let _ = PostMessageW(Some(target), WM_LBUTTONUP, WPARAM(0), at);
        }
    }

    /// A picture of Roon's client area: BGRA rows, top down.
    struct Picture {
        pixels: Vec<u8>,
        width: i32,
        height: i32,
    }

    impl Picture {
        fn rgb(&self, x: i32, y: i32) -> (i32, i32, i32) {
            let x = x.clamp(0, self.width - 1);
            let y = y.clamp(0, self.height - 1);
            let i = ((y * self.width + x) * 4) as usize;
            (self.pixels[i + 2] as i32, self.pixels[i + 1] as i32, self.pixels[i] as i32)
        }

        fn lightness(&self, x: i32, y: i32) -> i32 {
            let (r, g, b) = self.rgb(x, y);
            (r + g + b) / 3
        }
    }

    /// Whether Roon's player screen is open, from a picture of its window:
    /// None if no picture could be taken, Some(None) if it's closed, and
    /// Some(Some(spot)) with its down arrow (real pixels in Roon's client
    /// area) if it's open. Open, the player bar's album art spot is plain
    /// and the bar's left part holds only the down arrow: one small mark
    /// (closed, it holds the track's title and artist, or nothing).
    fn player_screen(roon: HWND) -> Option<Option<(i32, i32)>> {
        let picture = capture_roon(roon)?;
        Some(screen_state(&picture, roon_scale()))
    }

    /// `player_screen`, from a picture already taken: its down arrow if
    /// Roon's player is open.
    fn screen_state(picture: &Picture, scale: f64) -> Option<(i32, i32)> {
        let (width, height) = (picture.width, picture.height);
        let px = |v: f64| (v * scale).round() as i32;
        // The album art spot: plain?
        let (mut low, mut high) = (255, 0);
        for y in (height - px(72.0)).max(0)..(height - px(32.0)).min(height) {
            for x in px(33.0)..px(73.0).min(width) {
                let value = picture.lightness(x, y);
                low = low.min(value);
                high = high.max(value);
            }
        }
        if high - low >= 12 {
            return None;
        }
        // The marks in the bar's left part (column groups split by gaps of
        // 6 or more)
        let background = picture.lightness(px(10.0), height - px(10.0));
        let (top, bottom) = ((height - px(70.0)).max(0), (height - px(38.0)).min(height));
        let (left, right) = (px(100.0), (width as f64 * 0.3) as i32);
        let ink = |x: i32| (top..bottom).any(|y| (picture.lightness(x, y) - background).abs() > 40);
        let columns: Vec<bool> = (left..right).map(ink).collect();
        let mut marks: Vec<(i32, i32)> = Vec::new();
        let mut x = 0;
        while x < columns.len() {
            if !columns[x] {
                x += 1;
                continue;
            }
            let start = x;
            let mut end = x;
            while x < columns.len() {
                if columns[x] {
                    end = x;
                    x += 1;
                } else {
                    let blank = x;
                    while x < columns.len() && !columns[x] {
                        x += 1;
                    }
                    if x - blank >= px(6.0) as usize {
                        break;
                    }
                }
            }
            marks.push((left + start as i32, left + end as i32));
        }
        let [(first, last)] = marks[..] else {
            return None;
        };
        if last - first > px(30.0) {
            return None;
        }
        // Its middle, top to bottom
        let x = (first + last) / 2;
        let rows: Vec<i32> = (top..bottom)
            .filter(|&y| (first..=last).any(|x| (picture.lightness(x, y) - background).abs() > 40))
            .collect();
        let (&row_top, &row_bottom) = (rows.first()?, rows.last()?);
        Some((x, (row_top + row_bottom) / 2))
    }

    /// Takes a picture of Roon's client area (even while it's covered).
    fn capture_roon(roon: HWND) -> Option<Picture> {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
        let (width, height) = client_size(roon)?;
        if width < 300 || height < 200 {
            return None;
        }
        // SAFETY: a bitmap of our own the size of Roon's client area, freed
        // below; PrintWindow draws Roon into it (client area only, with
        // what the GPU drew: flags 1 | 2).
        let pixels: Vec<u8> = unsafe {
            let screen = GetDC(None);
            let memory = CreateCompatibleDC(Some(screen));
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
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let result = match CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) => {
                    let old = SelectObject(memory, bitmap.into());
                    let drawn = PrintWindow(roon, memory, PRINT_WINDOW_FLAGS(1 | 2)).as_bool();
                    let copy = (drawn && !bits.is_null())
                        .then(|| std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize).to_vec());
                    SelectObject(memory, old);
                    let _ = DeleteObject(bitmap.into());
                    copy
                }
                Err(_) => None,
            };
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            result
        }?;
        Some(Picture { pixels, width, height })
    }

    /// Where a link is on Roon's open player screen (real pixels in its
    /// client area), found in a picture of it, since its place changes
    /// with the window's size. Under the line that tops the player's info
    /// panel come the track's lines: the title, "On" + album, and
    /// "Performed by" + artist. Colors differ with the window's size, so
    /// the words are told apart by the spaces between them: the album
    /// starts at the line's second word, the artist at its third. A line
    /// whose first word is short ("On") is the album's. The click goes just
    /// inside the name's first letter.
    fn find_link(picture: &Picture, scale: f64, which: &str) -> Option<(i32, i32)> {
        let (width, height) = (picture.width, picture.height);
        let px = |v: f64| (v * scale).round() as i32;
        let background = picture.rgb(px(10.0), height - px(10.0));
        let background_light = (background.0 + background.1 + background.2) / 3;
        let differs = |a: (i32, i32, i32), b: (i32, i32, i32)| {
            (a.0 - b.0).abs().max((a.1 - b.1).abs()).max((a.2 - b.2).abs())
        };
        // The panel's top line: from the bottom up, the first row that's
        // one color from left to right and not the background
        let (line_left, line_right) = (px(10.0), px(400.0).min(width - 1));
        let top = (0..height).rev().find(|&y| {
            let first = picture.rgb(line_left, y);
            differs(first, background) > 20
                && (line_left..line_right).step_by(4).all(|x| differs(picture.rgb(x, y), first) < 6)
        })?;
        // The first three lines of text under it, on the left (clear of the
        // transport buttons)
        let (left, right) = (px(20.0), (width as f64 * 0.35) as i32);
        let ink = |x: i32, y: i32| (picture.lightness(x, y) - background_light).abs() > 40;
        let mut lines = Vec::new();
        let mut y = top + 1;
        while y < height && lines.len() < 3 {
            if (left..right).any(|x| ink(x, y)) {
                let start = y;
                while y < height && (left..right).any(|x| ink(x, y)) {
                    y += 1;
                }
                lines.push((start, y));
            }
            y += 1;
        }
        // A line's words (first and last column of each): ink separated by
        // a gap of at least a quarter of the line's height
        let words = |y0: i32, y1: i32| -> Vec<(i32, i32)> {
            let columns: Vec<bool> = (left..right).map(|x| (y0..y1).any(|y| ink(x, y))).collect();
            let gap = ((y1 - y0) / 4).max(3) as usize;
            let mut found = Vec::new();
            let mut x = 0;
            while x < columns.len() {
                if !columns[x] {
                    x += 1;
                    continue;
                }
                let start = x;
                let mut end = x;
                while x < columns.len() {
                    if columns[x] {
                        end = x;
                        x += 1;
                    } else {
                        let blank = x;
                        while x < columns.len() && !columns[x] {
                            x += 1;
                        }
                        if x - blank >= gap {
                            break;
                        }
                    }
                }
                found.push((left + start as i32, left + end as i32));
            }
            found
        };
        let &(y0, y1) = lines.first()?;
        if which == "title" {
            let first = *words(y0, y1).first()?;
            return Some((first.0 + (y1 - y0) / 2, (y0 + y1) / 2));
        }
        for &(y0, y1) in lines.iter().skip(1) {
            let line = words(y0, y1);
            if line.len() < 2 {
                continue;
            }
            let is_album = ((line[0].1 - line[0].0) as f64) < 2.5 * (y1 - y0) as f64;
            if which == "album" && is_album {
                return Some((line[1].0 + px(3.0), (y0 + y1) / 2));
            }
            if which == "artist" && !is_album && line.len() >= 3 {
                return Some((line[2].0 + px(3.0), (y0 + y1) / 2));
            }
        }
        None
    }
}
