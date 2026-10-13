// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The tray icon and its right-click menu: Open Wrapper and Open Toaster
//! (each only while it's switched on), Settings, switches for the Toaster,
//! the Wrapper and the Taskbar Widget themselves, two quick settings
//! (Toasts, Start with Windows) and Quit. Double-clicking the icon
//! opens the Wrapper while it's switched on, otherwise the Toaster, or
//! Settings with both off. The icon is always the Roon: Toasted logo
//! (icon.rs); the Wrapper keeps its own mark for its window.
//!
//! The quick settings change the same saved settings as the Settings window
//! (through `update_settings`), and their check marks follow any change made
//! in Settings (`sync`, called by `update_settings`).

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, Wry,
};

use crate::settings::{self, AppSettings};
use crate::ToasterMode;

/// The quick settings' menu items, kept so their check marks can be updated.
pub struct TrayMenu {
    menu: Menu<Wry>,
    /// "Open Wrapper", first in the menu, only while the Wrapper is on.
    wrapper: MenuItem<Wry>,
    wrapper_shown: AtomicBool,
    /// "Open Toaster", next, only while the Toaster is on.
    toaster: MenuItem<Wry>,
    toaster_shown: AtomicBool,
    /// The switches for the Toaster and the Wrapper themselves.
    toaster_on: CheckMenuItem<Wry>,
    wrapper_on: CheckMenuItem<Wry>,
    widget_on: CheckMenuItem<Wry>,
    toasts: CheckMenuItem<Wry>,
    start_with_windows: CheckMenuItem<Wry>,
    /// "Check for Updates", or "Update to x" once one is waiting (update.rs).
    update: MenuItem<Wry>,
}

/// Creates the tray icon and its menu (in setup).
pub fn build(app: &App) -> tauri::Result<()> {
    let saved = settings::get_settings(app.handle().clone());

    let open_toaster = MenuItem::with_id(app, "open_toaster", "Open Toaster", true, None::<&str>)?;
    let open_wrapper = MenuItem::with_id(app, "open_wrapper", "Open Wrapper", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let toaster_on =
        CheckMenuItem::with_id(app, "toaster_on", "Toaster", true, saved.toaster_enabled, None::<&str>)?;
    let wrapper_on =
        CheckMenuItem::with_id(app, "wrapper_on", "Wrapper", true, saved.wrapper_enabled, None::<&str>)?;
    let widget_on =
        CheckMenuItem::with_id(app, "widget_on", "Taskbar Widget", true, saved.widget_enabled, None::<&str>)?;
    let toasts =
        CheckMenuItem::with_id(app, "toasts", "Toasts", true, saved.toasts_enabled, None::<&str>)?;
    let start_with_windows = CheckMenuItem::with_id(
        app,
        "start_with_windows",
        "Start with Windows",
        true,
        saved.start_with_windows,
        None::<&str>,
    )?;
    let update = MenuItem::with_id(app, "update", "Check for Updates", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &settings_item,
            &PredefinedMenuItem::separator(app)?,
            &toaster_on,
            &wrapper_on,
            &widget_on,
            &PredefinedMenuItem::separator(app)?,
            &toasts,
            &start_with_windows,
            &PredefinedMenuItem::separator(app)?,
            &update,
            &quit,
        ],
    )?;

    // The Roon: Toasted logo (see icon.rs)
    let icon = crate::icon::tray_icon(app.handle())
        .unwrap_or_else(|| app.default_window_icon().unwrap().clone());

    TrayIconBuilder::with_id("tray")
        .icon(icon)
        .tooltip("Roon: Toasted")
        .menu(&menu)
        // Left click shouldn't pop the menu; that's reserved for double-click.
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu)
        .on_tray_icon_event(|tray, event| {
            // Double-click opens the Wrapper while it's on, otherwise the
            // Toaster (which opens Settings when it's off too). Windows and
            // Mac only; Linux trays don't report clicks, so the menu covers
            // it there.
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                let app = tray.app_handle();
                if crate::wrapper::enabled(app) {
                    crate::wrapper::open(app);
                } else {
                    crate::open_toaster(app, ToasterMode::Player);
                }
            }
        })
        .build(app)?;

    // "Open Wrapper" goes first, then "Open Toaster" (each while it's on)
    if saved.toaster_enabled {
        menu.insert(&open_toaster, 0)?;
    }
    if saved.wrapper_enabled {
        menu.insert(&open_wrapper, 0)?;
    }

    app.manage(TrayMenu {
        menu: menu.clone(),
        wrapper: open_wrapper,
        wrapper_shown: AtomicBool::new(saved.wrapper_enabled),
        toaster: open_toaster,
        toaster_shown: AtomicBool::new(saved.toaster_enabled),
        toaster_on,
        wrapper_on,
        widget_on,
        toasts,
        start_with_windows,
        update,
    });
    Ok(())
}

/// Puts the update entry's text in line with the updater's state.
pub fn sync_update(app: &AppHandle, status: &crate::update::UpdateStatus) {
    use crate::update::UpdateState;
    let Some(menu) = app.try_state::<TrayMenu>() else { return };
    let (text, enabled) = match &status.state {
        UpdateState::Available { version, .. } => (format!("Update to {version}"), true),
        UpdateState::Checking => ("Checking for updates...".to_string(), false),
        UpdateState::Downloading { .. } => ("Downloading update...".to_string(), false),
        UpdateState::Installing { .. } => ("Installing update...".to_string(), false),
        _ => ("Check for Updates".to_string(), true),
    };
    let _ = menu.update.set_text(text);
    let _ = menu.update.set_enabled(enabled);
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let saved = settings::get_settings(app.clone());
    let change = match event.id.as_ref() {
        "open_toaster" => return crate::open_toaster(app, ToasterMode::Player),
        "settings" => return crate::show(app, crate::SETTINGS_WINDOW),
        "open_wrapper" => return crate::wrapper::open(app),
        "quit" => return app.exit(0),
        "update" => return crate::update::from_tray(app),
        "toaster_on" => json!({ "toasterEnabled": !saved.toaster_enabled }),
        "wrapper_on" => json!({ "wrapperEnabled": !saved.wrapper_enabled }),
        "widget_on" => json!({ "widgetEnabled": !saved.widget_enabled }),
        "toasts" => json!({ "toastsEnabled": !saved.toasts_enabled }),
        "start_with_windows" => json!({ "startWithWindows": !saved.start_with_windows }),
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
    let _ = menu.toaster_on.set_checked(settings.toaster_enabled);
    let _ = menu.wrapper_on.set_checked(settings.wrapper_enabled);
    let _ = menu.widget_on.set_checked(settings.widget_enabled);
    let _ = menu.toasts.set_checked(settings.toasts_enabled);
    let _ = menu.start_with_windows.set_checked(settings.start_with_windows);

    // "Open Wrapper" and "Open Toaster" come and go with their switches
    // (the Wrapper's first, so the Toaster's goes right after it)
    if menu.wrapper_shown.swap(settings.wrapper_enabled, Ordering::Relaxed) != settings.wrapper_enabled {
        let _ = if settings.wrapper_enabled {
            menu.menu.insert(&menu.wrapper, 0)
        } else {
            menu.menu.remove(&menu.wrapper)
        };
    }
    if menu.toaster_shown.swap(settings.toaster_enabled, Ordering::Relaxed) != settings.toaster_enabled {
        let _ = if settings.toaster_enabled {
            let at = usize::from(settings.wrapper_enabled);
            menu.menu.insert(&menu.toaster, at)
        } else {
            menu.menu.remove(&menu.toaster)
        };
    }
}
