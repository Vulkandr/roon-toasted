// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use tauri::{
    webview::PageLoadEvent,
    AppHandle, Emitter, Manager, WebviewWindow, WindowEvent,
};

mod audio;
mod autostart;
mod browse;
mod changelog;
mod discover;
mod dive;
mod judge;
mod ollama;
mod sources;
mod hotkeys;
mod icon;
mod launch;
mod library;
mod update;
mod monitors;
mod ocr;
mod queue;
mod roon;
mod roonnav;
mod settings;
mod skin;
mod sound;
mod home;
mod scrobbles;
mod shuffle;
mod musicfolder;
mod status;
mod toast;
mod tray;
mod widget;
mod tags;
mod wrapped;
mod wrapper;
mod zonekeeper;
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

/// Saves the Toaster's (and the Wrapper's) current size/position right away
/// (the plugin also saves when the app exits), so it isn't lost if the app is
/// ever force-closed.
pub(crate) fn save_window_state(app: &AppHandle) {
    #[cfg(desktop)]
    {
        use tauri_plugin_window_state::AppHandleExt;
        let _ = app.save_window_state(WINDOW_STATE);
    }
    #[cfg(not(desktop))]
    let _ = app;
}

/// Saves the window state a second after the Wrapper stops moving or
/// resizing, so its size sticks even when the app doesn't get to quit
/// cleanly (a dev rebuild, a crash, Windows shutting down). Maximized and
/// minimized never overwrite the size (the window-state plugin keeps the
/// last normal one).
fn save_window_state_soon(app: &AppHandle) {
    static TICKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    use std::sync::atomic::Ordering;
    let ticket = TICKET.fetch_add(1, Ordering::Relaxed) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if TICKET.load(Ordering::Relaxed) == ticket {
            save_window_state(&app);
            #[cfg(windows)]
            wrapper::remember_size(&app);
        }
    });
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
pub(crate) fn fit_to_screen(app: &AppHandle, label: &str, use_default_size: bool) {
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
    // (The Wrapper may fill the whole screen: a margin shrank one left that
    // wide every time it opened)
    let margin = if label == wrapper::WRAPPER_WINDOW { 0.0 } else { SCREEN_MARGIN };
    let avail_w = (work.size.width as f64 / scale - 2.0 * margin).max(200.0);
    let avail_h = (work.size.height as f64 / scale - 2.0 * margin).max(200.0);

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

/// What opens instead of the Toaster while it's switched off: the Wrapper
/// if it's on, otherwise Settings, so a hotkey, link, toast or widget click
/// always lands somewhere.
fn open_instead_of_toaster(app: &AppHandle) {
    if wrapper::enabled(app) {
        wrapper::open(app);
    } else {
        show(app, SETTINGS_WINDOW);
    }
}

/// The Toaster was switched off: out of sight if it was open.
pub(crate) fn hide_toaster(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(TOASTER_WINDOW) {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
            save_window_state(app);
        }
    }
}

