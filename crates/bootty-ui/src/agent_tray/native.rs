use super::{Snapshot, artwork};
#[cfg(target_os = "macos")]
use bootty_config::config::TrayIconStyle;
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
        let icon = Icon::from_rgba(
            artwork::icon(snapshot.config.icon_style)?,
            artwork::SIZE,
            artwork::SIZE,
        )
        .map_err(|error| error.to_string())?;
        let builder = TrayIconBuilder::new();
        #[cfg(target_os = "macos")]
        let builder = if snapshot.config.icon_style == TrayIconStyle::Monochrome {
            builder.with_icon_templated(icon)
        } else {
            builder.with_icon(icon)
        };
        #[cfg(target_os = "windows")]
        let builder = builder.with_icon(icon);
        let tray = builder
            .with_tooltip(snapshot.title())
            .with_menu(Box::new(menu(&snapshot)?))
            .with_menu_on_left_click(true)
            .build()
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        set_unread_title(&tray, &snapshot);
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
        if snapshot.config.icon_style != self.shown.config.icon_style {
            match artwork::icon(snapshot.config.icon_style).and_then(|rgba| {
                Icon::from_rgba(rgba, artwork::SIZE, artwork::SIZE)
                    .map_err(|error| error.to_string())
            }) {
                Ok(icon) => {
                    #[cfg(target_os = "macos")]
                    let result = if snapshot.config.icon_style == TrayIconStyle::Monochrome {
                        self.tray.set_icon_templated(Some(icon))
                    } else {
                        self.tray.set_icon(Some(icon))
                    };
                    #[cfg(target_os = "windows")]
                    let result = self.tray.set_icon(Some(icon));
                    if let Err(error) = result {
                        eprintln!("Agent tray icon unavailable: {error}");
                    }
                }
                Err(error) => eprintln!("Agent tray icon unavailable: {error}"),
            }
        }
        #[cfg(target_os = "macos")]
        if snapshot.unread != self.shown.unread
            || snapshot.config.macos_unread_count != self.shown.config.macos_unread_count
        {
            set_unread_title(&self.tray, &snapshot);
        }
        self.shown = snapshot;
    }
}

#[cfg(target_os = "macos")]
fn set_unread_title(tray: &TrayIcon, snapshot: &Snapshot) {
    tray.set_title(Some(
        if snapshot.config.macos_unread_count && snapshot.unread > 0 {
            snapshot.unread.to_string()
        } else {
            String::new()
        },
    ));
}
fn menu(snapshot: &Snapshot) -> Result<Menu, String> {
    let menu = Menu::new();
    for (id, label) in &snapshot.items {
        menu.append(&MenuItem::with_id(id, label, true, None))
            .map_err(|error| error.to_string())?;
    }
    Ok(menu)
}
