//! Keyboard/input model for the static site backend.

use tuirealm::event::{Event, Key, KeyEvent, KeyModifiers, NoUserEvent};

#[derive(Debug, PartialEq)]
pub(crate) enum Msg {
    Move(isize),
    SwitchTab(isize),
    SwitchSubTab(isize),
    Focus(Focus),
    ToggleFocus,
    Scroll(isize),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Focus {
    Menu,
    Detail,
}

pub(crate) fn parse_input(input: &str) -> Vec<Event<NoUserEvent>> {
    let mut events = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        let event = if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            match chars.next() {
                Some('A') => key(Key::Up),
                Some('B') => key(Key::Down),
                Some('C') => key(Key::Right),
                Some('D') => key(Key::Left),
                Some('F') => key(Key::End),
                Some('H') => key(Key::Home),
                Some('5') if chars.next() == Some('~') => key(Key::PageUp),
                Some('6') if chars.next() == Some('~') => key(Key::PageDown),
                _ => key(Key::Esc),
            }
        } else {
            match ch {
                '\u{1b}' => key(Key::Esc),
                '\r' | '\n' => key(Key::Enter),
                '\t' => key(Key::Tab),
                '\u{7f}' | '\u{8}' => key(Key::Backspace),
                _ => key(Key::Char(ch)),
            }
        };
        events.push(event);
    }
    events
}

fn key(code: Key) -> Event<NoUserEvent> {
    Event::Keyboard(KeyEvent {
        code,
        modifiers: KeyModifiers::empty(),
    })
}

pub(crate) fn wrap(value: isize, len: usize) -> usize {
    value.rem_euclid(len as isize) as usize
}