/// Shows the Toaster and tells its page which mode it was opened in, via the
/// `toaster-open` event (payload "player" or "search"). While the Toaster is
/// switched off, opens the Wrapper (or Settings) instead.
pub(crate) fn open_toaster(app: &AppHandle, mode: ToasterMode) {
    // "Default" window size means every opening, not just the first one.
    let saved = settings::get_settings(app.clone());
    if !saved.toaster_enabled {
        return open_instead_of_toaster(app);
    }
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

/// The Search hotkey or link: Roon's own search through the Wrapper, or
/// the Toaster's Search tab (Settings decides while both are on, see
/// `settings::search_in_roon`).
pub(crate) fn open_search(app: &AppHandle) {
    if settings::search_in_roon(&settings::get_settings(app.clone())) {
        wrapper::search(app);
    } else {
        open_toaster(app, ToasterMode::Search);
    }
}

/// The Player hotkey: hides the Toaster when it's open and in front,
/// otherwise opens it as the player. (With the Toaster switched off, a
/// roon-toasted://toggle link toggles the Wrapper instead.)
pub(crate) fn toggle_toaster(app: &AppHandle) {
    if !settings::get_settings(app.clone()).toaster_enabled {
        if wrapper::enabled(app) {
            wrapper::toggle(app);
        } else {
            show(app, SETTINGS_WINDOW);
        }
        return;
    }
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
        wrapper::WRAPPER_WINDOW => wrapper::open(&app),
        changelog::CHANGELOG_WINDOW => changelog::open(&app),
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
        // The Changelog, opened from Settings, counts as part of the group
        let changelog = app.get_webview_window(changelog::CHANGELOG_WINDOW);
        let focused =
            |w: &Option<WebviewWindow>| w.as_ref().is_some_and(|w| w.is_focused().unwrap_or(false));
        if focused(&toaster) || focused(&settings) || focused(&changelog) {
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
/// round frameless windows on its own; maximized windows stay square). Also
/// after Zonekeeper lets go of one (zonekeeper.rs).
/// Does nothing on Windows 10, which has no rounded corners.
#[cfg(windows)]
pub(crate) fn round_corners(window: &WebviewWindow) {
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
    // Dev builds: print warnings from libraries (like roon-api) to the
    // terminal; but not Symphonia's MP3 decoder's at the start of every
    // preview (a 30-second clip cut from a longer song begins in the middle
    // of the data its first frames lean on, which the decoder skips)
    #[cfg(debug_assertions)]
    {
        use tracing_subscriber::filter::{LevelFilter, Targets};
        use tracing_subscriber::prelude::*;
        let shown = Targets::new().with_default(LevelFilter::WARN).with_target("symphonia_bundle_mp3", LevelFilter::ERROR);
        let _ = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer()).with(shown).try_init();
    }

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
            launch::relaunched(app, &args);
        }));

        // Tracks the Toaster's and the Wrapper's size/position so they can
        // reopen where they were left. Restoring is done in setup() (the
        // Toaster only when the setting says "remember"; the Wrapper always,
        // but never maximized). The Wrapper is also saved whenever it stops
        // moving or resizing (`save_window_state_soon`). Settings
        // and the Changelog always open centered, and the toast, the
        // taskbar widget and the "Enable Wrapper" button place themselves
        // (toast.rs, widget.rs, wrapper_button.rs), so none of those are
        // tracked.
        builder = builder.plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(WINDOW_STATE)
                .with_denylist(&[
                    SETTINGS_WINDOW,
                    toast::TOAST_WINDOW,
                    widget::WIDGET_WINDOW,
                    wrapper::BUTTON_WINDOW,
                    changelog::CHANGELOG_WINDOW,
                ])
                .skip_initial_state(TOASTER_WINDOW)
                .skip_initial_state(wrapper::WRAPPER_WINDOW)
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
        .manage(wrapped::Wrapped::default())
        .manage(discover::Discover::default())
        .manage(judge::Judge::default())
        .manage(sources::Sources::default())
        .manage(wrapped::LibraryCopy::default())
        .manage(tags::Tags::default())
        .manage(sound::Sound::default())
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
            // Tray icon and its right-click menu (see tray.rs), and the
            // logo on the tray and taskbar buttons (icon.rs)
            tray::build(app)?;
            icon::start(app.handle());

            #[cfg(windows)]
            for label in [
                TOASTER_WINDOW,
                SETTINGS_WINDOW,
                toast::TOAST_WINDOW,
                changelog::CHANGELOG_WINDOW,
            ] {
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
                // The Wrapper's size and position (never maximized: it reopens
                // at the size it last had un-maximized)
                if let Some(wrapper) = app.get_webview_window(wrapper::WRAPPER_WINDOW) {
                    use tauri_plugin_window_state::StateFlags;
                    let _ = wrapper.restore_state(StateFlags::SIZE | StateFlags::POSITION);
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

            // The Wrapper (see wrapper.rs): picks up a Roon an earlier run
            // left hidden. Before launch::startup, which may open it.
            wrapper::start(app.handle());
            // Roon's sidebar in the Wrapper's colors (see skin.rs)
            skin::start(app.handle());
            // Which page Roon is on, from its log (see roonnav.rs)
            roonnav::start(app.handle());

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
            // Wrapped's Home page kept current (see home.rs)
            home::schedule(app.handle());
            scrobbles::schedule(app.handle());

            // Just updated: the Changelog opens (see changelog.rs)
            changelog::at_start(app.handle());

            // Square corners while in a Zonekeeper zone (see zonekeeper.rs)
            zonekeeper::start(app.handle());

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
                // Closing the Wrapper hides it and Roon (Roon keeps running)
                if window.label() == wrapper::WRAPPER_WINDOW {
                    wrapper::park(window.app_handle());
                } else {
                    let _ = window.hide();
                    save_window_state(window.app_handle());
                }
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
            // Roon follows the Wrapper as it moves, and waits out resizing
            // (and its size and place are saved once it settles)
            WindowEvent::Moved(_) if window.label() == wrapper::WRAPPER_WINDOW => {
                wrapper::sync(window.app_handle());
                save_window_state_soon(window.app_handle());
            }
            WindowEvent::Resized(_) if window.label() == wrapper::WRAPPER_WINDOW => {
                wrapper::resized(window.app_handle());
                save_window_state_soon(window.app_handle());
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            open_window,
            hide_window,
            zonekeeper::zone_squared,
            settings::get_settings,
            settings::update_settings,
            roon::roon_status,
            roon::switch_core,
            roon::roon_search_again,
            roon::roon_set_manual_core,
            roon::roon_diagnostics,
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
            queue::roon_queue_start,
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
            update::update_check_if_stale,
            update::update_install,
            library::library_album_tracks,
            library::library_track_heart,
            library::library_track_add,
            library::library_play_track,
            library::library_album_by_id,
            library::library_artist_by_id,
            library::library_album_card,
            library::library_artist_card,
            library::library_artist_albums_for,
            library::library_artist_extras,
            library::library_artist_facts,
            library::library_genre_page,
            library::library_history,
            library::library_artists,
            library::library_share_link,
            wrapped::wrapped_history_status,
            wrapped::wrapped_history_sync,
            wrapped::wrapped_history_recent,
            wrapped::wrapped_history_top,
            wrapped::wrapped_artist_by_images,
            ocr::wrapper_read_page_name,
            ocr::wrapper_open_in_roon,
            wrapped::wrapped_library_status,
            wrapped::wrapped_library_sync,
            wrapped::wrapped_library_artists,
            wrapped::wrapped_library_albums,
            wrapped::wrapped_library_tracks,
            wrapped::wrapped_library_favorite,
            tags::wrapped_genres,
            tags::wrapped_genre_artists,
            library::library_track_handle,
            library::library_playlist_tracks,
            library::library_play_playlist_from,
            library::library_playlist_remove,
            library::library_tags,
            library::library_tag_contents,
            library::library_listen_later,
            library::library_listen_later_remove,
            discover::wrapper_build,
            discover::wrapped_discover_status,
            discover::wrapped_discover_build,
            discover::wrapped_discover_ratings,
            discover::wrapped_discover_vote,
            discover::wrapped_discover_reset,
            discover::wrapped_discover_request_add,
            discover::wrapped_discover_request_remove,
            discover::wrapped_discover_request_keep,
            dive::wrapped_dive_chat,
            dive::wrapped_dive_build,
            dive::wrapped_dive_status,
            dive::wrapped_dive_forget,
            sound::sound_status,
            sound::sound_download,
            sound::sound_cancel,
            sound::sound_check,
            home::wrapped_home,
            home::wrapped_home_memories,
            shuffle::wrapped_shuffle_start,
            shuffle::wrapped_shuffle_stop,
            shuffle::wrapped_shuffle_status,
            musicfolder::wrapped_music_folder_pick,
            musicfolder::wrapped_music_folder_clear,
            musicfolder::wrapped_music_folder_rescan,
            musicfolder::wrapped_music_folder_status,
            scrobbles::wrapped_scrobbles_status,
            scrobbles::wrapped_scrobbles_sync,
            discover::wrapped_ban_list,
            discover::wrapped_ban_add,
            discover::wrapped_ban_remove,
            judge::wrapped_llm_test,
            ollama::ollama_status,
            ollama::ollama_install,
            ollama::ollama_start,
            ollama::ollama_pull,
            ollama::ollama_cancel,
            wrapped::wrapped_settings_get,
            wrapped::wrapped_settings_update,
            tags::wrapped_artist_tags,
            tags::wrapped_tags_count,
            tags::wrapped_album_tags,
            tags::wrapped_tags_status,
            tags::wrapped_tags_fill,
            tags::wrapped_similar,
            library::library_album_facts,
            library::library_album_extras,
            library::library_recommended_artists,
            library::library_search,
            library::library_play_album,
            library::library_play_radio,
            library::library_play_artist_mix,
            library::library_save_queue,
            library::library_queue_songs,
            library::library_playlists,
            library::library_playing_album,
            library::library_playlist_from_playing,
            library::library_create_playlist,
            library::library_delete_playlist,
            library::library_play_playlist,
            library::library_queue_play_from,
            wrapper::wrapper_hole,
            wrapper::wrapper_status,
            wrapper::wrapper_attention,
            wrapper::wrapper_put_back,
            wrapper::list_fonts,
            wrapper::wrapper_roon_player,
            wrapper::wrapper_roon_link,
            wrapper::wrapper_roon_close,
            wrapper::wrapper_roon_search,
            wrapper::wrapper_roon_search_for,
            wrapper::wrapper_roon_queue,
            skin::wrapper_colors,
            roonnav::wrapper_nav
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            // Quitting: Roon gets its own window back if it's in the Wrapper
            if let tauri::RunEvent::Exit = event {
                wrapper::put_back(app);
                // (and the Ollama Wrapped started, if it's running)
                ollama::close_ours();
            }
        });
}
