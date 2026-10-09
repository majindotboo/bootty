//! Native application menu, owned by GPUI.
//!
//! Edit actions use GPUI's typed input actions so native responder selectors and focused GPUI
//! inputs share one dispatch path. Muda remains the event source for the agent tray.

#[cfg(target_os = "macos")]
mod platform_menu {
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    use gpui_kit::{Menu, MenuItem, OsAction};
    use muda::MenuEvent;

    const SETTINGS_COMMAND: &str = "open_settings";
    const QUIT_COMMAND: &str = "quit";

    #[derive(Clone, Debug, PartialEq, Eq, gpui_kit::Action)]
    #[action(namespace = bootty_menu, no_json)]
    pub struct About;

    /// Keeps the app menu installation in the native host's lifetime slot.
    pub struct AppMenu;

    #[must_use]
    pub fn install(localizer: &crate::i18n::Localizer, cx: &mut gpui_kit::App) -> AppMenu {
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            crate::agent_tray::dispatch(&event.id.0);
        }));
        refresh(localizer, cx);
        AppMenu
    }

    pub fn refresh(localizer: &crate::i18n::Localizer, cx: &gpui_kit::App) {
        let command = |name| {
            crate::gpui_actions::InvokeCommand::new(bootty_control::CommandInvocation::from_action(
                name,
                bootty_control::Caller::Keybinding,
            ))
        };
        cx.set_menus([
            Menu::new("Bootty").items([
                MenuItem::action(localizer.message("menu-about", None), About),
                MenuItem::separator(),
                MenuItem::action(
                    localizer.message("menu-settings", None),
                    command(SETTINGS_COMMAND),
                ),
                MenuItem::separator(),
                MenuItem::action(localizer.message("menu-quit", None), command(QUIT_COMMAND)),
            ]),
            Menu::new(localizer.text("menu-file", "File")).items([MenuItem::action(
                localizer.text("menu-new-session", "New Session"),
                command("new_mux_session"),
            )]),
            Menu::new(localizer.text("menu-edit", "Edit")).items([
                MenuItem::os_action(localizer.text("menu-undo", "Undo"), Undo, OsAction::Undo),
                MenuItem::os_action(localizer.text("menu-redo", "Redo"), Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action(localizer.text("menu-cut", "Cut"), Cut, OsAction::Cut),
                MenuItem::os_action(localizer.text("menu-copy", "Copy"), Copy, OsAction::Copy),
                MenuItem::os_action(
                    localizer.text("menu-paste", "Paste"),
                    Paste,
                    OsAction::Paste,
                ),
                MenuItem::separator(),
                MenuItem::os_action(
                    localizer.text("menu-select-all", "Select All"),
                    SelectAll,
                    OsAction::SelectAll,
                ),
            ]),
        ]);
    }
}

#[cfg(not(target_os = "macos"))]
mod platform_menu {
    pub struct AppMenu;

    #[must_use]
    pub const fn install(_: &crate::i18n::Localizer, _: &mut gpui_kit::App) -> AppMenu {
        AppMenu
    }
}

#[cfg(target_os = "macos")]
pub use platform_menu::{About, refresh};
pub use platform_menu::{AppMenu, install};
