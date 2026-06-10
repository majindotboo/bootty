use bootty_ui::{Theme, ThemePalette};
use eframe::egui::{self, CornerRadius, Pos2, Rect, Stroke, UiBuilder};

use crate::mux::snapshot::MuxSession;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionPickerDialog {
    filter: String,
    selected: usize,
    focus_filter: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionPickerEvent {
    None,
    Close,
    ActivateSession(String),
}

impl SessionPickerDialog {
    pub fn open() -> Self {
        Self {
            filter: String::new(),
            selected: 0,
            focus_filter: true,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
        sessions: &[MuxSession],
        selected_session: Option<&str>,
    ) -> SessionPickerEvent {
        let palette = theme.palette;
        let matches = filtered_session_indices(sessions, &self.filter);
        self.clamp_selection(matches.len());
        let mut event = SessionPickerEvent::None;

        egui::Area::new(egui::Id::new("session-picker-dialog"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                bootty_ui::configure_style(ui.style_mut(), theme);
                let size = picker_panel_size(ctx);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, CornerRadius::ZERO, palette.mantle);
                painter.rect_stroke(
                    rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, palette.border),
                    egui::StrokeKind::Inside,
                );

                let header = Rect::from_min_size(rect.min, egui::vec2(rect.width(), 52.0));
                painter.text(
                    Pos2::new(header.min.x + 16.0, header.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Session Finder",
                    egui::FontId::monospace(15.0),
                    palette.warning,
                );
                painter.text(
                    Pos2::new(header.max.x - 16.0, header.center().y),
                    egui::Align2::RIGHT_CENTER,
                    "Enter select  Esc close",
                    egui::FontId::monospace(12.0),
                    palette.muted,
                );
                painter.line_segment(
                    [
                        Pos2::new(rect.min.x + 16.0, header.max.y),
                        Pos2::new(rect.max.x - 16.0, header.max.y),
                    ],
                    Stroke::new(1.0, palette.border),
                );

                let filter_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x + 16.0, header.max.y + 10.0),
                    Pos2::new(rect.max.x - 16.0, header.max.y + 36.0),
                );
                self.draw_filter(ctx, ui, filter_rect, theme, palette);

                let footer_height = 30.0;
                let list_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x + 16.0, filter_rect.max.y + 8.0),
                    Pos2::new(rect.max.x - 16.0, rect.max.y - footer_height - 12.0),
                );
                self.handle_row_navigation(ui, matches.len());
                if ui.input(|input| input.key_pressed(egui::Key::Enter))
                    && let Some(index) = matches.get(self.selected)
                    && let Some(session) = sessions.get(*index)
                {
                    event = SessionPickerEvent::ActivateSession(session.id.clone());
                }
                if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                    event = SessionPickerEvent::Close;
                }
                if let Some(session_id) = draw_session_picker_rows(
                    ui,
                    list_rect,
                    palette,
                    sessions,
                    &matches,
                    self.selected,
                    selected_session,
                ) {
                    event = SessionPickerEvent::ActivateSession(session_id);
                }

                let footer = Rect::from_min_max(
                    Pos2::new(rect.min.x + 16.0, rect.max.y - footer_height),
                    Pos2::new(rect.max.x - 16.0, rect.max.y),
                );
                painter.line_segment(
                    [
                        Pos2::new(footer.min.x, footer.min.y),
                        Pos2::new(footer.max.x, footer.min.y),
                    ],
                    Stroke::new(1.0, palette.border),
                );
                painter.text(
                    Pos2::new(footer.min.x, footer.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{} / {} sessions", matches.len(), sessions.len()),
                    egui::FontId::monospace(12.0),
                    palette.muted,
                );
            });

        event
    }

    fn draw_filter(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        rect: Rect,
        theme: Theme,
        palette: ThemePalette,
    ) {
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                let filter_id = egui::Id::new("session-picker-filter");
                let response =
                    bootty_ui::flat_text_edit_singleline(ui, &mut self.filter, theme, |edit| {
                        edit.id(filter_id)
                            .desired_width(f32::INFINITY)
                            .hint_text("filter sessions...")
                    });
                if self.focus_filter {
                    response.request_focus();
                    self.focus_filter = false;
                }
            },
        );
        let focus_color =
            if ctx.memory(|memory| memory.has_focus(egui::Id::new("session-picker-filter"))) {
                palette.accent
            } else {
                palette.border
            };
        ui.painter().line_segment(
            [
                Pos2::new(rect.min.x, rect.max.y - 2.0),
                Pos2::new(rect.max.x, rect.max.y - 2.0),
            ],
            Stroke::new(1.0, focus_color),
        );
    }

    fn handle_row_navigation(&mut self, ui: &egui::Ui, row_count: usize) {
        if row_count == 0 {
            self.selected = 0;
            return;
        }
        let (next, previous) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowDown)
                    || input.key_pressed(egui::Key::N) && input.modifiers.ctrl,
                input.key_pressed(egui::Key::ArrowUp)
                    || input.key_pressed(egui::Key::P) && input.modifiers.ctrl,
            )
        });
        if next && self.selected + 1 < row_count {
            self.selected += 1;
        }
        if previous && self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn clamp_selection(&mut self, row_count: usize) {
        if row_count == 0 {
            self.selected = 0;
        } else if self.selected >= row_count {
            self.selected = row_count - 1;
        }
    }
}

