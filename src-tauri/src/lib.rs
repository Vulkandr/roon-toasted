// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use tauri::{
    webview::PageLoadEvent,
    AppHandle, Emitter, Manager, WebviewWindow, WindowEvent,
};

mod autostart;
mod browse;
mod hotkeys;
mod icon;
mod launch;
mod library;
mod update;
mod monitors;
mod queue;
mod roon;
mod settings;
mod status;
mod toast;
mod tray;
mod widget;
mod zones;

// Window labels, as set in tauri.conf.json.
/// The Toaster: the app's main screen (now playing, queue, browse/search, zones).
pub(crate) const TOASTER_WINDOW: &str = "toaster";
pub(crate) const SETTINGS_WINDOW: &str = "settings";

/// Set by `switch_core` (roon.rs) before the app restarts itself; the new copy
/// sees it and waits a moment before opening its windows (see `run`).
pub(crate) const RESTARTED_ENV: &str = "ROON_TOASTED_RESTARTED";

/// What gets saved and restored for the Toaster when "remember size" is on.
/// (Not visibility: the Toaster should never pop open on its own at launch.)
#[cfg(desktop)]
const WINDOW_STATE: tauri_plugin_window_state::StateFlags =
    tauri_plugin_window_state::StateFlags::SIZE
        .union(tauri_plugin_window_state::StateFlags::POSITION)
        .union(tauri_plugin_window_state::StateFlags::MAXIMIZED);

/// Saves the Toaster's current size/position right away (the plugin also saves
/// when the app exits), so it isn't lost if the app is ever force-closed.
fn save_window_state(app: &AppHandle) {
    #[cfg(desktop)]
    {
        use tauri_plugin_window_state::AppHandleExt;
        let _ = app.save_window_state(WINDOW_STATE);
    }
    #[cfg(not(desktop))]
    let _ = app;
}

/// Brings a window to the front, un-hiding and un-minimizing it if needed.
pub(crate) fn show(app: &AppHandle, label: &str) {
    if label == SETTINGS_WINDOW {
        fit_to_screen(app, label, true);
    }
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// How the Toaster was opened. Every way of opening it (tray, launching the
/// app again, and later hotkeys and the Stream Deck) goes through here.
#[derive(Clone, Copy)]
pub(crate) enum ToasterMode {
    /// Open as the player, as it was last left.
    Player,
    /// Open straight to Browse with the search box focused, ready to type.
    Search,
}

/// Gap kept between a window and the edges of the screen's usable area
/// (logical pixels).
const SCREEN_MARGIN: f64 = 16.0;

/// Makes a hidden window fit the usable area of the screen it will open on
/// (the screen minus the taskbar), so it never opens taller or wider than the
/// screen, or partly off it. Does nothing while the window is showing, so an
/// open window never jumps.
///
/// With `use_default_size` the window gets its size from tauri.conf.json
/// (shrunk to fit if needed) and is centered; otherwise it keeps its current
/// size (shrunk to fit if needed) and is only moved back on screen if it was
/// off it. The window's minimum size is lowered on small screens too, or
/// Windows would refuse to shrink it.
fn fit_to_screen(app: &AppHandle, label: &str, use_default_size: bool) {
    let Some(window) = app.get_webview_window(label) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        return;
    }
    let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    else {
        return;
    };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let avail_w = (work.size.width as f64 / scale - 2.0 * SCREEN_MARGIN).max(200.0);
    let avail_h = (work.size.height as f64 / scale - 2.0 * SCREEN_MARGIN).max(200.0);

    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == label)
        .map(|w| (w.width, w.height, w.min_width, w.min_height));
    let Some((default_w, default_h, min_w, min_h)) = config else {
        return;
    };

    let (mut width, mut height) = (default_w, default_h);
    if !use_default_size {
        if let Ok(size) = window.inner_size() {
            let size = size.to_logical::<f64>(scale);
            (width, height) = (size.width, size.height);
        }
    }
    let (width, height) = (width.min(avail_w), height.min(avail_h));

    // Set the minimum first, so the new size is never held back by the old one.
    if min_w.is_some() || min_h.is_some() {
        let min = tauri::LogicalSize::new(
            min_w.unwrap_or(0.0).min(avail_w),
            min_h.unwrap_or(0.0).min(avail_h),
        );
        let _ = window.set_min_size(Some(tauri::Size::Logical(min)));
    }
    let _ = window.set_size(tauri::LogicalSize::new(width, height));

    let Ok(outer) = window.outer_size() else {
        return;
    };
    let (work_x, work_y) = (work.position.x, work.position.y);
    let (free_x, free_y) = (
        work.size.width as i32 - outer.width as i32,
        work.size.height as i32 - outer.height as i32,
    );
    let (x, y) = if use_default_size {
        (work_x + free_x / 2, work_y + free_y / 2)
    } else {
        let (cur_x, cur_y) = window
            .outer_position()
            .map(|p| (p.x, p.y))
            .unwrap_or((work_x, work_y));
        (
            cur_x.clamp(work_x, work_x + free_x.max(0)),
            cur_y.clamp(work_y, work_y + free_y.max(0)),
        )
    };
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

