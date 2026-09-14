//! Native application menu, built with `muda`.
//!
//! The menu is installed as the macOS application menu (`NSApp.mainMenu`); its `Settings…`
//! accelerator (cmd+,) is dispatched by `AppKit` and clicks arrive on `muda`'s global event channel,
//! which the app drains each frame via [`settings_requested`]. The keybind path opens the same
//! window, so the menu is an additional entry point rather than the only one.
//!
//! Other platforms fall back to the keybind only; their native menu integration is a follow-up.

#[cfg(target_os = "macos")]
mod platform_menu {
    use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc::Receiver, mpsc::SyncSender};

    use muda::{
        Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
        accelerator::{Accelerator, Code, Modifiers},
    };

    const SETTINGS_ID: &str = "bootty.settings";

    /// Holds the menu alive for the process lifetime; dropping it would tear down the menu.
    pub struct AppMenu {
        _menu: Menu,
    }

    type MenuEvents = (SyncSender<MenuEvent>, Mutex<Receiver<MenuEvent>>);
    type MenuWake = dyn Fn() + Send + Sync;
    type MenuWakes = Mutex<Vec<Weak<MenuWake>>>;
    static EVENTS: OnceLock<MenuEvents> = OnceLock::new();
    static WAKES: OnceLock<MenuWakes> = OnceLock::new();

    fn events() -> &'static MenuEvents {
        EVENTS.get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::sync_channel(16);
            (sender, Mutex::new(receiver))
        })
    }

    fn wakes() -> &'static MenuWakes {
        WAKES.get_or_init(|| Mutex::new(Vec::new()))
    }

    #[must_use]
    pub fn install(localizer: &crate::i18n::Localizer) -> Option<AppMenu> {
        let sender = events().0.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if crate::agent_tray::dispatch(&event.id.0) {
                return;
            }
            let _ = sender.try_send(event);
            if let Ok(mut wakes) = wakes().lock() {
                wakes.retain(|wake| {
                    let Some(wake) = wake.upgrade() else {
                        return false;
                    };
                    wake();
                    true
                });
            }
        }));
        let menu = Menu::new();
        let app_menu = Submenu::new("Bootty", true);
        let settings = MenuItem::with_id(
            SETTINGS_ID,
            localizer.message("menu-settings", None),
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::Comma)),
        );
        app_menu
            .append_items(&[
                &PredefinedMenuItem::about(Some(&localizer.message("menu-about", None)), None),
                &PredefinedMenuItem::separator(),
                &settings,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(Some(&localizer.message("menu-quit", None))),
            ])
            .ok()?;
        menu.append(&app_menu).ok()?;
        menu.init_for_nsapp();
        Some(AppMenu { _menu: menu })
    }

    /// Register a GPUI wake edge so a native menu click is observed even while the app is idle.
    pub fn set_wake(wake: &Arc<dyn Fn() + Send + Sync>) {
        if let Ok(mut wakes) = wakes().lock() {
            wakes.push(Arc::downgrade(wake));
            wakes.retain(|wake| wake.strong_count() > 0);
        }
    }

    /// Drain pending menu events; returns `true` if the Settings item was activated.
    #[must_use]
    pub fn settings_requested(window_active: bool) -> bool {
        if !window_active {
            return false;
        }
        let mut requested = false;
        let Ok(events) = events().1.lock() else {
            return false;
        };
        while let Ok(event) = events.try_recv() {
            if event.id == MenuId::new(SETTINGS_ID) {
                requested = true;
            }
        }
        requested
    }
}

#[cfg(not(target_os = "macos"))]
mod platform_menu {
    pub struct AppMenu;

    pub fn install(_: &crate::i18n::Localizer) -> Option<AppMenu> {
        None
    }

    pub fn set_wake(_: &std::sync::Arc<dyn Fn() + Send + Sync>) {}

    pub fn settings_requested(_: bool) -> bool {
        false
    }
}

pub use platform_menu::{AppMenu, install, set_wake, settings_requested};
