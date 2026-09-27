//! The tray icon and its right-click menu: Open Toaster, Settings, a few
//! quick settings (Toasts, Auto-Hide, Remember Window) and Quit. Double-clicking
//! the icon opens the Toaster.
//!
//! The quick settings change the same saved settings as the Settings window
//! (through `update_settings`), and their check marks follow any change made
//! in Settings (`sync`, called by `update_settings`).

use serde_json::json;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, Wry,
};

use crate::settings::{self, AppSettings, ToasterWindow};
use crate::ToasterMode;

/// The quick settings' menu items, kept so their check marks can be updated.
pub struct TrayMenu {
    toasts: CheckMenuItem<Wry>,
    auto_hide: CheckMenuItem<Wry>,
    /// Settings' Window Size: checked = Last Position, unchecked = Default.
    remember_window: CheckMenuItem<Wry>,
}

/// Creates the tray icon and its menu (in setup).
pub fn build(app: &App) -> tauri::Result<()> {
    let saved = settings::get_settings(app.handle().clone());

    let open_toaster = MenuItem::with_id(app, "open_toaster", "Open Toaster", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let toasts =
        CheckMenuItem::with_id(app, "toasts", "Toasts", true, saved.toasts_enabled, None::<&str>)?;
    let auto_hide =
        CheckMenuItem::with_id(app, "auto_hide", "Auto-Hide", true, saved.hide_on_blur, None::<&str>)?;
    let remember = saved.toaster_window == ToasterWindow::Remember;
    let remember_window = CheckMenuItem::with_id(
        app,
        "remember_window",
        "Remember Window",
        true,
        remember,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open_toaster,
            &settings_item,
            &PredefinedMenuItem::separator(app)?,
            &toasts,
            &auto_hide,
            &remember_window,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    TrayIconBuilder::with_id("tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Roon: Toasted")
        .menu(&menu)
        // Left click shouldn't pop the menu; that's reserved for double-click.
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu)
        .on_tray_icon_event(|tray, event| {
            // Double-click opens the Toaster. Windows and Mac only; Linux trays
            // don't report clicks, so the menu's "Open Toaster" covers it there.
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                crate::open_toaster(tray.app_handle(), ToasterMode::Player);
            }
        })
        .build(app)?;

    app.manage(TrayMenu {
        toasts,
        auto_hide,
        remember_window,
    });
    Ok(())
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let saved = settings::get_settings(app.clone());
    let change = match event.id.as_ref() {
        "open_toaster" => return crate::open_toaster(app, ToasterMode::Player),
        "settings" => return crate::show(app, crate::SETTINGS_WINDOW),
        "quit" => return app.exit(0),
        "toasts" => json!({ "toastsEnabled": !saved.toasts_enabled }),
        "auto_hide" => json!({ "hideOnBlur": !saved.hide_on_blur }),
        "remember_window" => {
            let remember = saved.toaster_window == ToasterWindow::Remember;
            json!({ "toasterWindow": if remember { "default" } else { "remember" } })
        }
        _ => return,
    };
    // Saves, tells Settings, and puts the check marks right (Windows also
    // flips a check mark by itself when clicked)
    if settings::update_settings(app.clone(), change).is_err() {
        sync(app, &saved);
    }
}

/// Sets the check marks to match the settings.
pub fn sync(app: &AppHandle, settings: &AppSettings) {
    let Some(menu) = app.try_state::<TrayMenu>() else {
        return;
    };
    let _ = menu.toasts.set_checked(settings.toasts_enabled);
    let _ = menu.auto_hide.set_checked(settings.hide_on_blur);
    let _ = menu
        .remember_window
        .set_checked(settings.toaster_window == ToasterWindow::Remember);
}
