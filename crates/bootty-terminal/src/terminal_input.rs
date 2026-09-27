use crate::terminal::{KeyInput, MouseInput};
use crate::terminal_engine::TerminalEngine;
use anyhow::Result;

/// Modifier side state retained by a native input adapter when the host reports physical keys.
/// GPUI itself exposes aggregate modifiers, so the desktop may leave this at its default value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Physical modifier keys are independent pressed bits."
)]
pub struct ModifierSideState {
    pub left_shift: bool,
    pub right_shift: bool,
    pub left_alt: bool,
    pub right_alt: bool,
    pub left_ctrl: bool,
    pub right_ctrl: bool,
    pub left_command: bool,
    pub right_command: bool,
}

impl ModifierSideState {
    /// Select terminal Meta behavior; hosts without side information retain the aggregate fallback.
    #[must_use]
    pub const fn option_alt_is_meta(
        self,
        option_as_alt: crate::terminal_input_model::MacosOptionAsAlt,
    ) -> bool {
        use crate::terminal_input_model::MacosOptionAsAlt;
        match option_as_alt {
            MacosOptionAsAlt::None => false,
            MacosOptionAsAlt::Both => true,
            MacosOptionAsAlt::Left => self.left_alt || !self.right_alt,
            MacosOptionAsAlt::Right => self.right_alt || !self.left_alt,
        }
    }

    /// Update one physical modifier using the terminal's framework-free key vocabulary.
    pub const fn update_key(&mut self, key: crate::terminal::TerminalKey, pressed: bool) {
        match key {
            crate::terminal::TerminalKey::ShiftLeft => self.left_shift = pressed,
            crate::terminal::TerminalKey::ShiftRight => self.right_shift = pressed,
            crate::terminal::TerminalKey::AltLeft => self.left_alt = pressed,
            crate::terminal::TerminalKey::AltRight => self.right_alt = pressed,
            crate::terminal::TerminalKey::ControlLeft => self.left_ctrl = pressed,
            crate::terminal::TerminalKey::ControlRight => self.right_ctrl = pressed,
            _ => {}
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub const fn apply_to_key_input(self, input: &mut KeyInput) {
        input.mods.shift = input.mods.shift || self.left_shift || self.right_shift;
        input.mods.alt = input.mods.alt || self.left_alt || self.right_alt;
        input.mods.ctrl = input.mods.ctrl || self.left_ctrl || self.right_ctrl;
        input.mods.command = input.mods.command || self.left_command || self.right_command;
        input.mods.right_shift = input.mods.shift && self.right_shift;
        input.mods.right_alt = input.mods.alt && self.right_alt;
        input.mods.right_ctrl = input.mods.ctrl && self.right_ctrl;
        input.mods.right_command = input.mods.command && self.right_command;
    }
}

/// A terminal key delivered by a native input adapter after physical/logical policy is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectKeyInput {
    pub input: KeyInput,
}

impl DirectKeyInput {
    #[must_use]
    pub const fn input(self) -> KeyInput {
        self.input
    }
}

/// One input decision made by the UI and delivered to a terminal runtime.
///
/// Keeping this command beside the terminal input model makes the delivery contract independent
/// of GPUI and of the native window event source. UI adapters may construct commands, while the
/// terminal runtime remains the single encoder and writer.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalInputCommand {
    Text(String),
    Paste(String),
    Focus(bool),
    Key(KeyInput),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
}

/// Frame work required after applying input, independent of its transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalInputEffects {
    pub viewport_changed: bool,
    pub force_publish: bool,
}

impl TerminalInputCommand {
    /// Apply input policy and encode bytes for the runtime to deliver.
    ///
    /// # Errors
    /// Returns an error if the terminal cannot inspect its input modes or encode the event.
    pub fn apply(
        &self,
        engine: &mut TerminalEngine,
        output: &mut Vec<u8>,
    ) -> Result<TerminalInputEffects> {
        output.clear();
        let mut effects = TerminalInputEffects::default();
        match self {
            Self::Text(text) => output.extend_from_slice(text.as_bytes()),
            Self::Paste(text) => engine.encode_paste_to_vec(text, output)?,
            Self::Key(input) => engine.encode_key_to_vec(*input, output)?,
            Self::Focus(gained) => engine.encode_focus_to_vec(*gained, output)?,
            Self::Mouse(input) => engine.encode_mouse_to_vec(*input, output)?,
            Self::MouseWheel {
                input,
                scroll_delta,
            } => {
                if engine.is_mouse_tracking()? {
                    engine.encode_mouse_wheel_to_vec(
                        *input,
                        scroll_delta.unsigned_abs().max(1),
                        output,
                    )?;
                    effects.force_publish = true;
                } else if *scroll_delta != 0 {
                    engine.scroll_viewport_delta(*scroll_delta);
                    effects.viewport_changed = true;
                }
            }
        }
        if matches!(self, Self::Text(_) | Self::Paste(_) | Self::Key(_)) {
            engine.scroll_viewport_bottom();
            effects.viewport_changed = true;
        }
        // Untracked pointer motion must not bypass the output quiet window.
        effects.force_publish |=
            effects.viewport_changed || !output.is_empty() || matches!(self, Self::Focus(_));
        Ok(effects)
    }
}
