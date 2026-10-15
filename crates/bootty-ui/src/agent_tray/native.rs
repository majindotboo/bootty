use super::Snapshot;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuItem},
};
pub(super) struct Backend {
    tray: TrayIcon,
    shown: Snapshot,
}
impl Backend {
    pub(super) fn new(snapshot: Snapshot) -> Result<Self, String> {
        // macOS shares muda with the application menu; its one handler forwards tray IDs.
        #[cfg(target_os = "windows")]
        tray_icon::menu::MenuEvent::set_event_handler(Some(|event: tray_icon::menu::MenuEvent| {
            super::dispatch(&event.id.0);
        }));
        let tray = TrayIconBuilder::new()
            .with_icon(
                Icon::from_rgba(super::icon(snapshot.unread > 0), 16, 16)
                    .map_err(|error| error.to_string())?,
            )
            .with_tooltip(snapshot.title())
            .with_menu(Box::new(menu(&snapshot)?))
            .with_menu_on_left_click(true)
            .build()
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        set_unread_title(&tray, snapshot.unread);
        Ok(Self {
            tray,
            shown: snapshot,
        })
    }
    /// Touch only what changed; every native status item mutation costs `AppKit` work.
    pub(super) fn update(&mut self, snapshot: Snapshot) {
        if snapshot == self.shown {
            return;
        }
        if snapshot.items != self.shown.items {
            match menu(&snapshot) {
                Ok(menu) => self.tray.set_menu(Some(Box::new(menu))),
                Err(error) => eprintln!("Agent tray menu unavailable: {error}"),
            }
        }
        if snapshot.title() != self.shown.title() {
            let _ = self.tray.set_tooltip(Some(snapshot.title()));
        }
        if (snapshot.unread > 0) != (self.shown.unread > 0)
            && let Ok(icon) = Icon::from_rgba(super::icon(snapshot.unread > 0), 16, 16)
        {
            let _ = self.tray.set_icon(Some(icon));
        }
        #[cfg(target_os = "macos")]
        if snapshot.unread != self.shown.unread {
            set_unread_title(&self.tray, snapshot.unread);
        }
        self.shown = snapshot;
    }
}

#[cfg(target_os = "macos")]
fn set_unread_title(tray: &TrayIcon, unread: usize) {
    tray.set_title(Some(if unread > 0 {
        unread.to_string()
    } else {
        String::new()
    }));
}
fn menu(snapshot: &Snapshot) -> Result<Menu, String> {
    let menu = Menu::new();
    for (id, label) in &snapshot.items {
        menu.append(&MenuItem::with_id(id, label, true, None))
            .map_err(|error| error.to_string())?;
    }
    Ok(menu)
}
