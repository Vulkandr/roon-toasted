use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewWindow, WindowEvent,
};

mod roon;
mod settings;
mod zones;

// Window labels, as set in tauri.conf.json.
/// The Toaster: the app's main screen (now playing, queue, browse/search, zones).
const TOASTER_WINDOW: &str = "toaster";
const SETTINGS_WINDOW: &str = "settings";

/// Brings a window to the front, un-hiding and un-minimizing it if needed.
fn show(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// How the Toaster was opened. Every way of opening it (tray, launching the
/// app again, and later hotkeys and the Stream Deck) goes through here.
#[derive(Clone, Copy)]
enum ToasterMode {
    /// Open as the player, as it was last left.
    Player,
    /// Open straight to Browse with the search box focused, ready to type.
    Search,
}

/// Shows the Toaster and tells its page which mode it was opened in, via the
/// `toaster-open` event (payload "player" or "search").
fn open_toaster(app: &AppHandle, mode: ToasterMode) {
    show(app, TOASTER_WINDOW);
    let mode = match mode {
        ToasterMode::Player => "player",
        ToasterMode::Search => "search",
    };
    let _ = app.emit_to(TOASTER_WINDOW, "toaster-open", mode);
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

/// Hides the window that calls it, e.g. when Escape is pressed.
#[tauri::command]
fn hide_window(window: WebviewWindow) {
    let _ = window.hide();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Dev builds: print warnings from libraries (like roon-api) to the terminal.
    #[cfg(debug_assertions)]
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::WARN)
        .try_init();

    let mut builder = tauri::Builder::default();

    // Only one copy of the app may run at a time. Launching it again opens the
    // Toaster of the copy that's already running: in search mode when launched
    // with --search (or a roon-toasted://search link, once that's registered),
    // otherwise as the player.
    // This has to be the first plugin registered.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let search = args
                .iter()
                .any(|a| a == "--search" || a.starts_with("roon-toasted://search"));
            let mode = if search {
                ToasterMode::Search
            } else {
                ToasterMode::Player
            };
            open_toaster(app, mode);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        // Album art for the pages, fetched from the Core (see zones.rs).
        .register_asynchronous_uri_scheme_protocol("roonimg", |ctx, request, responder| {
            zones::image_request(ctx.app_handle(), request, responder)
        })
        .setup(|app| {
            // Tray right-click menu
            let open_toaster_item =
                MenuItem::with_id(app, "open_toaster", "Open Toaster", true, None::<&str>)?;
            let settings_item =
                MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[&open_toaster_item, &settings_item, &separator, &quit],
            )?;

            TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Roon: Toasted")
                .menu(&menu)
                // Left click shouldn't pop the menu; that's reserved for double-click.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open_toaster" => open_toaster(app, ToasterMode::Player),
                    "settings" => show(app, SETTINGS_WINDOW),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Double-click opens the Toaster. Windows and Mac only; Linux trays
                    // don't report clicks, so the menu's "Open Toaster" covers it there.
                    if let TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        open_toaster(tray.app_handle(), ToasterMode::Player);
                    }
                })
                .build(app)?;

            // Zone state has to exist before the Roon connection starts feeding it.
            app.manage(zones::Zones::new(app.handle()));
            // Connect to Roon in the background.
            roon::start(app.handle());

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing any window hides it instead of quitting.
            // The app keeps running in the tray until "Quit" is chosen.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            open_window,
            hide_window,
            roon::roon_status,
            roon::switch_core,
            zones::roon_zones,
            zones::roon_select_zone,
            zones::roon_control,
            zones::roon_seek,
            zones::roon_zone_settings,
            zones::roon_set_volume,
            zones::roon_mute
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