fn picker_panel_size(ctx: &egui::Context) -> egui::Vec2 {
    let viewport = ctx.input(|input| input.content_rect().size());
    egui::vec2(
        780.0_f32.min((viewport.x - 72.0).max(520.0)),
        520.0_f32.min((viewport.y - 96.0).max(320.0)),
    )
}

fn draw_session_picker_rows(
    ui: &egui::Ui,
    rect: Rect,
    palette: ThemePalette,
    sessions: &[MuxSession],
    matches: &[usize],
    selected: usize,
    selected_session: Option<&str>,
) -> Option<String> {
    let painter = ui.painter_at(rect);
    let row_h = 30.0;
    let max = ((rect.height() / row_h).floor() as usize).min(matches.len());
    if matches.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "no matching sessions",
            egui::FontId::monospace(13.0),
            palette.muted,
        );
        return None;
    }

    let mut activated = None;
    for (row_index, session_index) in matches.iter().take(max).enumerate() {
        let Some(session) = sessions.get(*session_index) else {
            continue;
        };
        let row = Rect::from_min_size(
            Pos2::new(rect.min.x, rect.min.y + row_index as f32 * row_h),
            egui::vec2(rect.width(), row_h),
        );
        let response = ui.interact(
            row,
            ui.make_persistent_id(("session-picker-row", session.id.as_str())),
            egui::Sense::click(),
        );
        let is_selected = row_index == selected;
        let is_current = selected_session.is_some_and(|current| {
            current == session.id.as_str() || current == session.name.as_str()
        });
        let bg = if is_selected {
            palette.hover
        } else if is_current {
            palette.surface
        } else {
            palette.mantle
        };
        painter.rect_filled(row, 0.0, bg);
        if is_current {
            let bar = Rect::from_min_max(row.min, Pos2::new(row.min.x + 4.0, row.max.y));
            painter.rect_filled(bar, 0.0, palette.primary);
        }
        painter.text(
            Pos2::new(row.min.x + 14.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            &session.name,
            egui::FontId::monospace(13.0),
            if is_current {
                palette.primary
            } else {
                palette.text
            },
        );
        if let Some(process) = session
            .anchor
            .process
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            painter.text(
                Pos2::new(row.max.x - 14.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                process,
                egui::FontId::monospace(12.0),
                palette.muted,
            );
        }
        if response.clicked_by(egui::PointerButton::Primary) {
            activated = Some(session.id.clone());
        }
    }
    activated
}

fn filtered_session_indices(sessions: &[MuxSession], filter: &str) -> Vec<usize> {
    sessions
        .iter()
        .enumerate()
        .filter_map(|(index, session)| session_matches(session, filter).then_some(index))
        .collect()
}

fn session_matches(session: &MuxSession, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return true;
    }
    fuzzy_match(&session.name, filter)
        || fuzzy_match(&session.id, filter)
        || session
            .anchor
            .process
            .as_deref()
            .is_some_and(|process| fuzzy_match(process, filter))
}

fn fuzzy_match(candidate: &str, pattern: &str) -> bool {
    let mut remaining = pattern.chars().flat_map(char::to_lowercase);
    let mut current = remaining.next();
    if current.is_none() {
        return true;
    }
    for ch in candidate.chars().flat_map(char::to_lowercase) {
        if Some(ch) == current {
            current = remaining.next();
            if current.is_none() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::snapshot::MuxPaneAnchor;

    fn session(id: &str, name: &str, process: Option<&str>) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                process: process.map(str::to_owned),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    #[test]
    fn filters_sessions_by_fuzzy_name_id_or_process() {
        let sessions = vec![
            session("s1", "bootty", Some("cargo")),
            session("s2", "dotfiles", Some("nvim")),
            session("s3", "blueprints", Some("zsh")),
        ];

        assert_eq!(filtered_session_indices(&sessions, "bty"), vec![0]);
        assert_eq!(filtered_session_indices(&sessions, "nv"), vec![1]);
        assert_eq!(filtered_session_indices(&sessions, "s3"), vec![2]);
        assert_eq!(
            filtered_session_indices(&sessions, "missing"),
            Vec::<usize>::new()
        );
    }
}
