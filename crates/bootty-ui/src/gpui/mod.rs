//! GPUI-native Bootty presentation primitives.

pub mod chrome;
mod color_picker;
mod config_editor;
mod dialogs;
pub mod direct_input;
mod icons;
mod input;
mod keybinding;
mod keymap_editor;
mod overlay_host;
mod panes;
mod settings;
mod space_editor;
pub(crate) mod tabs;
mod terminal;
mod terminal_zoom;
mod theme;
mod theme_integration;

pub use chrome::*;
pub use config_editor::*;
pub use dialogs::*;
pub use icons::*;
pub use input::*;
pub use keybinding::*;
pub use keymap_editor::*;
pub use overlay_host::*;
pub use panes::*;
pub use settings::*;
pub use space_editor::*;
pub use terminal::*;
pub use terminal_zoom::*;
pub use theme::*;
pub use theme_integration::*;
