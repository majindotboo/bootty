use crate::gpui::{InputEvent, Key, Modifiers};
use bootty_terminal::terminal_input::ModifierSideState;
use bootty_terminal::{
    terminal_engine::{TerminalCopyModeAction, TerminalCopyModeMotion, TerminalSearchDirection},
    terminal_input_model::{KeyInput, TerminalKey},
};

use crate::app_actions::{key_mods_for_binding, terminal_key};

pub(super) fn copy_shortcut_pressed(event: &InputEvent) -> bool {
    let InputEvent::Key {
        key,
        pressed: true,
        repeat,
        modifiers,
        ..
    } = event
    else {
        return false;
    };
    copy_mode_input_for_key(*key, *modifiers, *repeat).is_some_and(direct_copy_shortcut_pressed)
}

pub(super) fn direct_copy_shortcut_pressed(input: KeyInput) -> bool {
    input.key == TerminalKey::C
        && input.mods.command
        && !input.mods.ctrl
        && !input.mods.alt
        && !input.repeat
}

pub(super) fn copy_mode_key_input_present(events: &[InputEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, InputEvent::Key { .. } | InputEvent::ImeCommit(_)))
}

pub(super) fn copy_mode_key_should_pass_to_app(key: Key, modifiers: Modifiers) -> bool {
    copy_mode_input_for_key(key, modifiers, false).map_or_else(
        || {
            let mods = key_mods_for_binding(modifiers, ModifierSideState::default());
            mods.alt || mods.command
        },
        copy_mode_input_should_pass_to_app,
    )
}

pub(super) fn copy_mode_input_should_pass_to_app(input: KeyInput) -> bool {
    input.mods.alt || (input.mods.command && !direct_copy_shortcut_pressed(input))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CopyModeKeyAction {
    Terminal(TerminalCopyModeAction),
    SearchPrompt(TerminalSearchDirection),
    SearchWord(TerminalSearchDirection),
    SearchRepeat(CopyModeSearchRepeat),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CopyModeSearchRepeat {
    SameDirection,
    OppositeDirection,
}

impl CopyModeSearchRepeat {
    pub(super) const fn direction(
        self,
        started_direction: TerminalSearchDirection,
    ) -> TerminalSearchDirection {
        match self {
            Self::SameDirection => started_direction,
            Self::OppositeDirection => opposite_terminal_search_direction(started_direction),
        }
    }
}

const fn opposite_terminal_search_direction(
    direction: TerminalSearchDirection,
) -> TerminalSearchDirection {
    match direction {
        TerminalSearchDirection::Previous => TerminalSearchDirection::Next,
        TerminalSearchDirection::Current | TerminalSearchDirection::Next => {
            TerminalSearchDirection::Previous
        }
    }
}

const fn copy_mode_terminal_action(action: TerminalCopyModeAction) -> CopyModeKeyAction {
    CopyModeKeyAction::Terminal(action)
}

pub(super) fn copy_mode_action_for_event(
    event: &InputEvent,
    suppress_next_text: &mut bool,
) -> Option<CopyModeKeyAction> {
    match event {
        InputEvent::Key {
            key,
            pressed: true,
            repeat,
            modifiers,
            ..
        } => {
            let action = copy_mode_input_for_key(*key, *modifiers, *repeat)
                .and_then(copy_mode_action_for_input);
            *suppress_next_text = action.is_some() && copy_mode_key_may_emit_text(*key);
            action
        }
        InputEvent::ImeCommit(text) => {
            if std::mem::take(suppress_next_text) {
                None
            } else {
                text.chars()
                    .find_map(copy_mode_action_for_shifted_punctuation)
            }
        }
        _ => None,
    }
}

pub(super) const fn copy_mode_key_may_emit_text(key: Key) -> bool {
    matches!(
        key,
        Key::Letter(_) | Key::Digit(_) | Key::QuestionMark | Key::Slash | Key::Space
    )
}

fn copy_mode_input_for_key(key: Key, modifiers: Modifiers, repeat: bool) -> Option<KeyInput> {
    let mut mods = key_mods_for_binding(modifiers, ModifierSideState::default());
    // Some platforms report `?` as a logical key without the Shift bit.
    mods.shift |= key == Key::QuestionMark;
    Some(KeyInput {
        key: terminal_key(key)?,
        mods,
        repeat,
        utf8: None,
        unshifted: None,
    })
}

pub(super) fn copy_mode_action_for_input(input: KeyInput) -> Option<CopyModeKeyAction> {
    if direct_copy_shortcut_pressed(input) {
        return Some(copy_mode_terminal_action(
            TerminalCopyModeAction::CopySelectionAndCancel,
        ));
    }
    if input.mods.command || input.mods.alt {
        return None;
    }
    if input.mods.ctrl {
        return copy_mode_ctrl_terminal_key_action(input.key);
    }
    if input.mods.shift {
        return copy_mode_shift_terminal_key_action(input.key);
    }
    copy_mode_terminal_key_action(input.key)
}

const fn copy_mode_ctrl_terminal_key_action(key: TerminalKey) -> Option<CopyModeKeyAction> {
    Some(match key {
        TerminalKey::B => copy_mode_motion(TerminalCopyModeMotion::PageUp),
        TerminalKey::C | TerminalKey::G => {
            copy_mode_terminal_action(TerminalCopyModeAction::Cancel)
        }
        TerminalKey::D => copy_mode_motion(TerminalCopyModeMotion::HalfPageDown),
        TerminalKey::E => copy_mode_motion(TerminalCopyModeMotion::ScrollDown),
        TerminalKey::F => copy_mode_motion(TerminalCopyModeMotion::PageDown),
        TerminalKey::J | TerminalKey::Enter | TerminalKey::NumpadEnter => {
            copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel)
        }
        TerminalKey::N => copy_mode_motion(TerminalCopyModeMotion::Down),
        TerminalKey::P => copy_mode_motion(TerminalCopyModeMotion::Up),
        TerminalKey::U => copy_mode_motion(TerminalCopyModeMotion::HalfPageUp),
        TerminalKey::V => copy_mode_terminal_action(TerminalCopyModeAction::ToggleRectangle),
        TerminalKey::Y => copy_mode_motion(TerminalCopyModeMotion::ScrollUp),
        _ => return None,
    })
}

const fn copy_mode_shift_terminal_key_action(key: TerminalKey) -> Option<CopyModeKeyAction> {
    Some(match key {
        TerminalKey::Slash => CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Previous),
        TerminalKey::Digit3 => CopyModeKeyAction::SearchWord(TerminalSearchDirection::Previous),
        TerminalKey::Digit8 | TerminalKey::NumpadMultiply => {
            CopyModeKeyAction::SearchWord(TerminalSearchDirection::Next)
        }
        TerminalKey::NumpadDivide => CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Next),
        TerminalKey::Numpad0 => copy_mode_motion(TerminalCopyModeMotion::StartOfLine),
        TerminalKey::G => copy_mode_motion(TerminalCopyModeMotion::HistoryBottom),
        TerminalKey::H => copy_mode_motion(TerminalCopyModeMotion::TopLine),
        TerminalKey::L => copy_mode_motion(TerminalCopyModeMotion::BottomLine),
        TerminalKey::M => copy_mode_motion(TerminalCopyModeMotion::MiddleLine),
        TerminalKey::N => CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::OppositeDirection),
        TerminalKey::V => copy_mode_terminal_action(TerminalCopyModeAction::SelectLine),
        TerminalKey::Digit4 => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
        TerminalKey::Digit6 => copy_mode_motion(TerminalCopyModeMotion::BackToIndentation),
        _ => return None,
    })
}

