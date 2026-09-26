use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};

mod roon;

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

/// Lets a page open another window, e.g. the Toaster's settings cog
/// (`invoke("open_window", { window: "settings" })`) or an "Open Toaster"
/// button in Settings. Only the app's own windows are allowed.
#[tauri::command]
fn open_window(app: AppHandle, window: String) -> Result<(), String> {
    match window.as_str() {
        TOASTER_WINDOW | SETTINGS_WINDOW => {
            show(&app, &window);
            Ok(())
        }
        _ => Err(format!("Unknown window: {window}")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Dev builds: print warnings from libraries (like roon-api) to the terminal.
    #[cfg(debug_assertions)]
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::WARN)
        .try_init();

    let mut builder = tauri::Builder::default();

    // Only one copy of the app may run at a time. Launching it again just
    // opens the Toaster of the copy that's already running.
    // This has to be the first plugin registered.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show(app, TOASTER_WINDOW);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Tray right-click menu
            let open_toaster =
                MenuItem::with_id(app, "open_toaster", "Open Toaster", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_toaster, &settings, &separator, &quit])?;

            TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Roon: Toasted")
                .menu(&menu)
                // Left click shouldn't pop the menu; that's reserved for double-click.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open_toaster" => show(app, TOASTER_WINDOW),
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
                        show(tray.app_handle(), TOASTER_WINDOW);
                    }
                })
                .build(app)?;

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
            roon::roon_status,
            roon::switch_core
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
