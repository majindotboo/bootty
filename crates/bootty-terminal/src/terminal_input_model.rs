use crate::geometry::MouseSurfaceMetrics;
use libghostty_vt::{key, mouse};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MacosOptionAsAlt {
    None,
    Left,
    Right,
    #[default]
    Both,
}

impl From<MacosOptionAsAlt> for key::OptionAsAlt {
    fn from(value: MacosOptionAsAlt) -> Self {
        match value {
            MacosOptionAsAlt::None => Self::False,
            MacosOptionAsAlt::Left => Self::Left,
            MacosOptionAsAlt::Right => Self::Right,
            MacosOptionAsAlt::Both => Self::True,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub key: TerminalKey,
    pub mods: KeyMods,
    pub repeat: bool,
    pub utf8: Option<&'static str>,
    pub unshifted: Option<char>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Terminal protocol modifier flags vary independently."
)]
pub struct KeyMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub command: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub right_shift: bool,
    pub right_alt: bool,
    pub right_ctrl: bool,
    pub right_command: bool,
}

impl From<KeyMods> for key::Mods {
    fn from(value: KeyMods) -> Self {
        let mut mods = Self::empty();
        if value.shift {
            mods |= Self::SHIFT;
        }
        if value.alt {
            mods |= Self::ALT;
        }
        if value.ctrl {
            mods |= Self::CTRL;
        }
        if value.command {
            mods |= Self::SUPER;
        }
        if value.caps_lock {
            mods |= Self::CAPS_LOCK;
        }
        if value.num_lock {
            mods |= Self::NUM_LOCK;
        }
        if value.shift && value.right_shift {
            mods |= Self::SHIFT_SIDE;
        }
        if value.alt && value.right_alt {
            mods |= Self::ALT_SIDE;
        }
        if value.ctrl && value.right_ctrl {
            mods |= Self::CTRL_SIDE;
        }
        if value.command && value.right_command {
            mods |= Self::SUPER_SIDE;
        }
        mods
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseInput {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub mods: KeyMods,
    /// Position projected onto the integer cell grid for cell mouse protocols.
    pub x: f32,
    pub y: f32,
    /// Raw logical surface position for SGR pixel mouse mode.
    pub pixel_x: f32,
    pub pixel_y: f32,
    pub size: MouseEncoderSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEncoderSize {
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_right: u32,
    pub padding_left: u32,
}

impl From<MouseSurfaceMetrics> for MouseEncoderSize {
    fn from(metrics: MouseSurfaceMetrics) -> Self {
        Self {
            screen_width: metrics.screen_width,
            screen_height: metrics.screen_height,
            // The VT mouse encoder divides by cell dimensions; a surface can
            // report zero-sized cells before the first real layout.
            cell_width: metrics.cell_width.max(1),
            cell_height: metrics.cell_height.max(1),
            padding_top: metrics.padding.top,
            padding_bottom: metrics.padding.bottom,
            padding_right: metrics.padding.right,
            padding_left: metrics.padding.left,
        }
    }
}

impl From<MouseEncoderSize> for mouse::EncoderSize {
    fn from(value: MouseEncoderSize) -> Self {
        Self {
            screen_width: value.screen_width,
            screen_height: value.screen_height,
            cell_width: value.cell_width,
            cell_height: value.cell_height,
            padding_top: value.padding_top,
            padding_bottom: value.padding_bottom,
            padding_right: value.padding_right,
            padding_left: value.padding_left,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

impl From<MouseAction> for mouse::Action {
    fn from(value: MouseAction) -> Self {
        match value {
            MouseAction::Press => Self::Press,
            MouseAction::Release => Self::Release,
            MouseAction::Motion => Self::Motion,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
    Eleven,
}

impl From<MouseButton> for mouse::Button {
    fn from(value: MouseButton) -> Self {
        match value {
            MouseButton::Left => Self::Left,
            MouseButton::Right => Self::Right,
            MouseButton::Middle => Self::Middle,
            MouseButton::Four => Self::Four,
            MouseButton::Five => Self::Five,
            MouseButton::Six => Self::Six,
            MouseButton::Seven => Self::Seven,
            MouseButton::Eight => Self::Eight,
            MouseButton::Nine => Self::Nine,
            MouseButton::Ten => Self::Ten,
            MouseButton::Eleven => Self::Eleven,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalKey {
    Backquote,
    Backslash,
    BracketLeft,
    BracketRight,
    Comma,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Minus,
    Period,
    Quote,
    Semicolon,
    Slash,
    Enter,
    Tab,
    Backspace,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
    Insert,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadDecimal,
    NumpadDivide,
    NumpadEnter,
    NumpadEqual,
    NumpadMultiply,
    NumpadSubtract,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
}

impl From<TerminalKey> for key::Key {
    fn from(value: TerminalKey) -> Self {
        match value {
            TerminalKey::Backquote => Self::Backquote,
            TerminalKey::Backslash => Self::Backslash,
            TerminalKey::BracketLeft => Self::BracketLeft,
            TerminalKey::BracketRight => Self::BracketRight,
            TerminalKey::Comma => Self::Comma,
            TerminalKey::Digit0 => Self::Digit0,
            TerminalKey::Digit1 => Self::Digit1,
            TerminalKey::Digit2 => Self::Digit2,
            TerminalKey::Digit3 => Self::Digit3,
            TerminalKey::Digit4 => Self::Digit4,
            TerminalKey::Digit5 => Self::Digit5,
            TerminalKey::Digit6 => Self::Digit6,
            TerminalKey::Digit7 => Self::Digit7,
            TerminalKey::Digit8 => Self::Digit8,
            TerminalKey::Digit9 => Self::Digit9,
            TerminalKey::Equal => Self::Equal,
            TerminalKey::A => Self::A,
            TerminalKey::B => Self::B,
            TerminalKey::C => Self::C,
            TerminalKey::D => Self::D,
            TerminalKey::E => Self::E,
            TerminalKey::F => Self::F,
            TerminalKey::G => Self::G,
            TerminalKey::H => Self::H,
            TerminalKey::I => Self::I,
            TerminalKey::J => Self::J,
            TerminalKey::K => Self::K,
            TerminalKey::L => Self::L,
            TerminalKey::M => Self::M,
            TerminalKey::N => Self::N,
            TerminalKey::O => Self::O,
            TerminalKey::P => Self::P,
            TerminalKey::Q => Self::Q,
            TerminalKey::R => Self::R,
            TerminalKey::S => Self::S,
            TerminalKey::T => Self::T,
            TerminalKey::U => Self::U,
            TerminalKey::V => Self::V,
            TerminalKey::W => Self::W,
            TerminalKey::X => Self::X,
            TerminalKey::Y => Self::Y,
            TerminalKey::Z => Self::Z,
            TerminalKey::Minus => Self::Minus,
            TerminalKey::Period => Self::Period,
            TerminalKey::Quote => Self::Quote,
            TerminalKey::Semicolon => Self::Semicolon,
            TerminalKey::Slash => Self::Slash,
            TerminalKey::Enter => Self::Enter,
            TerminalKey::Tab => Self::Tab,
            TerminalKey::Backspace => Self::Backspace,
            TerminalKey::Escape => Self::Escape,
            TerminalKey::ArrowUp => Self::ArrowUp,
            TerminalKey::ArrowDown => Self::ArrowDown,
            TerminalKey::ArrowRight => Self::ArrowRight,
            TerminalKey::ArrowLeft => Self::ArrowLeft,
            TerminalKey::Delete => Self::Delete,
            TerminalKey::Home => Self::Home,
            TerminalKey::End => Self::End,
            TerminalKey::PageUp => Self::PageUp,
            TerminalKey::PageDown => Self::PageDown,
            TerminalKey::Space => Self::Space,
            TerminalKey::Insert => Self::Insert,
            TerminalKey::F1 => Self::F1,
            TerminalKey::F2 => Self::F2,
            TerminalKey::F3 => Self::F3,
            TerminalKey::F4 => Self::F4,
            TerminalKey::F5 => Self::F5,
            TerminalKey::F6 => Self::F6,
            TerminalKey::F7 => Self::F7,
            TerminalKey::F8 => Self::F8,
            TerminalKey::F9 => Self::F9,
            TerminalKey::F10 => Self::F10,
            TerminalKey::F11 => Self::F11,
            TerminalKey::F12 => Self::F12,
            TerminalKey::Numpad0 => Self::Numpad0,
            TerminalKey::Numpad1 => Self::Numpad1,
            TerminalKey::Numpad2 => Self::Numpad2,
            TerminalKey::Numpad3 => Self::Numpad3,
            TerminalKey::Numpad4 => Self::Numpad4,
            TerminalKey::Numpad5 => Self::Numpad5,
            TerminalKey::Numpad6 => Self::Numpad6,
            TerminalKey::Numpad7 => Self::Numpad7,
            TerminalKey::Numpad8 => Self::Numpad8,
            TerminalKey::Numpad9 => Self::Numpad9,
            TerminalKey::NumpadAdd => Self::NumpadAdd,
            TerminalKey::NumpadDecimal => Self::NumpadDecimal,
            TerminalKey::NumpadDivide => Self::NumpadDivide,
            TerminalKey::NumpadEnter => Self::NumpadEnter,
            TerminalKey::NumpadEqual => Self::NumpadEqual,
            TerminalKey::NumpadMultiply => Self::NumpadMultiply,
            TerminalKey::NumpadSubtract => Self::NumpadSubtract,
            TerminalKey::ShiftLeft => Self::ShiftLeft,
            TerminalKey::ShiftRight => Self::ShiftRight,
            TerminalKey::ControlLeft => Self::ControlLeft,
            TerminalKey::ControlRight => Self::ControlRight,
            TerminalKey::AltLeft => Self::AltLeft,
            TerminalKey::AltRight => Self::AltRight,
        }
    }
}

#[must_use]
pub fn physical_key_utf8(key: TerminalKey, shifted: bool) -> Option<&'static str> {
    key_text(key).map(|text| {
        if shifted {
            text.shifted_utf8.unwrap_or(text.unshifted_utf8)
        } else {
            text.unshifted_utf8
        }
    })
}

#[must_use]
pub fn key_unshifted(key: TerminalKey) -> Option<char> {
    key_text(key).map(|text| text.unshifted)
}

#[must_use]
pub fn shifted_ascii_symbol(unshifted: char) -> Option<&'static str> {
    const SYMBOL_KEYS: &[TerminalKey] = &[
        TerminalKey::Backquote,
        TerminalKey::Backslash,
        TerminalKey::BracketLeft,
        TerminalKey::BracketRight,
        TerminalKey::Comma,
        TerminalKey::Digit0,
        TerminalKey::Digit1,
        TerminalKey::Digit2,
        TerminalKey::Digit3,
        TerminalKey::Digit4,
        TerminalKey::Digit5,
        TerminalKey::Digit6,
        TerminalKey::Digit7,
        TerminalKey::Digit8,
        TerminalKey::Digit9,
        TerminalKey::Equal,
        TerminalKey::Minus,
        TerminalKey::Period,
        TerminalKey::Quote,
        TerminalKey::Semicolon,
        TerminalKey::Slash,
    ];

    SYMBOL_KEYS.iter().find_map(|key| {
        let text = key_text(*key)?;
        (text.unshifted == unshifted)
            .then_some(text.shifted_utf8)
            .flatten()
    })
}

struct KeyText {
    unshifted: char,
    unshifted_utf8: &'static str,
    shifted_utf8: Option<&'static str>,
}

const fn key_text(key: TerminalKey) -> Option<KeyText> {
    let (unshifted, unshifted_utf8, shifted_utf8) = match key {
        TerminalKey::Space => (' ', " ", None),
        TerminalKey::Backquote => ('`', "`", Some("~")),
        TerminalKey::Backslash => ('\\', "\\", Some("|")),
        TerminalKey::BracketLeft => ('[', "[", Some("{")),
        TerminalKey::BracketRight => (']', "]", Some("}")),
        TerminalKey::Comma => (',', ",", Some("<")),
        TerminalKey::Digit0 => ('0', "0", Some(")")),
        TerminalKey::Digit1 => ('1', "1", Some("!")),
        TerminalKey::Digit2 => ('2', "2", Some("@")),
        TerminalKey::Digit3 => ('3', "3", Some("#")),
        TerminalKey::Digit4 => ('4', "4", Some("$")),
        TerminalKey::Digit5 => ('5', "5", Some("%")),
        TerminalKey::Digit6 => ('6', "6", Some("^")),
        TerminalKey::Digit7 => ('7', "7", Some("&")),
        TerminalKey::Digit8 => ('8', "8", Some("*")),
        TerminalKey::Digit9 => ('9', "9", Some("(")),
        TerminalKey::Equal => ('=', "=", Some("+")),
        TerminalKey::Minus => ('-', "-", Some("_")),
        TerminalKey::Numpad0 => ('0', "0", None),
        TerminalKey::Numpad1 => ('1', "1", None),
        TerminalKey::Numpad2 => ('2', "2", None),
        TerminalKey::Numpad3 => ('3', "3", None),
        TerminalKey::Numpad4 => ('4', "4", None),
        TerminalKey::Numpad5 => ('5', "5", None),
        TerminalKey::Numpad6 => ('6', "6", None),
        TerminalKey::Numpad7 => ('7', "7", None),
        TerminalKey::Numpad8 => ('8', "8", None),
        TerminalKey::Numpad9 => ('9', "9", None),
        TerminalKey::NumpadAdd => ('+', "+", None),
        TerminalKey::NumpadDecimal => ('.', ".", None),
        TerminalKey::NumpadDivide => ('/', "/", None),
        TerminalKey::NumpadEqual => ('=', "=", None),
        TerminalKey::NumpadMultiply => ('*', "*", None),
        TerminalKey::NumpadSubtract => ('-', "-", None),
        TerminalKey::Period => ('.', ".", Some(">")),
        TerminalKey::Quote => ('\'', "'", Some("\"")),
        TerminalKey::Semicolon => (';', ";", Some(":")),
        TerminalKey::Slash => ('/', "/", Some("?")),
        TerminalKey::A => return Some(letter_text('a', "a", "A")),
        TerminalKey::B => return Some(letter_text('b', "b", "B")),
        TerminalKey::C => return Some(letter_text('c', "c", "C")),
        TerminalKey::D => return Some(letter_text('d', "d", "D")),
        TerminalKey::E => return Some(letter_text('e', "e", "E")),
        TerminalKey::F => return Some(letter_text('f', "f", "F")),
        TerminalKey::G => return Some(letter_text('g', "g", "G")),
        TerminalKey::H => return Some(letter_text('h', "h", "H")),
        TerminalKey::I => return Some(letter_text('i', "i", "I")),
        TerminalKey::J => return Some(letter_text('j', "j", "J")),
        TerminalKey::K => return Some(letter_text('k', "k", "K")),
        TerminalKey::L => return Some(letter_text('l', "l", "L")),
        TerminalKey::M => return Some(letter_text('m', "m", "M")),
        TerminalKey::N => return Some(letter_text('n', "n", "N")),
        TerminalKey::O => return Some(letter_text('o', "o", "O")),
        TerminalKey::P => return Some(letter_text('p', "p", "P")),
        TerminalKey::Q => return Some(letter_text('q', "q", "Q")),
        TerminalKey::R => return Some(letter_text('r', "r", "R")),
        TerminalKey::S => return Some(letter_text('s', "s", "S")),
        TerminalKey::T => return Some(letter_text('t', "t", "T")),
        TerminalKey::U => return Some(letter_text('u', "u", "U")),
        TerminalKey::V => return Some(letter_text('v', "v", "V")),
        TerminalKey::W => return Some(letter_text('w', "w", "W")),
        TerminalKey::X => return Some(letter_text('x', "x", "X")),
        TerminalKey::Y => return Some(letter_text('y', "y", "Y")),
        TerminalKey::Z => return Some(letter_text('z', "z", "Z")),
        _ => return None,
    };
    Some(KeyText {
        unshifted,
        unshifted_utf8,
        shifted_utf8,
    })
}

const fn letter_text(
    unshifted: char,
    unshifted_utf8: &'static str,
    shifted_utf8: &'static str,
) -> KeyText {
    KeyText {
        unshifted,
        unshifted_utf8,
        shifted_utf8: Some(shifted_utf8),
    }
}