/// Puts a hidden Toaster back to its default size (from tauri.conf.json),
/// centered and not maximized, and small enough for the screen. Skipped if
/// it's already on screen, so opening it again (e.g. the search hotkey) never
/// makes an open window jump.
fn reset_toaster_size(app: &AppHandle) {
    let Some(window) = app.get_webview_window(TOASTER_WINDOW) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        return;
    }
    let _ = window.unmaximize();
    fit_to_screen(app, TOASTER_WINDOW, true);
}

/// Shows the Toaster and tells its page which mode it was opened in, via the
/// `toaster-open` event (payload "player" or "search").
pub(crate) fn open_toaster(app: &AppHandle, mode: ToasterMode) {
    // "Default" window size means every opening, not just the first one.
    let saved = settings::get_settings(app.clone());
    if saved.toaster_window == settings::ToasterWindow::Default {
        reset_toaster_size(app);
    } else {
        // A remembered size still has to fit the screen it opens on
        // (e.g. after a resolution or scaling change)
        fit_to_screen(app, TOASTER_WINDOW, false);
    }
    show(app, TOASTER_WINDOW);
    let mode = match mode {
        ToasterMode::Player => "player",
        ToasterMode::Search => "search",
    };
    let _ = app.emit_to(TOASTER_WINDOW, "toaster-open", mode);
}

/// The Player hotkey: hides the Toaster when it's open and in front,
/// otherwise opens it as the player.
pub(crate) fn toggle_toaster(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(TOASTER_WINDOW) {
        let in_front = window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false);
        if in_front {
            let _ = window.hide();
            save_window_state(app);
            return;
        }
    }
    open_toaster(app, ToasterMode::Player);
}

/// Lets a page open another window, e.g. the Toaster's settings cog
/// (`invoke("open_window", { window: "settings" })`) or an "Open Toaster"
/// button in Settings. Only the app's own windows are allowed.
#[tauri::command]
fn open_window(app: AppHandle, window: String) -> Result<(), String> {
    match window.as_str() {
        TOASTER_WINDOW => open_toaster(&app, ToasterMode::Player),
        SETTINGS_WINDOW => show(&app, SETTINGS_WINDOW),
        _ => return Err(format!("Unknown window: {window}")),
    }
    Ok(())
}

/// Hides the Toaster and Settings once neither of them has focus, if the
/// "Auto-Hide" setting is on. The two count as one group: moving
/// between them hides nothing. Settings opened on its own (Toaster closed)
/// hides the same way. Waits a moment and checks again, so a brief focus
/// flicker (a window being shown, or focus passing from one to the other)
/// doesn't hide anything.
fn hide_if_app_unfocused(app: AppHandle) {
    if !settings::auto_hide_active(&app) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let toaster = app.get_webview_window(TOASTER_WINDOW);
        let settings = app.get_webview_window(SETTINGS_WINDOW);
        let focused =
            |w: &Option<WebviewWindow>| w.as_ref().is_some_and(|w| w.is_focused().unwrap_or(false));
        if focused(&toaster) || focused(&settings) {
            return;
        }
        for window in [toaster, settings].into_iter().flatten() {
            if window.is_visible().unwrap_or(false) {
                let _ = window.hide();
            }
        }
        save_window_state(&app);
    });
}

