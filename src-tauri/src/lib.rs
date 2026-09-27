use tauri::{
    webview::PageLoadEvent,
    AppHandle, Emitter, Manager, WebviewWindow, WindowEvent,
};

mod browse;
mod hotkeys;
mod launch;
mod monitors;
mod queue;
mod roon;
mod settings;
mod toast;
mod tray;
mod zones;

// Window labels, as set in tauri.conf.json.
/// The Toaster: the app's main screen (now playing, queue, browse/search, zones).
pub(crate) const TOASTER_WINDOW: &str = "toaster";
pub(crate) const SETTINGS_WINDOW: &str = "settings";

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

/// Puts a hidden Toaster back to its default size (from tauri.conf.json),
/// centered and not maximized. Skipped if it's already on screen, so opening
/// it again (e.g. the search hotkey) never makes an open window jump.
fn reset_toaster_size(app: &AppHandle) {
    let Some(window) = app.get_webview_window(TOASTER_WINDOW) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        return;
    }
    let default = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == TOASTER_WINDOW)
        .map(|w| (w.width, w.height));
    let _ = window.unmaximize();
    if let Some((width, height)) = default {
        let _ = window.set_size(tauri::LogicalSize::new(width, height));
    }
    let _ = window.center();
}

/// Shows the Toaster and tells its page which mode it was opened in, via the
/// `toaster-open` event (payload "player" or "search").
pub(crate) fn open_toaster(app: &AppHandle, mode: ToasterMode) {
    // "Default" window size means every opening, not just the first one.
    let saved = settings::get_settings(app.clone());
    if saved.toaster_window == settings::ToasterWindow::Default {
        reset_toaster_size(app);
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
    if !settings::get_settings(app.clone()).hide_on_blur {
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
        // Settings always opens centered and the toast places itself (toast.rs),
        // so neither is tracked.
        builder = builder.plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(WINDOW_STATE)
                .with_denylist(&[SETTINGS_WINDOW, toast::TOAST_WINDOW])
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
        .plugin(tauri_plugin_opener::init())
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
        .setup(|app| {
            // Tray icon and its right-click menu (see tray.rs)
            tray::build(app)?;

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

            // roon-toasted:// links, and opening the Toaster if a link or
            // --search started the app (see launch.rs)
            launch::startup(app.handle());

            // Connect the zone state to the app, then connect to Roon in the background.
            app.state::<zones::Zones>().init(app.handle());
            app.state::<queue::Queue>().init(app.handle());
            roon::start(app.handle());

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
            launch::take_startup_mode
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
