use super::Snapshot;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuItem},
};
pub(super) struct Backend(TrayIcon);
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
        let backend = Self(tray);
        backend.update(snapshot);
        Ok(backend)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "The shared platform API transfers snapshots to the Linux DBus worker."
    )]
    pub(super) fn update(&self, snapshot: Snapshot) {
        match menu(&snapshot) {
            Ok(menu) => self.0.set_menu(Some(Box::new(menu))),
            Err(error) => eprintln!("Agent tray menu unavailable: {error}"),
        }
        let _ = self.0.set_tooltip(Some(snapshot.title()));
        if let Ok(icon) = Icon::from_rgba(super::icon(snapshot.unread > 0), 16, 16) {
            let _ = self.0.set_icon(Some(icon));
        }
        #[cfg(target_os = "macos")]
        self.0.set_title(Some(if snapshot.unread > 0 {
            snapshot.unread.to_string()
        } else {
            String::new()
        }));
    }
}
fn menu(snapshot: &Snapshot) -> Result<Menu, String> {
    let menu = Menu::new();
    for (id, label) in &snapshot.items {
        menu.append(&MenuItem::with_id(id, label, true, None))
            .map_err(|error| error.to_string())?;
    }
    Ok(menu)
}
