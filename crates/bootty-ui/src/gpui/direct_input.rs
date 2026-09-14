use bootty_terminal::{
    terminal_input::DirectKeyInput,
    terminal_input_model::{KeyInput, KeyMods, TerminalKey},
};
use gpui_kit::KeyDownEvent;

/// Whether the terminal owns a key before GPUI offers it to the platform text input system.
///
/// Printable keys without Control, Command, or Function must propagate so keyboard layouts,
/// dead keys, paste, and IMEs can commit their actual text. Terminal control keys and modified
/// shortcuts are already represented by the raw key event; allowing those through on macOS can
/// dispatch the same key again through `doCommandBySelector`.
#[must_use]
pub fn terminal_owns_key_down(event: &KeyDownEvent) -> bool {
    let key = event.keystroke.key.to_ascii_lowercase();
    // Modifier keys do not make an unsupported physical key encodable.
    if terminal_key(&key).is_none() {
        return false;
    }
    let modifiers = event.keystroke.modifiers;
    modifiers.control
        || modifiers.platform
        || modifiers.function
        || matches!(
            key.as_str(),
            "down"
                | "left"
                | "right"
                | "up"
                | "escape"
                | "tab"
                | "backspace"
                | "enter"
                | "insert"
                | "delete"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
        )
        || key
            .strip_prefix('f')
            .and_then(|number| number.parse::<u8>().ok())
            // The terminal input model currently represents F1-F12. Higher function keys are
            // still valid GPUI keystrokes, but cannot be encoded here, so let them continue to
            // the platform instead of consuming and then dropping them.
            .is_some_and(|number| (1..=12).contains(&number))
}

/// Convert an unbound GPUI Command/Super keystroke into the terminal input path.
///
/// GPUI dispatches bound actions before raw key events, so reaching this seam means the app did
/// not consume the chord. Its platform text handler intentionally suppresses Command/Super text;
/// this adapter supplies the terminal's physical key and text representation directly.
#[must_use]
pub fn direct_key_input_from_gpui_event(event: &KeyDownEvent) -> Option<DirectKeyInput> {
    let modifiers = event.keystroke.modifiers;
    if !modifiers.platform {
        return None;
    }
    let key = terminal_key(&event.keystroke.key)?;
    Some(DirectKeyInput {
        input: KeyInput {
            key,
            mods: KeyMods {
                shift: modifiers.shift,
                alt: modifiers.alt,
                ctrl: modifiers.control,
                command: true,
                ..KeyMods::default()
            },
            repeat: event.is_held,
            utf8: bootty_terminal::terminal_input_model::physical_key_utf8(key, modifiers.shift),
            unshifted: bootty_terminal::terminal_input_model::key_unshifted(key),
        },
    })
}

fn terminal_key(value: &str) -> Option<TerminalKey> {
    use TerminalKey as Key;

    let normalized = value.to_ascii_lowercase();
    Some(match normalized.as_str() {
        "`" | "~" => Key::Backquote,
        "\\" | "|" => Key::Backslash,
        "[" | "{" => Key::BracketLeft,
        "]" | "}" => Key::BracketRight,
        "," | "<" => Key::Comma,
        "=" | "+" => Key::Equal,
        "-" | "_" => Key::Minus,
        "." | ">" => Key::Period,
        "'" | "\"" => Key::Quote,
        ";" | ":" => Key::Semicolon,
        "/" | "?" => Key::Slash,
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "right" => Key::ArrowRight,
        "left" => Key::ArrowLeft,
        "delete" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" | " " => Key::Space,
        "insert" => Key::Insert,
        value if value.len() == 1 => match *value.as_bytes().first()? {
            value @ b'0'..=b'9' => digit_key(value.checked_sub(b'0')?)?,
            value @ b'a'..=b'z' => letter_key(value)?,
            _ => return None,
        },
        value if value.strip_prefix('f').is_some() => function_key(value)?,
        _ => return None,
    })
}

fn digit_key(value: u8) -> Option<TerminalKey> {
    use TerminalKey as Key;
    [
        Key::Digit0,
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
        Key::Digit9,
    ]
    .get(usize::from(value))
    .copied()
}

fn letter_key(value: u8) -> Option<TerminalKey> {
    use TerminalKey as Key;
    [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ]
    .get(usize::from(value.checked_sub(b'a')?))
    .copied()
}

fn function_key(value: &str) -> Option<TerminalKey> {
    use TerminalKey as Key;
    [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ]
    .get(
        value
            .strip_prefix('f')?
            .parse::<usize>()
            .ok()?
            .checked_sub(1)?,
    )
    .copied()
}