/// Asks Windows 11 for rounded corners on a frameless window (Windows doesn't
/// round frameless windows on its own; maximized windows stay square).
/// Does nothing on Windows 10, which has no rounded corners.
#[cfg(windows)]
fn round_corners(window: &WebviewWindow) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    if let Ok(hwnd) = window.hwnd() {
        let preference = DWMWCP_ROUND;
        // SAFETY: a valid window handle, and a pointer to a value of the size given.
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&preference) as u32,
            );
        }
    }
}

/// Hides the window that calls it, e.g. when Escape is pressed.
#[tauri::command]
fn hide_window(window: WebviewWindow) {
    let _ = window.hide();
    save_window_state(window.app_handle());
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Dev builds: print warnings from libraries (like roon-api) to the terminal.
    #[cfg(debug_assertions)]
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::WARN)
        .try_init();

    // Restarted (switching Cores): give the old copy's WebView2 time to finish
    // closing first. Opening windows while it's still shutting down makes them
    // fail ("Access is denied" / "The group or resource is not in the correct
    // state") and leaves the app with broken windows.
    let restarted = std::env::var_os(RESTARTED_ENV).is_some();
    if restarted {
        std::env::remove_var(RESTARTED_ENV);
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    let mut builder = tauri::Builder::default();

    // Only one copy of the app may run at a time. Launching it again (or a
    // roon-toasted:// link) goes to the copy that's already running, which
    // opens the Toaster as asked (see launch.rs); a plain launch opens it as
    // the player.
    // This has to be the first plugin registered.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let request = launch::request(&args)
                .unwrap_or(launch::Request::Open(ToasterMode::Player));
            launch::handle(app, request);
        }));

        // Tracks the Toaster's size/position so it can reopen where it was left.
        // Restoring is done in setup() (only when the setting says "remember").
        // Settings always opens centered, and the toast and the taskbar widget
        // place themselves (toast.rs, widget.rs), so none of those are tracked.
        builder = builder.plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(WINDOW_STATE)
                .with_denylist(&[SETTINGS_WINDOW, toast::TOAST_WINDOW, widget::WIDGET_WINDOW])
                .skip_initial_state(TOASTER_WINDOW)
                .build(),
        );

        // Global hotkeys (see hotkeys.rs); registered in setup()
        builder = builder.plugin(hotkeys::plugin());
    }

    builder
        // Registered before any window opens, so pages asking for zones or the
        // connection status right at startup always get an answer.
        .manage(zones::Zones::default())
        .manage(queue::Queue::default())
        .manage(roon::Roon::default())
        .manage(browse::BrowseState::default())
        .manage(hotkeys::HotkeyState::default())
        .manage(toast::ToastState::default())
        .manage(launch::StartupMode::default())
        .manage(library::Library::default())
        .manage(update::Updates::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Album art for the pages, fetched from the Core (see zones.rs).
        .register_asynchronous_uri_scheme_protocol("roonimg", |ctx, request, responder| {
            zones::image_request(ctx.app_handle(), request, responder)
        })
        // The zoom setting (the Toaster's, which the toast follows) is applied
        // each time their pages load, including dev-mode reloads.
        .on_page_load(|webview, payload| {
            let zoomed = matches!(webview.label(), TOASTER_WINDOW | toast::TOAST_WINDOW);
            if zoomed && payload.event() == PageLoadEvent::Started {
                settings::apply_zoom(webview.app_handle());
            }
        })
        .setup(move |app| {
            // Tray icon and its right-click menu (see tray.rs); the tray and
            // taskbar-button icons follow the taskbar's light/dark mode
            tray::build(app)?;
            icon::start(app.handle());

            #[cfg(windows)]
            for label in [TOASTER_WINDOW, SETTINGS_WINDOW, toast::TOAST_WINDOW] {
                if let Some(window) = app.get_webview_window(label) {
                    round_corners(&window);
                }
            }

            // Reopen the Toaster at its last size/position if the user chose that.
            #[cfg(desktop)]
            {
                use tauri_plugin_window_state::WindowExt;
                let saved = settings::get_settings(app.handle().clone());
                if saved.toaster_window == settings::ToasterWindow::Remember {
                    if let Some(toaster) = app.get_webview_window(TOASTER_WINDOW) {
                        let _ = toaster.restore_state(WINDOW_STATE);
                    }
                }
            }

            // Taskbar buttons only when "Auto-Hide" is off
            settings::apply_taskbar(app.handle());

            // The global hotkeys from Settings
            #[cfg(desktop)]
            hotkeys::register_saved(app.handle());

            // Windows' startup list, to match Start with Windows (autostart.rs)
            autostart::apply(app.handle());

            // The status check other apps (the Stream Deck plugin) use to see
            // that Roon: Toasted is running (see status.rs)
            status::start(app.handle());

            // The taskbar widget, if it's switched on (see widget.rs)
            widget::start(app.handle());

            // roon-toasted:// links, and opening the Toaster if a link or
            // --search started the app (see launch.rs)
            launch::startup(app.handle());

            // Connect the zone state to the app, then connect to Roon in the background.
            app.state::<zones::Zones>().init(app.handle());
            app.state::<queue::Queue>().init(app.handle());
            roon::start(app.handle());

            // Hearts and add-to-library, if switched on (see library.rs)
            library::start(app.handle());
            update::start(app.handle());

            // After switching Cores (the app restarts itself), bring Settings
            // back so the new connection can be seen
            if restarted {
                show(app.handle(), SETTINGS_WINDOW);
            }

            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing any window hides it instead of quitting.
            // The app keeps running in the tray until "Quit" is chosen.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
                save_window_state(window.app_handle());
                // Closing Settings goes back to the Toaster if it's open, so
                // focus doesn't land elsewhere and hide it too.
                if window.label() == SETTINGS_WINDOW {
                    if let Some(toaster) = window.app_handle().get_webview_window(TOASTER_WINDOW) {
                        if toaster.is_visible().unwrap_or(false) {
                            let _ = toaster.set_focus();
                        }
                    }
                }
            }
            // Clicking outside the Toaster and Settings sends them back to the
            // tray (a setting, on by default).
            WindowEvent::Focused(false)
                if matches!(window.label(), TOASTER_WINDOW | SETTINGS_WINDOW) =>
            {
                hide_if_app_unfocused(window.app_handle().clone());
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            open_window,
            hide_window,
            settings::get_settings,
            settings::update_settings,
            roon::roon_status,
            roon::switch_core,
            zones::roon_zones,
            zones::roon_select_zone,
            zones::roon_control,
            zones::roon_seek,
            zones::roon_zone_settings,
            zones::roon_set_volume,
            zones::roon_mute,
            browse::roon_browse,
            browse::roon_browse_more,
            browse::roon_browse_path,
            browse::roon_search,
            hotkeys::hotkey_status,
            hotkeys::set_hotkey,
            hotkeys::pause_hotkeys,
            queue::roon_queue,
            queue::roon_play_from_here,
            toast::toast_present,
            toast::toast_hide,
            toast::toast_test,
            monitors::list_monitors,
            launch::take_startup_mode,
            library::library_status,
            library::library_track,
            library::library_heart,
            library::library_add,
            library::library_album_heart,
            library::library_artist_albums,
            library::library_album_tracks_for,
            library::library_album_heart_for,
            library::library_album_add_for,
            update::update_status,
            update::update_check,
            update::update_install,
            library::library_album_tracks,
            library::library_track_heart,
            library::library_track_add,
            library::library_play_track
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
