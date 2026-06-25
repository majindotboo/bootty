//! A filterable, keyboard-and-mouse navigable list rendered inside a scroll
//! area. Rows carry a leading icon, a primary label, and an optional trailing
//! label; the framework owns navigation, selection highlight, and scrolling.

use bootty_ui::ThemePalette;
use eframe::egui::{self, Align, Color32, FontId, Pos2, Rect, Sense};

use crate::ui::icons;

const ROW_HEIGHT: f32 = 30.0;

/// One row of a [`ListView`].
#[derive(Clone, Debug, Default)]
pub struct ListRow {
    /// Leading icon slug (resolved through `ui::icons`); omitted if `None`.
    pub icon: Option<String>,
    /// Override the icon tint; defaults to the row's text color.
    pub icon_tint: Option<Color32>,
    /// Primary label.
    pub primary: String,
    /// Override the primary label color.
    pub primary_tint: Option<Color32>,
    /// Optional dim description rendered under the primary label (needs a taller
    /// [`ListView::row_height`]).
    pub secondary: Option<String>,
    /// Optional right-aligned secondary label.
    pub trailing: Option<String>,
    /// Marks the active/current entry: accent bar + primary tint.
    pub current: bool,
}

/// What a [`ListView`] produced for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListOutcome {
    /// Selection index after this frame's keyboard navigation.
    pub selected: usize,
    /// Row activated this frame by Enter or a primary click, if any.
    pub activated: Option<usize>,
}

/// A scrollable, selectable list. Construct per frame from the rows the caller
/// already filtered, render with [`ListView::show`], and feed `selected` back.
pub struct ListView<'a> {
    id_salt: egui::Id,
    rows: &'a [ListRow],
    selected: usize,
    max_height: f32,
    row_height: f32,
    empty_text: &'a str,
}

impl<'a> ListView<'a> {
    pub fn new(id_salt: impl std::hash::Hash, rows: &'a [ListRow], selected: usize) -> Self {
        Self {
            id_salt: egui::Id::new(id_salt),
            rows,
            selected,
            max_height: f32::INFINITY,
            row_height: ROW_HEIGHT,
            empty_text: "no matches",
        }
    }

    /// Cap the scroll viewport so long lists scroll instead of growing the panel.
    #[must_use]
    pub fn max_height(mut self, max_height: f32) -> Self {
        self.max_height = max_height;
        self
    }

    /// Taller rows, e.g. to fit a [`ListRow::secondary`] description line.
    #[must_use]
    pub fn row_height(mut self, row_height: f32) -> Self {
        self.row_height = row_height;
        self
    }

    #[must_use]
    pub fn empty_text(mut self, text: &'a str) -> Self {
        self.empty_text = text;
        self
    }

    pub fn show(self, ui: &mut egui::Ui, palette: ThemePalette) -> ListOutcome {
        let (next, previous, enter) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowDown)
                    || (input.key_pressed(egui::Key::N) && input.modifiers.ctrl),
                input.key_pressed(egui::Key::ArrowUp)
                    || (input.key_pressed(egui::Key::P) && input.modifiers.ctrl),
                input.key_pressed(egui::Key::Enter),
            )
        });
        let selected = selection_after_nav(self.selected, self.rows.len(), next, previous);

        if self.rows.is_empty() {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ROW_HEIGHT * 2.0),
                Sense::hover(),
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                self.empty_text,
                FontId::monospace(13.0),
                palette.muted,
            );
            return ListOutcome {
                selected: 0,
                activated: None,
            };
        }

        let navigated = next || previous;
        let mut activated = enter.then_some(selected);

        // Reserve a definite height (content, capped at max_height) instead of
        // letting the ScrollArea negotiate with the auto-sizing floating panel —
        // that negotiation collapses the viewport to a couple of rows.
        let view_height = (self.rows.len() as f32 * self.row_height).min(self.max_height);
        ui.allocate_ui(egui::vec2(ui.available_width(), view_height), |ui| {
            egui::ScrollArea::vertical()
                .id_salt(self.id_salt)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let width = ui.available_width();
                    for (index, row) in self.rows.iter().enumerate() {
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(width, self.row_height),
                            Sense::click(),
                        );
                        paint_row(ui.painter(), rect, palette, row, index == selected);
                        if response.clicked() {
                            activated = Some(index);
                        }
                        // Keep the cursor visible only when navigation moved it, so a
                        // resting selection doesn't fight a user's manual scroll.
                        if index == selected && navigated {
                            response.scroll_to_me(Some(Align::Center));
                        }
                    }
                });
        });

        ListOutcome {
            selected,
            activated,
        }
    }
}

fn paint_row(
    painter: &egui::Painter,
    rect: Rect,
    palette: ThemePalette,
    row: &ListRow,
    selected: bool,
) {
    let background = if selected {
        Some(palette.hover)
    } else if row.current {
        Some(palette.surface)
    } else {
        None
    };
    if let Some(background) = background {
        painter.rect_filled(rect, 0.0, background);
    }
    if row.current {
        painter.rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            palette.primary,
        );
    }

    let text_color = if row.current {
        palette.primary
    } else {
        palette.text
    };
    let mut x = rect.left() + 14.0;
    if let Some(slug) = &row.icon {
        let tint = row.icon_tint.unwrap_or(if selected || row.current {
            text_color
        } else {
            palette.muted
        });
        if icons::paint_icon_slug(
            painter,
            slug,
            Pos2::new(x + 8.0, rect.center().y),
            15.0,
            tint,
        ) {
            x += 26.0;
        }
    }
    // With a description, stack the primary above it; otherwise center the primary.
    let primary_y = match row.secondary {
        Some(_) => rect.top() + rect.height() * 0.34,
        None => rect.center().y,
    };
    painter.text(
        Pos2::new(x, primary_y),
        egui::Align2::LEFT_CENTER,
        &row.primary,
        FontId::monospace(13.0),
        row.primary_tint.unwrap_or(text_color),
    );
    if let Some(secondary) = &row.secondary {
        painter.text(
            Pos2::new(x, rect.top() + rect.height() * 0.70),
            egui::Align2::LEFT_CENTER,
            secondary,
            FontId::monospace(11.0),
            palette.muted,
        );
    }
    if let Some(trailing) = &row.trailing {
        painter.text(
            Pos2::new(rect.right() - 14.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            trailing,
            FontId::monospace(12.0),
            palette.muted,
        );
    }
}

/// Move the selection one row for a down/up press, clamped to the list (no wrap).
#[must_use]
pub fn selection_after_nav(selected: usize, len: usize, next: bool, previous: bool) -> usize {
    if len == 0 {
        return 0;
    }
    let mut selected = selected.min(len - 1);
    if next {
        selected = (selected + 1).min(len - 1);
    }
    if previous {
        selected = selected.saturating_sub(1);
    }
    selected
}

/// Keep a stored selection in range after the row set shrinks.
#[must_use]
pub fn clamp_selection(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { selected.min(len - 1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_moves_within_bounds_without_wrapping() {
        assert_eq!(selection_after_nav(0, 3, true, false), 1);
        assert_eq!(selection_after_nav(2, 3, true, false), 2);
        assert_eq!(selection_after_nav(2, 3, false, true), 1);
        assert_eq!(selection_after_nav(0, 3, false, true), 0);
        assert_eq!(selection_after_nav(5, 0, true, true), 0);
    }

    #[test]
    fn clamp_keeps_selection_in_range() {
        assert_eq!(clamp_selection(0, 0), 0);
        assert_eq!(clamp_selection(9, 3), 2);
        assert_eq!(clamp_selection(1, 3), 1);
    }
}
