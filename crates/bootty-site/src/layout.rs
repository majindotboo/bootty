//! Cell-space layout for the legacy egui shell and page scrolling.

use tuirealm::ratatui::layout::{Constraint, Direction as LayoutDirection, Layout, Rect};

use crate::constants::{
    CELL_HEIGHT, EGUI_FOOTER_ROWS, EGUI_HEADER_ROWS, EGUI_SIDEBAR_ROW_HEIGHT_PX,
    EGUI_SIDEBAR_TOP_PX, EGUI_SIDEBAR_WIDTH_COLS,
};
use crate::content::sections;

pub(crate) fn site_layout(cols: u16, rows: u16) -> SiteLayout {
    let area = Rect::new(0, 0, cols, rows);
    let [header, body, footer] = Layout::default()
        .direction(LayoutDirection::Vertical)
        .constraints([
            Constraint::Length(EGUI_HEADER_ROWS),
            Constraint::Min(8),
            Constraint::Length(EGUI_FOOTER_ROWS),
        ])
        .areas(area);
    let narrow = body.width < 78;
    let menu_height = egui_sidebar_rows()
        .min(body.height.saturating_sub(8))
        .max(egui_sidebar_rows());
    let [menu, _detail] = Layout::default()
        .direction(if narrow {
            LayoutDirection::Vertical
        } else {
            LayoutDirection::Horizontal
        })
        .constraints(if narrow {
            [Constraint::Length(menu_height), Constraint::Min(8)]
        } else {
            [
                Constraint::Length(EGUI_SIDEBAR_WIDTH_COLS),
                Constraint::Min(24),
            ]
        })
        .areas(body);

    SiteLayout {
        header,
        menu,
        footer,
    }
}

pub(crate) fn egui_sidebar_rows() -> u16 {
    ((EGUI_SIDEBAR_TOP_PX + sections().len() as f32 * EGUI_SIDEBAR_ROW_HEIGHT_PX)
        / CELL_HEIGHT as f32)
        .ceil() as u16
}

pub(crate) fn max_scroll(line_count: usize, area_height: u16) -> u16 {
    let content_height = area_height as usize;
    line_count.saturating_sub(content_height) as u16
}

#[derive(Clone, Copy)]
pub(crate) struct SiteLayout {
    pub(crate) header: Rect,
    pub(crate) menu: Rect,
    pub(crate) footer: Rect,
}