const fn copy_mode_terminal_key_action(key: TerminalKey) -> Option<CopyModeKeyAction> {
    Some(match key {
        TerminalKey::Escape => {
            copy_mode_terminal_action(TerminalCopyModeAction::CancelOrClearSelection)
        }
        TerminalKey::Enter | TerminalKey::NumpadEnter => {
            copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel)
        }
        TerminalKey::Space => copy_mode_terminal_action(TerminalCopyModeAction::BeginSelection),
        TerminalKey::ArrowLeft | TerminalKey::H => copy_mode_motion(TerminalCopyModeMotion::Left),
        TerminalKey::ArrowRight | TerminalKey::L => copy_mode_motion(TerminalCopyModeMotion::Right),
        TerminalKey::ArrowUp | TerminalKey::K => copy_mode_motion(TerminalCopyModeMotion::Up),
        TerminalKey::ArrowDown | TerminalKey::J => copy_mode_motion(TerminalCopyModeMotion::Down),
        TerminalKey::PageUp => copy_mode_motion(TerminalCopyModeMotion::PageUp),
        TerminalKey::PageDown => copy_mode_motion(TerminalCopyModeMotion::PageDown),
        TerminalKey::Home => copy_mode_motion(TerminalCopyModeMotion::StartOfLine),
        TerminalKey::End => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
        TerminalKey::Slash | TerminalKey::NumpadDivide => {
            CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Next)
        }
        TerminalKey::NumpadMultiply => CopyModeKeyAction::SearchWord(TerminalSearchDirection::Next),
        TerminalKey::N => CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::SameDirection),
        TerminalKey::G => copy_mode_motion(TerminalCopyModeMotion::HistoryTop),
        TerminalKey::W => copy_mode_motion(TerminalCopyModeMotion::NextWord),
        TerminalKey::B => copy_mode_motion(TerminalCopyModeMotion::PreviousWord),
        TerminalKey::E => copy_mode_motion(TerminalCopyModeMotion::NextWordEnd),
        TerminalKey::V => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelection),
        TerminalKey::O => copy_mode_terminal_action(TerminalCopyModeAction::ToggleSelectionEnd),
        TerminalKey::Y => copy_mode_terminal_action(TerminalCopyModeAction::CopySelectionAndCancel),
        TerminalKey::Q => copy_mode_terminal_action(TerminalCopyModeAction::Cancel),
        TerminalKey::Digit0 | TerminalKey::Numpad0 => {
            copy_mode_motion(TerminalCopyModeMotion::StartOfLine)
        }
        _ => return None,
    })
}

const fn copy_mode_action_for_shifted_punctuation(ch: char) -> Option<CopyModeKeyAction> {
    Some(match ch {
        '?' => CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Previous),
        '*' => CopyModeKeyAction::SearchWord(TerminalSearchDirection::Next),
        '#' => CopyModeKeyAction::SearchWord(TerminalSearchDirection::Previous),
        '$' => copy_mode_motion(TerminalCopyModeMotion::EndOfLine),
        '^' => copy_mode_motion(TerminalCopyModeMotion::BackToIndentation),
        _ => return None,
    })
}

const fn copy_mode_motion(motion: TerminalCopyModeMotion) -> CopyModeKeyAction {
    copy_mode_terminal_action(TerminalCopyModeAction::Move(motion))
}
