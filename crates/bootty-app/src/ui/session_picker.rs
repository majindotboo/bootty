use bootty_ui::Theme;
use eframe::egui;

use crate::mux::snapshot::MuxSession;
use crate::ui::overlay::{self, FloatingWindow, ListRow, ListView, list};

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
        let matches = filtered_session_indices(sessions, &self.filter);
        self.selected = list::clamp_selection(self.selected, matches.len());
        let rows = session_rows(sessions, &matches, selected_session);
        let list_max_height = overlay::list_max_height(ctx, 150.0, 520.0);

        let result = FloatingWindow::new("session-picker-dialog", "Session Finder")
            .icon("terminal")
            .shortcut_hint([("enter", "select"), ("esc", "close")])
            .footer(format!("{} / {} sessions", matches.len(), sessions.len()))
            .width(overlay::panel_width(ctx, 780.0, 520.0))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new("session-picker-filter"),
                    &mut self.filter,
                    theme,
                    "filter sessions...",
                );
                if self.focus_filter {
                    filter.request_focus();
                    self.focus_filter = false;
                }
                ui.add_space(8.0);
                let outcome = ListView::new("session-picker-list", &rows, self.selected)
                    .max_height(list_max_height)
                    .empty_text("no matching sessions")
                    .show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(session_index) = matches.get(index)
            && let Some(session) = sessions.get(*session_index)
        {
            return SessionPickerEvent::ActivateSession(session.id.clone());
        }
        if result.escaped || result.clicked_outside {
            return SessionPickerEvent::Close;
        }
        SessionPickerEvent::None
    }
}

fn session_rows(
    sessions: &[MuxSession],
    matches: &[usize],
    selected_session: Option<&str>,
) -> Vec<ListRow> {
    matches
        .iter()
        .filter_map(|&index| {
            let session = sessions.get(index)?;
            let current = selected_session.is_some_and(|current| {
                current == session.id.as_str() || current == session.name.as_str()
            });
            let trailing = session
                .anchor
                .process
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            Some(ListRow {
                icon: Some("terminal".to_owned()),
                primary: session.name.clone(),
                trailing,
                current,
                ..ListRow::default()
            })
        })
        .collect()
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
    overlay::fuzzy_match(&session.name, filter)
        || overlay::fuzzy_match(&session.id, filter)
        || session
            .anchor
            .process
            .as_deref()
            .is_some_and(|process| overlay::fuzzy_match(process, filter))
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

    #[test]
    fn current_session_row_is_marked_by_id_or_name() {
        let sessions = vec![
            session("s1", "bootty", None),
            session("s2", "dotfiles", None),
        ];
        let matches = filtered_session_indices(&sessions, "");

        let by_id = session_rows(&sessions, &matches, Some("s1"));
        assert!(by_id[0].current && !by_id[1].current);

        let by_name = session_rows(&sessions, &matches, Some("dotfiles"));
        assert!(!by_name[0].current && by_name[1].current);
    }
}
