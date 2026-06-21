//! Shared geometry and assets for the web terminal site.

pub(crate) const CELL_WIDTH: u32 = 9;
pub(crate) const CELL_HEIGHT: u32 = 18;
pub(crate) const DEFAULT_COLS: u16 = 96;
pub(crate) const DEFAULT_ROWS: u16 = 32;
pub(crate) const ICON_TEXTURE_SIZE: u32 = 96;
pub(crate) const ICON_RENDER_SIZE: u32 = 48;
pub(crate) const ICON_PNG: &[u8] = include_bytes!("../assets/bootty-mascot.png");
pub(crate) const EGUI_SIDEBAR_TOP_PX: f32 = 18.0;
pub(crate) const EGUI_SIDEBAR_ROW_HEIGHT_PX: f32 = 36.0;
pub(crate) const EGUI_SIDEBAR_WIDTH_COLS: u16 = 32;
pub(crate) const EGUI_HEADER_ROWS: u16 = 4;
pub(crate) const EGUI_FOOTER_ROWS: u16 = 2;
