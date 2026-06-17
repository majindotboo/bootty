//! Native application menu, built with `muda`.
//!
//! The menu is installed as the macOS application menu (`NSApp.mainMenu`); its `Settings…`
//! accelerator (cmd+,) is dispatched by AppKit and clicks arrive on `muda`'s global event channel,
//! which the app drains each frame via [`settings_requested`]. The keybind path opens the same
//! window, so the menu is an additional entry point rather than the only one.
//!
//! Other platforms fall back to the keybind only: attaching `muda` to a winit/eframe window needs
//! a raw window handle that eframe does not expose, so the menu bar there is a follow-up.

#[cfg(target_os = "macos")]
mod platform_menu {
    use muda::{
        Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
        accelerator::{Accelerator, Code, Modifiers},
    };

    const SETTINGS_ID: &str = "bootty.settings";

    /// Holds the menu alive for the process lifetime; dropping it would tear down the menu.
    pub struct AppMenu {
        _menu: Menu,
    }

    pub fn install() -> Option<AppMenu> {
        let menu = Menu::new();
        let app_menu = Submenu::new("Bootty", true);
        let settings = MenuItem::with_id(
            SETTINGS_ID,
            "Settings…",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::Comma)),
        );
        app_menu
            .append_items(&[
                &PredefinedMenuItem::about(Some("About Bootty"), None),
                &PredefinedMenuItem::separator(),
                &settings,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(Some("Quit Bootty")),
            ])
            .ok()?;
        menu.append(&app_menu).ok()?;
        menu.init_for_nsapp();
        Some(AppMenu { _menu: menu })
    }

    /// Drain pending menu events; returns `true` if the Settings item was activated.
    pub fn settings_requested() -> bool {
        let mut requested = false;
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == SETTINGS_ID {
                requested = true;
            }
        }
        requested
    }
}

#[cfg(not(target_os = "macos"))]
mod platform_menu {
    pub struct AppMenu;

    pub fn install() -> Option<AppMenu> {
        None
    }

    pub fn settings_requested() -> bool {
        false
    }
}

pub use platform_menu::{AppMenu, install, settings_requested};
