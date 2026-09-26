use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};

mod roon;

// Label of the settings window, as set in tauri.conf.json.
const SETTINGS_WINDOW: &str = "settings";

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Brings the settings window to the front, un-hiding and un-minimizing it if needed.
fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
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
    // opens the settings window of the copy that's already running.
    // This has to be the first plugin registered.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_settings(app);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Tray right-click menu
            let open_settings =
                MenuItem::with_id(app, "open_settings", "Open Settings", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_settings, &separator, &quit])?;

            TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Roon: Toasted")
                .menu(&menu)
                // Left click shouldn't pop the menu; that's reserved for double-click.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open_settings" => show_settings(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Double-click opens settings. Windows and Mac only; Linux trays
                    // don't report clicks, so the menu's "Open Settings" covers it there.
                    if let TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        show_settings(tray.app_handle());
                    }
                })
                .build(app)?;

            // Connect to Roon in the background.
            roon::start(app.handle());

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window hides it instead of quitting.
            // The app keeps running in the tray until "Quit" is chosen.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == SETTINGS_WINDOW {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
