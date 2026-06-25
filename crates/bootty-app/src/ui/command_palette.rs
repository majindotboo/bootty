//! A searchable palette of app commands, opened with `command_palette` (default
//! `cmd+p`). Commands and their titles/descriptions come from the shared
//! [`crate::action_catalog`]; a choice dispatches through the same path as a
//! keybinding (see `app_actions::keybind_action_for_name`).

use std::collections::HashMap;

use bootty_ui::Theme;
use eframe::egui;

use crate::action_catalog::Command;
use crate::ui::overlay::{self, FloatingWindow, ListRow, ListView, list};

#[derive(Clone, Debug)]
pub struct CommandPaletteDialog {
    filter: String,
    selected: usize,
    focus_filter: bool,
    /// The palette subset of the catalog, in display order.
    commands: Vec<Command>,
    /// dispatch-action string -> the chord it is bound to, for the trailing hint.
    bindings: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPaletteEvent {
    None,
    Close,
    /// The user activated a command; the value is its dispatch action string.
    Run(&'static str),
}

impl CommandPaletteDialog {
    /// `keybinds` is the active `chord=action` list, used to annotate each command
    /// with the key that triggers it.
    pub fn open(keybinds: &[String]) -> Self {
        let mut bindings = HashMap::new();
        for raw in keybinds {
            if let Some((chord, action)) = overlay::parse_keybind(raw) {
                bindings.entry(action).or_insert(chord);
            }
        }
        Self {
            filter: String::new(),
            selected: 0,
            focus_filter: true,
            commands: Command::all()
                .filter(|command| command.palette_action().is_some())
                .collect(),
            bindings,
        }
    }

    /// The base action name of the row under the cursor, for "configure this
    /// command's keybinding" (`cmd+shift+,`).
    pub fn current_action(&self) -> Option<&'static str> {
        let matches = filtered(&self.commands, &self.filter);
        matches
            .get(self.selected)
            .and_then(|&index| self.commands.get(index))
            .map(|command| command.action())
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> CommandPaletteEvent {
        let matches = filtered(&self.commands, &self.filter);
        self.selected = list::clamp_selection(self.selected, matches.len());
        let rows: Vec<ListRow> = matches
            .iter()
            .filter_map(|&index| self.commands.get(index))
            .map(|command| ListRow {
                icon: Some(command.icon().to_owned()),
                primary: command.title().to_owned(),
                secondary: Some(command.description().to_owned()),
                trailing: command
                    .palette_action()
                    .and_then(|action| self.bindings.get(action).cloned()),
                ..ListRow::default()
            })
            .collect();
        let list_max = overlay::list_max_height(ctx, 220.0, 560.0);

        let result = FloatingWindow::new("command-palette-dialog", "Commands")
            .icon("search")
            .hint("Enter run   Esc close")
            .footer(format!(
                "{} / {} commands",
                matches.len(),
                self.commands.len()
            ))
            .width(overlay::panel_width(ctx, 760.0, 480.0))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new("command-palette-filter"),
                    &mut self.filter,
                    theme,
                    "search commands...",
                );
                if self.focus_filter {
                    filter.request_focus();
                    self.focus_filter = false;
                }
                ui.add_space(8.0);
                let outcome = ListView::new("command-palette-list", &rows, self.selected)
                    .max_height(list_max)
                    .row_height(44.0)
                    .empty_text("no matching commands")
                    .show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(action) = matches
                .get(index)
                .and_then(|&i| self.commands.get(i))
                .and_then(|command| command.palette_action())
        {
            return CommandPaletteEvent::Run(action);
        }
        if result.escaped || result.clicked_outside {
            return CommandPaletteEvent::Close;
        }
        CommandPaletteEvent::None
    }
}

/// Indices of commands matching `filter` (fuzzy over title, action, description).
fn filtered(commands: &[Command], filter: &str) -> Vec<usize> {
    let filter = filter.trim();
    commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (filter.is_empty()
                || overlay::fuzzy_match(command.title(), filter)
                || overlay::fuzzy_match(command.action(), filter)
                || overlay::fuzzy_match(command.description(), filter))
            .then_some(index)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_title_action_or_description() {
        let commands: Vec<Command> = Command::all()
            .filter(|command| command.palette_action().is_some())
            .collect();
        assert!(!filtered(&commands, "rename").is_empty());
        assert!(!filtered(&commands, "split").is_empty());
        assert!(filtered(&commands, "zzzznotacommand").is_empty());
        assert_eq!(filtered(&commands, "").len(), commands.len());
    }
}
