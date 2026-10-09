//! GPUI presentation contract for Bootty settings.
//!
//! The window follows Zed's settings navigation and row structure while Bootty's existing
//! settings owner remains authoritative for drafts, validation, persistence, and side effects.

mod components;
mod environment;
mod font_features;
mod inline_inputs;
mod model;
mod number_input;
mod picker;
mod provider_models;
mod search;
mod status_segments;
mod text_input;
mod theme_colors;
mod theme_editor;
mod title_bar;
mod window;

pub use components::{SETTINGS_SIDEBAR_WIDTH_PX, SETTINGS_SIDEBAR_WIDTH_REMS};
pub use font_features::*;
pub use model::*;
pub use theme_editor::GpuiThemeEditor;
pub use title_bar::SettingsTitleBar;
pub use window::{GpuiSettings, ToggleFocusNav};
