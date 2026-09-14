use crate::gpui::InputEvent;

use super::focus::InputFocus;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoutedInput {
    pub terminal_events: Vec<InputEvent>,
    pub ui_events: Vec<InputEvent>,
}

#[must_use]
pub const fn route_events(focus: InputFocus, events: Vec<InputEvent>) -> RoutedInput {
    if focus.terminal_owns_input() {
        return RoutedInput {
            terminal_events: events,
            ui_events: Vec::new(),
        };
    }

    RoutedInput {
        terminal_events: Vec::new(),
        ui_events: events,
    }
}
