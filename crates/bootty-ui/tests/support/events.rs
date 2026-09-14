use bootty_ui::gpui::{InputEvent, Key, Modifiers};

pub const fn key_event(key: Key, modifiers: Modifiers) -> InputEvent {
    InputEvent::Key {
        key,
        pressed: true,
        repeat: false,
        modifiers,
    }
}
